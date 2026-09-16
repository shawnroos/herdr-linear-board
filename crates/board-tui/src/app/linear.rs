//! Linear mode: the state and reducer for a herdr space bound to a Linear
//! project. The document is the work plugin's snapshot (`linear.snapshot`);
//! nothing here reads `App::board`, and no effect emitted here writes to
//! Linear, to the plugin's records, or to the herdr layout.

use board_core::protocol::{
    LinearGroup, LinearIssue, LinearListEnvelope, LinearListKind, LinearListResult, LinearSnapshot,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::nav::{nav_delta, step_clamped};
use super::{App, Effect, Msg, Screen};

/// Why a snapshot request failed, as the driver classifies the client error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearFailure {
    /// The daemon answered "unknown method": it predates `linear.snapshot`.
    MethodNotFound,
    /// Any other failure, with the daemon's or the transport's message.
    Failed(String),
}

/// What a Linear worker delivers: one snapshot or one list, each classified.
#[derive(Debug)]
pub enum LinearArrival {
    Snapshot(Box<Result<LinearSnapshot, LinearFailure>>),
    List {
        kind: LinearListKind,
        id: Option<String>,
        result: Result<LinearListResult, LinearFailure>,
    },
}

/// One pane row of the detail screen: which binding it belongs to, its id,
/// and the live status the daemon attached (`unknown` when it attached none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRow {
    pub binding: usize,
    pub pane_id: String,
    pub status: String,
}

pub struct LinearState {
    pub workspace_id: String,
    pub board_version: String,
    pub daemon_version: Option<String>,
    /// The last document that arrived, sanitised. Stays on screen when a
    /// later refresh fails (R19).
    pub last_good: Option<LinearSnapshot>,
    pub sel_group: usize,
    pub sel_card: usize,
    /// The identifier open on `Screen::LinearDetail`.
    pub detail: Option<String>,
    pub pane_cursor: usize,
    pub in_flight: bool,
    /// An automatic refresh (a reconnect) asked for while one was in flight;
    /// it is sent when that one lands, so it is not lost.
    pub queued: bool,
    pub error: Option<String>,
    /// `(board_version, daemon_version)` once the daemon answered that it has
    /// no `linear.snapshot` (R25).
    pub stale_daemon: Option<(String, Option<String>)>,
    /// `App::now` when `last_good` arrived.
    pub fetched_at: Option<i64>,
    /// Lists with a read on the way, keyed by kind and the list's argument.
    pub lists_in_flight: std::collections::BTreeSet<(LinearListKind, Option<String>)>,
    /// The row last chosen in a Linear picker, for the flow that opened it.
    pub pick: Option<super::LinearPick>,
}

impl LinearState {
    pub fn new(
        workspace_id: String,
        board_version: String,
        daemon_version: Option<String>,
    ) -> LinearState {
        LinearState {
            workspace_id,
            board_version,
            daemon_version,
            last_good: None,
            sel_group: 0,
            sel_card: 0,
            detail: None,
            pane_cursor: 0,
            in_flight: false,
            queued: false,
            error: None,
            stale_daemon: None,
            fetched_at: None,
            lists_in_flight: Default::default(),
            pick: None,
        }
    }

    pub fn list_in_flight(&self, kind: LinearListKind, id: Option<&str>) -> bool {
        self.lists_in_flight
            .iter()
            .any(|(k, i)| *k == kind && i.as_deref() == id)
    }

    pub fn snapshot(&self) -> Option<&LinearSnapshot> {
        self.last_good.as_ref()
    }

    /// Only a `bound` record reaches Linear; every other record state (and
    /// an unreadable record, whose state is null) is the not-bound screen.
    pub fn bound(&self) -> bool {
        self.snapshot()
            .is_some_and(|s| s.record.state.as_deref() == Some("bound"))
    }

    pub fn groups(&self) -> &[LinearGroup] {
        self.snapshot().map(|s| s.groups.as_slice()).unwrap_or(&[])
    }

    pub fn issue(&self, identifier: &str) -> Option<&LinearIssue> {
        self.snapshot()?.issues.get(identifier)
    }

    pub fn selected_identifier(&self) -> Option<&str> {
        self.groups()
            .get(self.sel_group)?
            .issues
            .get(self.sel_card)
            .map(String::as_str)
    }

    pub fn selected_issue(&self) -> Option<&LinearIssue> {
        self.issue(self.selected_identifier()?)
    }

    pub fn detail_issue(&self) -> Option<&LinearIssue> {
        self.issue(self.detail.as_deref()?)
    }

    pub fn pane_status(&self, pane_id: &str) -> &str {
        self.snapshot()
            .and_then(|s| s.pane_status.get(pane_id))
            .map(String::as_str)
            .unwrap_or("unknown")
    }

