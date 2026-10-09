//! `board linear report`: a Claude Code PostToolUse hook pipes its payload in
//! on stdin; the verb sends one `linear.activity.record` and always exits 0.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use board_core::client::{BoardClient, UnixClient};
use std::sync::mpsc::Receiver;

use board_core::protocol::{
    ActivityClaims, LinearActivityListParams, LinearActivityRecordParams, LinearBindParams,
    LinearImportParams, LinearStateGetParams, MarkKind, Request,
};
use serde_json::{json, Value};

use super::{scripted_boardd, TestDaemon, BOARD_BIN};

const HERDR_SOCKET: &str = "/tmp/herdr-test.sock";
/// The verb's own bound is 5 s; the slack covers process start on a busy box.
const EXIT_WITHIN: Duration = Duration::from_secs(7);

struct Report<'a> {
    socket: &'a Path,
    home: &'a Path,
    cwd: &'a Path,
    claims: &'a [(&'a str, &'a str)],
}

impl Report<'_> {
    fn run(&self, payload: &str) -> (Output, Duration) {
        let mut cmd = Command::new(BOARD_BIN);
        cmd.args(["linear", "report"])
            .current_dir(self.cwd)
            .env("BOARD_SOCKET", self.socket)
            .env("BOARD_DB", self.home.join("board.db"))
            .env("HERDR_BOARD_CONFIG", self.home.join("missing-config.toml"))
            .env("HOME", self.home)
            .env("BOARD_SPAWNER", "local")
            .env("BOARD_BIN", BOARD_BIN)
            .env_remove("BOARD_SCOPE_PATH")
            .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_PLUGIN_ID")
            .env_remove("BOARD_CARD_ID")
            .env_remove("BOARD_RUN_ID")
            .env_remove("CLAUDE_PROJECT_DIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in self.claims {
            cmd.env(key, value);
        }
        let started = Instant::now();
        let mut child = cmd.spawn().expect("spawn board linear report");
        let mut stdin = child.stdin.take().unwrap();
        // A report that exits before reading all of stdin breaks this pipe;
        // the exit status is what the test judges.
        let _ = stdin.write_all(payload.as_bytes());
        drop(stdin);
        let out = child.wait_with_output().expect("wait for report");
        (out, started.elapsed())
    }
}

fn for_daemon<'a>(
    td: &'a TestDaemon,
    cwd: &'a Path,
    claims: &'a [(&'a str, &'a str)],
) -> Report<'a> {
    Report {
        socket: &td.socket,
        home: td._dir.path(),
        cwd,
        claims,
    }
}

fn assert_silent_success(out: &Output) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "stdout must stay empty: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

pub(crate) fn git_worktree(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(path.join(".git")).unwrap();
    path.canonicalize().unwrap()
}

/// A daemon whose session `default` has space `wA` bound to a Linear project,
/// seeded through the work-store import.
pub(crate) fn daemon_with_bound_space() -> (TestDaemon, tempfile::TempDir) {
    daemon_with_bound_space_and(&[])
}

pub(crate) fn daemon_with_bound_space_and(
    extra: &[(&str, &str)],
) -> (TestDaemon, tempfile::TempDir) {
    let store = tempfile::tempdir().unwrap();
    let workspaces = store.path().join("workspaces");
    std::fs::create_dir_all(&workspaces).unwrap();
    let record = workspaces.join("wA.json");
    std::fs::write(
        &record,
        json!({
            "version": 1,
            "worktree_path": "workspace:wA",
            "state": "bound",
            "issue_identifier": "project-a",
        })
        .to_string(),
    )
    .unwrap();
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut env = vec![("HERDR_LINEAR_STORE_DIR", store.path().to_str().unwrap())];
    env.extend_from_slice(extra);
    let td = TestDaemon::start(&env);
    td.client()
        .linear_import(&LinearImportParams { dry_run: false })
        .unwrap();
    (td, store)
}

fn pane_claims() -> [(&'static str, &'static str); 3] {
    [
        ("HERDR_SOCKET_PATH", HERDR_SOCKET),
        ("HERDR_PANE_ID", "wA:p2"),
        ("HERDR_WORKSPACE_ID", "wA"),
    ]
}

fn content(text: Value) -> Value {
    json!([{"type": "text", "text": text.to_string()}])
}

