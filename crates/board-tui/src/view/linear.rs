//! Linear-mode rendering: the space board built from the work plugin's
//! snapshot, the card detail, and the not-bound / error / stale-daemon
//! screens. Only `Mode::Linear` reaches this module, and it reads
//! `App::linear` only, never `App::board`.

use board_core::protocol::{LinearCardPr, LinearIssue, LinearSnapshot};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{sanitise, App, CardOverlay, CardOverlays, Focus, LinearState, Screen};
use crate::theme::Theme;
use crate::widgets::{HitMap, Zone};

use super::linear_strip::{draw_strip, strip_lines};
use super::{centered_rect_abs, linear_help_keys};

// Fixed so lanes line up across columns; supersedes the 5-row card of the
// 2026-09-16 pickers-and-layout plan.
const CARD_H: u16 = 13;
const CARD_INSET: u16 = 2;
// Each column's half of the 4-cell gap between columns.
const SLOT_PAD: u16 = 2;
const MIN_COL_W: u16 = 36;
const HEADER_ROWS: u16 = 2;
// The column title bar and the blank row under it.
const TITLE_ROWS: u16 = 2;

mod icon {
    pub const PERSON: &str = "\u{f415}";
    pub const MILESTONE: &str = "\u{f45d}";
    pub const PULL_REQUEST: &str = "\u{f407}";
    pub const EYE: &str = "\u{f441}";
    pub const DIFF: &str = "\u{f4d2}";
    pub const CHECK: &str = "\u{f058}";
    pub const FILTER: &str = "\u{f0232}";
    pub const FILTER_OUTLINE: &str = "\u{f0233}";
    pub const SLANT: &str = "\u{e0bc}";
}
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
    f.render_widget(Block::default().style(state.theme.app()), area);
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
    scrub(f);
}

/// The view boundary for outside text (KTD15). Every Linear and agent string
/// is sanitised on arrival, but ratatui writes ESC and BEL into cells as-is, so
/// a string that missed that pass would reach the terminal as a sequence. A
/// cell holding a control or format character is blanked instead.
pub(super) fn scrub(f: &mut Frame) {
    for cell in &mut f.buffer_mut().content {
        let symbol = cell.symbol();
        if symbol
            .chars()
            .any(|c| c.is_control() || board_core::text::is_format_char(c))
        {
            let clean = board_core::text::strip_control_and_format(symbol);
            cell.set_symbol(if clean.is_empty() { " " } else { &clean });
        }
    }
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
    let theme = &state.theme;
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
        Span::styled(badge, theme.column_title_active()),
        Span::raw(lead),
        Span::raw(view),
        Span::raw(format!(" · space: {space} · fetched {}", age(app, state))),
    ];
    if state.in_flight {
        first.push(Span::styled(" · refreshing…", theme.accent()));
    }
    let mut warnings = state.source_warnings();
    if let Some(mapping) = state.non_default_mapping() {
        warnings.push(format!(
            "mapping {mapping} is not the default; rendering as default"
        ));
    }
    let second = if warnings.is_empty() {
        vec![Span::styled(
            " Enter detail · r refresh · ? help · ctrl+q quit",
            theme.dim(),
        )]
    } else {
        vec![Span::styled(
            format!(" ! {}", warnings.join(" · ")),
            theme.warning(),
        )]
    };
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

/// `n` rows of `text` by display width: every row but the last breaks at a
/// word boundary (mid-word only when one word is wider than the row), and the
/// last takes the rest, ending in an ellipsis when it does not fit.
fn wrap_lines(text: &str, width: usize, n: usize) -> Vec<String> {
    let mut rest = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = Vec::with_capacity(n);
    for at in 0..n {
        if at + 1 == n {
            out.push(fit(&rest, width));
            break;
        }
        if rest.width() <= width {
            out.push(std::mem::take(&mut rest));
            continue;
        }
        let mut end = 0;
        for (i, _) in rest.match_indices(' ') {
            if rest[..i].width() > width {
                break;
            }
            end = i;
        }
        let (head, tail) = if end == 0 {
            split_at_width(&rest, width)
        } else {
            rest.split_at(end)
        };
        out.push(head.to_string());
        rest = tail.trim_start().to_string();
    }
    out.resize(n, String::new());
    out
}

