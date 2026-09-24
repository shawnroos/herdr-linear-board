//! Linear-mode rendering: the space board built from the work plugin's
//! snapshot, the card detail, and the not-bound / error / stale-daemon
//! screens. Only `Mode::Linear` reaches this module, and it reads
//! `App::linear` only, never `App::board`.

use board_core::protocol::{LinearIssue, LinearSnapshot};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{sanitise, App, LinearState, Screen};
use crate::widgets::{HitMap, Zone};

use super::linear_strip::{draw_strip, strip_lines};
use super::{centered_rect_abs, linear_help_keys, truncate};

// Identifier, two title lines, assignee, then the blank separator row.
const CARD_H: u16 = 5;
const MIN_COL_W: u16 = 36;
const HEADER_ROWS: u16 = 2;
const DEFAULT_VIEW_LABEL: &str = "view: project issues (default)";

/// `Linear: <bound object>`: the project, else the space label, else the space
/// id. Brackets go with control and format characters so no name can forge
/// the kanban's `Board [...]` title or break the launcher's matcher.
pub fn linear_pane_title(snapshot: &LinearSnapshot, workspace_id: &str) -> String {
    let clean = |name: &str| {
        board_core::text::strip_control_and_format(name)
            .chars()
            .filter(|c| !matches!(c, '[' | ']'))
            .collect::<String>()
            .trim()
            .to_string()
    };
    let name = [
        snapshot.project.name.as_deref().unwrap_or_default(),
        &snapshot.workspace.label,
        &snapshot.workspace.id,
        workspace_id,
    ]
    .into_iter()
    .map(clean)
    .find(|name| !name.is_empty())
    .unwrap_or_default();
    format!("Linear: {name}")
}

pub(super) fn draw(app: &App, f: &mut Frame) {
    app.hit_map.borrow_mut().clear();
    let area = f.area();
    let Some(state) = app.linear.as_ref() else {
        return;
    };
    match app.screen {
        Screen::LinearNotBound => draw_not_bound(app, state, f, area),
        Screen::LinearStaleDaemon => draw_stale_daemon(state, f, area),
        Screen::LinearError => {
            draw_board(app, state, f, area);
            draw_error(state, f, area);
        }
        // A page, not an overlay: it replaces the board rather than sitting
        // over it, at every width.
        Screen::LinearDetail => super::linear_issue::draw(app, state, f, area),
        Screen::Help => {
            draw_backdrop(app, state, f, area, app.help_return_to);
            draw_help(app, state, f, area);
        }
        Screen::LinearPicker => {
            let return_to = app.picker.as_ref().map(|p| p.return_to);
            draw_backdrop(
                app,
                state,
                f,
                area,
                return_to.unwrap_or(Screen::LinearBoard),
            );
            super::linear_picker::draw_linear_picker(app, state, f, area);
        }
        _ => draw_board(app, state, f, area),
    }
    draw_bottom(app, f, area);
}

/// What an overlay covers: the not-bound screen when it opened there, the
/// board otherwise.
fn draw_backdrop(app: &App, state: &LinearState, f: &mut Frame, area: Rect, under: Screen) {
    if under == Screen::LinearNotBound {
        draw_not_bound(app, state, f, area);
    } else {
        draw_board(app, state, f, area);
    }
}

/// Single-line cell text: the sanitiser keeps tab and newline, which a
/// one-row cell cannot show.
pub(super) fn line(s: &str) -> String {
    if s.contains(['\n', '\t']) {
        s.replace(['\n', '\t'], " ")
    } else {
        s.to_string()
    }
}

fn age(app: &App, state: &LinearState) -> String {
    match state.fetched_at {
        None => "never".to_string(),
        Some(at) => {
            let secs = (app.now - at).max(0);
            if secs < 60 {
                format!("{secs}s ago")
            } else if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else {
                format!("{}h ago", secs / 3600)
            }
        }
    }
}

