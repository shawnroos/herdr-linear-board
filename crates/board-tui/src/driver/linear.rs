//! Linear-mode effects: the snapshot fetch (worker thread in production,
//! synchronous against a client with no reconnect path), `pane.focus`, the
//! URL opener and the clipboard, plus the daemon-signal policy the runtime
//! delegates to.

use std::sync::mpsc;

use board_core::client::{BoardClient, RpcClientError, UnixClient};
use board_core::protocol::{
    LinearListKind, LinearListParams, LinearSnapshotParams, PaneFocusParams,
    LINEAR_SNAPSHOT_CLIENT_TIMEOUT,
};

use crate::app::{LinearArrival, LinearFailure, Mode, Msg};
use crate::Driver;

/// A read held by the test hook instead of running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Pending {
    Snapshot,
    List {
        kind: LinearListKind,
        id: Option<String>,
    },
}

/// The only effects the driver executes in Linear mode. Everything else is
/// refused before a request is built. This guards the board's own code, not
/// what a socket client may ask the daemon for.
pub(super) fn linear_allows(eff: &crate::app::Effect) -> bool {
    use crate::app::Effect;
    matches!(
        eff,
        Effect::Refetch
            | Effect::LinearSnapshot
            | Effect::LinearList { .. }
            | Effect::FocusPane(_)
            | Effect::OpenIssueUrl(_)
            | Effect::CopyWorktreePath { .. }
            | Effect::SetLinearPaneTitle(_)
            | Effect::Quit
    )
}

/// The daemon reports an unknown method as protocol code 1 with the message
/// `bad request: unknown method: <name>`; code 1 alone also covers bad params.
pub(crate) fn classify<T>(result: anyhow::Result<T>) -> Result<T, LinearFailure> {
    result.map_err(|error| {
        let rpc = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<RpcClientError>());
        let timed_out = error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
            .any(|io| {
                matches!(
                    io.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
            });
        if rpc.is_none() && timed_out {
            return LinearFailure::Failed(format!(
                "the daemon did not answer within {}s; press r to try again",
                LINEAR_SNAPSHOT_CLIENT_TIMEOUT.as_secs()
            ));
        }
        match rpc {
            Some(rpc) if rpc.code == 1 && rpc.message.contains("unknown method") => {
                LinearFailure::MethodNotFound
            }
            Some(rpc) => LinearFailure::Failed(rpc.message.clone()),
            None => LinearFailure::Failed(format!("{error:#}")),
        }
    })
}