fn payload(tool_name: &str, cwd: &Path, response: Value) -> String {
    let server = tool_name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.rsplit_once("__"))
        .map(|(server, _)| server)
        .unwrap_or("");
    json!({
        "session_id": "s-1",
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": cwd.to_str().unwrap(),
        "permission_mode": "default",
        "hook_event_name": "PostToolUse",
        "tool_name": tool_name,
        "tool_input": {"title": "A ticket"},
        "tool_response": response,
        "tool_use_id": "toolu_1",
        "duration_ms": 120,
        "mcp_server": {"name": server, "source": "claude.ai"},
    })
    .to_string()
}

fn saved_issue(id: &str, created: bool) -> Value {
    content(json!({
        "id": id,
        "uuid": "3f1c1b0e-9f3a-4c61-8d0e-2a4b5c6d7e8f",
        "title": "A ticket",
        "createdAt": "2026-09-30T10:00:00.000Z",
        "updatedAt": if created { "2026-09-30T10:00:00.000Z" } else { "2026-09-30T11:00:00.000Z" },
    }))
}

/// Activity is listed per space; the tests report into `wA` or into no space.
fn activity_in(client: &mut UnixClient) -> Vec<board_core::db::Activity> {
    [Some("wA".to_string()), None]
        .into_iter()
        .flat_map(|space| {
            client
                .linear_activity_list(&LinearActivityListParams {
                    space,
                    ..LinearActivityListParams::default()
                })
                .unwrap()
                .activity
        })
        .collect()
}

fn activity(td: &TestDaemon) -> Vec<board_core::db::Activity> {
    activity_in(&mut td.client())
}

#[test]
fn a_save_issue_from_a_known_unbound_pane_links_that_session() {
    let (td, _store) = daemon_with_bound_space();
    let dir = tempfile::tempdir().unwrap();
    let worktree = git_worktree(dir.path(), "wt");
    let claims = pane_claims();

    let (out, _) = for_daemon(&td, &worktree, &claims).run(&payload(
        "mcp__claude_ai_Linear__save_issue",
        &worktree,
        saved_issue("ENG-7", false),
    ));
    assert_silent_success(&out);

    let state = td
        .client()
        .linear_state_get(&LinearStateGetParams {
            space: "wA".into(),
            herdr_socket: None,
        })
        .unwrap();
    assert!(
        state
            .worktree_bindings
            .iter()
            .any(|b| b.worktree_path == worktree.to_str().unwrap() && b.issue == "ENG-7"),
        "{:?}",
        state.worktree_bindings
    );
    let recorded = activity(&td);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].tool_name, "mcp__claude_ai_Linear__save_issue");
    assert_eq!(recorded[0].issue.as_deref(), Some("ENG-7"));
    assert_eq!(recorded[0].space.as_deref(), Some("wA"));
    assert_eq!(recorded[0].claims.herdr_pane_id.as_deref(), Some("wA:p2"));
}

#[test]
fn the_same_save_issue_from_a_bound_session_becomes_a_suggestion() {
    let (td, _store) = daemon_with_bound_space();
    let dir = tempfile::tempdir().unwrap();
    let worktree = git_worktree(dir.path(), "wt");
    td.client()
        .linear_bind(&LinearBindParams {
            cwd: worktree.to_str().unwrap().into(),
            issue: "ENG-1".into(),
            space: Some("wA".into()),
            claims: ActivityClaims::default(),
            ..LinearBindParams::default()
        })
        .unwrap();
    let claims = pane_claims();

    let (out, _) = for_daemon(&td, &worktree, &claims).run(&payload(
        "mcp__claude_ai_Linear__save_issue",
        &worktree,
        saved_issue("ENG-7", false),
    ));
    assert_silent_success(&out);

    let state = td
        .client()
        .linear_state_get(&LinearStateGetParams {
            space: "wA".into(),
            herdr_socket: None,
        })
        .unwrap();
    assert!(
        state
            .marks
            .iter()
            .any(|m| m.issue == "ENG-7" && m.kind == MarkKind::Suggestion),
        "{:?}",
        state.marks
    );
    assert!(
        state.worktree_bindings.iter().all(|b| b.issue != "ENG-7"),
        "a bound session was rebound silently"
    );
}