/// The header's two lines, and the columns of the first line the view text
/// spans as `(start, width)`.
fn header_lines(app: &App, state: &LinearState) -> (Vec<Line<'static>>, (u16, u16)) {
    let snapshot = state.snapshot();
    let project = snapshot
        .and_then(|s| s.project.name.clone())
        .unwrap_or_else(|| "(no project)".to_string());
    let view = match snapshot.map(|s| (s.view.status.as_str(), s.view.name.clone())) {
        Some(("ok", Some(name))) => format!("view: {}", line(&name)),
        Some(("none", _)) => DEFAULT_VIEW_LABEL.to_string(),
        None => "no view chosen: /work:bind".to_string(),
        // The record still names the view Linear no longer has, so say which
        // one failed and that a new choice is needed (AE9).
        Some((status @ ("not_found" | "archived" | "not_in_project"), Some(name))) => format!(
            "view {} {} · no view chosen: /work:bind",
            line(&name),
            status.replace('_', " ")
        ),
        Some((status, Some(name))) => {
            format!("view: {} ({})", line(&name), status.replace('_', " "))
        }
        Some((status, None)) => format!("view {}", status.replace('_', " ")),
    };
    let space = snapshot
        .map(|s| line(&s.workspace.label))
        .unwrap_or_else(|| state.workspace_id.clone());
    let badge = " Linear ";
    let lead = format!(" {} · ", line(&project));
    let view_span = (
        u16::try_from(badge.width() + lead.width()).unwrap_or(u16::MAX),
        u16::try_from(view.width()).unwrap_or(u16::MAX),
    );
    let mut first = vec![
        Span::styled(
            badge,
            Style::default().fg(Color::Black).bg(Color::LightBlue),
        ),
        Span::raw(lead),
        Span::raw(view),
        Span::raw(format!(" · space: {space} · fetched {}", age(app, state))),
    ];
    if state.in_flight {
        first.push(Span::styled(
            " · refreshing…",
            Style::default().fg(Color::Yellow),
        ));
    }
    let mut warnings = state.source_warnings();
    if let Some(mapping) = state.non_default_mapping() {
        warnings.push(format!(
            "mapping {mapping} is not the default; rendering as default"
        ));
    }
    let mut second = if warnings.is_empty() {
        vec![Span::styled(
            " Enter detail · r refresh · ? help · q quit",
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        vec![Span::styled(
            format!(" ! {}", warnings.join(" · ")),
            Style::default().fg(Color::LightYellow),
        )]
    };
    if let Some(note) = &state.bind_note {
        // Ahead of the key hints and warnings, which the width may cut.
        second.insert(
            0,
            Span::styled(format!(" {note} ·"), Style::default().fg(Color::LightGreen)),
        );
    }
    (vec![Line::from(first), Line::from(second)], view_span)
}

fn split_at_width(s: &str, max: usize) -> (&str, &str) {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        used += c.width().unwrap_or(0);
        if used > max {
            return (&s[..i], &s[i..]);
        }
    }
    (s, "")
}

/// Truncation by display cells: `view::truncate` counts chars, which lets a
/// wide glyph push the ellipsis past the column edge.
pub(super) fn fit(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    format!("{}…", split_at_width(s, max - 1).0)
}

/// Two title lines: the first breaks at a word boundary (mid-word only when
/// one word is wider than the column), the second takes the rest and ends in
/// an ellipsis when it does not fit.
fn title_lines(title: &str, width: usize) -> [String; 2] {
    let words: Vec<&str> = title.split_whitespace().collect();
    if words.is_empty() {
        return ["(no title)".to_string(), String::new()];
    }
    let text = words.join(" ");
    if text.width() <= width {
        return [text, String::new()];
    }
    let mut first_len = 0;
    for word in &words {
        let end = if first_len == 0 {
            word.len()
        } else {
            first_len + 1 + word.len()
        };
        if text[..end].width() > width {
            break;
        }
        first_len = end;
    }
    let (first, rest) = if first_len == 0 {
        split_at_width(&text, width)
    } else {
        (&text[..first_len], &text[first_len..])
    };
    [first.to_string(), fit(rest.trim_start(), width)]
}

fn card_lines(issue: &LinearIssue, width: usize) -> Vec<String> {
    let mut first = line(&issue.identifier);
    if let Some(p) = issue.priority {
        first.push_str(&format!("  P{p}"));
    }
    if issue.stale {
        first.push_str("  ~stale");
    }
    let panes = LinearState::pane_count(issue);
    if panes > 0 {
        first.push_str(&format!("  ⧉{panes}"));
    }
    let assignee = issue
        .assignee
        .as_ref()
        .and_then(|a| a.name.as_deref())
        .map(|n| format!("@{}", line(n)))
        .unwrap_or_else(|| "unassigned".to_string());
    let [title_first, title_second] = title_lines(&line(&issue.title), width);
    vec![
        fit(&first, width),
        title_first,
        title_second,
        fit(&assignee, width),
    ]
}

fn draw_board(app: &App, state: &LinearState, f: &mut Frame, area: Rect) {
    let (header, (view_x, view_w)) = header_lines(app, state);
    let header_area = Rect::new(area.x, area.y, area.width, HEADER_ROWS.min(area.height));
    f.render_widget(Paragraph::new(header), header_area);
    let view_w = view_w.min(area.width.saturating_sub(view_x));
    if header_area.height > 0 && view_w > 0 {
        app.hit_map.borrow_mut().push(
            Rect::new(area.x + view_x, area.y, view_w, 1),
            Zone::LinearHeaderView,
        );
    }

    let Some(snapshot) = state.snapshot() else {
        let body = Rect::new(
            area.x,
            area.y + HEADER_ROWS,
            area.width,
            area.height.saturating_sub(HEADER_ROWS + 1),
        );
        f.render_widget(Paragraph::new(" waiting for the first snapshot…"), body);
        return;
    };
    let strip = strip_lines(state, snapshot, area.width as usize);
    let strip_h = strip.len() as u16;
    let footer_h: u16 = 1;
    let body_h = area.height.saturating_sub(HEADER_ROWS + strip_h + footer_h);
    let body = Rect::new(area.x, area.y + HEADER_ROWS, area.width, body_h);
    draw_columns(state, snapshot, f, body, &mut app.hit_map.borrow_mut());
    draw_strip(app, strip, f, area, body.bottom());
}

fn draw_columns(
    state: &LinearState,
    snapshot: &LinearSnapshot,
    f: &mut Frame,
    body: Rect,
    hit_map: &mut HitMap,
) {
    let groups = &snapshot.groups;
    if body.height == 0 {
        return;
    }
    if groups.is_empty() {
        f.render_widget(Paragraph::new(" no columns in this snapshot"), body);
        return;
    }
    if body.height < 3 || body.width == 0 {
        f.render_widget(
            Paragraph::new(truncate(
                " body too short for a card; make the pane taller",
                body.width as usize,
            )),
            body,
        );
        return;
    }
    let stacked = body.width < 2 * MIN_COL_W;
    let sel_group = state.sel_group.min(groups.len() - 1);
    let sel_card = state
        .sel_card
        .min(groups[sel_group].issues.len().saturating_sub(1));
    let visible = if stacked {
        1
    } else {
        ((body.width / MIN_COL_W) as usize).min(groups.len())
    };
    let start = sel_group
        .saturating_sub(visible - 1)
        .min(groups.len() - visible);
    let col_w = body.width / visible as u16;
    for (slot, (idx, group)) in groups
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .enumerate()
    {
        let x = body.x + slot as u16 * col_w;
        let rect = Rect::new(x, body.y, col_w, body.height);
        let focused = idx == sel_group;
        let position = if stacked {
            format!("· {}/{} ", idx + 1, groups.len())
        } else {
            String::new()
        };
        let title = fit(
            &format!(
                " {} ({}) {position}",
                line(&group.label),
                group.issues.len()
            ),
            col_w.saturating_sub(2) as usize,
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(if focused {
                Style::default().fg(Color::LightBlue)
            } else {
                Style::default().fg(Color::DarkGray)
            });
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        hit_map.push(
            Rect::new(x, body.y, col_w, 1),
            Zone::LinearGroup(group.key.clone()),
        );
        // The last card may drop its separator row at the column's bottom edge.
        let per_col = ((inner.height + 1) / CARD_H).max(1) as usize;
        let first = if focused {
            sel_card.saturating_sub(per_col - 1)
        } else {
            0
        };
        for (row, identifier) in group.issues.iter().enumerate().skip(first).take(per_col) {
            let y = inner.y + ((row - first) as u16) * CARD_H;
            let card = Rect::new(
                inner.x,
                y,
                inner.width,
                (CARD_H - 1).min(inner.bottom().saturating_sub(y)),
            );
            let selected = focused && row == sel_card;
            let lines: Vec<Line> = match snapshot.issues.get(identifier) {
                Some(issue) => card_lines(issue, inner.width as usize)
                    .into_iter()
                    .map(Line::from)
                    .collect(),
                None => vec![Line::from(fit(
                    &format!("{identifier} (missing)"),
                    inner.width as usize,
                ))],
            };
            let style = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            f.render_widget(Paragraph::new(lines).style(style), card);
            hit_map.push(
                card,
                Zone::LinearCard {
                    group: group.key.clone(),
                    identifier: identifier.clone(),
                },
            );
        }
    }
}

fn boxed(f: &mut Frame, area: Rect, title: &str, color: Color, lines: Vec<Line<'static>>) {
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = centered_rect_abs(area.width.saturating_sub(4).clamp(20, 90), h, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(Style::default().fg(color));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_not_bound(app: &App, state: &LinearState, f: &mut Frame, area: Rect) {
    let (label, record) = match state.snapshot() {
        Some(s) => (
            line(&s.workspace.label),
            match (s.record.status.as_str(), s.record.state.as_deref()) {
                ("unreadable", _) => "the workspace record is unreadable".to_string(),
                ("missing", _) => "no workspace record exists".to_string(),
                (_, Some(state)) => format!("the workspace record is {state}"),
                (status, None) => format!("workspace record status: {status}"),
            },
        ),
        None => (state.workspace_id.clone(), String::new()),
    };
    let mut lines = vec![
        Line::from(format!(
            "Space {label} ({}) is not bound to a Linear project.",
            state.workspace_id
        )),
        Line::from(record),
        Line::from(""),
        Line::from("Press s (or click a space below) to pick a project for it,"),
        Line::from("or run /work:bind in this space; then press r to refresh."),
    ];
    if let Some(note) = &state.bind_note {
        lines.push(Line::from(Span::styled(
            note.clone(),
            Style::default().fg(Color::LightGreen),
        )));
    }
    for warning in state.source_warnings() {
        lines.push(Line::from(Span::styled(
            format!("! {warning}"),
            Style::default().fg(Color::LightYellow),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "s pick a space · r refresh · q quit",
        Style::default().fg(Color::DarkGray),
    )));
    let Some(snapshot) = state.snapshot() else {
        boxed(f, area, "Not bound", Color::LightYellow, lines);
        return;
    };
    let strip = strip_lines(state, snapshot, area.width as usize);
    // The bottom row stays free for the toast, as under the board's strip.
    let strip_y = area
        .bottom()
        .saturating_sub(strip.len() as u16 + 1)
        .max(area.y);
    let above = Rect::new(area.x, area.y, area.width, strip_y - area.y);
    boxed(f, above, "Not bound", Color::LightYellow, lines);
    draw_strip(app, strip, f, area, strip_y);
}

fn draw_stale_daemon(state: &LinearState, f: &mut Frame, area: Rect) {
    let mut lines = vec![
        Line::from("The board daemon lacks linear.snapshot; it is older than this board."),
        Line::from(""),
    ];
    if let Some((board, daemon)) = &state.stale_daemon {
        let daemon = daemon.as_deref().unwrap_or("unknown");
        if daemon != board {
            lines.push(Line::from(format!("board {board} · daemon {daemon}")));
            lines.push(Line::from(""));
        }
    }
    lines.push(Line::from("Run `board daemon stop` and reopen the board."));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "r retry · q quit",
        Style::default().fg(Color::DarkGray),
    )));
    boxed(
        f,
        area,
        "Daemon is older than this board",
        Color::LightRed,
        lines,
    );
}

fn draw_error(state: &LinearState, f: &mut Frame, area: Rect) {
    let mut lines = vec![Line::from(Span::styled(
        line(state.error.as_deref().unwrap_or("snapshot failed")),
        Style::default().fg(Color::LightRed),
    ))];
    lines.push(Line::from(""));
    lines.push(Line::from(if state.last_good.is_some() {
        "The last good snapshot stays on screen behind this message."
    } else {
        "No snapshot has arrived yet."
    }));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        if state.last_good.is_some() {
            "r retry · Esc dismiss · q quit"
        } else {
            "r retry · q quit"
        },
        Style::default().fg(Color::DarkGray),
    )));
    boxed(f, area, "Snapshot failed", Color::LightRed, lines);
}

