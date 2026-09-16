//! Linear mode: the board rendered from the work plugin's snapshot, the
//! closed effect allow set, the one-in-flight refresh rule, and the three
//! card actions. Every test runs against `FakeBoardClient` (or a wrapper) and
//! the vendored plugin fixtures in `board-core/tests/fixtures/linear-snapshot`.

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::protocol::{CardCreateParams, Event, LinearSnapshot, PaneFocusResult};
use board_tui::app::{Effect, Mode, Msg, Screen};
use board_tui::testkit::left_down;
use board_tui::testkit::{
    draw, hostile_origin, key, linear_driver, linear_driver_deferred,
    linear_driver_failing_platform, linear_fixture, linear_start, methods, render_at,
    MethodNotFoundClient, RecordingClient,
};
use board_tui::widgets::Zone;
use board_tui::{Driver, LinearStart, OriginContext};
use crossterm::event::{KeyCode, MouseEventKind};
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
fn bound_no_view_renders_team_states_under_the_default_view() {
    let (d, _, _) = linear_driver(fake_with(linear_fixture("bound-no-view")), linear_start());
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("view: project issues (default)"), "{frame}");
    assert!(
        !frame.contains("no view chosen") && !frame.contains("/work:bind"),
        "{frame}"
    );
    // Five 36-cell columns need 180 cells; at W only the first three fit.
    let wide = draw(&d.app, 180, H);
    assert!(
        wide.contains("Backlog (1)") && wide.contains("Done (0)"),
        "{wide}"
    );
    insta::assert_snapshot!("linear_bound_no_view", frame);
}

#[test]
fn a_view_linear_no_longer_has_is_named_with_why_it_is_unusable() {
    for (status, words) in [
        ("not_found", "not found"),
        ("archived", "archived"),
        ("not_in_project", "not in project"),
    ] {
        let mut snapshot = linear_fixture("bound-no-view");
        snapshot.view.status = status.into();
        snapshot.view.id = Some("cccccccc-cccc-4ccc-8ccc-cccccccccccc".into());
        snapshot.view.name = Some("Old board".into());
        let (d, _, _) = linear_driver(fake_with(snapshot), linear_start());
        let frame = draw(&d.app, W, H);
        assert!(
            frame.contains(&format!(
                "view Old board {words} · no view chosen: /work:bind"
            )),
            "{status}:\n{frame}"
        );
        assert!(frame.contains(&format!("! view {words}")), "{frame}");
        assert!(!frame.contains("(default)"), "{frame}");
    }
    let mut snapshot = linear_fixture("bound-no-view");
    snapshot.view.status = "not_found".into();
    snapshot.view.name = Some("Old board".into());
    let (d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    let wide = draw(&d.app, 180, H);
    assert!(
        wide.contains("Backlog (1)") && wide.contains("Done (0)"),
        "{wide}"
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
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), start_with_herdr_fixture());
    press(&mut d, KeyCode::Char('?'));
    assert_eq!(d.app.screen, Screen::Help);
    let frame = render_at(&mut d, W, H);
    assert!(frame.contains("Help — Linear mode"), "{frame}");
    assert!(frame.contains("copy worktree path"), "{frame}");
    assert!(frame.contains("more below"), "{frame}");
    insta::assert_snapshot!("linear_help", frame);
    press(&mut d, KeyCode::Char('x'));
    assert_eq!(d.app.screen, Screen::LinearBoard);
}

fn herdr_fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/linear/fixtures/herdr-config.toml")
}

fn start_with_herdr_fixture() -> LinearStart {
    LinearStart {
        herdr_keys: board_tui::herdr_keys::read_file(&herdr_fixture()),
        ..linear_start()
    }
}

fn help_scrolled_to_the_end(start: LinearStart) -> (Driver, String) {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), start);
    press(&mut d, KeyCode::Char('?'));
    render_at(&mut d, W, H);
    for _ in 0..200 {
        press(&mut d, KeyCode::Char('j'));
    }
    assert_eq!(d.app.screen, Screen::Help, "scrolling keeps the sheet open");
    let frame = render_at(&mut d, W, H);
    (d, frame)
}

#[test]
fn the_help_sheet_scrolls_to_a_herdr_section_built_from_the_config() {
    let (mut d, frame) = help_scrolled_to_the_end(start_with_herdr_fixture());
    assert!(frame.contains("herdr (your config)"), "{frame}");
    for text in [
        "ctrl+alt+w",
        "workspace picker",
        "example.picker: pick",
        "example-viewer: open-example",
        "open the example dashboard",
    ] {
        assert!(frame.contains(text), "{text} missing:\n{frame}");
    }
    assert!(!frame.contains("more below"), "{frame}");
    insta::assert_snapshot!("linear_help_herdr_keys", frame);

    let at_end = d.app.help_scroll;
    press(&mut d, KeyCode::Down);
    assert_eq!(
        d.app.help_scroll, at_end,
        "the scroll stops at the last row"
    );
    press(&mut d, KeyCode::Up);
    assert_eq!(d.app.help_scroll, at_end - 1);
    press(&mut d, KeyCode::Char('x'));
    assert_eq!(d.app.screen, Screen::LinearBoard);
}

#[test]
fn a_missing_herdr_config_leaves_no_herdr_section() {
    let dir = tempfile::tempdir().unwrap();
    let start = LinearStart {
        herdr_keys: board_tui::herdr_keys::read_file(&dir.path().join("config.toml")),
        ..linear_start()
    };
    let (_, frame) = help_scrolled_to_the_end(start);
    assert!(!frame.contains("herdr (your config)"), "{frame}");
    assert!(frame.contains("any other key closes"), "{frame}");
}

#[test]
fn herdr_keys_never_join_the_board_key_table() {
    let rows = board_tui::herdr_keys::read_file(&herdr_fixture());
    assert!(!rows.is_empty());
    for row in rows {
        assert!(
            board_tui::view::HELP_KEYS
                .iter()
                .all(|(_, key, description)| *key != row.key && *description != row.label),
            "{row:?} leaked into HELP_KEYS"
        );
    }
}

