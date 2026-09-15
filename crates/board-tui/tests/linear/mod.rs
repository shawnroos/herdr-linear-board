//! Linear mode: the board rendered from the work plugin's snapshot, the
//! closed effect allow set, the one-in-flight refresh rule, and the three
//! card actions. Every test runs against `FakeBoardClient` (or a wrapper) and
//! the vendored plugin fixtures in `board-core/tests/fixtures/linear-snapshot`.

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::protocol::{CardCreateParams, Event, LinearSnapshot, PaneFocusResult};
use board_tui::app::{Effect, Mode, Msg, Screen};
use board_tui::testkit::{
    draw, hostile_origin, key, linear_driver, linear_driver_deferred, linear_fixture, linear_start,
    methods, render_at, MethodNotFoundClient, RecordingClient,
};
use board_tui::{Driver, LinearStart, OriginContext};
use crossterm::event::KeyCode;
use serde_json::Value;

const W: u16 = 120;
const H: u16 = 32;

/// `bound-with-view` with the daemon-attached pane statuses: the first pane
/// idle, the second working, the unmapped tab's pane working.
fn bound_with_view() -> LinearSnapshot {
    let mut snapshot = linear_fixture("bound-with-view");
    for (pane, status) in [
        ("wA:p1", "idle"),
        ("wA:p2", "working"),
        ("wA:p9", "working"),
    ] {
        snapshot.pane_status.insert(pane.into(), status.into());
    }
    snapshot
}

fn fake_with(snapshot: LinearSnapshot) -> FakeBoardClient {
    FakeBoardClient::new()
        .unwrap()
        .with_linear_snapshot(snapshot)
}

fn start_with_socket() -> LinearStart {
    LinearStart {
        origin: OriginContext {
            origin_socket: Some("/tmp/herdr-test.sock".into()),
            ..OriginContext::default()
        },
        ..linear_start()
    }
}

fn press(d: &mut Driver, code: KeyCode) {
    d.handle(key(code));
}

fn toast(d: &Driver) -> String {
    d.app
        .toast
        .as_ref()
        .map(|t| t.text.clone())
        .unwrap_or_default()
}

/// Open WEB-3312 (third column, first card) from the board.
fn open_web_3312(d: &mut Driver) {
    press(d, KeyCode::Char('l'));
    press(d, KeyCode::Char('l'));
    press(d, KeyCode::Enter);
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert_eq!(
        d.app.linear.as_ref().unwrap().detail.as_deref(),
        Some("WEB-3312")
    );
}

// -- rendering (AE1, AE9, AE2, AE4, herdr down, mapping, detail, stale) -----

#[test]
fn bound_with_view_renders_the_views_columns_in_order() {
    let (d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    assert_eq!(d.app.mode, Mode::Linear);
    assert_eq!(d.app.screen, Screen::LinearBoard);
    let frame = draw(&d.app, W, H);
    let backlog = frame.find("Backlog (1)").expect("Backlog column");
    let todo = frame.find("Todo (1)").expect("Todo column");
    let progress = frame.find("In Progress (1)").expect("In Progress column");
    assert!(
        backlog < todo && todo < progress,
        "columns keep the view's order:\n{frame}"
    );
    assert!(
        !frame.contains("st-cancel") && !frame.contains("Canceled"),
        "hidden column omitted"
    );
    assert!(frame.contains("view: Canvas board"), "{frame}");
    assert!(
        frame.contains("WEB-3312") && frame.contains("⧉2"),
        "{frame}"
    );
    assert!(
        frame.contains("Elsewhere (wA:t2)") && frame.contains("wA:p9 working"),
        "{frame}"
    );
    insta::assert_snapshot!("linear_bound_with_view", frame);
}

#[test]
fn bound_no_view_renders_team_states_and_the_bind_hint() {
    let (d, _, _) = linear_driver(fake_with(linear_fixture("bound-no-view")), linear_start());
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("no view chosen: /work:bind"), "{frame}");
    assert!(
        frame.contains("Backlog (1)") && frame.contains("Done (0)"),
        "{frame}"
    );
    insta::assert_snapshot!("linear_bound_no_view", frame);
}

