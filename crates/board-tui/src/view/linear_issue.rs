//! The issue page: Linear's own issue page, in the pane.
//!
//! It is a page, not an overlay. `draw_detail` used to centre a bordered box
//! over the board; this replaces the board for as long as it is open, at every
//! width, so the reader is on the issue rather than looking at it through a
//! hole in something else.
//!
//! The page builds ONE list of sections per draw and lays out from it. That is
//! what keeps the cursor, Enter's target and the visible order from drifting
//! apart: a section with no rows contributes no row to move onto, and the
//! narrow layout is the same list flattened rather than a second arrangement of
//! the same content.

use board_core::protocol::{
    LinearIssue, LinearIssueDetail, LinearIssueDocument, LinearLinkedIssue,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::linear::{fit, line};
use super::truncate;
use crate::app::{App, LinearState};

/// Two columns need this much body width, the same rule the board stacks by.
const MIN_COL_W: u16 = 36;
/// The sidebar's share when there are two columns. Linear's is narrow: the
/// properties are short, and the description and Activity are what need room.
const SIDEBAR_W: u16 = 34;

/// One row the cursor can land on. `line` is its index in the rendered column,
/// which is what lets the page scroll to keep the selected row on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    pub kind: RowKind,
    pub column: Column,
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
}

/// Everything one draw needs: the rendered lines of each column, and the rows
/// the cursor can reach, in reading order.
pub struct Page {
    pub main: Vec<Line<'static>>,
    pub sidebar: Vec<Line<'static>>,
    /// Rows of the main column, then the sidebar's, which is the order they are
    /// read in at both widths.
    pub rows: Vec<Row>,
    pub stacked: bool,
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn heading(text: &str, width: usize) -> Line<'static> {
    Line::from(Span::styled(
        fit(text, width),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

/// A property row. The value is always drawn, as an em dash when Linear has
/// none: a row that disappears when empty makes "no milestone" and "not loaded
/// yet" the same thing on screen (R4).
fn property(label: &str, value: Option<String>, width: usize) -> Line<'static> {
    let value = value.filter(|v| !v.trim().is_empty());
    let text = format!("{label:<9} {}", value.clone().unwrap_or_else(|| "—".into()));
    Line::from(Span::styled(
        fit(&text, width),
        if value.is_some() {
            Style::default()
        } else {
            dim()
        },
    ))
}

/// A path in a narrow column keeps its TAIL: `…/worktrees/web-3312` says which
/// worktree this is, while the head it drops is the same for every one of them.
/// `fit` elides the other end, which is right for prose and wrong for a path.
fn fit_path(path: &str, width: usize) -> String {
    let path = line(path);
    if unicode_width::UnicodeWidthStr::width(path.as_str()) <= width || width <= 1 {
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
fn ago(now: i64, then: Option<&str>) -> String {
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
fn plain_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![];
    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            out.push(Line::from(""));
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if unicode_width::UnicodeWidthStr::width(candidate.as_str()) > width
                && !current.is_empty()
            {
                out.push(Line::from(line(&current)));
                current = word.to_string();
            } else {
                current = candidate;
            }
        }
        if !current.is_empty() {
            out.push(Line::from(line(&current)));
        }
    }
    out
}

/// The main column: what Linear puts on the left of its issue page.
fn main_column(
    now: i64,
    issue: &Subject<'_>,
    detail: Option<&LinearIssueDetail>,
    loading: bool,
    width: usize,
    rows: &mut Vec<Row>,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = vec![];
    for line_of_title in plain_lines(issue.title(), width) {
        out.push(Line::from(Span::styled(
            line_of_title.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
    }
    out.push(Line::from(""));

    match detail.and_then(|d| d.description.as_deref()) {
        Some(text) if !text.trim().is_empty() => {
            out.extend(crate::markdown::render(text, width));
        }
        _ if loading => out.push(Line::from(Span::styled("loading…", dim()))),
        Some(_) | None => out.push(Line::from(Span::styled("no description", dim()))),
    }
    out.push(Line::from(""));

    let children = detail.map(|d| d.children.as_slice()).unwrap_or(&[]);
    if !children.is_empty() {
        let done = children
            .iter()
            .filter(|c| c.state.kind.as_deref() == Some("completed"))
            .count();
        out.push(heading(
            &format!("Sub-issues  {done}/{}", children.len()),
            width,
        ));
        for child in children {
            rows.push(Row {
                line: out.len(),
                kind: RowKind::Issue(child.identifier.clone()),
                column: Column::Main,
            });
            out.push(linked_row(child, width));
        }
        out.push(Line::from(""));
    }

    out.push(heading("Activity", width));
    let comments = detail.map(|d| d.comments.as_slice()).unwrap_or(&[]);
    let history = detail.map(|d| d.history.as_slice()).unwrap_or(&[]);
    if comments.is_empty() && history.is_empty() {
        out.push(Line::from(Span::styled(
            if loading { "loading…" } else { "nothing yet" },
            dim(),
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
    out
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
    if !event.added_labels.is_empty() {
        return format!(
            "added {}",
            event
                .added_labels
                .iter()
                .map(|l| line(l))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !event.removed_labels.is_empty() {
        return format!(
            "removed {}",
            event
                .removed_labels
                .iter()
                .map(|l| line(l))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(to) = event.to_priority {
        return format!("set priority P{to}");
    }
    String::new()
}

/// The sidebar: Linear's properties, then the board's own block last (R10).
fn sidebar_column(
    state: &LinearState,
    issue: &Subject<'_>,
    detail: Option<&LinearIssueDetail>,
    width: usize,
    rows: &mut Vec<Row>,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = vec![];
    out.push(property("Status", issue.state_name().map(line), width));
    out.push(property(
        "Priority",
        issue.priority().map(|p| format!("P{p}")),
        width,
    ));
    out.push(property("Assignee", issue.assignee().map(line), width));
    out.push(property(
        "Labels",
        (!issue.labels().is_empty()).then(|| {
            issue
                .labels()
                .iter()
                .map(|l| line(l))
                .collect::<Vec<_>>()
                .join(", ")
        }),
        width,
    ));
    out.push(property(
        "Project",
        detail
            .and_then(|d| d.project.as_ref())
            .and_then(|p| p.name.as_deref())
            .map(line),
        width,
    ));
    out.push(property(
        "Milestone",
        detail
            .and_then(|d| d.milestone.as_ref())
            .and_then(|m| m.name.as_deref())
            .map(line),
        width,
    ));
    out.push(property(
        "Cycle",
        detail.and_then(|d| d.cycle.as_ref()).and_then(|c| {
            c.name
                .as_deref()
                .map(line)
                .or(c.number.map(|n| n.to_string()))
        }),
        width,
    ));
    out.push(property(
        "Estimate",
        detail.and_then(|d| d.estimate).map(|e| format!("{e}")),
        width,
    ));
    out.push(property(
        "Due",
        detail.and_then(|d| d.due_date.as_deref()).map(line),
        width,
    ));

    // Parent and relations appear only when the issue has them: unlike a value
    // property, an absent one is not a field with no value (R4).
    if let Some(parent) = detail.and_then(|d| d.parent.as_ref()) {
        out.push(Line::from(""));
        out.push(heading("Parent", width));
        rows.push(Row {
            line: out.len(),
            kind: RowKind::Issue(parent.identifier.clone()),
            column: Column::Sidebar,
        });
        out.push(linked_row(parent, width));
    }
    let relations = detail.map(|d| d.relations.as_slice()).unwrap_or(&[]);
    if !relations.is_empty() {
        out.push(Line::from(""));
        out.push(heading("Relations", width));
        for relation in relations {
            out.push(Line::from(Span::styled(
                fit(&format!("  {}", relation_label(relation)), width),
                dim(),
            )));
            rows.push(Row {
                line: out.len(),
                kind: RowKind::Issue(relation.issue.identifier.clone()),
                column: Column::Sidebar,
            });
            out.push(linked_row(&relation.issue, width));
        }
    }

    out.push(Line::from(""));
    out.push(heading("Board", width));
    out.extend(board_block(state, issue.bindings(), width, rows, out.len()));
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
    let mut out: Vec<Line> = vec![];
    // No row on the board at all: a linked issue the reader opened from this
    // page is not on it, which is a different thing from one that is on it with
    // no binding -- but reads the same to someone looking for a worktree.
    let Some(issue) = issue.filter(|i| !i.bindings.is_empty()) else {
        out.push(Line::from(Span::styled(
            fit("not on this board", width),
            dim(),
        )));
        return out;
    };
    let pane_rows = state.pane_rows(issue);
    let mut pane_index = 0usize;
    for (b, binding) in issue.bindings.iter().enumerate() {
        let state_label = format!("[{}] ", line(&binding.state));
        let path_w =
            width.saturating_sub(unicode_width::UnicodeWidthStr::width(state_label.as_str()));
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
    Linked(&'a board_core::protocol::LinearLinkedIssue),
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

    fn state_name(&self) -> Option<&str> {
        match self {
            Subject::OnBoard(issue) => issue.state.name.as_deref(),
            Subject::Linked(row) => row.state.name.as_deref(),
        }
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

/// The issue the page is for, whether the board knows it or not.
fn subject<'a>(state: &'a LinearState) -> Option<Subject<'a>> {
    if let Some(issue) = state.detail_issue() {
        return Some(Subject::OnBoard(issue));
    }
    state.detail_seed.as_ref().map(Subject::Linked)
}

/// Build both columns and the rows the cursor can reach.
pub(super) fn build(app: &App, state: &LinearState, area: Rect) -> Option<Page> {
    let issue = subject(state)?;
    let body = page_body(app, area);
    let stacked = body.width < 2 * MIN_COL_W;
    let (main_w, side_w) = if stacked {
        (body.width as usize, body.width as usize)
    } else {
        (
            body.width.saturating_sub(SIDEBAR_W + 1) as usize,
            SIDEBAR_W as usize,
        )
    };
    let detail = state
        .detail_doc
        .as_ref()
        .filter(|(id, _)| Some(id.as_str()) == state.detail.as_deref())
        .and_then(|(_, doc): &(String, LinearIssueDocument)| doc.issue.as_ref());
    let loading = state.detail_in_flight.is_some() && state.detail_error.is_none();

    let mut rows = vec![];
    let main = main_column(app.now, &issue, detail, loading, main_w, &mut rows);
    // The sidebar's rows follow the main column's, which is the order they are
    // read in at both widths: the narrow layout is this list flattened.
    let mut sidebar_rows = vec![];
    let sidebar = sidebar_column(state, &issue, detail, side_w, &mut sidebar_rows);
    let main_len = main.len();
    for row in sidebar_rows {
        rows.push(Row {
            // In the stacked layout the sidebar follows the main column, so a
            // row's index is its own plus everything above it.
            line: if stacked {
                main_len + row.line
            } else {
                row.line
            },
            kind: row.kind,
            column: row.column,
        });
    }
    let mut page = Page {
        main,
        sidebar,
        rows,
        stacked,
    };
    let selected = LinearState::selected_row(&page.rows, state.detail_selection.as_ref());
    mark_selected(&mut page, selected, stacked, main_len);
    Some(page)
}

/// The whole pane, less a row for a toast. The page draws no board header, so
/// reserving the board's rows here would leave blank lines at the top of every
/// issue page.
fn page_area(app: &App, area: Rect) -> Rect {
    let toast = u16::from(app.toast.is_some() && area.height > 1);
    Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(toast),
    )
}

/// Put the cursor marker on the selected row. The page is keyboard-only, so a
/// row the reader cannot see they are on is a row they cannot act on.
fn mark_selected(page: &mut Page, cursor: usize, stacked: bool, main_len: usize) {
    let Some(row) = page.rows.get(cursor) else {
        return;
    };
    let (lines, index) = match row.column {
        Column::Main => (&mut page.main, row.line),
        // A sidebar row's stored index is the stacked one; its own column
        // numbering is that minus everything the main column contributed.
        Column::Sidebar if stacked => (&mut page.sidebar, row.line.saturating_sub(main_len)),
        Column::Sidebar => (&mut page.sidebar, row.line),
    };
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

/// The page's body: everything under the title rows and above the hint row.
fn page_body(app: &App, area: Rect) -> Rect {
    let content = page_area(app, area);
    let header = header_rows(app);
    Rect::new(
        content.x,
        content.y.saturating_add(header),
        content.width,
        content.height.saturating_sub(header + 1),
    )
}

/// The identifier row, plus a failure row when there is one, plus a blank.
fn header_rows(app: &App) -> u16 {
    let failed = app
        .linear
        .as_ref()
        .is_some_and(|s| s.detail_error.is_some());
    if failed {
        3
    } else {
        2
    }
}

pub(super) fn draw(app: &App, state: &LinearState, f: &mut Frame, area: Rect) {
    let Some(issue) = subject(state) else {
        return;
    };
    let content = page_area(app, area);
    if content.height < 4 || content.width == 0 {
        f.render_widget(
            Paragraph::new(truncate(
                " pane too small for the issue page",
                content.width as usize,
            )),
            content,
        );
        return;
    }
    let Some(page) = build(app, state, area) else {
        return;
    };

    // Title row, then a blank: the issue's identifier is the page's name.
    let width = content.width as usize;
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            fit(&format!(" {}", line(issue.identifier())), width),
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        Rect::new(content.x, content.y, content.width, 1),
    );
    if let Some(error) = state.detail_error.as_ref() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                fit(&format!(" {}", error.message), width),
                Style::default().fg(Color::Yellow),
            ))),
            Rect::new(content.x, content.y + 1, content.width, 1),
        );
    }

    let body = page_body(app, area);
    let scroll = page_scroll(&page, state, body.height as usize);
    if page.stacked {
        let mut all = page.main.clone();
        all.extend(page.sidebar.clone());
        f.render_widget(Paragraph::new(all).scroll((scroll as u16, 0)), body);
    } else {
        let main_w = body.width.saturating_sub(SIDEBAR_W + 1);
        f.render_widget(
            Paragraph::new(page.main.clone()).scroll((scroll as u16, 0)),
            Rect::new(body.x, body.y, main_w, body.height),
        );
        f.render_widget(
            Paragraph::new(page.sidebar.clone()).scroll((scroll as u16, 0)),
            Rect::new(
                body.x + main_w + 1,
                body.y,
                SIDEBAR_W.min(body.width.saturating_sub(main_w)),
                body.height,
            ),
        );
    }

    let hint = Rect::new(content.x, content.bottom() - 1, content.width, 1);
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate(hint_text(state), content.width as usize),
            dim(),
        )),
        hint,
    );
}

fn hint_text(state: &LinearState) -> &'static str {
    match state.detail_error.as_ref() {
        Some(e) if e.retryable => {
            " j/k move · Enter open · r retry · o focus pane · u Linear · y copy · b bind · Esc back"
        }
        _ => " j/k move · Enter open · o focus pane · u Linear · y copy · b bind · Esc back",
    }
}

/// Scroll the page as one unit, keeping the selected row on screen with a line
/// of context either side where the content allows (R5).
fn page_scroll(page: &Page, state: &LinearState, height: usize) -> usize {
    let total = if page.stacked {
        page.main.len() + page.sidebar.len()
    } else {
        page.main.len().max(page.sidebar.len())
    };
    if total <= height {
        return 0;
    }
    let max = total - height;
    let selected = LinearState::selected_row(&page.rows, state.detail_selection.as_ref());
    let Some(row) = page.rows.get(selected) else {
        return 0;
    };
    let pad = 1usize;
    let top = row.line.saturating_sub(pad);
    let bottom = (row.line + pad + 1).saturating_sub(height);
    top.max(bottom).min(max)
}