fn title_lines(title: &str, width: usize) -> Vec<String> {
    if title.trim().is_empty() {
        return vec!["(no title)".to_string(), String::new()];
    }
    wrap_lines(title, width, 2)
}

/// `spans` cut to `width` display cells, the last kept span ending in `…`
/// when anything was cut.
fn clip(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut used = 0;
    let mut out = vec![];
    for span in spans {
        let w = span.content.width();
        if used + w <= width {
            used += w;
            out.push(span);
            continue;
        }
        let room = width - used;
        if room > 0 {
            out.push(Span::styled(fit(&span.content, room), span.style));
        }
        break;
    }
    out
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

#[derive(Debug, Clone, Copy)]
struct CardLook {
    selected: bool,
    muted: bool,
}

/// What `gh` adds to a card's PR row: nothing yet (Linear's number and status
/// with dim placeholders), still loading, or unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrGh {
    LinearOnly,
    Loading,
    GhUnavailable,
}

fn pr_gh(_pr: &LinearCardPr) -> PrGh {
    PrGh::LinearOnly
}

/// The card's PR row: icon, number and Linear's status, then what `gh`
/// knows, in the state `gh` says.
pub fn card_pr_line(theme: &Theme, pr: &LinearCardPr, gh: PrGh, muted: bool) -> Line<'static> {
    let icon_style = |c| Style::default().fg(if muted { theme.muted_detail } else { c });
    let label = if muted {
        theme.card_detail_muted()
    } else {
        theme.card_detail()
    };
    let dim = if muted {
        theme.card_detail_muted()
    } else {
        theme.dim()
    };
    let mut spans = vec![
        Span::styled(icon::PULL_REQUEST, icon_style(theme.palette.green)),
        Span::styled(format!(" #{}", pr.number), label),
    ];
    let status = if pr.draft == Some(true) {
        Some("draft".to_string())
    } else {
        pr.status.as_deref().map(line)
    };
    if let Some(status) = status.filter(|s| !s.trim().is_empty()) {
        spans.push(Span::styled(format!(" {status}"), label));
    }
    match gh {
        PrGh::LinearOnly => {
            for glyph in [icon::EYE, icon::DIFF, icon::CHECK] {
                spans.push(Span::styled(format!("  {glyph} –"), dim));
            }
        }
        PrGh::Loading => spans.push(Span::styled("  …", dim)),
        PrGh::GhUnavailable => spans.push(Span::styled("  gh unavailable", dim)),
    }
    Line::from(spans)
}

fn card_header(
    state: &LinearState,
    overlay: CardOverlay<'_>,
    issue: &LinearIssue,
    inner: usize,
    look: CardLook,
) -> Vec<Span<'static>> {
    let t = &state.theme;
    let tone = |style: Style| {
        if look.muted {
            t.card_detail_muted()
        } else {
            style
        }
    };
    let id_style = if look.muted {
        t.card_title_muted()
    } else {
        t.card_title()
    };
    let mut left = vec![Span::styled(line(&issue.identifier), id_style)];
    if let Some(p) = issue.priority {
        let style = if p <= 1 { t.warning() } else { t.dim() };
        left.push(Span::styled(format!(" P{p}"), tone(style)));
    }
    let panes = LinearState::pane_count(issue);
    if panes > 0 {
        left.push(Span::styled(
            format!(" ⧉{panes}"),
            tone(Style::default().fg(t.palette.teal)),
        ));
    }
    if issue.stale {
        left.push(Span::styled(" ~stale", tone(t.warning())));
    }
    let mut right: Vec<Span> = overlay
        .marks()
        .into_iter()
        .map(|(glyph, kind)| {
            let style = kind.map_or_else(|| t.show_mark(), |kind| t.mark(kind));
            Span::styled(glyph.to_string(), style.add_modifier(Modifier::BOLD))
        })
        .collect();
    let running = issue
        .bindings
        .iter()
        .flat_map(|b| &b.panes)
        .any(|pane| state.pane_status(pane) == "working");
    if running {
        let lead = if right.is_empty() { "▶" } else { " ▶" };
        right.push(Span::styled(lead, tone(t.success())));
    }
    let right_w = spans_width(&right);
    let mut out = clip(left, inner.saturating_sub(right_w + 1).max(1));
    let gap = inner.saturating_sub(spans_width(&out) + right_w);
    out.push(Span::raw(" ".repeat(gap)));
    out.extend(right);
    clip(out, inner)
}

