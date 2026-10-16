//! `board mcp`: the stdio MCP server driven over its real stdin/stdout with
//! hand-written JSON-RPC, against a real daemon or a recording fake boardd.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::{Request, Response};
use serde_json::{json, Value};

use super::{fixtures_dir, scripted_boardd, TestDaemon, BOARD_BIN};

const TOOLS: [&str; 12] = [
    "state",
    "panes_for_issue",
    "bind",
    "unbind",
    "mark",
    "unmark",
    "note",
    "notify",
    "ask_to_show",
    "withdraw_show",
    "open_board",
    "close_board",
];
const READ_TOOLS: [&str; 2] = ["state", "panes_for_issue"];
const REPLY_WAIT: Duration = Duration::from_secs(20);

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    seen: Vec<String>,
    stderr: Option<std::thread::JoinHandle<String>>,
    next_id: i64,
}

/// Environment for one `board mcp` process: the board paths plus whatever
/// herdr and card claims the test sets. Ambient claims from a suite that
/// itself runs in a herdr pane are removed.
struct McpEnv<'a> {
    socket: &'a Path,
    db: PathBuf,
    config: PathBuf,
    home: &'a Path,
    cwd: &'a Path,
    claims: &'a [(&'a str, &'a str)],
}

impl Mcp {
    fn spawn(env: McpEnv<'_>) -> Mcp {
        let mut cmd = Command::new(BOARD_BIN);
        cmd.arg("mcp")
            .current_dir(env.cwd)
            .env("BOARD_SOCKET", env.socket)
            .env("BOARD_DB", &env.db)
            .env("HERDR_BOARD_CONFIG", &env.config)
            .env("HOME", env.home)
            .env("BOARD_SPAWNER", "local")
            .env("BOARD_BIN", BOARD_BIN)
            .env_remove("BOARD_SCOPE_PATH")
            .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_TAB_ID")
            .env_remove("BOARD_CARD_ID")
            .env_remove("BOARD_RUN_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env.claims {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().expect("spawn board mcp");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("mcp stdout");
        let mut stderr = child.stderr.take().expect("mcp stderr");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let stderr = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        Mcp {
            child,
            stdin,
            lines,
            seen: Vec::new(),
            stderr: Some(stderr),
            next_id: 0,
        }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("mcp stdin open");
        let mut wire = serde_json::to_string(message).unwrap();
        wire.push('\n');
        stdin
            .write_all(wire.as_bytes())
            .expect("write to board mcp");
        stdin.flush().expect("flush board mcp stdin");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self
                .lines
                .recv_timeout(REPLY_WAIT)
                .unwrap_or_else(|_| panic!("no reply to {method}; seen: {:?}", self.seen));
            self.seen.push(line.clone());
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    fn initialize(&mut self) -> Value {
        let reply = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "board-mcp-test", "version": "0"}
            }),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        reply["result"].clone()
    }

    fn tools(&mut self) -> Vec<Value> {
        let reply = self.request("tools/list", json!({}));
        reply["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools/list result: {reply}"))
            .clone()
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let reply = self.request("tools/call", json!({"name": tool, "arguments": arguments}));
        reply
            .get("result")
            .unwrap_or_else(|| {
                panic!("{tool} answered a protocol error, not a tool result: {reply}")
            })
            .clone()
    }

    /// Close stdin, let the server exit, and return every stdout line it
    /// wrote, asserting each one is a JSON-RPC 2.0 message.
    fn finish(mut self) -> Vec<String> {
        drop(self.stdin.take());
        let status = self.child.wait().expect("wait for board mcp");
        self.seen.extend(self.lines.try_iter());
        let stderr = self
            .stderr
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();
        assert!(
            status.success(),
            "board mcp exited {status}; stderr: {stderr}"
        );
        for line in &self.seen {
            let message: Value = serde_json::from_str(line).unwrap_or_else(|_| {
                panic!("non-JSON line on board mcp stdout: {line:?}; stderr: {stderr}")
            });
            assert_eq!(message["jsonrpc"], "2.0", "not JSON-RPC: {line}");
        }
        std::mem::take(&mut self.seen)
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn for_daemon<'a>(
    td: &'a TestDaemon,
    cwd: &'a Path,
    claims: &'a [(&'a str, &'a str)],
) -> McpEnv<'a> {
    McpEnv {
        socket: &td.socket,
        db: td._dir.path().join("board.db"),
        config: td._dir.path().join("config.toml"),
        home: td._dir.path(),
        cwd,
        claims,
    }
}

fn structured(result: &Value) -> &Value {
    assert_eq!(result["isError"], false, "tool failed: {result}");
    &result["structuredContent"]
}

fn error_text(result: &Value) -> String {
    assert_eq!(result["isError"], true, "expected a tool error: {result}");
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool error without text: {result}"))
        .to_string()
}

fn git_worktree(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(path.join(".git")).unwrap();
    path.canonicalize().unwrap()
}