#[test]
fn control_characters_never_reach_the_frame() {
    let mut snapshot = bound_with_view();
    let issue = snapshot.issues.get_mut("WEB-3318").unwrap();
    issue.title = "Example\u{202E} panel\u{1b}[2J is blank".into();
    let (mut d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    press(&mut d, KeyCode::Enter);
    let frame = draw(&d.app, W, H);
    assert!(
        frame.contains("WEB-3318 — Example panel[2J is blank"),
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
fn only_the_linear_pane_title_and_no_board_get_across_construction_and_a_session() {
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
    assert!(!seen.iter().any(|m| m == "board.get"), "{seen:?}");
    assert_eq!(
        seen,
        vec![
            "linear.snapshot",
            "pane.set_title",
            "linear.snapshot",
            "pane.set_title"
        ]
    );
    let titles: Vec<Value> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == "pane.set_title")
        .map(|(_, params)| params.clone())
        .collect();
    let expected = serde_json::json!({
        "pane_id": "hostile-pane-sentinel",
        "title": "Linear: AI Canvas Tools",
        "origin_socket": "/hostile/socket",
    });
    assert_eq!(titles, vec![expected.clone(), expected]);
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
    assert_eq!(
        d.app.screen,
        Screen::Help,
        "the help sheet survives the arrival"
    );
    press(&mut d, KeyCode::Char('x'));
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
fn an_opener_or_clipboard_failure_is_toasted() {
    let mut d = linear_driver_failing_platform(fake_with(bound_with_view()), linear_start());
    open_web_3312(&mut d);
    press(&mut d, KeyCode::Char('u'));
    assert!(toast(&d).starts_with("open failed:"), "{}", toast(&d));
    press(&mut d, KeyCode::Char('y'));
    assert!(toast(&d).starts_with("copy failed:"), "{}", toast(&d));
}

#[test]
fn a_focus_the_daemon_cannot_carry_out_is_toasted_with_its_reason() {
    let client = fake_with(bound_with_view()).with_pane_focus_error("herdr is not running");
    let (mut d, _, _) = linear_driver(client, start_with_socket());
    open_web_3312(&mut d);
    press(&mut d, KeyCode::Char('o'));
    let text = toast(&d);
    assert!(text.starts_with("pane wA:p2:"), "{text}");
    assert!(text.contains("herdr is not running"), "{text}");
    assert_eq!(d.app.screen, Screen::LinearDetail);
}

#[test]
fn a_reconnect_while_a_snapshot_is_in_flight_is_sent_when_that_one_lands() {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()));
    let mut d = linear_driver_deferred(client, linear_start());
    assert!(d.app.linear.as_ref().unwrap().in_flight);
    d.on_daemon_signals(false, true);
    assert!(
        d.app.toast.is_none(),
        "an automatic refresh is not refused with a toast"
    );
    assert!(d.app.linear.as_ref().unwrap().queued);
    assert!(d.deliver_pending_linear_snapshot());
    assert!(
        d.deliver_pending_linear_snapshot(),
        "the queued refresh was sent"
    );
    assert!(!d.deliver_pending_linear_snapshot());
    assert_eq!(methods(&log), vec!["linear.snapshot", "linear.snapshot"]);
    let state = d.app.linear.as_ref().unwrap();
    assert!(!state.in_flight && !state.queued);
}

#[test]
fn a_document_without_herdr_or_mapping_sections_warns_about_neither() {
    let mut doc = bound_with_view();
    doc.herdr = Default::default();
    doc.mapping = Default::default();
    let (d, _, _) = linear_driver(fake_with(doc), linear_start());
    let state = d.app.linear.as_ref().unwrap();
    assert_eq!(state.source_warnings(), Vec::<String>::new());
    assert_eq!(state.non_default_mapping(), None);
}

#[test]
fn every_string_in_the_document_is_sanitised_including_ones_no_code_names() {
    const POISON: &str = "\u{202E}\u{1b}[2J\u{200B}";
    fn poison(value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(format!("{text}{POISON}")),
            Value::Array(items) => Value::Array(items.into_iter().map(poison).collect()),
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, item)| (key, poison(item)))
                    .collect(),
            ),
            other => other,
        }
    }
    fn dirty(value: &Value) -> bool {
        let bad = |text: &str| {
            text.chars()
                .any(|c| POISON.contains(c) && c != '[' && c != '2' && c != 'J')
        };
        match value {
            Value::String(text) => bad(text),
            Value::Array(items) => items.iter().any(dirty),
            Value::Object(map) => map.iter().any(|(key, item)| bad(key) || dirty(item)),
            _ => false,
        }
    }
    let mut doc = poison(serde_json::to_value(bound_with_view()).unwrap());
    // The two maps keyed by data: their keys are document text too.
    for section in ["issues", "pane_status"] {
        let map = doc[section].as_object().unwrap().clone();
        doc[section] = Value::Object(
            map.into_iter()
                .map(|(key, item)| (format!("{key}{POISON}"), item))
                .collect(),
        );
    }
    let mut snapshot: LinearSnapshot = serde_json::from_value(doc).unwrap();
    assert!(
        dirty(&serde_json::to_value(&snapshot).unwrap()),
        "the poison did not take"
    );
    let strings_before = serde_json::to_string(&snapshot)
        .unwrap()
        .matches('"')
        .count();
    board_tui::app::sanitise_snapshot(&mut snapshot);
    let after = serde_json::to_value(&snapshot).unwrap();
    assert!(!dirty(&after), "{after:#}");
    assert_eq!(snapshot.issues.len(), 3);
    assert_eq!(
        serde_json::to_string(&snapshot)
            .unwrap()
            .matches('"')
            .count(),
        strings_before,
        "sanitising dropped or added strings"
    );
}

#[test]
fn a_view_whose_filter_left_the_project_is_named_and_asks_for_a_new_choice() {
    let mut doc = linear_fixture("bound-no-view");
    doc.view.status = "not_in_project".into();
    doc.view.id = Some("cccccccc-cccc-4ccc-8ccc-cccccccccccc".into());
    doc.view.name = Some("Canvas board".into());
    let (d, _, _) = linear_driver(fake_with(doc), linear_start());
    let frame = draw(&d.app, W, H);
    assert!(
        frame.contains("view Canvas board not in project"),
        "{frame}"
    );
    assert!(frame.contains("no view chosen: /work:bind"), "{frame}");
}