#[test]
fn a_view_linear_no_longer_has_falls_back_and_says_no_view_is_chosen() {
    let mut snapshot = linear_fixture("bound-no-view");
    snapshot.view.status = "not_found".into();
    snapshot.view.id = Some("cccccccc-cccc-4ccc-8ccc-cccccccccccc".into());
    snapshot.view.name = Some("Old board".into());
    let (d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    let frame = draw(&d.app, W, H);
    assert!(
        frame.contains("view Old board not found · no view chosen: /work:bind"),
        "{frame}"
    );
    assert!(frame.contains("! view not found"), "{frame}");
    assert!(
        frame.contains("Backlog (1)") && frame.contains("Done (0)"),
        "{frame}"
    );
}

#[test]
fn unbound_space_shows_the_not_bound_screen_and_makes_one_request() {
    let (client, log) = RecordingClient::new(fake_with(linear_fixture("unbound")));
    let (d, _, _) = linear_driver(client, linear_start());
    assert_eq!(d.app.screen, Screen::LinearNotBound);
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("Space Plugins (wA) is not bound"), "{frame}");
    assert!(frame.contains("/work:bind"), "{frame}");
    assert_eq!(methods(&log), vec!["linear.snapshot"]);
    insta::assert_snapshot!("linear_unbound", frame);
}

#[test]
fn record_unreadable_lands_on_the_not_bound_screen_and_says_so() {
    let (d, _, _) = linear_driver(
        fake_with(linear_fixture("record-unreadable")),
        linear_start(),
    );
    assert_eq!(d.app.screen, Screen::LinearNotBound);
    let frame = draw(&d.app, W, H);
    assert!(
        frame.contains("the workspace record is unreadable"),
        "{frame}"
    );
    assert!(frame.contains("record unreadable"), "{frame}");
}

#[test]
fn linear_unavailable_marks_cards_stale_and_names_linear_in_the_header() {
    let (d, _, _) = linear_driver(
        fake_with(linear_fixture("linear-unavailable")),
        linear_start(),
    );
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("~stale"), "{frame}");
    assert!(frame.contains("! Linear unavailable"), "{frame}");
    assert!(frame.contains("view: Canvas board (unreadable)"), "{frame}");
    insta::assert_snapshot!("linear_unavailable", frame);
}

#[test]
fn herdr_unavailable_still_renders_cards_with_unknown_pane_status() {
    let (mut d, _, _) = linear_driver(
        fake_with(linear_fixture("herdr-unavailable")),
        linear_start(),
    );
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("! herdr unavailable"), "{frame}");
    assert!(
        frame.contains("space: wA"),
        "label falls back to the id:\n{frame}"
    );
    assert!(frame.contains("WEB-3312"), "{frame}");
    open_web_3312(&mut d);
    let detail = draw(&d.app, W, H);
    assert!(detail.contains("(label unknown)"), "{detail}");
    assert!(detail.contains("(no panes listed)"), "{detail}");
    insta::assert_snapshot!("linear_herdr_unavailable", frame);
}

#[test]
fn non_default_mapping_warns_and_renders_as_default() {
    let mut snapshot = bound_with_view();
    snapshot.mapping.source = "config".into();
    snapshot.mapping.tab = "issue".into();
    let (d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    let frame = draw(&d.app, W, H);
    assert!(
        frame.contains("mapping config: space=project tab=issue pane=session is not the default"),
        "{frame}"
    );
    assert!(
        frame.contains("In Progress (1)"),
        "columns still render:\n{frame}"
    );
    insta::assert_snapshot!("linear_non_default_mapping", frame);
}

#[test]
fn unsupported_grouping_is_named_in_the_header() {
    let (d, _, _) = linear_driver(
        fake_with(linear_fixture("bound-view-unsupported-grouping")),
        linear_start(),
    );
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("view unsupported grouping cycle"), "{frame}");
    assert!(frame.contains("Backlog (1)"), "fallback columns:\n{frame}");
}

