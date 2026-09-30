//! `board mcp`: the stdio MCP server driven over its real stdin/stdout with
//! hand-written JSON-RPC, against a real daemon or a recording fake boardd.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::{Request, Response};
use serde_json::{json, Value};

use super::{fixtures_dir, TestDaemon, BOARD_BIN};

const TOOLS: [&str; 10] = [
    "state",
    "panes_for_issue",
    "bind",
    "unbind",
    "mark",
    "note",
    "notify",
    "ask_to_show",
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
            .env_remove("BOARD_WORK_PLUGIN_ROOT")
            .env_remove("BOARD_SCOPE_PATH")
            .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env_remove("BOARD_CARD_ID")
            .env_remove("BOARD_RUN_ID")
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
fn initialize_then_tools_list_returns_the_ten_tools_with_read_only_hints_on_reads() {
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
    error_text(&mcp.call("mark", json!({"issue": "not an issue"})));
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
