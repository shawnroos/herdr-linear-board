//! `board linear snapshot`: the CLI-to-daemon-to-script read, driven end to
//! end against a real `board daemon --foreground` and a fake plugin root
//! (U13). Every plugin-side failure exits `6` through the CLI.

use std::process::Output;

use serde_json::Value;

use super::{fake_plugin_root, json_error, json_output, TestDaemon};

const PLUGIN_UNAVAILABLE: i32 = 6;

fn code(out: &Output) -> i32 {
    out.status.code().expect("board exits, never signals")
}

/// A daemon whose environment names `root`. The CLI's own
/// `BOARD_WORK_PLUGIN_ROOT` is removed by the test runner, so the daemon's is
/// the one that answers.
fn daemon_with_root(root: &std::path::Path) -> TestDaemon {
    TestDaemon::start(&[("BOARD_WORK_PLUGIN_ROOT", root.to_str().unwrap())])
}

fn snapshot(td: &TestDaemon) -> Output {
    td.board(&["linear", "snapshot", "wA", "--json"])
}

fn plugin_error(out: &Output) -> String {
    assert_eq!(
        code(out),
        PLUGIN_UNAVAILABLE,
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let error = json_error(out);
    assert_eq!(error["error"]["code"], PLUGIN_UNAVAILABLE);
    error["error"]["message"].as_str().unwrap().to_string()
}

/// (a) The fixture document comes back through the CLI with a `pane_status`
/// entry per named pane; the test daemon has no herdr, so every one is unknown.
#[test]
fn snapshot_returns_the_fixture_document_with_pane_statuses() {
    let root = fake_plugin_root(board_core::PLUGIN_VERSION_FLOOR, &[]);
    let td = daemon_with_root(root.path());
    let doc = json_output(&snapshot(&td));
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["workspace"]["id"], "wA");
    assert_eq!(doc["record"]["state"], "bound");
    let statuses = doc["pane_status"].as_object().expect("pane_status map");
    for pane in ["wA:p1", "wA:p2"] {
        assert_eq!(statuses[pane], "unknown", "{statuses:?}");
    }
    assert!(
        statuses.values().all(|status| status == "unknown"),
        "{statuses:?}"
    );
}

/// (b) A plugin below the floor is refused with both versions in the message.
#[test]
fn a_plugin_below_the_floor_is_refused_naming_both_versions() {
    let root = fake_plugin_root("0.0.1", &[]);
    let td = daemon_with_root(root.path());
    let message = plugin_error(&snapshot(&td));
    assert!(message.contains("0.0.1"), "{message}");
    assert!(
        message.contains(board_core::PLUGIN_VERSION_FLOOR),
        "{message}"
    );
}

/// The native read of `wA` on a fresh daemon: unbound, and Linear not called.
fn assert_native_unbound(out: &Output) {
    let doc = json_output(out);
    assert_eq!(doc["workspace"]["id"], "wA");
    assert_eq!(doc["record"]["state"], "unbound");
    assert_eq!(doc["linear"]["status"], "unknown");
}

/// (c) With no plugin root named anywhere, the daemon reads natively, and no
/// plugin error is raised.
#[test]
fn no_root_anywhere_reads_the_snapshot_natively() {
    let td = TestDaemon::start(&[]);
    assert_native_unbound(&snapshot(&td));
}

/// (d) Exit 3 from the script is the "no such space" answer.
#[test]
fn a_script_exit_3_names_no_such_space() {
    let root = fake_plugin_root(
        board_core::PLUGIN_VERSION_FLOOR,
        &[
            ("FAKE_WORK_SNAPSHOT_EXIT", "3"),
            ("FAKE_WORK_SNAPSHOT_STDERR", "wA is not a space here"),
        ],
    );
    let td = daemon_with_root(root.path());
    let message = plugin_error(&snapshot(&td));
    assert!(message.contains("no such space"), "{message}");
    // The script's stderr is never shown; the message says how to see it.
    assert!(!message.contains("wA is not a space here"), "{message}");
    assert!(
        message.contains("in a shell to see its output"),
        "{message}"
    );
}

