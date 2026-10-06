//! The bottom bar: global controls, a tip for the selection and board-wide
//! alerts, focused with `?`, on the two-lane `tabs_lanes` board.

use super::marks::{board, params};
use super::*;

use board_core::protocol::{LinearCardPr, LinearGhSource, LinearPrCi};
use board_tui::app::BarItem;

fn focus(d: &Driver) -> Focus {
    d.app.linear.as_ref().unwrap().focus.clone()
}

fn bar(item: BarItem) -> Focus {
    Focus::Bar(item)
}

/// `?`, then → until `item` is selected; panics when the bar never reaches it.
fn bar_to(d: &mut Driver, item: BarItem) {
    press(d, KeyCode::Char('?'));
    for _ in 0..30 {
        if focus(d) == bar(item.clone()) {
            return;
        }
        press(d, KeyCode::Right);
    }
    panic!("the bar never reached {item:?}; stopped on {:?}", focus(d));
}

/// Every item → visits, from `?` until it comes back round to the first.
fn bar_walk(d: &mut Driver) -> Vec<BarItem> {
    press(d, KeyCode::Char('?'));
    let mut seen = vec![];
    for _ in 0..40 {
        match focus(d) {
            Focus::Bar(item) if seen.first() == Some(&item) => return seen,
            Focus::Bar(item) => seen.push(item),
            other => panic!("left the bar for {other:?}"),
        }
        press(d, KeyCode::Right);
    }
    panic!("the bar never wrapped: {seen:?}");
}

fn failing_pr(snapshot: &mut LinearSnapshot, id: &str, number: u64) {
    snapshot.issues.get_mut(id).unwrap().pr = Some(LinearCardPr {
        repo: "acme/web".into(),
        number,
        url: format!("https://github.com/acme/web/pull/{number}"),
        ci: Some(LinearPrCi {
            state: "fail".into(),
            failed: 1,
            ..LinearPrCi::default()
        }),
        ..LinearCardPr::default()
    });
}

fn gh(status: &str) -> Option<LinearGhSource> {
    Some(LinearGhSource {
        status: status.into(),
        message: Some("gh is not installed".into()),
    })
}

/// The bar's rows: the last `n` rows of a frame, trailing blanks trimmed.
fn bar_rows(frame: &str, n: usize) -> Vec<String> {
    let rows: Vec<String> = frame
        .lines()
        .map(|row| backend_row(row).trim_end().to_string())
        .collect();
    rows[rows.len() - n..].to_vec()
}

#[test]
fn question_mark_focuses_the_bar_on_its_first_item_and_arrows_move_along_it() {
    let shared = SharedFake::quiet(tabs_lanes());
    let (mut d, _) = board(&shared);
    press(&mut d, KeyCode::Char('j'));
    press(&mut d, KeyCode::Char('?'));
    assert_eq!(focus(&d), bar(BarItem::Keys));
    press(&mut d, KeyCode::Right);
    assert_eq!(focus(&d), bar(BarItem::Filter));
    press(&mut d, KeyCode::Left);
    press(&mut d, KeyCode::Left);
    assert_eq!(focus(&d), bar(BarItem::Quit), "← from the first wraps");
    press(&mut d, KeyCode::Up);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-102"), "↑ back");
    press(&mut d, KeyCode::Char('?'));
    press(&mut d, KeyCode::Right);
    press(&mut d, KeyCode::Char('?'));
    assert_eq!(
        focus(&d),
        bar(BarItem::Keys),
        "it always opens on the first"
    );
    press(&mut d, KeyCode::Esc);
    assert_eq!(focus(&d), Focus::None);
}

#[test]
fn keys_opens_the_help_sheet_and_esc_returns_to_the_bar() {
    let shared = SharedFake::quiet(tabs_lanes());
    let (mut d, _) = board(&shared);
    press(&mut d, KeyCode::Char('?'));
    press(&mut d, KeyCode::Enter);
    assert_eq!(d.app.screen, Screen::Help);
    press(&mut d, KeyCode::Esc);
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert_eq!(focus(&d), bar(BarItem::Keys));
}

