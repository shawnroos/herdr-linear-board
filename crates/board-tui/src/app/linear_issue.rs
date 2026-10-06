//! The issue view's keys: the open issue's rows, its back-stack, the open
//! mode (`e`), and pinning it beside the board (`o`).

use board_core::protocol::LinearLinkedIssue;
use crossterm::event::{KeyCode, KeyEvent};

use super::linear::{
    request_issue, DetailStep, IssueMode, LinearFailure, LinearState, PinnedIssue, PrRead,
    DETAIL_STACK_MAX,
};
use super::linear_cursor::Focus;
use super::nav::{nav_delta, step_clamped};
use super::{App, Effect, Screen};
use crate::view::IssueRowKind;

pub(super) fn detail_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    if app.linear.is_none() {
        return vec![];
    }
    if let Some(delta) = nav_delta(k.code) {
        step_open(app, delta);
        return vec![];
    }
    match k.code {
        KeyCode::PageDown => page_open(app, 1),
        KeyCode::PageUp => page_open(app, -1),
        KeyCode::Enter => return follow_open_row(app),
        KeyCode::Char('e') => toggle_size(app),
        KeyCode::Char('o') => return pin_open(app),
        // `u`, `y` and `b` act on the page's OWN issue whatever row the cursor
        // is on: they are about the issue being read, not the row under the
        // cursor.
        KeyCode::Char('u') => match app
            .linear
            .as_ref()
            .and_then(|state| state.detail_issue())
            .and_then(|i| i.url.clone())
        {
            Some(url) => return vec![Effect::OpenIssueUrl(url)],
            None => app.set_toast("this card has no Linear URL (cached issue)", true),
        },
        KeyCode::Char('y') => {
            let binding = app
                .linear
                .as_ref()
                .and_then(|state| state.detail_binding())
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
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Left => return leave_issue_page(app),
        _ => {}
    }
    vec![]
}

/// Each read can cost the daemon four `gh` calls; an issue with more pull
/// requests than this shows Linear's data for the rest.
const PR_READS_MAX: usize = 8;

/// Why `e` and `o` do nothing: the narrow view draws no buttons.
fn narrow_toast(app: &mut App) {
    app.set_toast(
        format!(
            "e and o need a pane at least {} cells wide",
            crate::view::MODAL_MIN
        ),
        false,
    );
}

/// `e`: a window becomes the whole board and back.
fn toggle_size(app: &mut App) {
    if !crate::view::issue_buttons_shown(app) {
        return narrow_toast(app);
    }
    if let Some(state) = app.linear.as_mut() {
        state.issue_mode = match state.issue_mode {
            IssueMode::Window => IssueMode::Expanded,
            IssueMode::Expanded => IssueMode::Window,
        };
    }
}

struct Cursor<'a> {
    selection: &'a mut Option<IssueRowKind>,
    scroll: &'a mut usize,
    follow: &'a mut bool,
}

impl Cursor<'_> {
    /// ↑ / ↓: the next selectable row that way, or, with none, the page
    /// scrolls a line, so text below the last row can still be read.
    fn step(self, layout: crate::view::IssueLayout, delta: isize) {
        let at = LinearState::selected_row(&layout.rows, self.selection.as_ref());
        let next = step_clamped(at, delta, layout.rows.len().saturating_sub(1));
        // Nothing chosen yet and the first row out of sight: read toward it
        // rather than jump past it.
        let unseen = self.selection.is_none() && layout.in_flow(at) && !layout.visible(at);
        if next != at && layout.rows.get(next).is_some() && !unseen {
            *self.selection = layout.rows.get(next).map(|r| r.kind.clone());
            *self.scroll = layout.scroll;
            *self.follow = true;
            return;
        }
        *self.scroll = layout.scroll.saturating_add_signed(delta).min(layout.max);
        *self.follow = false;
    }

    /// PgUp / PgDn: the page by a screen, the selection moving onto the
    /// first row the new screen shows when the old one scrolled away.
    fn page(self, mut layout: crate::view::IssueLayout, delta: isize) {
        let step = layout.height.saturating_sub(1).max(1) as isize;
        layout.scroll = layout
            .scroll
            .saturating_add_signed(delta * step)
            .min(layout.max);
        *self.scroll = layout.scroll;
        *self.follow = false;
        let at = LinearState::selected_row(&layout.rows, self.selection.as_ref());
        if layout.in_flow(at) && !layout.visible(at) {
            if let Some(first) =
                (0..layout.rows.len()).find(|&i| layout.in_flow(i) && layout.visible(i))
            {
                *self.selection = Some(layout.rows[first].kind.clone());
            }
        }
    }
}

