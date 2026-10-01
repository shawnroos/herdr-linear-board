//! The session side pane: the bound issue on the board's own issue page, the
//! lane list, the bind hint, and the session-only help. Drawn for
//! `Mode::Session` only, at whatever width the split gives it.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::linear::{fit, line};
use super::{centered_rect_abs, session_help_keys};
use crate::app::{sanitise, session_lane, App, LinearState, Screen, SessionState, SessionView};

const LOADING: &str = " waiting for the first snapshot…";
const HINT: &str = "not bound — /work:bind";
const HELP_KEY_W: usize = 10;

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub(super) fn draw(app: &App, f: &mut Frame) {
    app.hit_map.borrow_mut().clear();
    let area = f.area();
    let (Some(state), Some(session)) = (app.linear.as_ref(), app.session.as_ref()) else {
        return;
    };
    let status = status_line(app, session);
    let body = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(u16::from(status.is_some())),
    );
    match (session.bound_issue(), session.view) {
        // The page keeps its own row for a toast.
        (Some(_), SessionView::Issue) => {
            super::linear_issue::draw(app, state, f, if app.toast.is_some() { area } else { body })
        }
        (Some(issue), SessionView::List) => draw_list(state, session, issue, f, body),
        (None, _) => draw_unbound(state, session, f, body),
    }
    if let Some(status) = status {
        f.render_widget(
            Paragraph::new(status),
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
        );
    }
    if app.screen == Screen::Help {
        draw_help(app, f, area);
    }
}

/// The bottom row: a toast, else why the read on screen is an older one.
fn status_line(app: &App, session: &SessionState) -> Option<Line<'static>> {
    if let Some(toast) = &app.toast {
        let style = if toast.is_error {
            Style::default().fg(Color::White).bg(Color::Red)
        } else {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        };
        return Some(Line::from(Span::styled(
            fit(
                &format!(" {} ", line(&sanitise(&toast.text))),
                app.last_area.width as usize,
            ),
            style,
        )));
    }
    let failed = session.failed.as_ref().filter(|_| session.read.is_some())?;
    Some(Line::from(Span::styled(
        format!(" last read kept: {}", line(failed)),
        Style::default().fg(Color::Yellow),
    )))
}

/// The daemon's not-imported message, as the board's not-bound body prints it.
fn not_imported(state: &LinearState) -> Option<Vec<Line<'static>>> {
    let snapshot = state
        .snapshot()
        .filter(|s| s.linear.status == "not_imported")?;
    let message = snapshot
        .linear
        .message
        .as_deref()
        .unwrap_or("the work store has not been imported; run `board import work-store`");
    Some(
        message
            .lines()
            .map(|text| {
                Line::from(Span::styled(
                    text.to_string(),
                    Style::default().fg(Color::LightYellow),
                ))
            })
            .collect(),
    )
}

fn paragraph(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_unbound(state: &LinearState, session: &SessionState, f: &mut Frame, area: Rect) {
    let mut lines = vec![];
    if let Some(message) = not_imported(state) {
        lines.extend(message);
        lines.push(Line::from(""));
    }
    let Some(read) = session.read.as_ref() else {
        if lines.is_empty() {
            lines.push(Line::from(LOADING));
        }
        paragraph(f, area, lines);
        return;
    };
    lines.push(Line::from(Span::styled(
        format!(" {HINT}"),
        Style::default()
            .fg(Color::LightYellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        if read.space_bound {
            " this agent's worktree is bound to no issue"
        } else {
            " this space is not bound to a Linear project"
        },
        dim(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(" ? help · q close", dim())));
    paragraph(f, area, lines);
}

fn draw_list(state: &LinearState, session: &SessionState, issue: &str, f: &mut Frame, area: Rect) {
    let width = area.width as usize;
    let mut lines = vec![];
    if let Some(message) = not_imported(state) {
        lines.extend(message);
    }
    let lane = session_lane(state, issue);
    match &lane {
        None if state.snapshot().is_none() => lines.push(Line::from(LOADING)),
        None => lines.push(Line::from(Span::styled(
            fit(&format!(" {issue} is not on this board's view"), width),
            dim(),
        ))),
        Some(lane) => {
            let scope = match &lane.lane {
                Some(label) => format!("lane: {}", line(label)),
                None => "whole tab".to_string(),
            };
            let tab = if lane.tab.is_empty() {
                String::new()
            } else {
                format!(" · {}", line(&lane.tab))
            };
            lines.push(Line::from(Span::styled(
                fit(&format!(" {scope}{tab}"), width),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            let mut at = 0;
            for (label, issues) in &lane.columns {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    fit(&format!(" {} ({})", line(label), issues.len()), width),
                    Style::default().fg(Color::LightBlue),
                )));
                for id in issues {
                    let title = state.issue(id).map(|i| line(&i.title)).unwrap_or_default();
                    let text = fit(&format!(" {}{} {title}", state.gutter(id), line(id)), width);
                    let style = if at == session.list_sel {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else if id == issue {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    lines.push(Line::from(Span::styled(text, style)));
                    at += 1;
                }
            }
        }
    }
    let body_h = area.height.saturating_sub(1) as usize;
    let selected_line = lines
        .iter()
        .position(|l| {
            l.spans
                .first()
                .is_some_and(|s| s.style.add_modifier.contains(Modifier::REVERSED))
        })
        .unwrap_or(0);
    let scroll = (selected_line + 2).saturating_sub(body_h);
    f.render_widget(
        Paragraph::new(lines).scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        Rect::new(area.x, area.y, area.width, body_h as u16),
    );
    if area.height > 0 {
        f.render_widget(
            Paragraph::new(Span::styled(
                fit(" Tab issue · j/k move · ? help · q close", width),
                dim(),
            )),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }
}

fn help_lines(width: usize) -> Vec<Line<'static>> {
    session_help_keys()
        .iter()
        .filter(|(_, key, _)| *key != "--")
        .map(|(_, key, description)| {
            let key = fit(key, HELP_KEY_W);
            let pad = " ".repeat(HELP_KEY_W.saturating_sub(key.width()));
            Line::from(fit(&format!(" {key}{pad} {description}"), width))
        })
        .collect()
}

fn help_sheet(area: Rect, rows: usize) -> Rect {
    let wanted = u16::try_from(rows).unwrap_or(u16::MAX).saturating_add(3);
    centered_rect_abs(
        area.width.saturating_sub(2).clamp(10, 50),
        wanted.min(area.height),
        area,
    )
}

pub fn session_help_max_scroll(_app: &App, area: Rect) -> usize {
    let sheet = help_sheet(area, help_lines(area.width as usize).len());
    let rows = help_lines(sheet.width.saturating_sub(2) as usize).len();
    rows.saturating_sub(sheet.height.saturating_sub(3) as usize)
}

fn draw_help(app: &App, f: &mut Frame, area: Rect) {
    let sheet = help_sheet(area, help_lines(area.width as usize).len());
    let inner = Block::default().borders(Borders::ALL).inner(sheet);
    let lines = help_lines(inner.width as usize);
    let body_h = inner.height.saturating_sub(1);
    f.render_widget(Clear, sheet);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(" Help — session pane ")
            .border_style(Style::default().fg(Color::LightBlue)),
        sheet,
    );
    let scroll = app
        .help_scroll
        .min(lines.len().saturating_sub(body_h as usize));
    f.render_widget(
        Paragraph::new(lines).scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        Rect::new(inner.x, inner.y, inner.width, body_h),
    );
    if inner.height > 0 {
        f.render_widget(
            Paragraph::new(Span::styled(
                fit("any other key closes", inner.width as usize),
                dim(),
            )),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }
}