impl Driver {
    /// Run `read` on a worker against a fresh connection when the client can
    /// reconnect, else synchronously, and feed the answer as an arrival. Every
    /// Linear read shares this, so a list does not rebuild the channel.
    fn run_linear_read<T: Send + 'static>(
        &mut self,
        read: impl FnOnce(&mut dyn BoardClient) -> anyhow::Result<T> + Send + 'static,
        wrap: impl FnOnce(Result<T, LinearFailure>) -> LinearArrival + Send + 'static,
    ) {
        match (self.client.reconnect_path(), self.linear_tx.clone()) {
            (Some(path), Some(tx)) => {
                // Bounded, so a daemon that never answers cannot hold a read
                // in flight forever and refuse every later one. The
                // connection is dropped with the thread, which tells the
                // daemon to stop the script.
                std::thread::spawn(move || {
                    let result = UnixClient::connect(&path).and_then(|mut client| {
                        client.set_read_timeout(Some(LINEAR_SNAPSHOT_CLIENT_TIMEOUT))?;
                        read(&mut client)
                    });
                    let _ = tx.send(wrap(classify(result)));
                });
            }
            _ => {
                let result = read(self.client.as_mut());
                self.handle(Msg::LinearArrived(Box::new(wrap(classify(result)))));
            }
        }
    }

    pub(super) fn fetch_linear_snapshot(&mut self) {
        if let Some(pending) = self.deferred_linear.as_mut() {
            pending.push_back(Pending::Snapshot);
            return;
        }
        let Some(params) = self.linear_params() else {
            return;
        };
        self.run_linear_read(
            move |client| client.linear_snapshot(&params),
            |result| LinearArrival::Snapshot(Box::new(result)),
        );
    }

    pub(super) fn fetch_linear_list(&mut self, kind: LinearListKind, id: Option<String>) {
        if let Some(pending) = self.deferred_linear.as_mut() {
            pending.push_back(Pending::List { kind, id });
            return;
        }
        let params = self.list_params(kind, id.clone());
        self.run_linear_read(
            move |client| client.linear_list(&params),
            move |result| LinearArrival::List { kind, id, result },
        );
    }

    /// Open the Linear picker for `kind` (`id` is a views list's project id)
    /// and read its list. The read goes straight to the client rather than
    /// through `Effect::LinearList`, which the strip keys use.
    pub fn open_linear_picker(&mut self, kind: LinearListKind, id: Option<String>) {
        if crate::app::open_linear_picker(&mut self.app, kind, id.clone()) {
            self.fetch_linear_list(kind, id);
        }
    }

    fn linear_params(&self) -> Option<LinearSnapshotParams> {
        let workspace_id = self.app.linear.as_ref()?.workspace_id.clone();
        Some(LinearSnapshotParams {
            workspace_id,
            origin_socket: self.origin.origin_socket.clone(),
            plugin_root: self.origin.plugin_root.clone(),
        })
    }

    fn list_params(&self, kind: LinearListKind, id: Option<String>) -> LinearListParams {
        LinearListParams {
            kind,
            id,
            origin_socket: self.origin.origin_socket.clone(),
            plugin_root: self.origin.plugin_root.clone(),
        }
    }

    /// Feed every answer the workers delivered since the last call.
    pub fn drain_linear_arrivals(&mut self) {
        let mut arrived = Vec::new();
        if let Some(rx) = &self.linear_rx {
            while let Ok(arrival) = rx.try_recv() {
                arrived.push(arrival);
            }
        }
        for arrival in arrived {
            self.handle(Msg::LinearArrived(Box::new(arrival)));
        }
    }

    /// Hold Linear reads instead of running them, so a test can observe the
    /// in-flight state against a synchronous client.
    pub fn defer_linear_snapshots(&mut self) {
        self.deferred_linear = Some(Default::default());
    }

    fn take_pending(&mut self, want: impl Fn(&Pending) -> bool) -> Option<Pending> {
        let pending = self.deferred_linear.as_mut()?;
        let at = pending.iter().position(want)?;
        pending.remove(at)
    }

    /// Run the oldest held snapshot fetch synchronously and feed its arrival.
    /// Returns whether one was pending.
    pub fn deliver_pending_linear_snapshot(&mut self) -> bool {
        if self
            .take_pending(|p| matches!(p, Pending::Snapshot))
            .is_none()
        {
            return false;
        }
        let Some(params) = self.linear_params() else {
            return false;
        };
        let result = self.client.linear_snapshot(&params);
        self.handle(Msg::LinearArrived(Box::new(LinearArrival::Snapshot(
            Box::new(classify(result)),
        ))));
        true
    }

    /// Run the oldest held list read synchronously and feed its arrival.
    /// Returns whether one was pending.
    pub fn deliver_pending_linear_list(&mut self) -> bool {
        let Some(Pending::List { kind, id }) =
            self.take_pending(|p| matches!(p, Pending::List { .. }))
        else {
            return false;
        };
        let result = self.client.linear_list(&self.list_params(kind, id.clone()));
        self.handle(Msg::LinearArrived(Box::new(LinearArrival::List {
            kind,
            id,
            result: classify(result),
        })));
        true
    }

    /// The runtime's coalesced subscription signals for one loop iteration.
    /// Upstream mode: a reconnect installs a fresh request client and forces
    /// one refetch; a change refetches. Linear mode: a change is ignored and
    /// a reconnect is the one automatic snapshot request (R21).
    pub fn on_daemon_signals(&mut self, changed: bool, reconnected: bool) {
        if self.app.mode == Mode::Linear {
            if reconnected {
                if let Some(path) = self.reconnect_path() {
                    self.reconnect(&path);
                }
                self.handle(Msg::LinearRefresh);
            }
            return;
        }
        let mut refreshed = changed;
        if reconnected {
            refreshed = self
                .reconnect_path()
                .as_deref()
                .is_some_and(|path| self.reconnect(path));
        }
        if refreshed {
            self.handle(Msg::Refresh);
        }
    }

    pub(super) fn focus_pane(&mut self, pane_id: String) {
        let Some(origin_socket) = self.origin.origin_socket.clone() else {
            self.app.set_toast(
                "focus pane requires Herdr (HERDR_SOCKET_PATH is unset)",
                true,
            );
            return;
        };
        let params = PaneFocusParams {
            origin_socket,
            pane_id: pane_id.clone(),
        };
        match self.client.pane_focus(&params) {
            Ok(result) if result.gone => self.app.set_toast("pane is closed; refresh", true),
            Ok(_) => self.app.set_toast(format!("focused pane {pane_id}"), false),
            Err(error) => self
                .app
                .set_toast(format!("pane {pane_id}: {error:#}"), true),
        }
    }

    pub(super) fn open_issue_url(&mut self, url: String) {
        // The URL is document text from the plugin; `open`/`xdg-open` read a
        // leading `-` as a flag, so only an http(s) URL reaches their argv.
        if !is_http_url(&url) {
            self.app
                .set_toast("issue URL is not http(s); not opening it", true);
            return;
        }
        match self.platform.open_url(&url) {
            Ok(()) => self.app.set_toast("opening the issue in Linear", false),
            Err(error) => self.app.set_toast(format!("open failed: {error:#}"), true),
        }
    }

    pub(super) fn copy_worktree_path(&mut self, path: String, missing: bool) {
        match self.platform.copy_text(&path) {
            Ok(()) if missing => self.app.set_toast("copied; directory is gone", false),
            Ok(()) => self.app.set_toast("copied", false),
            Err(error) => self.app.set_toast(format!("copy failed: {error:#}"), true),
        }
    }
}

pub(crate) fn is_http_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

pub(super) type ArrivalChannel = (mpsc::Sender<LinearArrival>, mpsc::Receiver<LinearArrival>);

pub(super) fn arrival_channel() -> ArrivalChannel {
    mpsc::channel()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_timeout_is_named_as_no_answer_from_the_daemon() {
        let io = std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "Resource temporarily unavailable",
        );
        match classify::<()>(Err(anyhow::Error::new(io))) {
            Err(LinearFailure::Failed(text)) => {
                assert!(text.contains("did not answer within"), "{text}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_daemon_error_is_not_mistaken_for_a_timeout() {
        let rpc = RpcClientError::new(6, None, "plugin unavailable".into(), None);
        match classify::<()>(Err(anyhow::Error::new(rpc))) {
            Err(LinearFailure::Failed(text)) => assert_eq!(text, "plugin unavailable"),
            other => panic!("{other:?}"),
        }
    }
}