#[test]
fn detail_lists_bindings_tabs_and_panes_with_live_status() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    open_web_3312(&mut d);
    let frame = draw(&d.app, W, H);
    assert!(
        frame.contains("[bound] $SANDBOX/worktrees/web-3312"),
        "{frame}"
    );
    assert!(frame.contains("tab: Plugin PM (wA:t1)"), "{frame}");
    assert!(
        frame.contains("wA:p1  idle") && frame.contains("▶ wA:p2  working"),
        "{frame}"
    );
    assert!(
        frame.contains("url: https://linear.app/example/issue/web-3312/x"),
        "{frame}"
    );
    insta::assert_snapshot!("linear_detail", frame);
}

#[test]
fn stale_daemon_screen_names_the_method_and_the_stop_command() {
    let start = LinearStart {
        board_version: "0.18.0".into(),
        daemon_version: Some("0.17.0".into()),
        ..linear_start()
    };
    let (d, _, _) = linear_driver(MethodNotFoundClient(FakeBoardClient::new().unwrap()), start);
    assert_eq!(d.app.screen, Screen::LinearStaleDaemon);
    let frame = draw(&d.app, W, H);
    assert!(
        frame.contains("lacks linear.snapshot; it is older than this board"),
        "{frame}"
    );
    assert!(frame.contains("board 0.18.0 · daemon 0.17.0"), "{frame}");
    assert!(
        frame.contains("Run `board daemon stop` and reopen the board."),
        "{frame}"
    );
    insta::assert_snapshot!("linear_stale_daemon", frame);
}

#[test]
fn stale_daemon_screen_hides_equal_versions() {
    let (d, _, _) = linear_driver(
        MethodNotFoundClient(FakeBoardClient::new().unwrap()),
        linear_start(),
    );
    let frame = draw(&d.app, W, H);
    assert!(!frame.contains("board 0.17.0"), "{frame}");
    assert!(frame.contains("board daemon stop"), "{frame}");
}

#[test]
fn help_sheet_lists_the_linear_keys_and_any_key_closes_it() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    press(&mut d, KeyCode::Char('?'));
    assert_eq!(d.app.screen, Screen::Help);
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("Help — Linear mode"), "{frame}");
    assert!(frame.contains("copy worktree path"), "{frame}");
    insta::assert_snapshot!("linear_help", frame);
    press(&mut d, KeyCode::Char('x'));
    assert_eq!(d.app.screen, Screen::LinearBoard);
}

#[test]
fn control_characters_never_reach_the_frame() {
    let mut snapshot = bound_with_view();
    let issue = snapshot.issues.get_mut("WEB-3318").unwrap();
    issue.title = "AI Tools\u{202E} drawer\u{1b}[2J is blank".into();
    let (mut d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    press(&mut d, KeyCode::Enter);
    let frame = draw(&d.app, W, H);
    assert!(
        frame.contains("WEB-3318 — AI Tools drawer[2J is blank"),
        "{frame}"
    );
    assert!(
        !frame.contains('\u{202E}') && !frame.contains('\u{1b}'),
        "{frame}"
    );
}

// -- the allow set (AE11, R17) -----------------------------------------------

#[test]
fn a_card_create_effect_is_refused_before_any_request_is_built() {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()));
    let (mut d, _, _) = linear_driver(client, linear_start());
    assert_eq!(methods(&log), vec!["linear.snapshot"]);
    d.apply_effect(Effect::CardCreate(CardCreateParams {
        title: "smuggled".into(),
        ..Default::default()
    }));
    d.apply_effect(Effect::CardDelete(1));
    d.apply_effect(Effect::LoadProjects);
    assert_eq!(methods(&log), vec!["linear.snapshot"], "no request left");
    assert_eq!(toast(&d), "not available in Linear mode");
}

