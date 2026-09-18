//! Linear mode: the state and reducer for a herdr space bound to a Linear
//! project. The document is the work plugin's snapshot (`linear.snapshot`);
//! nothing here reads `App::board`, and no effect emitted here writes to
//! Linear, to the plugin's records, or to SQLite. The herdr writes it asks the
//! daemon for are this pane's title, pane focus, and a bind handoff that opens
//! one `bind` tab; the bind skill's confirmation in that tab gates every write.

use board_core::protocol::{
    LinearBindHandoffResult, LinearBinding, LinearGroup, LinearIssue, LinearListEnvelope,
    LinearListKind, LinearListResult, LinearSnapshot, LinearSpaceRow, LinearSpacesList,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::nav::{nav_delta, step_clamped};
use super::{App, Effect, Msg, Screen};

/// Why a snapshot request failed, as the driver classifies the client error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearFailure {
    /// The daemon answered "unknown method": it predates `linear.snapshot`.
    MethodNotFound,
    /// No answer within the client's read limit for that request.
    TimedOut(std::time::Duration),
    /// Any other failure, with the daemon's or the transport's message.
    Failed(String),
}

/// A read's timeout as the board says it: `r` sends the read again.
pub(super) fn read_timeout_text(limit: std::time::Duration) -> String {
    format!(
        "the daemon did not answer within {}s; press r to try again",
        limit.as_secs()
    )
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
    Handoff(Result<LinearBindHandoffResult, LinearFailure>),
}

/// One pane row of the detail screen: which binding it belongs to, its id,
/// and the live status the daemon attached (`unknown` when it attached none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRow {
    pub binding: usize,
    pub pane_id: String,
    pub status: String,
}

/// The space list the strip draws from. A failed read is kept apart from an
/// empty one, so the strip never reports a failure as "every space is bound".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SpaceList {
    #[default]
    NotRead,
    Read(LinearSpacesList),
    Failed(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StripView {
    #[default]
    Spaces,
    Tabs,
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
    /// Read once at start; shown only in the `?` sheet.
    pub herdr_keys: Vec<crate::herdr_keys::HerdrKey>,
    pub spaces: SpaceList,
    pub strip: StripView,
    /// Whether up, down, Enter and Escape act on the strip instead of the board.
    pub strip_focus: bool,
    /// Index into [`LinearState::unbound_spaces`].
    pub strip_sel: usize,
    /// The space chosen on the strip, whose project picker is or was open.
    /// A project pick (`pick.kind == Projects`) is for this space.
    pub bind_space: Option<LinearSpaceRow>,
    /// A `linear.bind_handoff` is on the way; a second choice sends nothing.
    pub handoff_in_flight: bool,
    /// The picker (list kind and argument) that started the handoff in
    /// flight; only that picker closes when it succeeds.
    pub handoff_picker: Option<(LinearListKind, Option<String>)>,
    /// Set when a handoff opened its tab; the board names the refresh key
    /// until the next refresh.
    pub bind_note: Option<String>,
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
            herdr_keys: Vec::new(),
            spaces: SpaceList::NotRead,
            strip: StripView::Spaces,
            strip_focus: false,
            strip_sel: 0,
            bind_space: None,
            handoff_in_flight: false,
            handoff_picker: None,
            bind_note: None,
        }
    }

    /// The rows the strip lists: every read space whose state is not `bound`.
    /// A space whose own snapshot says it is not bound leads the list even
    /// when the space read failed, lags, or calls it bound, so the not-bound
    /// screen can always start a bind for it.
    pub fn unbound_spaces(&self) -> Vec<LinearSpaceRow> {
        let listed: &[LinearSpaceRow] = match &self.spaces {
            SpaceList::Read(list) => &list.rows,
            _ => &[],
        };
        let mut rows: Vec<LinearSpaceRow> = listed
            .iter()
            .filter(|r| r.state != "bound")
            .cloned()
            .collect();
        if let Some(snapshot) = self.snapshot().filter(|_| !self.bound()) {
            let current = listed
                .iter()
                .find(|r| r.id == self.workspace_id)
                .cloned()
                .unwrap_or_else(|| LinearSpaceRow {
                    id: self.workspace_id.clone(),
                    label: snapshot.workspace.label.clone(),
                    state: snapshot.record.state.clone().unwrap_or_default(),
                    ..LinearSpaceRow::default()
                });
            rows.retain(|r| r.id != self.workspace_id);
            rows.insert(0, current);
        }
        rows
    }

    pub(super) fn clamp_strip(&mut self) {
        let n = self.unbound_spaces().len();
        self.strip_sel = self.strip_sel.min(n.saturating_sub(1));
        if n == 0 {
            self.strip_focus = false;
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

    /// The project the space's record binds, when the space is bound.
    pub fn bound_project_id(&self) -> Option<&str> {
        if !self.bound() {
            return None;
        }
        self.snapshot()?.record.project_id.as_deref()
    }

    /// The binding of the detail's selected pane row; the first binding when
    /// the card lists no panes.
    pub fn detail_binding(&self) -> Option<&LinearBinding> {
        let binding = self
            .detail_pane_rows()
            .get(self.pane_cursor)
            .map_or(0, |row| row.binding);
        self.detail_issue()?.bindings.get(binding)
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

fn empty_groups_last(snapshot: &mut LinearSnapshot) {
    snapshot.groups.sort_by_key(|g| g.issues.is_empty());
}

pub(super) fn update_linear(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        // `board_changed` never refreshes a Linear board (R21).
        Msg::Refresh => vec![],
        Msg::Mouse(m) => super::mouse::on_mouse(app, m),
        Msg::LinearRefresh => request_or_queue(app),
        Msg::LinearArrived(arrival) => match *arrival {
            LinearArrival::Snapshot(result) => arrived(app, *result),
            LinearArrival::List { kind, id, result } => {
                super::linear_picker::list_arrived(app, kind, id, result);
                vec![]
            }
            LinearArrival::Handoff(result) => super::linear_picker::handoff_arrived(app, result),
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
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.in_flight = true;
    state.bind_note = None;
    let mut effects = vec![Effect::LinearSnapshot];
    // The strip's space list is read with every snapshot.
    if state.lists_in_flight.insert((LinearListKind::Spaces, None)) {
        effects.push(Effect::LinearList {
            kind: LinearListKind::Spaces,
            id: None,
        });
    }
    effects
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
            empty_groups_last(&mut snapshot);
            effects.push(Effect::SetLinearPaneTitle(crate::view::linear_pane_title(
                &snapshot,
                &state.workspace_id,
            )));
            state.error = None;
            state.stale_daemon = None;
            state.fetched_at = Some(now);
            state.last_good = Some(snapshot);
            state.clamp();
            state.clamp_strip();
            state.home_screen()
        }
        Err(LinearFailure::MethodNotFound) => {
            state.stale_daemon = Some((state.board_version.clone(), state.daemon_version.clone()));
            Screen::LinearStaleDaemon
        }
        Err(LinearFailure::TimedOut(limit)) => {
            state.error = Some(read_timeout_text(limit));
            Screen::LinearError
        }
        Err(LinearFailure::Failed(text)) => {
            state.error = Some(sanitise(&text));
            Screen::LinearError
        }
    };
    // An overlay open when a snapshot lands stays open: the new home
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

pub(super) fn linear_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
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
    // so `r` must not fetch and `?` must not open help.
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
            match nav_delta(k.code) {
                Some(delta) => {
                    let max = crate::view::linear_help_max_scroll(app, app.last_area);
                    app.help_scroll = step_clamped(app.help_scroll, delta, max);
                }
                None => app.screen = app.help_return_to,
            }
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
        Screen::LinearNotBound => match strip_owned_key(app, k) {
            Some(effects) => effects,
            None if matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) => vec![Effect::Quit],
            None => vec![],
        },
        Screen::LinearStaleDaemon => match k.code {
            KeyCode::Char('q') | KeyCode::Esc => vec![Effect::Quit],
            _ => vec![],
        },
        _ => vec![],
    }
}

