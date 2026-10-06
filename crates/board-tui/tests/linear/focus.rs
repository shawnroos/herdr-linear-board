//! The board's focus model and keys: one test per transition of the board
//! focus state diagram, on the two-lane `tabs_lanes` board (Todo: lane Alpha
//! WEB-101..103, lane Beta WEB-104; In Progress: Alpha WEB-105, Beta WEB-106;
//! In Review: Beta WEB-107; Done: Alpha WEB-108).

use super::*;

fn focus(d: &Driver) -> Focus {
    d.app.linear.as_ref().unwrap().focus.clone()
}

fn type_text(d: &mut Driver, text: &str) {
    for c in text.chars() {
        press(d, KeyCode::Char(c));
    }
}

fn line(key: &str) -> Focus {
    Focus::LaneLine(key.into())
}

#[test]
fn q_on_the_board_does_nothing() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('q'));
    assert!(!d.app.should_quit);
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
}

#[test]
fn ctrl_q_quits_from_the_board() {
    let mut d = tabs_driver();
    d.handle(ctrl_q());
    assert!(d.app.should_quit);
}

#[test]
fn ctrl_q_quits_from_the_filter_and_q_there_is_text() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('/'));
    press(&mut d, KeyCode::Char('q'));
    assert!(!d.app.should_quit);
    assert_eq!(d.app.linear.as_ref().unwrap().filter, "q");
    d.handle(ctrl_q());
    assert!(d.app.should_quit);
}

#[test]
fn q_on_the_issue_view_does_nothing_and_ctrl_q_quits() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Enter);
    assert_eq!(d.app.screen, Screen::LinearDetail);
    press(&mut d, KeyCode::Char('q'));
    assert!(!d.app.should_quit);
    assert_eq!(d.app.screen, Screen::LinearDetail);
    d.handle(ctrl_q());
    assert!(d.app.should_quit);
}

#[test]
fn esc_on_a_card_leaves_nothing_selected_and_does_not_quit() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Esc);
    assert!(!d.app.should_quit);
    assert_eq!(selected_id(&d), None);
    assert_eq!(focus(&d), Focus::None);
    press(&mut d, KeyCode::Esc);
    assert!(!d.app.should_quit, "Esc with nothing selected does nothing");
    assert_eq!(d.app.screen, Screen::LinearBoard);
}

#[test]
fn down_after_esc_selects_the_last_card_again() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('j'));
    press(&mut d, KeyCode::Esc);
    press(&mut d, KeyCode::Down);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-102"));
}

#[test]
fn tab_after_esc_selects_in_the_last_column_rather_than_moving() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('l'));
    press(&mut d, KeyCode::Esc);
    press(&mut d, KeyCode::Tab);
    assert_eq!(selected_column(&d), "st-prog");
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-105"));
}

#[test]
fn return_after_esc_from_a_lane_line_selects_and_opens_nothing() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('k'));
    assert_eq!(focus(&d), line("ln-alpha"));
    press(&mut d, KeyCode::Esc);
    assert_eq!(focus(&d), Focus::None);
    press(&mut d, KeyCode::Enter);
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
}

#[test]
fn esc_from_a_filter_holding_text_clears_it_and_returns_to_the_last_card() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('j'));
    press(&mut d, KeyCode::Char('/'));
    type_text(&mut d, "web-102");
    press(&mut d, KeyCode::Esc);
    assert_eq!(d.app.linear.as_ref().unwrap().filter, "");
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-102"));
    let frame = without_bar(&render_at(&mut d, 140, 40), 1);
    assert!(!frame.contains("\u{f0232}"), "the filter closes:\n{frame}");
}

#[test]
fn esc_from_an_empty_filter_leaves_nothing_selected() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('/'));
    press(&mut d, KeyCode::Esc);
    assert_eq!(focus(&d), Focus::None);
    press(&mut d, KeyCode::Down);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
}