#[test]
fn no_pane_set_title_or_board_get_across_construction_and_a_session() {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()));
    let start = LinearStart {
        origin: OriginContext {
            plugin_id: Some("herdr-board".into()),
            ..hostile_origin()
        },
        ..linear_start()
    };
    let (mut d, _, _) = linear_driver(client, start);
    open_web_3312(&mut d);
    press(&mut d, KeyCode::Char('j'));
    press(&mut d, KeyCode::Esc);
    press(&mut d, KeyCode::Char('r'));
    press(&mut d, KeyCode::Char('?'));
    press(&mut d, KeyCode::Esc);
    press(&mut d, KeyCode::Char('q'));
    assert!(d.app.should_quit);
    let seen = methods(&log);
    assert!(!seen.iter().any(|m| m == "pane.set_title"), "{seen:?}");
    assert!(!seen.iter().any(|m| m == "board.get"), "{seen:?}");
    assert_eq!(seen, vec!["linear.snapshot", "linear.snapshot"]);
}

#[test]
fn upstream_effects_still_run_outside_linear_mode() {
    let mut d = board_tui::testkit::demo_driver("x");
    assert_eq!(d.app.mode, Mode::Upstream);
    d.apply_effect(Effect::Refetch);
    assert!(d.app.toast.is_none());
}

// -- refresh policy (R21) ----------------------------------------------------

#[test]
fn a_refresh_while_one_is_in_flight_is_dropped_with_a_toast() {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()));
    let mut d = linear_driver_deferred(client, linear_start());
    assert!(d.app.linear.as_ref().unwrap().in_flight);
    assert!(methods(&log).is_empty(), "held, not sent");
    press(&mut d, KeyCode::Char('r'));
    assert_eq!(toast(&d), "refresh already in flight");
    assert!(d.deliver_pending_linear_snapshot());
    assert!(
        !d.deliver_pending_linear_snapshot(),
        "exactly one fetch was pending"
    );
    assert_eq!(methods(&log), vec!["linear.snapshot"]);
    assert!(!d.app.linear.as_ref().unwrap().in_flight);
    press(&mut d, KeyCode::Char('R'));
    assert!(d.deliver_pending_linear_snapshot());
    assert_eq!(methods(&log), vec!["linear.snapshot", "linear.snapshot"]);
}

#[test]
fn board_changed_sends_nothing_and_reconnect_sends_exactly_one_snapshot() {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()));
    let (mut d, _, _) = linear_driver(client, linear_start());
    assert_eq!(methods(&log), vec!["linear.snapshot"]);
    d.handle(Msg::Refresh);
    d.on_daemon_signals(true, false);
    assert_eq!(
        methods(&log),
        vec!["linear.snapshot"],
        "board_changed is ignored"
    );
    d.on_daemon_signals(true, true);
    assert_eq!(methods(&log), vec!["linear.snapshot", "linear.snapshot"]);
}

#[test]
fn a_key_is_handled_while_a_snapshot_is_in_flight_and_the_result_replaces_the_board() {
    let client = fake_with(bound_with_view());
    let mut d = linear_driver_deferred(client, linear_start());
    let waiting = draw(&d.app, W, H);
    assert!(waiting.contains("refreshing…"), "{waiting}");
    assert!(
        waiting.contains("waiting for the first snapshot"),
        "{waiting}"
    );
    press(&mut d, KeyCode::Char('?'));
    assert_eq!(d.app.screen, Screen::Help);
    let help = draw(&d.app, W, H);
    assert!(help.contains("Help — Linear mode"), "{help}");
    assert!(d.deliver_pending_linear_snapshot());
    assert_eq!(d.app.screen, Screen::LinearBoard);
    let board = draw(&d.app, W, H);
    assert!(board.contains("In Progress (1)"), "{board}");
    assert!(!board.contains("refreshing…"), "{board}");
}

// -- errors (R19, R25) --------------------------------------------------------

