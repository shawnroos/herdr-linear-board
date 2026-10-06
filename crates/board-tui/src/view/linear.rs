//! Linear-mode rendering: the space board built from the work plugin's
//! snapshot, the card detail, and the not-bound / error / stale-daemon
//! screens. Only `Mode::Linear` reaches this module, and it reads
//! `App::linear` only, never `App::board`.

use board_core::protocol::{LinearCardPr, LinearGhSource, LinearIssue, LinearSnapshot, MarkKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{
    sanitise, App, BarItem, CardOverlay, CardOverlays, Density, Focus, LinearState, Page, Screen,
    Side, BAR_GAP, BAR_HINT, CONTROLS,
};
use crate::theme::Theme;
use crate::widgets::{HitMap, Zone};

use super::linear_strip::{draw_strip, strip_lines};
use super::{centered_rect_abs, linear_help_keys};

// The tallest card in each density, which the board needs room for.
const FULL_CARD_H: u16 = 13;
const FULL_COMPACT_CARD_H: u16 = 8;
const CARD_INSET: u16 = 2;
// Each column's half of the 4-cell gap between columns; the outer halves
// hold the paging buttons.
const SLOT_PAD: u16 = 2;
// A 44-cell card and its gaps. Supersedes the 72-cell stacking threshold of
// the 2026-09-16 pickers-and-layout plan.
const MIN_COL_W: u16 = 48;
const CARD_MAX: u16 = 64;
const CARD_MIN: u16 = 40;
const CARD_GAP: u16 = 1;
const HEADER_ROWS: u16 = 2;
// The column title bar and the blank row under it.
const TITLE_ROWS: u16 = 2;
/// The filter card and the blank row under it; the title rows' blank sits above.
const FILTER_ROWS: u16 = 2;

mod icon {
    pub const PERSON: &str = "\u{f415}";
    pub const MILESTONE: &str = "\u{f45d}";
    pub const PULL_REQUEST: &str = "\u{f407}";
    pub const EYE: &str = "\u{f441}";
    pub const DIFF: &str = "\u{f4d2}";
    pub const CHECK: &str = "\u{f058}";
    pub const CROSS: &str = "\u{f057}";
    pub const CLOCK: &str = "\u{f017}";
    pub const FILTER: &str = "\u{f0232}";
    pub const FILTER_OUTLINE: &str = "\u{f0233}";
    pub const SLANT: &str = "\u{e0bc}";
    pub const INFO: &str = "\u{f02fd}";
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
        Screen::LinearDetail => draw_issue_screen(app, state, f, area),
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
    match under {
        Screen::LinearNotBound => draw_not_bound(app, state, f, area),
        Screen::LinearDetail => draw_issue_screen(app, state, f, area),
        _ => draw_board(app, state, f, area),
    }
}

/// The open issue: a modal over the board faded behind it, or the issue over
/// the whole board. The bar is drawn after, unfaded, either way.
fn draw_issue_screen(app: &App, state: &LinearState, f: &mut Frame, area: Rect) {
    use super::linear_issue::{open_rect, open_slot, Look};
    let Some(slot) = open_slot(state) else {
        return draw_board(app, state, f, area);
    };
    let (rect, look) = open_rect(app, state, area);
    if look == Look::Modal {
        draw_board(app, state, f, area);
        // The board under a modal takes no clicks.
        app.hit_map.borrow_mut().clear();
        let bar = super::linear_issue::bar_rows(state, area);
        let behind = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(bar));
        fade_outside(state, f, behind, rect);
    }
    super::linear_issue::draw(app, state, &slot, f, rect, look);
}

/// Every cell of `area` outside `keep` blended toward the app background
/// (KTD6), so whatever drew there recedes without a muted style of its own.
fn fade_outside(state: &LinearState, f: &mut Frame, area: Rect, keep: Rect) {
    let buf = f.buffer_mut();
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if keep.contains((x, y).into()) {
                continue;
            }
            let cell = &mut buf[(x, y)];
            cell.fg = state.theme.fade(cell.fg);
            cell.bg = state.theme.fade(cell.bg);
        }
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
            " Enter detail · r refresh · ? bar · ctrl+q quit",
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