#[test]
fn up_from_the_top_card_selects_the_lane_line_then_opens_the_filter() {
    let mut d = tabs_driver();
    let frame = without_bar(&render_at(&mut d, 140, 40), 1);
    assert!(
        !frame.contains("\u{f0232}"),
        "the filter is hidden:\n{frame}"
    );
    press(&mut d, KeyCode::Char('k'));
    assert_eq!(selected_id(&d), None);
    assert_eq!(focus(&d), line("ln-alpha"));
    press(&mut d, KeyCode::Up);
    assert_eq!(focus(&d), Focus::Filter);
    let frame = render_at(&mut d, 140, 40);
    assert!(frame.contains("\u{f0232} ▏"), "{frame}");
    press(&mut d, KeyCode::Down);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
}

#[test]
fn down_walks_lane_lines_and_cards_in_drawing_order() {
    let mut d = tabs_driver();
    let mut seen = vec![];
    for _ in 0..5 {
        press(&mut d, KeyCode::Char('j'));
        seen.push(
            selected_id(&d)
                .or_else(|| {
                    d.app
                        .linear
                        .as_ref()
                        .unwrap()
                        .selected_lane_line()
                        .map(str::to_string)
                })
                .unwrap(),
        );
    }
    assert_eq!(
        seen,
        ["WEB-102", "WEB-103", "ln-beta", "WEB-104", "WEB-104"]
    );
}

#[test]
fn slash_opens_the_filter_from_a_card_in_another_column_and_from_a_lane_line() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('l'));
    press(&mut d, KeyCode::Char('/'));
    assert_eq!(focus(&d), Focus::Filter);
    press(&mut d, KeyCode::Esc);
    press(&mut d, KeyCode::Down);
    press(&mut d, KeyCode::Char('k'));
    assert_eq!(focus(&d), line("ln-alpha"));
    press(&mut d, KeyCode::Char('/'));
    assert_eq!(focus(&d), Focus::Filter);
}

#[test]
fn the_filter_hides_cards_it_does_not_match_and_return_selects_the_first_match() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('/'));
    type_text(&mut d, "WEB-103");
    press(&mut d, KeyCode::Enter);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-103"));
    let frame = render_at(&mut d, 140, 40);
    assert!(
        !frame.contains("WEB-101") && !frame.contains("WEB-104"),
        "{frame}"
    );
    assert!(
        !frame.contains("Beta"),
        "a lane with no match is hidden:\n{frame}"
    );
}

#[test]
fn a_filter_that_matches_nothing_stays_selected_and_clearing_it_restores_the_last_card() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('j'));
    press(&mut d, KeyCode::Char('/'));
    type_text(&mut d, "zzz");
    for key in [KeyCode::Enter, KeyCode::Tab, KeyCode::Down] {
        press(&mut d, key);
        assert_eq!(focus(&d), Focus::Filter, "{key:?}");
    }
    for _ in 0..3 {
        press(&mut d, KeyCode::Backspace);
    }
    press(&mut d, KeyCode::Enter);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-102"));
}

#[test]
fn a_column_the_filter_empties_selects_the_lane_line() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('/'));
    type_text(&mut d, "WEB-101");
    press(&mut d, KeyCode::Enter);
    press(&mut d, KeyCode::Char('l'));
    assert_eq!(selected_column(&d), "st-prog");
    assert_eq!(focus(&d), line("ln-alpha"));
}

#[test]
fn return_on_a_lane_line_folds_it_in_every_column_and_again_unfolds_it() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('k'));
    press(&mut d, KeyCode::Enter);
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert!(d.app.linear.as_ref().unwrap().folded("ln-alpha"));
    assert_eq!(focus(&d), line("ln-alpha"), "the line stays selected");
    let frame = render_at(&mut d, 140, 40);
    for hidden in ["WEB-101", "WEB-103", "WEB-105"] {
        assert!(!frame.contains(hidden), "{hidden}:\n{frame}");
    }
    assert!(
        frame.contains("─ ALPHA ─") && frame.contains("5 \u{f0232} ─"),
        "the folded lane keeps its line and count:\n{frame}"
    );
    assert!(
        frame.contains("WEB-104") && frame.contains("WEB-106"),
        "{frame}"
    );
    press(&mut d, KeyCode::Char('j'));
    assert_eq!(
        focus(&d),
        line("ln-beta"),
        "a folded lane's cards are skipped"
    );
    press(&mut d, KeyCode::Char('k'));
    press(&mut d, KeyCode::Enter);
    assert!(!d.app.linear.as_ref().unwrap().folded("ln-alpha"));
    assert!(render_at(&mut d, 140, 40).contains("WEB-105"));
}