#[test]
fn initialize_then_tools_list_returns_the_twelve_tools_with_read_only_hints_on_reads() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let mut mcp = Mcp::spawn(McpEnv {
        socket: &socket,
        db: dir.path().join("board.db"),
        config: dir.path().join("missing-config.toml"),
        home: dir.path(),
        cwd: dir.path(),
        claims: &[],
    });

    let info = mcp.initialize();
    let name = info["serverInfo"]["name"].as_str().unwrap();
    assert_eq!(name, "board");
    assert!(
        !name.to_ascii_lowercase().contains("linear"),
        "the work plugin's Linear hook matcher must not match this server"
    );
    assert!(info["capabilities"]["tools"].is_object(), "{info}");

    let tools = mcp.tools();
    let names: BTreeSet<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOLS.into_iter().collect::<BTreeSet<_>>());
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        let read_only = tool["annotations"]["readOnlyHint"] == json!(true);
        assert_eq!(
            read_only,
            READ_TOOLS.contains(&name),
            "{name} readOnlyHint: {tool}"
        );
        assert_eq!(tool["inputSchema"]["type"], "object", "{name}: {tool}");
        assert!(
            tool["inputSchema"]["properties"]["pane"].is_object(),
            "{name} takes the caller's pane: {tool}"
        );
    }
    let open = tools.iter().find(|t| t["name"] == "open_board").unwrap();
    let placement = &open["inputSchema"]["properties"]["placement"];
    assert!(
        placement.get("enum").is_none() && placement["type"].to_string().contains("string"),
        "placement must reach the daemon as free text so a refused placement is a tool error, \
         not a JSON-RPC invalid-params error: {placement}"
    );

    mcp.finish();
    assert!(
        !socket.exists(),
        "initialize and tools/list must not start boardd"
    );
}

/// Stops the daemon a tool call auto-started, since no `TestDaemon` owns it.
struct StopOnDrop(PathBuf);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        if let Ok(mut client) = UnixClient::connect(&self.0) {
            let _ = client.daemon_stop();
        }
    }
}

#[test]
fn with_no_daemon_tools_list_starts_nothing_and_the_first_tool_call_starts_it() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[harness.fake]\nargv = [\"bash\", \"{}\"]\n\n[daemon]\nspawner = \"local\"\n",
            fixtures_dir().join("fake-agent.sh").display()
        ),
    )
    .unwrap();
    let _stop = StopOnDrop(socket.clone());
    let mut mcp = Mcp::spawn(McpEnv {
        socket: &socket,
        db: dir.path().join("board.db"),
        config,
        home: dir.path(),
        cwd: dir.path(),
        claims: &[],
    });

    mcp.initialize();
    assert_eq!(mcp.tools().len(), TOOLS.len());
    assert!(!socket.exists(), "tools/list must not start boardd");

    let state = mcp.call("state", json!({"space": "w1"}));
    assert_eq!(structured(&state)["space"], "w1");
    assert!(socket.exists(), "the first tool call starts boardd");
    UnixClient::connect(&socket)
        .expect("the started daemon answers")
        .daemon_status()
        .expect("daemon.status");

    mcp.finish();
}

#[test]
fn nothing_but_json_rpc_reaches_stdout_during_tool_calls() {
    let td = TestDaemon::start(&[]);
    let repo = tempfile::tempdir().unwrap();
    let worktree = git_worktree(repo.path(), "wt");
    let claims = [("HERDR_WORKSPACE_ID", "w1")];
    let mut mcp = Mcp::spawn(for_daemon(&td, &worktree, &claims));

    mcp.initialize();
    mcp.tools();
    structured(&mcp.call("state", json!({})));
    structured(&mcp.call("bind", json!({"issue": "ENG-1"})));
    structured(&mcp.call("panes_for_issue", json!({"issue": "ENG-1"})));
    error_text(&mcp.call(
        "mark",
        json!({"issue": "not an issue", "kind": "needs_you"}),
    ));
    error_text(&mcp.call("close_board", json!({"issue": "ENG-1"})));

    let lines = mcp.finish();
    assert!(lines.len() >= 7, "every request was answered: {lines:?}");
}

#[test]
fn bind_returns_the_before_and_after_and_unbind_undoes_it() {
    let td = TestDaemon::start(&[]);
    let repo = tempfile::tempdir().unwrap();
    let worktree = git_worktree(repo.path(), "wt");
    let mut mcp = Mcp::spawn(for_daemon(&td, &worktree, &[]));
    mcp.initialize();

    let first = mcp.call("bind", json!({"issue": "ENG-1"}));
    let first = structured(&first);
    assert_eq!(first["before"], Value::Null);
    assert_eq!(first["after"]["issue"], "ENG-1");
    assert_eq!(
        first["after"]["worktree_path"],
        worktree.to_str().unwrap(),
        "cwd defaults to the server's own working directory"
    );

    let second = mcp.call("bind", json!({"issue": "ENG-2"}));
    let second = structured(&second);
    assert_eq!(second["before"]["issue"], "ENG-1");
    assert_eq!(second["after"]["issue"], "ENG-2");

    let unbound = mcp.call("unbind", json!({}));
    let unbound = structured(&unbound);
    assert_eq!(unbound["before"]["issue"], "ENG-2");
    assert_eq!(unbound["after"], Value::Null);

    mcp.finish();
}

/// A one-shot boardd stand-in that records the first request it receives and
/// answers it with `result`.
fn recording_boardd(socket: &Path, result: Value) -> Receiver<Request> {
    let listener = UnixListener::bind(socket).expect("bind recording boardd");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let request: Request = serde_json::from_str(line.trim_end()).expect("board request");
        let mut wire = serde_json::to_string(&Response::ok(request.id.clone(), result)).unwrap();
        wire.push('\n');
        let mut writer = stream;
        let _ = writer.write_all(wire.as_bytes());
        let _ = tx.send(request);
    });
    rx
}

