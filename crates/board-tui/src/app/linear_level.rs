//! The board's level: the space board, or the issue of the split beside it,
//! picked from where the board's own pane sits in its herdr tab. The
//! split level is read-only: it opens the issue without clearing its
//! marks, and its keys only read.

use std::collections::BTreeSet;

use board_core::protocol::{LinearLinkedIssue, LinearSessionGetResult, PaneContext};
use crossterm::event::{KeyCode, KeyEvent};

use super::linear::{request_issue, LinearFailure, LinearState};
use super::linear_cursor::Focus;
use super::nav::nav_delta;
use super::{App, Effect, Screen};
use crate::view::IssueRowKind;
use crate::SessionIdentity;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LevelState {
    pub own_pane: String,
    pub plugin_pane: bool,
    pub context: Option<PaneContext>,
    /// The last focused pane in the tab that is not the board itself.
    pub last_other: Option<String>,
    /// Esc at the split level's stack root: the space board until focus
    /// moves to another pane.
    pub dismissed: bool,
    /// The daemon predates `pane.context`: no further read is sent.
    pub missing: bool,
    pub in_flight: bool,
    pub queued: bool,
    /// The issue the split level shows; `None` at the space level.
    pub shown: Option<String>,
    /// `board tui --session`: the agent the board starts beside, and the
    /// issue `linear.session.get` resolved for it.
    pub start: Option<SessionIdentity>,
    pub start_issue: Option<String>,
    pub start_in_flight: bool,
    pub start_queued: bool,
}

impl LevelState {
    pub fn new(own_pane: String, plugin_pane: bool) -> LevelState {
        LevelState {
            own_pane,
            plugin_pane,
            ..LevelState::default()
        }
    }

    pub fn starting_beside(mut self, identity: SessionIdentity) -> LevelState {
        self.last_other = identity.herdr_pane_id.clone();
        self.start = Some(identity);
        self
    }

    fn start_pane(&self) -> Option<&str> {
        self.start.as_ref()?.herdr_pane_id.as_deref()
    }

    /// The agent's own cwd wins for the pane it named: herdr's cwd for that
    /// pane can be a shell outside the worktree the agent runs in.
    fn issue_of(&self, pane: &str, resolved: Option<&String>) -> Option<String> {
        if self.start_pane() == Some(pane) {
            if let Some(issue) = &self.start_issue {
                return Some(issue.clone());
            }
        }
        resolved.cloned()
    }

    pub fn wanted(&self) -> Option<String> {
        if self.dismissed {
            return None;
        }
        let Some(ctx) = &self.context else {
            self.start_pane()?;
            return self.start_issue.clone();
        };
        if ctx.overlay || ctx.alone {
            return None;
        }
        let focused = self
            .last_other
            .as_deref()
            .and_then(|id| ctx.panes.iter().find(|p| p.pane_id == id));
        if let Some(issue) = focused.and_then(|p| self.issue_of(&p.pane_id, p.issue.as_ref())) {
            return Some(issue);
        }
        ctx.panes
            .iter()
            .find_map(|p| self.issue_of(&p.pane_id, p.issue.as_ref()))
    }

    fn take_context(&mut self, ctx: PaneContext) {
        let focused = ctx
            .focused_pane_id
            .clone()
            .filter(|id| *id != self.own_pane && ctx.panes.iter().any(|p| p.pane_id == *id));
        if let Some(id) = focused {
            let before = self
                .context
                .as_ref()
                .and_then(|c| c.focused_pane_id.as_deref());
            if before != Some(id.as_str()) {
                self.dismissed = false;
            }
            self.last_other = Some(id);
        }
        self.context = Some(ctx);
    }
}

impl LinearState {
    pub fn at_split(&self) -> bool {
        self.level.as_ref().is_some_and(|l| l.shown.is_some())
    }
}

pub(super) fn request(app: &mut App) -> Vec<Effect> {
    let mut effects = request_start(app);
    let Some(level) = app.linear.as_mut().and_then(|s| s.level.as_mut()) else {
        return effects;
    };
    if level.missing {
        return effects;
    }
    if level.in_flight {
        level.queued = true;
        return effects;
    }
    level.in_flight = true;
    effects.push(Effect::PaneContext);
    effects
}

fn request_start(app: &mut App) -> Vec<Effect> {
    let Some(level) = app.linear.as_mut().and_then(|s| s.level.as_mut()) else {
        return vec![];
    };
    if level.start.is_none() {
        return vec![];
    }
    if level.start_in_flight {
        level.start_queued = true;
        return vec![];
    }
    level.start_in_flight = true;
    vec![Effect::LinearSessionGet]
}

/// `pane_layout_changed` names a workspace, or none; a board re-reads only
/// for its own, or for every event before it knows which is its own.
pub(super) fn layout_changed(app: &mut App, workspaces: &BTreeSet<Option<String>>) -> Vec<Effect> {
    let own = app
        .linear
        .as_ref()
        .and_then(|s| s.level.as_ref())
        .and_then(|l| l.context.as_ref())
        .map(|c| c.workspace_id.clone())
        .filter(|w| !w.is_empty());
    let ours = match own {
        Some(own) => workspaces
            .iter()
            .any(|w| w.as_deref().is_none_or(|w| w == own)),
        None => !workspaces.is_empty(),
    };
    if ours {
        request_without_start(app)
    } else {
        vec![]
    }
}

