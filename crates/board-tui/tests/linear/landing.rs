//! Edge states and entry points: `R` forcing a read past the daemon's cache
//! (R23, AE8), the not-imported message (R24), and a board opened by an agent
//! landing on the context it was given (R25).

use super::*;

use board_tui::{Landing, ShowContext};

fn snapshot_params(log: &RequestLog) -> Vec<Value> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == "linear.snapshot")
        .map(|(_, p)| p.clone())
        .collect()
}

fn landing_on(issue: Option<&str>, card: Option<&str>) -> LinearStart {
    LinearStart {
        show: ShowContext::parse(None, issue, card),
        ..linear_start()
    }
}

#[test]
fn ae8_on_a_stale_board_capital_r_forces_a_read_and_r_does_not() {
    let (client, log) = RecordingClient::new(fake_with(linear_fixture("linear-unavailable")));
    let (mut d, _, _) = linear_driver(client, linear_start());
    assert!(draw(&d.app, W, H).contains("~stale"));
    press(&mut d, KeyCode::Char('R'));
    press(&mut d, KeyCode::Char('r'));
    let sent = snapshot_params(&log);
    assert_eq!(sent.len(), 3, "{:?}", methods(&log));
    assert!(
        sent[0].get("force").is_none(),
        "the opening read: {}",
        sent[0]
    );
    assert_eq!(sent[1]["force"], true, "{}", sent[1]);
    assert!(
        sent[2].get("force").is_none(),
        "r stays unforced: {}",
        sent[2]
    );
}

#[test]
fn capital_r_during_an_in_flight_read_queues_one_forced_read() {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()));
    let mut d = linear_driver_deferred(client, linear_start());
    press(&mut d, KeyCode::Char('R'));
    press(&mut d, KeyCode::Char('R'));
    assert_eq!(toast(&d), "", "R is queued, not dropped");
    assert!(d.deliver_pending_linear_snapshot());
    assert!(d.deliver_pending_linear_snapshot(), "one forced follow-up");
    assert!(!d.deliver_pending_linear_snapshot(), "and only one");
    let sent = snapshot_params(&log);
    assert_eq!(sent.len(), 2);
    assert!(sent[0].get("force").is_none(), "{}", sent[0]);
    assert_eq!(sent[1]["force"], true, "{}", sent[1]);
}

#[test]
fn lower_r_during_an_in_flight_read_still_toasts_and_queues_nothing() {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()));
    let mut d = linear_driver_deferred(client, linear_start());
    press(&mut d, KeyCode::Char('r'));
    assert_eq!(toast(&d), "refresh already in flight");
    assert!(d.deliver_pending_linear_snapshot());
    assert!(!d.deliver_pending_linear_snapshot());
    assert_eq!(snapshot_params(&log).len(), 1);
}

#[test]
fn not_imported_prints_the_daemons_message_in_the_not_bound_body() {
    let mut snapshot = linear_fixture("unbound");
    snapshot.linear.status = "not_imported".into();
    snapshot.linear.message =
        Some("store not imported;\u{1b}[31m run `board import work-store`".into());
    let (d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    assert_eq!(d.app.screen, Screen::LinearNotBound);
    let frame = draw(&d.app, W, H);
    assert!(
        frame.contains("store not imported;[31m run `board import work-store`"),
        "{frame}"
    );
    assert!(
        !frame.contains("not_imported"),
        "no bare status word:\n{frame}"
    );
    assert!(!frame.contains('\u{1b}'));
}

#[test]
fn board_show_issue_lands_once_and_a_later_snapshot_does_not_reapply_it() {
    let shared = SharedFake::new(tabs_lanes());
    let (client, log) = RecordingClient::new(shared);
    let (mut d, _, _) = linear_driver(client, landing_on(Some("WEB-202"), None));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-202"));
    assert_eq!(selected_column(&d), "st-prog");
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert_eq!(count(&log, "linear.mark.clear"), 0);
    press(&mut d, KeyCode::Char('['));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
    arrive(&mut d, tabs_lanes());
    press(&mut d, KeyCode::Char('R'));
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
}

#[test]
fn a_landing_target_absent_from_the_view_opens_its_detail_and_clears_nothing() {
    let shared = SharedFake::new(tabs_lanes());
    shared.mark("WEB-9999");
    let (client, log) = RecordingClient::new(shared);
    let (mut d, _, _) = linear_driver(client, landing_on(Some("WEB-9999"), None));
    assert_eq!(d.app.screen, Screen::LinearDetail);
    let state = d.app.linear.as_ref().unwrap();
    assert_eq!(state.detail.as_deref(), Some("WEB-9999"));
    assert_eq!(state.marks_for("WEB-9999").count(), 1);
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|(m, p)| m == "linear.issue" && p["issue"] == "WEB-9999")
            .count(),
        1
    );
    assert_eq!(count(&log, "linear.mark.clear"), 0, "landing never clears");
    press(&mut d, KeyCode::Esc);
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert_eq!(count(&log, "linear.mark.clear"), 0);
}

