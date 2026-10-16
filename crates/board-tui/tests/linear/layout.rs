//! The board's look: powerline titles, lane lines drawn once across the
//! columns, content-sized cards in two densities, muted unfocused columns, one board-wide
//! scroll, and the empty and short-pane states (`docs/board-design.md` §2–§3).

use super::marks::board;
use super::*;

use board_core::protocol::{LinearCardPr, LinearGhSource, LinearPrCi};
use board_tui::app::Side;
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
    // a 6-row title-only card and a blank row each).
    assert_eq!(beta, alpha + 2 + 3 * 7, "{}", dump(&buf));
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
    let mut snapshot = described(tabs_lanes());
    for issue in snapshot.issues.values_mut() {
        issue.assignee = assignee("Example User");
    }
    let mut d = driver_for(snapshot);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
    let before = buffer_at(&mut d, 200, 80);
    press(&mut d, KeyCode::Down);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-102"));
    let after = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&before, "WEB-101");
    assert_eq!(at(&after, "WEB-101"), (x, y));
    // Header, two title rows, three description rows, one meta row; the two
    // reserved rule rows are the only ones selection may change.
    for dy in [0, 2, 3, 5, 6, 7, 9] {
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
    let buf = buffer_at(&mut d, 140, 20);
    let (_, title) = at(&buf, "TODO");
    let (top, h) = card_span(&buf, "WEB-103");
    assert!(top > title + 1, "{}", dump(&buf));
    assert!(top + h < 20, "card bottom off screen:\n{}", dump(&buf));
    // One offset for the whole board: In Progress's first card shares
    // WEB-101's rows, so it scrolled away with it.
    assert!(find(&buf, "WEB-101").is_none(), "{}", dump(&buf));
    assert!(find(&buf, "WEB-105").is_none(), "{}", dump(&buf));
    press(&mut d, KeyCode::Up);
    press(&mut d, KeyCode::Up);
    let buf = buffer_at(&mut d, 140, 20);
    assert!(find(&buf, "WEB-101").is_some(), "{}", dump(&buf));
}

#[test]
fn the_description_follows_the_title_and_a_blank_row_sets_off_the_footer() {
    let mut d = driver_for(sized_cards());
    let buf = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&buf, "WEB-101");
    let row = |dy: u16| slice(&buf, y + dy, x, 40);
    let desc = (1..12)
        .find(|dy| row(*dy).starts_with("When you"))
        .expect("description");
    assert!(
        !row(desc - 1).trim().is_empty(),
        "the row above the description is the title:\n{}",
        dump(&buf)
    );
    let meta = (desc..13)
        .find(|dy| row(*dy).contains("Example User"))
        .expect("meta");
    assert!(
        row(meta - 2).trim().is_empty(),
        "a blank row between description and footer:\n{}",
        dump(&buf)
    );
    assert_eq!(
        card_height(&buf, "WEB-101"),
        13,
        "a full card keeps its height"
    );
}

#[test]
fn a_card_with_a_note_shows_it_in_place_of_the_last_description_line() {
    let shared = SharedFake::quiet(described(tabs_lanes()));
    shared.note("WEB-101", "pushed the fix to the branch", "agent-a");
    let (mut d, _) = board(&shared);
    let buf = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&buf, "WEB-101");
    assert!(slice(&buf, y + 4, x, 10).starts_with("When you"));
    assert!(
        slice(&buf, y + 6, x, 40).starts_with("› pushed the fix"),
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
        ..Default::default()
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
        ..Default::default()
    });
    let mut d = driver_for(snapshot);
    let buf = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&buf, "WEB-101");
    assert!(
        slice(&buf, y + 5, x, 40).contains("#2132 open"),
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
    assert!(slice(&buf, y + 4, x, 40).contains("the link"), "{frame}");
    assert!(slice(&buf, y + 6, x, 40).contains("here"), "{frame}");
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
    let buf = buffer_at(&mut d, 140, 20);
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

// -- widths, 2-up and paging (`docs/responsive-board.md` Width, Hidden columns)

fn one_column_board() -> Driver {
    let mut snapshot = tabs_lanes();
    edit_first_tab(&mut snapshot, |groups| groups.truncate(1));
    driver_for(snapshot)
}

fn page_focus(d: &Driver) -> Focus {
    d.app.linear.as_ref().unwrap().focus.clone()
}

fn on_pager(d: &Driver, side: Side) -> bool {
    matches!(page_focus(d), Focus::Pager(s, _) if s == side)
}

fn resize(d: &mut Driver, w: u16, h: u16) -> Buffer {
    d.sync_frame_area(ratatui::layout::Rect::new(0, 0, w, h));
    buffer_at(d, w, h)
}

fn shift_wheel(kind: MouseEventKind, x: u16, y: u16) -> Msg {
    Msg::Mouse(crossterm::event::MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: crossterm::event::KeyModifiers::SHIFT,
    })
}