#[test]
fn bind_passes_the_herdr_and_card_claims_from_its_environment() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let worktree = git_worktree(dir.path(), "wt");
    let change = json!({
        "before": null,
        "after": {
            "worktree_path": worktree.to_str().unwrap(),
            "issue": "ENG-7",
            "state": "bound",
            "branch": null,
            "tab": null,
            "display_name": null,
            "team_ids": [],
            "view": null,
            "carried": {}
        }
    });
    let requests = recording_boardd(&socket, change.clone());
    let claims = [
        ("HERDR_SOCKET_PATH", "/tmp/herdr-test.sock"),
        ("HERDR_PANE_ID", "w1:p2"),
        ("HERDR_WORKSPACE_ID", "w1"),
        ("BOARD_CARD_ID", "7"),
        // A rescued pane carries an empty run id on purpose.
        ("BOARD_RUN_ID", ""),
    ];
    let mut mcp = Mcp::spawn(McpEnv {
        socket: &socket,
        db: dir.path().join("board.db"),
        config: dir.path().join("missing-config.toml"),
        home: dir.path(),
        cwd: &worktree,
        claims: &claims,
    });
    mcp.initialize();

    let result = mcp.call("bind", json!({"issue": "ENG-7"}));
    assert_eq!(
        structured(&result),
        &change,
        "the daemon's before/after, unchanged"
    );

    let request = requests
        .recv_timeout(REPLY_WAIT)
        .expect("the fake boardd saw the bind");
    assert_eq!(request.method, "linear.bind");
    assert_eq!(request.params["issue"], "ENG-7");
    assert_eq!(request.params["cwd"], worktree.to_str().unwrap());
    assert_eq!(
        request.params["claims"],
        json!({
            "herdr_socket": "/tmp/herdr-test.sock",
            "herdr_pane_id": "w1:p2",
            "herdr_workspace_id": "w1",
            "card_id": 7,
            "run_id": null
        })
    );

    mcp.finish();
}

#[test]
fn open_board_with_an_overlay_placement_returns_a_tool_error() {
    let td = TestDaemon::start(&[]);
    let cwd = tempfile::tempdir().unwrap();
    let claims = [
        ("HERDR_SOCKET_PATH", "/tmp/herdr-test.sock"),
        ("HERDR_PANE_ID", "w1:p2"),
        ("HERDR_WORKSPACE_ID", "w1"),
    ];
    let mut mcp = Mcp::spawn(for_daemon(&td, cwd.path(), &claims));
    mcp.initialize();

    let result = mcp.call(
        "open_board",
        json!({"placement": "overlay", "issue": "ENG-1"}),
    );
    let text = error_text(&result);
    assert!(text.contains("overlay"), "{text}");

    mcp.finish();
}

#[test]
fn open_board_with_session_reaches_the_daemon_as_a_session_context() {
    let td = TestDaemon::start(&[]);
    let cwd = tempfile::tempdir().unwrap();
    let claims = [
        ("HERDR_SOCKET_PATH", "/tmp/herdr-test.sock"),
        ("HERDR_PANE_ID", "w1:p2"),
        ("HERDR_WORKSPACE_ID", "w1"),
    ];
    let mut mcp = Mcp::spawn(for_daemon(&td, cwd.path(), &claims));
    mcp.initialize();

    // A session pane takes no other target: the daemon's refusal names it,
    // which only a context carrying `session` can reach.
    let result = mcp.call("open_board", json!({"session": true, "issue": "ENG-1"}));
    let text = error_text(&result);
    assert!(text.contains("session pane"), "{text}");
    // Past the context check (a bare context is refused there) to the
    // caller's herdr socket, which this test does not run.
    let result = mcp.call("close_board", json!({"session": true}));
    let text = error_text(&result);
    assert!(text.contains("Herdr socket"), "{text}");

    mcp.finish();
}