    pub fn pane_count(issue: &LinearIssue) -> usize {
        issue.bindings.iter().map(|b| b.panes.len()).sum()
    }

    /// Every pane of `issue`'s bindings, in document order.
    pub fn pane_rows(&self, issue: &LinearIssue) -> Vec<PaneRow> {
        issue
            .bindings
            .iter()
            .enumerate()
            .flat_map(|(binding, b)| {
                b.panes.iter().map(move |pane_id| PaneRow {
                    binding,
                    pane_id: pane_id.clone(),
                    status: self.pane_status(pane_id).to_string(),
                })
            })
            .collect()
    }

    pub fn detail_pane_rows(&self) -> Vec<PaneRow> {
        self.detail_issue()
            .map(|issue| self.pane_rows(issue))
            .unwrap_or_default()
    }

    /// The mapping the snapshot reports, when it is not the default (R9).
    pub fn non_default_mapping(&self) -> Option<String> {
        let mapping = &self.snapshot()?.mapping;
        // A document with no mapping section has nothing to report.
        if mapping.source.is_empty() {
            return None;
        }
        let default = ("default", "project", "work", "session");
        let actual = (
            mapping.source.as_str(),
            mapping.space.as_str(),
            mapping.tab.as_str(),
            mapping.pane.as_str(),
        );
        (actual != default).then(|| {
            format!(
                "{}: space={} tab={} pane={}",
                mapping.source, mapping.space, mapping.tab, mapping.pane
            )
        })
    }

    /// One line per source whose status is not `ok` (R12, R19).
    pub fn source_warnings(&self) -> Vec<String> {
        let Some(s) = self.snapshot() else {
            return vec![];
        };
        let mut out = Vec::new();
        match s.linear.status.as_str() {
            "ok" | "unknown" => {}
            "unavailable" => out.push("Linear unavailable".to_string()),
            "truncated" => out.push("Linear listing truncated".to_string()),
            other => out.push(format!("Linear {other}")),
        }
        // An empty status is a section the document left out, not a failure.
        match s.herdr.status.as_str() {
            "ok" | "" => {}
            "unavailable" => out.push("herdr unavailable".to_string()),
            other => out.push(format!("herdr {other}")),
        }
        if s.record.status == "unreadable" {
            out.push("record unreadable".to_string());
        }
        match s.view.status.as_str() {
            "ok" | "none" => {}
            "unsupported_grouping" => out.push(format!(
                "view unsupported grouping {}",
                s.view
                    .layout
                    .as_ref()
                    .map(|l| l.grouping.as_str())
                    .unwrap_or("?")
            )),
            other => out.push(format!("view {}", other.replace('_', " "))),
        }
        if !matches!(s.mapping.status.as_str(), "ok" | "") {
            out.push(format!("mapping {}", s.mapping.status));
        }
        out
    }

    fn clamp(&mut self) {
        let n = self.groups().len();
        if self.sel_group >= n {
            self.sel_group = n.saturating_sub(1);
        }
        let cards = self
            .groups()
            .get(self.sel_group)
            .map_or(0, |g| g.issues.len());
        if self.sel_card >= cards {
            self.sel_card = cards.saturating_sub(1);
        }
        if self
            .detail
            .as_deref()
            .is_some_and(|id| self.issue(id).is_none())
        {
            self.detail = None;
        }
        let panes = self.detail_issue().map_or(0, Self::pane_count);
        if self.pane_cursor >= panes {
            self.pane_cursor = panes.saturating_sub(1);
        }
    }

    pub(super) fn home_screen(&self) -> Screen {
        if !self.bound() {
            Screen::LinearNotBound
        } else if self.detail.is_some() {
            Screen::LinearDetail
        } else {
            Screen::LinearBoard
        }
    }
}

pub(super) fn update_linear(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        // `board_changed` never refreshes a Linear board (R21).
        Msg::Refresh | Msg::Mouse(_) => vec![],
        Msg::LinearRefresh => request_or_queue(app),
        Msg::LinearArrived(arrival) => match *arrival {
            LinearArrival::Snapshot(result) => arrived(app, *result),
            LinearArrival::List { kind, id, result } => {
                super::linear_picker::list_arrived(app, kind, id, result);
                vec![]
            }
        },
        Msg::Key(k) => linear_key(app, k),
    }
}

fn request_snapshot(app: &mut App) -> Vec<Effect> {
    let Some(in_flight) = app.linear.as_ref().map(|s| s.in_flight) else {
        return vec![];
    };
    if in_flight {
        app.set_toast("refresh already in flight", false);
        return vec![];
    }
    if let Some(state) = app.linear.as_mut() {
        state.in_flight = true;
    }
    vec![Effect::LinearSnapshot]
}

