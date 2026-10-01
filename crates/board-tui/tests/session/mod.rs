//! The session side pane: one agent's bound issue, its lane list
//! or the bind hint, read through `linear.session.get` and never written.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::db::SpaceBinding;
use board_core::protocol::{
    Event, LinearActivityRecordParams, LinearBindParams, LinearMarkSetParams, LinearSnapshot,
    LinearStateGetParams, MarkKind,
};
use board_tui::app::{LocalStateSignals, Mode, Screen};
use board_tui::testkit::{draw, key, linear_fixture, methods, session_driver, RecordingClient};
use board_tui::{Driver, SessionIdentity};
use crossterm::event::KeyCode;
use serde_json::Value;

const SOCKET: &str = "/tmp/hb/sessions/main/herdr.sock";
/// A split beside an agent: about half a terminal wide.
const W: u16 = 60;
const H: u16 = 30;
const HINT: &str = "not bound — /work:bind";
const READS: [&str; 4] = [
    "linear.session.get",
    "linear.snapshot",
    "linear.state.get",
    "linear.issue",
];

#[derive(Clone)]
struct Shared {
    inner: Arc<Mutex<FakeBoardClient>>,
    down: Arc<AtomicBool>,
}

impl BoardClient for Shared {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        if self.down.load(Ordering::SeqCst) {
            anyhow::bail!("connection refused");
        }
        self.inner.lock().unwrap().call(method, params)
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        self.inner.lock().unwrap().subscribe()
    }
}

/// An agent in space `wA` of herdr session `main`, working in its own git
/// worktree, with the space bound to a Linear project.
struct Agent {
    shared: Shared,
    worktree: tempfile::TempDir,
}

impl Agent {
    fn new(snapshot: LinearSnapshot) -> Agent {
        let client = FakeBoardClient::new()
            .unwrap()
            .with_linear_snapshot(snapshot);
        client
            .db()
            .set_space_binding(&SpaceBinding {
                herdr_session: "main".into(),
                space: "wA".into(),
                project_id: "proj-1".into(),
                display_name: None,
                team_ids: Vec::new(),
                view: None,
            })
            .unwrap();
        let worktree = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(worktree.path().join(".git")).unwrap();
        std::fs::create_dir_all(worktree.path().join("src")).unwrap();
        Agent {
            shared: Shared {
                inner: Arc::new(Mutex::new(client)),
                down: Arc::new(AtomicBool::new(false)),
            },
            worktree,
        }
    }

    fn root(&self) -> String {
        std::fs::canonicalize(self.worktree.path())
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn bind(&self, issue: &str) {
        self.shared
            .inner
            .lock()
            .unwrap()
            .linear_bind(&LinearBindParams {
                cwd: self.root(),
                issue: issue.into(),
                ..LinearBindParams::default()
            })
            .unwrap();
    }

    fn attention(&self, issue: &str) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner
            .linear_activity_record(&LinearActivityRecordParams {
                tool_name: "mcp__linear__save_comment".into(),
                issue: Some(issue.into()),
                space: Some("wA".into()),
                claims: board_core::db::ActivityClaims {
                    herdr_socket: Some(SOCKET.into()),
                    ..Default::default()
                },
                ..LinearActivityRecordParams::default()
            })
            .unwrap();
        inner
            .linear_mark_set(&LinearMarkSetParams {
                space: "wA".into(),
                issue: issue.into(),
                kind: MarkKind::Attention,
                text: Some("look here".into()),
                created_by: Some("agent-a".into()),
                owner: board_core::db::LinearOwner {
                    herdr_socket: Some(SOCKET.into()),
                    ..Default::default()
                },
            })
            .unwrap();
    }

    fn marks_on(&self, issue: &str) -> usize {
        self.shared
            .inner
            .lock()
            .unwrap()
            .linear_state_get(&LinearStateGetParams {
                space: "wA".into(),
                herdr_socket: Some(SOCKET.into()),
            })
            .unwrap()
            .marks
            .iter()
            .filter(|m| m.issue == issue)
            .count()
    }

    fn identity(&self) -> SessionIdentity {
        SessionIdentity {
            space: "wA".into(),
            herdr_socket: Some(SOCKET.into()),
            herdr_pane_id: Some("wA:p1".into()),
            claude_session_id: Some("claude-1".into()),
            cwd: Some(format!("{}/src", self.root())),
        }
    }

    fn open(&self) -> (Driver, board_tui::testkit::RequestLog) {
        let (client, log) = RecordingClient::new(self.shared.clone());
        (session_driver(client, self.identity()), log)
    }
}

fn changed() -> LocalStateSignals {
    let mut signals = LocalStateSignals::default();
    signals.add(Some("wA".into()), false);
    signals
}

fn frame(d: &Driver) -> String {
    draw(&d.app, W, H)
}

fn press(d: &mut Driver, code: KeyCode) {
    d.handle(key(code));
}

#[test]
fn ae7_an_unbound_session_shows_the_bind_hint() {
    let agent = Agent::new(linear_fixture("bound-with-view"));
    let (d, log) = agent.open();
    assert_eq!(d.app.mode, Mode::Session);
    assert_eq!(d.app.screen, Screen::SessionPane);
    let text = frame(&d);
    assert!(text.contains(HINT), "{text}");
    assert!(!text.contains("WEB-3302"), "{text}");
    let sent = methods(&log);
    assert!(sent.contains(&"linear.session.get".to_string()), "{sent:?}");
    let params = log.lock().unwrap()[0].1.clone();
    assert_eq!(params["herdr_pane_id"], "wA:p1");
    assert_eq!(params["herdr_socket"], SOCKET);
}