fn card_meta(state: &LinearState, issue: &LinearIssue, look: CardLook) -> [Vec<Span<'static>>; 2] {
    let t = &state.theme;
    let icon_style = |c| Style::default().fg(if look.muted { t.muted_detail } else { c });
    let label = if look.muted {
        t.card_detail_muted()
    } else {
        t.card_detail()
    };
    let who = issue
        .assignee
        .as_ref()
        .and_then(|a| a.name.as_deref())
        .map(|n| format!(" @{}", line(n)))
        .unwrap_or_else(|| " unassigned".to_string());
    let mut first = vec![
        Span::styled(icon::PERSON, icon_style(t.palette.blue)),
        Span::styled(who, label),
    ];
    if let Some(name) = issue
        .milestone
        .as_ref()
        .and_then(|m| m.name.as_deref())
        .filter(|n| !n.trim().is_empty())
    {
        first.push(Span::raw("  "));
        first.push(Span::styled(icon::MILESTONE, icon_style(t.palette.mauve)));
        first.push(Span::styled(format!(" {}", line(name)), label));
    }
    let second = issue
        .pr
        .as_ref()
        .map(|pr| card_pr_line(t, pr, pr_gh(pr), look.muted).spans)
        .unwrap_or_default();
    [first, second]
}

/// A card's 13 rows, each starting with the 2-cell inset. Selection changes
/// only the title colour and the two reserved rule rows, so no text moves.
fn card_lines(
    state: &LinearState,
    overlay: CardOverlay<'_>,
    issue: &LinearIssue,
    width: u16,
    look: CardLook,
) -> Vec<Line<'static>> {
    let t = &state.theme;
    let inner = width.saturating_sub(2 * CARD_INSET) as usize;
    let inset = |spans: Vec<Span<'static>>| {
        let mut spans = clip(spans, inner);
        spans.insert(0, Span::raw(" ".repeat(CARD_INSET as usize)));
        Line::from(spans)
    };
    let rule = |text: String| {
        if look.selected {
            inset(vec![Span::styled(text, t.rule())])
        } else {
            Line::default()
        }
    };
    let title_style = if look.selected {
        t.card_title_active()
    } else if look.muted {
        t.card_title_muted()
    } else {
        t.card_title()
    };
    let detail = if look.muted {
        t.card_detail_muted()
    } else {
        t.card_detail()
    };
    let mut lines = vec![
        Line::default(),
        inset(card_header(state, overlay, issue, inner, look)),
        rule("─".repeat(inner)),
    ];
    for text in title_lines(&line(&issue.title), inner) {
        lines.push(inset(vec![Span::styled(text, title_style)]));
    }
    lines.push(Line::default());
    let mut description = wrap_lines(&line(issue.description.as_deref().unwrap_or("")), inner, 3);
    if let Some(note) = overlay.latest_note {
        description[2] = fit(&format!("› {}", line(&note.body)), inner);
    }
    for text in description {
        lines.push(inset(vec![Span::styled(text, detail)]));
    }
    lines.push(rule("─ ".repeat(inner / 2)));
    for row in card_meta(state, issue, look) {
        lines.push(inset(row));
    }
    lines.push(Line::default());
    lines
}

/// How many columns one page holds at `width` cells: at least one, so a
/// sidebar narrower than a column still shows a column.
pub fn linear_columns_per_page(width: u16) -> usize {
    usize::from(width / MIN_COL_W).max(1)
}

fn draw_too_short(state: &LinearState, f: &mut Frame, area: Rect) {
    let text = fit("pane too short", area.width as usize);
    let x = area.x + area.width.saturating_sub(text.width() as u16) / 2;
    let y = area.y + area.height.saturating_sub(1) / 2;
    f.render_widget(
        Paragraph::new(Span::styled(text, state.theme.dim())),
        Rect::new(x, y, area.width.saturating_sub(x - area.x), 1),
    );
}

