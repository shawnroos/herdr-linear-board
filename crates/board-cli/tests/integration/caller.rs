//! `board caller`: the shell door to the caller-pane resolver, driven against
//! a scripted boardd so each answer state is exact.

use std::path::Path;
use std::process::{Command, Output};
use std::sync::mpsc::Receiver;

use board_core::protocol::Request;
use serde_json::{json, Value};

use super::{scripted_boardd, BOARD_BIN};

fn board_caller(socket: &Path, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(BOARD_BIN);
    cmd.arg("caller")
        .args(args)
        .current_dir(cwd)
        .env("BOARD_SOCKET", socket)
        .env("BOARD_DB", cwd.join("board.db"))
        .env("HERDR_BOARD_CONFIG", cwd.join("missing-config.toml"))
        .env("HOME", cwd)
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_TAB_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID");
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("run board caller")
}

fn json_out(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "board caller failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout is not JSON: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn answering(socket: &Path, resolve: Value) -> Receiver<Request> {
    scripted_boardd(socket, move |request| match request.method.as_str() {
        "caller.resolve" => Ok(resolve.clone()),
        other => Err((3, format!("unexpected {other}"))),
    })
}

fn candidates() -> Value {
    json!([{
        "pane": "default/w1:p2",
        "session": "default",
        "socket": "/tmp/default.sock",
        "workspace_id": "w1",
        "workspace_label": "api",
        "tab_id": "w1:t1",
        "pane_id": "w1:p2",
        "title": "✳ Claude Code"
    }])
}

fn location() -> Value {
    json!({
        "session": "work",
        "socket": "/tmp/work.sock",
        "workspace_id": "w4",
        "tab_id": "w4:t1",
        "pane_id": "w4:p1"
    })
}

#[test]
fn full_env_prints_the_env_location_without_contacting_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let env = [
        ("HERDR_SOCKET_PATH", "/tmp/x/sessions/work/herdr.sock"),
        ("HERDR_PANE_ID", "w4:p1"),
        ("HERDR_WORKSPACE_ID", "w4"),
        ("HERDR_TAB_ID", "w4:t1"),
        ("CLAUDE_CODE_SESSION_ID", "sess-1"),
    ];
    let out = json_out(&board_caller(&socket, dir.path(), &["--json"], &env));
    assert_eq!(
        out,
        json!({
            "state": "resolved",
            "location": {
                "session": "work",
                "socket": "/tmp/x/sessions/work/herdr.sock",
                "workspace_id": "w4",
                "tab_id": "w4:t1",
                "pane_id": "w4:p1"
            }
        })
    );
    assert!(!socket.exists(), "no daemon was started or contacted");
}

#[test]
fn without_env_an_unconfirmed_lookup_prints_the_candidates_and_their_pane_values() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = answering(
        &socket,
        json!({"state": "unconfirmed", "candidates": candidates()}),
    );
    let cwd = dir.path().canonicalize().unwrap();
    let out = json_out(&board_caller(
        &socket,
        &cwd,
        &["--json"],
        &[("CLAUDE_CODE_SESSION_ID", "sess-1")],
    ));
    assert_eq!(out["state"], "unconfirmed");
    assert_eq!(out["candidates"], candidates());

    let request = requests.try_recv().expect("the daemon was asked");
    assert_eq!(request.method, "caller.resolve");
    assert_eq!(request.params["cwd"], cwd.to_str().unwrap());
    assert_eq!(request.params["claude_session_id"], "sess-1");
    assert!(request.params.get("pane").is_none(), "{}", request.params);
}

#[test]
fn without_env_a_remembered_location_prints_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let _requests = answering(
        &socket,
        json!({"state": "resolved", "location": location()}),
    );
    let out = json_out(&board_caller(
        &socket,
        dir.path(),
        &["--json"],
        &[("CLAUDE_CODE_SESSION_ID", "sess-1")],
    ));
    assert_eq!(out, json!({"state": "resolved", "location": location()}));
}

#[test]
fn without_env_and_no_match_prints_not_in_herdr() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let _requests = answering(&socket, json!({"state": "not_in_herdr"}));
    let out = json_out(&board_caller(&socket, dir.path(), &["--json"], &[]));
    assert_eq!(out, json!({"state": "not_in_herdr"}));
}

#[test]
fn a_pane_is_sent_with_the_session_id_so_the_daemon_remembers_it() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let requests = answering(
        &socket,
        json!({"state": "resolved", "location": location()}),
    );
    let env = [
        ("HERDR_SOCKET_PATH", "/tmp/herdr-env.sock"),
        ("HERDR_PANE_ID", "w1:p2"),
        ("HERDR_WORKSPACE_ID", "w1"),
        ("CLAUDE_CODE_SESSION_ID", "sess-1"),
    ];
    let out = json_out(&board_caller(
        &socket,
        dir.path(),
        &["--json", "--pane", "work/w4:p1"],
        &env,
    ));
    assert_eq!(out, json!({"state": "resolved", "location": location()}));

    let request = requests
        .try_recv()
        .expect("a named pane always asks the daemon");
    assert_eq!(request.params["pane"], "work/w4:p1");
    assert_eq!(request.params["claude_session_id"], "sess-1");
}

#[test]
fn text_mode_lists_each_candidate_with_its_pane_value() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let _requests = answering(
        &socket,
        json!({"state": "unconfirmed", "candidates": candidates()}),
    );
    let output = board_caller(&socket, dir.path(), &[], &[]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "unconfirmed",
        "default/w1:p2",
        "api",
        "✳ Claude Code",
        "--pane",
    ] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
}

#[test]
fn a_named_pane_no_session_lists_is_echoed_so_it_differs_from_no_folder_match() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fake.sock");
    let _requests = answering(&socket, json!({"state": "not_in_herdr"}));
    let out = json_out(&board_caller(
        &socket,
        dir.path(),
        &["--json", "--pane", "s9/w9:p9"],
        &[],
    ));
    assert_eq!(out, json!({"state": "not_in_herdr", "pane": "s9/w9:p9"}));
}