#[test]
fn reads_and_non_linear_servers_record_nothing() {
    let td = TestDaemon::start(&[]);
    let dir = tempfile::tempdir().unwrap();
    let claims = pane_claims();
    let report = for_daemon(&td, dir.path(), &claims);

    for tool in [
        "mcp__claude_ai_Linear__get_issue",
        "mcp__claude_ai_Linear__list_issues",
        "mcp__linear__search_documentation",
        "mcp__claude_ai_Linear__extract_images",
        "mcp__github__save_issue",
        "mcp__board__mark",
        "Bash",
    ] {
        let (out, _) = report.run(&payload(tool, dir.path(), saved_issue("ENG-7", false)));
        assert_silent_success(&out);
    }
    assert!(activity(&td).is_empty(), "{:?}", activity(&td));
}

#[test]
fn a_save_comment_records_activity_and_links_nothing() {
    let (td, _store) = daemon_with_bound_space();
    let dir = tempfile::tempdir().unwrap();
    let worktree = git_worktree(dir.path(), "wt");
    let claims = pane_claims();

    let (out, _) = for_daemon(&td, &worktree, &claims).run(&payload(
        "mcp__claude_ai_Linear__save_comment",
        &worktree,
        content(json!({"id": "c-1", "body": "done"})),
    ));
    assert_silent_success(&out);

    let recorded = activity(&td);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].tool_name, "mcp__claude_ai_Linear__save_comment");
    assert_eq!(recorded[0].issue, None);
    let state = td
        .client()
        .linear_state_get(&LinearStateGetParams {
            space: "wA".into(),
            herdr_socket: None,
        })
        .unwrap();
    assert!(state.worktree_bindings.is_empty());
    assert!(state.marks.is_empty());
}

#[test]
fn a_save_issue_create_response_yields_the_new_identifier() {
    let td = TestDaemon::start(&[]);
    let dir = tempfile::tempdir().unwrap();
    let claims = [("HERDR_WORKSPACE_ID", "wA")];

    let (out, _) = for_daemon(&td, dir.path(), &claims).run(&payload(
        "mcp__claude_ai_Linear__save_issue",
        dir.path(),
        saved_issue("WEB-3318", true),
    ));
    assert_silent_success(&out);

    let recorded = activity(&td);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].issue.as_deref(), Some("WEB-3318"));
}

#[test]
fn a_save_issue_without_a_valid_identifier_records_nothing() {
    let td = TestDaemon::start(&[]);
    let dir = tempfile::tempdir().unwrap();
    let claims = [("HERDR_WORKSPACE_ID", "wA")];
    let report = for_daemon(&td, dir.path(), &claims);

    for response in [
        content(json!({"id": "WEB-1\u{1b}[31m", "uuid": "not-a-uuid"})),
        json!([{"type": "text", "text": "Error: issue not found"}]),
        json!(null),
    ] {
        let (out, _) = report.run(&payload(
            "mcp__claude_ai_Linear__save_issue",
            dir.path(),
            response,
        ));
        assert_silent_success(&out);
    }
    assert!(activity(&td).is_empty(), "{:?}", activity(&td));
}

#[test]
fn with_no_daemon_running_a_save_issue_report_starts_boardd_and_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let claims = [("HERDR_WORKSPACE_ID", "wA")];
    let report = Report {
        socket: &socket,
        home: dir.path(),
        cwd: dir.path(),
        claims: &claims,
    };

    let (out, elapsed) = report.run(&payload(
        "mcp__claude_ai_Linear__save_issue",
        dir.path(),
        saved_issue("ENG-9", true),
    ));
    let started = UnixClient::connect(&socket);
    let recorded = started
        .as_ref()
        .ok()
        .map(|_| activity_in(&mut UnixClient::connect(&socket).unwrap()));
    if let Ok(mut client) = started {
        let _ = client.daemon_stop();
    }

    assert_silent_success(&out);
    assert!(elapsed < EXIT_WITHIN, "took {elapsed:?}");
    let recorded = recorded.expect("the report did not start boardd");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].issue.as_deref(), Some("ENG-9"));
}