#[test]
fn the_column_count_is_the_width_over_48_capped_at_the_workflow() {
    let mut d = tabs_driver();
    for (width, columns) in [
        (47u16, 1usize),
        (96, 2),
        (143, 2),
        (144, 3),
        (191, 3),
        (192, 4),
        (228, 4),
    ] {
        let frame = render_at(&mut d, width, 40);
        assert_eq!(titles(&frame), columns, "{width}:\n{frame}");
    }
}

#[test]
fn a_column_inside_80_shows_one_capped_card_a_row_and_81_shows_two() {
    let theme = gruvbox();
    let mut d = one_column_board();
    // 84 cells: a 2-cell gap each side leaves an inside of 80.
    let buf = buffer_at(&mut d, 84, 60);
    let (x, y) = at(&buf, "WEB-101");
    let (x2, y2) = at(&buf, "WEB-102");
    // A one-row title at 64 cells: a 5-row card and its blank row.
    assert_eq!((x2, y2), (x, y + 6), "1-up:\n{}", dump(&buf));
    let card_x = x - 2;
    assert_eq!(buf[(card_x + 63, y)].bg, theme.palette.surface0, "64 wide");
    assert_eq!(buf[(card_x + 64, y)].bg, theme.app_bg, "capped at 64");

    let buf = buffer_at(&mut d, 85, 60);
    let (x, y) = at(&buf, "WEB-101");
    let (x2, y2) = at(&buf, "WEB-102");
    assert_eq!(
        (x2, y2),
        (x + 41, y),
        "2-up, 40 wide, 1-cell gap:\n{}",
        dump(&buf)
    );
    assert_eq!(buf[(x - 2 + 40, y)].bg, theme.app_bg, "the gap");
    let (x3, y3) = at(&buf, "WEB-103");
    assert_eq!((x3, y3), (x, y + 7), "{}", dump(&buf));
    // Alpha holds three cards: two rows of 6-row cards before the Beta line.
    let (_, alpha) = at(&buf, "ALPHA");
    let (_, beta) = at(&buf, "BETA");
    assert_eq!(beta, alpha + 2 + 2 * 7, "{}", dump(&buf));
}

#[test]
fn a_click_on_the_second_card_of_a_two_up_row_opens_it() {
    let mut d = one_column_board();
    let buf = buffer_at(&mut d, 85, 60);
    let (x, y) = at(&buf, "WEB-102");
    d.handle(left_down(x + 3, y + 3));
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert_eq!(
        d.app.linear.as_ref().unwrap().detail.as_deref(),
        Some("WEB-102")
    );
}

#[test]
fn hidden_columns_turn_the_outer_gaps_into_arrow_buttons_with_counts() {
    let theme = gruvbox();
    let mut d = tabs_driver();
    let buf = buffer_at(&mut d, 96, 40);
    assert!(find(&buf, "←").is_none(), "{}", dump(&buf));
    let (x, y) = at(&buf, "→");
    assert_eq!(x, 94, "in the right gap");
    assert_eq!(slice(&buf, y + 1, 94, 2).trim(), "2", "two hidden");
    assert_eq!(buf[(x, y)].bg, theme.app_bg, "flush until selected");
    assert!(
        find(&buf, "‹").is_none() && find(&buf, "›").is_none(),
        "no pager row:\n{}",
        dump(&buf)
    );
}