#[test]
fn return_on_needs_you_selects_its_card_unfolding_its_lane_and_paging_to_it() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-107", MarkKind::Attention, "look", "agent-a");
    let (mut d, _) = board(&shared);
    render_at(&mut d, 44, 40);
    press(&mut d, KeyCode::Up);
    assert_eq!(focus(&d), Focus::LaneLine("ln-alpha".into()));
    for _ in 0..4 {
        press(&mut d, KeyCode::Down);
    }
    assert_eq!(focus(&d), Focus::LaneLine("ln-beta".into()));
    press(&mut d, KeyCode::Enter);
    assert!(d.app.linear.as_ref().unwrap().folded("ln-beta"));
    bar_to(&mut d, BarItem::NeedsYou);
    press(&mut d, KeyCode::Enter);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-107"));
    assert_eq!(selected_column(&d), "st-review");
    assert!(!d.app.linear.as_ref().unwrap().folded("ln-beta"));
    let frame = render_at(&mut d, 44, 40);
    assert!(frame.contains("IN REVIEW"), "paged to it:\n{frame}");
}

#[test]
fn a_hidden_alert_card_the_filter_hides_is_still_reached() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-104", MarkKind::Question, "which?", "agent-a");
    let (mut d, _) = board(&shared);
    press(&mut d, KeyCode::Char('/'));
    for c in "WEB-101".chars() {
        press(&mut d, KeyCode::Char(c));
    }
    press(&mut d, KeyCode::Enter);
    bar_to(&mut d, BarItem::Question);
    press(&mut d, KeyCode::Enter);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-104"));
}

#[test]
fn a_on_a_request_alert_accepts_it_and_selects_its_card() {
    let shared = SharedFake::quiet(tabs_lanes());
    let id = shared.ask("WEB-202", "agent-b");
    let (mut d, log) = board(&shared);
    bar_to(&mut d, BarItem::Request(id));
    press(&mut d, KeyCode::Char('a'));
    let accepts = params(&log, "linear.show.accept");
    assert_eq!(accepts.len(), 1, "{:?}", methods(&log));
    assert_eq!(accepts[0]["id"], id);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-202"));
    assert_eq!(
        d.app.linear.as_ref().unwrap().active_tab().unwrap().key,
        "ms-backend"
    );
}

#[test]
fn x_on_a_request_alert_dismisses_it_and_moves_to_the_next_item() {
    let shared = SharedFake::quiet(tabs_lanes());
    let first = shared.ask("WEB-105", "agent-b");
    let second = shared.ask("WEB-106", "agent-c");
    let (mut d, log) = board(&shared);
    bar_to(&mut d, BarItem::Request(first));
    press(&mut d, KeyCode::Char('x'));
    let dismissals = params(&log, "linear.show.dismiss");
    assert_eq!(dismissals.len(), 1, "{:?}", methods(&log));
    assert_eq!(dismissals[0]["id"], first);
    assert_eq!(focus(&d), bar(BarItem::Request(second)));
    d.on_local_state_changed(local(&[(Some("wA"), false)]));
    assert_eq!(focus(&d), bar(BarItem::Request(second)));
}

#[test]
fn a_request_that_expires_while_selected_leaves_a_valid_item_and_a_then_refuses() {
    let shared = SharedFake::quiet(tabs_lanes());
    let first = shared.ask("WEB-105", "agent-b");
    let second = shared.ask("WEB-106", "agent-c");
    let (mut d, log) = board(&shared);
    bar_to(&mut d, BarItem::Request(first));
    shared.close(first);
    d.on_local_state_changed(local(&[(Some("wA"), false)]));
    assert_eq!(focus(&d), bar(BarItem::Request(second)), "clamped in place");
    log.lock().unwrap().clear();
    press(&mut d, KeyCode::Char('a'));
    assert_eq!(toast(&d), "request no longer pending");
    assert!(methods(&log).is_empty(), "{:?}", methods(&log));
    press(&mut d, KeyCode::Char('a'));
    assert_eq!(
        params(&log, "linear.show.accept")[0]["id"],
        second,
        "the refusal is spent"
    );
}

#[test]
fn a_and_x_on_an_item_that_is_not_a_request_say_so() {
    let shared = SharedFake::quiet(tabs_lanes());
    let (mut d, log) = board(&shared);
    press(&mut d, KeyCode::Char('?'));
    press(&mut d, KeyCode::Char('a'));
    assert_eq!(toast(&d), "nothing to accept");
    press(&mut d, KeyCode::Char('x'));
    assert_eq!(toast(&d), "nothing to dismiss");
    assert!(methods(&log).is_empty(), "{:?}", methods(&log));
    assert_eq!(focus(&d), bar(BarItem::Keys));
}

