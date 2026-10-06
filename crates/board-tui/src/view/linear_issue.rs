//! The issue view: one Linear issue as a modal over the faded board, expanded
//! over it, narrow, pinned beside it, or in the session pane.
//!
//! The view builds ONE list of sections per draw and lays out from it. That is
//! what keeps the cursor, Enter's target and the visible order from drifting
//! apart: a section with no rows contributes no row to move onto, and the
//! one-column layout is the same list flattened rather than a second
//! arrangement of the same content.

use board_core::protocol::{
    LinearIssue, LinearIssueDetail, LinearIssueDocument, LinearLinkedIssue, Mark,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::issue_cards;
use super::linear::{fit, line, FrameBar};
use crate::app::{App, DetailError, IssueMode, LinearState, Mode, Screen};
use crate::theme::Theme;

/// The narrowest board that opens an issue as a modal and draws its buttons.
pub const MODAL_MIN: u16 = 136;
const MODAL_MAX: u16 = 128;
/// Two columns from this view width down; one below. Supersedes the 72-cell
/// (two 36-cell columns) stacking rule of the 2026-09-16 pickers-and-layout
/// plan.
const TWO_COLUMN_MIN: u16 = 100;
const SIDEBAR_W: u16 = 40;
const MAIN_MAX: u16 = 100;
const INSET: u16 = 2;
/// The issue's header row and the row under it, which holds a failure.
const HEADER_ROWS: u16 = 2;
/// The board's own header, which a modal never covers.
const BOARD_HEADER_ROWS: u16 = 2;

const EXPAND: &str = "\u{f004c}";
const COLLAPSE: &str = "\u{f0044}";
const OVERLAY: &str = "\u{f0e5a}";
const SLANT: &str = "\u{e0bc}";

/// How the issue is drawn, which decides its buttons and what it sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Modal,
    Expanded,
    Narrow,
    Pinned,
    Session,
}

impl Look {
    fn buttons(self) -> &'static [(&'static str, &'static str, &'static str)] {
        match self {
            Look::Modal => &[(EXPAND, "Expand", "e"), (OVERLAY, "Overlay", "o")],
            Look::Expanded => &[(COLLAPSE, "Window", "e"), (OVERLAY, "Overlay", "o")],
            Look::Pinned => &[(OVERLAY, "Unpin", "o")],
            Look::Narrow | Look::Session => &[],
        }
    }

    /// A window or a panel is one tone above the board; a full view is the
    /// board's own background.
    pub fn ground(self, theme: &Theme) -> Color {
        match self {
            Look::Modal | Look::Pinned => theme.palette.panel_bg,
            Look::Expanded | Look::Narrow | Look::Session => theme.app_bg,
        }
    }
}

/// One row the cursor can land on. `line` is its first line in the column it
/// was drawn in, and `height` how many lines it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    pub kind: RowKind,
    pub column: Column,
    pub height: usize,
}

/// Which column a row was drawn in. Marking the selected row needs this: the
/// two columns have their own line numbering when they sit side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Main,
    Sidebar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// An issue the reader can open: a sub-issue, the parent, or a relation.
    Issue(String),
    /// A pane the reader can focus, by its index in the page's pane rows.
    Pane(usize),
    /// An attachment or pull request card, by its URL.
    Link(String),
}

/// What one issue slot holds, borrowed from either the open issue or the
/// pinned one, so both draw through the same code.
pub struct Slot<'a> {
    pub issue: &'a str,
    pub doc: Option<&'a LinearIssueDocument>,
    pub loading: bool,
    pub error: Option<&'a DetailError>,
    pub seed: Option<&'a LinearLinkedIssue>,
    pub selection: Option<&'a RowKind>,
    pub marks: &'a [Mark],
    pub scroll: usize,
    pub follow: bool,
}

impl Slot<'_> {
    fn detail(&self) -> Option<&LinearIssueDetail> {
        self.doc.and_then(|doc| doc.issue.as_ref())
    }

    fn truncated(&self) -> &[String] {
        self.doc.map_or(&[], |doc| doc.truncated.as_slice())
    }
}

pub fn open_slot(state: &LinearState) -> Option<Slot<'_>> {
    let issue = state.detail.as_deref()?;
    Some(Slot {
        issue,
        doc: state
            .detail_doc
            .as_ref()
            .filter(|(id, _)| id == issue)
            .map(|(_, doc)| doc),
        loading: state.detail_in_flight.is_some() && state.detail_error.is_none(),
        error: state.detail_error.as_ref().filter(|e| e.issue == issue),
        seed: state.detail_seed.as_ref(),
        selection: state.detail_selection.as_ref(),
        marks: state.detail_marks(),
        scroll: state.detail_scroll,
        follow: state.detail_follow,
    })
}