#[test]
fn the_board_sends_its_plugin_root_with_every_snapshot_request() {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()));
    let start = LinearStart {
        origin: OriginContext {
            plugin_root: Some("/plugins/work".into()),
            ..OriginContext::default()
        },
        ..linear_start()
    };
    let (_d, _, _) = linear_driver(client, start);
    let sent = log.lock().unwrap();
    let (method, params) = &sent[0];
    assert_eq!(method, "linear.snapshot");
    assert_eq!(params["plugin_root"], "/plugins/work");
}

// -- card and column geometry (R1, R2, R3, AE2) ------------------------------

/// A 36-cell frame is one column; rows come back without the quotes the
/// backend wraps each row in.
fn narrow_rows(title: &str, assignee: Option<&str>) -> Vec<String> {
    let mut snapshot = bound_with_view();
    let issue = snapshot.issues.get_mut("WEB-3318").unwrap();
    issue.title = title.into();
    issue.assignee = assignee.map(|name| board_core::protocol::LinearAssignee {
        id: None,
        name: Some(name.into()),
    });
    let (mut d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    render_at(&mut d, 36, 20).lines().map(backend_row).collect()
}

/// The backend quotes each row and appends a note after the closing quote
/// when a row holds wide glyphs.
fn backend_row(row: &str) -> String {
    row.split("\" Hidden by multi-width symbols")
        .next()
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// The inside of the column's border on frame row `row`.
fn cell(rows: &[String], row: usize) -> String {
    let chars: Vec<char> = rows[row].chars().collect();
    assert_eq!(chars.first(), Some(&'│'), "row {row}: {rows:#?}");
    assert_eq!(chars.last(), Some(&'│'), "row {row}: {rows:#?}");
    chars[1..chars.len() - 1]
        .iter()
        .collect::<String>()
        .trim_end()
        .to_string()
}

const NINETY: &str =
    "Example issue: the panel stays blank while a long list of items is still loading from disk";

#[test]
fn a_ninety_character_title_at_36_cells_wraps_to_two_lines_and_ends_in_an_ellipsis() {
    assert_eq!(NINETY.chars().count(), 90);
    let rows = narrow_rows(NINETY, Some("Example User"));
    assert!(cell(&rows, 3).starts_with("WEB-3318"), "{rows:#?}");
    let first = cell(&rows, 4);
    let second = cell(&rows, 5);
    assert!(
        NINETY.starts_with(&first) && !first.ends_with('…'),
        "{rows:#?}"
    );
    assert!(second.ends_with('…'), "{rows:#?}");
    assert_eq!(cell(&rows, 6), "@Example User", "{rows:#?}");
    insta::assert_snapshot!("linear_card_two_line_title_36", rows.join("\n"));
}

#[test]
fn a_one_word_title_longer_than_the_column_breaks_mid_word() {
    let word = "Exampleissuewithoutanyspacesthatrunsfarpastthecolumnedge";
    let rows = narrow_rows(word, None);
    let first = cell(&rows, 4);
    assert_eq!(first.chars().count(), 34, "{rows:#?}");
    assert!(word.starts_with(&first), "{rows:#?}");
    let second = cell(&rows, 5);
    assert!(word[34..].starts_with(&second), "{rows:#?}");
}

#[test]
fn a_title_of_wide_glyphs_wraps_by_display_width() {
    let title = "例".repeat(40);
    let rows = narrow_rows(&title, None);
    assert_eq!(cell(&rows, 4), "例".repeat(17), "{rows:#?}");
    assert_eq!(cell(&rows, 5), format!("{}…", "例".repeat(16)), "{rows:#?}");
    assert_eq!(cell(&rows, 6), "unassigned", "{rows:#?}");
}

#[test]
fn a_one_line_title_leaves_the_second_title_row_blank() {
    let rows = narrow_rows("Example short title", Some("Example User"));
    assert_eq!(cell(&rows, 4), "Example short title", "{rows:#?}");
    assert_eq!(cell(&rows, 5), "", "{rows:#?}");
    assert_eq!(cell(&rows, 6), "@Example User", "{rows:#?}");
}

#[test]
fn an_empty_title_and_a_missing_assignee_render_placeholders() {
    let rows = narrow_rows("", None);
    assert_eq!(cell(&rows, 4), "(no title)", "{rows:#?}");
    assert_eq!(cell(&rows, 5), "", "{rows:#?}");
    assert_eq!(cell(&rows, 6), "unassigned", "{rows:#?}");
}

#[test]
fn cards_in_a_column_are_separated_by_a_blank_row() {
    let mut snapshot = bound_with_view();
    snapshot.groups[0].issues = vec!["WEB-3318".into(), "WEB-3317".into()];
    let (mut d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    let rows: Vec<String> = render_at(&mut d, 36, 20).lines().map(backend_row).collect();
    assert_eq!(cell(&rows, 7), "", "{rows:#?}");
    assert!(cell(&rows, 8).starts_with("WEB-3317"), "{rows:#?}");
}

#[test]
fn a_column_of_zero_cards_renders_its_header_and_nothing_else() {
    let mut snapshot = bound_with_view();
    snapshot.groups[0].issues.clear();
    let (mut d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    let rows: Vec<String> = render_at(&mut d, 36, 20).lines().map(backend_row).collect();
    assert!(rows[2].contains("Backlog (0)"), "{rows:#?}");
    let bottom = rows.iter().position(|r| r.starts_with('└')).unwrap();
    for row in 3..bottom {
        assert_eq!(cell(&rows, row), "", "{rows:#?}");
    }
}

#[test]
fn a_body_36_cells_wide_draws_one_column_and_72_draws_two() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    for (width, columns) in [(36u16, 1usize), (71, 1), (72, 2)] {
        let frame = render_at(&mut d, width, 20);
        let top = frame.lines().nth(2).unwrap();
        assert_eq!(top.matches('┌').count(), columns, "{width}:\n{frame}");
    }
}

// -- stacked narrow layout (R4, R5, R6, AE1) ---------------------------------

/// The frame's third row: the top border of every drawn column.
fn column_tops(frame: &str) -> String {
    frame.lines().nth(2).unwrap_or_default().to_string()
}

#[test]
fn a_70_cell_body_shows_one_group_with_its_position_and_the_right_key_moves_on() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    let frame = render_at(&mut d, 70, 20);
    let tops = column_tops(&frame);
    assert_eq!(tops.matches('┌').count(), 1, "{frame}");
    assert!(tops.contains("Backlog (1) · 1/5"), "{frame}");
    assert!(
        tops.trim_end_matches('"').ends_with('┐'),
        "fills the width:\n{frame}"
    );
    assert!(!frame.contains("Todo (1)"), "{frame}");
    insta::assert_snapshot!("linear_stacked_70", frame);
    press(&mut d, KeyCode::Char('l'));
    let frame = render_at(&mut d, 70, 20);
    assert!(column_tops(&frame).contains("Todo (1) · 2/5"), "{frame}");
    assert!(!frame.contains("Backlog (1)"), "{frame}");
    press(&mut d, KeyCode::Left);
    let frame = render_at(&mut d, 70, 20);
    assert!(column_tops(&frame).contains("Backlog (1) · 1/5"), "{frame}");
}

#[test]
fn two_columns_at_100_and_all_five_groups_at_200_carry_no_position() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    let frame = render_at(&mut d, 100, 20);
    assert_eq!(column_tops(&frame).matches('┌').count(), 2, "{frame}");
    assert!(!column_tops(&frame).contains("/5"), "{frame}");
    insta::assert_snapshot!("linear_two_column_100", frame);
    let frame = render_at(&mut d, 200, 20);
    assert_eq!(column_tops(&frame).matches('┌').count(), 5, "{frame}");
    assert!(!column_tops(&frame).contains("/5"), "{frame}");
    insta::assert_snapshot!("linear_five_column_200", frame);
}

#[test]
fn a_one_group_board_at_full_width_is_not_stacked() {
    let mut snapshot = bound_with_view();
    snapshot.groups.truncate(1);
    let (mut d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    let frame = render_at(&mut d, 200, 20);
    assert!(column_tops(&frame).contains("Backlog (1) ─"), "{frame}");
    assert!(!column_tops(&frame).contains("1/1"), "{frame}");
}

#[test]
fn resizing_from_200_to_70_keeps_the_selected_group() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    render_at(&mut d, 200, 20);
    press(&mut d, KeyCode::Char('l'));
    let frame = render_at(&mut d, 70, 20);
    assert_eq!(d.app.linear.as_ref().unwrap().sel_group, 1);
    assert!(column_tops(&frame).contains("Todo (1) · 2/5"), "{frame}");
}

#[test]
fn resizing_from_70_to_200_keeps_the_same_group_in_view() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    render_at(&mut d, 70, 20);
    press(&mut d, KeyCode::Char('l'));
    press(&mut d, KeyCode::Char('l'));
    assert!(column_tops(&render_at(&mut d, 70, 20)).contains("In Progress (1) · 3/5"));
    let frame = render_at(&mut d, 200, 20);
    assert_eq!(d.app.linear.as_ref().unwrap().sel_group, 2);
    assert!(column_tops(&frame).contains("In Progress (1)"), "{frame}");
}