fn draw_board(app: &App, state: &LinearState, f: &mut Frame, area: Rect) {
    if area.height < HEADER_ROWS + CARD_H + 1 {
        draw_too_short(state, f, area);
        return;
    }
    let (header, (view_x, view_w)) = header_lines(app, state);
    let header_area = Rect::new(area.x, area.y, area.width, HEADER_ROWS);
    f.render_widget(Paragraph::new(header), header_area);
    let view_w = view_w.min(area.width.saturating_sub(view_x));
    if view_w > 0 {
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
    let pinned = pinned_line(state, area.width as usize);
    let pinned_h = u16::from(pinned.is_some());
    let footer_h: u16 = 1;
    let mut y = area.y + HEADER_ROWS;
    let mut body_h = area
        .height
        .saturating_sub(HEADER_ROWS + pinned_h + strip_h + footer_h);
    let per_page = linear_columns_per_page(area.width);
    let overlays = state.overlays();
    let rows = [
        tab_row(state, &overlays, area.width as usize),
        (state.groups().len() > per_page)
            .then(|| pager_row(state, &overlays, per_page, area.width as usize)),
        filter_row(state, area.width as usize),
    ];
    for row in rows.into_iter().flatten() {
        if body_h == 0 {
            break;
        }
        f.render_widget(Paragraph::new(row), Rect::new(area.x, y, area.width, 1));
        y += 1;
        body_h -= 1;
    }
    let body = Rect::new(area.x, y, area.width, body_h);
    draw_columns(
        state,
        &overlays,
        snapshot,
        f,
        body,
        per_page,
        &mut app.hit_map.borrow_mut(),
    );
    if let Some(pinned) = pinned {
        if body.bottom() < area.bottom() {
            f.render_widget(
                Paragraph::new(pinned),
                Rect::new(area.x, body.bottom(), area.width, 1),
            );
        }
    }
    draw_strip(app, strip, f, area, body.bottom() + pinned_h);
}

/// One powerline bar: each segment's text on its colour, a `/` cut where the
/// colour changes and at the right end, and a plain cell between two
/// segments of the same colour.
fn powerline(theme: &Theme, segments: Vec<(String, bool)>) -> Vec<Span<'static>> {
    let bg = |active: bool| {
        if active {
            theme.palette.accent
        } else {
            theme.palette.panel_bg
        }
    };
    let mut spans = vec![];
    for (at, (text, active)) in segments.iter().enumerate() {
        let style = if *active {
            theme.column_title_active()
        } else {
            theme.column_title_inactive()
        };
        spans.push(Span::styled(text.clone(), style));
        let here = bg(*active);
        let next = segments
            .get(at + 1)
            .map_or(theme.app_bg, |(_, active)| bg(*active));
        spans.push(if next == here {
            Span::styled(" ", Style::default().bg(here))
        } else {
            Span::styled(icon::SLANT, theme.cut(here, next))
        });
    }
    spans
}

/// The view tabs as a powerline row, the active tab on the accent. `None`
/// for a document from before tabs, whose one tab has no name to show. When
/// the row is too narrow it starts at a later tab so the active one stays in
/// view.
fn tab_row(
    state: &LinearState,
    overlays: &CardOverlays<'_>,
    width: usize,
) -> Option<Line<'static>> {
    let tabs = state.tabs();
    if tabs.iter().all(|tab| tab.label.is_empty()) {
        return None;
    }
    let names: Vec<String> = tabs
        .iter()
        .enumerate()
        .map(|(at, tab)| {
            let label = line(&tab.label);
            let label = if label.is_empty() {
                "(no label)".to_string()
            } else {
                label
            };
            let count = if at == state.sel_tab {
                String::new()
            } else {
                attention_count(overlays, tab.groups.iter().flat_map(|g| g.issues.iter()))
            };
            format!("  {label}{count}  ")
        })
        .collect();
    let active = state.sel_tab.min(names.len() - 1);
    let mut first = 0;
    while first < active
        && 1 + names[first..=active]
            .iter()
            .map(|n| n.width() + 1)
            .sum::<usize>()
            > width
    {
        first += 1;
    }
    let lead = if first > 0 { "‹" } else { " " };
    let segments = names
        .into_iter()
        .enumerate()
        .skip(first)
        .map(|(at, name)| (name, at == active))
        .collect();
    let mut spans = vec![Span::styled(lead, state.theme.dim())];
    spans.extend(powerline(&state.theme, segments));
    Some(Line::from(clip(spans, width)))
}