/// Two agents that share a herdr pane (a nested or background session
/// inherits `HERDR_PANE_ID`) and differ only in their Claude session.
fn agent(session: &str) -> [(&'static str, String); 4] {
    [
        ("HERDR_SOCKET_PATH", "/tmp/herdr-test.sock".to_string()),
        ("HERDR_PANE_ID", "w1:p2".to_string()),
        ("HERDR_WORKSPACE_ID", "w1".to_string()),
        ("CLAUDE_CODE_SESSION_ID", session.to_string()),
    ]
}

fn borrowed<'a>(claims: &'a [(&'static str, String); 4]) -> [(&'static str, &'a str); 4] {
    claims.each_ref().map(|(key, value)| (*key, value.as_str()))
}

fn marks_on(state: &Value, issue: &str) -> Vec<Value> {
    state["marks"]
        .as_array()
        .unwrap_or_else(|| panic!("state without marks: {state}"))
        .iter()
        .filter(|mark| mark["issue"] == issue)
        .cloned()
        .collect()
}

fn pending_for(state: &Value, issue: &str) -> Vec<Value> {
    state["show_requests"]
        .as_array()
        .unwrap_or_else(|| panic!("state without show_requests: {state}"))
        .iter()
        .filter(|request| request["issue"] == issue)
        .cloned()
        .collect()
}

fn resolved(state: &Value) -> Vec<Value> {
    state["your_resolved_requests"]
        .as_array()
        .unwrap_or_else(|| panic!("state without your_resolved_requests: {state}"))
        .clone()
}

#[test]
fn ae9_two_callers_each_hold_their_own_needs_you_and_unmark_clears_only_the_callers() {
    let td = TestDaemon::start(&[]);
    let repo = tempfile::tempdir().unwrap();
    let worktree = git_worktree(repo.path(), "wt");
    let (a_env, b_env) = (agent("session-a"), agent("session-b"));
    let (a_claims, b_claims) = (borrowed(&a_env), borrowed(&b_env));
    let mut a = Mcp::spawn(for_daemon(&td, &worktree, &a_claims));
    let mut b = Mcp::spawn(for_daemon(&td, &worktree, &b_claims));
    a.initialize();
    b.initialize();
    structured(&a.call("bind", json!({"issue": "ENG-148"})));

    let set = |mcp: &mut Mcp, text: &str| {
        let result = mcp.call(
            "mark",
            json!({"issue": "ENG-148", "kind": "needs_you", "text": text}),
        );
        structured(&result).clone()
    };
    assert_eq!(set(&mut a, "from a")["after"]["text"], "from a");
    let b_set = set(&mut b, "from b");
    assert_eq!(
        b_set["before"],
        json!([]),
        "B's mark replaces nothing of A's: {b_set}"
    );

    for (mcp, mine) in [(&mut a, "from a"), (&mut b, "from b")] {
        let state = mcp.call("state", json!({}));
        let marks = marks_on(structured(&state), "ENG-148");
        assert_eq!(marks.len(), 2, "both callers' marks stay (R9): {marks:?}");
        for mark in &marks {
            assert_eq!(mark["kind"], "needs_you", "{mark}");
            assert_eq!(
                mark["yours"],
                json!(mark["text"] == mine),
                "only the caller's own mark is flagged yours: {mark}"
            );
        }
    }

    let removed = a.call("unmark", json!({"issue": "ENG-148"}));
    let removed = structured(&removed)["removed"].as_array().unwrap().clone();
    assert_eq!(removed.len(), 1, "{removed:?}");
    assert_eq!(removed[0]["text"], "from a");

    let state = b.call("state", json!({}));
    let marks = marks_on(structured(&state), "ENG-148");
    assert_eq!(marks.len(), 1, "B's mark stays: {marks:?}");
    assert_eq!(marks[0]["text"], "from b");
    assert_eq!(marks[0]["yours"], true);

    a.finish();
    b.finish();
}

#[test]
fn mark_refuses_suggestion_and_unknown_kinds_and_writes_nothing() {
    let td = TestDaemon::start(&[]);
    let repo = tempfile::tempdir().unwrap();
    let worktree = git_worktree(repo.path(), "wt");
    let env = agent("session-a");
    let claims = borrowed(&env);
    let mut mcp = Mcp::spawn(for_daemon(&td, &worktree, &claims));
    mcp.initialize();
    structured(&mcp.call("bind", json!({"issue": "ENG-148"})));

    for kind in ["suggestion", "attention", "blocked", ""] {
        let text = error_text(&mcp.call("mark", json!({"issue": "ENG-148", "kind": kind})));
        assert!(
            text.contains("needs_you") && text.contains("question") && text.contains("done"),
            "the refusal names the kinds an agent may set: {text}"
        );
    }
    let state = mcp.call("state", json!({}));
    assert_eq!(
        structured(&state)["marks"],
        json!([]),
        "nothing was written"
    );

    for (kind, shown) in [("question", "question"), ("done", "done")] {
        let result = mcp.call("mark", json!({"issue": "ENG-148", "kind": kind}));
        assert_eq!(structured(&result)["after"]["kind"], shown);
    }

    mcp.finish();
}

#[test]
fn ae11_asking_twice_keeps_one_request_and_withdraw_show_closes_it_as_withdrawn() {
    let td = TestDaemon::start(&[]);
    let repo = tempfile::tempdir().unwrap();
    let worktree = git_worktree(repo.path(), "wt");
    let env = agent("session-a");
    let claims = borrowed(&env);
    let mut a = Mcp::spawn(for_daemon(&td, &worktree, &claims));
    a.initialize();
    structured(&a.call("bind", json!({"issue": "ENG-160"})));

    let first = a.call("ask_to_show", json!({"issue": "ENG-160", "reason": "one"}));
    let id = structured(&first)["after"]["id"].clone();
    let again = a.call("ask_to_show", json!({"issue": "ENG-160", "reason": "two"}));
    assert_eq!(
        structured(&again)["after"]["id"],
        id,
        "a re-ask refreshes (R31)"
    );

    let state = a.call("state", json!({}));
    let pending = pending_for(structured(&state), "ENG-160");
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0]["reason"], "two");
    assert_eq!(pending[0]["yours"], true);
    assert!(
        pending[0]["expires_at"].is_string(),
        "each pending request shows its expiry: {}",
        pending[0]
    );

    let withdrawn = a.call("withdraw_show", json!({"issue": "ENG-160"}));
    assert_eq!(structured(&withdrawn)["after"]["outcome"], "withdrawn");

    let state = a.call("state", json!({}));
    let state = structured(&state);
    assert!(pending_for(state, "ENG-160").is_empty(), "{state}");
    let resolved = resolved(state);
    assert_eq!(resolved.len(), 1, "{resolved:?}");
    assert_eq!(resolved[0]["id"], id);
    assert_eq!(resolved[0]["outcome"], "withdrawn");

    a.finish();
}

#[test]
fn another_caller_cannot_withdraw_a_request_and_sees_none_of_its_outcomes() {
    let td = TestDaemon::start(&[]);
    let repo = tempfile::tempdir().unwrap();
    let worktree = git_worktree(repo.path(), "wt");
    let (a_env, b_env) = (agent("session-a"), agent("session-b"));
    let (a_claims, b_claims) = (borrowed(&a_env), borrowed(&b_env));
    let mut a = Mcp::spawn(for_daemon(&td, &worktree, &a_claims));
    let mut b = Mcp::spawn(for_daemon(&td, &worktree, &b_claims));
    a.initialize();
    b.initialize();
    structured(&a.call("bind", json!({"issue": "ENG-160"})));

    let asked = a.call("ask_to_show", json!({"issue": "ENG-160"}));
    let id = structured(&asked)["after"]["id"].as_i64().unwrap();

    error_text(&b.call("withdraw_show", json!({"issue": "ENG-160"})));
    let state = b.call("state", json!({}));
    let pending = pending_for(structured(&state), "ENG-160");
    assert_eq!(pending.len(), 1, "A's request stays pending: {pending:?}");
    assert_eq!(pending[0]["yours"], false);

    UnixClient::connect(&td.socket)
        .unwrap()
        .linear_show_dismiss(id)
        .expect("the person rejects A's request");

    let state = a.call("state", json!({}));
    let resolved_a = resolved(structured(&state));
    assert_eq!(resolved_a.len(), 1, "{resolved_a:?}");
    assert_eq!(resolved_a[0]["id"], id);
    assert_eq!(resolved_a[0]["outcome"], "rejected");
    let state = b.call("state", json!({}));
    assert_eq!(
        resolved(structured(&state)),
        Vec::<Value>::new(),
        "B never sees A's outcomes"
    );

    a.finish();
    b.finish();
}

