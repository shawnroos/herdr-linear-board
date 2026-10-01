//! The session side pane (KTD10): the state and reducer for a split beside
//! one agent. It shows that agent's bound issue on the issue page, the lane
//! that issue sits in, or the bind hint. It reads `linear.session.get`, the
//! space's snapshot and local state, and the issue document; it never writes,
//! so no key here builds a mark clear, a show answer or a bind.

use board_core::protocol::{LinearLinkedIssue, LinearSessionGetResult};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::linear::{order_tabs, sanitise, sanitise_local, sanitise_snapshot};
use super::nav::{nav_delta, step_clamped};
use super::{App, Effect, LinearArrival, LinearFailure, LinearState, Msg, Screen};
use crate::SessionIdentity;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionView {
    #[default]
    Issue,
    List,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionState {
    pub identity: SessionIdentity,
    /// The last read that arrived. A failed read keeps it on screen.
    pub read: Option<LinearSessionGetResult>,
    /// Why the last read failed, while the one on screen is older.
    pub failed: Option<String>,
    pub in_flight: bool,
    pub queued: bool,
    pub view: SessionView,
    /// The selected row of the lane list, counting issues only.
    pub list_sel: usize,
}

impl SessionState {
    pub fn new(identity: SessionIdentity) -> SessionState {
        SessionState {
            identity,
            ..SessionState::default()
        }
    }

    pub fn bound_issue(&self) -> Option<&str> {
        self.read
            .as_ref()?
            .binding
            .as_ref()
            .map(|b| b.issue.as_str())
            .filter(|issue| !issue.is_empty())
    }
}

/// The lane list: the bound issue's lane in the tab that holds it, by column.
/// `lane` is `None` on a board without lane grouping, where the list is the
/// whole tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLane {
    pub tab: String,
    pub lane: Option<String>,
    pub columns: Vec<(String, Vec<String>)>,
}

impl SessionLane {
    pub fn issues(&self) -> Vec<&str> {
        self.columns
            .iter()
            .flat_map(|(_, issues)| issues.iter().map(String::as_str))
            .collect()
    }
}

pub fn session_lane(state: &LinearState, issue: &str) -> Option<SessionLane> {
    let tab = state.tabs().iter().find(|tab| {
        tab.groups.iter().any(|g| {
            g.issues.iter().any(|i| i == issue)
                || g.lanes.iter().any(|l| l.issues.iter().any(|i| i == issue))
        })
    })?;
    let lane = tab
        .groups
        .iter()
        .flat_map(|g| &g.lanes)
        .find(|l| l.issues.iter().any(|i| i == issue));
    let columns = tab
        .groups
        .iter()
        .map(|g| {
            let issues = match lane {
                Some(lane) => g
                    .lanes
                    .iter()
                    .find(|l| l.key == lane.key)
                    .map(|l| l.issues.clone())
                    .unwrap_or_default(),
                None => g.issues.clone(),
            };
            (g.label.clone(), issues)
        })
        .filter(|(_, issues)| !issues.is_empty())
        .collect();
    Some(SessionLane {
        tab: tab.label.clone(),
        lane: lane.map(|l| l.label.clone()),
        columns,
    })
}

pub(super) fn update_session(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Refresh | Msg::Mouse(_) => vec![],
        Msg::LinearRefresh => read_everything(app),
        Msg::Key(k) => session_key(app, k),
        Msg::LocalStateChanged(signals) => {
            let Some(snapshot) = app
                .linear
                .as_ref()
                .and_then(|s| signals.for_space(&s.workspace_id))
            else {
                return vec![];
            };
            let mut effects = read_session(app);
            effects.extend(read_local(app));
            if snapshot {
                effects.extend(read_snapshot(app));
            }
            effects
        }
        Msg::LinearArrived(arrival) => match *arrival {
            LinearArrival::Session(result) => session_arrived(app, *result),
            LinearArrival::Snapshot(result) => snapshot_arrived(app, *result),
            LinearArrival::State(result) => local_arrived(app, *result),
            LinearArrival::Issue {
                issue,
                generation,
                result,
            } => {
                super::linear::issue_arrived(app, &issue, generation, *result);
                vec![]
            }
            LinearArrival::List { .. }
            | LinearArrival::Handoff(_)
            | LinearArrival::Wrote { .. } => {
                vec![]
            }
        },
    }
}

fn read_everything(app: &mut App) -> Vec<Effect> {
    let mut effects = read_session(app);
    effects.extend(read_snapshot(app));
    effects.extend(read_local(app));
    effects
}

fn read_session(app: &mut App) -> Vec<Effect> {
    let Some(session) = app.session.as_mut() else {
        return vec![];
    };
    if session.in_flight {
        session.queued = true;
        return vec![];
    }
    session.in_flight = true;
    vec![Effect::LinearSessionGet]
}

fn read_snapshot(app: &mut App) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    if state.in_flight {
        state.queued = true;
        return vec![];
    }
    state.in_flight = true;
    vec![Effect::LinearSnapshot { force: false }]
}

fn read_local(app: &mut App) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    if state.local_in_flight {
        state.local_queued = true;
        return vec![];
    }
    state.local_in_flight = true;
    vec![Effect::LinearStateGet]
}

fn failure_text(failure: LinearFailure) -> String {
    match failure {
        LinearFailure::MethodNotFound => "the daemon predates the session read".to_string(),
        LinearFailure::TimedOut(limit) => super::linear::read_timeout_text(limit),
        LinearFailure::OpUnsupported(text) | LinearFailure::Failed(text) => sanitise(&text),
    }
}