/// One lane's band across the board: its line (when the board has lanes),
/// a blank row, then as many card rows as its busiest column holds.
struct Band<'a> {
    key: &'a str,
    label: Option<&'a str>,
    total: usize,
    folded: bool,
    top: u16,
    columns: Vec<Vec<&'a str>>,
}

impl Band<'_> {
    fn head(&self) -> u16 {
        if self.label.is_some() {
            2
        } else {
            0
        }
    }

    fn card_top(&self, at: usize) -> u16 {
        self.top + self.head() + at as u16 * (CARD_H + 1)
    }

    fn height(&self) -> u16 {
        let cards = self.columns.iter().map(Vec::len).max().unwrap_or(0);
        self.head() + cards as u16 * (CARD_H + 1)
    }
}

/// The active tab's bands, top to bottom, for every column (not just the
/// page shown), so the bands keep their height across pages.
fn bands(state: &LinearState) -> Vec<Band<'_>> {
    let groups = state.groups();
    let rows: Vec<_> = groups.iter().map(|g| state.lane_rows(g, None)).collect();
    let lanes = state.lanes();
    let keys: Vec<(&str, Option<&str>)> = if lanes.is_empty() {
        vec![("", None)]
    } else {
        lanes
            .into_iter()
            .map(|(key, label)| (key, Some(label)))
            .collect()
    };
    let mut top = 0;
    keys.into_iter()
        .map(|(key, label)| {
            let mut total = 0;
            let columns = rows
                .iter()
                .map(|column| {
                    match column
                        .iter()
                        .find(|row| row.key == key && row.label.is_some() == label.is_some())
                    {
                        Some(row) => {
                            total += row.count;
                            row.issues.clone()
                        }
                        None => vec![],
                    }
                })
                .collect();
            let band = Band {
                key,
                label,
                total,
                folded: label.is_some() && state.folded(key),
                top,
                columns,
            };
            top += band.height();
            band
        })
        .collect()
}

/// The board's offset after moving by the least that keeps `target` (top
/// row and height) in a viewport of `h` rows; an item taller than the
/// viewport shows its top.
fn scroll_to(prev: u16, target: Option<(u16, u16)>, h: u16, total: u16) -> u16 {
    let mut off = prev;
    if let Some((top, len)) = target {
        if top < off || len > h {
            off = top;
        } else if top + len > off + h {
            off = top + len - h;
        }
    }
    off.min(total.saturating_sub(h))
}

fn lane_line(
    state: &LinearState,
    band: &Band<'_>,
    label: &str,
    width: usize,
    holding: bool,
) -> Line<'static> {
    let t = &state.theme;
    let selected = state.selected_lane_line() == Some(band.key);
    let (rule, label_style) = if selected || holding {
        (t.lane(), t.lane_holding())
    } else {
        (t.rule(), t.lane())
    };
    let icon = if band.folded {
        icon::FILTER
    } else {
        icon::FILTER_OUTLINE
    };
    let right = format!(" {} {icon} ", band.total);
    let label = fit(
        &format!(" {} ", line(label).to_uppercase()),
        width.saturating_sub(2 + right.width()),
    );
    let rest = width.saturating_sub(2 + label.width() + right.width());
    let line = Line::from(clip(
        vec![
            Span::styled("─", rule),
            Span::styled(label, label_style.add_modifier(Modifier::BOLD)),
            Span::styled("─".repeat(rest), rule),
            Span::styled(right, label_style),
            Span::styled("─", rule),
        ],
        width,
    ));
    if selected {
        line.style(t.lane_selected())
    } else {
        line
    }
}