#[test]
fn malformed_json_exits_0_quietly() {
    let td = TestDaemon::start(&[]);
    let dir = tempfile::tempdir().unwrap();
    let report = for_daemon(&td, dir.path(), &[]);

    for input in ["", "{not json", "[1, 2]", "\"text\""] {
        let (out, elapsed) = report.run(input);
        assert_silent_success(&out);
        assert!(elapsed < EXIT_WITHIN, "took {elapsed:?}");
    }
    assert!(activity(&td).is_empty());
}

#[test]
fn a_failed_daemon_start_exits_0_within_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("not-a-dir");
    std::fs::write(&blocker, "").unwrap();
    let socket = blocker.join("boardd.sock");
    let report = Report {
        socket: &socket,
        home: dir.path(),
        cwd: dir.path(),
        claims: &[],
    };

    let (out, elapsed) = report.run(&payload(
        "mcp__claude_ai_Linear__save_issue",
        dir.path(),
        saved_issue("ENG-9", true),
    ));
    assert_silent_success(&out);
    assert!(elapsed < EXIT_WITHIN, "took {elapsed:?}");
}

#[test]
fn a_daemon_that_never_answers_exits_0_within_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mute.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    let report = Report {
        socket: &socket,
        home: dir.path(),
        cwd: dir.path(),
        claims: &[],
    };

    let (out, elapsed) = report.run(&payload(
        "mcp__claude_ai_Linear__save_issue",
        dir.path(),
        saved_issue("ENG-9", true),
    ));
    assert_silent_success(&out);
    assert!(elapsed < EXIT_WITHIN, "took {elapsed:?}");
    assert!(
        elapsed >= Duration::from_secs(4),
        "returned before the daemon could answer: {elapsed:?}"
    );
}

fn recorded(claims: &Value) -> Value {
    json!({
        "activity": {
            "id": 1, "space": claims["herdr_workspace_id"], "tool_name": "t", "issue": null,
            "claims": claims, "created_at": "2026-10-08 10:00:00",
        },
        "outcome": "recorded",
    })
}

/// A boardd stand-in that answers `caller.resolve` with `resolve` after
/// `stall`, and records any activity.
fn resolving_boardd(socket: &Path, resolve: Value, stall: Duration) -> Receiver<Request> {
    scripted_boardd(socket, move |request| match request.method.as_str() {
        "caller.resolve" => {
            std::thread::sleep(stall);
            Ok(resolve.clone())
        }
        "linear.activity.record" => Ok(recorded(&request.params["claims"])),
        other => Err((3, format!("unexpected {other}"))),
    })
}

fn requests(rx: &Receiver<Request>) -> Vec<Request> {
    let mut seen = Vec::new();
    while let Ok(request) = rx.recv_timeout(Duration::from_millis(300)) {
        seen.push(request);
    }
    seen
}

fn record_of(seen: &[Request]) -> LinearActivityRecordParams {
    let records: Vec<_> = seen
        .iter()
        .filter(|r| r.method == "linear.activity.record")
        .collect();
    assert_eq!(records.len(), 1, "{seen:?}");
    serde_json::from_value(records[0].params.clone()).unwrap()
}

fn resolved_at(pane: &str, workspace: &str) -> Value {
    json!({"state": "resolved", "location": {
        "session": "work", "socket": "/tmp/work.sock", "workspace_id": workspace,
        "tab_id": "w7:t1", "pane_id": pane,
    }})
}

fn one_candidate() -> Value {
    json!({"state": "unconfirmed", "candidates": [{
        "pane": "work/w7:p3", "session": "work", "socket": "/tmp/work.sock",
        "workspace_id": "w7", "workspace_label": "api", "tab_id": "w7:t1",
        "pane_id": "w7:p3", "title": "claude",
    }]})
}

fn scripted_report<'a>(
    dir: &'a Path,
    socket: &'a Path,
    claims: &'a [(&'a str, &'a str)],
) -> Report<'a> {
    Report {
        socket,
        home: dir,
        cwd: dir,
        claims,
    }
}

fn a_save_comment(cwd: &Path) -> String {
    payload(
        "mcp__claude_ai_Linear__save_comment",
        cwd,
        content(json!({"id": "c-1"})),
    )
}