#[test]
fn ae7_a_bound_session_on_a_board_without_lanes_flips_to_the_whole_tab_by_column() {
    let agent = Agent::new(linear_fixture("bound-with-view"));
    agent.bind("WEB-3302");
    let (mut d, _) = agent.open();
    let issue = frame(&d);
    assert!(issue.contains("WEB-3302"), "{issue}");
    assert!(!issue.contains("Backlog ("), "opens on the issue:\n{issue}");

    press(&mut d, KeyCode::Tab);
    let list = frame(&d);
    for text in [
        "whole tab",
        "Backlog (1)",
        "WEB-3308",
        "Todo (1)",
        "WEB-3307",
        "In Progress (1)",
        "WEB-3302",
    ] {
        assert!(list.contains(text), "{text}:\n{list}");
    }
    let order: Vec<usize> = ["Backlog (1)", "Todo (1)", "In Progress (1)"]
        .iter()
        .map(|c| list.find(c).unwrap())
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{list}");

    press(&mut d, KeyCode::Tab);
    assert!(!frame(&d).contains("Backlog ("), "Tab flips back");
}

#[test]
fn a_session_bound_inside_a_lane_lists_that_lane_by_column() {
    let agent = Agent::new(linear_fixture("bound-tabs-lanes"));
    agent.bind("WEB-106");
    let (mut d, _) = agent.open();
    press(&mut d, KeyCode::Tab);
    let list = frame(&d);
    for text in ["lane: Beta", "WEB-104", "WEB-106", "WEB-107"] {
        assert!(list.contains(text), "{text}:\n{list}");
    }
    for other in ["WEB-101", "WEB-105", "WEB-201"] {
        assert!(!list.contains(other), "{other} is in another lane:\n{list}");
    }
}

#[test]
fn a_binding_made_while_open_switches_the_hint_to_the_issue_on_the_next_event() {
    let agent = Agent::new(linear_fixture("bound-with-view"));
    let (mut d, _) = agent.open();
    assert!(frame(&d).contains(HINT));

    agent.bind("WEB-3302");
    d.on_local_state_changed(changed());

    let text = frame(&d);
    assert!(!text.contains(HINT), "{text}");
    assert!(text.contains("WEB-3302"), "{text}");
}

#[test]
fn an_attention_mark_shows_in_the_pane_and_stays_on_the_board() {
    let agent = Agent::new(linear_fixture("bound-with-view"));
    agent.bind("WEB-3302");
    let (mut d, log) = agent.open();
    agent.attention("WEB-3302");
    d.on_local_state_changed(changed());

    let text = frame(&d);
    assert!(text.contains("! agent-a  look here"), "{text}");
    for code in [
        KeyCode::Enter,
        KeyCode::Char('a'),
        KeyCode::Char('x'),
        KeyCode::Char('n'),
        KeyCode::Char('b'),
        KeyCode::Char('o'),
        KeyCode::Char('u'),
        KeyCode::Char('y'),
        KeyCode::Char('r'),
        KeyCode::Char('R'),
        KeyCode::Char('s'),
        KeyCode::Char('v'),
        KeyCode::Tab,
        KeyCode::Char('j'),
        KeyCode::Enter,
        KeyCode::Char('a'),
        KeyCode::Char('x'),
    ] {
        press(&mut d, code);
    }
    let sent = methods(&log);
    assert!(
        sent.iter().all(|m| READS.contains(&m.as_str())),
        "the pane sent something other than a read: {sent:?}"
    );
    assert_eq!(agent.marks_on("WEB-3302"), 1, "the mark is still there");
    assert!(!d.app.should_quit);
    press(&mut d, KeyCode::Char('q'));
    assert!(d.app.should_quit);
}

#[test]
fn help_lists_only_the_session_keys_and_closes_back_to_the_pane() {
    let agent = Agent::new(linear_fixture("bound-with-view"));
    agent.bind("WEB-3302");
    let (mut d, _) = agent.open();
    press(&mut d, KeyCode::Char('?'));
    assert_eq!(d.app.screen, Screen::Help);
    let help = frame(&d);
    for text in ["issue / lane list", "move / scroll", "close the pane"] {
        assert!(help.contains(text), "{text}:\n{help}");
    }
    for board in ["focus column", "clears its marks", "focus the spaces strip"] {
        assert!(!help.contains(board), "{board}:\n{help}");
    }
    press(&mut d, KeyCode::Char('x'));
    assert_eq!(d.app.screen, Screen::SessionPane);
    assert!(!d.app.should_quit);
}

#[test]
fn not_imported_renders_the_daemons_message_in_the_split() {
    let mut snapshot = linear_fixture("unbound");
    snapshot.linear.status = "not_imported".into();
    snapshot.linear.message =
        Some("the work store has not been imported; run `board import work-store`".into());
    let agent = Agent::new(snapshot);
    let (d, _) = agent.open();
    let text = frame(&d);
    // Wrapped to the split's width, so its two halves land on two rows.
    assert!(
        text.contains("has not been imported; run `board import"),
        "{text}"
    );
    assert!(text.contains("work-store`"), "{text}");
    assert!(!text.contains("not_imported"), "{text}");
}

#[test]
fn a_daemon_disconnect_keeps_the_last_read() {
    let agent = Agent::new(linear_fixture("bound-with-view"));
    agent.bind("WEB-3302");
    let (mut d, _) = agent.open();
    agent.shared.down.store(true, Ordering::SeqCst);
    d.on_local_state_changed(changed());
    d.on_daemon_signals(false, true);
    let text = frame(&d);
    assert!(text.contains("WEB-3302"), "{text}");
    assert!(text.contains("last read kept"), "{text}");
    assert!(!text.contains(HINT), "{text}");
}