#[test]
fn alerts_follow_the_controls_in_order_requests_needs_you_questions_then_ci() {
    let mut snapshot = tabs_lanes();
    failing_pr(&mut snapshot, "WEB-103", 7);
    snapshot.gh = gh("ok");
    let shared = SharedFake::quiet(snapshot);
    shared.mark_kind("WEB-101", MarkKind::Question, "which?", "agent-a");
    shared.mark_kind("WEB-102", MarkKind::Attention, "look", "agent-a");
    shared.mark_kind("WEB-9999", MarkKind::Attention, "off the board", "agent-a");
    let id = shared.ask("WEB-105", "agent-b");
    let (mut d, _) = board(&shared);
    render_at(&mut d, 240, 40);
    assert_eq!(
        bar_walk(&mut d),
        vec![
            BarItem::Keys,
            BarItem::Filter,
            BarItem::Deselect,
            BarItem::Quit,
            BarItem::Request(id),
            BarItem::NeedsYou,
            BarItem::Question,
            BarItem::CiFailing("WEB-103".into()),
        ]
    );
    let frame = render_at(&mut d, 240, 40);
    let row = &bar_rows(&frame, 1)[0];
    assert!(
        row.contains("! 1 needs you"),
        "off-board marks do not count:\n{frame}"
    );
    assert!(row.contains("? 1 question"), "{frame}");
    assert!(row.contains("✗ WEB-103 CI failing"), "{frame}");
}

#[test]
fn with_gh_unavailable_the_bar_says_so_and_shows_no_ci_alert() {
    let mut snapshot = tabs_lanes();
    failing_pr(&mut snapshot, "WEB-103", 7);
    snapshot.gh = gh("unavailable");
    let shared = SharedFake::quiet(snapshot);
    let (mut d, _) = board(&shared);
    render_at(&mut d, 240, 40);
    let items = bar_walk(&mut d);
    assert!(items.contains(&BarItem::GhUnavailable), "{items:?}");
    assert!(
        !items
            .iter()
            .any(|i| matches!(i, BarItem::CiFailing(_) | BarItem::CiCollapsed)),
        "{items:?}"
    );
    let frame = render_at(&mut d, 240, 40);
    assert!(bar_rows(&frame, 1)[0].contains("gh unavailable"), "{frame}");
    assert!(!frame.contains("CI failing"), "{frame}");
}

#[test]
fn failing_ci_shows_one_alert_per_pr_or_one_collapsed_alert_when_the_row_overflows() {
    let mut snapshot = tabs_lanes();
    for (n, id) in ["WEB-101", "WEB-102", "WEB-103", "WEB-104", "WEB-105"]
        .into_iter()
        .enumerate()
    {
        failing_pr(&mut snapshot, id, 10 + n as u64);
    }
    snapshot.gh = gh("ok");
    let shared = SharedFake::quiet(snapshot);
    let (mut d, _) = board(&shared);
    render_at(&mut d, 240, 40);
    let wide = bar_walk(&mut d);
    let per_pr = wide
        .iter()
        .filter(|i| matches!(i, BarItem::CiFailing(_)))
        .count();
    assert_eq!(per_pr, 5, "{wide:?}");
    press(&mut d, KeyCode::Esc);
    render_at(&mut d, 96, 40);
    let narrow = bar_walk(&mut d);
    assert!(narrow.contains(&BarItem::CiCollapsed), "{narrow:?}");
    let frame = render_at(&mut d, 96, 40);
    assert!(frame.contains("✗ 5 CI failing"), "{frame}");
    press(&mut d, KeyCode::Esc);
    bar_to(&mut d, BarItem::CiCollapsed);
    press(&mut d, KeyCode::Enter);
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
}