#[test]
fn right_past_the_last_column_selects_the_button_and_again_pages_one_column() {
    let theme = gruvbox();
    let mut d = tabs_driver();
    render_at(&mut d, 96, 40);
    press(&mut d, KeyCode::Right);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-105"));
    press(&mut d, KeyCode::Right);
    assert!(on_pager(&d, Side::Right), "{:?}", page_focus(&d));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-105"));
    let buf = buffer_at(&mut d, 96, 40);
    let (x, y) = at(&buf, "→");
    assert_eq!(buf[(x, y)].bg, theme.palette.panel_bg, "selected");
    press(&mut d, KeyCode::Right);
    assert!(on_pager(&d, Side::Right));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-105"));
    let frame = render_at(&mut d, 96, 40);
    assert!(column_tops(&frame).contains("IN PROGRESS  2"), "{frame}");
    assert!(column_tops(&frame).contains("IN REVIEW  1"), "{frame}");
    assert!(!column_tops(&frame).contains("TODO"), "{frame}");
    let buf = buffer_at(&mut d, 96, 40);
    let (lx, ly) = at(&buf, "←");
    assert_eq!(lx, 1);
    assert_eq!(slice(&buf, ly + 1, 0, 2).trim(), "1");
}

#[test]
fn paging_to_the_end_lands_on_the_edge_column() {
    let mut d = tabs_driver();
    render_at(&mut d, 96, 40);
    for _ in 0..3 {
        press(&mut d, KeyCode::Right);
    }
    assert!(on_pager(&d, Side::Right));
    press(&mut d, KeyCode::Right);
    assert!(
        matches!(page_focus(&d), Focus::Card(_)),
        "{:?}",
        page_focus(&d)
    );
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-108"));
    let frame = render_at(&mut d, 96, 40);
    assert!(column_tops(&frame).contains("DONE  1"), "{frame}");
    assert!(column_tops(&frame).contains("IN REVIEW  1"), "{frame}");
}

#[test]
fn left_on_the_right_button_steps_onto_the_column_beside_it() {
    let mut d = tabs_driver();
    render_at(&mut d, 96, 40);
    for _ in 0..3 {
        press(&mut d, KeyCode::Right);
    }
    press(&mut d, KeyCode::Left);
    assert!(
        matches!(page_focus(&d), Focus::Card(_)),
        "{:?}",
        page_focus(&d)
    );
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-107"));
    let frame = render_at(&mut d, 96, 40);
    assert!(column_tops(&frame).contains("IN PROGRESS  2"), "{frame}");
}

#[test]
fn enter_on_a_button_returns_to_the_selected_issue_and_pages_back_to_it() {
    let mut d = tabs_driver();
    render_at(&mut d, 48, 40);
    press(&mut d, KeyCode::Right);
    assert!(on_pager(&d, Side::Right), "{:?}", page_focus(&d));
    press(&mut d, KeyCode::Right);
    let frame = render_at(&mut d, 48, 40);
    assert!(column_tops(&frame).contains("IN PROGRESS"), "{frame}");
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
    press(&mut d, KeyCode::Enter);
    assert_eq!(d.app.screen, Screen::LinearBoard, "Enter did not open it");
    assert_eq!(page_focus(&d), Focus::Card(0));
    let frame = render_at(&mut d, 48, 40);
    assert!(column_tops(&frame).contains("TODO"), "{frame}");
}

#[test]
fn a_sideways_scroll_pages_and_selects_the_matching_button() {
    let mut d = tabs_driver();
    render_at(&mut d, 96, 40);
    d.handle(board_tui::testkit::mouse(
        MouseEventKind::ScrollRight,
        40,
        20,
    ));
    assert!(on_pager(&d, Side::Right), "{:?}", page_focus(&d));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
    let frame = render_at(&mut d, 96, 40);
    assert!(column_tops(&frame).contains("IN PROGRESS"), "{frame}");
    assert!(!column_tops(&frame).contains("TODO"), "{frame}");
    d.handle(board_tui::testkit::mouse(
        MouseEventKind::ScrollRight,
        40,
        20,
    ));
    assert!(on_pager(&d, Side::Left), "the right button is gone");
    d.handle(shift_wheel(MouseEventKind::ScrollUp, 40, 20));
    assert!(on_pager(&d, Side::Left));
    let frame = render_at(&mut d, 96, 40);
    assert!(column_tops(&frame).contains("IN PROGRESS"), "{frame}");
    d.handle(shift_wheel(MouseEventKind::ScrollDown, 40, 20));
    let frame = render_at(&mut d, 96, 40);
    assert!(column_tops(&frame).contains("DONE"), "{frame}");
}