/// Up to `n` rows of `text` by display width, only as many as it fills:
/// every row but the last breaks at a word boundary (mid-word only when one
/// word is wider than the row), and the last takes the rest, ending in an
/// ellipsis when it does not fit.
fn wrap_lines(text: &str, width: usize, n: usize) -> Vec<String> {
    let mut rest = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = Vec::with_capacity(n);
    for at in 0..n {
        if rest.is_empty() {
            break;
        }
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
    out
}

fn title_lines(title: &str, width: usize) -> Vec<String> {
    if title.trim().is_empty() {
        return vec!["(no title)".to_string()];
    }
    wrap_lines(title, width, 2)
}

/// The description's rows, the newest note taking the last of the three.
fn description_lines(issue: &LinearIssue, overlay: CardOverlay<'_>, width: usize) -> Vec<String> {
    let note = overlay
        .latest_note
        .map(|note| fit(&format!("› {}", line(&note.body)), width));
    let room = if note.is_some() { 2 } else { 3 };
    let mut rows = wrap_lines(
        &line(issue.description.as_deref().unwrap_or("")),
        width,
        room,
    );
    rows.extend(note);
    rows
}

/// `spans` cut to `width` display cells, the last kept span ending in `…`
/// when anything was cut.
pub(super) fn clip_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    clip(spans, width)
}

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

pub(super) fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

#[derive(Debug, Clone, Copy)]
struct CardLook {
    selected: bool,
    muted: bool,
}

/// What `gh` adds to a card's PR row: its preview, diff and CI once read,
/// nothing from a daemon without `gh` (dim placeholders), still loading, or
/// unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrGh {
    Read,
    LinearOnly,
    Loading,
    GhUnavailable,
}

/// Data `gh` already read wins, even while `gh` is failing now.
fn pr_gh(source: Option<&LinearGhSource>, pr: &LinearCardPr) -> PrGh {
    let read =
        pr.state.is_some() || pr.ci.is_some() || pr.additions.is_some() || pr.preview.is_some();
    match source {
        _ if read => PrGh::Read,
        None => PrGh::LinearOnly,
        Some(source) if source.status == "unavailable" || pr.gh_error.is_some() => {
            PrGh::GhUnavailable
        }
        Some(_) => PrGh::Loading,
    }
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
        PrGh::Read => spans.extend(pr_read(theme, pr, muted)),
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

/// Preview, diff and CI as `gh` read them; each is left out when `gh` has
/// nothing for it.
fn pr_read(theme: &Theme, pr: &LinearCardPr, muted: bool) -> Vec<Span<'static>> {
    let tone = |c| Style::default().fg(if muted { theme.muted_detail } else { c });
    let label = if muted {
        theme.card_detail_muted()
    } else {
        theme.card_detail()
    };
    let mut spans = vec![];
    if pr.preview.is_some() {
        spans.push(Span::styled(
            format!("  {}", icon::EYE),
            tone(theme.palette.teal),
        ));
        spans.push(Span::styled(" Preview", label));
    }
    if pr.additions.is_some() || pr.deletions.is_some() {
        spans.push(Span::styled(
            format!("  {}", icon::DIFF),
            tone(theme.palette.yellow),
        ));
        spans.push(Span::styled(
            format!(" +{}", pr.additions.unwrap_or(0)),
            tone(theme.palette.green),
        ));
        spans.push(Span::styled(
            format!(" −{}", pr.deletions.unwrap_or(0)),
            tone(theme.palette.red),
        ));
    }
    let ci = pr.ci.as_ref().and_then(|ci| match ci.state.as_str() {
        "pass" => Some((icon::CHECK, theme.palette.green)),
        "fail" => Some((icon::CROSS, theme.palette.red)),
        "pending" => Some((icon::CLOCK, theme.palette.yellow)),
        _ => None,
    });
    if let Some((glyph, colour)) = ci {
        spans.push(Span::styled(format!("  {glyph}"), tone(colour)));
        spans.push(Span::styled(" CI", label));
    }
    spans
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

/// One row per meta group with data: assignee and milestone, then the PR.
fn card_meta(state: &LinearState, issue: &LinearIssue, look: CardLook) -> Vec<Vec<Span<'static>>> {
    let t = &state.theme;
    let icon_style = |c| Style::default().fg(if look.muted { t.muted_detail } else { c });
    let label = if look.muted {
        t.card_detail_muted()
    } else {
        t.card_detail()
    };
    let who = issue.assignee.as_ref().and_then(|a| a.name.as_deref());
    let milestone = issue
        .milestone
        .as_ref()
        .and_then(|m| m.name.as_deref())
        .filter(|n| !n.trim().is_empty());
    let mut rows = vec![];
    if who.is_some() || milestone.is_some() {
        let who = who.map_or_else(|| " unassigned".to_string(), |n| format!(" @{}", line(n)));
        let mut first = vec![
            Span::styled(icon::PERSON, icon_style(t.palette.blue)),
            Span::styled(who, label),
        ];
        if let Some(name) = milestone {
            first.push(Span::raw("  "));
            first.push(Span::styled(icon::MILESTONE, icon_style(t.palette.mauve)));
            first.push(Span::styled(format!(" {}", line(name)), label));
        }
        rows.push(first);
    }
    if let Some(pr) = &issue.pr {
        let source = state.snapshot().and_then(|s| s.gh.as_ref());
        rows.push(card_pr_line(t, pr, pr_gh(source, pr), look.muted).spans);
    }
    rows
}