fn draw_columns(
    state: &LinearState,
    overlays: &CardOverlays<'_>,
    snapshot: &LinearSnapshot,
    f: &mut Frame,
    body: Rect,
    per_page: usize,
    hit_map: &mut HitMap,
) {
    let groups = state.groups();
    if body.height == 0 {
        return;
    }
    if groups.is_empty() {
        f.render_widget(Paragraph::new(" no columns in this snapshot"), body);
        return;
    }
    let t = &state.theme;
    let sel_group = state.sel_group.min(groups.len() - 1);
    let (start, visible) = if groups.len() > per_page {
        (sel_group / per_page * per_page, per_page)
    } else {
        (0, groups.len())
    };
    let col_w = body.width / visible as u16;
    let nothing_selected = state.focus == Focus::None;
    let bands = bands(state);
    let counts: Vec<usize> = (0..groups.len())
        .map(|g| {
            state
                .lane_rows(&groups[g], None)
                .iter()
                .map(|row| row.count)
                .sum()
        })
        .collect();
    let slots: Vec<(u16, usize)> = (start..(start + visible).min(groups.len()))
        .enumerate()
        .map(|(slot, idx)| (body.x + slot as u16 * col_w, idx))
        .collect();

    let titles = slots
        .iter()
        .map(|&(_, idx)| {
            let text = fit(
                &format!(
                    "   {}  {}",
                    line(&groups[idx].label).to_uppercase(),
                    counts[idx]
                ),
                col_w.saturating_sub(1) as usize,
            );
            let pad = (col_w.saturating_sub(1) as usize).saturating_sub(text.width());
            (
                format!("{text}{}", " ".repeat(pad)),
                !nothing_selected && idx == sel_group,
            )
        })
        .collect();
    f.render_widget(
        Paragraph::new(Line::from(powerline(t, titles))),
        Rect::new(body.x, body.y, body.width, 1),
    );
    for &(x, idx) in &slots {
        hit_map.push(
            Rect::new(x, body.y, col_w, 1),
            Zone::LinearGroup(groups[idx].key.clone()),
        );
    }
    if body.height <= TITLE_ROWS {
        return;
    }
    let view = Rect::new(
        body.x,
        body.y + TITLE_ROWS,
        body.width,
        body.height - TITLE_ROWS,
    );

    let selected_slot = state.selected_slot();
    let total: u16 = bands.iter().map(Band::height).sum();
    let target = match &state.focus {
        Focus::LaneLine(key) => bands
            .iter()
            .find(|b| b.label.is_some() && b.key == key)
            .map(|b| (b.top, 1)),
        Focus::Card(_) => selected_slot.and_then(|(lane, id)| {
            let band = bands.iter().find(|b| b.key == lane)?;
            let at = band.columns[sel_group].iter().position(|c| *c == id)?;
            Some((band.card_top(at), CARD_H))
        }),
        Focus::Filter | Focus::None => None,
    };
    let off = scroll_to(state.board_scroll.get(), target, view.height, total);
    state.board_scroll.set(off);
    let on_screen = |top: u16, len: u16| -> Option<(u16, u16, u16)> {
        let bottom = (top + len).min(off + view.height);
        let from = top.max(off);
        (from < bottom).then(|| (view.y + from - off, top.max(off) - top, bottom - from))
    };

    let inset = SLOT_PAD + CARD_INSET;
    if !state.filter.is_empty() && counts.iter().all(|&n| n == 0) {
        f.render_widget(
            Paragraph::new(Span::styled(
                fit(
                    "no matching issues",
                    view.width.saturating_sub(inset) as usize,
                ),
                t.dim(),
            )),
            Rect::new(
                view.x + inset.min(view.width),
                view.y,
                view.width.saturating_sub(inset),
                1,
            ),
        );
        return;
    }

    let holding_lane = selected_slot.map(|(lane, _)| lane);
    let line_w = body.width.saturating_sub(2 * SLOT_PAD);
    for band in &bands {
        let Some(label) = band.label else { continue };
        if let Some((y, _, _)) = on_screen(band.top, 1) {
            let holding = nothing_selected || holding_lane == Some(band.key);
            f.render_widget(
                Paragraph::new(lane_line(state, band, label, line_w as usize, holding)),
                Rect::new(body.x + SLOT_PAD, y, line_w, 1),
            );
        }
    }

    let card_w = col_w.saturating_sub(2 * SLOT_PAD);
    for &(x, idx) in &slots {
        let in_focus = idx == sel_group;
        let muted = !in_focus && !nothing_selected;
        let card_x = x + SLOT_PAD;
        if counts[idx] == 0 {
            let first = bands.first().map_or(0, |b| b.card_top(0));
            if let Some((y, _, _)) = on_screen(first, 1) {
                f.render_widget(
                    Paragraph::new(Span::styled(
                        fit("no issues", card_w.saturating_sub(CARD_INSET) as usize),
                        t.dim(),
                    )),
                    Rect::new(card_x + CARD_INSET, y, card_w.saturating_sub(CARD_INSET), 1),
                );
            }
            continue;
        }
        for band in &bands {
            for (at, &identifier) in band.columns[idx].iter().enumerate() {
                let Some((y, skip, h)) = on_screen(band.card_top(at), CARD_H) else {
                    continue;
                };
                let look = CardLook {
                    selected: in_focus && selected_slot == Some((band.key, identifier)),
                    muted,
                };
                let surface = if look.selected {
                    t.card_active()
                } else if look.muted {
                    t.card_muted()
                } else {
                    t.card()
                };
                let lines = match snapshot.issues.get(identifier) {
                    Some(issue) => card_lines(state, overlays.get(identifier), issue, card_w, look),
                    None => vec![
                        Line::default(),
                        Line::from(format!(
                            "{}{}",
                            " ".repeat(CARD_INSET as usize),
                            fit(
                                &format!("{} (missing)", line(identifier)),
                                card_w.saturating_sub(2 * CARD_INSET) as usize
                            )
                        )),
                    ],
                };
                let rect = Rect::new(card_x, y, card_w, h);
                f.render_widget(Paragraph::new(lines).style(surface).scroll((skip, 0)), rect);
                hit_map.push(
                    rect,
                    Zone::LinearCard {
                        group: groups[idx].key.clone(),
                        lane: band.key.to_string(),
                        identifier: identifier.to_string(),
                    },
                );
            }
        }
    }
}

