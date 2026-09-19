//! `linear.bind_handoff`: a validated bind line carried in `agent.start`'s argv
//! into an unfocused `bind` tab, with the tab closed on any later failure.

use super::*;
use crate::ops::bind_handoff::{
    agent_name, bind_handoff, HandoffRunner, AGENT_NAME_MAX, AGENT_START_BUSY_BUDGET,
    BIND_AGENT_KIND, BIND_TAB_LABEL,
};
use board_core::protocol::{LinearBindHandoffParams, LinearBindHandoffResult};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A home holding `projects/app` and `worktrees/card-1`, the two roots'
/// default spellings.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects/app")).unwrap();
        std::fs::create_dir_all(dir.path().join("worktrees/card-1")).unwrap();
        Home { dir }
    }

    fn path(&self, relative: &str) -> String {
        self.dir.path().join(relative).display().to_string()
    }

    fn canonical(&self, relative: &str) -> String {
        std::fs::canonicalize(self.dir.path().join(relative))
            .unwrap()
            .display()
            .to_string()
    }

    fn env(&self) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), self.path(""));
        env
    }
}

type Sleeps = Arc<Mutex<Vec<Duration>>>;

fn runner(env: BTreeMap<String, String>) -> (HandoffRunner, Sleeps) {
    let sleeps: Sleeps = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&sleeps);
    let runner = HandoffRunner {
        env,
        sleep: Arc::new(move |d| recorded.lock().unwrap().push(d)),
        cancelled: Arc::new(|| false),
    };
    (runner, sleeps)
}

fn params(socket: &Path, dir: Option<String>) -> LinearBindHandoffParams {
    LinearBindHandoffParams {
        space: "w2".into(),
        project: "proj-1".into(),
        view: None,
        issue: None,
        working_directory: dir,
        origin_socket: socket.display().to_string(),
    }
}

/// How `agent.start` answers: the index is the zero-based `agent.start` count.
type StartAnswer = fn(&Value, usize) -> Value;

fn started(req: &Value, _: usize) -> Value {
    let pane = req["params"]["pane_id"].as_str().unwrap().to_string();
    testkit::agent_started(req, &pane, true, false)
}

/// A session listing spaces `w1` and `w2`, numbering each new tab `w2:tN` with
/// root pane `w2:pN`, and answering `agent.start` with `start`.
fn session(start: StartAnswer) -> FakeHerdr {
    let tabs = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(AtomicUsize::new(0));
    testkit::herdr_server()
        .on("workspace.list", |req| {
            let space = |id: &str| {
                json!({"workspace_id": id, "label": id, "number": 1,
                       "focused": false, "agent_status": "idle"})
            };
            testkit::reply(
                req,
                json!({"type": "workspace_list", "workspaces": [space("w1"), space("w2")]}),
            )
        })
        .on("tab.create", move |req| {
            let n = tabs.fetch_add(1, Ordering::SeqCst) + 2;
            let tab_id = format!("w2:T{n}");
            let mut root = testkit::pane_info(&format!("w2:p{n}"));
            root["tab_id"] = json!(tab_id);
            testkit::reply(
                req,
                json!({
                    "type": "tab_created",
                    "tab": {"tab_id": tab_id, "workspace_id": "w2", "number": n,
                            "label": "bind", "focused": false, "pane_count": 1,
                            "agent_status": "unknown"},
                    "root_pane": root
                }),
            )
        })
        .on("agent.start", move |req| {
            start(req, starts.fetch_add(1, Ordering::SeqCst))
        })
        .on("pane.close", |req| {
            testkit::reply(req, json!({"type": "ok"}))
        })
        .serve()
}

fn call(
    _herdr: &FakeHerdr,
    env: BTreeMap<String, String>,
    p: LinearBindHandoffParams,
) -> (board_core::Result<LinearBindHandoffResult>, Sleeps) {
    let (runner, sleeps) = runner(env);
    (bind_handoff(&runner, p), sleeps)
}