/// A card's rows, each starting with the 2-cell inset, sized to its content
/// (supersedes the fixed 5-row card of the 2026-09-16 pickers-and-layout
/// plan). The board measures cards with this, so its length is the card's
/// height. Selection changes only the title colour and the rule rows roomy
/// cards reserve, so no text moves and no height changes.
fn card_lines(
    state: &LinearState,
    overlay: CardOverlay<'_>,
    issue: &LinearIssue,
    width: u16,
    look: CardLook,
    density: Density,
) -> Vec<Line<'static>> {
    let roomy = density == Density::Roomy;
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
    let mut lines = vec![];
    if roomy {
        lines.push(Line::default());
    }
    lines.push(inset(card_header(state, overlay, issue, inner, look)));
    if roomy {
        lines.push(rule("─".repeat(inner)));
    }
    for text in title_lines(&line(&issue.title), inner) {
        lines.push(inset(vec![Span::styled(text, title_style)]));
    }
    let description = description_lines(issue, overlay, inner);
    if roomy && !description.is_empty() {
        lines.push(Line::default());
    }
    for text in description {
        lines.push(inset(vec![Span::styled(text, detail)]));
    }
    let meta = card_meta(state, issue, look);
    if roomy && !meta.is_empty() {
        lines.push(rule("─ ".repeat(inner / 2)));
    }
    for row in meta {
        lines.push(inset(row));
    }
    if roomy {
        lines.push(Line::default());
    }
    lines
}

/// `card_lines` for the card `identifier`, or one `(missing)` row, padded as
/// the density pads a card, when the snapshot lacks the issue.
fn card_rows(
    state: &LinearState,
    overlays: &CardOverlays<'_>,
    identifier: &str,
    width: u16,
    look: CardLook,
) -> Vec<Line<'static>> {
    if let Some(issue) = state.snapshot().and_then(|s| s.issues.get(identifier)) {
        return card_lines(
            state,
            overlays.get(identifier),
            issue,
            width,
            look,
            state.density,
        );
    }
    let missing = Line::from(format!(
        "{}{}",
        " ".repeat(CARD_INSET as usize),
        fit(
            &format!("{} (missing)", line(identifier)),
            width.saturating_sub(2 * CARD_INSET) as usize
        )
    ));
    match state.density {
        Density::Roomy => vec![Line::default(), missing, Line::default()],
        Density::Compact => vec![missing],
    }
}