#[test]
fn a_selection_past_the_last_group_is_clamped_when_drawn() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    d.app.linear.as_mut().unwrap().sel_group = 9;
    let frame = render_at(&mut d, 70, 20);
    assert!(column_tops(&frame).contains("Done (0) · 5/5"), "{frame}");
    let state = d.app.linear.as_mut().unwrap();
    state.sel_group = 2;
    state.sel_card = 9;
    let frame = render_at(&mut d, 70, 20);
    assert!(
        column_tops(&frame).contains("In Progress (1) · 3/5"),
        "{frame}"
    );
    assert!(
        frame.contains("WEB-3312"),
        "scrolled past the only card:\n{frame}"
    );
}

#[test]
fn a_body_two_rows_tall_renders_the_header_and_says_the_body_is_too_short() {
    let mut snapshot = bound_with_view();
    snapshot.unmapped.clear();
    let (mut d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    let frame = render_at(&mut d, 70, 5);
    let rows: Vec<&str> = frame.lines().collect();
    assert!(
        rows[0].contains(" Linear ") && rows[0].contains("view: Canvas board"),
        "{frame}"
    );
    assert!(rows[2].contains("too short"), "{frame}");
    assert!(!frame.contains("no columns"), "{frame}");
    insta::assert_snapshot!("linear_body_too_short", frame);
}

#[test]
fn a_stacked_board_with_zero_groups_says_the_snapshot_has_no_columns() {
    let mut snapshot = bound_with_view();
    snapshot.groups.clear();
    let (mut d, _, _) = linear_driver(fake_with(snapshot), linear_start());
    let frame = render_at(&mut d, 70, 20);
    assert!(frame.contains("no columns in this snapshot"), "{frame}");
    assert!(!frame.contains("too short"), "{frame}");
}

// -- the filter picker (R17, R18, R19, R24, R27, AE3) -------------------------

use board_core::protocol::{
    LinearListEnvelope, LinearListKind, LinearListResult, LinearListStatus, LinearProjectRow,
    LinearSpaceRow,
};

fn space(id: &str, label: &str) -> LinearSpaceRow {
    LinearSpaceRow {
        id: id.into(),
        label: label.into(),
        live: Some(true),
        state: "unmapped".into(),
        ..Default::default()
    }
}

fn spaces(rows: Vec<LinearSpaceRow>) -> LinearListResult {
    LinearListResult::Spaces(LinearListEnvelope {
        status: LinearListStatus::Ok,
        message: None,
        rows,
    })
}

fn three_spaces() -> LinearListResult {
    spaces(vec![
        space("wA", "Alpha work"),
        space("wB", "Beta notes"),
        space("wC", "Gamma alpha"),
    ])
}

fn project(id: &str, team_key: &str, name: &str) -> LinearProjectRow {
    LinearProjectRow {
        id: id.into(),
        name: name.into(),
        team_key: team_key.into(),
    }
}

fn projects(rows: Vec<LinearProjectRow>) -> LinearListResult {
    LinearListResult::Projects(LinearListEnvelope {
        status: LinearListStatus::Ok,
        message: None,
        rows,
    })
}

fn picker_driver(list: LinearListResult) -> (Driver, board_tui::testkit::RequestLog) {
    let (client, log) = RecordingClient::new(fake_with(bound_with_view()).with_linear_list(list));
    let (d, _, _) = linear_driver(client, linear_start());
    (d, log)
}

fn type_text(d: &mut Driver, text: &str) {
    for c in text.chars() {
        press(d, KeyCode::Char(c));
    }
}

fn filter(d: &Driver) -> String {
    d.app.picker.as_ref().unwrap().filter.clone()
}

fn selected(d: &Driver) -> Option<String> {
    d.app
        .picker
        .as_ref()
        .and_then(|p| p.selected_id().map(str::to_string))
}

fn visible_ids(d: &Driver) -> Vec<String> {
    d.app
        .picker
        .as_ref()
        .unwrap()
        .visible_rows()
        .into_iter()
        .map(|(_, row)| match row {
            board_tui::app::PickerRow::Linear(row) => row.id.clone(),
            other => panic!("{other:?}"),
        })
        .collect()
}

#[test]
fn typing_narrows_the_list_and_clamps_the_selection() {
    let (mut d, _) = picker_driver(three_spaces());
    d.open_linear_picker(LinearListKind::Spaces, None);
    assert_eq!(d.app.screen, Screen::LinearPicker);
    press(&mut d, KeyCode::Down);
    press(&mut d, KeyCode::Down);
    assert_eq!(selected(&d).as_deref(), Some("wC"));
    type_text(&mut d, "BETA");
    assert_eq!(visible_ids(&d), vec!["wB"]);
    assert_eq!(d.app.picker.as_ref().unwrap().sel, 0);
    assert_eq!(selected(&d).as_deref(), Some("wB"));
    press(&mut d, KeyCode::Down);
    assert_eq!(
        selected(&d).as_deref(),
        Some("wB"),
        "nowhere past the last row"
    );
}

#[test]
fn a_filter_matching_nothing_says_so_and_enter_does_nothing() {
    let (mut d, log) = picker_driver(three_spaces());
    d.open_linear_picker(LinearListKind::Spaces, None);
    type_text(&mut d, "zzz");
    assert!(visible_ids(&d).is_empty());
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("no match for"), "{frame}");
    let before = methods(&log);
    press(&mut d, KeyCode::Enter);
    assert_eq!(d.app.screen, Screen::LinearPicker);
    assert!(d.app.linear.as_ref().unwrap().pick.is_none());
    assert_eq!(methods(&log), before);
}