pub fn pinned_slot(state: &LinearState) -> Option<Slot<'_>> {
    let pin = state.pinned.as_ref()?;
    Some(Slot {
        issue: &pin.issue,
        doc: pin.doc.as_ref(),
        loading: pin.in_flight.is_some() && pin.error.is_none(),
        error: pin.error.as_ref(),
        seed: pin.seed.as_ref(),
        selection: pin.selection.as_ref(),
        marks: &pin.marks,
        scroll: pin.scroll,
        follow: pin.follow,
    })
}

/// Everything one draw needs: the rendered lines of each column, the rows the
/// cursor can reach in reading order, and where the columns sit.
pub struct Page {
    pub main: Vec<Line<'static>>,
    pub sidebar: Vec<Line<'static>>,
    pub rows: Vec<Row>,
    pub stacked: bool,
    body: Rect,
    main_rect: Rect,
    side_rect: Rect,
}

impl Page {
    fn in_flow(&self, row: &Row) -> bool {
        self.stacked || row.column == Column::Main
    }

    /// Lines of the column that scrolls with the page: both columns flattened
    /// when stacked, else the main column.
    fn flow_len(&self) -> usize {
        if self.stacked {
            self.main.len() + self.sidebar.len()
        } else {
            self.main.len()
        }
    }

    fn flow_height(&self) -> usize {
        self.body.height as usize
    }

    fn side_text(&self) -> Rect {
        Rect::new(
            self.side_rect.x + INSET,
            self.side_rect.y + 1,
            self.side_rect.width.saturating_sub(2 * INSET),
            self.side_rect.height.saturating_sub(1),
        )
    }
}

/// The rows that sit between the issue and the bottom of `area`: the bar's,
/// when the bar has room to draw.
pub(super) fn bar_rows(bar: &FrameBar<'_>, area: Rect) -> u16 {
    let rows = bar.rows();
    if area.height >= rows + HEADER_ROWS + 4 {
        rows
    } else {
        0
    }
}

/// Where the open issue draws in a frame of `area`, and how: a modal over
/// the board when it is wide enough and a window was asked for, else over
/// the whole board.
pub fn open_rect(app: &App, state: &LinearState, area: Rect) -> (Rect, Look) {
    open_rect_in(app, state, area, &FrameBar::new(state, area.width))
}

pub(super) fn open_rect_in(
    app: &App,
    state: &LinearState,
    area: Rect,
    bar: &FrameBar<'_>,
) -> (Rect, Look) {
    if app.mode == Mode::Session {
        return (area, Look::Session);
    }
    let bar = bar_rows(bar, area);
    let full = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(bar));
    if area.width < MODAL_MIN || state.at_split() {
        return (full, Look::Narrow);
    }
    if state.issue_mode == IssueMode::Expanded {
        return (full, Look::Expanded);
    }
    let body_h = full.height.saturating_sub(BOARD_HEADER_ROWS);
    let w = area.width.saturating_sub(8).min(MODAL_MAX);
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + BOARD_HEADER_ROWS + 1.min(body_h),
        w,
        body_h.saturating_sub(2),
    );
    (rect, Look::Modal)
}

/// Whether the screen on show draws the open issue with its buttons, which
/// is where `e` and `o` act.
pub fn buttons_shown(app: &App, state: &LinearState) -> bool {
    app.screen == Screen::LinearDetail
        && !open_rect(app, state, app.last_area).1.buttons().is_empty()
}

fn dim(theme: &Theme) -> Style {
    theme.dim()
}

/// A section line in the board's lane-line style: the label on a rule, an
/// optional count or note on the right.
pub(super) fn section(theme: &Theme, title: &str, right: &str, width: usize) -> Line<'static> {
    let label = format!(" {title} ");
    let right = if right.is_empty() {
        String::new()
    } else {
        format!(" {right} ")
    };
    let rest = width.saturating_sub(2 + label.width() + right.width());
    let spans = vec![
        Span::styled("─", theme.rule()),
        Span::styled(label, theme.lane().add_modifier(Modifier::BOLD)),
        Span::styled("─".repeat(rest), theme.rule()),
        Span::styled(right, theme.lane()),
        Span::styled("─", theme.rule()),
    ];
    Line::from(super::linear::clip_spans(spans, width))
}

fn heading(theme: &Theme, text: &str, width: usize) -> Line<'static> {
    Line::from(Span::styled(
        fit(text, width),
        theme.lane().add_modifier(Modifier::BOLD),
    ))
}