#[test]
fn bind_handoff_creates_one_unfocused_bind_tab_and_starts_claude_with_the_bind_line() {
    let home = Home::new();
    let herdr = session(started);
    let mut p = params(&herdr.socket, Some(home.path("projects/app")));
    p.view = Some("view-9".into());

    let (result, sleeps) = call(&herdr, home.env(), p);

    assert_eq!(
        result.unwrap(),
        LinearBindHandoffResult {
            tab_id: "w2:T2".into(),
            pane_id: "w2:p2".into(),
        }
    );
    assert_eq!(
        herdr.methods(),
        vec!["ping", "workspace.list", "tab.create", "agent.start"]
    );
    assert!(sleeps.lock().unwrap().is_empty());

    let tab = &herdr.requests_for("tab.create")[0]["params"];
    assert_eq!(tab["workspace_id"], "w2");
    assert_eq!(tab["focus"], false);
    assert_eq!(tab["label"], BIND_TAB_LABEL);
    assert_eq!(tab["label"], "bind");
    assert!(
        tab.get("env")
            .is_none_or(|env| env.as_object().is_some_and(|o| o.is_empty())),
        "the bind tab must get an empty environment: {tab}"
    );
    assert_eq!(tab["cwd"], home.canonical("projects/app"));

    let start = &herdr.requests_for("agent.start")[0]["params"];
    assert_eq!(start["kind"], BIND_AGENT_KIND);
    assert_eq!(start["kind"], "claude");
    assert_eq!(start["name"], "bind-w2-t2");
    assert_eq!(start["pane_id"], "w2:p2");
    assert_eq!(
        start["args"],
        json!(["/work:bind --space w2 --project proj-1 --view view-9"])
    );
}

#[test]
fn bind_handoff_line_is_one_interactive_argument_with_no_headless_flag() {
    let home = Home::new();
    let herdr = session(started);
    let mut p = params(&herdr.socket, None);
    p.issue = Some("ENG-42".into());

    let (result, _) = call(&herdr, home.env(), p);
    result.unwrap();

    let start = &herdr.requests_for("agent.start")[0]["params"];
    let args: Vec<&str> = start["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(
        args,
        vec!["/work:bind --space w2 --project proj-1 --issue ENG-42"]
    );
    let line = args[0];
    assert!(
        !line.contains(['\n', '\r', '\t']),
        "the bind line must stay one line: {line:?}"
    );
    const NON_INTERACTIVE: &[&str] = &[
        "-p",
        "--print",
        "--output-format",
        "--input-format",
        "--dangerously-skip-permissions",
        "--permission-mode",
    ];
    for arg in &args {
        for word in arg.split_whitespace() {
            assert!(
                !NON_INTERACTIVE.contains(&word),
                "the handoff session must be interactive; found {word} in {args:?}"
            );
        }
    }
    assert!(
        args.iter().skip(1).all(|a| !a.starts_with('-')),
        "no flag may follow the bind line: {args:?}"
    );
}

#[test]
fn bind_handoff_without_a_working_directory_opens_in_the_projects_root() {
    let home = Home::new();
    let herdr = session(started);

    let (result, _) = call(&herdr, home.env(), params(&herdr.socket, None));
    result.unwrap();

    let tab = &herdr.requests_for("tab.create")[0]["params"];
    assert_eq!(tab["cwd"], home.canonical("projects"));
    let start = &herdr.requests_for("agent.start")[0]["params"];
    assert_eq!(
        start["args"],
        json!(["/work:bind --space w2 --project proj-1"])
    );
}

#[test]
fn bind_handoff_accepts_a_card_worktree_under_the_worktrees_root() {
    let home = Home::new();
    let herdr = session(started);

    let (result, _) = call(
        &herdr,
        home.env(),
        params(&herdr.socket, Some(home.path("worktrees/card-1"))),
    );
    result.unwrap();

    let tab = &herdr.requests_for("tab.create")[0]["params"];
    assert_eq!(tab["cwd"], home.canonical("worktrees/card-1"));
}

#[test]
fn bind_handoff_reads_the_root_overrides_as_the_plugin_does() {
    let home = Home::new();
    std::fs::create_dir_all(home.path("elsewhere/wt/card-2")).unwrap();
    std::fs::create_dir_all(home.path("slate/repo")).unwrap();
    let herdr = session(started);
    let mut env = home.env();
    env.insert(
        "HERDR_LINEAR_WORKTREES_ROOT".into(),
        home.path("elsewhere/wt"),
    );
    env.insert("HERDR_LINEAR_SLATE_ROOT".into(), home.path("slate"));

    let (result, _) = call(
        &herdr,
        env.clone(),
        params(&herdr.socket, Some(home.path("elsewhere/wt/card-2"))),
    );
    result.unwrap();
    let (result, _) = call(
        &herdr,
        env.clone(),
        params(&herdr.socket, Some(home.path("slate/repo"))),
    );
    result.unwrap();

    // The override replaces the default rather than adding to it.
    let (result, _) = call(
        &herdr,
        env,
        params(&herdr.socket, Some(home.path("worktrees/card-1"))),
    );
    assert_eq!(result.unwrap_err().code(), 1);
    assert_eq!(herdr.count("tab.create"), 2);
}