/// While a card with a suggestion is selected, the footer names `a` and `x`.
fn suggestion_selected(state: &LinearState) -> bool {
    state.selected_suggestion().is_some()
}

/// The oldest pending show-request: who asked, the issue, its title and how
/// many more wait. Only the title and then the head give way to a narrow
/// pane; the count is never cut.
fn pinned_line(state: &LinearState, width: usize) -> Option<Line<'static>> {
    let requests = state.pending_requests();
    let oldest = requests.first()?;
    let more = match requests.len() - 1 {
        0 => String::new(),
        n => format!(" +{n}"),
    };
    let who = oldest
        .requested_by
        .as_deref()
        .map(line)
        .filter(|w| !w.trim().is_empty())
        .unwrap_or_else(|| "an agent".to_string());
    let title = state
        .issue(&oldest.issue)
        .map(|i| line(&i.title))
        .or_else(|| oldest.reason.as_deref().map(line))
        .unwrap_or_default();
    let tail = more.width();
    let room = width.saturating_sub(tail + 2);
    let head = fit(&format!("◉ {who} · {} ", line(&oldest.issue)), room);
    let title = fit(&title, room.saturating_sub(head.width()));
    let gap = width
        .saturating_sub(head.width() + title.width() + tail)
        .max(1);
    Some(Line::from(vec![
        Span::styled(head, state.theme.show_mark()),
        Span::raw(title),
        Span::raw(" ".repeat(gap)),
        Span::styled(more, state.theme.accent()),
    ]))
}

/// ` !N` for the cards among `identifiers` that need attention, or nothing.
fn attention_count<'a>(
    overlays: &CardOverlays<'_>,
    identifiers: impl Iterator<Item = &'a String>,
) -> String {
    let needing: std::collections::BTreeSet<&str> = identifiers
        .map(String::as_str)
        .filter(|id| overlays.get(id).needs_attention())
        .collect();
    match needing.len() {
        0 => String::new(),
        n => format!(" !{n}"),
    }
}

/// The board filter, shown while it is selected or holds text.
fn filter_row(state: &LinearState, width: usize) -> Option<Line<'static>> {
    let selected = state.focus == Focus::Filter;
    if !selected && state.filter.is_empty() {
        return None;
    }
    let caret = if selected { "▏" } else { "" };
    let style = if selected {
        state.theme.accent()
    } else {
        state.theme.dim()
    };
    Some(Line::from(Span::styled(
        fit(&format!(" / {}{caret}", line(&state.filter)), width),
        style,
    )))
}