/// A property row. The value is always drawn, as an em dash when Linear has
/// none: a row that disappears when empty makes "no milestone" and "not loaded
/// yet" the same thing on screen.
fn property(theme: &Theme, label: &str, value: Option<String>, width: usize) -> Line<'static> {
    let value = value.filter(|v| !v.trim().is_empty());
    let name = format!("{label:<10} ");
    let shown = fit(
        &value.clone().unwrap_or_else(|| "—".into()),
        width.saturating_sub(name.width()),
    );
    Line::from(vec![
        Span::styled(name, dim(theme)),
        Span::styled(
            shown,
            if value.is_some() {
                theme.text()
            } else {
                Style::default().fg(theme.palette.surface1)
            },
        ),
    ])
}

/// A path in a narrow column keeps its TAIL: `…/worktrees/web-3312` says which
/// worktree this is, while the head it drops is the same for every one of them.
/// `fit` elides the other end, which is right for prose and wrong for a path.
fn fit_path(path: &str, width: usize) -> String {
    let path = line(path);
    if path.width() <= width || width <= 1 {
        return path;
    }
    let keep = width - 1;
    let tail: String = path
        .chars()
        .rev()
        .scan(0usize, |acc, c| {
            *acc += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            (*acc <= keep).then_some(c)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

fn linked_row(issue: &LinearLinkedIssue, width: usize) -> Line<'static> {
    let state = issue.state.name.as_deref().unwrap_or("?");
    Line::from(fit(
        &format!(
            "  {}  {}  {}",
            line(&issue.identifier),
            line(&issue.title),
            line(state)
        ),
        width,
    ))
}

/// Relative time, coarse on purpose: the page says when something happened, and
/// a timestamp to the second would cost more width than it is worth.
///
/// Linear sends ISO-8601 (`2026-09-05T08:00:00.000Z`) and `parse_timestamp`
/// takes a space between the date and the time, so the two have to be brought
/// together here. Without this every time on the page rendered empty, which
/// read as a layout bug rather than a parse one.
pub(super) fn ago(now: i64, then: Option<&str>) -> String {
    let Some(then) = then else {
        return String::new();
    };
    let normalised = then.replacen('T', " ", 1);
    let normalised = normalised
        .split_once('.')
        .map(|(head, _)| head)
        .unwrap_or_else(|| normalised.trim_end_matches('Z'));
    let Some(then) = board_core::protocol::parse_timestamp(normalised) else {
        return String::new();
    };
    let secs = (now - then).max(0);
    match secs {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// `text · 3d`, or just `text` when the time is missing or unparseable: a
/// separator with nothing after it reads as missing data.
fn join_time(now: i64, text: &str, at: Option<&str>) -> String {
    let when = ago(now, at);
    if when.is_empty() {
        text.to_string()
    } else {
        format!("{text} · {when}")
    }
}

/// Plain text, wrapped. Used for a title and anything else that is not
/// Markdown; `crate::markdown::render` handles the fields that are.
pub(super) fn plain_lines(text: &str, width: usize) -> Vec<String> {
    let mut out = vec![];
    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if candidate.width() > width && !current.is_empty() {
                out.push(line(&current));
                current = word.to_string();
            } else {
                current = candidate;
            }
        }
        if !current.is_empty() {
            out.push(line(&current));
        }
    }
    out
}

/// The main column: title, description, attachments, pull requests,
/// sub-issues and activity.
fn main_column(
    app: &App,
    state: &LinearState,
    slot: &Slot<'_>,
    issue: &Subject<'_>,
    width: usize,
    card_bg: Color,
    rows: &mut Vec<Row>,
) -> Vec<Line<'static>> {
    let theme = &state.theme;
    let detail = slot.detail();
    let mut out: Vec<Line> = vec![];
    for text in plain_lines(issue.title(), width) {
        out.push(Line::from(Span::styled(
            fit(&text, width),
            theme.card_title_active(),
        )));
    }
    out.push(Line::from(""));

    match detail.and_then(|d| d.description.as_deref()) {
        Some(text) if !text.trim().is_empty() => {
            out.extend(crate::markdown::render(text, width));
        }
        _ if slot.loading => out.push(Line::from(Span::styled("loading…", dim(theme)))),
        Some(_) | None => out.push(Line::from(Span::styled("no description", dim(theme)))),
    }

    let attachments = detail.map(|d| d.attachments.as_slice()).unwrap_or(&[]);
    issue_cards::attachments_section(app, state, attachments, width, card_bg, &mut out, rows);
    issue_cards::pull_requests_section(app, state, attachments, width, card_bg, &mut out, rows);

    let children = detail.map(|d| d.children.as_slice()).unwrap_or(&[]);
    if !children.is_empty() {
        let done = children
            .iter()
            .filter(|c| c.state.kind.as_deref() == Some("completed"))
            .count();
        // `3/50` would be a WRONG number when the read stopped at its cap, not
        // merely an incomplete one: the total is however many rows came back,
        // which is not how many the issue has. The `+` says so.
        let more = slot.truncated().iter().any(|s| s == "children");
        let total = format!("{}{}", children.len(), if more { "+" } else { "" });
        out.push(Line::from(""));
        out.push(section(
            theme,
            "SUB-ISSUES",
            &format!("{done}/{total}"),
            width,
        ));
        for child in children {
            rows.push(Row {
                line: out.len(),
                kind: RowKind::Issue(child.identifier.clone()),
                column: Column::Main,
                height: 1,
            });
            out.push(linked_row(child, width));
        }
    }

    out.push(Line::from(""));
    out.push(section(theme, "ACTIVITY", "", width));
    activity(app.now, theme, slot, width, &mut out);
    out
}

fn activity(now: i64, theme: &Theme, slot: &Slot<'_>, width: usize, out: &mut Vec<Line<'static>>) {
    let detail = slot.detail();
    let comments = detail.map(|d| d.comments.as_slice()).unwrap_or(&[]);
    let history = detail.map(|d| d.history.as_slice()).unwrap_or(&[]);
    if comments.is_empty() && history.is_empty() {
        out.push(Line::from(Span::styled(
            if slot.loading {
                "loading…"
            } else {
                "nothing yet"
            },
            dim(theme),
        )));
    }
    for event in history {
        let actor = event.actor.as_deref().unwrap_or("someone");
        let what = history_text(event);
        if what.is_empty() {
            continue;
        }
        out.push(Line::from(fit(
            &join_time(
                now,
                &format!("  {} {}", line(actor), what),
                event.created_at.as_deref(),
            ),
            width,
        )));
    }
    // R8a -- a read that stopped at its cap says so where the reader is looking,
    // rather than presenting what came back as the whole thread.
    if slot
        .truncated()
        .iter()
        .any(|s| s == "comments" || s == "history")
    {
        out.push(Line::from(Span::styled(
            fit("  the rest is in Linear — u opens it", width),
            dim(theme),
        )));
    }
    for comment in comments.iter().filter(|c| c.parent_id.is_none()) {
        out.push(Line::from(fit(
            &join_time(
                now,
                &format!(
                    "  {} commented",
                    line(comment.author.as_deref().unwrap_or("someone"))
                ),
                comment.created_at.as_deref(),
            ),
            width,
        )));
        for body in crate::markdown::render(&comment.body, width.saturating_sub(4)) {
            out.push(Line::from(format!("    {body}")));
        }
        for reply in comments
            .iter()
            .filter(|r| r.parent_id.as_deref() == comment.id.as_deref())
        {
            out.push(Line::from(fit(
                &join_time(
                    now,
                    &format!(
                        "    ↳ {}",
                        line(reply.author.as_deref().unwrap_or("someone"))
                    ),
                    reply.created_at.as_deref(),
                ),
                width,
            )));
            for body in crate::markdown::render(&reply.body, width.saturating_sub(6)) {
                out.push(Line::from(format!("      {body}")));
            }
        }
    }
}

/// One history event as a line of prose. The plugin already dropped events that
/// changed nothing this page shows, so an empty string here means a shape this
/// board does not know how to phrase rather than a routine skip.
fn history_text(event: &board_core::protocol::LinearHistoryEvent) -> String {
    if let Some(to) = event.to_state.as_deref() {
        return format!("moved to {}", line(to));
    }
    if let Some(to) = event.to_assignee.as_deref() {
        return format!("assigned to {}", line(to));
    }
    let labels = |list: &[String]| list.iter().map(|l| line(l)).collect::<Vec<_>>().join(", ");
    if !event.added_labels.is_empty() {
        return format!("added {}", labels(&event.added_labels));
    }
    if !event.removed_labels.is_empty() {
        return format!("removed {}", labels(&event.removed_labels));
    }
    if let Some(to) = event.to_priority {
        return format!("set priority P{to}");
    }
    String::new()
}

/// The sidebar: Linear's properties, then the board's own block last.
fn sidebar_column(
    state: &LinearState,
    slot: &Slot<'_>,
    issue: &Subject<'_>,
    width: usize,
    rows: &mut Vec<Row>,
) -> Vec<Line<'static>> {
    let theme = &state.theme;
    let detail = slot.detail();
    let mut out: Vec<Line> = vec![heading(theme, "PROPERTIES", width), Line::from("")];
    let named =
        |n: Option<&board_core::protocol::LinearNamed>| n.and_then(|n| n.name.as_deref()).map(line);
    let props: [(&str, Option<String>); 9] = [
        ("Status", issue.state_name().map(line)),
        ("Priority", issue.priority().map(|p| format!("P{p}"))),
        ("Assignee", issue.assignee().map(line)),
        (
            "Labels",
            (!issue.labels().is_empty()).then(|| {
                issue
                    .labels()
                    .iter()
                    .map(|l| line(l))
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
        ),
        ("Project", named(detail.and_then(|d| d.project.as_ref()))),
        (
            "Milestone",
            named(detail.and_then(|d| d.milestone.as_ref())),
        ),
        (
            "Cycle",
            detail.and_then(|d| d.cycle.as_ref()).and_then(|c| {
                c.name
                    .as_deref()
                    .map(line)
                    .or(c.number.map(|n| n.to_string()))
            }),
        ),
        (
            "Estimate",
            detail.and_then(|d| d.estimate).map(|e| format!("{e}")),
        ),
        ("Due", detail.and_then(|d| d.due_date.as_deref()).map(line)),
    ];
    for (label, value) in props {
        out.push(property(theme, label, value, width));
    }

    // Parent and relations appear only when the issue has them: unlike a value
    // property, an absent one is not a field with no value.
    if let Some(parent) = detail.and_then(|d| d.parent.as_ref()) {
        out.push(Line::from(""));
        out.push(heading(theme, "PARENT", width));
        rows.push(Row {
            line: out.len(),
            kind: RowKind::Issue(parent.identifier.clone()),
            column: Column::Sidebar,
            height: 1,
        });
        out.push(linked_row(parent, width));
    }
    let relations = detail.map(|d| d.relations.as_slice()).unwrap_or(&[]);
    if !relations.is_empty() {
        out.push(Line::from(""));
        out.push(heading(theme, "RELATIONS", width));
        for relation in relations {
            out.push(Line::from(Span::styled(
                fit(&format!("  {}", relation_label(relation)), width),
                dim(theme),
            )));
            rows.push(Row {
                line: out.len(),
                kind: RowKind::Issue(relation.issue.identifier.clone()),
                column: Column::Sidebar,
                height: 1,
            });
            out.push(linked_row(&relation.issue, width));
        }
    }

    // What the card showed when it was opened, kept after it clears.
    if !slot.marks.is_empty() {
        out.push(Line::from(""));
        out.push(heading(theme, "MARKS", width));
        for mark in slot.marks {
            let by = mark.created_by.as_deref().map(line).unwrap_or_default();
            let text = mark.text.as_deref().map(line).unwrap_or_default();
            out.push(Line::from(fit(
                &format!("{} {by}  {text}", crate::app::mark_glyph(mark.kind)),
                width,
            )));
        }
    }
    out.push(Line::from(""));
    out.push(heading(theme, "BOARD", width));
    let offset = out.len();
    out.extend(board_block(state, issue.bindings(), width, rows, offset));
    out
}

/// "blocks" and "blocked by" are the same Linear edge read from opposite ends,
/// so the direction is what the reader actually needs.
fn relation_label(relation: &board_core::protocol::LinearRelation) -> String {
    let kind = line(&relation.r#type);
    match (kind.as_str(), relation.direction.as_str()) {
        ("blocks", "inward") => "blocked by".into(),
        ("blocks", _) => "blocks".into(),
        (other, _) => other.to_string(),
    }
}

/// The board's own section: the worktrees, tabs and panes bound to this issue.
fn board_block(
    state: &LinearState,
    issue: Option<&LinearIssue>,
    width: usize,
    rows: &mut Vec<Row>,
    offset: usize,
) -> Vec<Line<'static>> {
    let theme = &state.theme;
    let mut out: Vec<Line> = vec![];
    // No row on the board at all: a linked issue the reader opened from this
    // page is not on it, which is a different thing from one that is on it with
    // no binding -- but reads the same to someone looking for a worktree.
    let Some(issue) = issue.filter(|i| !i.bindings.is_empty()) else {
        out.push(Line::from(Span::styled(
            fit("not on this board", width),
            dim(theme),
        )));
        return out;
    };
    let pane_rows = state.pane_rows(issue);
    let mut pane_index = 0usize;
    for (b, binding) in issue.bindings.iter().enumerate() {
        let state_label = format!("[{}] ", line(&binding.state));
        let path_w = width.saturating_sub(state_label.width());
        out.push(Line::from(Span::styled(
            format!("{state_label}{}", fit_path(&binding.worktree_path, path_w)),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        match &binding.tab {
            Some(tab) => out.push(Line::from(fit(
                &format!(
                    "  tab: {} ({})",
                    line(tab.label.as_deref().unwrap_or("(label unknown)")),
                    line(&tab.id)
                ),
                width,
            ))),
            None => out.push(Line::from(fit("  no recorded tab, so no panes", width))),
        }
        for row in pane_rows.iter().filter(|row| row.binding == b) {
            rows.push(Row {
                line: offset + out.len(),
                kind: RowKind::Pane(pane_index),
                column: Column::Sidebar,
                height: 1,
            });
            pane_index += 1;
            out.push(Line::from(fit(
                &format!("  {}  {}", line(&row.pane_id), row.status),
                width,
            )));
        }
        if binding.tab.is_some() && binding.panes.is_empty() {
            out.push(Line::from(fit("    (no panes listed)", width)));
        }
    }
    out
}

/// What the page draws its header and board block from. The board's snapshot
/// only holds the issues on the board, so a linked issue opened with Enter has
/// no row there; it comes from the row the reader opened instead.
enum Subject<'a> {
    OnBoard(&'a LinearIssue),
    Linked(&'a LinearLinkedIssue),
}

impl Subject<'_> {
    fn identifier(&self) -> &str {
        match self {
            Subject::OnBoard(issue) => &issue.identifier,
            Subject::Linked(row) => &row.identifier,
        }
    }

    fn title(&self) -> &str {
        match self {
            Subject::OnBoard(issue) => &issue.title,
            Subject::Linked(row) => &row.title,
        }
    }

    fn state(&self) -> &board_core::protocol::LinearIssueState {
        match self {
            Subject::OnBoard(issue) => &issue.state,
            Subject::Linked(row) => &row.state,
        }
    }

    fn state_name(&self) -> Option<&str> {
        self.state().name.as_deref()
    }

    fn priority(&self) -> Option<i64> {
        match self {
            Subject::OnBoard(issue) => issue.priority,
            Subject::Linked(_) => None,
        }
    }

    fn assignee(&self) -> Option<&str> {
        match self {
            Subject::OnBoard(issue) => issue.assignee.as_ref().and_then(|a| a.name.as_deref()),
            Subject::Linked(_) => None,
        }
    }

    fn labels(&self) -> &[String] {
        match self {
            Subject::OnBoard(issue) => &issue.labels,
            Subject::Linked(_) => &[],
        }
    }

    /// Only an issue on the board has bindings; a linked one says so.
    fn bindings(&self) -> Option<&LinearIssue> {
        match self {
            Subject::OnBoard(issue) => Some(issue),
            Subject::Linked(_) => None,
        }
    }
}

/// The issue the slot is for, whether the board knows it or not.
fn subject<'a>(state: &'a LinearState, slot: &Slot<'a>) -> Option<Subject<'a>> {
    if let Some(issue) = state.issue(slot.issue) {
        return Some(Subject::OnBoard(issue));
    }
    slot.seed.map(Subject::Linked)
}

/// The body under the issue's header, 2 cells in from each side, less the
/// session pane's hint row.
fn body_rect(rect: Rect, look: Look) -> Rect {
    let hint = u16::from(look == Look::Session);
    Rect::new(
        rect.x + INSET.min(rect.width),
        rect.y + HEADER_ROWS.min(rect.height),
        rect.width.saturating_sub(2 * INSET),
        rect.height.saturating_sub(HEADER_ROWS + hint),
    )
}

/// Build both columns and the rows the cursor can reach for `slot` drawn in
/// `rect`.
pub fn build(
    app: &App,
    state: &LinearState,
    slot: &Slot<'_>,
    rect: Rect,
    look: Look,
) -> Option<Page> {
    let issue = subject(state, slot)?;
    let theme = &state.theme;
    let body = body_rect(rect, look);
    let stacked = rect.width < TWO_COLUMN_MIN;
    let side_w = if stacked {
        0
    } else {
        SIDEBAR_W.min(rect.width / 3)
    };
    let main_w = if stacked {
        body.width
    } else {
        body.width.saturating_sub(side_w + INSET).min(MAIN_MAX)
    };
    let main_rect = Rect::new(body.x, body.y, main_w, body.height);
    let side_rect = Rect::new(
        body.right().saturating_sub(side_w),
        body.y,
        side_w,
        body.height,
    );
    let ground = look.ground(theme);
    let card_bg = theme.surface_above(ground);

    let mut rows = vec![];
    let main = main_column(
        app,
        state,
        slot,
        &issue,
        main_w as usize,
        card_bg,
        &mut rows,
    );
    let side_inner = if stacked {
        body.width
    } else {
        side_w.saturating_sub(2 * INSET)
    };
    // The sidebar's rows follow the main column's, which is the order they are
    // read in at both widths: the one-column layout is this list flattened.
    let mut sidebar_rows = vec![];
    let mut sidebar = sidebar_column(state, slot, &issue, side_inner as usize, &mut sidebar_rows);
    let main_len = main.len();
    if stacked {
        sidebar.insert(0, Line::from(""));
    }
    for row in sidebar_rows {
        rows.push(Row {
            line: if stacked {
                main_len + 1 + row.line
            } else {
                row.line
            },
            ..row
        });
    }
    let mut page = Page {
        main,
        sidebar,
        rows,
        stacked,
        body,
        main_rect,
        side_rect,
    };
    // The session pane has no row to act on, so it marks none.
    if app.mode != Mode::Session {
        let selected = LinearState::selected_row(&page.rows, slot.selection);
        mark_selected(&mut page, selected, theme, card_bg);
    }
    Some(page)
}

/// Put the cursor on the selected row: a line row gets the marker, a card a
/// raised surface. The view is keyboard-only, so a row the reader cannot see
/// they are on is a row they cannot act on.
fn mark_selected(page: &mut Page, cursor: usize, theme: &Theme, card_bg: Color) {
    let Some(row) = page.rows.get(cursor).cloned() else {
        return;
    };
    let main_len = page.main.len();
    let (lines, index) = match row.column {
        Column::Main => (&mut page.main, row.line),
        // A sidebar row's stored index is the stacked one; its own column
        // numbering is that minus everything the main column contributed.
        Column::Sidebar if page.stacked => (&mut page.sidebar, row.line.saturating_sub(main_len)),
        Column::Sidebar => (&mut page.sidebar, row.line),
    };
    if let RowKind::Link(_) = row.kind {
        let raised = theme.surface_above(card_bg);
        for target in lines.iter_mut().skip(index).take(row.height) {
            let spans: Vec<Span<'static>> = target
                .spans
                .iter()
                .map(|s| Span::styled(s.content.clone(), s.style.bg(raised)))
                .collect();
            *target = Line::from(spans).style(target.style.bg(raised));
        }
        return;
    }
    let Some(target) = lines.get_mut(index) else {
        return;
    };
    let text = target.to_string();
    let marked = match text.strip_prefix("  ") {
        Some(rest) => format!("▶ {rest}"),
        None => format!("▶ {text}"),
    };
    *target = Line::from(Span::styled(
        marked,
        Style::default().add_modifier(Modifier::REVERSED),
    ));
}

/// The issue's header row across the whole view: the powerline `ID │
/// status`, the breadcrumb, and the look's buttons right-aligned.
fn header_line(
    state: &LinearState,
    slot: &Slot<'_>,
    issue: &Subject<'_>,
    look: Look,
    width: usize,
) -> Line<'static> {
    let t = &state.theme;
    let ground = look.ground(t);
    let status = issue
        .state_name()
        .map(line)
        .or_else(|| {
            slot.detail()
                .and_then(|d| d.state.name.as_deref())
                .map(line)
        })
        .unwrap_or_else(|| "—".into());
    let status_fg = match issue.state().kind.as_deref() {
        Some("completed") => t.palette.green,
        Some("started") => t.palette.yellow,
        Some("canceled") => t.palette.red,
        _ => t.palette.subtext0,
    };
    let chip = t.palette.surface0;
    let mut left = vec![
        Span::styled(
            format!("  {} ", line(issue.identifier())),
            t.column_title_active(),
        ),
        Span::styled(SLANT, t.cut(t.palette.accent, chip)),
        Span::styled(
            format!(" {status} "),
            Style::default()
                .fg(status_fg)
                .bg(chip)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(SLANT, t.cut(chip, ground)),
    ];
    let mut right = vec![];
    for (glyph, label, key) in look.buttons() {
        right.push(Span::styled(format!(" {glyph} {label} "), t.chip()));
        right.push(Span::styled(
            format!("{key} "),
            Style::default().fg(t.palette.overlay0).bg(chip),
        ));
        right.push(Span::raw(" "));
    }
    let crumb = breadcrumb(state, slot);
    let used = super::linear::spans_width(&left) + super::linear::spans_width(&right);
    if !crumb.is_empty() {
        let room = width.saturating_sub(used + 2);
        if room > 1 {
            left.push(Span::styled(format!("  {}", fit(&crumb, room - 2)), dim(t)));
        }
    }
    let gap = width
        .saturating_sub(super::linear::spans_width(&left) + super::linear::spans_width(&right));
    left.push(Span::raw(" ".repeat(gap)));
    left.extend(right);
    Line::from(super::linear::clip_spans(left, width))
}

/// `project › milestone`, from the issue read, else the board's project.
fn breadcrumb(state: &LinearState, slot: &Slot<'_>) -> String {
    let detail = slot.detail();
    let project = detail
        .and_then(|d| d.project.as_ref())
        .and_then(|p| p.name.clone())
        .or_else(|| state.snapshot().and_then(|s| s.project.name.clone()));
    let milestone = detail
        .and_then(|d| d.milestone.as_ref())
        .and_then(|m| m.name.clone());
    [project, milestone]
        .into_iter()
        .flatten()
        .map(|s| line(&s))
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" › ")
}

/// Draw `slot` in `rect` as `look`.
pub fn draw(
    app: &App,
    state: &LinearState,
    slot: &Slot<'_>,
    f: &mut Frame,
    rect: Rect,
    look: Look,
) {
    let Some(issue) = subject(state, slot) else {
        return;
    };
    let t = &state.theme;
    let ground = look.ground(t);
    f.render_widget(Clear, rect);
    f.render_widget(
        Block::default().style(Style::default().bg(ground).fg(t.palette.text)),
        rect,
    );
    if rect.height < 4 || rect.width < 2 * INSET + 4 {
        f.render_widget(
            Paragraph::new(fit(" pane too small for the issue", rect.width as usize)),
            rect,
        );
        return;
    }
    let Some(page) = build(app, state, slot, rect, look) else {
        return;
    };
    let width = rect.width as usize;
    f.render_widget(
        Paragraph::new(header_line(state, slot, &issue, look, width)),
        Rect::new(rect.x, rect.y, rect.width, 1),
    );
    if let Some(error) = slot.error {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                fit(&line(&error.message), page.body.width as usize),
                t.warning(),
            ))),
            Rect::new(page.body.x, rect.y + 1, page.body.width, 1),
        );
    }

    let scroll = flow_scroll(app, &page, slot) as u16;
    if page.stacked {
        let mut all = page.main.clone();
        all.extend(page.sidebar.clone());
        f.render_widget(Paragraph::new(all).scroll((scroll, 0)), page.body);
    } else {
        f.render_widget(
            Paragraph::new(page.main.clone()).scroll((scroll, 0)),
            page.main_rect,
        );
        f.render_widget(
            Block::default().style(Style::default().bg(t.surface_above(ground))),
            page.side_rect,
        );
        let text = page.side_text();
        f.render_widget(
            Paragraph::new(page.sidebar.clone()).scroll((side_scroll(&page, slot) as u16, 0)),
            text,
        );
    }

    if look == Look::Session {
        let hint = Rect::new(rect.x, rect.bottom() - 1, rect.width, 1);
        f.render_widget(
            Paragraph::new(Span::styled(
                fit(" Tab lane list · j/k scroll · ? help · q close", width),
                dim(t),
            )),
            hint,
        );
    }
}