fn session_arrived(
    app: &mut App,
    result: Result<LinearSessionGetResult, LinearFailure>,
) -> Vec<Effect> {
    let Some(session) = app.session.as_mut() else {
        return vec![];
    };
    session.in_flight = false;
    let follow_up = std::mem::take(&mut session.queued);
    match result {
        Ok(read) => {
            session.read = Some(clean(read));
            session.failed = None;
        }
        Err(failure) => session.failed = Some(failure_text(failure)),
    }
    let mut effects = follow_issue(app);
    if follow_up {
        effects.extend(read_session(app));
    }
    effects
}

/// Mark text and issue ids are agent text, cleaned as the board cleans them.
fn clean(read: LinearSessionGetResult) -> LinearSessionGetResult {
    serde_json::to_value(&read)
        .map(board_core::text::sanitise_json)
        .and_then(serde_json::from_value)
        .unwrap_or_default()
}

fn snapshot_arrived(
    app: &mut App,
    result: Result<board_core::protocol::LinearSnapshot, LinearFailure>,
) -> Vec<Effect> {
    let now = app.now;
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.in_flight = false;
    let follow_up = std::mem::take(&mut state.queued);
    match result {
        Ok(mut snapshot) => {
            sanitise_snapshot(&mut snapshot);
            order_tabs(&mut snapshot);
            state.last_good = Some(snapshot);
            state.fetched_at = Some(now);
            state.error = None;
        }
        Err(failure) => state.error = Some(failure_text(failure)),
    }
    let mut effects = follow_issue(app);
    if follow_up {
        effects.extend(read_snapshot(app));
    }
    effects
}

fn local_arrived(
    app: &mut App,
    result: Result<board_core::protocol::LinearState, LinearFailure>,
) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    state.local_in_flight = false;
    let follow_up = std::mem::take(&mut state.local_queued);
    if let Ok(local) = result {
        state.local = Some(sanitise_local(local));
    }
    if follow_up {
        read_local(app)
    } else {
        vec![]
    }
}

/// Point the issue page at the bound issue, with the session read's marks.
/// The page is never opened through the board's Enter, which clears marks.
fn follow_issue(app: &mut App) -> Vec<Effect> {
    let Some(session) = app.session.as_ref() else {
        return vec![];
    };
    let bound = session.bound_issue().map(str::to_string);
    let marks = session
        .read
        .as_ref()
        .map(|r| r.marks.clone())
        .unwrap_or_default();
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    let Some(issue) = bound else {
        state.detail = None;
        state.detail_seed = None;
        state.detail_marks = None;
        if let Some(session) = app.session.as_mut() {
            session.view = SessionView::Issue;
        }
        return vec![];
    };
    if state.detail.as_deref() != Some(issue.as_str()) {
        state.detail_scroll = 0;
        state.detail_error = None;
    }
    state.detail = Some(issue.clone());
    // Off the snapshot (not read yet, failed, or outside the view) the page
    // still needs a subject, or it draws nothing at all.
    state.detail_seed = state.issue(&issue).is_none().then(|| LinearLinkedIssue {
        identifier: issue.clone(),
        ..LinearLinkedIssue::default()
    });
    state.detail_marks = Some((issue.clone(), marks));
    let have = state
        .detail_doc
        .as_ref()
        .is_some_and(|(id, _)| *id == issue)
        || state
            .detail_error
            .as_ref()
            .is_some_and(|e| e.issue == issue);
    if have {
        return vec![];
    }
    super::linear::request_issue(app, &issue)
}

fn session_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    if k.modifiers.intersects(
        KeyModifiers::CONTROL
            | KeyModifiers::ALT
            | KeyModifiers::META
            | KeyModifiers::SUPER
            | KeyModifiers::HYPER,
    ) {
        return vec![];
    }
    if app.screen == Screen::Help {
        match nav_delta(k.code) {
            Some(delta) => {
                let max = crate::view::session_help_max_scroll(app, app.last_area);
                app.help_scroll = step_clamped(app.help_scroll, delta, max);
            }
            None => app.screen = app.help_return_to,
        }
        return vec![];
    }
    match k.code {
        KeyCode::Char('?') => {
            app.help_return_to = app.screen;
            app.help_scroll = 0;
            app.screen = Screen::Help;
        }
        KeyCode::Char('q') | KeyCode::Esc => return vec![Effect::Quit],
        KeyCode::Tab => flip(app),
        code => {
            if let Some(delta) = nav_delta(code) {
                step(app, delta);
            }
        }
    }
    vec![]
}

fn flip(app: &mut App) {
    let Some(session) = app.session.as_mut() else {
        return;
    };
    let Some(issue) = session.bound_issue().map(str::to_string) else {
        return;
    };
    session.view = match session.view {
        SessionView::Issue => SessionView::List,
        SessionView::List => SessionView::Issue,
    };
    if session.view == SessionView::List {
        let lane = app.linear.as_ref().and_then(|s| session_lane(s, &issue));
        session.list_sel = lane
            .and_then(|lane| lane.issues().iter().position(|id| *id == issue))
            .unwrap_or(0);
    }
}

fn step(app: &mut App, delta: isize) {
    let Some(session) = app.session.as_mut() else {
        return;
    };
    match session.view {
        SessionView::List => {
            let len = session
                .bound_issue()
                .and_then(|issue| app.linear.as_ref().and_then(|s| session_lane(s, issue)))
                .map_or(0, |lane| lane.issues().len());
            session.list_sel = step_clamped(session.list_sel, delta, len.saturating_sub(1));
        }
        SessionView::Issue => {
            let max = crate::view::issue_page_max_scroll(app);
            if let Some(state) = app.linear.as_mut() {
                state.detail_scroll = step_clamped(state.detail_scroll, delta, max);
            }
        }
    }
}
