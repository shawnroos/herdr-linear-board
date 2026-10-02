//! The board's look: powerline titles, lane lines drawn once across the
//! columns, fixed 13-row cards, muted unfocused columns, one board-wide
//! scroll, and the empty and short-pane states (`docs/board-design.md` §2–§3).

use super::marks::board;
use super::*;

use board_core::protocol::LinearCardPr;
use board_tui::testkit::buffer_at;
use board_tui::theme::Theme;
use board_tui::view::{card_pr_line, PrGh};
use ratatui::buffer::Buffer;

const CUT: &str = "\u{e0bc}";

fn cells(buf: &Buffer, y: u16) -> Vec<String> {
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect()
}

fn row(buf: &Buffer, y: u16) -> String {
    cells(buf, y).concat()
}

fn dump(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| row(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where `needle` starts, by cell, scanning rows top to bottom.
fn find(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    let chars: Vec<String> = needle.chars().map(String::from).collect();
    (0..buf.area.height).find_map(|y| {
        let row = cells(buf, y);
        (0..row.len())
            .find(|&x| {
                chars
                    .iter()
                    .enumerate()
                    .all(|(i, c)| row.get(x + i) == Some(c))
            })
            .map(|x| (x as u16, y))
    })
}

fn at(buf: &Buffer, needle: &str) -> (u16, u16) {
    find(buf, needle).unwrap_or_else(|| panic!("{needle:?} is not drawn:\n{}", dump(buf)))
}

fn slice(buf: &Buffer, y: u16, x: u16, w: u16) -> String {
    (x..x + w).map(|x| buf[(x, y)].symbol()).collect()
}

fn gruvbox() -> Theme {
    Theme::default()
}

fn described(mut snapshot: LinearSnapshot) -> LinearSnapshot {
    for issue in snapshot.issues.values_mut() {
        issue.description = Some(
            "When you export twice in a row the save dialog opens in Downloads instead of the \
             folder you used last, so people with deep trees navigate again every time."
                .into(),
        );
    }
    snapshot
}

fn driver_for(snapshot: LinearSnapshot) -> Driver {
    let (d, _, _) = linear_driver(
        fake_with(snapshot).with_linear_list(spaces(vec![])),
        linear_start(),
    );
    d
}

#[test]
fn a_lane_line_sits_on_one_row_across_columns_with_different_card_counts() {
    let mut d = tabs_driver();
    let buf = buffer_at(&mut d, 200, 80);
    let (_, alpha) = at(&buf, "ALPHA");
    let (_, beta) = at(&buf, "BETA");
    // Todo holds three Alpha cards and In Progress one: the Beta band starts
    // below the busiest column's Alpha cards (2 rows of line and blank, then
    // a 13-row card and a blank row each).
    assert_eq!(beta, alpha + 2 + 3 * 14, "{}", dump(&buf));
    assert_eq!(row(&buf, beta).matches("BETA").count(), 1);
    let (_, todo) = at(&buf, "WEB-104");
    let (_, progress) = at(&buf, "WEB-106");
    let (_, review) = at(&buf, "WEB-107");
    assert_eq!((todo, progress), (review, review));
    assert_eq!(review, beta + 3, "line, blank, the card's top row, header");
}

#[test]
fn a_lane_line_shows_its_count_and_fold_icon_right_aligned() {
    let mut d = tabs_driver();
    let buf = buffer_at(&mut d, 200, 80);
    let (_, alpha) = at(&buf, "ALPHA");
    let line = row(&buf, alpha).trim_end().to_string();
    assert!(line.ends_with("5 \u{f0233} ─"), "{line:?}");
    press(&mut d, KeyCode::Up);
    press(&mut d, KeyCode::Enter);
    let buf = buffer_at(&mut d, 200, 80);
    let line = row(&buf, alpha).trim_end().to_string();
    assert!(line.ends_with("5 \u{f0232} ─"), "folded: {line:?}");
}

#[test]
fn selecting_a_card_moves_no_text() {
    let mut d = driver_for(described(tabs_lanes()));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
    let before = buffer_at(&mut d, 200, 80);
    press(&mut d, KeyCode::Down);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-102"));
    let after = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&before, "WEB-101");
    assert_eq!(at(&after, "WEB-101"), (x, y));
    // Header, two title rows, three description rows, two meta rows; the two
    // reserved rule rows are the only ones selection may change.
    for dy in [0, 2, 3, 5, 6, 7, 9, 10] {
        assert_eq!(
            slice(&before, y + dy, x, 40),
            slice(&after, y + dy, x, 40),
            "row {dy} of the card moved"
        );
    }
    assert!(slice(&before, y + 1, x, 40).starts_with("────"));
    assert!(slice(&before, y + 8, x, 40).starts_with("─ ─ "));
    assert_eq!(slice(&after, y + 1, x, 40).trim(), "");
}

#[test]
fn the_focused_title_is_accent_and_inactive_titles_run_together() {
    let theme = gruvbox();
    let mut d = tabs_driver();
    let buf = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&buf, "TODO");
    assert_eq!(buf[(x, y)].bg, theme.palette.accent);
    assert_eq!(buf[(x, y)].fg, theme.palette.panel_bg);
    let (x, _) = at(&buf, "IN PROGRESS");
    assert_eq!(buf[(x, y)].bg, theme.palette.panel_bg);
    assert_eq!(buf[(x, y)].fg, theme.palette.overlay0);
    // One cut where the accent ends and one at the bar's right end; none
    // between the three inactive titles.
    assert_eq!(row(&buf, y).matches(CUT).count(), 2, "{}", row(&buf, y));
}