#[test]
fn question_mark_r_and_q_are_literal_while_filtering() {
    let (mut d, log) = picker_driver(three_spaces());
    d.open_linear_picker(LinearListKind::Spaces, None);
    let before = methods(&log);
    type_text(&mut d, "?rqfjk");
    assert_eq!(d.app.screen, Screen::LinearPicker, "no help sheet");
    assert!(!d.app.should_quit, "no quit");
    assert_eq!(methods(&log), before, "no fetch");
    assert_eq!(filter(&d), "?rqfjk");
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("filter: ?rqfjk"), "{frame}");
}

#[test]
fn escape_clears_the_filter_and_a_second_escape_closes_the_picker() {
    let (mut d, _) = picker_driver(three_spaces());
    d.open_linear_picker(LinearListKind::Spaces, None);
    type_text(&mut d, "alpha");
    assert_eq!(visible_ids(&d), vec!["wA", "wC"]);
    press(&mut d, KeyCode::Esc);
    assert_eq!(filter(&d), "");
    assert_eq!(d.app.screen, Screen::LinearPicker);
    assert_eq!(visible_ids(&d).len(), 3);
    press(&mut d, KeyCode::Esc);
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert!(d.app.picker.is_none());
    assert!(!d.app.should_quit);
}

#[test]
fn enter_records_the_chosen_row_and_closes_the_picker() {
    let (mut d, _) = picker_driver(three_spaces());
    d.open_linear_picker(LinearListKind::Spaces, None);
    type_text(&mut d, "gamma");
    press(&mut d, KeyCode::Enter);
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert!(d.app.picker.is_none());
    assert_eq!(
        d.app.linear.as_ref().unwrap().pick,
        Some(board_tui::app::LinearPick {
            kind: LinearListKind::Spaces,
            list_id: None,
            id: "wC".into(),
        })
    );
}

#[test]
fn a_snapshot_arriving_leaves_the_picker_open_with_its_filter_and_selection() {
    let (mut d, log) = picker_driver(three_spaces());
    d.open_linear_picker(LinearListKind::Spaces, None);
    type_text(&mut d, "a");
    press(&mut d, KeyCode::Down);
    assert_eq!(selected(&d).as_deref(), Some("wB"));
    d.handle(Msg::LinearRefresh);
    assert_eq!(
        methods(&log)
            .iter()
            .filter(|m| *m == "linear.snapshot")
            .count(),
        2,
        "the snapshot arrived"
    );
    assert_eq!(d.app.screen, Screen::LinearPicker);
    assert_eq!(filter(&d), "a");
    assert_eq!(selected(&d).as_deref(), Some("wB"));
    press(&mut d, KeyCode::Esc);
    press(&mut d, KeyCode::Esc);
    assert_eq!(d.app.screen, Screen::LinearBoard, "closes to the board");
}

#[test]
fn a_reconnect_leaves_the_picker_open() {
    let (mut d, log) = picker_driver(three_spaces());
    d.open_linear_picker(LinearListKind::Spaces, None);
    type_text(&mut d, "beta");
    d.on_daemon_signals(false, true);
    assert_eq!(
        methods(&log)
            .iter()
            .filter(|m| *m == "linear.snapshot")
            .count(),
        2
    );
    assert_eq!(d.app.screen, Screen::LinearPicker);
    assert_eq!(filter(&d), "beta");
    assert_eq!(selected(&d).as_deref(), Some("wB"));
}

#[test]
fn a_help_sheet_open_when_a_snapshot_lands_stays_open() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    press(&mut d, KeyCode::Char('?'));
    d.handle(Msg::LinearRefresh);
    assert_eq!(d.app.screen, Screen::Help);
    press(&mut d, KeyCode::Char('x'));
    assert_eq!(d.app.screen, Screen::LinearBoard);
}

/// Answers `linear.list` from a queue, one envelope per call.
struct ListSequence {
    inner: FakeBoardClient,
    lists: std::collections::VecDeque<LinearListResult>,
}

impl BoardClient for ListSequence {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        if method == "linear.list" {
            let next = self.lists.pop_front().expect("a queued list");
            return Ok(serde_json::to_value(next)?);
        }
        self.inner.call(method, params)
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        self.inner.subscribe()
    }
}