fn open_cursor(state: &mut LinearState) -> Cursor<'_> {
    Cursor {
        selection: &mut state.detail_selection,
        scroll: &mut state.detail_scroll,
        follow: &mut state.detail_follow,
    }
}

fn step_open(app: &mut App, delta: isize) {
    let Some(layout) = crate::view::issue_layout(app) else {
        return;
    };
    if let Some(state) = app.linear.as_mut() {
        open_cursor(state).step(layout, delta);
    }
}

fn page_open(app: &mut App, delta: isize) {
    let Some(layout) = crate::view::issue_layout(app) else {
        return;
    };
    if let Some(state) = app.linear.as_mut() {
        open_cursor(state).page(layout, delta);
    }
}

/// Return: an issue row opens that issue in place, a pane row focuses the
/// pane, and a card opens its link.
fn follow_open_row(app: &mut App) -> Vec<Effect> {
    let rows = crate::view::issue_page_rows(app);
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    match LinearState::selected_kind(&rows, state.detail_selection.as_ref()) {
        // The page it came from goes on the stack with what it was showing.
        Some(IssueRowKind::Issue(identifier)) => {
            let seed = linked_row(state, &identifier);
            open_linked_issue(app, &identifier, seed)
        }
        Some(IssueRowKind::Pane(_)) => focus_selected_pane(app, &rows),
        Some(IssueRowKind::Link(url)) => vec![Effect::OpenLink(url)],
        None => vec![],
    }
}

/// `o` in the modal or the expanded view: pinned beside the board, replacing
/// any issue pinned before, and the board gets the keys back.
fn pin_open(app: &mut App) -> Vec<Effect> {
    if !crate::view::issue_buttons_shown(app) {
        narrow_toast(app);
        return vec![];
    }
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    let Some(issue) = state.detail.clone() else {
        return vec![];
    };
    let doc = state
        .detail_doc
        .take()
        .filter(|(id, _)| *id == issue)
        .map(|(_, doc)| doc);
    let in_flight = state
        .detail_in_flight
        .take()
        .filter(|(id, _)| *id == issue)
        .map(|(_, generation)| generation);
    state.pinned = Some(PinnedIssue {
        issue: issue.clone(),
        doc,
        in_flight,
        error: state.detail_error.take(),
        seed: state.detail_seed.take(),
        selection: state.detail_selection.take(),
        stack: vec![],
        marks: state
            .detail_marks
            .take()
            .map(|(_, m)| m)
            .unwrap_or_default(),
        scroll: 0,
        follow: true,
    });
    close_open(state);
    app.screen = Screen::LinearBoard;
    app.set_toast(format!("{issue} pinned · p focuses it · o unpins"), false);
    if let Some(state) = app.linear.as_mut() {
        state.refit(width);
    }
    vec![]
}

fn close_open(state: &mut LinearState) {
    state.detail = None;
    state.detail_doc = None;
    state.detail_error = None;
    state.detail_in_flight = None;
    state.detail_selection = None;
    state.detail_seed = None;
    state.detail_marks = None;
    state.detail_stack.clear();
}

/// Opening a card clears its marks, so under a mark filter it can leave the
/// board: the next shown card in board order takes the selection, and the
/// filter stays.
fn reselect_after_close(state: &mut LinearState) {
    let Some((id, at)) = state.opened_from.take() else {
        return;
    };
    if state.selected_identifier() == Some(id.as_str()) {
        return;
    }
    if let Some((tab, group, card)) = state.shown_from(at) {
        state.select_slot(tab, group, card);
    }
}

pub(super) fn unpin(app: &mut App) {
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return;
    };
    match state.pinned.take() {
        Some(pin) => {
            if state.focus == Focus::Pinned {
                state.reselect();
            }
            state.refit(width);
            app.set_toast(format!("{} unpinned", pin.issue), false);
        }
        None => app.set_toast("nothing is pinned; o in an open issue pins it", false),
    }
}