#[test]
fn without_herdr_env_or_a_remembered_pane_activity_is_recorded_without_a_space() {
    let td = TestDaemon::start(&[]);
    let dir = tempfile::tempdir().unwrap();

    let (out, elapsed) = for_daemon(&td, dir.path(), &[]).run(&a_save_comment(dir.path()));

    assert_silent_success(&out);
    assert!(elapsed < EXIT_WITHIN, "took {elapsed:?}");
    let recorded = activity(&td);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].space, None);
    assert_eq!(recorded[0].claims, ActivityClaims::default());
}

#[test]
fn a_remembered_pane_attributes_the_activity_to_its_space_and_pane() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let rx = resolving_boardd(&socket, resolved_at("w7:p3", "w7"), Duration::ZERO);

    let (out, _) = scripted_report(dir.path(), &socket, &[]).run(&a_save_comment(dir.path()));

    assert_silent_success(&out);
    let seen = requests(&rx);
    let resolve = seen
        .iter()
        .find(|r| r.method == "caller.resolve")
        .expect("no caller.resolve");
    assert_eq!(resolve.params["claude_session_id"], "s-1");
    assert_eq!(
        resolve.params["remembered_only"], true,
        "{:?}",
        resolve.params
    );
    assert!(resolve.params.get("pane").is_none(), "{:?}", resolve.params);
    let record = record_of(&seen);
    assert_eq!(record.claims.herdr_workspace_id.as_deref(), Some("w7"));
    assert_eq!(record.claims.herdr_pane_id.as_deref(), Some("w7:p3"));
    assert_eq!(
        record.claims.herdr_socket.as_deref(),
        Some("/tmp/work.sock")
    );
}

#[test]
fn a_folder_candidate_nobody_confirmed_leaves_the_activity_without_a_space() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let rx = resolving_boardd(&socket, one_candidate(), Duration::ZERO);

    let (out, _) = scripted_report(dir.path(), &socket, &[]).run(&a_save_comment(dir.path()));

    assert_silent_success(&out);
    let seen = requests(&rx);
    assert!(
        seen.iter().any(|r| r.method == "caller.resolve"),
        "{seen:?}"
    );
    assert_eq!(record_of(&seen).claims, ActivityClaims::default());
}

#[test]
fn full_herdr_env_records_without_asking_the_resolver() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let rx = resolving_boardd(&socket, resolved_at("w7:p3", "w7"), Duration::ZERO);
    let claims = pane_claims();

    let (out, _) = scripted_report(dir.path(), &socket, &claims).run(&a_save_comment(dir.path()));

    assert_silent_success(&out);
    let seen = requests(&rx);
    assert!(
        seen.iter().all(|r| r.method != "caller.resolve"),
        "{seen:?}"
    );
    assert_eq!(
        record_of(&seen).claims.herdr_pane_id.as_deref(),
        Some("wA:p2")
    );
}

#[test]
fn a_stalled_resolve_still_records_the_activity_on_a_fresh_connection() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let rx = resolving_boardd(&socket, resolved_at("w7:p3", "w7"), Duration::from_secs(4));

    let (out, elapsed) = scripted_report(dir.path(), &socket, &[]).run(&a_save_comment(dir.path()));

    assert_silent_success(&out);
    assert!(elapsed < EXIT_WITHIN, "took {elapsed:?}");
    let first = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("the activity was never recorded");
    assert_eq!(first.method, "linear.activity.record");
    let record: LinearActivityRecordParams = serde_json::from_value(first.params).unwrap();
    assert_eq!(record.claims, ActivityClaims::default());
}

#[test]
fn the_resolver_is_asked_about_the_project_dir_before_the_payload_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().canonicalize().unwrap();
    let sub = project.join("crates/sub");
    std::fs::create_dir_all(&sub).unwrap();
    let socket = dir.path().join("boardd.sock");
    let rx = resolving_boardd(&socket, one_candidate(), Duration::ZERO);
    let claims = [("CLAUDE_PROJECT_DIR", project.to_str().unwrap())];

    let (out, _) = scripted_report(dir.path(), &socket, &claims).run(&a_save_comment(&sub));

    assert_silent_success(&out);
    let seen = requests(&rx);
    let resolve = seen
        .iter()
        .find(|r| r.method == "caller.resolve")
        .expect("no caller.resolve");
    assert_eq!(resolve.params["cwd"], project.to_str().unwrap());
    assert_eq!(record_of(&seen).cwd.as_deref(), sub.to_str());
}