const HELP_KEY_W: usize = 12;
const HERDR_KEY_W_MAX: usize = 24;

fn help_heading(text: &str, width: usize) -> Line<'static> {
    Line::from(Span::styled(
        fit(text, width),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

fn help_row(key: &str, key_w: usize, description: &str, width: usize) -> Line<'static> {
    let key = fit(key, key_w);
    let pad = " ".repeat(key_w.saturating_sub(key.width()));
    Line::from(fit(&format!("  {key}{pad} {description}"), width))
}

fn help_body(state: &LinearState, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let mut section: Option<Screen> = None;
    for (screen, key, description) in linear_help_keys() {
        if *key == "--" {
            continue;
        }
        if section != Some(*screen) {
            section = Some(*screen);
            lines.push(help_heading(
                match screen {
                    Screen::LinearBoard => "board",
                    Screen::LinearDetail => "card detail",
                    Screen::LinearNotBound => "not bound",
                    Screen::LinearError => "snapshot failed",
                    Screen::LinearStaleDaemon => "daemon older than board",
                    Screen::LinearPicker => "picker",
                    _ => "",
                },
                width,
            ));
        }
        lines.push(help_row(key, HELP_KEY_W, description, width));
    }
    if !state.herdr_keys.is_empty() {
        let key_w = state
            .herdr_keys
            .iter()
            .map(|row| row.key.width())
            .max()
            .unwrap_or(0)
            .clamp(HELP_KEY_W, HERDR_KEY_W_MAX);
        lines.push(help_heading("herdr (your config)", width));
        for row in &state.herdr_keys {
            lines.push(help_row(&row.key, key_w, &row.label, width));
        }
    }
    lines
}

struct HelpGeometry {
    sheet: Rect,
    body: Rect,
    footer: Rect,
}

// One footer row stays pinned below the scrolling body.
fn help_geometry(area: Rect, body_rows: usize) -> HelpGeometry {
    let wanted = u16::try_from(body_rows)
        .unwrap_or(u16::MAX)
        .saturating_add(3);
    let sheet = centered_rect_abs(
        area.width.saturating_sub(4).clamp(20, 90),
        wanted.min(area.height),
        area,
    );
    let inner = Block::default().borders(Borders::ALL).inner(sheet);
    let body_h = inner.height.saturating_sub(1);
    HelpGeometry {
        sheet,
        body: Rect::new(inner.x, inner.y, inner.width, body_h),
        footer: Rect::new(inner.x, inner.y + body_h, inner.width, inner.height.min(1)),
    }
}

// The rightmost body column is left for the scrollbar.
fn help_text_width(area: Rect) -> usize {
    area.width.saturating_sub(4).clamp(20, 90).saturating_sub(3) as usize
}

pub fn linear_help_max_scroll(app: &App, area: Rect) -> usize {
    let Some(state) = app.linear.as_ref() else {
        return 0;
    };
    let rows = help_body(state, help_text_width(area)).len();
    rows.saturating_sub(help_geometry(area, rows).body.height as usize)
}

fn draw_help(app: &App, state: &LinearState, f: &mut Frame, area: Rect) {
    let lines = help_body(state, help_text_width(area));
    let total = lines.len();
    let geometry = help_geometry(area, total);
    let visible = geometry.body.height as usize;
    let max_scroll = total.saturating_sub(visible);
    let scroll = app.help_scroll.min(max_scroll);
    f.render_widget(Clear, geometry.sheet);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(" Help — Linear mode ")
            .border_style(Style::default().fg(Color::LightBlue)),
        geometry.sheet,
    );
    let shown: Vec<Line> = lines.into_iter().skip(scroll).take(visible).collect();
    f.render_widget(Paragraph::new(shown), geometry.body);
    let below = max_scroll - scroll;
    let footer = if below > 0 {
        format!("↑/↓ k/j scroll · {below} more below · any other key closes")
    } else {
        "any other key closes".to_string()
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            fit(&footer, geometry.footer.width as usize),
            Style::default().fg(Color::DarkGray),
        )),
        geometry.footer,
    );
    if max_scroll > 0 && geometry.body.width > 0 {
        let bar = Rect::new(
            geometry.body.right() - 1,
            geometry.body.y,
            1,
            geometry.body.height,
        );
        crate::widgets::vertical_scrollbar(f, bar, total, scroll, visible);
    }
}