pub(super) fn focus_pinned(app: &mut App) {
    let width = app.last_area.width;
    let Some(state) = app.linear.as_mut() else {
        return;
    };
    if state.pinned.is_none() {
        return app.set_toast("nothing is pinned; o in an open issue pins it", false);
    }
    if !state.pin_shown(width) {
        return app.set_toast("the pinned issue needs a wider pane to show", false);
    }
    state.remember_card();
    state.focus = Focus::Pinned;
}

/// Keys while the pinned panel has the focus. `None` hands the key to the
/// board.
pub(super) fn pinned_key(app: &mut App, k: KeyEvent) -> Option<Vec<Effect>> {
    let width = app.last_area.width;
    let state = app.linear.as_mut()?;
    if state.focus != Focus::Pinned {
        return None;
    }
    if !state.pin_shown(width) {
        state.reselect();
        return None;
    }
    if let Some(delta) = nav_delta(k.code) {
        let layout = crate::view::pinned_layout(app)?;
        let pin = app.linear.as_mut()?.pinned.as_mut()?;
        pin_cursor(pin).step(layout, delta);
        return Some(vec![]);
    }
    Some(match k.code {
        KeyCode::PageDown | KeyCode::PageUp => {
            let delta = if k.code == KeyCode::PageDown { 1 } else { -1 };
            let layout = crate::view::pinned_layout(app)?;
            let pin = app.linear.as_mut()?.pinned.as_mut()?;
            pin_cursor(pin).page(layout, delta);
            vec![]
        }
        KeyCode::Esc | KeyCode::Char('p') => {
            state.reselect();
            vec![]
        }
        KeyCode::Enter => follow_pinned_row(app),
        KeyCode::Backspace | KeyCode::Left => pinned_back(app),
        KeyCode::Char('o') => {
            unpin(app);
            vec![]
        }
        _ => vec![],
    })
}

fn pin_cursor(pin: &mut PinnedIssue) -> Cursor<'_> {
    Cursor {
        selection: &mut pin.selection,
        scroll: &mut pin.scroll,
        follow: &mut pin.follow,
    }
}

/// Return in the panel: a linked issue opens in the panel, a pane is
/// focused, a card opens its link.
fn follow_pinned_row(app: &mut App) -> Vec<Effect> {
    let Some(layout) = crate::view::pinned_layout(app) else {
        return vec![];
    };
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    let Some(pin) = state.pinned.as_ref() else {
        return vec![];
    };
    match LinearState::selected_kind(&layout.rows, pin.selection.as_ref()) {
        Some(IssueRowKind::Issue(identifier)) => {
            let seed = pin
                .doc
                .as_ref()
                .and_then(|doc| doc.issue.as_ref())
                .and_then(|issue| linked_in(issue, &identifier));
            open_in_pin(state, &identifier, seed)
        }
        Some(IssueRowKind::Pane(index)) => {
            let pane = state
                .issue(&pin.issue)
                .map(|issue| state.pane_rows(issue))
                .and_then(|rows| rows.get(index).map(|row| row.pane_id.clone()));
            match pane {
                Some(pane_id) => vec![Effect::FocusPane(pane_id)],
                None => vec![],
            }
        }
        Some(IssueRowKind::Link(url)) => vec![Effect::OpenLink(url)],
        None => vec![],
    }
}

fn open_in_pin(
    state: &mut LinearState,
    identifier: &str,
    seed: Option<LinearLinkedIssue>,
) -> Vec<Effect> {
    let Some(pin) = state.pinned.as_mut() else {
        return vec![];
    };
    if pin.issue == identifier {
        return vec![];
    }
    pin.stack.push(DetailStep {
        issue: std::mem::replace(&mut pin.issue, identifier.to_string()),
        doc: pin.doc.take(),
        seed: pin.seed.take(),
        selection: pin.selection.take(),
    });
    if pin.stack.len() > DETAIL_STACK_MAX {
        pin.stack.remove(0);
    }
    pin.seed = seed;
    pin.error = None;
    pin.marks.clear();
    pin.scroll = 0;
    pin.follow = true;
    read_pinned(state)
}