/// (e) `[daemon] work_plugin_root` alone resolves the root.
#[test]
fn the_toml_key_alone_resolves_the_root() {
    let root = fake_plugin_root(
        board_core::PLUGIN_VERSION_FLOOR,
        &[("FAKE_WORK_SNAPSHOT_FIXTURE", "unbound")],
    );
    let td = TestDaemon::start_with_config(
        &[],
        &format!("work_plugin_root = \"{}\"\n", root.path().display()),
    );
    let doc = json_output(&td.board(&["linear", "snapshot", "wA", "--json"]));
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["record"]["state"], "unbound");
}

/// The CLI's `HERDR_SOCKET_PATH` becomes the request's origin socket and
/// reaches the script as `HERDR_SOCKET_PATH`: the one proof that the
/// CLI-side argument path is wired, not only the daemon's.
#[test]
fn the_origin_socket_reaches_the_script() {
    let dump_dir = tempfile::tempdir().unwrap();
    let dump = dump_dir.path().join("script-env");
    let root = fake_plugin_root(
        board_core::PLUGIN_VERSION_FLOOR,
        &[("FAKE_WORK_SNAPSHOT_ENV_FILE", dump.to_str().unwrap())],
    );
    let td = daemon_with_root(root.path());
    let socket = td._dir.path().join("origin-herdr.sock");
    let out = td.board_with_env(
        &["linear", "snapshot", "wA", "--json"],
        &[("HERDR_SOCKET_PATH", socket.to_str().unwrap())],
    );
    json_output(&out);
    let env = std::fs::read_to_string(&dump).unwrap();
    let line = format!("HERDR_SOCKET_PATH={}", socket.display());
    assert!(env.lines().any(|l| l == line), "{env}");
}