#[test]
fn a_list_arriving_again_keeps_the_filter_and_the_selected_identifier() {
    let client = ListSequence {
        inner: fake_with(bound_with_view()),
        lists: [
            three_spaces(),
            spaces(vec![
                space("wZ", "Zeta alpha"),
                space("wA", "Alpha work"),
                space("wB", "Beta notes"),
                space("wC", "Gamma alpha"),
            ]),
        ]
        .into(),
    };
    let (mut d, _, _) = linear_driver(client, linear_start());
    d.open_linear_picker(LinearListKind::Spaces, None);
    type_text(&mut d, "alpha");
    press(&mut d, KeyCode::Down);
    assert_eq!(selected(&d).as_deref(), Some("wC"));
    d.open_linear_picker(LinearListKind::Spaces, None);
    assert_eq!(filter(&d), "alpha");
    assert_eq!(visible_ids(&d), vec!["wZ", "wA", "wC"]);
    assert_eq!(
        selected(&d).as_deref(),
        Some("wC"),
        "by identifier, not index"
    );
}

#[test]
fn a_name_with_a_newline_and_a_tab_draws_on_one_row_and_filters_collapsed() {
    let (mut d, _) = picker_driver(spaces(vec![
        space("wA", "Alpha\nwork\tspace"),
        space("wB", "Beta"),
    ]));
    d.open_linear_picker(LinearListKind::Spaces, None);
    let frame = draw(&d.app, W, H);
    let row = frame.lines().find(|l| l.contains("wA")).expect("row drawn");
    assert!(row.contains("Alpha work space"), "{frame}");
    assert!(
        !frame
            .lines()
            .any(|l| l.trim_start_matches(['│', ' ']).starts_with("work")),
        "{frame}"
    );
    type_text(&mut d, "alpha work space");
    assert_eq!(visible_ids(&d), vec!["wA"]);
}

#[test]
fn two_projects_with_identical_names_show_their_team_keys_first() {
    let (mut d, _) = picker_driver(projects(vec![
        project("p1", "WEB", "Example launch"),
        project("p2", "OPS", "Example launch"),
    ]));
    d.open_linear_picker(LinearListKind::Projects, None);
    let frame = draw(&d.app, W, H);
    let rows: Vec<&str> = frame
        .lines()
        .filter(|l| l.contains("Example launch"))
        .collect();
    assert_eq!(rows.len(), 2, "{frame}");
    let web = rows[0].find("WEB").expect("team key drawn");
    assert!(web < rows[0].find("Example launch").unwrap(), "{frame}");
    assert!(rows[1].find("OPS").unwrap() < rows[1].find("Example launch").unwrap());
    type_text(&mut d, "ops");
    assert_eq!(visible_ids(&d), vec!["p2"]);
}

#[test]
fn a_multi_byte_filter_matches_character_wise() {
    let (mut d, _) = picker_driver(spaces(vec![
        space("wA", "Café Ünïcode"),
        space("wB", "Cafe plain"),
    ]));
    d.open_linear_picker(LinearListKind::Spaces, None);
    type_text(&mut d, "éü");
    assert!(visible_ids(&d).is_empty());
    press(&mut d, KeyCode::Backspace);
    assert_eq!(filter(&d), "é", "backspace removes one character");
    assert_eq!(visible_ids(&d), vec!["wA"]);
    press(&mut d, KeyCode::Backspace);
    type_text(&mut d, "ÜNÏ");
    assert_eq!(visible_ids(&d), vec!["wA"], "case-insensitive beyond ASCII");
}

#[test]
fn a_row_longer_than_the_picker_truncates_without_losing_the_discriminator() {
    let long = "An extremely long project name ".repeat(8);
    let (mut d, _) = picker_driver(projects(vec![project("p1", "WEB", &long)]));
    d.open_linear_picker(LinearListKind::Projects, None);
    let frame = draw(&d.app, 60, 20);
    let row = frame
        .lines()
        .find(|l| l.contains("An extremely"))
        .unwrap_or_else(|| panic!("row drawn:\n{frame}"));
    assert!(row.contains("›WEB  An extremely"), "{frame}");
    assert!(row.trim_end_matches(['"', '│']).ends_with('…'), "{frame}");
    assert_eq!(
        frame.lines().filter(|l| l.contains("An extremely")).count(),
        1
    );
}

#[test]
fn an_empty_list_an_unavailable_list_and_a_failed_read_render_differently() {
    let (mut empty, _) = picker_driver(spaces(vec![]));
    empty.open_linear_picker(LinearListKind::Spaces, None);
    let empty_frame = draw(&empty.app, W, H);
    assert!(empty_frame.contains("no spaces"), "{empty_frame}");
    insta::assert_snapshot!("linear_picker_empty_list", empty_frame);

    let (mut unavailable, _) = picker_driver(LinearListResult::Spaces(LinearListEnvelope {
        status: LinearListStatus::Unavailable,
        message: Some("herdr is not running".into()),
        rows: vec![],
    }));
    unavailable.open_linear_picker(LinearListKind::Spaces, None);
    let unavailable_frame = draw(&unavailable.app, W, H);
    assert!(
        unavailable_frame.contains("unavailable: herdr is not running"),
        "{unavailable_frame}"
    );
    assert!(
        !unavailable_frame.contains("no spaces"),
        "{unavailable_frame}"
    );

    let client = fake_with(bound_with_view())
        .with_linear_list_error(LinearListKind::Spaces, "running work-spaces.sh: timed out");
    let (mut failed, _, _) = linear_driver(client, linear_start());
    failed.open_linear_picker(LinearListKind::Spaces, None);
    let failed_frame = draw(&failed.app, W, H);
    assert!(
        failed_frame.contains("read failed: plugin unavailable: running work-spaces.sh: timed out"),
        "{failed_frame}"
    );
    assert!(!failed_frame.contains("no spaces"), "{failed_frame}");
    insta::assert_snapshot!("linear_picker_read_failed", failed_frame);
}

