//! The board's level from where its own pane sits in herdr (KTD10, KTD11):
//! the space board alone or as an overlay, the issue of the focused split
//! when it is one split among several.

use super::*;

use board_core::protocol::{
    LinearSessionBinding, LinearSessionGetResult, PaneContext, PaneContextPane,
};
use board_tui::testkit::{beside_driver, UnknownMethods};
use board_tui::SessionIdentity;
use std::collections::BTreeSet;

const OWN: &str = "wA:p7";

fn level_start() -> LinearStart {
    LinearStart {
        origin: OriginContext {
            origin_socket: Some("/tmp/herdr-test.sock".into()),
            pane_id: Some(OWN.into()),
            ..OriginContext::default()
        },
        ..linear_start()
    }
}

fn pane(id: &str, issue: Option<&str>) -> PaneContextPane {
    PaneContextPane {
        pane_id: id.into(),
        cwd: Some(format!("/w/{id}")),
        issue: issue.map(str::to_string),
    }
}

fn split(focused: &str, panes: Vec<PaneContextPane>) -> PaneContext {
    PaneContext {
        workspace_id: "wA".into(),
        tab_id: "wA:t1".into(),
        focused_pane_id: Some(focused.into()),
        panes,
        ..PaneContext::default()
    }
}

fn beside_two() -> PaneContext {
    split(
        "wA:p2",
        vec![
            pane("wA:p2", Some("WEB-3302")),
            pane("wA:p3", Some("WEB-3307")),
        ],
    )
}

fn alone() -> PaneContext {
    PaneContext {
        workspace_id: "wA".into(),
        tab_id: "wA:t1".into(),
        alone: true,
        focused_pane_id: Some(OWN.into()),
        ..PaneContext::default()
    }
}

struct Levels {
    d: Driver,
    ctx: Arc<Mutex<PaneContext>>,
    log: RequestLog,
}

fn levels_from<C: BoardClient + 'static>(
    fake: FakeBoardClient,
    client: impl FnOnce(FakeBoardClient) -> C,
    ctx: PaneContext,
) -> Levels {
    let handle = Arc::new(Mutex::new(ctx));
    let fake = fake.with_pane_context(handle.clone());
    let (recording, log) = RecordingClient::new(client(fake));
    let (mut d, _, _) = linear_driver(recording, level_start());
    d.app.last_area = ratatui::layout::Rect::new(0, 0, 200, 50);
    Levels {
        d,
        ctx: handle,
        log,
    }
}

fn levels(ctx: PaneContext) -> Levels {
    levels_from(fake_with(bound_with_view()), |fake| fake, ctx)
}

impl Levels {
    fn moved(&mut self, ctx: PaneContext) {
        *self.ctx.lock().unwrap() = ctx;
        let workspaces: BTreeSet<Option<String>> = [Some("wA".to_string())].into_iter().collect();
        self.d.handle(Msg::PaneLayoutChanged(workspaces));
    }

    fn split_issue(&self) -> Option<String> {
        let state = self.d.app.linear.as_ref().unwrap();
        state.at_split().then(|| state.detail.clone()).flatten()
    }

    fn reads(&self) -> usize {
        methods(&self.log)
            .iter()
            .filter(|m| *m == "pane.context")
            .count()
    }
}

#[test]
fn a_board_alone_in_its_tab_shows_the_space_board() {
    let l = levels(alone());
    assert_eq!(l.reads(), 1, "the level is read at start");
    assert_eq!(l.d.app.screen, Screen::LinearBoard);
    assert_eq!(l.split_issue(), None);
}