/// Without `--json` the document is still printed as JSON.
#[test]
fn snapshot_without_json_prints_the_document() {
    let root = fake_plugin_root(board_core::PLUGIN_VERSION_FLOOR, &[]);
    let td = daemon_with_root(root.path());
    let out = td.board(&["linear", "snapshot", "wA"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).expect("JSON document");
    assert_eq!(doc["workspace"]["id"], "wA");
}

/// The CLI's `BOARD_WORK_PLUGIN_ROOT` reaches the daemon with the request, so
/// setting it needs no daemon restart.
#[test]
fn the_callers_plugin_root_is_used_by_a_daemon_started_without_one() {
    let td = TestDaemon::start(&[]);
    assert_native_unbound(&snapshot(&td));
    let root = fake_plugin_root(board_core::PLUGIN_VERSION_FLOOR, &[]);
    let out = td.board_with_env(
        &["linear", "snapshot", "wA", "--json"],
        &[("BOARD_WORK_PLUGIN_ROOT", root.path().to_str().unwrap())],
    );
    assert_eq!(
        json_output(&out)["record"]["state"],
        "bound",
        "the script ran"
    );
}

/// `[daemon] work_plugin_root` written after the daemon started is read on
/// the next request.
#[test]
fn a_toml_key_added_after_start_is_read_without_a_restart() {
    let td = TestDaemon::start(&[]);
    assert_native_unbound(&snapshot(&td));
    let root = fake_plugin_root(board_core::PLUGIN_VERSION_FLOOR, &[]);
    let cfg = td._dir.path().join("config.toml");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str(&format!(
        "work_plugin_root = \"{}\"\n",
        root.path().display()
    ));
    std::fs::write(&cfg, text).unwrap();
    assert_eq!(
        json_output(&snapshot(&td))["record"]["state"],
        "bound",
        "the script ran"
    );
}

fn pid_gone(pid: i32) -> bool {
    let rc = unsafe { libc::kill(pid, 0) };
    rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn wait_for(what: &str, limit: std::time::Duration, mut done: impl FnMut() -> bool) {
    let until = std::time::Instant::now() + limit;
    while !done() {
        assert!(
            std::time::Instant::now() < until,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// A client that sends `linear.snapshot` and goes away stops the script,
/// rather than leaving it to run to the daemon's deadline.
#[test]
fn a_client_that_disconnects_mid_snapshot_stops_the_script() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let root = fake_plugin_root(
        board_core::PLUGIN_VERSION_FLOOR,
        &[
            ("FAKE_WORK_SNAPSHOT_PID_FILE", pid_file.to_str().unwrap()),
            ("FAKE_WORK_SNAPSHOT_SLEEP", "90"),
        ],
    );
    let td = daemon_with_root(root.path());
    let mut stream = std::os::unix::net::UnixStream::connect(&td.socket).unwrap();
    stream
        .write_all(
            b"{\"id\":\"1\",\"method\":\"linear.snapshot\",\"params\":{\"workspace_id\":\"wA\"}}\n",
        )
        .unwrap();
    wait_for(
        "the script to start",
        std::time::Duration::from_secs(15),
        || std::fs::read_to_string(&pid_file).is_ok_and(|t| !t.trim().is_empty()),
    );
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    drop(stream);
    wait_for(
        "the script to stop",
        std::time::Duration::from_secs(15),
        || pid_gone(pid),
    );
    // The daemon still serves the next client.
    let root_ok = fake_plugin_root(board_core::PLUGIN_VERSION_FLOOR, &[]);
    let out = td.board_with_env(
        &["linear", "snapshot", "wA", "--json"],
        &[("BOARD_WORK_PLUGIN_ROOT", root_ok.path().to_str().unwrap())],
    );
    assert_eq!(json_output(&out)["schema"], 1);
}

/// `board daemon --stop` while a snapshot runs stops the script and the
/// daemon exits promptly, not after the script's deadline.
#[test]
fn stopping_the_daemon_mid_snapshot_stops_the_script() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let root = fake_plugin_root(
        board_core::PLUGIN_VERSION_FLOOR,
        &[
            ("FAKE_WORK_SNAPSHOT_PID_FILE", pid_file.to_str().unwrap()),
            ("FAKE_WORK_SNAPSHOT_SLEEP", "90"),
        ],
    );
    let mut td = daemon_with_root(root.path());
    let socket = td.socket.clone();
    let asker = std::thread::spawn(move || {
        let mut client = board_core::client::UnixClient::connect(&socket).unwrap();
        let _ = board_core::client::BoardClient::linear_snapshot(
            &mut client,
            &board_core::protocol::LinearSnapshotParams {
                workspace_id: "wA".into(),
                origin_socket: None,
                plugin_root: None,
                force: false,
            },
        );
    });
    wait_for(
        "the script to start",
        std::time::Duration::from_secs(15),
        || std::fs::read_to_string(&pid_file).is_ok_and(|t| !t.trim().is_empty()),
    );
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let started = std::time::Instant::now();
    super::run_board_stop(&td.socket);
    wait_for(
        "the script to stop",
        std::time::Duration::from_secs(15),
        || pid_gone(pid),
    );
    wait_for(
        "the daemon to exit",
        std::time::Duration::from_secs(20),
        || td.try_exited(),
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    let _ = asker.join();
}

// -- the three list verbs -----------------------------------------------------

const CLI_ERROR: i32 = 64;

fn stdout_text(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).expect("UTF-8 stdout")
}

fn with_socket(td: &TestDaemon) -> std::path::PathBuf {
    td._dir.path().join("origin-herdr.sock")
}

#[test]
fn space_list_prints_a_table_and_its_json() {
    let root = fake_plugin_root(board_core::PLUGIN_VERSION_FLOOR, &[]);
    let td = daemon_with_root(root.path());
    let socket = with_socket(&td);
    let env = [("HERDR_SOCKET_PATH", socket.to_str().unwrap())];

    let text = stdout_text(&td.board_with_env(&["linear", "space", "list"], &env));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    for cell in ["wA", "alpha", "bound", "Example"] {
        assert!(lines[0].contains(cell), "{text}");
    }
    assert!(
        lines[1].contains("wB") && lines[1].contains("unbound"),
        "{text}"
    );
    assert!(
        !text.contains("status"),
        "an ok list has no status line: {text}"
    );

    let doc = json_output(&td.board_with_env(&["linear", "space", "list", "--json"], &env));
    assert_eq!(doc["status"], "ok");
    assert_eq!(doc["rows"][0]["id"], "wA");
    assert_eq!(doc["rows"][0]["project_name"], "Example");
    assert_eq!(doc["rows"][1]["state"], "unbound");
}

#[test]
fn project_list_prints_a_table_and_its_json() {
    let root = fake_plugin_root(board_core::PLUGIN_VERSION_FLOOR, &[]);
    let td = daemon_with_root(root.path());

    let text = stdout_text(&td.board(&["linear", "project", "list"]));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    for cell in ["proj-1", "EX", "Example"] {
        assert!(lines[0].contains(cell), "{text}");
    }

    let doc = json_output(&td.board(&["linear", "project", "list", "--json"]));
    assert_eq!(doc["status"], "ok");
    assert_eq!(doc["rows"][1]["team_key"], "SA");
}

#[test]
fn view_list_passes_the_project_id_and_prints_a_table_and_its_json() {
    let root = fake_plugin_root(board_core::PLUGIN_VERSION_FLOOR, &[]);
    let td = daemon_with_root(root.path());

    let text = stdout_text(&td.board(&["linear", "view", "list", "proj-1"]));
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("view-1"), "{text}");
    assert!(text.contains("Open work in proj-1"), "{text}");

    let doc = json_output(&td.board(&["linear", "view", "list", "proj-1", "--json"]));
    assert_eq!(doc["rows"][0]["name"], "Open work in proj-1");
}

#[test]
fn view_list_without_a_project_id_is_a_usage_error() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["linear", "view", "list", "--json"]);
    assert_eq!(code(&out), CLI_ERROR);
    assert_eq!(json_error(&out)["error"]["kind"], "cli");
}

