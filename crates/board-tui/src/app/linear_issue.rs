//! The issue view's keys: the open issue's rows, its back-stack, the open
//! mode (`e`), and pinning it beside the board (`o`).

use board_core::protocol::LinearLinkedIssue;
use crossterm::event::{KeyCode, KeyEvent};

use super::linear::{
    request_issue, DetailStep, IssueMode, LinearFailure, LinearState, PrRead, DETAIL_STACK_MAX,
};
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

/// ↑ / ↓: the next selectable row that way, or, with none, the page scrolls
/// a line, so text below the last row can still be read.
fn step_open(app: &mut App, delta: isize) {
    let Some(layout) = crate::view::issue_layout(app) else {
        return;
    };
    let Some(state) = app.linear.as_mut() else {
        return;
    };
    let at = LinearState::selected_row(&layout.rows, state.detail_selection.as_ref());
    let next = step_clamped(at, delta, layout.rows.len().saturating_sub(1));
    if next != at && layout.rows.get(next).is_some() {
        state.detail_selection = layout.rows.get(next).map(|r| r.kind.clone());
        state.detail_scroll = layout.scroll;
        state.detail_follow = true;
        return;
    }
    state.detail_scroll = layout.scroll.saturating_add_signed(delta).min(layout.max);
    state.detail_follow = false;
}

/// PgUp / PgDn: the page by a screen, the selection moving onto the first
/// row the new screen shows when the old one scrolled away.
fn page_open(app: &mut App, delta: isize) {
    let Some(layout) = crate::view::issue_layout(app) else {
        return;
    };
    let Some(state) = app.linear.as_mut() else {
        return;
    };
    let step = layout.height.saturating_sub(1).max(1) as isize;
    let scroll = layout
        .scroll
        .saturating_add_signed(delta * step)
        .min(layout.max);
    state.detail_scroll = scroll;
    state.detail_follow = false;
    let mut moved = layout;
    moved.scroll = scroll;
    let at = LinearState::selected_row(&moved.rows, state.detail_selection.as_ref());
    if moved.in_flow(at) && !moved.visible(at) {
        if let Some(first) = (0..moved.rows.len()).find(|&i| moved.in_flow(i) && moved.visible(i)) {
            state.detail_selection = Some(moved.rows[first].kind.clone());
        }
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
/// any issue pinned before.
fn pin_open(app: &mut App) -> Vec<Effect> {
    if !crate::view::issue_buttons_shown(app) {
        narrow_toast(app);
    }
    vec![]
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

/// The row the reader is opening, as the page already had it. Looked up in the
/// open document rather than the snapshot, because that is where a linked issue
/// the board has never seen comes from.
fn linked_row(state: &LinearState, identifier: &str) -> Option<LinearLinkedIssue> {
    let (_, doc) = state.detail_doc.as_ref()?;
    let issue = doc.issue.as_ref()?;
    issue
        .children
        .iter()
        .chain(issue.parent.iter())
        .chain(issue.relations.iter().map(|r| &r.issue))
        .find(|row| row.identifier == identifier)
        .cloned()
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
        state.detail = None;
        state.detail_doc = None;
        state.detail_error = None;
        state.detail_in_flight = None;
        state.detail_selection = None;
        state.detail_seed = None;
        state.detail_marks = None;
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