#[test]
fn angle_brackets_page_like_a_sideways_scroll() {
    let mut d = tabs_driver();
    render_at(&mut d, 96, 40);
    press(&mut d, KeyCode::Char('>'));
    assert!(on_pager(&d, Side::Right));
    press(&mut d, KeyCode::Char('<'));
    assert!(on_pager(&d, Side::Right), "back at the start");
    let frame = render_at(&mut d, 96, 40);
    assert!(column_tops(&frame).contains("TODO"), "{frame}");
}

#[test]
fn a_resize_that_fits_every_column_moves_button_focus_to_the_edge_column() {
    let mut d = tabs_driver();
    render_at(&mut d, 96, 40);
    press(&mut d, KeyCode::Right);
    press(&mut d, KeyCode::Right);
    assert!(on_pager(&d, Side::Right));
    let buf = resize(&mut d, 200, 40);
    assert!(
        matches!(page_focus(&d), Focus::Card(_)),
        "{:?}",
        page_focus(&d)
    );
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-108"));
    assert!(find(&buf, "→").is_none(), "{}", dump(&buf));
}

#[test]
fn narrowing_the_pane_shifts_the_page_to_keep_the_selected_column() {
    let mut d = tabs_driver();
    render_at(&mut d, 200, 40);
    for _ in 0..3 {
        press(&mut d, KeyCode::Right);
    }
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-108"));
    resize(&mut d, 96, 40);
    let frame = render_at(&mut d, 96, 40);
    assert!(column_tops(&frame).contains("DONE  1"), "{frame}");
    assert!(column_tops(&frame).contains("IN REVIEW  1"), "{frame}");
    press(&mut d, KeyCode::Left);
    let frame = render_at(&mut d, 96, 40);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-107"));
    assert!(
        column_tops(&frame).contains("DONE  1"),
        "the page moves only when the selection leaves it:\n{frame}"
    );
}

#[test]
fn a_snapshot_that_drops_the_hidden_columns_moves_button_focus_to_the_edge_column() {
    let mut d = tabs_driver();
    render_at(&mut d, 96, 40);
    press(&mut d, KeyCode::Right);
    press(&mut d, KeyCode::Right);
    assert!(on_pager(&d, Side::Right));
    let mut snapshot = tabs_lanes();
    edit_first_tab(&mut snapshot, |groups| groups.truncate(2));
    arrive(&mut d, snapshot);
    assert!(
        matches!(page_focus(&d), Focus::Card(_)),
        "{:?}",
        page_focus(&d)
    );
    assert_eq!(selected_column(&d), "st-prog");
}

// -- the PR row from the daemon's gh data, through the snapshot ---------------

fn pr_row(pr: LinearCardPr, gh: Option<(&str, Option<&str>)>) -> String {
    let mut snapshot = tabs_lanes();
    snapshot.issues.get_mut("WEB-101").unwrap().pr = Some(pr);
    snapshot.gh = gh.map(|(status, message)| LinearGhSource {
        status: status.into(),
        message: message.map(str::to_string),
    });
    let mut d = driver_for(snapshot);
    let buf = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&buf, "WEB-101");
    slice(&buf, y + 5, x, 42)
}

fn linear_pr() -> LinearCardPr {
    LinearCardPr {
        repo: "acme/web".into(),
        number: 2132,
        url: "https://github.com/acme/web/pull/2132".into(),
        status: Some("open".into()),
        ..Default::default()
    }
}

fn read_pr() -> LinearCardPr {
    LinearCardPr {
        state: Some("OPEN".into()),
        ci: Some(LinearPrCi {
            state: "fail".into(),
            passed: 4,
            failed: 1,
            ..Default::default()
        }),
        additions: Some(48),
        deletions: Some(12),
        preview: Some("https://web-2132.preview.example".into()),
        ..linear_pr()
    }
}