/// A focus or layout move changes no binding, so the start read stays.
fn request_without_start(app: &mut App) -> Vec<Effect> {
    let Some(level) = app.linear.as_mut().and_then(|s| s.level.as_mut()) else {
        return vec![];
    };
    if level.missing {
        return vec![];
    }
    if level.in_flight {
        level.queued = true;
        return vec![];
    }
    level.in_flight = true;
    vec![Effect::PaneContext]
}

pub(super) fn arrived(app: &mut App, result: Result<PaneContext, LinearFailure>) -> Vec<Effect> {
    let Some(level) = app.linear.as_mut().and_then(|s| s.level.as_mut()) else {
        return vec![];
    };
    level.in_flight = false;
    let again = std::mem::take(&mut level.queued);
    match result {
        Ok(ctx) => level.take_context(sanitised(ctx)),
        Err(LinearFailure::MethodNotFound) => level.missing = true,
        // A failed read keeps the last context: a herdr hiccup is not a
        // reason to drop the issue on screen.
        Err(_) => {}
    }
    let mut effects = if again {
        request_without_start(app)
    } else {
        vec![]
    };
    effects.extend(settle(app));
    effects
}

/// Pane ids and issue keys go back to the daemon and onto the screen.
fn sanitised(ctx: PaneContext) -> PaneContext {
    board_core::text::scrubbed(&ctx).unwrap_or_default()
}

pub(super) fn start_arrived(
    app: &mut App,
    result: Result<LinearSessionGetResult, LinearFailure>,
) -> Vec<Effect> {
    let Some(level) = app.linear.as_mut().and_then(|s| s.level.as_mut()) else {
        return vec![];
    };
    level.start_in_flight = false;
    let again = std::mem::take(&mut level.start_queued);
    if let Ok(read) = result {
        level.start_issue = read
            .binding
            .map(|b| super::linear::sanitise(&b.issue))
            .filter(|issue| !issue.is_empty());
    }
    let mut effects = if again { request_start(app) } else { vec![] };
    effects.extend(settle(app));
    effects
}

/// Whether a level change may replace what is on screen now: never over a
/// modal, the filter, the bar, help or a picker.
fn idle(app: &App) -> bool {
    let Some(state) = app.linear.as_ref() else {
        return false;
    };
    match app.screen {
        Screen::LinearBoard => {
            !matches!(state.focus, Focus::Filter | Focus::Bar(_)) && !state.strip_focus
        }
        Screen::LinearError | Screen::LinearNotBound | Screen::LinearStaleDaemon => true,
        Screen::LinearDetail => state.at_split(),
        _ => false,
    }
}

/// Apply the wanted level when the screen allows it; otherwise it waits for
/// the next return to the board, where this runs again.
pub(super) fn settle(app: &mut App) -> Vec<Effect> {
    let Some(level) = app.linear.as_ref().and_then(|s| s.level.as_ref()) else {
        return vec![];
    };
    let wanted = level.wanted();
    if wanted == level.shown || !idle(app) {
        return vec![];
    }
    match wanted {
        Some(issue) => enter(app, &issue),
        None => {
            leave(app);
            vec![]
        }
    }
}

fn enter(app: &mut App, issue: &str) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    let seed = LinearLinkedIssue {
        identifier: issue.to_string(),
        title: state
            .issue(issue)
            .map(|i| i.title.clone())
            .unwrap_or_default(),
        ..LinearLinkedIssue::default()
    };
    state.open_fresh();
    state.detail = Some(issue.to_string());
    state.detail_stack.clear();
    state.detail_selection = None;
    state.detail_error = None;
    // Without a seed, `clamp` closes a page whose issue the snapshot lacks.
    state.detail_seed = Some(seed);
    state.detail_marks = Some((issue.to_string(), state.marks_for(issue).cloned().collect()));
    if let Some(level) = state.level.as_mut() {
        level.shown = Some(issue.to_string());
    }
    app.screen = Screen::LinearDetail;
    request_issue(app, issue)
}

fn leave(app: &mut App) {
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return;
    };
    super::linear_issue::close_open(state);
    if let Some(level) = state.level.as_mut() {
        level.shown = None;
    }
    state.refit(width);
    app.screen = if state.stale_daemon.is_some() {
        Screen::LinearStaleDaemon
    } else if state.error.is_some() {
        Screen::LinearError
    } else {
        state.home_screen()
    };
}

/// Keys at the split level: reading only. Rows move and scroll, Return
/// follows a linked issue, and Esc steps back, leaving for the space board
/// from the issue the level opened.
pub(super) fn split_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let back = matches!(k.code, KeyCode::Esc | KeyCode::Backspace | KeyCode::Left);
    let reads = nav_delta(k.code).is_some()
        || matches!(k.code, KeyCode::PageUp | KeyCode::PageDown)
        || (k.code == KeyCode::Enter && on_linked_issue(app));
    if back {
        let root = app
            .linear
            .as_ref()
            .is_some_and(|s| s.detail_stack.is_empty());
        if root {
            if let Some(level) = app.linear.as_mut().and_then(|s| s.level.as_mut()) {
                level.dismissed = true;
            }
            leave(app);
            return vec![];
        }
        return super::linear_issue::detail_key(app, k);
    }
    if reads {
        return super::linear_issue::detail_key(app, k);
    }
    vec![]
}

fn on_linked_issue(app: &App) -> bool {
    let rows = crate::view::issue_page_rows(app);
    app.linear.as_ref().is_some_and(|state| {
        matches!(
            LinearState::selected_kind(&rows, state.detail_selection.as_ref()),
            Some(IssueRowKind::Issue(_))
        )
    })
}