/// A plugin failure is an error only: nothing reaches stdout, in either mode.
#[test]
fn a_list_plugin_failure_exits_6_with_empty_stdout_and_an_envelope() {
    let root = fake_plugin_root(
        board_core::PLUGIN_VERSION_FLOOR,
        &[("FAKE_WORK_LIST_EXIT", "9")],
    );
    let td = daemon_with_root(root.path());
    for verb in [
        vec!["linear", "project", "list"],
        vec!["linear", "space", "list"],
        vec!["linear", "view", "list", "proj-1"],
    ] {
        let mut json_args = verb.clone();
        json_args.push("--json");
        let message = plugin_error(&td.board(&json_args));
        assert!(message.contains("work-"), "{message}");

        let out = td.board(&verb);
        assert_eq!(code(&out), PLUGIN_UNAVAILABLE, "{verb:?}");
        assert!(out.stdout.is_empty(), "{verb:?}: {:?}", out.stdout);
        assert!(!out.stderr.is_empty(), "{verb:?}");
    }
}

#[test]
fn a_partial_list_exits_0_and_prints_its_status_line() {
    let root = fake_plugin_root(
        board_core::PLUGIN_VERSION_FLOOR,
        &[(
            "FAKE_WORK_LIST_JSON",
            r#"{"status":"partial","message":"listed the first pages only","rows":[{"id":"proj-1","name":"Example","team_key":"EX"}]}"#,
        )],
    );
    let td = daemon_with_root(root.path());
    let text = stdout_text(&td.board(&["linear", "project", "list"]));
    assert!(
        text.lines()
            .any(|l| l.contains("partial") && l.contains("listed the first pages only")),
        "{text}"
    );
    assert!(text.lines().any(|l| l.contains("proj-1")), "{text}");

    let doc = json_output(&td.board(&["linear", "project", "list", "--json"]));
    assert_eq!(doc["status"], "partial");
    assert_eq!(doc["message"], "listed the first pages only");
}

#[test]
fn without_a_herdr_socket_spaces_are_unavailable_while_projects_answer_fully() {
    let root = fake_plugin_root(board_core::PLUGIN_VERSION_FLOOR, &[]);
    let td = daemon_with_root(root.path());

    let out = td.board(&["linear", "space", "list"]);
    let text = stdout_text(&out);
    assert!(
        text.lines()
            .any(|l| l.contains("unavailable") && l.contains("herdr socket missing")),
        "{text}"
    );

    let doc = json_output(&td.board(&["linear", "project", "list", "--json"]));
    assert_eq!(doc["status"], "ok");
    assert_eq!(doc["rows"].as_array().unwrap().len(), 2);
}