#[test]
fn card_beats_issue_and_an_unresolvable_card_falls_to_the_issue() {
    let show = ShowContext::parse(None, Some("WEB-202"), Some("7"));
    assert_eq!(
        show.targets(),
        vec![Landing::Card(7), Landing::Issue("WEB-202".into())]
    );
    // A Linear snapshot carries no card ids, so the card resolves to nothing
    // here and the issue lands.
    let (d, _, _) = linear_driver(
        fake_with(tabs_lanes()),
        landing_on(Some("WEB-202"), Some("7")),
    );
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-202"));
    let (d, _, _) = linear_driver(fake_with(tabs_lanes()), landing_on(None, Some("7")));
    assert_eq!(
        selected_id(&d).as_deref(),
        Some("WEB-101"),
        "nothing to land on"
    );
    assert_eq!(d.app.screen, Screen::LinearBoard);
}

#[test]
fn invalid_board_show_values_are_ignored() {
    let bad = ShowContext::parse(Some("w A;rm -rf"), Some("web-202\u{1b}"), Some("-3"));
    assert_eq!(bad, ShowContext::default());
    assert!(bad.targets().is_empty());
    for card in ["0", "7x", ""] {
        assert_eq!(
            ShowContext::parse(None, None, Some(card)).card,
            None,
            "{card:?}"
        );
    }
    let long = "w".repeat(65);
    assert_eq!(ShowContext::parse(Some(&long), None, None).space, None);
    let good = ShowContext::parse(Some("ws_1.a:b-2"), Some("ENG-123"), Some("42"));
    assert_eq!(good.space.as_deref(), Some("ws_1.a:b-2"));
    assert_eq!(good.issue.as_deref(), Some("ENG-123"));
    assert_eq!(good.card, Some(42));
    let (d, _, _) = linear_driver(
        fake_with(tabs_lanes()),
        LinearStart {
            show: bad,
            ..linear_start()
        },
    );
    assert_eq!(selected_id(&d).as_deref(), Some("WEB-101"));
}

#[test]
fn a_valid_board_show_space_names_the_board_and_an_invalid_one_does_not() {
    let valid = ShowContext::parse(Some("wB"), None, None);
    assert_eq!(valid.workspace(Some("wA")).as_deref(), Some("wB"));
    let invalid = ShowContext::parse(Some("w B"), None, None);
    assert_eq!(invalid.workspace(Some("wA")).as_deref(), Some("wA"));
    assert_eq!(invalid.workspace(None), None);
}

#[test]
fn b_on_the_issue_page_binds_through_linear_bind_not_a_handoff() {
    let worktree = Worktree::new("u10-b");
    let path = worktree.0.to_str().unwrap().to_string();
    let mut snapshot = bound_with_view();
    snapshot.issues.get_mut("WEB-3302").unwrap().bindings =
        vec![binding("proposed", &path, "wA:p2")];
    let (client, log) = RecordingClient::new(fake_with(snapshot));
    let (mut d, _, _) = linear_driver(client, linear_start());
    open_web_3302(&mut d);
    log.lock().unwrap().clear();
    press(&mut d, KeyCode::Char('b'));
    assert_eq!(count(&log, "linear.bind_handoff"), 0);
    let binds: Vec<Value> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == "linear.bind")
        .map(|(_, p)| p.clone())
        .collect();
    assert_eq!(binds.len(), 1, "{:?}", methods(&log));
    assert_eq!(binds[0]["cwd"], path.as_str());
    assert_eq!(binds[0]["issue"], "WEB-3302");
    assert_eq!(binds[0]["space"], "wA");
    assert_eq!(toast(&d), "bound WEB-3302");
}
