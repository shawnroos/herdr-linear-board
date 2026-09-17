//! The strip under the Linear board and the not-bound screen: the spaces
//! with no project, or the live tabs no binding claims.

use board_core::protocol::{LinearListStatus, LinearSnapshot};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, LinearState, SpaceList, StripView};
use crate::widgets::Zone;

use super::linear::{fit, line};

/// The strip's rows from `y` down, each space row registered for a click.
pub(super) fn draw_strip(
    app: &App,
    strip: Vec<(Line<'static>, Option<String>)>,
    f: &mut Frame,
    area: Rect,
    y: u16,
) {
    // The strip is clamped to the frame: at a tiny height the body saturates
    // to zero rows and an unclamped rect would index past the buffer.
    let shown = (strip.len() as u16).min(area.bottom().saturating_sub(y));
    let mut hit_map = app.hit_map.borrow_mut();
    for (at, (line, space_id)) in strip.into_iter().take(shown as usize).enumerate() {
        let rect = Rect::new(area.x, y + at as u16, area.width, 1);
        f.render_widget(Paragraph::new(line), rect);
        if let Some(space_id) = space_id {
            hit_map.push(rect, Zone::LinearStripRow(space_id));
        }
    }
}

const STRIP_ROWS: usize = 3;

/// The strip's rows, each with the space id a click on it opens. Empty when
/// the tabs view has no unmapped tabs, as before the spaces view existed.
pub(super) fn strip_lines(
    state: &LinearState,
    snapshot: &LinearSnapshot,
    width: usize,
) -> Vec<(Line<'static>, Option<String>)> {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    if state.strip == StripView::Tabs {
        if snapshot.unmapped.is_empty() {
            return vec![];
        }
        let mut lines = vec![(
            Line::from(Span::styled(
                " Unmapped tabs (live tabs no binding claims)",
                bold,
            )),
            None,
        )];
        for tab in snapshot.unmapped.iter().take(STRIP_ROWS) {
            let panes: Vec<String> = tab
                .panes
                .iter()
                .map(|p| format!("{} {}", line(p), state.pane_status(p)))
                .collect();
            lines.push((
                Line::from(fit(
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
                    width,
                )),
                None,
            ));
        }
        return lines;
    }
    let dim = Style::default().fg(Color::DarkGray);
    let warn = Style::default().fg(Color::LightYellow);
    let error = Style::default().fg(Color::LightRed);
    let note =
        |text: String, style: Style| (Line::from(Span::styled(fit(&text, width), style)), None);
    let with_message = |label: &str, message: &Option<String>| match message {
        Some(text) if !text.is_empty() => format!("  {label}: {}", line(text)),
        _ => format!("  {label}"),
    };
    let mut header = " Spaces with no project".to_string();
    if let SpaceList::Read(list) = &state.spaces {
        if list.status == LinearListStatus::Partial {
            header.push_str(" (partial list)");
        }
    }
    header.push_str(" · s select · t unmapped tabs");
    let mut lines = vec![(Line::from(Span::styled(fit(&header, width), bold)), None)];
    let rows = state.unbound_spaces();
    // Off a bound space the rows always hold that space, so they draw beside
    // any note about the list.
    let mut show_rows = !state.bound();
    match &state.spaces {
        SpaceList::NotRead => lines.push(note("  loading spaces…".to_string(), dim)),
        SpaceList::Failed(text) => lines.push(note(
            format!("  space list unavailable: {}", line(text)),
            error,
        )),
        SpaceList::Read(list) if list.status == LinearListStatus::Unavailable => lines.push(note(
            with_message("space list unavailable", &list.message),
            error,
        )),
        SpaceList::Read(list) if list.status == LinearListStatus::Unknown && rows.is_empty() => {
            lines.push(note(
                with_message("space list status unknown", &list.message),
                warn,
            ))
        }
        SpaceList::Read(_) if rows.is_empty() => {
            lines.push(note("  every space is bound".to_string(), dim))
        }
        SpaceList::Read(_) => show_rows = true,
    }
    if show_rows && !rows.is_empty() {
        let heights = vec![1u16; rows.len()];
        let sel = state.strip_sel.min(rows.len() - 1);
        let (start, end) = crate::widgets::windowed_rows(&heights, sel, STRIP_ROWS as u16);
        for (at, row) in rows.iter().enumerate().take(end).skip(start) {
            let selected = state.strip_focus && at == sel;
            let text = fit(
                &format!(
                    "  {} {} ({})",
                    if selected { "›" } else { " " },
                    line(if row.label.is_empty() {
                        "(no label)"
                    } else {
                        &row.label
                    }),
                    line(&row.id)
                ),
                width,
            );
            let style = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push((Line::from(Span::styled(text, style)), Some(row.id.clone())));
        }
    }
    lines
}