/// An automatic refresh: sent now, or queued behind the one in flight.
fn request_or_queue(app: &mut App) -> Vec<Effect> {
    match app.linear.as_mut() {
        Some(state) if state.in_flight => {
            state.queued = true;
            vec![]
        }
        Some(_) => request_snapshot(app),
        None => vec![],
    }
}

fn arrived(app: &mut App, result: Result<LinearSnapshot, LinearFailure>) -> Vec<Effect> {
    let now = app.now;
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.in_flight = false;
    let follow_up = std::mem::take(&mut state.queued);
    let mut effects = vec![];
    let screen = match result {
        Ok(mut snapshot) => {
            sanitise_snapshot(&mut snapshot);
            effects.push(Effect::SetLinearPaneTitle(crate::view::linear_pane_title(
                &snapshot,
                &state.workspace_id,
            )));
            state.error = None;
            state.stale_daemon = None;
            state.fetched_at = Some(now);
            state.last_good = Some(snapshot);
            state.clamp();
            state.home_screen()
        }
        Err(LinearFailure::MethodNotFound) => {
            state.stale_daemon = Some((state.board_version.clone(), state.daemon_version.clone()));
            Screen::LinearStaleDaemon
        }
        Err(LinearFailure::Failed(text)) => {
            state.error = Some(sanitise(&text));
            Screen::LinearError
        }
    };
    // An overlay open when a snapshot lands stays open (R19): the new home
    // screen becomes where closing it goes.
    match app.screen {
        Screen::LinearPicker if app.picker.is_some() => {
            if let Some(picker) = app.picker.as_mut() {
                picker.return_to = screen;
            }
        }
        Screen::Help => app.help_return_to = screen,
        _ => app.screen = screen,
    }
    if follow_up {
        effects.extend(request_snapshot(app));
    }
    effects
}

fn linear_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    // Same rule as the upstream reducer: a fast `Esc <key>` arrives as
    // `Alt+<key>`, which would otherwise open the browser or focus a pane.
    if k.modifiers.intersects(
        KeyModifiers::CONTROL
            | KeyModifiers::ALT
            | KeyModifiers::META
            | KeyModifiers::SUPER
            | KeyModifiers::HYPER,
    ) {
        return vec![];
    }
    // Before the global keys: in a picker every printable key is filter text,
    // so `r` must not fetch and `?` must not open help (KTD7).
    if app.screen == Screen::LinearPicker {
        return super::linear_picker::linear_picker_key(app, k);
    }
    if k.code == KeyCode::Char('?') && app.screen != Screen::Help {
        app.help_return_to = app.screen;
        app.help_scroll = 0;
        app.screen = Screen::Help;
        return vec![];
    }
    if matches!(k.code, KeyCode::Char('r') | KeyCode::Char('R')) && app.screen != Screen::Help {
        return request_snapshot(app);
    }
    match app.screen {
        Screen::Help => {
            app.screen = app.help_return_to;
            vec![]
        }
        Screen::LinearBoard => board_key(app, k),
        Screen::LinearDetail => detail_key(app, k),
        Screen::LinearError => {
            match k.code {
                KeyCode::Char('q') => return vec![Effect::Quit],
                KeyCode::Esc => {
                    if let Some(state) = app.linear.as_ref() {
                        if state.last_good.is_some() {
                            app.screen = state.home_screen();
                        }
                    }
                }
                _ => {}
            }
            vec![]
        }
        Screen::LinearNotBound | Screen::LinearStaleDaemon => match k.code {
            KeyCode::Char('q') | KeyCode::Esc => vec![Effect::Quit],
            _ => vec![],
        },
        _ => vec![],
    }
}

fn board_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    if let Some(delta) = nav_delta(k.code) {
        let cards = state
            .groups()
            .get(state.sel_group)
            .map_or(0, |g| g.issues.len());
        state.sel_card = step_clamped(state.sel_card, delta, cards.saturating_sub(1));
        return vec![];
    }
    match k.code {
        KeyCode::Left | KeyCode::Char('h') => {
            state.sel_group = step_clamped(state.sel_group, -1, 0);
            state.clamp();
        }
        KeyCode::Right | KeyCode::Char('l') => {
            let max = state.groups().len().saturating_sub(1);
            state.sel_group = step_clamped(state.sel_group, 1, max);
            state.clamp();
        }
        KeyCode::Enter => {
            if let Some(id) = state.selected_identifier().map(str::to_string) {
                state.detail = Some(id);
                let rows = state.detail_pane_rows();
                state.pane_cursor = rows
                    .iter()
                    .position(|row| row.status == "working")
                    .unwrap_or(0);
                app.screen = Screen::LinearDetail;
            }
        }
        KeyCode::Char('q') | KeyCode::Esc => return vec![Effect::Quit],
        _ => {}
    }
    vec![]
}