#[test]
fn a_picker_opened_before_its_list_arrives_shows_the_loading_line_then_the_rows() {
    let client = fake_with(bound_with_view()).with_linear_list(projects(vec![
        project("p1", "WEB", "Example launch"),
        project("p2", "OPS", "Example rollout"),
    ]));
    let mut d = linear_driver_deferred(client, linear_start());
    assert!(d.deliver_pending_linear_snapshot());
    d.open_linear_picker(LinearListKind::Projects, None);
    assert!(d
        .app
        .linear
        .as_ref()
        .unwrap()
        .list_in_flight(LinearListKind::Projects, None));
    d.open_linear_picker(LinearListKind::Projects, None);
    let loading = draw(&d.app, W, H);
    assert!(loading.contains("loading projects…"), "{loading}");
    assert!(
        !loading.contains("no projects") && !loading.contains("read failed"),
        "{loading}"
    );
    insta::assert_snapshot!("linear_picker_loading", loading);
    assert!(d.deliver_pending_linear_list());
    assert!(
        !d.deliver_pending_linear_list(),
        "a second open sent no second read"
    );
    assert!(!d
        .app
        .linear
        .as_ref()
        .unwrap()
        .list_in_flight(LinearListKind::Projects, None));
    let rows = draw(&d.app, W, H);
    assert!(!rows.contains("loading projects"), "{rows}");
    assert!(
        rows.contains("Example launch") && rows.contains("Example rollout"),
        "{rows}"
    );
}

#[test]
fn the_project_picker_filtered() {
    let (mut d, _) = picker_driver(projects(vec![
        project("p1", "WEB", "Example launch"),
        project("p2", "OPS", "Example rollout"),
        project("p3", "WEB", "Sample cleanup"),
    ]));
    d.open_linear_picker(LinearListKind::Projects, None);
    type_text(&mut d, "example");
    press(&mut d, KeyCode::Down);
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("filter: example"), "{frame}");
    assert!(!frame.contains("Sample cleanup"), "{frame}");
    insta::assert_snapshot!("linear_project_picker_filtered", frame);
}

#[test]
fn every_string_in_a_list_is_sanitised_on_arrival() {
    let (mut d, _) = picker_driver(LinearListResult::Spaces(LinearListEnvelope {
        status: LinearListStatus::Unavailable,
        message: Some("herdr\u{1b}[2J down\u{202E}".into()),
        rows: vec![space("w\u{200B}A", "Alpha\u{7f} work")],
    }));
    d.open_linear_picker(LinearListKind::Spaces, None);
    let picker = d.app.picker.as_ref().unwrap();
    assert_eq!(selected(&d).as_deref(), Some("wA"));
    assert_eq!(
        picker.outcome,
        Some(board_tui::app::ListOutcome::Read {
            status: LinearListStatus::Unavailable,
            message: Some("herdr[2J down".into()),
        })
    );
    let frame = draw(&d.app, W, H);
    assert!(frame.contains("Alpha work"), "{frame}");
    assert!(
        !frame.contains('\u{1b}') && !frame.contains('\u{202E}'),
        "{frame}"
    );
}

#[test]
fn a_view_picker_reads_the_views_of_the_project_it_was_opened_for() {
    let client = fake_with(bound_with_view()).with_linear_list(LinearListResult::Views(
        LinearListEnvelope {
            status: LinearListStatus::Ok,
            message: None,
            rows: vec![board_core::protocol::LinearViewRow {
                id: "v1".into(),
                name: "Example view".into(),
            }],
        },
    ));
    let (client, log) = RecordingClient::new(client);
    let mut d = linear_driver_deferred(client, linear_start());
    assert!(d.deliver_pending_linear_snapshot());
    d.open_linear_picker(LinearListKind::Views, Some("proj-a".into()));
    press(&mut d, KeyCode::Esc);
    d.open_linear_picker(LinearListKind::Views, Some("proj-b".into()));
    assert!(d.deliver_pending_linear_list());
    assert!(
        d.deliver_pending_linear_list(),
        "the second project was read too"
    );
    let ids: Vec<Value> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == "linear.list")
        .map(|(_, p)| p["id"].clone())
        .collect();
    assert_eq!(ids, vec![Value::from("proj-a"), Value::from("proj-b")]);
    assert_eq!(selected(&d).as_deref(), Some("v1"));
}

// -- mouse on the board (R21, R22, KTD9) -------------------------------------

/// `bound_with_view` with three more cards under In Progress, so a second card
/// sits at frame rows 8..=11 of the third column at `W`×`H`.
fn four_in_progress() -> LinearSnapshot {
    let mut snapshot = bound_with_view();
    let template = snapshot.issues["WEB-3312"].clone();
    let group = snapshot
        .groups
        .iter_mut()
        .find(|g| g.key == "st-prog")
        .unwrap();
    for identifier in ["WEB-9001", "WEB-9002", "WEB-9003"] {
        group.issues.push(identifier.to_string());
        let mut issue = template.clone();
        issue.identifier = identifier.to_string();
        issue.bindings.clear();
        snapshot.issues.insert(identifier.to_string(), issue);
    }
    snapshot
}

fn mouse_driver() -> Driver {
    let (d, _, _) = linear_driver(fake_with(four_in_progress()), linear_start());
    d
}

fn wheel(d: &mut Driver, kind: MouseEventKind) {
    d.handle(board_tui::testkit::mouse(kind, 90, 10));
}

fn selection(d: &Driver) -> (usize, usize, Option<String>) {
    let state = d.app.linear.as_ref().unwrap();
    (state.sel_group, state.sel_card, state.detail.clone())
}

/// Frame row `row` as the screen shows it, without the backend's quoting.
fn frame_row(frame: &str, row: usize) -> String {
    backend_row(frame.lines().nth(row).unwrap_or_default())
}

/// The In Progress column at `W`×`H`: x 80..120, title row 2, first card rows
/// 3..=6, separator row 7, second card rows 8..=11.
fn assert_in_progress_geometry(frame: &str) {
    let title: String = frame_row(frame, 2).chars().skip(80).collect();
    assert!(title.contains("In Progress (4)"), "{frame}");
    let first: String = frame_row(frame, 3).chars().skip(80).collect();
    assert!(first.contains("WEB-3312"), "{frame}");
    let gap: String = frame_row(frame, 7).chars().skip(81).take(38).collect();
    assert!(gap.trim().is_empty(), "{frame}");
    let second: String = frame_row(frame, 8).chars().skip(80).collect();
    assert!(second.contains("WEB-9001"), "{frame}");
}