#[test]
fn a_pr_gh_has_read_shows_its_preview_diff_and_ci() {
    let row = pr_row(read_pr(), Some(("ok", None)));
    assert!(row.contains("#2132"), "{row:?}");
    assert!(row.contains("Preview"), "{row:?}");
    assert!(row.contains("+48") && row.contains("−12"), "{row:?}");
    assert!(row.contains("\u{f057} CI"), "failing CI: {row:?}");
    assert!(!row.contains('–') && !row.contains('…'), "{row:?}");
}

#[test]
fn a_pr_gh_has_not_read_yet_shows_loading() {
    for status in ["ok", "unknown"] {
        let row = pr_row(linear_pr(), Some((status, None)));
        assert!(row.contains("#2132 open"), "{status}: {row:?}");
        assert!(row.trim_end().ends_with('…'), "{status}: {row:?}");
    }
}

#[test]
fn a_pr_while_gh_is_unavailable_says_so() {
    let row = pr_row(
        linear_pr(),
        Some(("unavailable", Some("gh is not logged in"))),
    );
    assert!(row.contains("#2132 open"), "{row:?}");
    assert!(row.contains("gh unavailable"), "{row:?}");
    let failed = LinearCardPr {
        gh_error: Some("HTTP 404".into()),
        ..linear_pr()
    };
    let row = pr_row(failed, Some(("ok", None)));
    assert!(
        row.contains("gh unavailable"),
        "a PR gh failed to read: {row:?}"
    );
}

#[test]
fn a_pr_from_a_daemon_without_gh_shows_linear_data_only() {
    let row = pr_row(linear_pr(), None);
    assert!(row.contains("#2132 open"), "{row:?}");
    assert_eq!(row.matches('–').count(), 3, "{row:?}");
}

// -- content-sized cards and compact density (`docs/board-design.md` §3) -----

const TWO_ROW_DESCRIPTION: &str = "The save dialog opens in Downloads instead of the last folder.";

fn assignee(name: &str) -> Option<board_core::protocol::LinearAssignee> {
    Some(board_core::protocol::LinearAssignee {
        id: None,
        name: Some(name.into()),
    })
}

/// WEB-101 with every row filled, WEB-102 with a 2-row description and only
/// an assignee, WEB-103 with a one-row title and nothing else.
fn sized_cards() -> LinearSnapshot {
    let mut snapshot = described(tabs_lanes());
    let full = snapshot.issues.get_mut("WEB-101").unwrap();
    full.assignee = assignee("Example User");
    full.pr = Some(linear_pr());
    let partial = snapshot.issues.get_mut("WEB-102").unwrap();
    partial.description = Some(TWO_ROW_DESCRIPTION.into());
    partial.assignee = assignee("Example User");
    let bare = snapshot.issues.get_mut("WEB-103").unwrap();
    bare.title = "Blurry icon".into();
    bare.description = None;
    snapshot
}

/// The rows of the card whose header shows `id`: the run of rows around it
/// whose card-edge cell is not the board background.
fn card_span(buf: &Buffer, id: &str) -> (u16, u16) {
    let (x, y) = at(buf, id);
    let edge = x - 2;
    let theme = gruvbox();
    let on_card = |y: u16| buf[(edge, y)].bg != theme.app_bg;
    let mut top = y;
    while top > 0 && on_card(top - 1) {
        top -= 1;
    }
    let mut bottom = y;
    while bottom + 1 < buf.area.height && on_card(bottom + 1) {
        bottom += 1;
    }
    (top, bottom + 1 - top)
}

fn card_height(buf: &Buffer, id: &str) -> u16 {
    card_span(buf, id).1
}

fn compact(d: &mut Driver) {
    press(d, KeyCode::Char('z'));
}

#[test]
fn default_cards_drop_only_the_rows_their_content_lacks() {
    let mut d = driver_for(sized_cards());
    let buf = buffer_at(&mut d, 200, 80);
    assert_eq!(card_height(&buf, "WEB-101"), 13, "full:\n{}", dump(&buf));
    assert_eq!(
        card_height(&buf, "WEB-102"),
        11,
        "2-row description, one meta row"
    );
    assert_eq!(
        card_height(&buf, "WEB-103"),
        5,
        "title only:\n{}",
        dump(&buf)
    );
    let (x, y) = at(&buf, "WEB-103");
    assert_eq!(slice(&buf, y + 2, x, 20).trim(), "Blurry icon");
    assert_eq!(slice(&buf, y + 3, x, 20).trim(), "", "bottom padding");
}