const FULL_ENV: [(&str, &str); 4] = [
    ("HERDR_SOCKET_PATH", "/tmp/herdr-env.sock"),
    ("HERDR_PANE_ID", "w1:p2"),
    ("HERDR_WORKSPACE_ID", "w1"),
    ("CLAUDE_CODE_SESSION_ID", "sess-env"),
];
const NO_HERDR_ENV: [(&str, &str); 1] = [("CLAUDE_CODE_SESSION_ID", "sess-1")];

fn candidate(session: &str, pane: &str, label: &str, title: &str) -> Value {
    let workspace = pane.split(':').next().unwrap();
    json!({
        "pane": format!("{session}/{pane}"),
        "session": session,
        "socket": format!("/tmp/{session}.sock"),
        "workspace_id": workspace,
        "workspace_label": label,
        "tab_id": format!("{workspace}:t1"),
        "pane_id": pane,
        "title": title,
    })
}

/// The location a confirmed `<session>/<pane id>` resolves to.
fn location_of(pane: &str) -> Value {
    let (session, pane_id) = pane.split_once('/').unwrap();
    let workspace = pane_id.split(':').next().unwrap();
    json!({
        "state": "resolved",
        "location": {
            "session": session,
            "socket": format!("/tmp/{session}.sock"),
            "workspace_id": workspace,
            "tab_id": format!("{workspace}:t1"),
            "pane_id": pane_id,
        }
    })
}

fn opened() -> Value {
    json!({
        "pane_id": "w9:p9",
        "tab_id": "w9:t9",
        "workspace_id": "w9",
        "placement": "split",
        "reused": false
    })
}

/// The panes a fake boardd remembers, by Claude session id. A test empties
/// it to stand for boardd finding the remembered pane closed.
type Remembered = Arc<Mutex<BTreeMap<String, String>>>;

/// A boardd whose folder lookup offers `candidates` and that resolves any
/// session-qualified pane; every other method it does not know is refused.
fn caller_boardd(socket: &Path, candidates: Vec<Value>) -> Receiver<Request> {
    caller_boardd_remembering(socket, candidates).0
}

/// [`caller_boardd`] that, like boardd, remembers a pane confirmed with a
/// Claude session id and resolves that session's later lookups to it.
fn caller_boardd_remembering(
    socket: &Path,
    candidates: Vec<Value>,
) -> (Receiver<Request>, Remembered) {
    let remembered = Remembered::default();
    let memory = Arc::clone(&remembered);
    let requests = scripted_boardd(socket, move |request| match request.method.as_str() {
        "caller.resolve" => {
            let session = request.params["claude_session_id"].as_str();
            let mut memory = memory.lock().unwrap();
            match (request.params["pane"].as_str(), session) {
                (Some(pane), Some(session)) => {
                    memory.insert(session.to_string(), pane.to_string());
                    Ok(location_of(pane))
                }
                (Some(pane), None) => Ok(location_of(pane)),
                (None, _) => match session.and_then(|s| memory.get(s)) {
                    Some(pane) => Ok(location_of(pane)),
                    None if candidates.is_empty() => Ok(json!({"state": "not_in_herdr"})),
                    None => Ok(json!({"state": "unconfirmed", "candidates": candidates})),
                },
            }
        }
        "board.pane.open" => Ok(opened()),
        "board.notify" => Ok(json!({"shown": true})),
        "linear.mark.set" => Ok(done_mark(request.params["space"].as_str().unwrap_or(""))),
        other => Err((3, format!("fake boardd does not answer {other}"))),
    });
    (requests, remembered)
}

fn fake_env<'a>(
    dir: &'a Path,
    socket: &'a Path,
    cwd: &'a Path,
    claims: &'a [(&'a str, &'a str)],
) -> McpEnv<'a> {
    McpEnv {
        socket,
        db: dir.join("board.db"),
        config: dir.join("missing-config.toml"),
        home: dir,
        cwd,
        claims,
    }
}

fn drain(requests: &Receiver<Request>) -> Vec<Request> {
    requests.try_iter().collect()
}

fn methods(requests: &[Request]) -> Vec<&str> {
    requests.iter().map(|r| r.method.as_str()).collect()
}

#[test]
fn ae1_full_env_opens_beside_the_env_pane_and_never_asks_the_daemon_to_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = caller_boardd(&socket, vec![candidate("s1", "w5:p5", "api", "x")]);
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &FULL_ENV));
    mcp.initialize();

    structured(&mcp.call("open_board", json!({"issue": "ENG-1"})));
    structured(&mcp.call("notify", json!({"title": "hi"})));

    mcp.finish();
    let seen = drain(&requests);
    assert_eq!(methods(&seen), ["board.pane.open", "board.notify"]);
    assert_eq!(seen[0].params["origin_pane"], "w1:p2");
    assert_eq!(seen[0].params["origin_socket"], "/tmp/herdr-env.sock");
    assert_eq!(seen[1].params["origin_socket"], "/tmp/herdr-env.sock");
}

#[test]
fn ae7_an_explicit_pane_wins_over_full_env() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = caller_boardd(&socket, Vec::new());
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &FULL_ENV));
    mcp.initialize();

    structured(&mcp.call("open_board", json!({"issue": "ENG-1", "pane": "s2/w3:p4"})));

    mcp.finish();
    let seen = drain(&requests);
    assert_eq!(methods(&seen), ["caller.resolve", "board.pane.open"]);
    assert_eq!(seen[0].params["pane"], "s2/w3:p4");
    assert_eq!(seen[0].params["claude_session_id"], "sess-env");
    assert_eq!(seen[1].params["origin_pane"], "w3:p4");
    assert_eq!(seen[1].params["origin_socket"], "/tmp/s2.sock");
}