#[test]
fn a_name_carrying_a_bidi_override_prints_stripped() {
    let envelope = "{\"status\":\"ok\",\"message\":null,\"rows\":[{\"id\":\"proj-1\",\"name\":\"Exa\u{202e}mple\",\"team_key\":\"EX\"},{\"id\":\"proj-2\",\"name\":\"Sam\\nple\",\"team_key\":\"SA\"}]}";
    let root = fake_plugin_root(
        board_core::PLUGIN_VERSION_FLOOR,
        &[("FAKE_WORK_LIST_JSON", envelope)],
    );
    let td = daemon_with_root(root.path());

    let text = stdout_text(&td.board(&["linear", "project", "list"]));
    assert!(!text.contains('\u{202e}'), "{text:?}");
    assert!(text.contains("Example"), "{text}");
    // The daemon keeps a newline in plugin text; the table must not split a row.
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(text.contains("Sample"), "{text}");

    let out = td.board(&["linear", "project", "list", "--json"]);
    let raw = String::from_utf8(out.stdout.clone()).unwrap();
    assert!(!raw.contains('\u{202e}') && !raw.contains("202e"), "{raw}");
    assert_eq!(json_output(&out)["rows"][0]["name"], "Example");
}

#[test]
fn a_project_with_no_team_prints_an_empty_team_cell_and_a_null_key() {
    let envelope = r#"{"status":"ok","message":null,"rows":[{"id":"proj-1","name":"Example","team_key":null}]}"#;
    let root = fake_plugin_root(
        board_core::PLUGIN_VERSION_FLOOR,
        &[("FAKE_WORK_LIST_JSON", envelope)],
    );
    let td = daemon_with_root(root.path());

    let text = stdout_text(&td.board(&["linear", "project", "list"]));
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.contains("proj-1") && text.contains("Example"),
        "{text}"
    );
    assert!(!text.contains("null"), "{text}");

    let doc = json_output(&td.board(&["linear", "project", "list", "--json"]));
    assert_eq!(doc["status"], "ok");
    assert!(doc["rows"][0]["team_key"].is_null(), "{doc}");
}

/// The positional is optional; the pane's space id stands in for it.
#[test]
fn snapshot_without_a_positional_uses_herdr_workspace_id() {
    let root = fake_plugin_root(board_core::PLUGIN_VERSION_FLOOR, &[]);
    let td = daemon_with_root(root.path());
    let doc = json_output(&td.board_with_env(
        &["linear", "snapshot", "--json"],
        &[("HERDR_WORKSPACE_ID", "wA")],
    ));
    assert_eq!(doc["workspace"]["id"], "wA");
}

#[test]
fn snapshot_with_neither_a_positional_nor_herdr_workspace_id_exits_64() {
    let root = fake_plugin_root(board_core::PLUGIN_VERSION_FLOOR, &[]);
    let td = daemon_with_root(root.path());
    let out = td.board(&["linear", "snapshot", "--json"]);
    assert_eq!(code(&out), CLI_ERROR);
    let error = json_error(&out);
    assert_eq!(error["error"]["code"], CLI_ERROR);
    assert_eq!(error["error"]["kind"], "cli");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("HERDR_WORKSPACE_ID"),
        "{error}"
    );

    let empty = td.board_with_env(&["linear", "snapshot"], &[("HERDR_WORKSPACE_ID", "")]);
    assert_eq!(code(&empty), CLI_ERROR);
    assert!(empty.stdout.is_empty());
}

/// The bind handoff has no command-line verb.
#[test]
fn bind_is_not_a_command_line_verb() {
    let td = TestDaemon::start(&[]);
    for args in [vec!["linear", "bind", "--json"], vec!["bind", "--json"]] {
        let out = td.board(&args);
        assert_eq!(code(&out), CLI_ERROR, "{args:?}");
        let error = json_error(&out);
        assert_eq!(error["error"]["kind"], "cli", "{args:?}");
    }
}