#[test]
fn compact_cards_drop_padding_rules_and_blank_rows() {
    let mut d = driver_for(sized_cards());
    compact(&mut d);
    let buf = buffer_at(&mut d, 200, 80);
    assert_eq!(card_height(&buf, "WEB-101"), 8, "full:\n{}", dump(&buf));
    assert_eq!(
        card_height(&buf, "WEB-103"),
        2,
        "title only:\n{}",
        dump(&buf)
    );
    let (top, h) = card_span(&buf, "WEB-101");
    let (x, y) = at(&buf, "WEB-101");
    assert_eq!(top, y, "the header is the first row");
    for dy in 0..h {
        let text = slice(&buf, top + dy, x, 40);
        assert!(
            !text.contains("──") && !text.contains("─ ─"),
            "a rule in compact: {text:?}"
        );
    }
    assert!(slice(&buf, y + 1, x, 40).starts_with("Example issue"));
}

#[test]
fn selecting_a_card_changes_no_height_and_moves_no_text_in_either_density() {
    for dense in [false, true] {
        let mut d = driver_for(sized_cards());
        if dense {
            compact(&mut d);
        }
        let before = buffer_at(&mut d, 200, 80);
        press(&mut d, KeyCode::Down);
        assert_eq!(selected_id(&d).as_deref(), Some("WEB-102"));
        let after = buffer_at(&mut d, 200, 80);
        for id in ["WEB-101", "WEB-102", "WEB-103"] {
            assert_eq!(
                at(&before, id),
                at(&after, id),
                "{id} moved (compact {dense})"
            );
            assert_eq!(
                card_span(&before, id),
                card_span(&after, id),
                "{id} changed height (compact {dense})"
            );
        }
    }
}

#[test]
fn lanes_align_when_columns_hold_cards_of_different_heights() {
    for (dense, head, gap_rows) in [
        (false, 2, 13 + 1 + 11 + 1 + 5 + 1),
        (true, 1, 8 + 1 + 6 + 1 + 2 + 1),
    ] {
        let mut d = driver_for(sized_cards());
        if dense {
            compact(&mut d);
        }
        let buf = buffer_at(&mut d, 200, 80);
        let (_, alpha) = at(&buf, "ALPHA");
        let (_, beta) = at(&buf, "BETA");
        assert_eq!(
            beta,
            alpha + head + gap_rows,
            "compact {dense}:\n{}",
            dump(&buf)
        );
        let (todo, _) = card_span(&buf, "WEB-104");
        let (progress, _) = card_span(&buf, "WEB-106");
        let (review, _) = card_span(&buf, "WEB-107");
        assert_eq!((todo, progress), (review, review), "compact {dense}");
        assert_eq!(review, beta + head, "compact {dense}");
    }
}

#[test]
fn a_two_up_row_is_as_tall_as_its_taller_card_and_clicks_land_on_the_right_card() {
    let mut snapshot = sized_cards();
    edit_first_tab(&mut snapshot, |groups| groups.truncate(1));
    snapshot.issues.get_mut("WEB-102").unwrap().description = None;
    snapshot.issues.get_mut("WEB-102").unwrap().assignee = None;
    let mut d = driver_for(snapshot);
    let buf = buffer_at(&mut d, 85, 60);
    let (top, h) = card_span(&buf, "WEB-101");
    assert_eq!(h, 13);
    assert_eq!(card_height(&buf, "WEB-102"), 6, "{}", dump(&buf));
    let (third, _) = card_span(&buf, "WEB-103");
    assert_eq!(third, top + 13 + 1, "{}", dump(&buf));
    let (x2, y2) = at(&buf, "WEB-102");
    d.handle(left_down(x2 + 3, y2 + 8));
    assert_eq!(
        d.app.screen,
        Screen::LinearBoard,
        "below the shorter card is no card"
    );
    let (x3, y3) = at(&buf, "WEB-103");
    d.handle(left_down(x3 + 3, y3));
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert_eq!(
        d.app.linear.as_ref().unwrap().detail.as_deref(),
        Some("WEB-103")
    );
}