/// Answers the first `linear.snapshot` from the fake, then fails every later
/// one, so a test can watch the last good document survive a bad refresh.
struct FlakyClient {
    inner: FakeBoardClient,
    calls: usize,
}

impl BoardClient for FlakyClient {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        if method == "linear.snapshot" {
            self.calls += 1;
            if self.calls > 1 {
                return Err(board_core::Error::PluginUnavailable(
                    "running /plug/bin/work-snapshot.sh: timed out\u{1b}[0m".into(),
                )
                .into());
            }
        }
        self.inner.call(method, params)
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        self.inner.subscribe()
    }
}

#[test]
fn a_failed_refresh_keeps_the_last_good_snapshot_behind_the_error() {
    let client = FlakyClient {
        inner: fake_with(bound_with_view()),
        calls: 0,
    };
    let (mut d, _, _) = linear_driver(client, linear_start());
    press(&mut d, KeyCode::Char('r'));
    assert_eq!(d.app.screen, Screen::LinearError);
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("Snapshot failed"), "{frame}");
    assert!(
        frame.contains("work-snapshot.sh: timed out[0m"),
        "sanitised error:\n{frame}"
    );
    assert!(
        frame.contains("In Progress (1)"),
        "last good stays behind:\n{frame}"
    );
    assert!(
        frame.contains("The last good snapshot stays on screen"),
        "{frame}"
    );
    insta::assert_snapshot!("linear_error_over_last_good", frame);
    press(&mut d, KeyCode::Esc);
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert!(d.app.linear.as_ref().unwrap().error.is_some());
}

#[test]
fn a_first_fetch_failure_has_no_last_good_and_no_dismiss() {
    let client = FakeBoardClient::new()
        .unwrap()
        .with_linear_snapshot_error("no resolvable plugin root: tried BOARD_WORK_PLUGIN_ROOT");
    let (mut d, _, _) = linear_driver(client, linear_start());
    assert_eq!(d.app.screen, Screen::LinearError);
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("no resolvable plugin root"), "{frame}");
    assert!(frame.contains("No snapshot has arrived yet."), "{frame}");
    press(&mut d, KeyCode::Esc);
    assert_eq!(d.app.screen, Screen::LinearError, "nothing to dismiss to");
}

// -- the three actions (R20, AE6) -------------------------------------------

#[test]
fn detail_cursor_starts_on_the_working_pane_and_o_focuses_it() {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()));
    let (mut d, _, _) = linear_driver(client, start_with_socket());
    open_web_3312(&mut d);
    assert_eq!(
        d.app.linear.as_ref().unwrap().pane_cursor,
        1,
        "wA:p2 is working"
    );
    press(&mut d, KeyCode::Char('o'));
    let requests = log.lock().unwrap();
    let (method, params) = requests.last().unwrap();
    assert_eq!(method, "pane.focus");
    assert_eq!(params["pane_id"], "wA:p2");
    assert_eq!(params["origin_socket"], "/tmp/herdr-test.sock");
    drop(requests);
    assert_eq!(toast(&d), "focused pane wA:p2");
    press(&mut d, KeyCode::Char('k'));
    press(&mut d, KeyCode::Char('o'));
    assert_eq!(log.lock().unwrap().last().unwrap().1["pane_id"], "wA:p1");
}

#[test]
fn a_gone_pane_toasts_and_changes_nothing_else() {
    let client = fake_with(bound_with_view()).with_pane_focus(PaneFocusResult {
        focused: false,
        gone: true,
    });
    let (mut d, _, _) = linear_driver(client, start_with_socket());
    open_web_3312(&mut d);
    press(&mut d, KeyCode::Char('o'));
    assert_eq!(toast(&d), "pane is closed; refresh");
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert!(!d.app.should_quit);
}

#[test]
fn focus_without_an_origin_socket_toasts_and_sends_nothing() {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()));
    let (mut d, _, _) = linear_driver(client, linear_start());
    open_web_3312(&mut d);
    press(&mut d, KeyCode::Char('o'));
    assert!(toast(&d).contains("HERDR_SOCKET_PATH"), "{}", toast(&d));
    assert_eq!(methods(&log), vec!["linear.snapshot"]);
}