/// The keys the board and the not-bound screen share: the strip toggles, the
/// view picker, and every key while the strip has focus. `None` leaves the key
/// to the screen.
fn strip_owned_key(app: &mut App, k: KeyEvent) -> Option<Vec<Effect>> {
    let state = app.linear.as_mut()?;
    match k.code {
        KeyCode::Char('t') => {
            state.strip = match state.strip {
                StripView::Spaces => StripView::Tabs,
                StripView::Tabs => StripView::Spaces,
            };
            state.strip_focus = false;
            return Some(vec![]);
        }
        KeyCode::Char('s') => {
            state.strip = StripView::Spaces;
            state.strip_focus = !state.unbound_spaces().is_empty();
            return Some(vec![]);
        }
        KeyCode::Char('v') => {
            state.strip_focus = false;
            return Some(open_view_picker(app));
        }
        _ => {}
    }
    // Ahead of the screen's own arms: Escape here unfocuses the strip rather
    // than quitting, and up/down move the strip rather than the card.
    state.strip_focus.then(|| strip_key(app, k))
}

fn board_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    if let Some(effects) = strip_owned_key(app, k) {
        return effects;
    }
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

/// The view picker for the bound project; a space with no bound project has
/// no views to list, so it says so instead of opening an empty picker.
pub(super) fn open_view_picker(app: &mut App) -> Vec<Effect> {
    let Some(project) = app
        .linear
        .as_ref()
        .and_then(|s| s.bound_project_id())
        .map(str::to_string)
    else {
        app.set_toast("this space has no project to list views for", true);
        return vec![];
    };
    let id = Some(project);
    if super::linear_picker::open_linear_picker(app, LinearListKind::Views, id.clone()) {
        return vec![Effect::LinearList {
            kind: LinearListKind::Views,
            id,
        }];
    }
    vec![]
}