#[test]
fn z_toggles_density_keeping_the_selection_in_view() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Down);
    press(&mut d, KeyCode::Down);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-103"));
    let roomy = buffer_at(&mut d, 140, 22);
    let h = card_height(&roomy, "WEB-103");
    compact(&mut d);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-103"));
    let dense = buffer_at(&mut d, 140, 22);
    assert!(card_height(&dense, "WEB-103") < h, "{}", dump(&dense));
    compact(&mut d);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-103"));
    let back = buffer_at(&mut d, 140, 22);
    let (top, h) = card_span(&back, "WEB-103");
    assert!(top + h <= 22, "{}", dump(&back));
    assert_eq!(
        card_height(&back, "WEB-103"),
        card_height(&roomy, "WEB-103")
    );
}

#[test]
fn z_in_the_filter_is_typed_as_text() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('/'));
    press(&mut d, KeyCode::Char('z'));
    press(&mut d, KeyCode::Char('z'));
    assert_eq!(d.app.linear.as_ref().unwrap().filter, "zz");
    let buf = buffer_at(&mut d, 200, 40);
    assert!(find(&buf, "no matching issues").is_some(), "{}", dump(&buf));
}

#[test]
fn at_fifty_rows_a_compact_column_shows_more_cards() {
    let mut full = described(tabs_lanes());
    for issue in full.issues.values_mut() {
        issue.assignee = assignee("Example User");
        issue.pr = Some(linear_pr());
    }
    let shown = |dense: bool| {
        let mut d = driver_for(full.clone());
        if dense {
            compact(&mut d);
        }
        let frame = render_at(&mut d, 60, 50);
        (101..=108)
            .filter(|n| frame.contains(&format!("WEB-{n}")))
            .count()
    };
    assert!(
        shown(true) > shown(false),
        "{} vs {}",
        shown(true),
        shown(false)
    );
}

const FILTER_ICON: &str = "\u{f0232}";

fn row_text(buf: &Buffer, y: u16) -> String {
    cells(buf, y).concat()
}

#[test]
fn the_filter_is_a_card_across_the_board_with_one_blank_row_either_side() {
    let theme = gruvbox();
    let mut d = tabs_driver();
    let before = at(&buffer_at(&mut d, 200, 80), "WEB-101").1;
    press(&mut d, KeyCode::Char('/'));
    let buf = buffer_at(&mut d, 200, 80);
    let (_, y) = at(&buf, FILTER_ICON);
    assert!(row_text(&buf, y - 2).contains("TODO"), "{}", dump(&buf));
    assert_eq!(row_text(&buf, y - 1).trim(), "", "one blank row above");
    assert_eq!(row_text(&buf, y + 1).trim(), "", "one blank row below");
    assert!(row_text(&buf, y + 2).contains("ALPHA"), "{}", dump(&buf));
    let w = buf.area.width;
    assert_eq!(
        buf[(2, y)].bg,
        theme.palette.surface0,
        "starts at the inset"
    );
    assert_eq!(
        buf[(w - 3, y)].bg,
        theme.palette.surface0,
        "ends at the inset"
    );
    assert_ne!(buf[(1, y)].bg, theme.palette.surface0);
    assert_ne!(buf[(w - 2, y)].bg, theme.palette.surface0);
    assert_eq!(slice(&buf, y, 2, 4), format!("  {FILTER_ICON} "));
    assert!(
        slice(&buf, y, 2, w - 4).ends_with("8 matches  "),
        "the count is right-aligned: {:?}",
        slice(&buf, y, 2, w - 4)
    );
    assert_eq!(
        at(&buf, "WEB-101").1,
        before + 2,
        "the board moves down by the filter's rows"
    );
}

#[test]
fn an_unfocused_filter_keeps_its_card_on_the_card_surface_with_the_count() {
    let theme = gruvbox();
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('/'));
    for c in "export".chars() {
        press(&mut d, KeyCode::Char(c));
    }
    press(&mut d, KeyCode::Enter);
    let buf = buffer_at(&mut d, 200, 80);
    let (x, y) = at(&buf, FILTER_ICON);
    assert_eq!(buf[(x, y)].bg, theme.palette.panel_bg);
    assert!(row_text(&buf, y).contains("export"), "{}", dump(&buf));
    assert!(
        row_text(&buf, y).trim_end().ends_with("1 match"),
        "{}",
        dump(&buf)
    );
}
