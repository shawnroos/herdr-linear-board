use board_core::protocol::{LinearListKind, LinearListStatus};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::{
    collapse_line, sanitise, App, LinearState, ListOutcome, PickerPurpose, PickerRow,
};
use crate::widgets::{windowed_rows, Zone};

use super::centered_rect_abs;
use super::linear::fit as linear_fit;
use super::overlays::{render_overlay_frame, row_label};

fn list_noun(kind: LinearListKind) -> &'static str {
    match kind {
        LinearListKind::Spaces => "spaces",
        LinearListKind::Projects => "projects",
        LinearListKind::Views => "views",
    }
}

/// A Linear picker: the filter line, what the read said, then one row per
/// visible item with its discriminator drawn before the name.
pub(super) fn draw_linear_picker(app: &App, state: &LinearState, f: &mut Frame, area: Rect) {
    let Some(picker) = &app.picker else { return };
    let PickerPurpose::LinearList(kind) = picker.purpose else {
        return;
    };
    let dim = Style::default().fg(Color::DarkGray);
    let noun = list_noun(kind);
    let visible = picker.visible_rows();
    let clean = |s: &str| collapse_line(&sanitise(s));

    let mut notes: Vec<(String, Style)> = Vec::new();
    if let (LinearListKind::Projects, Some(space)) = (kind, &state.bind_space) {
        // The id first, as on every row, so a narrow box cuts the label.
        notes.push((
            format!("binds space {} · {}", clean(&space.id), clean(&space.label)),
            Style::default().fg(Color::LightCyan),
        ));
    }
    if let (LinearListKind::Views, Some(project)) = (kind, &picker.list_id) {
        notes.push((
            format!(
                "binds space {} · project {}",
                clean(&state.workspace_id),
                clean(project)
            ),
            Style::default().fg(Color::LightCyan),
        ));
    }
    if state.handoff_in_flight {
        notes.push((
            "starting the bind in a new tab…".to_string(),
            Style::default().fg(Color::Yellow),
        ));
    }
    let in_flight = state.list_in_flight(kind, picker.list_id.as_deref());
    if in_flight {
        notes.push((format!("loading {noun}…"), dim));
    }
    let with_message = |label: &str, message: &Option<String>| match message {
        Some(text) if !text.is_empty() => format!("{label}: {}", clean(text)),
        _ => label.to_string(),
    };
    let warn = Style::default().fg(Color::LightYellow);
    let error = Style::default().fg(Color::LightRed);
    match &picker.outcome {
        Some(ListOutcome::Failed(text)) => {
            notes.push((format!("read failed: {}", clean(text)), error));
        }
        Some(ListOutcome::Read { status, message }) => match status {
            LinearListStatus::Ok => {}
            LinearListStatus::Partial => notes.push((with_message("partial list", message), warn)),
            LinearListStatus::Unavailable => {
                notes.push((with_message("unavailable", message), error))
            }
            LinearListStatus::Unknown => {
                notes.push((with_message("status unknown", message), warn))
            }
        },
        None if !in_flight => notes.push(("not read yet".to_string(), dim)),
        None => {}
    }
    let read_ok = matches!(
        picker.outcome,
        Some(ListOutcome::Read {
            status: LinearListStatus::Ok | LinearListStatus::Partial,
            ..
        })
    );
    if picker.rows.is_empty() && read_ok && !in_flight {
        notes.push((format!("no {noun}"), dim));
    }
    if !picker.rows.is_empty() && visible.is_empty() {
        notes.push((format!("no match for “{}”", clean(&picker.filter)), dim));
    }

    let tag_w = visible
        .iter()
        .map(|(_, row)| match row {
            PickerRow::Linear(row) => clean(&row.tag).width(),
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    let widest = visible
        .iter()
        .map(|(_, row)| match row {
            PickerRow::Linear(row) => tag_w + 2 + clean(&row.name).width(),
            other => row_label(other).width(),
        })
        .chain(notes.iter().map(|(text, _)| text.width()))
        .max()
        .unwrap_or(0);
    let box_w = (widest as u16)
        .saturating_add(4)
        .clamp(44, 80)
        .min(area.width);
    let body_rows = 1 + notes.len() + visible.len();
    let box_h = (body_rows as u16)
        .saturating_add(2)
        .min(area.height.saturating_sub(2).max(3));
    let box_area = centered_rect_abs(box_w, box_h, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let title = clean(&picker.title);
    let inner = render_overlay_frame(
        f,
        box_area,
        false,
        (&title, &title),
        Style::default().fg(Color::LightBlue),
        "X",
        &mut hit_map,
    );
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let width = inner.width as usize;
    let mut y = inner.y;
    let put = |f: &mut Frame, line: Line<'static>, y: &mut u16| {
        if *y < inner.bottom() {
            f.render_widget(Paragraph::new(line), Rect::new(inner.x, *y, inner.width, 1));
            *y += 1;
        }
    };
    let filter_line = if picker.filter.is_empty() {
        Line::from(vec![
            Span::styled("filter: ", dim),
            Span::styled("type to narrow", dim),
        ])
    } else {
        Line::from(vec![
            Span::styled("filter: ", dim),
            Span::styled(
                linear_fit(&clean(&picker.filter), width.saturating_sub(8)),
                Style::default().fg(Color::White),
            ),
        ])
    };
    put(f, filter_line, &mut y);
    for (text, style) in &notes {
        put(
            f,
            Line::from(Span::styled(linear_fit(text, width), *style)),
            &mut y,
        );
    }

    let rows_h = inner.bottom().saturating_sub(y);
    let heights = vec![1u16; visible.len()];
    let (start, end) = windowed_rows(&heights, picker.sel, rows_h);
    // The discriminator keeps its cells; only the name gives way.
    let tag_w = tag_w.min(width.saturating_sub(1) / 2);
    for (at, (_, row)) in visible.iter().enumerate().take(end).skip(start) {
        let selected = at == picker.sel;
        let (tag, name) = match row {
            PickerRow::Linear(row) => (clean(&row.tag), clean(&row.name)),
            other => (String::new(), clean(row_label(other))),
        };
        let tag = linear_fit(&tag, tag_w);
        let pad = " ".repeat(tag_w.saturating_sub(tag.width()) + 2);
        let name_w = width.saturating_sub(1 + tag_w + 2);
        let name_style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(Color::White)
        };
        let rect = Rect::new(inner.x, y, inner.width, 1);
        put(
            f,
            Line::from(vec![
                Span::styled(
                    if selected { "›" } else { " " },
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(tag, Style::default().fg(Color::LightCyan)),
                Span::raw(pad),
                Span::styled(linear_fit(&name, name_w), name_style),
            ]),
            &mut y,
        );
        hit_map.push(rect, Zone::PickerRow(at));
    }
}
