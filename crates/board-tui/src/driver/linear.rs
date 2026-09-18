//! Linear-mode effects: the snapshot fetch (worker thread in production,
//! synchronous against a client with no reconnect path), `pane.focus`, the
//! URL opener and the clipboard, plus the daemon-signal policy the runtime
//! delegates to.

use std::sync::mpsc;

use board_core::client::{BoardClient, RpcClientError, UnixClient};
use board_core::protocol::{
    LinearBindHandoffParams, LinearIssueParams, LinearListKind, LinearListParams,
    LinearSnapshotParams, PaneFocusParams, LINEAR_BIND_HANDOFF_CLIENT_TIMEOUT,
    LINEAR_ISSUE_CLIENT_TIMEOUT, LINEAR_SNAPSHOT_CLIENT_TIMEOUT,
};
use std::time::Duration;

use crate::app::{BindTarget, LinearArrival, LinearFailure, Mode, Msg};
use crate::Driver;

/// A read held by the test hook instead of running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Pending {
    Snapshot,
    Issue(String),
    List {
        kind: LinearListKind,
        id: Option<String>,
    },
    Handoff(LinearBindHandoffParams),
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
            | Effect::LinearIssue { .. }
            | Effect::FocusPane(_)
            | Effect::OpenIssueUrl(_)
            | Effect::CopyWorktreePath { .. }
            | Effect::SetLinearPaneTitle(_)
            | Effect::BindHandoff { .. }
            | Effect::Quit
    )
}

/// The effects Linear mode refuses, named one by one. With [`linear_allows`]
/// this classifies every effect: the match has no catch-all, so a new variant
/// does not build until someone places it.
#[cfg(test)]
pub(super) fn linear_denies(eff: &crate::app::Effect) -> bool {
    use crate::app::Effect;
    match eff {
        Effect::Refetch
        | Effect::LinearSnapshot
        | Effect::LinearList { .. }
        | Effect::LinearIssue { .. }
        | Effect::FocusPane(_)
        | Effect::OpenIssueUrl(_)
        | Effect::CopyWorktreePath { .. }
        | Effect::SetLinearPaneTitle(_)
        | Effect::BindHandoff { .. }
        | Effect::Quit => false,
        Effect::LoadProjects
        | Effect::LoadProjectPicker
        | Effect::LoadBoardPicker { .. }
        | Effect::SelectProject { .. }
        | Effect::SelectBoard(_)
        | Effect::ProjectCreate(_)
        | Effect::BoardCreate(_)
        | Effect::LoadMoveColumns { .. }
        | Effect::LoadDetail(_)
        | Effect::CardCreate(_)
        | Effect::CardUpdate(_)
        | Effect::CardDelete(_)
        | Effect::CardDuplicate(_)
        | Effect::CardArchive { .. }
        | Effect::BoardArchive { .. }
        | Effect::ProjectArchive { .. }
        | Effect::CardMove(_)
        | Effect::ColumnCreate(_)
        | Effect::ColumnUpdate(_)
        | Effect::ColumnReorder { .. }
        | Effect::ColumnDelete { .. }
        | Effect::CommentAdd { .. }
        | Effect::CommentUpdate { .. }
        | Effect::CommentDelete { .. }
        | Effect::LoadCommentHistory { .. }
        | Effect::TemplateApply(_)
        | Effect::RunCancel(_)
        | Effect::RunRetry(_)
        | Effect::RunDone(..)
        | Effect::FocusRun(..)
        | Effect::EditFocusedTextArea
        | Effect::LoadFormOptions
        | Effect::SetPaneTitle(_)
        | Effect::ReloadPickers => true,
    }
}