fn draw_bottom(app: &App, f: &mut Frame, area: Rect) {
    let Some(toast) = &app.toast else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let rect = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    let style = if toast.is_error {
        Style::default().fg(Color::White).bg(Color::Red)
    } else {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            fit(
                &format!(" {} ", line(&sanitise(&toast.text))),
                area.width as usize,
            ),
            style,
        )),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::super::{
        linear_help_keys, upstream_help_keys, upstream_help_rows, HELP_KEYS, LINEAR_HELP_SENTINEL,
    };
    use crate::app::Screen;

    fn is_linear(screen: &Screen) -> bool {
        matches!(
            screen,
            Screen::LinearBoard
                | Screen::LinearDetail
                | Screen::LinearNotBound
                | Screen::LinearError
                | Screen::LinearStaleDaemon
                | Screen::LinearPicker
        )
    }

    #[test]
    fn the_linear_rows_start_exactly_at_the_upstream_boundary() {
        let boundary = upstream_help_rows();
        assert!(boundary < HELP_KEYS.len(), "the sentinel row is missing");
        assert_eq!(HELP_KEYS[boundary].1, "--");
        assert_eq!(HELP_KEYS[boundary].2, LINEAR_HELP_SENTINEL);
        assert!(upstream_help_keys().iter().all(|(s, _, _)| !is_linear(s)));
        assert!(linear_help_keys().iter().all(|(s, _, _)| is_linear(s)));
    }
}