#[test]
fn unfocused_columns_are_muted_and_esc_unmutes_them() {
    let theme = gruvbox();
    let mut d = tabs_driver();
    let buf = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&buf, "WEB-105");
    assert_eq!(buf[(x, y)].bg, theme.muted_card);
    let (x1, y1) = at(&buf, "WEB-101");
    assert_eq!(buf[(x1, y1)].bg, theme.palette.surface0, "selected");
    let (x2, y2) = at(&buf, "WEB-102");
    assert_eq!(buf[(x2, y2)].bg, theme.palette.panel_bg, "focused column");
    press(&mut d, KeyCode::Esc);
    let buf = buffer_at(&mut d, 200, 80);
    assert_eq!(buf[(x, y)].bg, theme.palette.panel_bg);
    assert_eq!(buf[(x1, y1)].bg, theme.palette.panel_bg);
}

#[test]
fn marks_keep_their_colours_in_a_muted_column() {
    let theme = gruvbox();
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-105", MarkKind::Attention, "look", "agent-a");
    shared.mark_kind("WEB-105", MarkKind::Question, "which?", "agent-a");
    let (mut d, _) = board(&shared);
    let buf = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&buf, "WEB-105");
    let (mx, my) = at(&buf, "!?");
    assert_eq!(my, y, "marks share the header row");
    assert!(mx > x + 20, "marks sit right-aligned");
    assert_eq!(buf[(mx, my)].fg, theme.palette.red);
    assert_eq!(buf[(mx + 1, my)].fg, theme.palette.peach);
    assert_eq!(buf[(mx, my)].bg, theme.muted_card);
}

#[test]
fn a_selected_card_at_the_bottom_of_a_tall_lane_scrolls_into_full_view() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Down);
    press(&mut d, KeyCode::Down);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-103"));
    let buf = buffer_at(&mut d, 140, 30);
    let (_, title) = at(&buf, "TODO");
    let (_, header) = at(&buf, "WEB-103");
    assert!(header > title + 2, "{}", dump(&buf));
    assert!(header + 11 < 29, "card bottom off screen:\n{}", dump(&buf));
    // One offset for the whole board: In Progress's first card shares
    // WEB-101's rows, so it scrolled away with it.
    assert!(find(&buf, "WEB-101").is_none(), "{}", dump(&buf));
    assert!(find(&buf, "WEB-105").is_none(), "{}", dump(&buf));
    press(&mut d, KeyCode::Up);
    press(&mut d, KeyCode::Up);
    let buf = buffer_at(&mut d, 140, 30);
    assert!(find(&buf, "WEB-101").is_some(), "{}", dump(&buf));
}