/// The daemon reports an unknown method as protocol code 1 with the message
/// `bad request: unknown method: <name>`; code 1 alone also covers bad params.
pub(crate) fn classify<T>(
    result: anyhow::Result<T>,
    timeout: Duration,
) -> Result<T, LinearFailure> {
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
            return LinearFailure::TimedOut(timeout);
        }
        // A local client answers with `board_core::Error` itself rather than a
        // wire error, so the code is read from whichever arrived. Keying only
        // on the wire error left the whole in-process tier unable to see a
        // code at all, which is where this distinction is tested.
        let local_code = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<board_core::Error>())
            .map(|e| (e.code(), e.to_string()));
        let code_and_message = rpc
            .map(|rpc| (rpc.code, rpc.message.clone()))
            .or(local_code);
        match code_and_message {
            Some((1, message)) if message.contains("unknown method") => {
                LinearFailure::MethodNotFound
            }
            // Code 7 is "the plugin ships no script for this op", which is
            // fixed by updating the plugin rather than by retrying.
            Some((7, message)) => LinearFailure::OpUnsupported(message),
            Some((_, message)) => LinearFailure::Failed(message),
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
        timeout: Duration,
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
                        client.set_read_timeout(Some(timeout))?;
                        read(&mut client)
                    });
                    let _ = tx.send(wrap(classify(result, timeout)));
                });
            }
            _ => {
                let result = read(self.client.as_mut());
                self.handle(Msg::LinearArrived(Box::new(wrap(classify(
                    result, timeout,
                )))));
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
            LINEAR_SNAPSHOT_CLIENT_TIMEOUT,
            move |client| client.linear_snapshot(&params),
            |result| LinearArrival::Snapshot(Box::new(result)),
        );
    }

    pub(super) fn fetch_linear_issue(&mut self, issue: String) {
        if let Some(pending) = self.deferred_linear.as_mut() {
            pending.push_back(Pending::Issue(issue));
            return;
        }
        let params = LinearIssueParams {
            issue: issue.clone(),
            origin_socket: self.origin.origin_socket.clone(),
            plugin_root: self.origin.plugin_root.clone(),
        };
        self.run_linear_read(
            LINEAR_ISSUE_CLIENT_TIMEOUT,
            move |client| client.linear_issue(&params),
            move |result| LinearArrival::Issue {
                issue,
                result: Box::new(result),
            },
        );
    }

    pub(super) fn fetch_linear_list(&mut self, kind: LinearListKind, id: Option<String>) {
        if let Some(pending) = self.deferred_linear.as_mut() {
            pending.push_back(Pending::List { kind, id });
            return;
        }
        let params = self.list_params(kind, id.clone());
        self.run_linear_read(
            LINEAR_SNAPSHOT_CLIENT_TIMEOUT,
            move |client| client.linear_list(&params),
            move |result| LinearArrival::List { kind, id, result },
        );
    }

    /// Start `linear.bind_handoff` off the event loop. It can take as long as
    /// a slow new pane's shell, so it runs on a worker like the reads.
    pub(super) fn start_bind_handoff(&mut self, params: BindTarget) {
        let Some(origin_socket) = self.origin.origin_socket.clone() else {
            self.handle(Msg::LinearArrived(Box::new(LinearArrival::Handoff(Err(
                LinearFailure::Failed(
                    "binding requires Herdr (HERDR_SOCKET_PATH is unset)".to_string(),
                ),
            )))));
            return;
        };
        let params = LinearBindHandoffParams {
            space: params.space,
            project: params.project,
            view: params.view,
            issue: params.issue,
            working_directory: params.working_directory,
            origin_socket,
        };
        if let Some(pending) = self.deferred_linear.as_mut() {
            pending.push_back(Pending::Handoff(params));
            return;
        }
        self.run_linear_read(
            LINEAR_BIND_HANDOFF_CLIENT_TIMEOUT,
            move |client| client.linear_bind_handoff(&params),
            LinearArrival::Handoff,
        );
    }

    /// Open the Linear picker for `kind` (`id` is a views list's project id)
    /// and read its list through the effect gate.
    pub fn open_linear_picker(&mut self, kind: LinearListKind, id: Option<String>) {
        if crate::app::open_linear_picker(&mut self.app, kind, id.clone()) {
            self.dispatch(crate::app::Effect::LinearList { kind, id });
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
            Box::new(classify(result, LINEAR_SNAPSHOT_CLIENT_TIMEOUT)),
        ))));
        true
    }

    /// Run the oldest held handoff synchronously and feed its arrival.
    /// Returns whether one was pending.
    pub fn deliver_pending_linear_handoff(&mut self) -> bool {
        let Some(Pending::Handoff(params)) =
            self.take_pending(|p| matches!(p, Pending::Handoff(_)))
        else {
            return false;
        };
        let result = self.client.linear_bind_handoff(&params);
        self.handle(Msg::LinearArrived(Box::new(LinearArrival::Handoff(
            classify(result, LINEAR_BIND_HANDOFF_CLIENT_TIMEOUT),
        ))));
        true
    }

    /// Run the oldest held issue read synchronously and feed its arrival.
    /// Returns whether one was pending. A test drives overlapping reads by
    /// holding two and delivering them in the order it wants.
    pub fn deliver_pending_linear_issue(&mut self) -> bool {
        let Some(Pending::Issue(issue)) = self.take_pending(|p| matches!(p, Pending::Issue(_)))
        else {
            return false;
        };
        let params = LinearIssueParams {
            issue: issue.clone(),
            origin_socket: self.origin.origin_socket.clone(),
            plugin_root: self.origin.plugin_root.clone(),
        };
        let result = self.client.linear_issue(&params);
        self.handle(Msg::LinearArrived(Box::new(LinearArrival::Issue {
            issue,
            result: Box::new(classify(result, LINEAR_ISSUE_CLIENT_TIMEOUT)),
        })));
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
            result: classify(result, LINEAR_SNAPSHOT_CLIENT_TIMEOUT),
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
    use crate::app::{CardFilter, Effect};
    use board_core::protocol::{
        BoardCreateParams, CardCreateParams, CardMoveParams, CardUpdateParams, ColumnCreateParams,
        ColumnUpdateParams, ProjectCreateParams, RunOutcome,
    };

    /// One of every effect. `linear_denies` has no catch-all, so a variant
    /// missing here but added there still has to be classified to build.
    fn one_of_each() -> Vec<Effect> {
        let s = || "x".to_string();
        vec![
            Effect::Refetch,
            Effect::LoadProjects,
            Effect::LoadProjectPicker,
            Effect::LoadBoardPicker { project_id: None },
            Effect::SelectProject {
                project_id: 1,
                board_id: 1,
            },
            Effect::SelectBoard(1),
            Effect::ProjectCreate(ProjectCreateParams { scope_path: s() }),
            Effect::BoardCreate(BoardCreateParams {
                project_id: 1,
                name: s(),
            }),
            Effect::LoadMoveColumns { board_id: 1 },
            Effect::LoadDetail(1),
            Effect::CardCreate(CardCreateParams::default()),
            Effect::CardUpdate(CardUpdateParams::default()),
            Effect::CardDelete(1),
            Effect::CardDuplicate(1),
            Effect::CardArchive {
                id: 1,
                archived: true,
            },
            Effect::BoardArchive {
                board_id: 1,
                archived: true,
            },
            Effect::ProjectArchive {
                project_id: 1,
                archived: true,
            },
            Effect::CardMove(CardMoveParams {
                id: 1,
                column_id: 1,
                board_id: None,
                position: None,
            }),
            Effect::ColumnCreate(ColumnCreateParams::default()),
            Effect::ColumnUpdate(ColumnUpdateParams::default()),
            Effect::ColumnReorder { id: 1, position: 0 },
            Effect::ColumnDelete {
                id: 1,
                move_cards_to: None,
            },
            Effect::CommentAdd {
                card_id: 1,
                body: s(),
            },
            Effect::CommentUpdate { id: 1, body: s() },
            Effect::CommentDelete { id: 1 },
            Effect::LoadCommentHistory { id: 1 },
            Effect::TemplateApply(s()),
            Effect::RunCancel(1),
            Effect::RunRetry(1),
            Effect::RunDone(1, RunOutcome::Ok),
            Effect::FocusRun(1, 1),
            Effect::EditFocusedTextArea,
            Effect::LoadFormOptions,
            Effect::SetPaneTitle(CardFilter::Active),
            Effect::SetLinearPaneTitle(s()),
            Effect::ReloadPickers,
            Effect::Quit,
            Effect::LinearSnapshot,
            Effect::LinearList {
                kind: LinearListKind::Spaces,
                id: None,
            },
            Effect::LinearIssue { issue: s() },
            Effect::BindHandoff {
                space: s(),
                project: s(),
                view: None,
                issue: None,
                working_directory: None,
            },
            Effect::FocusPane(s()),
            Effect::OpenIssueUrl(s()),
            Effect::CopyWorktreePath {
                path: s(),
                missing: false,
            },
        ]
    }

    #[test]
    fn every_effect_is_either_allowed_or_named_in_the_deny_list() {
        let effects = one_of_each();
        let kinds: std::collections::BTreeSet<_> = effects
            .iter()
            .map(|e| format!("{:?}", std::mem::discriminant(e)))
            .collect();
        assert_eq!(kinds.len(), effects.len(), "one sample per variant");
        for eff in &effects {
            assert_ne!(
                linear_allows(eff),
                linear_denies(eff),
                "{:?} must be allowed or denied, not both or neither",
                std::mem::discriminant(eff)
            );
        }
        let allowed = effects.iter().filter(|e| linear_allows(e)).count();
        // 10 since the issue page: `linear.issue` is the tenth read Linear mode
        // may make. The number is pinned so widening what this mode can do is a
        // deliberate edit rather than a side effect of adding an effect.
        assert_eq!(allowed, 10, "the allow set grew or shrank");
    }

    #[cfg(feature = "fake-client")]
    #[test]
    fn every_denied_effect_toasts_and_builds_no_request() {
        use crate::testkit::{linear_driver, linear_start, methods, RecordingClient};
        let client = board_core::client::FakeBoardClient::new().unwrap();
        let (client, log) = RecordingClient::new(client);
        let (mut driver, opened, copied) = linear_driver(client, linear_start());
        let before = methods(&log);
        for eff in one_of_each().into_iter().filter(linear_denies) {
            let name = format!("{:?}", std::mem::discriminant(&eff));
            driver.app.toast = None;
            driver.apply_effect(eff);
            assert_eq!(
                driver.app.toast.as_ref().map(|t| t.text.as_str()),
                Some("not available in Linear mode"),
                "{name}"
            );
        }
        assert_eq!(methods(&log), before, "no request left");
        assert!(opened.lock().unwrap().is_empty() && copied.lock().unwrap().is_empty());
        assert!(!driver.app.should_quit);
    }

    #[test]
    fn a_client_timeout_is_classified_with_the_limit_it_was_sent_with() {
        let io = std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "Resource temporarily unavailable",
        );
        match classify::<()>(
            Err(anyhow::Error::new(io)),
            LINEAR_BIND_HANDOFF_CLIENT_TIMEOUT,
        ) {
            Err(LinearFailure::TimedOut(limit)) => {
                assert_eq!(limit, LINEAR_BIND_HANDOFF_CLIENT_TIMEOUT)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_daemon_error_is_not_mistaken_for_a_timeout() {
        let rpc = RpcClientError::new(6, None, "plugin unavailable".into(), None);
        match classify::<()>(Err(anyhow::Error::new(rpc)), LINEAR_SNAPSHOT_CLIENT_TIMEOUT) {
            Err(LinearFailure::Failed(text)) => assert_eq!(text, "plugin unavailable"),
            other => panic!("{other:?}"),
        }
    }
}