#[test]
fn bind_handoff_refuses_hostile_ids_before_any_herdr_call() {
    let home = Home::new();
    let herdr = session(started);
    let long = "a".repeat(65);
    let hostile: &[&str] = &[
        "-rf", "--exec", ".hidden", "..", "a\nb", "a b", "", &long, "a;b", "a/b",
    ];
    type Field = fn(&mut LinearBindHandoffParams, String);
    let fields: &[(&str, Field)] = &[
        ("space", |p, v| p.space = v),
        ("project", |p, v| p.project = v),
        ("view", |p, v| p.view = Some(v)),
        ("issue", |p, v| p.issue = Some(v)),
    ];
    for (field, set) in fields {
        for id in hostile {
            let mut p = params(&herdr.socket, None);
            set(&mut p, id.to_string());
            let (result, _) = call(&herdr, home.env(), p);
            let err = result.expect_err(&format!("{field} {id:?} was accepted"));
            assert_eq!(err.code(), 1, "{field} {id:?}: {err}");
        }
    }
    assert!(
        herdr.methods().is_empty(),
        "herdr was contacted: {:?}",
        herdr.methods()
    );
}

#[test]
fn bind_handoff_accepts_the_longest_permitted_id() {
    let home = Home::new();
    let herdr = session(started);
    let mut p = params(&herdr.socket, None);
    p.project = format!("A{}", "b".repeat(63));
    let (result, _) = call(&herdr, home.env(), p);
    result.unwrap();
}

#[test]
fn bind_handoff_refuses_a_view_and_an_issue_together() {
    let home = Home::new();
    let herdr = session(started);
    let mut p = params(&herdr.socket, None);
    p.view = Some("view-9".into());
    p.issue = Some("ENG-42".into());

    let (result, _) = call(&herdr, home.env(), p);

    let err = result.unwrap_err();
    assert_eq!(err.code(), 1);
    assert!(err.to_string().contains("view"), "message: {err}");
    assert!(herdr.methods().is_empty());
}

#[test]
fn bind_handoff_refuses_relative_outside_missing_and_escaping_directories() {
    let home = Home::new();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path("projectsOther")).unwrap();
    std::os::unix::fs::symlink(outside.path(), home.path("projects/escape")).unwrap();
    std::fs::write(home.path("projects/file"), "x").unwrap();
    let herdr = session(started);

    for dir in [
        "projects/app".to_string(),
        "./projects/app".to_string(),
        outside.path().display().to_string(),
        home.path("projectsOther"),
        home.path("projects/escape"),
        home.path("projects/missing"),
        home.path("projects/file"),
        home.path(""),
        String::new(),
    ] {
        let (result, _) = call(&herdr, home.env(), params(&herdr.socket, Some(dir.clone())));
        let err = result.expect_err(&format!("{dir:?} was accepted"));
        assert_eq!(err.code(), 1, "{dir:?}: {err}");
    }
    assert!(herdr.methods().is_empty(), "herdr was contacted");
}

#[test]
fn bind_handoff_refuses_a_space_the_session_does_not_list() {
    let home = Home::new();
    let herdr = session(started);
    let mut p = params(&herdr.socket, None);
    p.space = "w9".into();

    let (result, _) = call(&herdr, home.env(), p);

    let err = result.unwrap_err();
    assert!(err.to_string().contains("w9"), "message: {err}");
    assert_eq!(herdr.methods(), vec!["ping", "workspace.list"]);
}

#[test]
fn bind_handoff_reports_an_unreachable_socket_as_herdr_unavailable() {
    let home = Home::new();
    let (runner, sleeps) = runner(home.env());
    let err = bind_handoff(
        &runner,
        params(Path::new("/tmp/no-such-herdr-bind.sock"), None),
    )
    .unwrap_err();
    assert_eq!(err.code(), 4);
    assert!(sleeps.lock().unwrap().is_empty());
}

#[test]
fn bind_handoff_closes_the_tab_when_agent_start_fails() {
    let home = Home::new();
    let herdr = session(|req, _| testkit::error(req, "unsupported_agent_kind", "no claude"));

    let (result, _) = call(&herdr, home.env(), params(&herdr.socket, None));

    let err = result.unwrap_err();
    assert_eq!(err.code(), 4);
    assert!(err.to_string().contains("agent.start"), "message: {err}");
    assert_eq!(
        herdr.methods(),
        vec![
            "ping",
            "workspace.list",
            "tab.create",
            "agent.start",
            "pane.close"
        ]
    );
    assert_eq!(
        herdr.requests_for("pane.close")[0]["params"]["pane_id"],
        "w2:p2"
    );
}