#[test]
fn a_click_on_a_card_selects_it_and_opens_its_detail() {
    let mut d = mouse_driver();
    let frame = render_at(&mut d, W, H);
    assert_in_progress_geometry(&frame);
    d.handle(left_down(90, 9));
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert_eq!(selection(&d), (2, 1, Some("WEB-9001".into())));
}

#[test]
fn a_click_on_a_column_header_selects_that_group_without_opening_anything() {
    let mut d = mouse_driver();
    let frame = render_at(&mut d, W, H);
    assert_in_progress_geometry(&frame);
    d.handle(left_down(95, 2));
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert_eq!(selection(&d), (2, 0, None));
}

#[test]
fn a_click_on_the_gap_between_cards_or_off_the_columns_changes_nothing() {
    let mut d = mouse_driver();
    let frame = render_at(&mut d, W, H);
    assert_in_progress_geometry(&frame);
    for (x, y) in [(90, 7), (90, 25), (10, 0), (80, 5)] {
        d.handle(left_down(x, y));
        assert_eq!(d.app.screen, Screen::LinearBoard, "click at {x},{y}");
        assert_eq!(selection(&d), (0, 0, None), "click at {x},{y}");
    }
}

#[test]
fn a_card_click_in_the_stacked_layout_opens_that_card() {
    let (mut d, _, _) = linear_driver(fake_with(bound_with_view()), linear_start());
    let frame = render_at(&mut d, 70, 20);
    assert!(frame_row(&frame, 3).contains("WEB-3318"), "{frame}");
    d.handle(left_down(10, 4));
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert_eq!(selection(&d), (0, 0, Some("WEB-3318".into())));
}

#[test]
fn a_click_on_a_card_that_moved_since_the_draw_resolves_by_identifier() {
    let mut d = mouse_driver();
    let frame = render_at(&mut d, W, H);
    assert_in_progress_geometry(&frame);
    let snapshot = d.app.linear.as_mut().unwrap().last_good.as_mut().unwrap();
    let group = snapshot
        .groups
        .iter_mut()
        .find(|g| g.key == "st-prog")
        .unwrap();
    group.issues.retain(|id| id != "WEB-3312");
    d.handle(left_down(90, 9));
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert_eq!(selection(&d), (2, 0, Some("WEB-9001".into())));
}

#[test]
fn a_click_on_a_card_that_changed_columns_since_the_draw_follows_it() {
    let mut d = mouse_driver();
    let frame = render_at(&mut d, W, H);
    assert_in_progress_geometry(&frame);
    let snapshot = d.app.linear.as_mut().unwrap().last_good.as_mut().unwrap();
    for group in snapshot.groups.iter_mut() {
        group.issues.retain(|id| id != "WEB-9001");
    }
    let todo = snapshot
        .groups
        .iter_mut()
        .find(|g| g.key == "st-todo")
        .unwrap();
    todo.issues.push("WEB-9001".into());
    d.handle(left_down(90, 9));
    assert_eq!(d.app.screen, Screen::LinearDetail);
    assert_eq!(selection(&d), (1, 1, Some("WEB-9001".into())));
}

#[test]
fn a_click_on_a_card_that_left_the_snapshot_since_the_draw_does_nothing() {
    let mut d = mouse_driver();
    let frame = render_at(&mut d, W, H);
    assert_in_progress_geometry(&frame);
    let snapshot = d.app.linear.as_mut().unwrap().last_good.as_mut().unwrap();
    for group in snapshot.groups.iter_mut() {
        group.issues.retain(|id| id != "WEB-9001");
    }
    snapshot.issues.remove("WEB-9001");
    d.handle(left_down(90, 9));
    assert_eq!(d.app.screen, Screen::LinearBoard);
    assert_eq!(selection(&d), (0, 0, None));
}

#[test]
fn scrolling_moves_the_card_selection_and_stops_at_the_ends() {
    let mut d = mouse_driver();
    press(&mut d, KeyCode::Char('l'));
    press(&mut d, KeyCode::Char('l'));
    render_at(&mut d, W, H);
    wheel(&mut d, MouseEventKind::ScrollDown);
    assert_eq!(selection(&d), (2, 1, None));
    for _ in 0..6 {
        wheel(&mut d, MouseEventKind::ScrollDown);
    }
    assert_eq!(selection(&d), (2, 3, None), "stops at the last card");
    wheel(&mut d, MouseEventKind::ScrollUp);
    assert_eq!(selection(&d), (2, 2, None));
    for _ in 0..6 {
        wheel(&mut d, MouseEventKind::ScrollUp);
    }
    assert_eq!(selection(&d), (2, 0, None), "stops at the first card");
    assert_eq!(d.app.screen, Screen::LinearBoard);
}

#[test]
fn a_click_while_the_detail_overlay_is_open_does_not_reach_the_board() {
    let mut d = mouse_driver();
    open_web_3312(&mut d);
    let frame = render_at(&mut d, W, H);
    // Column 1 is outside the overlay, so the first column's card shows there.
    assert!(frame_row(&frame, 3).starts_with("│W"), "{frame}");
    for (x, y) in [(1, 4), (90, 9), (95, 2)] {
        d.handle(left_down(x, y));
        assert_eq!(d.app.screen, Screen::LinearDetail, "click at {x},{y}");
        assert_eq!(selection(&d), (2, 0, Some("WEB-3312".into())));
    }
    wheel(&mut d, MouseEventKind::ScrollDown);
    assert_eq!(d.app.linear.as_ref().unwrap().sel_card, 0);
}

#[test]
fn a_click_inside_an_open_picker_does_not_reach_the_board_behind_it() {
    let mut d = mouse_driver();
    render_at(&mut d, W, H);
    assert!(
        matches!(
            d.app.hit_map.borrow().hit(81, 15),
            Some(Zone::LinearCard { ref identifier, .. }) if identifier == "WEB-9002"
        ),
        "a card sits under the picker's body"
    );
    d.open_linear_picker(LinearListKind::Spaces, None);
    let frame = render_at(&mut d, W, H);
    assert!(frame_row(&frame, 14).contains("Choose a space"), "{frame}");
    d.handle(left_down(81, 15));
    assert_eq!(d.app.screen, Screen::LinearPicker);
    assert_eq!(selection(&d), (0, 0, None));
}