/// Backspace or ← in the panel: back one linked issue.
fn pinned_back(app: &mut App) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    let Some(pin) = state.pinned.as_mut() else {
        return vec![];
    };
    let Some(step) = pin.stack.pop() else {
        return vec![];
    };
    pin.issue = step.issue;
    pin.doc = step.doc;
    pin.seed = step.seed;
    pin.selection = step.selection;
    pin.error = None;
    pin.scroll = 0;
    pin.follow = true;
    read_pinned(state)
}

fn read_pinned(state: &mut LinearState) -> Vec<Effect> {
    state.detail_reads += 1;
    let generation = state.detail_reads;
    let Some(pin) = state.pinned.as_mut() else {
        return vec![];
    };
    pin.in_flight = Some(generation);
    vec![Effect::LinearIssue {
        issue: pin.issue.clone(),
        generation,
    }]
}

/// An issue read the pinned slot asked for, applied there. `None` when the
/// read was not the pinned slot's.
pub(super) fn pinned_arrived(
    state: &mut LinearState,
    issue: &str,
    generation: u64,
    result: &Result<board_core::protocol::LinearIssueDocument, LinearFailure>,
) -> Option<Vec<Effect>> {
    let pin = state.pinned.as_mut()?;
    if pin.in_flight != Some(generation) {
        return None;
    }
    pin.in_flight = None;
    if pin.issue != issue {
        return Some(vec![]);
    }
    match result.clone() {
        Ok(doc) if doc.issue.is_some() => {
            let doc = super::linear::sanitise_issue(doc);
            pin.error = None;
            pin.doc = Some(doc.clone());
            Some(request_prs(state, &doc))
        }
        Ok(doc) => {
            let message = doc
                .message
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "this issue could not be read".into());
            pin.error = Some(super::linear::DetailError {
                issue: issue.to_string(),
                message: super::sanitise(&message),
                retryable: true,
            });
            Some(vec![])
        }
        Err(failure) => {
            pin.error = Some(super::linear::detail_failure(issue, failure));
            Some(vec![])
        }
    }
}

fn focus_selected_pane(app: &mut App, rows: &[crate::view::IssueRow]) -> Vec<Effect> {
    let Some(state) = app.linear.as_ref() else {
        return vec![];
    };
    let index = match LinearState::selected_kind(rows, state.detail_selection.as_ref()) {
        Some(IssueRowKind::Pane(index)) => index,
        _ => return vec![],
    };
    let pane = state
        .detail_pane_rows()
        .get(index)
        .map(|row| row.pane_id.clone());
    match pane {
        Some(pane_id) => vec![Effect::FocusPane(pane_id)],
        None => {
            app.set_toast("this card has no recorded pane", true);
            vec![]
        }
    }
}

/// A linked issue `issue` lists: a sub-issue, its parent or a relation.
fn linked_in(
    issue: &board_core::protocol::LinearIssueDetail,
    identifier: &str,
) -> Option<LinearLinkedIssue> {
    issue
        .children
        .iter()
        .chain(issue.parent.iter())
        .chain(issue.relations.iter().map(|r| &r.issue))
        .find(|row| row.identifier == identifier)
        .cloned()
}

/// The row the reader is opening, as the page already had it. Looked up in the
/// open document rather than the snapshot, because that is where a linked issue
/// the board has never seen comes from.
fn linked_row(state: &LinearState, identifier: &str) -> Option<LinearLinkedIssue> {
    let (_, doc) = state.detail_doc.as_ref()?;
    linked_in(doc.issue.as_ref()?, identifier)
}

