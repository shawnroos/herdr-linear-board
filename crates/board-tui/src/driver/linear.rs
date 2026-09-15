//! Linear-mode effects: the snapshot fetch (worker thread in production,
//! synchronous against a client with no reconnect path), `pane.focus`, the
//! URL opener and the clipboard, plus the daemon-signal policy the runtime
//! delegates to.

use std::sync::mpsc;

use board_core::client::{BoardClient, RpcClientError, UnixClient};
use board_core::protocol::{LinearSnapshot, LinearSnapshotParams, PaneFocusParams};

use crate::app::{LinearFailure, Mode, Msg};
use crate::Driver;

pub(crate) type LinearArrival = Result<LinearSnapshot, LinearFailure>;

/// The only effects the driver executes in Linear mode (KTD8). Everything
/// else is refused before a request is built.
pub(super) fn linear_allows(eff: &crate::app::Effect) -> bool {
    use crate::app::Effect;
    matches!(
        eff,
        Effect::Refetch
            | Effect::LinearSnapshot
            | Effect::FocusPane(_)
            | Effect::OpenIssueUrl(_)
            | Effect::CopyWorktreePath { .. }
            | Effect::Quit
    )
}

/// The daemon reports an unknown method as protocol code 1 with the message
/// `bad request: unknown method: <name>`; code 1 alone also covers bad params.
pub(crate) fn classify(result: anyhow::Result<LinearSnapshot>) -> LinearArrival {
    result.map_err(|error| {
        let rpc = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<RpcClientError>());
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
    pub(super) fn fetch_linear_snapshot(&mut self) {
        if let Some(pending) = self.deferred_linear.as_mut() {
            *pending += 1;
            return;
        }
        let Some(workspace_id) = self.app.linear.as_ref().map(|s| s.workspace_id.clone()) else {
            return;
        };
        let params = LinearSnapshotParams {
            workspace_id,
            origin_socket: self.origin.origin_socket.clone(),
        };
        match (self.client.reconnect_path(), self.linear_tx.clone()) {
            (Some(path), Some(tx)) => {
                std::thread::spawn(move || {
                    let result = UnixClient::connect(&path)
                        .and_then(|mut client| client.linear_snapshot(&params));
                    let _ = tx.send(classify(result));
                });
            }
            _ => {
                let result = self.client.linear_snapshot(&params);
                self.handle(Msg::LinearArrived(Box::new(classify(result))));
            }
        }
    }

    /// Feed every snapshot the worker delivered since the last call.
    pub fn drain_linear_arrivals(&mut self) {
        let mut arrived = Vec::new();
        if let Some(rx) = &self.linear_rx {
            while let Ok(result) = rx.try_recv() {
                arrived.push(result);
            }
        }
        for result in arrived {
            self.handle(Msg::LinearArrived(Box::new(result)));
        }
    }

    /// Hold snapshot fetches instead of running them, so a test can observe
    /// the in-flight state against a synchronous client.
    pub fn defer_linear_snapshots(&mut self) {
        self.deferred_linear = Some(0);
    }

    /// Run one held fetch synchronously and feed its arrival. Returns whether
    /// a fetch was pending.
    pub fn deliver_pending_linear_snapshot(&mut self) -> bool {
        let Some(pending) = self.deferred_linear.as_mut() else {
            return false;
        };
        if *pending == 0 {
            return false;
        }
        *pending -= 1;
        let Some(workspace_id) = self.app.linear.as_ref().map(|s| s.workspace_id.clone()) else {
            return false;
        };
        let params = LinearSnapshotParams {
            workspace_id,
            origin_socket: self.origin.origin_socket.clone(),
        };
        let result = self.client.linear_snapshot(&params);
        self.handle(Msg::LinearArrived(Box::new(classify(result))));
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

pub(super) fn arrival_channel() -> (mpsc::Sender<LinearArrival>, mpsc::Receiver<LinearArrival>) {
    mpsc::channel()
}