/// The scroll of the column that moves with the page. It starts from the
/// stored offset; while it follows the selection, it moves by the least that
/// keeps the selected row in view with a line either side.
fn flow_scroll(app: &App, page: &Page, slot: &Slot<'_>) -> usize {
    let height = page.flow_height();
    let max = page.flow_len().saturating_sub(height);
    let base = slot.scroll.min(max);
    // With no row chosen the view starts at the top, not at the first row.
    if app.mode == Mode::Session || !slot.follow || slot.selection.is_none() {
        return base;
    }
    let selected = LinearState::selected_row(&page.rows, slot.selection);
    let Some(row) = page.rows.get(selected).filter(|r| page.in_flow(r)) else {
        return base;
    };
    keep_in_view(base, row.line, row.height, height).min(max)
}

/// The sidebar scrolls only to show a selected row of its own.
fn side_scroll(page: &Page, slot: &Slot<'_>) -> usize {
    let height = page.side_text().height as usize;
    let max = page.sidebar.len().saturating_sub(height);
    let selected = LinearState::selected_row(&page.rows, slot.selection);
    match page.rows.get(selected).filter(|r| !page.in_flow(r)) {
        Some(row) => keep_in_view(0, row.line, row.height, height).min(max),
        None => 0,
    }
}

fn keep_in_view(base: usize, line: usize, height: usize, view: usize) -> usize {
    let pad = 1usize;
    let top = line.saturating_sub(pad);
    let bottom = (line + height + pad).saturating_sub(view);
    if top < base {
        top
    } else if bottom > base {
        bottom.min(top)
    } else {
        base
    }
}

/// The open issue's page at the last drawn size, and how its scroll stands:
/// what the reducer moves the cursor and the page through.
pub struct Layout {
    pub rows: Vec<Row>,
    pub scroll: usize,
    pub max: usize,
    pub height: usize,
    in_flow: Vec<bool>,
}

impl Layout {
    /// Whether `row` scrolls with the page, and sits inside the window.
    pub fn visible(&self, at: usize) -> bool {
        let Some(row) = self.rows.get(at) else {
            return false;
        };
        !self.in_flow[at] || (row.line >= self.scroll && row.line < self.scroll + self.height)
    }

    pub fn in_flow(&self, at: usize) -> bool {
        self.in_flow.get(at).copied().unwrap_or(false)
    }
}

pub fn layout(
    app: &App,
    state: &LinearState,
    slot: &Slot<'_>,
    rect: Rect,
    look: Look,
) -> Option<Layout> {
    let page = build(app, state, slot, rect, look)?;
    let height = page.flow_height();
    Some(Layout {
        scroll: flow_scroll(app, &page, slot),
        max: page.flow_len().saturating_sub(height),
        height,
        in_flow: page.rows.iter().map(|r| page.in_flow(r)).collect(),
        rows: page.rows,
    })
}