fn detail_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    if let Some(delta) = nav_delta(k.code) {
        let max = state
            .detail_issue()
            .map_or(0, LinearState::pane_count)
            .saturating_sub(1);
        state.pane_cursor = step_clamped(state.pane_cursor, delta, max);
        return vec![];
    }
    match k.code {
        KeyCode::Char('o') => {
            let rows = state.detail_pane_rows();
            match rows.get(state.pane_cursor) {
                Some(row) => return vec![Effect::FocusPane(row.pane_id.clone())],
                None => app.set_toast("this card has no recorded pane", true),
            }
        }
        KeyCode::Char('u') => match state.detail_issue().and_then(|i| i.url.clone()) {
            Some(url) => return vec![Effect::OpenIssueUrl(url)],
            None => app.set_toast("this card has no Linear URL (cached issue)", true),
        },
        KeyCode::Char('y') => {
            let rows = state.detail_pane_rows();
            let binding_index = rows.get(state.pane_cursor).map_or(0, |row| row.binding);
            let binding = state
                .detail_issue()
                .and_then(|issue| issue.bindings.get(binding_index))
                .cloned();
            match binding {
                Some(binding) => {
                    return vec![Effect::CopyWorktreePath {
                        path: binding.worktree_path,
                        missing: binding.state == "worktree_missing",
                    }]
                }
                None => app.set_toast("this card has no worktree binding", true),
            }
        }
        KeyCode::Char('q') | KeyCode::Esc => {
            state.detail = None;
            app.screen = Screen::LinearBoard;
        }
        _ => {}
    }
    vec![]
}

// -- sanitising -------------------------------------------------------------

/// The plugin's `HERDR_LINEAR_SANITIZE_JQ_DEF` codepoint set: C0 except tab
/// and newline, DEL, C1, and the invisible format characters. Applied to
/// every document string and to error text before rendering (R22).
pub fn sanitise(s: &str) -> String {
    s.chars().filter(|c| !is_stripped(*c)).collect()
}

fn is_stripped(c: char) -> bool {
    (c.is_control() && c != '\t' && c != '\n') || board_core::text::is_format_char(c)
}

fn sanitise_value(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(text) => Value::String(sanitise(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(sanitise_value).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (sanitise(&key), sanitise_value(item)))
                .collect(),
        ),
        other => other,
    }
}

/// Every string in the document, keys included, through [`sanitise`]. It walks
/// the serialised form rather than the struct's fields, so a string field
/// added to any Linear type is covered without a line here (R22).
pub fn sanitise_snapshot(snapshot: &mut LinearSnapshot) {
    let cleaned = serde_json::to_value(&*snapshot)
        .map(sanitise_value)
        .and_then(serde_json::from_value::<LinearSnapshot>);
    match cleaned {
        Ok(cleaned) => *snapshot = cleaned,
        // The document round-trips through its own types, so this does not
        // happen; if it did, nothing unsanitised may reach the screen.
        Err(_) => *snapshot = LinearSnapshot::default(),
    }
}

/// The same walk over a list envelope (R24).
pub fn sanitise_list(result: LinearListResult) -> LinearListResult {
    let kind = result.kind();
    let cleaned = serde_json::to_value(&result)
        .map(sanitise_value)
        .and_then(|value| LinearListResult::from_value(kind, value));
    match cleaned {
        Ok(cleaned) => cleaned,
        // Same reasoning as the snapshot: an unknown-status empty envelope,
        // never the unsanitised one.
        Err(_) => match kind {
            LinearListKind::Spaces => LinearListResult::Spaces(LinearListEnvelope::default()),
            LinearListKind::Projects => LinearListResult::Projects(LinearListEnvelope::default()),
            LinearListKind::Views => LinearListResult::Views(LinearListEnvelope::default()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_strips_bidi_override_and_escape_but_keeps_text() {
        let title = "Fix\u{202E}drawer\u{1b}[31m blank\u{FEFF} state\tnow\n";
        assert_eq!(sanitise(title), "Fixdrawer[31m blank state\tnow\n");
    }

    #[test]
    fn sanitise_snapshot_reaches_issue_keys_and_pane_status() {
        let mut snapshot = LinearSnapshot::default();
        snapshot.issues.insert(
            "WEB-1\u{200B}".into(),
            LinearIssue {
                title: "a\u{7f}b".into(),
                ..Default::default()
            },
        );
        snapshot
            .pane_status
            .insert("p\u{00AD}1".into(), "work\u{2066}ing".into());
        sanitise_snapshot(&mut snapshot);
        assert_eq!(snapshot.issues["WEB-1"].title, "ab");
        assert_eq!(snapshot.pane_status["p1"], "working");
    }
}
