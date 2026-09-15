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

use crate::app::{sanitise, App, LinearState, Screen};

use super::{centered_rect_abs, linear_help_keys, truncate};

const CARD_H: u16 = 3;
const MIN_COL_W: u16 = 18;
const HEADER_ROWS: u16 = 2;

pub(super) fn draw(app: &App, f: &mut Frame) {
    app.hit_map.borrow_mut().clear();
    let area = f.area();
    let Some(state) = app.linear.as_ref() else {
        return;
    };
    match app.screen {
        Screen::LinearNotBound => draw_not_bound(state, f, area),
        Screen::LinearStaleDaemon => draw_stale_daemon(state, f, area),
        Screen::LinearError => {
            draw_board(app, state, f, area);
            draw_error(state, f, area);
        }
        Screen::LinearDetail => {
            draw_board(app, state, f, area);
            draw_detail(state, f, area);
        }
        Screen::Help => {
            draw_board(app, state, f, area);
            draw_help(f, area);
        }
        _ => draw_board(app, state, f, area),
    }
    draw_bottom(app, f, area);
}

/// Single-line cell text: the sanitiser keeps tab and newline, which a
/// one-row cell cannot show.
fn line(s: &str) -> String {
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

fn header_lines(app: &App, state: &LinearState) -> Vec<Line<'static>> {
    let snapshot = state.snapshot();
    let project = snapshot
        .and_then(|s| s.project.name.clone())
        .unwrap_or_else(|| "(no project)".to_string());
    let view = match snapshot.map(|s| (s.view.status.as_str(), s.view.name.clone())) {
        Some(("ok", Some(name))) => format!("view: {}", line(&name)),
        Some(("none", _)) | None => "no view chosen: /work:bind".to_string(),
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
    let mut first = vec![
        Span::styled(
            " Linear ",
            Style::default().fg(Color::Black).bg(Color::LightBlue),
        ),
        Span::raw(format!(
            " {} · {view} · space: {space} · fetched {}",
            line(&project),
            age(app, state)
        )),
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
    let second = if warnings.is_empty() {
        Line::from(Span::styled(
            " Enter detail · r refresh · ? help · q quit",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Line::from(Span::styled(
            format!(" ! {}", warnings.join(" · ")),
            Style::default().fg(Color::LightYellow),
        ))
    };
    vec![Line::from(first), second]
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
    vec![
        truncate(&first, width),
        truncate(&line(&issue.title), width),
        truncate(&assignee, width),
    ]
}

fn draw_board(app: &App, state: &LinearState, f: &mut Frame, area: Rect) {
    let header = header_lines(app, state);
    let header_area = Rect::new(area.x, area.y, area.width, HEADER_ROWS.min(area.height));
    f.render_widget(Paragraph::new(header), header_area);

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
    let unmapped_h: u16 = if snapshot.unmapped.is_empty() {
        0
    } else {
        1 + snapshot.unmapped.len().min(3) as u16
    };
    let footer_h: u16 = 1;
    let body_h = area
        .height
        .saturating_sub(HEADER_ROWS + unmapped_h + footer_h);
    let body = Rect::new(area.x, area.y + HEADER_ROWS, area.width, body_h);
    draw_columns(state, snapshot, f, body);

    // The strip is clamped to the frame: at a tiny height the body saturates
    // to zero rows and an unclamped rect would index past the buffer.
    let y = body.bottom();
    let strip_h = unmapped_h.min(area.bottom().saturating_sub(y));
    if strip_h > 0 {
        let rect = Rect::new(area.x, y, area.width, strip_h);
        let mut lines = vec![Line::from(Span::styled(
            " Unmapped tabs (live tabs no binding claims)",
            Style::default().add_modifier(Modifier::BOLD),
        ))];
        for tab in snapshot.unmapped.iter().take(3) {
            let panes: Vec<String> = tab
                .panes
                .iter()
                .map(|p| format!("{} {}", line(p), state.pane_status(p)))
                .collect();
            lines.push(Line::from(truncate(
                &format!(
                    "  {} ({}) · {} · panes: {}",
                    line(tab.label.as_deref().unwrap_or("(no label)")),
                    line(&tab.tab_id),
                    line(&tab.reason),
                    if panes.is_empty() {
                        "none".to_string()
                    } else {
                        panes.join(", ")
                    }
                ),
                area.width as usize,
            )));
        }
        f.render_widget(Paragraph::new(lines), rect);
    }
}

fn draw_columns(state: &LinearState, snapshot: &LinearSnapshot, f: &mut Frame, body: Rect) {
    let groups = &snapshot.groups;
    if groups.is_empty() || body.height < 3 || body.width == 0 {
        if body.height > 0 {
            f.render_widget(Paragraph::new(" no columns in this snapshot"), body);
        }
        return;
    }
    let visible = ((body.width / MIN_COL_W).max(1) as usize).min(groups.len());
    let start = state
        .sel_group
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
        let focused = idx == state.sel_group;
        let title = truncate(
            &format!(" {} ({}) ", line(&group.label), group.issues.len()),
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
        let per_col = (inner.height / CARD_H).max(1) as usize;
        let first = if focused {
            state.sel_card.saturating_sub(per_col - 1)
        } else {
            0
        };
        for (row, identifier) in group.issues.iter().enumerate().skip(first).take(per_col) {
            let y = inner.y + ((row - first) as u16) * CARD_H;
            let card = Rect::new(
                inner.x,
                y,
                inner.width,
                CARD_H.min(inner.bottom().saturating_sub(y)),
            );
            let selected = focused && row == state.sel_card;
            let lines: Vec<Line> = match snapshot.issues.get(identifier) {
                Some(issue) => card_lines(issue, inner.width as usize)
                    .into_iter()
                    .map(Line::from)
                    .collect(),
                None => vec![Line::from(truncate(
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
        }
    }
}

fn detail_lines(state: &LinearState, issue: &LinearIssue, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::new();
    let meta = format!(
        "state: {} · priority: {} · assignee: {}{}",
        issue
            .state
            .name
            .as_deref()
            .map(line)
            .unwrap_or_else(|| "?".into()),
        issue
            .priority
            .map(|p| format!("P{p}"))
            .unwrap_or_else(|| "?".into()),
        issue
            .assignee
            .as_ref()
            .and_then(|a| a.name.as_deref())
            .map(line)
            .unwrap_or_else(|| "unassigned".into()),
        if issue.stale {
            " · ~stale (cached)"
        } else {
            ""
        }
    );
    out.push(Line::from(truncate(&meta, width)));
    if !issue.labels.is_empty() {
        out.push(Line::from(truncate(
            &format!(
                "labels: {}",
                issue
                    .labels
                    .iter()
                    .map(|l| line(l))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            width,
        )));
    }
    out.push(Line::from(truncate(
        &format!(
            "url: {}",
            issue
                .url
                .as_deref()
                .map(line)
                .unwrap_or_else(|| "(none; cached)".into())
        ),
        width,
    )));
    out.push(Line::from(""));
    if issue.bindings.is_empty() {
        out.push(Line::from(Span::styled(
            "no worktree binding recorded for this card",
            Style::default().fg(Color::DarkGray),
        )));
        return out;
    }
    let rows = state.pane_rows(issue);
    let mut row_index = 0usize;
    for (b, binding) in issue.bindings.iter().enumerate() {
        out.push(Line::from(Span::styled(
            truncate(
                &format!(
                    "[{}] {}",
                    line(&binding.state),
                    line(&binding.worktree_path)
                ),
                width,
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        match &binding.tab {
            Some(tab) => out.push(Line::from(truncate(
                &format!(
                    "  tab: {} ({})",
                    line(tab.label.as_deref().unwrap_or("(label unknown)")),
                    line(&tab.id)
                ),
                width,
            ))),
            None => out.push(Line::from("  no recorded tab, so no panes")),
        }
        for row in rows.iter().filter(|row| row.binding == b) {
            let cursor = if row_index == state.pane_cursor {
                "▶"
            } else {
                " "
            };
            let text = truncate(
                &format!("  {cursor} {}  {}", line(&row.pane_id), row.status),
                width,
            );
            let style = if row_index == state.pane_cursor {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            out.push(Line::from(Span::styled(text, style)));
            row_index += 1;
        }
        if binding.tab.is_some() && binding.panes.is_empty() {
            out.push(Line::from("    (no panes listed)"));
        }
    }
    out
}

fn draw_detail(state: &LinearState, f: &mut Frame, area: Rect) {
    let Some(issue) = state.detail_issue() else {
        return;
    };
    let rect = centered_rect_abs(
        area.width.saturating_sub(4).max(20),
        area.height.saturating_sub(2).max(8),
        area,
    );
    f.render_widget(Clear, rect);
    let title = truncate(
        &format!(" {} — {} ", line(&issue.identifier), line(&issue.title)),
        rect.width.saturating_sub(2) as usize,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::LightBlue));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    if inner.height == 0 {
        return;
    }
    let body = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(1),
    );
    f.render_widget(
        Paragraph::new(detail_lines(state, issue, inner.width as usize)),
        body,
    );
    let hint = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate(
                " j/k pane · o focus pane · u open in Linear · y copy path · Esc back",
                inner.width as usize,
            ),
            Style::default().fg(Color::DarkGray),
        )),
        hint,
    );
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

fn draw_not_bound(state: &LinearState, f: &mut Frame, area: Rect) {
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
        Line::from("Run /work:bind in this space, then press r to refresh."),
    ];
    for warning in state.source_warnings() {
        lines.push(Line::from(Span::styled(
            format!("! {warning}"),
            Style::default().fg(Color::LightYellow),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "r refresh · q quit",
        Style::default().fg(Color::DarkGray),
    )));
    boxed(f, area, "Not bound", Color::LightYellow, lines);
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

fn draw_help(f: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let mut section: Option<Screen> = None;
    for (screen, key, description) in linear_help_keys() {
        if *key == "--" {
            continue;
        }
        if section != Some(*screen) {
            section = Some(*screen);
            lines.push(Line::from(Span::styled(
                match screen {
                    Screen::LinearBoard => "board",
                    Screen::LinearDetail => "card detail",
                    Screen::LinearNotBound => "not bound",
                    Screen::LinearError => "snapshot failed",
                    Screen::LinearStaleDaemon => "daemon older than board",
                    _ => "",
                },
                Style::default().add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::from(format!("  {key:<12} {description}")));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "any key closes",
        Style::default().fg(Color::DarkGray),
    )));
    boxed(f, area, "Help — Linear mode", Color::LightBlue, lines);
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
            truncate(
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