#[test]
fn a_board_split_beside_a_bound_agent_shows_its_issue_narrow_and_read_only() {
    let mut fake = fake_with(bound_with_view());
    fake.linear_activity_record(&LinearActivityRecordParams {
        tool_name: "mcp__linear__save_comment".into(),
        issue: Some("WEB-3302".into()),
        space: Some("wA".into()),
        ..LinearActivityRecordParams::default()
    })
    .unwrap();
    fake.linear_mark_set(&LinearMarkSetParams {
        space: "wA".into(),
        issue: "WEB-3302".into(),
        kind: MarkKind::Attention,
        text: Some("look".into()),
        created_by: Some("agent".into()),
        owner: Default::default(),
    })
    .unwrap();
    let mut l = levels_from(fake, |fake| fake, alone());
    l.moved(beside_two());

    assert_eq!(l.split_issue().as_deref(), Some("WEB-3302"));
    assert_eq!(l.d.app.screen, Screen::LinearDetail);
    assert!(
        render_at(&mut l.d, 200, 50).contains("Read only beside a split"),
        "the bar tip does not say the split level is read only"
    );
    assert!(
        !board_tui::view::issue_buttons_shown(&l.d.app),
        "a 200-cell split level drew the wide view's buttons"
    );
    for code in [
        KeyCode::Char('a'),
        KeyCode::Char('x'),
        KeyCode::Char('b'),
        KeyCode::Char('e'),
        KeyCode::Char('o'),
    ] {
        press(&mut l.d, code);
    }
    let sent = methods(&l.log);
    for write in [
        "linear.mark.clear",
        "linear.bind",
        "linear.show.accept",
        "linear.show.dismiss",
    ] {
        assert!(!sent.iter().any(|m| m == write), "{write} sent: {sent:?}");
    }
    assert_eq!(l.split_issue().as_deref(), Some("WEB-3302"));
}

#[test]
fn focusing_another_split_switches_and_focusing_the_board_does_not() {
    let mut l = levels(beside_two());
    assert_eq!(l.split_issue().as_deref(), Some("WEB-3302"));

    l.moved(split("wA:p3", beside_two().panes));
    assert_eq!(l.split_issue().as_deref(), Some("WEB-3307"));

    l.moved(split(OWN, beside_two().panes));
    assert_eq!(
        l.split_issue().as_deref(),
        Some("WEB-3307"),
        "clicking into the board changed what it shows"
    );
}

#[test]
fn an_unbound_split_shows_the_tabs_issue_and_no_issue_shows_the_space_board() {
    let mut l = levels(split(
        "wA:p3",
        vec![pane("wA:p2", Some("WEB-3308")), pane("wA:p3", None)],
    ));
    assert_eq!(l.split_issue().as_deref(), Some("WEB-3308"));

    l.moved(split("wA:p3", vec![pane("wA:p3", None)]));
    assert_eq!(l.split_issue(), None);
    assert_eq!(l.d.app.screen, Screen::LinearBoard);
}

#[test]
fn with_no_focus_history_a_multi_pane_tab_shows_the_tabs_issue() {
    let l = levels(split(OWN, beside_two().panes));
    assert_eq!(l.split_issue().as_deref(), Some("WEB-3302"));
}

#[test]
fn a_split_opening_under_a_modal_waits_until_the_modal_closes() {
    let mut l = levels(alone());
    open_web_3302(&mut l.d);
    l.moved(split("wA:p3", beside_two().panes));
    assert_eq!(l.split_issue(), None, "the level replaced an open modal");
    assert_eq!(
        l.d.app.linear.as_ref().unwrap().detail.as_deref(),
        Some("WEB-3302")
    );

    press(&mut l.d, KeyCode::Esc);
    assert_eq!(l.split_issue().as_deref(), Some("WEB-3307"));
}

#[test]
fn the_filter_holds_a_level_change_until_it_closes() {
    let mut l = levels(alone());
    l.d.app.linear.as_mut().unwrap().focus = Focus::Filter;
    l.moved(beside_two());
    assert_eq!(l.split_issue(), None);
    press(&mut l.d, KeyCode::Esc);
    assert_ne!(l.d.app.linear.as_ref().unwrap().focus, Focus::Filter);
    assert_eq!(l.split_issue().as_deref(), Some("WEB-3302"));
}

#[test]
fn an_older_daemon_without_the_pane_read_stays_at_the_space_level() {
    let mut l = levels_from(
        fake_with(bound_with_view()),
        |fake| UnknownMethods(fake, "pane.context"),
        beside_two(),
    );
    assert_eq!(l.split_issue(), None);
    assert_eq!(l.d.app.screen, Screen::LinearBoard);
    l.moved(beside_two());
    assert_eq!(l.reads(), 1, "the board asked an older daemon again");
    assert_eq!(l.d.app.screen, Screen::LinearBoard);
}

#[test]
fn an_overlay_shows_the_space_board_whatever_is_focused_beneath() {
    let overlay = || PaneContext {
        overlay: true,
        ..beside_two()
    };
    let mut l = levels(overlay());
    assert_eq!(l.split_issue(), None);
    l.moved(PaneContext {
        focused_pane_id: Some("wA:p3".into()),
        ..overlay()
    });
    assert_eq!(l.split_issue(), None);
}