// -- the session read and the Claude Code status line -------------------------

mod session {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::process::{Command, Output, Stdio};
    use std::time::{Duration, Instant};

    use board_core::client::BoardClient;
    use board_core::protocol::{
        LinearBindParams, LinearMarkSetParams, LinearSessionGetParams, LinearSessionGetResult,
        LinearShowRequestParams, MarkKind,
    };
    use serde_json::{json, Value};

    use super::super::report::{
        daemon_with_bound_space, daemon_with_bound_space_and, git_worktree,
    };
    use super::super::{json_output, TestDaemon, BOARD_BIN};

    const HERDR_SOCKET: &str = "/tmp/herdr-test.sock";

    fn herdr_env() -> [(&'static str, &'static str); 3] {
        [
            ("HERDR_SOCKET_PATH", HERDR_SOCKET),
            ("HERDR_PANE_ID", "wA:p2"),
            ("HERDR_WORKSPACE_ID", "wA"),
        ]
    }

    fn board_with_stdin(
        socket: &Path,
        home: &Path,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: &str,
    ) -> (Output, Duration) {
        let mut cmd = Command::new(BOARD_BIN);
        cmd.args(args)
            .current_dir(cwd)
            .env("BOARD_SOCKET", socket)
            .env("BOARD_DB", home.join("board.db"))
            .env("HERDR_BOARD_CONFIG", home.join("missing-config.toml"))
            .env("HOME", home)
            .env("BOARD_SPAWNER", "local")
            .env("BOARD_BIN", BOARD_BIN)
            .env_remove("BOARD_WORK_PLUGIN_ROOT")
            .env_remove("BOARD_SCOPE_PATH")
            .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_PLUGIN_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            cmd.env(key, value);
        }
        let started = Instant::now();
        let mut child = cmd.spawn().expect("spawn board");
        let mut pipe = child.stdin.take().unwrap();
        let _ = pipe.write_all(stdin.as_bytes());
        drop(pipe);
        let out = child.wait_with_output().expect("wait for board");
        (out, started.elapsed())
    }

    fn status_line(td: &TestDaemon, cwd: &Path, env: &[(&str, &str)], stdin: &str) -> String {
        let (out, _) = board_with_stdin(
            &td.socket,
            td._dir.path(),
            cwd,
            &["linear", "status-line"],
            env,
            stdin,
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn claude_input(cwd: &Path) -> String {
        json!({
            "session_id": "claude-1",
            "cwd": cwd,
            "workspace": {"current_dir": cwd, "project_dir": cwd},
            "model": {"id": "x", "display_name": "X"},
        })
        .to_string()
    }

    fn bind(td: &TestDaemon, worktree: &Path, issue: &str) {
        td.client()
            .linear_bind(&LinearBindParams {
                cwd: worktree.to_str().unwrap().into(),
                issue: issue.into(),
                space: Some("wA".into()),
                branch: None,
                tab: None,
                display_name: None,
                claims: Default::default(),
            })
            .unwrap();
    }

    fn mark(td: &TestDaemon, issue: &str, kind: MarkKind) {
        td.client()
            .linear_mark_set(&LinearMarkSetParams {
                space: "wA".into(),
                issue: issue.into(),
                kind,
                text: None,
                created_by: None,
                owner: Default::default(),
            })
            .unwrap();
    }

    fn request(td: &TestDaemon, issue: &str) {
        td.client()
            .linear_show_request(&LinearShowRequestParams {
                space: "wA".into(),
                issue: issue.into(),
                ..Default::default()
            })
            .unwrap();
    }

    fn node(identifier: &str, state: (&str, &str, &str)) -> Value {
        json!({
            "id": format!("id-{identifier}"),
            "identifier": identifier,
            "title": format!("title {identifier}"),
            "url": format!("https://linear.app/x/{identifier}"),
            "state": {"id": state.0, "name": state.1, "type": state.2},
            "team": {"id": "team-1", "key": "WEB", "name": "Web"},
            "assignee": null,
            "priority": 2,
            "labels": {"nodes": []},
        })
    }

    fn linear_reply(body: &str) -> Value {
        if body.contains("issues(") {
            json!({"data": {"issues": {
                "nodes": [
                    node("WEB-1", ("s-doing", "In Progress", "started")),
                    node("WEB-2", ("s-todo", "Todo", "unstarted")),
                ],
                "pageInfo": {"hasNextPage": false, "endCursor": null}
            }}})
        } else if body.contains("project(id") {
            json!({"data": {"project": {
                "id": "project-a", "name": "Project A", "url": "https://linear.app/p",
                "teams": {"nodes": [{"id": "team-1", "key": "WEB", "name": "Web", "states": {"nodes": [
                    {"id": "s-todo", "name": "Todo", "type": "unstarted"},
                    {"id": "s-doing", "name": "In Progress", "type": "started"}
                ]}}]}
            }}})
        } else {
            json!({"errors": [{"message": "unexpected query"}]})
        }
    }

    /// A loopback stand-in for Linear's GraphQL endpoint; the thread outlives
    /// the test and ends with the test process.
    fn fake_linear() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/graphql", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let reply = linear_reply(&String::from_utf8_lossy(&body)).to_string();
                let mut stream = reader.into_inner();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        url
    }

    fn is_minutes(text: &str) -> bool {
        text.strip_suffix('m')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    }

    #[test]
    fn a_bound_session_shows_issue_column_run_time_marks_and_pending_requests() {
        let url = fake_linear();
        let (td, _store) = daemon_with_bound_space_and(&[
            ("BOARD_LINEAR_API_URL", &url),
            ("LINEAR_API_KEY", "lin_api_u6test"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let worktree = git_worktree(dir.path(), "wt");
        bind(&td, &worktree, "WEB-1");
        let warm = td.board_with_env(
            &["linear", "snapshot", "wA", "--json"],
            &[("HERDR_SOCKET_PATH", HERDR_SOCKET)],
        );
        assert_eq!(json_output(&warm)["linear"]["status"], "ok");
        mark(&td, "WEB-1", MarkKind::Question);
        mark(&td, "WEB-1", MarkKind::Attention);
        mark(&td, "WEB-2", MarkKind::Done);
        request(&td, "WEB-2");

        let line = status_line(&td, &worktree, &herdr_env(), &claude_input(&worktree));

        let parts: Vec<&str> = line.trim_end_matches('\n').split(" · ").collect();
        assert_eq!(parts.len(), 5, "{line:?}");
        assert_eq!(parts[0], "WEB-1", "{line:?}");
        assert_eq!(parts[1], "In Progress", "{line:?}");
        assert!(is_minutes(parts[2]), "{line:?}");
        assert_eq!(parts[3], "!?", "{line:?}");
        assert_eq!(parts[4], "◉1", "{line:?}");
        assert_eq!(line.lines().count(), 1, "{line:?}");
    }

    #[test]
    fn a_cold_cache_leaves_the_column_out() {
        let (td, _store) = daemon_with_bound_space();
        let dir = tempfile::tempdir().unwrap();
        let worktree = git_worktree(dir.path(), "wt");
        bind(&td, &worktree, "WEB-1");

        let line = status_line(&td, &worktree, &herdr_env(), &claude_input(&worktree));

        let parts: Vec<&str> = line.trim_end().split(" · ").collect();
        assert_eq!(parts.len(), 2, "{line:?}");
        assert_eq!(parts[0], "WEB-1");
        assert!(is_minutes(parts[1]), "{line:?}");
    }

    #[test]
    fn the_session_cwd_comes_from_stdin_before_the_process_cwd() {
        let (td, _store) = daemon_with_bound_space();
        let dir = tempfile::tempdir().unwrap();
        let worktree = git_worktree(dir.path(), "wt");
        let elsewhere = git_worktree(dir.path(), "elsewhere");
        bind(&td, &worktree, "WEB-1");
        std::fs::create_dir_all(worktree.join("src")).unwrap();

        let from_stdin = status_line(
            &td,
            &elsewhere,
            &herdr_env(),
            &claude_input(&worktree.join("src")),
        );
        assert!(from_stdin.starts_with("WEB-1 · "), "{from_stdin:?}");

        let from_process = status_line(&td, &worktree, &herdr_env(), "");
        assert!(from_process.starts_with("WEB-1 · "), "{from_process:?}");
    }

    #[test]
    fn an_unbound_session_says_so() {
        let (td, _store) = daemon_with_bound_space();
        let dir = tempfile::tempdir().unwrap();
        let worktree = git_worktree(dir.path(), "wt");

        let line = status_line(&td, &worktree, &herdr_env(), &claude_input(&worktree));
        assert_eq!(line, "board: not bound\n");

        let other_space = [
            ("HERDR_SOCKET_PATH", HERDR_SOCKET),
            ("HERDR_PANE_ID", "wZ:p1"),
            ("HERDR_WORKSPACE_ID", "wZ"),
        ];
        let line = status_line(&td, &worktree, &other_space, &claude_input(&worktree));
        assert_eq!(line, "board: not bound\n");
    }

    #[test]
    fn a_daemon_that_is_not_running_reads_as_down_and_is_not_started() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("boardd.sock");

        let (out, elapsed) = board_with_stdin(
            &socket,
            dir.path(),
            dir.path(),
            &["linear", "status-line"],
            &herdr_env(),
            &claude_input(dir.path()),
        );

        assert_eq!(out.status.code(), Some(0));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "board: daemon down\n");
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
        std::thread::sleep(Duration::from_millis(300));
        assert!(!socket.exists(), "the status line started a daemon");

        let (out, _) = board_with_stdin(
            &socket,
            dir.path(),
            dir.path(),
            &["linear", "session", "--json"],
            &herdr_env(),
            "",
        );
        assert_ne!(out.status.code(), Some(0));
        assert!(out.stdout.is_empty());
        std::thread::sleep(Duration::from_millis(300));
        assert!(!socket.exists(), "the session verb started a daemon");
    }

    #[test]
    fn without_a_herdr_environment_it_prints_nothing() {
        let (td, _store) = daemon_with_bound_space();
        let dir = tempfile::tempdir().unwrap();
        let worktree = git_worktree(dir.path(), "wt");
        bind(&td, &worktree, "WEB-1");

        assert_eq!(
            status_line(&td, &worktree, &[], &claude_input(&worktree)),
            ""
        );
    }

    #[test]
    fn session_json_is_the_protocol_result() {
        let (td, _store) = daemon_with_bound_space();
        let dir = tempfile::tempdir().unwrap();
        let worktree = git_worktree(dir.path(), "wt");
        bind(&td, &worktree, "WEB-1");
        mark(&td, "WEB-1", MarkKind::Attention);
        request(&td, "WEB-1");

        let (out, _) = board_with_stdin(
            &td.socket,
            td._dir.path(),
            &worktree,
            &["linear", "session", "--json"],
            &herdr_env(),
            "",
        );
        let doc = json_output(&out);
        let expected = td
            .client()
            .linear_session_get(&LinearSessionGetParams {
                space: "wA".into(),
                herdr_socket: Some(HERDR_SOCKET.into()),
                herdr_pane_id: Some("wA:p2".into()),
                claude_session_id: None,
                cwd: Some(worktree.to_str().unwrap().into()),
            })
            .unwrap();
        let parsed: LinearSessionGetResult = serde_json::from_value(doc.clone()).unwrap();
        assert_eq!(parsed, expected);
        assert_eq!(doc["space"], "wA");
        assert_eq!(doc["space_bound"], true);
        assert_eq!(doc["binding"]["issue"], "WEB-1");
        assert_eq!(doc["marks"][0]["kind"], "attention");
        assert_eq!(doc["pending_requests"], 1);

        let (out, _) = board_with_stdin(
            &td.socket,
            td._dir.path(),
            &worktree,
            &["linear", "session"],
            &herdr_env(),
            "",
        );
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        for cell in ["WEB-1", "attention", "wA"] {
            assert!(text.contains(cell), "{text}");
        }
    }
}