/// `‹ <hidden column> <cards>` on the left, `<hidden column> <cards> ›` on the
/// right, and the page number between them when there is room.
fn pager_row(
    state: &LinearState,
    overlays: &CardOverlays<'_>,
    per_page: usize,
    width: usize,
) -> Line<'static> {
    let groups = state.groups();
    let start = state.sel_group.min(groups.len().saturating_sub(1)) / per_page * per_page;
    let end = (start + per_page).min(groups.len());
    let named = |at: usize| {
        let group = &groups[at];
        format!("{} {}", line(&group.label), group.issues.len())
    };
    let hidden = |range: std::ops::Range<usize>| {
        attention_count(overlays, groups[range].iter().flat_map(|g| g.issues.iter()))
    };
    let left = if start > 0 {
        format!("‹ {}{}", named(start - 1), hidden(0..start))
    } else {
        String::new()
    };
    let right = if end < groups.len() {
        format!("{}{} ›", named(end), hidden(end..groups.len()))
    } else {
        String::new()
    };
    let page = format!(
        "{}/{}",
        start / per_page + 1,
        groups.len().div_ceil(per_page)
    );
    let half = width / 2;
    let (left, right) = if left.width() + right.width() + 1 > width {
        let left = fit(&left, half.saturating_sub(1));
        let right = fit(&right, width - half);
        (left, right)
    } else {
        (left, right)
    };
    let gap = width.saturating_sub(left.width() + right.width());
    let middle = if page.width() + 2 <= gap {
        let before = (gap - page.width()) / 2;
        format!(
            "{}{page}{}",
            " ".repeat(before),
            " ".repeat(gap - page.width() - before)
        )
    } else {
        " ".repeat(gap)
    };
    let dim = state.theme.dim();
    Line::from(vec![
        Span::raw(left),
        Span::styled(middle, dim),
        Span::raw(right),
    ])
}

fn boxed(f: &mut Frame, area: Rect, title: &str, border: Style, lines: Vec<Line<'static>>) {
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = centered_rect_abs(area.width.saturating_sub(4).clamp(20, 90), h, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(border);
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
    ];
    if let Some(message) = not_imported(state) {
        lines.push(Line::from(""));
        lines.extend(message);
    }
    lines.extend([
        Line::from(""),
        Line::from("Press s (or click a space below) to pick a project for it,"),
        Line::from("or run /work:bind in this space; then press r to refresh."),
    ]);
    for warning in state.source_warnings() {
        lines.push(Line::from(Span::styled(
            format!("! {warning}"),
            state.theme.warning(),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "s pick a space · r refresh · q quit",
        state.theme.dim(),
    )));
    let Some(snapshot) = state.snapshot() else {
        boxed(f, area, "Not bound", state.theme.warning(), lines);
        return;
    };
    let strip = strip_lines(state, snapshot, area.width as usize);
    // The bottom row stays free for the toast, as under the board's strip.
    let strip_y = area
        .bottom()
        .saturating_sub(strip.len() as u16 + 1)
        .max(area.y);
    let above = Rect::new(area.x, area.y, area.width, strip_y - area.y);
    boxed(f, above, "Not bound", state.theme.warning(), lines);
    draw_strip(app, strip, f, area, strip_y);
}

/// The daemon's own not-imported words, which name the import command.
pub(super) fn not_imported(state: &LinearState) -> Option<Vec<Line<'static>>> {
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
            .map(|text| Line::from(Span::styled(text.to_string(), state.theme.warning())))
            .collect(),
    )
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
        state.theme.dim(),
    )));
    boxed(
        f,
        area,
        "Daemon is older than this board",
        state.theme.error(),
        lines,
    );
}

fn draw_error(state: &LinearState, f: &mut Frame, area: Rect) {
    let mut lines = vec![Line::from(Span::styled(
        line(state.error.as_deref().unwrap_or("snapshot failed")),
        state.theme.error(),
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
        state.theme.dim(),
    )));
    boxed(f, area, "Snapshot failed", state.theme.error(), lines);
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
            .border_style(state.theme.accent()),
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
            state.theme.dim(),
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
    if area.height == 0 {
        return;
    }
    let rect = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    let Some(theme) = app.linear.as_ref().map(|state| &state.theme) else {
        return;
    };
    let Some(toast) = &app.toast else {
        let cue = app.screen == Screen::LinearBoard
            && app.linear.as_ref().is_some_and(suggestion_selected);
        if cue {
            f.render_widget(
                Paragraph::new(Span::styled(
                    fit(" a bind · x dismiss", area.width as usize),
                    theme.accent(),
                )),
                rect,
            );
        }
        return;
    };
    let style = theme.toast(toast.is_error);
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