#[test]
fn ae2_one_folder_candidate_is_offered_never_used_until_the_caller_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let cwd = git_worktree(dir.path(), "wt");
    let requests = caller_boardd(
        &socket,
        vec![candidate("default", "w1:p2", "api", "✳ Claude Code")],
    );
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, &cwd, &NO_HERDR_ENV));
    mcp.initialize();

    let text = error_text(&mcp.call("open_board", json!({"issue": "ENG-1"})));
    for expected in ["default/w1:p2", "api", "✳ Claude Code", "Ask the person"] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
    let seen = drain(&requests);
    assert_eq!(methods(&seen), ["caller.resolve"], "nothing opens");
    assert_eq!(seen[0].params["cwd"], cwd.to_str().unwrap());
    assert_eq!(seen[0].params["claude_session_id"], "sess-1");
    assert!(seen[0].params.get("pane").is_none(), "{}", seen[0].params);

    structured(&mcp.call(
        "open_board",
        json!({"issue": "ENG-1", "pane": "default/w1:p2"}),
    ));
    let seen = drain(&requests);
    assert_eq!(methods(&seen), ["caller.resolve", "board.pane.open"]);
    assert_eq!(seen[0].params["pane"], "default/w1:p2");
    assert_eq!(seen[0].params["claude_session_id"], "sess-1");
    assert_eq!(seen[1].params["origin_pane"], "w1:p2");
    assert_eq!(seen[1].params["origin_socket"], "/tmp/default.sock");

    mcp.call("mark", json!({"issue": "ENG-1", "kind": "needs_you"}));
    let seen = drain(&requests);
    assert_eq!(
        methods(&seen),
        ["caller.resolve", "linear.mark.set"],
        "boardd is asked again, so it can check the confirmed pane still exists"
    );
    assert!(seen[0].params.get("pane").is_none(), "{}", seen[0].params);
    assert_eq!(seen[1].params["space"], "w1");
    assert_eq!(seen[1].params["owner"]["herdr_pane_id"], "w1:p2");
    assert_eq!(seen[1].params["owner"]["herdr_socket"], "/tmp/default.sock");
    assert_eq!(seen[1].params["owner"]["claude_session_id"], "sess-1");

    mcp.finish();
}

#[test]
fn ae3_two_candidates_are_both_named_and_a_retry_opens_beside_the_chosen_one() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = caller_boardd(
        &socket,
        vec![
            candidate("default", "w1:p2", "api", "first"),
            candidate("work", "w4:p1", "web", "second"),
        ],
    );
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    let text = error_text(&mcp.call("open_board", json!({"issue": "ENG-1"})));
    for expected in [
        "default/w1:p2",
        "work/w4:p1",
        "api",
        "web",
        "first",
        "second",
    ] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
    assert!(!methods(&drain(&requests)).contains(&"board.pane.open"));

    structured(&mcp.call(
        "open_board",
        json!({"issue": "ENG-1", "pane": "work/w4:p1"}),
    ));
    let seen = drain(&requests);
    assert_eq!(methods(&seen), ["caller.resolve", "board.pane.open"]);
    assert_eq!(seen[1].params["origin_pane"], "w4:p1");
    assert_eq!(seen[1].params["origin_socket"], "/tmp/work.sock");

    mcp.finish();
}

#[test]
fn the_unconfirmed_error_strips_control_characters_from_candidate_text() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let _requests = caller_boardd(
        &socket,
        vec![candidate(
            "default",
            "w1:p2",
            "api\u{1b}[0m",
            "claude \u{1b}[31mred\u{7}",
        )],
    );
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    let result = mcp.call("open_board", json!({"issue": "ENG-1"}));
    let text = error_text(&result);
    let message = result["structuredContent"]["message"].as_str().unwrap();

    mcp.finish();
    for rendered in [text.as_str(), message] {
        assert!(
            rendered
                .contains(r#"- pane "default/w1:p2": workspace "api[0m", title "claude [31mred""#),
            "{rendered}"
        );
        for escaped in ["\u{1b}", "\u{7}", "\\u{1b}", "\\u{7}"] {
            assert!(!rendered.contains(escaped), "{escaped:?} in {rendered}");
        }
    }
}

#[test]
fn an_unconfirmed_result_is_not_remembered_so_the_next_call_resolves_again() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = caller_boardd(&socket, vec![candidate("default", "w1:p2", "api", "t")]);
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    error_text(&mcp.call("open_board", json!({"issue": "ENG-1"})));
    error_text(&mcp.call("state", json!({})));

    mcp.finish();
    assert_eq!(
        methods(&drain(&requests)),
        ["caller.resolve", "caller.resolve"]
    );
}

#[test]
fn the_unconfirmed_error_asks_the_person_before_any_retry() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let _requests = caller_boardd(&socket, vec![candidate("default", "w1:p2", "api", "t")]);
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    let result = mcp.call("note", json!({"issue": "ENG-1", "body": "x"}));
    let text = error_text(&result);
    assert!(text.contains("Ask the person"), "{text}");
    assert!(text.contains("in herdr at all"), "{text}");
    assert_eq!(
        result["structuredContent"]["candidates"][0]["pane"],
        "default/w1:p2"
    );

    mcp.finish();
}

#[test]
fn a_named_pane_no_herdr_session_lists_is_a_tool_error_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = scripted_boardd(&socket, |request| match request.method.as_str() {
        "caller.resolve" => Ok(json!({"state": "not_in_herdr"})),
        other => Err((3, format!("unexpected {other}"))),
    });
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    let text = error_text(&mcp.call(
        "mark",
        json!({"issue": "ENG-1", "kind": "done", "pane": "s9/w9:p9"}),
    ));
    assert!(text.contains("s9/w9:p9"), "{text}");

    mcp.finish();
    assert_eq!(methods(&drain(&requests)), ["caller.resolve"]);
}