#[test]
fn a_card_with_a_note_shows_it_in_place_of_the_last_description_line() {
    let shared = SharedFake::quiet(described(tabs_lanes()));
    shared.note("WEB-101", "pushed the fix to the branch", "agent-a");
    let (mut d, _) = board(&shared);
    let buf = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&buf, "WEB-101");
    assert!(slice(&buf, y + 5, x, 10).starts_with("When you"));
    assert!(
        slice(&buf, y + 7, x, 40).starts_with("› pushed the fix"),
        "{}",
        dump(&buf)
    );
}

#[test]
fn a_wide_glyph_title_and_the_meta_icons_stay_inside_the_card() {
    let mut snapshot = described(tabs_lanes());
    let issue = snapshot.issues.get_mut("WEB-101").unwrap();
    issue.title = "漢字のタイトル".repeat(8);
    issue.milestone = Some(board_core::protocol::LinearNamed {
        id: Some("m1".into()),
        name: Some("A milestone with a long enough name to overflow".into()),
    });
    let mut d = driver_for(snapshot);
    let buf = buffer_at(&mut d, 40, 30);
    let (x, y) = at(&buf, "WEB-101");
    let right = buf.area.width - (x - 2);
    for dy in 0..12 {
        let tail = slice(&buf, y + dy, right - 2, 2);
        assert_eq!(tail, "  ", "row {dy} reaches the inset:\n{}", dump(&buf));
    }
    assert!(row(&buf, y + 2).contains('漢'));
    assert!(row(&buf, y + 3).trim_end().ends_with('…'));
}

#[test]
fn the_pr_row_has_a_linear_only_a_loading_and_a_gh_unavailable_state() {
    let theme = gruvbox();
    let pr = LinearCardPr {
        repo: "acme/web".into(),
        number: 2132,
        url: "https://github.com/acme/web/pull/2132".into(),
        status: Some("open".into()),
        draft: Some(false),
    };
    let text = |gh| {
        card_pr_line(&theme, &pr, gh, false)
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect::<String>()
    };
    let linear = text(PrGh::LinearOnly);
    assert!(
        linear.contains("#2132") && linear.contains("open"),
        "{linear}"
    );
    assert_eq!(
        linear.matches('–').count(),
        3,
        "preview, diff, CI: {linear}"
    );
    let loading = text(PrGh::Loading);
    assert!(loading.contains("#2132") && loading.trim_end().ends_with('…'));
    assert!(!loading.contains('–'));
    let unavailable = text(PrGh::GhUnavailable);
    assert!(unavailable.contains("#2132") && unavailable.contains("gh unavailable"));
    let dim = card_pr_line(&theme, &pr, PrGh::GhUnavailable, false);
    let last = dim
        .spans
        .iter()
        .rev()
        .find(|s| !s.content.trim().is_empty());
    assert_eq!(last.unwrap().style.fg, Some(theme.palette.overlay0));
}

#[test]
fn a_card_with_a_linear_pr_shows_its_number_on_the_meta_row() {
    let mut snapshot = tabs_lanes();
    snapshot.issues.get_mut("WEB-101").unwrap().pr = Some(LinearCardPr {
        repo: "acme/web".into(),
        number: 2132,
        url: "https://github.com/acme/web/pull/2132".into(),
        status: Some("open".into()),
        draft: None,
    });
    let mut d = driver_for(snapshot);
    let buf = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&buf, "WEB-101");
    assert!(
        slice(&buf, y + 10, x, 40).contains("#2132 open"),
        "{}",
        dump(&buf)
    );
}