/// Enter on a sub-issue, the parent or a relation. The page being left goes on
/// the stack with what it was showing, so Esc can put it straight back.
fn open_linked_issue(
    app: &mut App,
    identifier: &str,
    seed: Option<LinearLinkedIssue>,
) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    let Some(from) = state.detail.clone() else {
        return vec![];
    };
    if from == identifier {
        return vec![];
    }
    let doc = state
        .detail_doc
        .as_ref()
        .filter(|(id, _)| *id == from)
        .map(|(_, doc)| doc.clone());
    state.detail_stack.push(DetailStep {
        issue: from,
        doc,
        seed: state.detail_seed.clone(),
        selection: state.detail_selection.clone(),
    });
    // Each step holds a whole document - description, comments, history - and
    // nothing else bounds the walk, so a long browse would keep every issue it
    // passed through. Past the cap the oldest step is dropped, which costs the
    // far end of a walk-back nobody takes and never the recent steps.
    if state.detail_stack.len() > DETAIL_STACK_MAX {
        let over = state.detail_stack.len() - DETAIL_STACK_MAX;
        state.detail_stack.drain(..over);
    }
    state.detail = Some(identifier.to_string());
    state.detail_selection = None;
    state.detail_scroll = 0;
    state.detail_follow = true;
    state.detail_doc = None;
    state.detail_error = None;
    state.detail_seed = seed;
    request_issue(app, identifier)
}

/// Esc: a step back through the issues opened to get here, and off the page
/// only from the first one.
fn leave_issue_page(app: &mut App) -> Vec<Effect> {
    let Some(state) = app.linear.as_mut() else {
        return vec![];
    };
    let Some(step) = state.detail_stack.pop() else {
        close_open(state);
        reselect_after_close(state);
        app.screen = Screen::LinearBoard;
        return vec![];
    };
    // What that page last showed goes back on screen now; the fresh read below
    // replaces it when it lands, and leaves it in place if it fails (R17).
    state.detail = Some(step.issue.clone());
    state.detail_selection = step.selection;
    state.detail_scroll = 0;
    state.detail_follow = true;
    state.detail_error = None;
    state.detail_seed = step.seed;
    state.detail_doc = step.doc.map(|doc| (step.issue.clone(), doc));
    request_issue(app, &step.issue)
}

/// `b` in the card detail: `linear.bind` of the selected binding's worktree
/// to this issue. Only a state a bind can confirm or repair is sent; every
/// other state is refused by name.
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
    vec![Effect::LinearBind {
        mark: None,
        issue: issue.identifier.clone(),
        cwd: binding.worktree_path.clone(),
    }]
}

/// A `linear.pr` answer, sanitised: check names, titles and `gh` errors are
/// outside text the snapshot sanitiser never sees.
pub(crate) fn pr_arrived(
    app: &mut App,
    url: String,
    result: Result<board_core::protocol::LinearPrDocument, LinearFailure>,
) {
    let Some(state) = app.linear.as_mut() else {
        return;
    };
    let read = match result {
        Ok(doc) => {
            let clean = serde_json::to_value(&doc)
                .map(board_core::text::sanitise_json)
                .and_then(serde_json::from_value);
            match clean {
                Ok(doc) => PrRead::Read(Box::new(doc)),
                Err(_) => PrRead::Failed("unreadable PR document".into()),
            }
        }
        Err(LinearFailure::MethodNotFound) => {
            state.pr_read_missing = true;
            state.prs.remove(&url);
            return;
        }
        Err(LinearFailure::GhUnavailable(why)) => PrRead::GhUnavailable(super::sanitise(&why)),
        Err(LinearFailure::TimedOut(limit)) => {
            PrRead::Failed(super::linear::read_timeout_text(limit))
        }
        Err(LinearFailure::OpUnsupported(why) | LinearFailure::Failed(why)) => {
            PrRead::Failed(super::sanitise(&why))
        }
    };
    state.prs.insert(url, read);
}

/// The PR reads an issue document asks for: each pull request it links,
/// newest first, up to a cap, unless one is already on the way or the daemon
/// has no PR read.
pub(super) fn request_prs(
    state: &mut LinearState,
    doc: &board_core::protocol::LinearIssueDocument,
) -> Vec<Effect> {
    if state.pr_read_missing {
        return vec![];
    }
    let Some(detail) = doc.issue.as_ref() else {
        return vec![];
    };
    crate::view::pull_requests(&detail.attachments)
        .into_iter()
        .take(PR_READS_MAX)
        .filter_map(|a| {
            let url = a.url.clone();
            if matches!(state.prs.get(&url), Some(PrRead::Loading)) {
                return None;
            }
            state.prs.insert(url.clone(), PrRead::Loading);
            Some(Effect::LinearPr { url })
        })
        .collect()
}