#[test]
fn right_on_a_lane_line_focuses_the_next_column_with_the_line_still_selected() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('k'));
    press(&mut d, KeyCode::Right);
    assert_eq!(selected_column(&d), "st-prog");
    assert_eq!(focus(&d), line("ln-alpha"));
    press(&mut d, KeyCode::Tab);
    assert_eq!(selected_column(&d), "st-review");
    assert_eq!(focus(&d), line("ln-alpha"));
    let frame = render_at(&mut d, 140, 40);
    assert!(
        frame.contains("─ ALPHA ─"),
        "the selected line is drawn across the board, In Review included:\n{frame}"
    );
}

#[test]
fn a_card_moved_into_a_folded_lane_selects_that_lanes_line() {
    let mut d = tabs_driver();
    // Fold Beta, then come back up to WEB-103.
    for _ in 0..3 {
        press(&mut d, KeyCode::Char('j'));
    }
    press(&mut d, KeyCode::Enter);
    press(&mut d, KeyCode::Char('k'));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-103"));
    let mut snapshot = tabs_lanes();
    edit_first_tab(&mut snapshot, |groups| {
        groups[0].lanes[0].issues.retain(|id| id != "WEB-103");
        groups[0].lanes[1].issues.push("WEB-103".into());
    });
    arrive(&mut d, snapshot);
    assert_eq!(focus(&d), line("ln-beta"));
    assert_eq!(selected_column(&d), "st-todo");
}

#[test]
fn a_fold_survives_a_refresh_and_a_vanished_lane_moves_to_the_next_line_and_drops_its_fold() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('k'));
    press(&mut d, KeyCode::Enter);
    arrive(&mut d, tabs_lanes());
    assert!(d.app.linear.as_ref().unwrap().folded("ln-alpha"));
    assert_eq!(focus(&d), line("ln-alpha"));
    let mut snapshot = tabs_lanes();
    edit_first_tab(&mut snapshot, |groups| {
        for group in groups.iter_mut() {
            let alpha: Vec<String> = group
                .lanes
                .iter()
                .filter(|lane| lane.key == "ln-alpha")
                .flat_map(|lane| lane.issues.clone())
                .collect();
            group.issues.retain(|id| !alpha.contains(id));
            group.lanes.retain(|lane| lane.key != "ln-alpha");
        }
    });
    arrive(&mut d, snapshot);
    assert_eq!(focus(&d), line("ln-beta"));
    assert!(d.app.linear.as_ref().unwrap().folds.is_empty());
}

#[test]
fn the_wheel_never_opens_the_filter() {
    let mut d = tabs_driver();
    render_at(&mut d, W, H);
    for _ in 0..3 {
        d.handle(board_tui::testkit::mouse(MouseEventKind::ScrollUp, 10, 10));
    }
    assert_eq!(focus(&d), line("ln-alpha"));
}

#[test]
fn folds_are_kept_per_tab() {
    let mut d = tabs_driver();
    press(&mut d, KeyCode::Char('k'));
    press(&mut d, KeyCode::Enter);
    press(&mut d, KeyCode::Char(']'));
    let state = d.app.linear.as_ref().unwrap();
    assert!(!state.folded("ln-alpha") && !state.folded("ln-gamma"));
    assert_eq!(focus(&d), line("ln-gamma"), "a lane line carries over");
    press(&mut d, KeyCode::Enter);
    assert!(d.app.linear.as_ref().unwrap().folded("ln-gamma"));
    press(&mut d, KeyCode::Char('['));
    let state = d.app.linear.as_ref().unwrap();
    assert!(state.folded("ln-alpha") && !state.folded("ln-gamma"));
    assert_eq!(focus(&d), line("ln-alpha"));
    press(&mut d, KeyCode::Char(']'));
    assert!(d.app.linear.as_ref().unwrap().folded("ln-gamma"));
    assert_eq!(focus(&d), line("ln-gamma"));
}