#[test]
fn an_empty_column_says_no_issues() {
    let mut snapshot = tabs_lanes();
    edit_first_tab(&mut snapshot, |groups| {
        groups[2].issues.clear();
        for lane in &mut groups[2].lanes {
            lane.issues.clear();
        }
    });
    let mut d = driver_for(snapshot);
    let buf = buffer_at(&mut d, 200, 80);
    let (_, alpha) = at(&buf, "ALPHA");
    let (x, y) = at(&buf, "no issues");
    let (review, _) = at(&buf, "IN REVIEW");
    assert_eq!(y, alpha + 2);
    assert!(x >= review, "drawn in the In Review column");
}

#[test]
fn a_filter_that_matches_nothing_says_so_across_the_board() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('/'));
    for c in "zzzz".chars() {
        press(&mut d, KeyCode::Char(c));
    }
    let frame = render_at(&mut d, 200, 40);
    assert_eq!(frame.matches("no matching issues").count(), 1, "{frame}");
    assert!(!frame.contains("no issues"), "{frame}");
}

#[test]
fn a_fourteen_row_pane_says_it_is_too_short() {
    let mut d = tabs_driver();
    let frame = render_at(&mut d, 80, 14);
    assert!(frame.contains("pane too short"), "{frame}");
    assert!(!frame.contains("WEB-101"), "{frame}");
}

#[test]
fn outside_text_with_escape_and_osc8_renders_as_plain_text() {
    let mut snapshot = tabs_lanes();
    snapshot.issues.get_mut("WEB-101").unwrap().description = Some(
        "see \u{1b}]8;;https://evil.example\u{7}the link\u{1b}]8;;\u{7} \u{1b}[31mred\u{1b}[0m"
            .into(),
    );
    let shared = SharedFake::quiet(snapshot);
    shared.note(
        "WEB-101",
        "done \u{1b}]8;;x\u{1b}\\here\u{1b}]8;;\u{1b}\\ ok",
        "agent-a",
    );
    let (mut d, _) = board(&shared);
    let buf = buffer_at(&mut d, 200, 80);
    let frame = dump(&buf);
    let (x, y) = at(&buf, "WEB-101");
    assert!(slice(&buf, y + 5, x, 40).contains("the link"), "{frame}");
    assert!(slice(&buf, y + 7, x, 40).contains("here"), "{frame}");
    assert!(
        !frame.contains(['\u{1b}', '\u{7}', '\u{9c}']),
        "a control byte reached the backend:\n{frame:?}"
    );
}

#[test]
fn raw_control_bytes_in_state_still_never_reach_the_backend() {
    // Bypasses the arrival sanitiser to probe the view boundary itself.
    let mut d = tabs_driver();
    let state = d.app.linear.as_mut().unwrap();
    let snapshot = state.last_good.as_mut().unwrap();
    snapshot.issues.get_mut("WEB-101").unwrap().description =
        Some("raw \u{1b}]8;;https://evil.example\u{7}link\u{1b}]8;;\u{7} text".into());
    let buf = buffer_at(&mut d, 200, 80);
    let frame = dump(&buf);
    assert!(frame.contains("link"), "{frame}");
    assert!(!frame.contains(['\u{1b}', '\u{7}']), "{frame:?}");
}

#[test]
fn a_click_on_a_card_the_board_scrolled_to_opens_that_card() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Down);
    press(&mut d, KeyCode::Down);
    let buf = buffer_at(&mut d, 140, 30);
    let (x, y) = at(&buf, "WEB-103");
    d.handle(left_down(x, y + 3));
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert_eq!(
        d.app.linear.as_ref().unwrap().detail.as_deref(),
        Some("WEB-103")
    );
}

#[test]
fn a_pane_narrower_than_a_card_draws_without_panicking() {
    let mut d = tabs_driver();
    render_at(&mut d, 5, 20);
    render_at(&mut d, 1, 20);
}