fn strip_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    if let Some(delta) = nav_delta(k.code) {
        let max = state.unbound_spaces().len().saturating_sub(1);
        state.strip_sel = step_clamped(state.strip_sel, delta, max);
        return vec![];
    }
    match k.code {
        KeyCode::Esc => state.strip_focus = false,
        KeyCode::Char('q') => return vec![Effect::Quit],
        KeyCode::Enter => {
            let Some(space) = state.unbound_spaces().into_iter().nth(state.strip_sel) else {
                return vec![];
            };
            state.bind_space = Some(space);
            if super::linear_picker::open_linear_picker(app, LinearListKind::Projects, None) {
                return vec![Effect::LinearList {
                    kind: LinearListKind::Projects,
                    id: None,
                }];
            }
        }
        _ => {}
    }
    vec![]
}

/// Selects the strip row drawn under the click, found again by space id, then
/// opens it the way `Enter` does. A space no longer listed is left alone.
pub(super) fn click_strip_row(app: &mut App, space_id: &str) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    let Some(at) = state
        .unbound_spaces()
        .iter()
        .position(|row| row.id == space_id)
    else {
        return vec![];
    };
    state.strip = StripView::Spaces;
    state.strip_sel = at;
    state.strip_focus = true;
    linear_key(app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

/// Selects the card drawn under the click, found again by identifier because
/// the snapshot may have changed since that frame, then opens it the way
/// `Enter` does. A card no longer in the snapshot is left alone.
pub(super) fn click_card(app: &mut App, group: &str, identifier: &str) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    let position = |g: &LinearGroup| g.issues.iter().position(|id| id == identifier);
    let groups = state.groups();
    let found = groups
        .iter()
        .position(|g| g.key == group && position(g).is_some())
        .or_else(|| groups.iter().position(|g| position(g).is_some()))
        .and_then(|g| Some((g, position(&groups[g])?)));
    let Some((sel_group, sel_card)) = found else {
        return vec![];
    };
    state.sel_group = sel_group;
    state.sel_card = sel_card;
    // Otherwise the Enter below reaches the strip, not the card.
    state.strip_focus = false;
    linear_key(app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

pub(super) fn click_group(app: &mut App, group: &str) {
    let Some(state) = app.linear.as_mut() else {
        return;
    };
    if let Some(index) = state.groups().iter().position(|g| g.key == group) {
        state.strip_focus = false;
        state.sel_group = index;
        state.clamp();
    }
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
        KeyCode::Char('b') => return bind_detail_card(app),
        KeyCode::Char('q') | KeyCode::Esc => {
            state.detail = None;
            app.screen = Screen::LinearBoard;
        }
        _ => {}
    }
    vec![]
}

/// `b` in the card detail: a bind for the selected binding, in its worktree.
/// Only a state the plugin can confirm or repair is sent; every other
/// state is refused by name.
fn bind_detail_card(app: &mut App) -> Vec<Effect> {
    let Some(state) = app.linear.as_ref() else {
        return vec![];
    };
    let (Some(issue), Some(binding)) = (state.detail_issue(), state.detail_binding()) else {
        app.set_toast("this card has no worktree binding to bind", true);
        return vec![];
    };
    let refusal = match binding.state.as_str() {
        "proposed" | "stale" | "misplaced" => None,
        "bound" => Some("this worktree is already bound".to_string()),
        "worktree_missing" => Some("this binding's worktree is missing; nothing to bind".into()),
        other => Some(format!(
            "a binding in state {other:?} cannot be bound from here"
        )),
    };
    if let Some(text) = refusal {
        app.set_toast(text, true);
        return vec![];
    }
    let Some(project) = state.bound_project_id() else {
        app.set_toast("this space has no bound project", true);
        return vec![];
    };
    let target = super::BindTarget {
        space: state.workspace_id.clone(),
        project: project.to_string(),
        issue: Some(issue.identifier.clone()),
        working_directory: Some(binding.worktree_path.clone()),
        ..Default::default()
    };
    super::linear_picker::start_bind_handoff(app, target)
}

// -- sanitising -------------------------------------------------------------

/// The plugin's `HERDR_LINEAR_SANITIZE_JQ_DEF` codepoint set: C0 except tab
/// and newline, DEL, C1, and the invisible format characters. Applied to
/// every document string and to error text before rendering.
pub fn sanitise(s: &str) -> String {
    board_core::text::strip_control_keep_lines(s)
}

/// Every string in the document, keys included, through [`sanitise`]. It walks
/// the serialised form rather than the struct's fields, so a string field
/// added to any Linear type is covered without a line here (R22).
pub fn sanitise_snapshot(snapshot: &mut LinearSnapshot) {
    let cleaned = serde_json::to_value(&*snapshot)
        .map(board_core::text::sanitise_json)
        .and_then(serde_json::from_value::<LinearSnapshot>);
    match cleaned {
        Ok(cleaned) => *snapshot = cleaned,
        // The document round-trips through its own types, so this does not
        // happen; if it did, nothing unsanitised may reach the screen.
        Err(_) => *snapshot = LinearSnapshot::default(),
    }
}

/// The same walk over a list envelope.
pub fn sanitise_list(result: LinearListResult) -> LinearListResult {
    let kind = result.kind();
    let cleaned = serde_json::to_value(&result)
        .map(board_core::text::sanitise_json)
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