#[test]
fn five_requests_at_96_show_the_ones_that_fit_then_more_and_arrows_reach_every_one() {
    let shared = SharedFake::quiet(tabs_lanes());
    let ids: Vec<i64> = ["WEB-101", "WEB-102", "WEB-103", "WEB-104", "WEB-105"]
        .into_iter()
        .map(|issue| shared.ask(issue, "agent-with-a-long-name"))
        .collect();
    let (mut d, _) = board(&shared);
    let frame = render_at(&mut d, 96, 40);
    let alerts = bar_rows(&frame, 3)[1].clone();
    assert!(alerts.contains("WEB-101"), "{frame}");
    assert!(!alerts.contains("WEB-105"), "{frame}");
    assert!(alerts.contains(" more"), "{frame}");
    let walked = bar_walk(&mut d);
    for id in &ids {
        assert!(walked.contains(&BarItem::Request(*id)), "{walked:?}");
    }
    assert!(walked.contains(&BarItem::More), "{walked:?}");
    press(&mut d, KeyCode::Esc);
    bar_to(&mut d, BarItem::More);
    press(&mut d, KeyCode::Enter);
    let Focus::Bar(BarItem::Request(shown)) = focus(&d) else {
        panic!("Return on more selects a hidden request: {:?}", focus(&d));
    };
    let frame = render_at(&mut d, 96, 40);
    let issue = ["WEB-101", "WEB-102", "WEB-103", "WEB-104", "WEB-105"]
        [ids.iter().position(|id| *id == shown).unwrap()];
    assert!(bar_rows(&frame, 3)[1].contains(issue), "{frame}");
    assert!(!bar_rows(&frame, 3)[1].contains("WEB-101"), "{frame}");
}

#[test]
fn the_tip_follows_the_selection() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-101", MarkKind::Attention, "look", "agent-a");
    shared.mark_kind("WEB-101", MarkKind::Question, "which?", "agent-a");
    let (mut d, _) = board(&shared);
    let tip = |d: &mut Driver| bar_rows(&render_at(d, 240, 40), 1)[0].clone();
    assert!(
        tip(&mut d).contains("WEB-101 needs you and has a question · Enter opens it"),
        "{}",
        tip(&mut d)
    );
    press(&mut d, KeyCode::Up);
    assert!(tip(&mut d).contains("Enter folds Alpha"), "{}", tip(&mut d));
    press(&mut d, KeyCode::Enter);
    assert!(
        tip(&mut d).contains("Enter unfolds Alpha"),
        "{}",
        tip(&mut d)
    );
    press(&mut d, KeyCode::Up);
    assert_eq!(focus(&d), Focus::Filter);
    assert!(tip(&mut d).contains("Type to filter"), "{}", tip(&mut d));
    press(&mut d, KeyCode::Esc);
    assert!(tip(&mut d).contains("Nothing selected"), "{}", tip(&mut d));
}

#[test]
fn the_nothing_selected_tip_names_the_density_key() {
    let shared = SharedFake::quiet(tabs_lanes());
    let (mut d, _) = board(&shared);
    let tip = |d: &mut Driver| bar_rows(&render_at(d, 240, 40), 1)[0].clone();
    press(&mut d, KeyCode::Esc);
    assert!(tip(&mut d).contains("z compacts cards"), "{}", tip(&mut d));
    press(&mut d, KeyCode::Char('z'));
    assert!(tip(&mut d).contains("z expands cards"), "{}", tip(&mut d));
}

#[test]
fn the_tip_names_the_paging_button_and_a_suggestions_keys() {
    let shared = SharedFake::quiet(tabs_lanes());
    let worktree = Worktree::new("bar-tip");
    shared.suggest("WEB-101", Some(&worktree.0));
    let (mut d, _) = board(&shared);
    let frame = render_at(&mut d, 144, 40);
    assert!(
        bar_rows(&frame, 1)[0].contains("a binds it · x dismisses it"),
        "{frame}"
    );
    press(&mut d, KeyCode::Char('>'));
    let frame = render_at(&mut d, 144, 40);
    assert!(bar_rows(&frame, 1)[0].contains("hidden"), "{frame}");
}

#[test]
fn where_the_zones_do_not_fit_the_bar_stacks_tip_alerts_and_controls_centred() {
    let shared = SharedFake::quiet(tabs_lanes());
    shared.mark_kind("WEB-101", MarkKind::Attention, "look", "agent-a");
    let (mut d, _) = board(&shared);
    let wide = render_at(&mut d, 240, 40);
    let one = bar_rows(&wide, 1)[0].clone();
    assert!(one.contains("keys") && one.contains("needs you"), "{wide}");
    let frame = render_at(&mut d, 60, 40);
    let rows = bar_rows(&frame, 3);
    assert!(rows[0].contains("WEB-101 needs you"), "{frame}");
    assert!(rows[1].contains("! 1 needs you"), "{frame}");
    assert!(rows[2].contains("keys"), "{frame}");
    for row in &rows {
        let lead = row.len() - row.trim_start().len();
        assert!(lead > 0, "centred, not flush left: {row:?}\n{frame}");
    }
}