#[test]
fn esc_at_the_split_root_shows_the_space_board_until_focus_moves_to_another_split() {
    let mut l = levels(beside_two());
    assert_eq!(l.split_issue().as_deref(), Some("WEB-3302"));

    press(&mut l.d, KeyCode::Esc);
    assert_eq!(l.split_issue(), None);
    assert_eq!(l.d.app.screen, Screen::LinearBoard);

    let mut signals = LocalStateSignals::default();
    signals.add(Some("wA".into()), false);
    l.d.handle(Msg::LocalStateChanged(signals));
    l.moved(beside_two());
    assert_eq!(
        l.split_issue(),
        None,
        "a re-read with focus unchanged undid Esc"
    );

    l.moved(split("wA:p3", beside_two().panes));
    assert_eq!(l.split_issue().as_deref(), Some("WEB-3307"));
}

#[test]
fn a_local_state_change_re_reads_the_level() {
    let mut l = levels(alone());
    let mut signals = LocalStateSignals::default();
    signals.add(Some("wA".into()), false);
    *l.ctx.lock().unwrap() = beside_two();
    l.d.handle(Msg::LocalStateChanged(signals));
    assert_eq!(l.reads(), 2);
    assert_eq!(l.split_issue().as_deref(), Some("WEB-3302"));
}

#[test]
fn a_layout_event_for_another_workspace_reads_nothing() {
    let mut l = levels(alone());
    let other: BTreeSet<Option<String>> = [Some("wZ".to_string())].into_iter().collect();
    l.d.handle(Msg::PaneLayoutChanged(other));
    assert_eq!(l.reads(), 1);
}

/// `linear.session.get` answered with a binding, as the daemon resolves the
/// agent's own cwd.
struct SessionBound<C>(C, &'static str);

impl<C: BoardClient> BoardClient for SessionBound<C> {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        if method == "linear.session.get" {
            return Ok(serde_json::to_value(LinearSessionGetResult {
                space: "wA".into(),
                space_bound: true,
                binding: Some(LinearSessionBinding {
                    worktree_path: "/w/agent".into(),
                    issue: self.1.into(),
                    bound_at: None,
                }),
                ..LinearSessionGetResult::default()
            })?);
        }
        self.0.call(method, params)
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        self.0.subscribe()
    }
}

fn agent() -> SessionIdentity {
    SessionIdentity {
        space: "wA".into(),
        herdr_socket: Some("/tmp/herdr-test.sock".into()),
        herdr_pane_id: Some("wA:p2".into()),
        claude_session_id: None,
        cwd: Some("/w/agent/src".into()),
    }
}

#[test]
fn board_tui_session_starts_at_the_split_level_for_the_named_agent_even_on_an_older_daemon() {
    let fake = fake_with(bound_with_view());
    let client = UnknownMethods(SessionBound(fake, "WEB-3307"), "pane.context");
    let d = beside_driver(client, level_start(), agent());
    assert_eq!(d.app.mode, Mode::Linear);
    let state = d.app.linear.as_ref().unwrap();
    assert!(state.at_split());
    assert_eq!(state.detail.as_deref(), Some("WEB-3307"));
}

#[test]
fn the_agents_own_cwd_wins_over_herdrs_for_the_pane_it_named() {
    let handle = Arc::new(Mutex::new(split(
        "wA:p2",
        vec![pane("wA:p2", None), pane("wA:p3", Some("WEB-3302"))],
    )));
    let fake = fake_with(bound_with_view()).with_pane_context(handle);
    let d = beside_driver(SessionBound(fake, "WEB-3307"), level_start(), agent());
    let state = d.app.linear.as_ref().unwrap();
    assert_eq!(state.detail.as_deref(), Some("WEB-3307"));
}

#[test]
fn a_board_started_beside_an_agent_never_renames_its_pane() {
    let (client, log) =
        RecordingClient::new(SessionBound(fake_with(bound_with_view()), "WEB-3307"));
    let start = LinearStart {
        origin: OriginContext {
            plugin_id: Some("herdr-board".into()),
            entrypoint: Some("session".into()),
            ..level_start().origin
        },
        ..level_start()
    };
    let mut d = beside_driver(client, start, agent());
    d.handle(Msg::LinearRefresh);
    let sent = methods(&log);
    assert!(sent.iter().any(|m| m == "linear.snapshot"), "{sent:?}");
    assert!(!sent.iter().any(|m| m == "pane.set_title"), "{sent:?}");
}