#[test]
fn u_opens_the_issue_url_with_the_platform_opener() {
    let (mut d, opened, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    open_web_3312(&mut d);
    press(&mut d, KeyCode::Char('u'));
    assert_eq!(
        *opened.lock().unwrap(),
        vec!["https://linear.app/example/issue/web-3312/x".to_string()]
    );
    assert_eq!(toast(&d), "opening the issue in Linear");
}

#[test]
fn u_refuses_a_url_that_is_not_http_before_the_opener_sees_it() {
    let mut doc = bound_with_view();
    doc.issues.get_mut("WEB-3312").unwrap().url = Some("-aTerminal".to_string());
    let (mut d, opened, _) = linear_driver(fake_with(doc), linear_start());
    open_web_3312(&mut d);
    press(&mut d, KeyCode::Char('u'));
    assert!(
        opened.lock().unwrap().is_empty(),
        "{:?}",
        opened.lock().unwrap()
    );
    assert!(toast(&d).contains("not http(s)"), "{}", toast(&d));

    let mut doc = bound_with_view();
    doc.issues.get_mut("WEB-3312").unwrap().url = Some("file:///tmp/x".to_string());
    let (mut d, opened, _) = linear_driver(fake_with(doc), linear_start());
    open_web_3312(&mut d);
    press(&mut d, KeyCode::Char('u'));
    assert!(opened.lock().unwrap().is_empty());
}

#[test]
fn a_tiny_frame_with_unmapped_tabs_draws_without_panicking() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    for h in [1u16, 2, 3, 4, 5] {
        let _ = render_at(&mut d, 40, h);
    }
}

#[test]
fn a_chorded_key_on_the_detail_screen_is_ignored() {
    let (mut d, opened, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    open_web_3312(&mut d);
    d.handle(Msg::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char('u'),
        crossterm::event::KeyModifiers::ALT,
    )));
    assert!(
        opened.lock().unwrap().is_empty(),
        "Alt+u opened the browser"
    );
    assert_eq!(d.app.screen, Screen::LinearDetail);
}

#[test]
fn y_copies_the_worktree_path_and_says_when_the_directory_is_gone() {
    let (mut d, _, copied) = linear_driver(
        fake_with(linear_fixture("worktree-missing")),
        linear_start(),
    );
    open_web_3312(&mut d);
    press(&mut d, KeyCode::Char('y'));
    assert_eq!(
        *copied.lock().unwrap(),
        vec!["$SANDBOX/worktrees/web-3312".to_string()]
    );
    assert_eq!(toast(&d), "copied; directory is gone");

    let (mut d, _, copied) = linear_driver(fake_with(bound_with_view()), linear_start());
    open_web_3312(&mut d);
    press(&mut d, KeyCode::Char('y'));
    assert_eq!(copied.lock().unwrap().len(), 1);
    assert_eq!(toast(&d), "copied");
}

#[test]
fn a_card_without_bindings_explains_instead_of_acting() {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()));
    let (mut d, _, copied) = linear_driver(client, start_with_socket());
    press(&mut d, KeyCode::Enter);
    assert_eq!(
        d.app.linear.as_ref().unwrap().detail.as_deref(),
        Some("WEB-3318")
    );
    press(&mut d, KeyCode::Char('o'));
    assert_eq!(toast(&d), "this card has no recorded pane");
    press(&mut d, KeyCode::Char('y'));
    assert_eq!(toast(&d), "this card has no worktree binding");
    assert!(copied.lock().unwrap().is_empty());
    assert_eq!(methods(&log), vec!["linear.snapshot"]);
}

#[test]
fn mouse_input_is_ignored_in_linear_mode() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    d.handle(board_tui::testkit::left_down(3, 3));
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert!(d.app.toast.is_none());
}