#[test]
fn a_remembered_pane_that_closed_asks_the_caller_to_confirm_again() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = scripted_boardd(&socket, |request| match request.method.as_str() {
        "caller.resolve" => Ok(location_of("default/w1:p2")),
        "board.pane.open" => Err((2, "pane w1:p2 not found".to_string())),
        other => Err((3, format!("unexpected {other}"))),
    });
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    let text = error_text(&mcp.call("open_board", json!({"issue": "ENG-1"})));
    assert!(text.contains(CLOSED_HINT), "{text}");

    mcp.finish();
    assert_eq!(
        methods(&drain(&requests)),
        ["caller.resolve", "board.pane.open"]
    );
}

const CLOSED_HINT: &str = "may have closed";

/// A boardd that resolves only a named pane, opens the first board and then
/// answers every open with the not-found code a closed origin pane gets.
fn closing_boardd(socket: &Path) -> Receiver<Request> {
    let opens = AtomicUsize::new(0);
    scripted_boardd(socket, move |request| match request.method.as_str() {
        "caller.resolve" => match request.params["pane"].as_str() {
            Some(pane) => Ok(location_of(pane)),
            None => Ok(json!({"state": "not_in_herdr"})),
        },
        "board.pane.open" if opens.fetch_add(1, Ordering::SeqCst) == 0 => Ok(opened()),
        "board.pane.open" => Err((
            2,
            "origin pane w1:p2 is not in the caller's herdr session".into(),
        )),
        "linear.mark.set" => Ok(done_mark("w1")),
        other => Err((3, format!("unexpected {other}"))),
    })
}

#[test]
fn without_a_session_id_a_cached_pane_that_closed_is_dropped_and_asked_again() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = closing_boardd(&socket);
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &[]));
    mcp.initialize();

    structured(&mcp.call(
        "open_board",
        json!({"issue": "ENG-1", "pane": "default/w1:p2"}),
    ));
    drain(&requests);

    let text = error_text(&mcp.call("open_board", json!({"issue": "ENG-2"})));
    assert!(text.contains(CLOSED_HINT), "{text}");
    assert_eq!(
        methods(&drain(&requests)),
        ["board.pane.open"],
        "the process cache served the pane"
    );

    mcp.call("mark", json!({"issue": "ENG-1", "kind": "done"}));
    let seen = drain(&requests);
    assert_eq!(
        methods(&seen)[0],
        "caller.resolve",
        "the closed pane left the cache"
    );
    assert!(
        seen.iter()
            .all(|r| r.params["owner"]["herdr_pane_id"] != "w1:p2"),
        "the closed pane was used again"
    );

    mcp.finish();
}

#[test]
fn a_not_found_for_an_env_or_named_pane_carries_no_closed_pane_hint() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let _requests = scripted_boardd(&socket, |request| match request.method.as_str() {
        "caller.resolve" => Ok(location_of(
            request.params["pane"].as_str().unwrap_or("s/w1:p2"),
        )),
        "board.pane.open" => Err((2, "origin pane is not in the caller's herdr session".into())),
        other => Err((3, format!("unexpected {other}"))),
    });
    let mut env_mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &FULL_ENV));
    env_mcp.initialize();
    let text = error_text(&env_mcp.call("open_board", json!({"issue": "ENG-1"})));
    assert!(!text.contains(CLOSED_HINT), "env pane: {text}");
    let text = error_text(&env_mcp.call(
        "open_board",
        json!({"issue": "ENG-1", "pane": "default/w1:p2"}),
    ));
    assert!(!text.contains(CLOSED_HINT), "named pane: {text}");
    env_mcp.finish();

    let mut named = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    named.initialize();
    let text = error_text(&named.call(
        "open_board",
        json!({"issue": "ENG-1", "pane": "default/w1:p2"}),
    ));
    assert!(!text.contains(CLOSED_HINT), "named pane: {text}");
    named.finish();
}

#[test]
fn a_confirmed_pane_boardd_reports_gone_is_not_used_by_the_next_call() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let (requests, remembered) =
        caller_boardd_remembering(&socket, vec![candidate("default", "w1:p2", "api", "t")]);
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    structured(&mcp.call(
        "open_board",
        json!({"issue": "ENG-1", "pane": "default/w1:p2"}),
    ));
    drain(&requests);
    remembered.lock().unwrap().clear();

    let text = error_text(&mcp.call("mark", json!({"issue": "ENG-1", "kind": "done"})));
    assert!(text.contains("default/w1:p2"), "{text}");

    mcp.finish();
    assert_eq!(
        methods(&drain(&requests)),
        ["caller.resolve"],
        "nothing is written under the closed pane"
    );
}

/// A boardd from before `caller.resolve`: it refuses the method by name.
fn old_boardd(socket: &Path) -> Receiver<Request> {
    scripted_boardd(socket, |request| match request.method.as_str() {
        "caller.resolve" => Err((1, "unknown method: caller.resolve".into())),
        "board.notify" => Ok(json!({"shown": true})),
        "linear.state.get" => Ok(json!({"space": request.params["space"]})),
        other => Err((3, format!("unexpected {other}"))),
    })
}

#[test]
fn an_old_boardd_without_caller_resolve_still_serves_tools_that_need_no_pane() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = old_boardd(&socket);
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    assert_eq!(
        structured(&mcp.call("notify", json!({"title": "hi"})))["shown"],
        true
    );
    assert_eq!(
        structured(&mcp.call("state", json!({"space": "w7"})))["space"],
        "w7"
    );
    let text = error_text(&mcp.call("state", json!({"space": "w7", "pane": "default/w1:p2"})));
    assert!(
        text.contains("caller.resolve"),
        "a named pane still fails: {text}"
    );

    mcp.finish();
    assert_eq!(
        methods(&drain(&requests)),
        [
            "caller.resolve",
            "board.notify",
            "caller.resolve",
            "linear.state.get",
            "caller.resolve"
        ]
    );
}