/// How many columns fit in `width` cells before the workflow caps them: at
/// least one, so a pane narrower than a column still shows a column.
pub fn board_columns(width: u16) -> usize {
    usize::from(width / MIN_COL_W).max(1)
}

/// Whether `width` cells leave the board two columns of room, the least a
/// pinned issue panel keeps (KTD12).
pub fn pin_fits(width: u16) -> bool {
    width >= 2 * MIN_COL_W
}

/// Cards a row and card width for a column `inside` cells wide: two when two
/// minimum cards and their gap fit, each capped at `CARD_MAX`.
fn grid(inside: u16) -> (usize, u16) {
    let ups = if inside >= 2 * CARD_MIN + CARD_GAP {
        2
    } else {
        1
    };
    let width = (inside.saturating_sub((ups - 1) * CARD_GAP) / ups).min(CARD_MAX);
    (usize::from(ups), width)
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

/// Whether `area` holds the header, one full card and the bar.
fn board_fits(state: &LinearState, area: Rect) -> bool {
    let card = match state.density {
        Density::Roomy => FULL_CARD_H,
        Density::Compact => FULL_COMPACT_CARD_H,
    };
    area.height >= HEADER_ROWS + card + state.bar_layout(area.width).rows()
}

fn draw_board(app: &App, state: &LinearState, f: &mut Frame, area: Rect) {
    if !board_fits(state, area) {
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
    let footer_h = state.bar_layout(area.width).rows();
    let mut y = area.y + HEADER_ROWS;
    let mut body_h = area.height.saturating_sub(HEADER_ROWS + strip_h + footer_h);
    let board_w = state.board_width(area.width);
    let overlays = state.overlays();
    if let Some(row) = tab_row(state, &overlays, board_w as usize).filter(|_| body_h > 0) {
        f.render_widget(Paragraph::new(row), Rect::new(area.x, y, board_w, 1));
        y += 1;
        body_h -= 1;
    }
    let body = Rect::new(area.x, y, board_w, body_h);
    draw_columns(
        state,
        &overlays,
        f,
        body,
        area.width,
        &mut app.hit_map.borrow_mut(),
    );
    draw_strip(app, strip, f, area, body.bottom());
    if let (Some(rect), Some(slot)) = (
        pinned_rect(state, area),
        super::linear_issue::pinned_slot(state),
    ) {
        let look = super::linear_issue::Look::Pinned;
        super::linear_issue::draw(app, state, &slot, f, rect, look);
    }
}

/// Where the pinned issue draws in a frame of `area`: one column's slot at
/// the right, from under the board's header to above the strip and the bar.
/// `None` while nothing is pinned or the board has no room for it.
pub(super) fn pinned_rect(state: &LinearState, area: Rect) -> Option<Rect> {
    if !state.pin_shown(area.width) || !board_fits(state, area) {
        return None;
    }
    let snapshot = state.snapshot()?;
    let strip_h = strip_lines(state, snapshot, area.width as usize).len() as u16;
    let footer_h = state.bar_layout(area.width).rows();
    let board_w = state.board_width(area.width);
    Some(Rect::new(
        area.x + board_w,
        area.y + HEADER_ROWS,
        area.width - board_w,
        area.height.saturating_sub(HEADER_ROWS + strip_h + footer_h),
    ))
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
/// a blank row in roomy density, then its cards. Each column stacks its cards
/// in rows of `ups`, a row as tall as its tallest card and one blank row
/// after it; the band is as tall as its tallest stack, so lanes align.
struct Band<'a> {
    key: &'a str,
    label: Option<&'a str>,
    total: usize,
    folded: bool,
    top: u16,
    head: u16,
    columns: Vec<Vec<&'a str>>,
    /// Each card's `(top, height)`, the top counted from the band's first
    /// card row.
    cards: Vec<Vec<(u16, u16)>>,
    stack: u16,
}

impl Band<'_> {
    fn cards_top(&self) -> u16 {
        self.top + self.head
    }

    fn card(&self, column: usize, at: usize) -> Option<(u16, u16)> {
        let (top, h) = *self.cards.get(column)?.get(at)?;
        Some((self.cards_top() + top, h))
    }

    fn height(&self) -> u16 {
        self.head + self.stack
    }
}

/// Each card's `(top, height)` in a column of `heights`, `ups` a row, and the
/// column's total height.
fn stack(heights: &[u16], ups: usize) -> (Vec<(u16, u16)>, u16) {
    let mut cards = Vec::with_capacity(heights.len());
    let mut top = 0;
    for row in heights.chunks(ups.max(1)) {
        cards.extend(row.iter().map(|&h| (top, h)));
        top += row.iter().copied().max().unwrap_or(0) + 1;
    }
    (cards, top)
}

/// The active tab's bands, top to bottom, for every column (not just the
/// page shown), so the bands keep their height across pages. Every column
/// measures its cards at the same `ups` and `card_w`, so a hidden column
/// measures as a shown one would.
fn bands<'a>(
    state: &'a LinearState,
    overlays: &CardOverlays<'_>,
    ups: usize,
    card_w: u16,
) -> Vec<Band<'a>> {
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
    let measure = CardLook {
        selected: false,
        muted: false,
    };
    let lane_gap = match state.density {
        Density::Roomy => 1,
        Density::Compact => 0,
    };
    let mut top = 0;
    keys.into_iter()
        .map(|(key, label)| {
            let mut total = 0;
            let columns: Vec<Vec<&str>> = rows
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
            let mut tallest = 0;
            let cards = columns
                .iter()
                .map(|column| {
                    let heights: Vec<u16> = column
                        .iter()
                        .map(|id| card_rows(state, overlays, id, card_w, measure).len() as u16)
                        .collect();
                    let (cards, h) = stack(&heights, ups);
                    tallest = tallest.max(h);
                    cards
                })
                .collect();
            let band = Band {
                key,
                label,
                total,
                folded: label.is_some() && state.folded(key),
                top,
                head: if label.is_some() { 1 + lane_gap } else { 0 },
                columns,
                cards,
                stack: tallest,
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

/// `full_w` is the frame's width, which the page is reckoned from; `body`
/// is the board's share of it.
fn draw_columns(
    state: &LinearState,
    overlays: &CardOverlays<'_>,
    f: &mut Frame,
    body: Rect,
    full_w: u16,
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
    let page = state.page(full_w);
    let visible = page.visible as u16;
    let board_focus = state.focus.board();
    let nothing_selected = matches!(board_focus, Focus::None | Focus::Bar(_) | Focus::Pinned);
    // Columns share the width, the remainder spread one cell at a time, so
    // the last column's outer gap ends at the board's edge.
    let slot_x =
        |slot: u16| body.x + (u32::from(slot) * u32::from(body.width) / u32::from(visible)) as u16;
    let slots: Vec<(u16, u16, usize)> = (0..visible)
        .map(|slot| {
            let x = slot_x(slot);
            (x, slot_x(slot + 1) - x, page.start + usize::from(slot))
        })
        .collect();
    // One card width for every column: a card's height depends on its width,
    // and slots differ by a cell, which would break lane alignment.
    let narrowest = slots.iter().map(|&(_, w, _)| w).min().unwrap_or(0);
    let (ups, card_w) = grid(narrowest.saturating_sub(2 * SLOT_PAD));
    let bands = bands(state, overlays, ups, card_w);
    let counts: Vec<usize> = (0..groups.len())
        .map(|g| {
            state
                .lane_rows(&groups[g], None)
                .iter()
                .map(|row| row.count)
                .sum()
        })
        .collect();

    let titles = slots
        .iter()
        .map(|&(_, col_w, idx)| {
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
    for &(x, col_w, idx) in &slots {
        hit_map.push(
            Rect::new(x, body.y, col_w, 1),
            Zone::LinearGroup(groups[idx].key.clone()),
        );
    }
    let line_w = body.width.saturating_sub(2 * SLOT_PAD);
    let mut above = TITLE_ROWS;
    if filter_shown(state) && body.height > TITLE_ROWS {
        f.render_widget(
            filter_card(state, line_w as usize, counts.iter().sum()),
            Rect::new(body.x + SLOT_PAD, body.y + TITLE_ROWS, line_w, 1),
        );
        above += FILTER_ROWS;
    }
    if body.height <= above {
        return;
    }
    let view = Rect::new(body.x, body.y + above, body.width, body.height - above);

    let selected_slot = state.selected_slot();
    let total: u16 = bands.iter().map(Band::height).sum();
    let target = match board_focus {
        Focus::LaneLine(key) => bands
            .iter()
            .find(|b| b.label.is_some() && b.key == key)
            .map(|b| (b.top, 1)),
        Focus::Card(_) => selected_slot.and_then(|(lane, id)| {
            let band = bands.iter().find(|b| b.key == lane)?;
            let at = band.columns[sel_group].iter().position(|c| *c == id)?;
            band.card(sel_group, at)
        }),
        _ => None,
    };
    let off = scroll_to(state.board_scroll.get(), target, view.height, total);
    state.board_scroll.set(off);
    let on_screen = |top: u16, len: u16| -> Option<(u16, u16, u16)> {
        let bottom = (top + len).min(off + view.height);
        let from = top.max(off);
        (from < bottom).then(|| (view.y + from - off, top.max(off) - top, bottom - from))
    };

    let inset = SLOT_PAD + CARD_INSET;
    if state.filtering() && counts.iter().all(|&n| n == 0) {
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
        draw_pager(state, f, view, page);
        return;
    }

    let holding_lane = selected_slot.map(|(lane, _)| lane);
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

    for &(x, col_w, idx) in &slots {
        let in_focus = idx == sel_group;
        let muted = !in_focus && !nothing_selected;
        let column_x = x + SLOT_PAD;
        let inside = col_w.saturating_sub(2 * SLOT_PAD);
        if counts[idx] == 0 {
            let first = bands.first().map_or(0, Band::cards_top);
            if let Some((y, _, _)) = on_screen(first, 1) {
                f.render_widget(
                    Paragraph::new(Span::styled(
                        fit("no issues", inside.saturating_sub(CARD_INSET) as usize),
                        t.dim(),
                    )),
                    Rect::new(
                        column_x + CARD_INSET,
                        y,
                        inside.saturating_sub(CARD_INSET),
                        1,
                    ),
                );
            }
            continue;
        }
        for band in &bands {
            for (at, &identifier) in band.columns[idx].iter().enumerate() {
                let Some((top, card_h)) = band.card(idx, at) else {
                    continue;
                };
                let Some((y, skip, h)) = on_screen(top, card_h) else {
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
                let lines = card_rows(state, overlays, identifier, card_w, look);
                let card_x = column_x + (at % ups) as u16 * (card_w + CARD_GAP);
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
    draw_pager(state, f, view, page);
}

/// The outer gaps as `←` / `→` buttons over the columns hidden on that side,
/// each with its count: one tone above the board while selected, flush
/// otherwise.
fn draw_pager(state: &LinearState, f: &mut Frame, view: Rect, page: Page) {
    let t = &state.theme;
    let focused = match state.focus {
        Focus::Pager(side, _) => Some(side),
        _ => None,
    };
    for side in [Side::Left, Side::Right] {
        let hidden = page.hidden(side);
        if hidden == 0 || view.width < 2 * SLOT_PAD {
            continue;
        }
        let style = if focused == Some(side) {
            t.panel().add_modifier(Modifier::BOLD)
        } else {
            t.app().patch(t.dim())
        };
        let count = fit(&hidden.to_string(), 2);
        let (x, arrow, count) = match side {
            Side::Left => (view.x, " ←", format!("{count:>2}")),
            Side::Right => (view.right() - SLOT_PAD, "→ ", format!("{count:<2}")),
        };
        let mid = view.height.saturating_sub(1) / 2;
        let lines: Vec<Line> = (0..view.height)
            .map(|row| {
                let text = if row == mid {
                    arrow.to_string()
                } else if row == mid + 1 {
                    count.clone()
                } else {
                    "  ".to_string()
                };
                Line::styled(text, style)
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines),
            Rect::new(x, view.y, SLOT_PAD, view.height),
        );
    }
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

/// The filter shows while selected or while it filters anything.
fn filter_shown(state: &LinearState) -> bool {
    state.focus == Focus::Filter || state.filtering()
}

/// The filter as a card `width` cells wide: icon, the group alert's chip in
/// its mark's colour, the text, and `matches` right-aligned. Only the text is
/// ever cut.
fn filter_card(state: &LinearState, width: usize, matches: usize) -> Paragraph<'static> {
    let t = &state.theme;
    let selected = state.focus == Focus::Filter;
    let surface = if selected { t.card_active() } else { t.card() };
    let inset = " ".repeat(CARD_INSET as usize);
    let icon_style = if selected { t.accent() } else { t.dim() };
    let mut spans = vec![
        Span::raw(inset.clone()),
        Span::styled(format!("{} ", icon::FILTER), icon_style),
    ];
    let mut used = 2 * inset.len() + icon::FILTER.width() + 1;
    if let Some(kind) = state.filter_chip {
        let chip = match kind {
            MarkKind::Attention => "! needs you",
            MarkKind::Question => "? question",
            MarkKind::Suggestion => "◇ suggestion",
            MarkKind::Done => "✓ done",
        };
        spans.push(Span::styled(chip, t.mark(kind)));
        spans.push(Span::raw(" "));
        used += chip.width() + 1;
    }
    let count = match matches {
        1 => "1 match".to_string(),
        n => format!("{n} matches"),
    };
    used += count.width() + 1;
    let caret = if selected { "▏" } else { "" };
    let text = fit(
        &format!("{}{caret}", line(&state.filter)),
        width.saturating_sub(used),
    );
    let pad = width.saturating_sub(used + text.width()) + 1;
    spans.push(Span::raw(text));
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(count, t.dim()));
    spans.push(Span::raw(inset));
    Paragraph::new(Line::from(spans)).style(surface)
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

/// Whether `screen` draws the board, and so the bar under it.
fn shows_bar(app: &App) -> bool {
    let under = match app.screen {
        Screen::Help => app.help_return_to,
        Screen::LinearPicker => app
            .picker
            .as_ref()
            .map_or(Screen::LinearBoard, |p| p.return_to),
        other => other,
    };
    matches!(
        under,
        Screen::LinearBoard | Screen::LinearError | Screen::LinearDetail
    )
}

/// Whether the screen on show, or the one under its overlay, is the issue.
fn on_issue(app: &App) -> bool {
    app.screen == Screen::LinearDetail
        || (app.screen == Screen::Help && app.help_return_to == Screen::LinearDetail)
}

fn draw_bottom(app: &App, f: &mut Frame, area: Rect) {
    let Some(state) = app.linear.as_ref() else {
        return;
    };
    if area.height == 0 {
        return;
    }
    if shows_bar(app) {
        let fits = if on_issue(app) {
            super::linear_issue::bar_rows(state, area) > 0
        } else {
            board_fits(state, area)
        };
        if fits {
            draw_bar(app, state, f, area);
        }
        return;
    }
    let Some(toast) = &app.toast else {
        return;
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            fit(
                &format!(" {} ", line(&sanitise(&toast.text))),
                area.width as usize,
            ),
            state.theme.toast(toast.is_error),
        )),
        Rect::new(area.x, area.bottom() - 1, area.width, 1),
    );
}

fn bar_item(state: &LinearState, item: &BarItem, label: String, selected: bool) -> Span<'static> {
    let t = &state.theme;
    let style = if selected {
        t.column_title_active()
    } else {
        match item {
            BarItem::Request(_) => t.show_mark(),
            BarItem::NeedsYou => t.mark(MarkKind::Attention).add_modifier(Modifier::BOLD),
            BarItem::Question => t.mark(MarkKind::Question),
            BarItem::CiFailing(_) | BarItem::CiCollapsed => t.error(),
            BarItem::More => t.accent(),
            _ => t.dim(),
        }
    };
    Span::styled(format!(" {label} "), style)
}

/// The issue's tip while it is open, else the board's.
fn tip_text(app: &App, state: &LinearState, width: u16) -> String {
    if !on_issue(app) {
        return state.bar_tip(width);
    }
    if width < super::linear_issue::MODAL_MIN {
        return format!(
            "Esc goes back · Enter opens the row · e and o need {} cells",
            super::linear_issue::MODAL_MIN
        );
    }
    match state.issue_mode {
        crate::app::IssueMode::Window => {
            "Esc goes back · e expands · o pins it · u opens it in Linear".into()
        }
        crate::app::IssueMode::Expanded => {
            "Esc goes back · e shrinks to a window · o pins it".into()
        }
    }
}

/// Controls left, the tip centred and alerts right on one row; three
/// centred rows (tip, alerts, controls) when they do not fit side by side.
fn draw_bar(app: &App, state: &LinearState, f: &mut Frame, area: Rect) {
    let t = &state.theme;
    let w = usize::from(area.width);
    let layout = state.bar_layout(area.width);
    let selected = match &state.focus {
        Focus::Bar(item) => Some(item),
        _ => None,
    };
    let mut controls = vec![];
    // `?` opens help on the issue, not the bar, so the hint stays off there.
    if layout.hint && !on_issue(app) {
        let (key, rest) = BAR_HINT.split_at(1);
        controls.push(Span::styled(format!(" {key}"), t.text()));
        controls.push(Span::styled(format!("{rest} "), t.dim()));
    }
    for item in &CONTROLS {
        controls.push(bar_item(
            state,
            item,
            state.bar_label(item),
            selected == Some(item),
        ));
    }
    let mut alerts: Vec<Span<'static>> = layout
        .alerts()
        .iter()
        .map(|item| bar_item(state, item, state.bar_label(item), selected == Some(item)))
        .collect();
    if layout.hidden() > 0 {
        alerts.push(bar_item(
            state,
            &BarItem::More,
            layout.more_label(),
            selected == Some(&BarItem::More),
        ));
    }
    let tip = |room: usize| -> Vec<Span<'static>> {
        match &app.toast {
            Some(toast) => vec![Span::styled(
                fit(&format!(" {} ", line(&sanitise(&toast.text))), room),
                t.toast(toast.is_error),
            )],
            None => clip(
                vec![
                    Span::styled(icon::INFO, t.accent()),
                    Span::styled(
                        format!(" {}", line(&tip_text(app, state, area.width))),
                        t.secondary(),
                    ),
                ],
                room,
            ),
        }
    };
    let rows = layout.rows();
    let top = area.bottom().saturating_sub(rows);
    let centred = |spans: Vec<Span<'static>>| {
        let spans = clip(spans, w);
        let mut out = vec![Span::raw(
            " ".repeat(w.saturating_sub(spans_width(&spans)) / 2),
        )];
        out.extend(spans);
        Line::from(out)
    };
    let lines = if layout.stacked {
        vec![centred(tip(w)), centred(alerts), centred(controls)]
    } else {
        let (lw, rw) = (spans_width(&controls), spans_width(&alerts));
        let room = w.saturating_sub(lw + rw + 2 * BAR_GAP);
        let tip = tip(room);
        let cw = spans_width(&tip);
        let start = (w.saturating_sub(cw) / 2)
            .min(w.saturating_sub(rw + BAR_GAP + cw))
            .max(lw + BAR_GAP);
        let mut spans = controls;
        spans.push(Span::raw(" ".repeat(start.saturating_sub(lw))));
        spans.extend(tip);
        spans.push(Span::raw(" ".repeat(w.saturating_sub(start + cw + rw))));
        spans.extend(alerts);
        vec![Line::from(spans)]
    };
    f.render_widget(
        Paragraph::new(lines).style(t.app()),
        Rect::new(area.x, top, area.width, rows.min(area.height)),
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