#[test]
fn bind_handoff_closes_the_tab_when_the_busy_retry_is_exhausted() {
    let home = Home::new();
    let herdr = session(|req, _| testkit::error(req, "agent_pane_busy", "pane is busy"));

    let (result, sleeps) = call(&herdr, home.env(), params(&herdr.socket, None));

    let err = result.unwrap_err();
    assert_eq!(err.code(), 4);
    assert!(err.to_string().contains("agent.start"), "message: {err}");
    let slept: Duration = sleeps.lock().unwrap().iter().sum();
    assert!(
        slept >= AGENT_START_BUSY_BUDGET - Duration::from_secs(5)
            && slept <= AGENT_START_BUSY_BUDGET,
        "the busy retry must wait about its budget, waited {slept:?}"
    );
    assert!(herdr.count("agent.start") > 5, "the retry gave up early");
    assert_eq!(herdr.count("pane.close"), 1);
    assert_eq!(
        herdr.requests_for("pane.close")[0]["params"]["pane_id"],
        "w2:p2"
    );
    assert_eq!(herdr.methods().last().unwrap(), "pane.close");
}

#[test]
fn bind_handoff_succeeds_once_a_busy_pane_accepts_the_start() {
    let home = Home::new();
    let herdr = session(|req, n| {
        if n < 3 {
            testkit::error(req, "agent_pane_busy", "pane is busy")
        } else {
            started(req, n)
        }
    });

    let (result, sleeps) = call(&herdr, home.env(), params(&herdr.socket, None));

    assert_eq!(result.unwrap().pane_id, "w2:p2");
    assert_eq!(herdr.count("agent.start"), 4);
    assert_eq!(sleeps.lock().unwrap().len(), 3);
    assert_eq!(herdr.count("pane.close"), 0);
    let names: Vec<Value> = herdr
        .requests_for("agent.start")
        .iter()
        .map(|r| r["params"].clone())
        .collect();
    assert!(
        names.windows(2).all(|w| w[0] == w[1]),
        "retries must resend the same start"
    );
}

#[test]
fn bind_handoff_stops_retrying_and_closes_the_tab_when_cancelled() {
    let home = Home::new();
    let herdr = session(|req, _| testkit::error(req, "agent_pane_busy", "pane is busy"));
    let (mut runner, _) = runner(home.env());
    runner.cancelled = Arc::new(|| true);

    let err = bind_handoff(&runner, params(&herdr.socket, None)).unwrap_err();

    assert_eq!(err.code(), 4);
    assert_eq!(herdr.count("agent.start"), 1);
    assert_eq!(herdr.count("pane.close"), 1);
}

#[test]
fn bind_handoff_twice_in_a_row_starts_two_differently_named_agents() {
    let home = Home::new();
    let herdr = session(started);

    let (first, _) = call(&herdr, home.env(), params(&herdr.socket, None));
    let (second, _) = call(&herdr, home.env(), params(&herdr.socket, None));

    assert_ne!(first.unwrap(), second.unwrap());
    let names: Vec<String> = herdr
        .requests_for("agent.start")
        .iter()
        .map(|r| r["params"]["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["bind-w2-t2", "bind-w2-t3"]);
    assert_eq!(herdr.count("tab.create"), 2);
    assert_eq!(herdr.count("pane.close"), 0);
}

#[test]
fn bind_handoff_is_routed_and_validates_before_touching_herdr() {
    let herdr = session(started);
    let d = test_daemon(Config::default());

    let err = handle_request(
        &d,
        "linear.bind_handoff",
        json!({"space": "--exec", "project": "p", "origin_socket": herdr.socket}),
    )
    .unwrap_err();

    assert_eq!(err.code(), 1);
    assert!(
        !err.to_string().contains("unknown method"),
        "message: {err}"
    );
    assert!(herdr.methods().is_empty());
}

#[test]
fn bind_handoff_agent_names_follow_the_herdr_name_rule() {
    assert_eq!(agent_name("w2:t2"), "bind-w2-t2");
    assert_eq!(agent_name("W_2:T-2.x"), "bind-w_2-t-2-x");
    let long = agent_name(&"Tab:".repeat(20));
    assert_eq!(long.len(), AGENT_NAME_MAX);
    assert_eq!(AGENT_NAME_MAX, 32);
    for name in [long, agent_name("é:ü"), agent_name("")] {
        assert!(name.starts_with("bind-"), "{name}");
        assert!(
            name.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
            "{name}"
        );
    }
}

#[test]
fn a_bind_handoff_client_waits_longer_than_the_daemon_can_take_to_answer() {
    let herdr = board_herdr::SocketDeadlines::default();
    // ping, workspace.list, tab.create, the last agent.start and pane.close,
    // each bounded by the request deadline.
    let herdr_calls = 5;
    let daemon_longest = herdr.connect
        + herdr.handshake
        + herdr.request * herdr_calls
        + AGENT_START_BUSY_BUDGET
        + Duration::from_secs(5);
    assert!(
        board_core::protocol::LINEAR_BIND_HANDOFF_CLIENT_TIMEOUT > daemon_longest,
        "client {:?} vs daemon {:?}",
        board_core::protocol::LINEAR_BIND_HANDOFF_CLIENT_TIMEOUT,
        daemon_longest
    );
}