#[test]
fn notify_refuses_with_the_candidates_when_the_pane_is_unconfirmed() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = unconfirmed_boardd(&socket, dir.path().to_path_buf());
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    let result = mcp.call("notify", json!({"title": "hi"}));
    let text = error_text(&result);
    assert!(text.contains("default/w1:p2"), "{text}");
    assert_eq!(result["structuredContent"]["state"], "unconfirmed");

    mcp.finish();
    assert_eq!(methods(&drain(&requests)), ["caller.resolve"]);
}

#[test]
fn every_pane_argument_and_the_unconfirmed_error_name_the_not_in_herdr_way_out() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let _requests = unconfirmed_boardd(&socket, dir.path().to_path_buf());
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    let way_out = "call again with an explicit `space`";
    let tools = mcp.tools();
    let with_pane: Vec<_> = tools
        .iter()
        .filter_map(|tool| tool["inputSchema"]["properties"]["pane"]["description"].as_str())
        .collect();
    assert_eq!(with_pane.len(), TOOLS.len(), "{tools:?}");
    for description in with_pane {
        assert!(description.contains(way_out), "{description}");
    }
    let text = error_text(&mcp.call("mark", json!({"issue": "ENG-1", "kind": "done"})));
    assert!(text.contains(way_out), "{text}");

    mcp.finish();
}

fn done_mark(space: &str) -> Value {
    json!({
        "before": [],
        "after": {
            "id": 1,
            "space": space,
            "issue": "ENG-1",
            "kind": "done",
            "text": null,
            "detail": null,
            "created_by": "agent",
            "created_at": "2026-10-08T00:00:00Z",
            "owner_herdr_socket": null,
            "owner_herdr_pane_id": null,
            "owner_claude_session_id": "sess-1"
        }
    })
}

fn binding_change(worktree: &Path) -> Value {
    json!({
        "before": null,
        "after": {
            "worktree_path": worktree.to_str().unwrap(),
            "issue": "ENG-7",
            "state": "bound",
            "branch": null,
            "tab": null,
            "display_name": null,
            "team_ids": [],
            "view": null,
            "carried": {}
        }
    })
}

/// A boardd whose folder lookup always offers one unconfirmed pane and that
/// accepts a mark and a bind.
fn unconfirmed_boardd(socket: &Path, worktree: PathBuf) -> Receiver<Request> {
    scripted_boardd(socket, move |request| match request.method.as_str() {
        "caller.resolve" => Ok(json!({
            "state": "unconfirmed",
            "candidates": [candidate("default", "w1:p2", "api", "t")]
        })),
        "linear.mark.set" => Ok(done_mark(request.params["space"].as_str().unwrap_or(""))),
        "linear.bind" => Ok(binding_change(&worktree)),
        other => Err((3, format!("unexpected {other}"))),
    })
}

#[test]
fn an_explicit_space_writes_as_before_when_the_pane_is_unconfirmed() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = unconfirmed_boardd(&socket, dir.path().to_path_buf());
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    let result = mcp.call(
        "mark",
        json!({"issue": "ENG-1", "kind": "done", "space": "w7"}),
    );
    assert_eq!(structured(&result)["after"]["space"], "w7");

    mcp.finish();
    let seen = drain(&requests);
    assert_eq!(methods(&seen), ["caller.resolve", "linear.mark.set"]);
    assert_eq!(seen[1].params["space"], "w7");
    assert_eq!(seen[1].params["owner"]["herdr_pane_id"], Value::Null);
    assert_eq!(seen[1].params["owner"]["herdr_socket"], Value::Null);
    assert_eq!(seen[1].params["owner"]["claude_session_id"], "sess-1");
}

#[test]
fn bind_succeeds_when_the_pane_is_unconfirmed_and_notes_the_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let worktree = git_worktree(dir.path(), "wt");
    let requests = unconfirmed_boardd(&socket, worktree.clone());
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, &worktree, &NO_HERDR_ENV));
    mcp.initialize();

    let result = mcp.call("bind", json!({"issue": "ENG-7"}));
    let value = structured(&result);
    assert_eq!(value["after"]["issue"], "ENG-7");
    assert_eq!(value["caller"]["state"], "unconfirmed");
    assert_eq!(value["caller"]["candidates"][0]["pane"], "default/w1:p2");
    let note = value["caller"]["message"].as_str().unwrap();
    assert!(note.contains("Ask the person"), "{note}");

    mcp.finish();
    let seen = drain(&requests);
    assert_eq!(methods(&seen), ["caller.resolve", "linear.bind"]);
    assert_eq!(
        seen[1].params["claims"],
        json!({
            "herdr_socket": null,
            "herdr_pane_id": null,
            "herdr_workspace_id": null,
            "card_id": null,
            "run_id": null
        })
    );
}

#[test]
fn a_space_tool_without_space_refuses_when_the_pane_is_unconfirmed() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = unconfirmed_boardd(&socket, dir.path().to_path_buf());
    let mut mcp = Mcp::spawn(fake_env(dir.path(), &socket, dir.path(), &NO_HERDR_ENV));
    mcp.initialize();

    let text = error_text(&mcp.call("mark", json!({"issue": "ENG-1", "kind": "done"})));
    assert!(text.contains("default/w1:p2"), "{text}");
    error_text(&mcp.call("close_board", json!({"issue": "ENG-1"})));

    mcp.finish();
    assert_eq!(
        methods(&drain(&requests)),
        ["caller.resolve", "caller.resolve"],
        "nothing is written"
    );
}
