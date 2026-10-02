//! `board linear snapshot`, `issue` and the three list verbs, driven end to
//! end against a real `board daemon --foreground` that reads a loopback fake
//! Linear.

use std::process::Output;

use board_core::client::BoardClient;
use board_core::protocol::{ActivityClaims, LinearSpaceBindParams};
use serde_json::{json, Value};

use super::{fake_linear, json_error, json_output, TestDaemon};

const CLI_ERROR: i32 = 64;

fn code(out: &Output) -> i32 {
    out.status.code().expect("board exits, never signals")
}

fn project_node(id: &str, name: &str, team_key: Option<&str>) -> Value {
    let teams: Vec<Value> = team_key
        .map(|key| json!({"id": format!("team-{key}"), "key": key, "name": key}))
        .into_iter()
        .collect();
    json!({"id": id, "name": name, "teams": {"nodes": teams}})
}

fn page(field: &str, nodes: Vec<Value>) -> Value {
    json!({"data": {field: {"nodes": nodes, "pageInfo": {"hasNextPage": false, "endCursor": null}}}})
}

fn issue_node(identifier: &str, state: (&str, &str, &str)) -> Value {
    json!({
        "id": format!("id-{identifier}"),
        "identifier": identifier,
        "title": format!("title {identifier}"),
        "url": null,
        "state": {"id": state.0, "name": state.1, "type": state.2},
        "team": {"id": "team-EX", "key": "EX", "name": "EX"},
        "assignee": null,
        "priority": 2,
        "labels": {"nodes": []},
    })
}

fn linear_reply(body: &str) -> Value {
    if body.contains("projects(") {
        page(
            "projects",
            vec![
                project_node("proj-1", "Example", Some("EX")),
                project_node("proj-2", "Sample", Some("SA")),
            ],
        )
    } else if body.contains("customViews(") {
        page(
            "customViews",
            vec![
                json!({"id": "view-1", "name": "Open work", "archivedAt": null,
                       "filterData": {"project": {"id": {"eq": "proj-1"}}}}),
                json!({"id": "view-2", "name": "Elsewhere", "archivedAt": null,
                       "filterData": {"project": {"id": {"eq": "proj-9"}}}}),
            ],
        )
    } else if body.contains("issues(") {
        page(
            "issues",
            vec![issue_node("EX-1", ("s-todo", "Todo", "unstarted"))],
        )
    } else if body.contains("project(id") {
        json!({"data": {"project": {
            "id": "proj-1", "name": "Example", "url": null,
            "teams": {"nodes": [{"id": "team-EX", "key": "EX", "name": "EX", "states": {"nodes": [
                {"id": "s-todo", "name": "Todo", "type": "unstarted"},
                {"id": "s-done", "name": "Done", "type": "completed"}
            ]}}]}
        }}})
    } else {
        json!({"errors": [{"message": "unexpected query"}]})
    }
}

/// A daemon reading the fake Linear that `reply` answers for.
fn daemon_with(reply: impl Fn(&str) -> Value + Send + 'static) -> TestDaemon {
    let url = fake_linear(reply);
    TestDaemon::start(&[
        ("BOARD_LINEAR_API_URL", url.as_str()),
        ("LINEAR_API_KEY", "lin_api_u12test"),
    ])
}

fn daemon() -> TestDaemon {
    daemon_with(linear_reply)
}

fn stdout_text(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).expect("UTF-8 stdout")
}

fn bind_space(td: &TestDaemon) {
    td.client()
        .linear_space_bind(&LinearSpaceBindParams {
            space: "wA".into(),
            project: "proj-1".into(),
            view: None,
            claims: ActivityClaims::default(),
        })
        .unwrap();
}

#[test]
fn a_bound_space_snapshot_carries_the_grouped_board_from_linear() {
    let td = daemon();
    bind_space(&td);
    let doc = json_output(&td.board(&["linear", "snapshot", "wA", "--json"]));
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["workspace"]["id"], "wA");
    assert_eq!(doc["record"]["state"], "bound");
    assert_eq!(doc["project"]["name"], "Example");
    assert_eq!(doc["linear"]["status"], "ok");
    let todo = doc["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["key"] == "s-todo")
        .expect("a Todo group");
    assert_eq!(todo["issues"], json!(["EX-1"]));
}

#[test]
fn an_unbound_space_reads_without_linear_or_a_plugin() {
    let td = TestDaemon::start(&[]);
    let doc = json_output(&td.board(&["linear", "snapshot", "wA", "--json"]));
    assert_eq!(doc["workspace"]["id"], "wA");
    assert_eq!(doc["record"]["state"], "unbound");
    assert_eq!(doc["linear"]["status"], "unknown");
}

/// The retired plugin root is accepted from either side and changes nothing.
#[test]
fn a_retired_plugin_root_in_either_environment_is_ignored() {
    let url = fake_linear(linear_reply);
    let td = TestDaemon::start_with_config(
        &[
            ("BOARD_LINEAR_API_URL", url.as_str()),
            ("LINEAR_API_KEY", "lin_api_u12test"),
            ("BOARD_WORK_PLUGIN_ROOT", "/nonexistent/work-plugin"),
        ],
        "work_plugin_root = \"/nonexistent/work-plugin\"\n",
    );
    bind_space(&td);
    let doc = json_output(&td.board_with_env(
        &["linear", "snapshot", "wA", "--json"],
        &[("BOARD_WORK_PLUGIN_ROOT", "/nonexistent/caller-plugin")],
    ));
    assert_eq!(doc["record"]["state"], "bound");
    assert_eq!(doc["linear"]["status"], "ok");
}

/// Without `--json` the document is still printed as JSON.
#[test]
fn snapshot_without_json_prints_the_document() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["linear", "snapshot", "wA"]);
    let doc: Value = serde_json::from_slice(stdout_text(&out).as_bytes()).expect("JSON document");
    assert_eq!(doc["workspace"]["id"], "wA");
}

#[test]
fn issue_prints_the_one_call_document_and_refuses_a_bad_id() {
    let td = daemon_with(|body| {
        if body.contains("issue(id") {
            let mut node = issue_node("EX-7", ("s-todo", "Todo", "unstarted"));
            node["description"] = json!("why it matters");
            json!({"data": {"issue": node}})
        } else {
            json!({"errors": [{"message": "unexpected query"}]})
        }
    });
    let doc = json_output(&td.board(&["linear", "issue", "EX-7", "--json"]));
    assert_eq!(doc["status"], "ok", "{doc}");
    assert_eq!(doc["issue"]["identifier"], "EX-7");
    assert_eq!(doc["issue"]["description"], "why it matters");

    let out = td.board(&["linear", "issue", "bad id", "--json"]);
    assert_eq!(code(&out), 1);
    assert_eq!(json_error(&out)["error"]["code"], 1);
}

#[test]
fn project_list_prints_a_table_and_its_json() {
    let td = daemon();

    let text = stdout_text(&td.board(&["linear", "project", "list"]));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    for cell in ["proj-1", "EX", "Example"] {
        assert!(lines[0].contains(cell), "{text}");
    }
    assert!(
        !text.contains("status"),
        "an ok list has no status line: {text}"
    );

    let doc = json_output(&td.board(&["linear", "project", "list", "--json"]));
    assert_eq!(doc["status"], "ok");
    assert_eq!(doc["rows"][1]["team_key"], "SA");
}

#[test]
fn view_list_keeps_the_projects_views_and_prints_a_table_and_its_json() {
    let td = daemon();

    let text = stdout_text(&td.board(&["linear", "view", "list", "proj-1"]));
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("view-1"), "{text}");
    assert!(text.contains("Open work"), "{text}");

    let doc = json_output(&td.board(&["linear", "view", "list", "proj-1", "--json"]));
    assert_eq!(doc["rows"], json!([{"id": "view-1", "name": "Open work"}]));
}

#[test]
fn view_list_without_a_project_id_is_a_usage_error() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["linear", "view", "list", "--json"]);
    assert_eq!(code(&out), CLI_ERROR);
    assert_eq!(json_error(&out)["error"]["kind"], "cli");
}

/// Linear being unreachable is a list status, not an error exit.
#[test]
fn an_unreachable_linear_lists_as_unavailable_and_exits_0() {
    let td = TestDaemon::start(&[
        ("BOARD_LINEAR_API_URL", "http://127.0.0.1:9/graphql"),
        ("LINEAR_API_KEY", "lin_api_u12test"),
    ]);
    let text = stdout_text(&td.board(&["linear", "project", "list"]));
    assert!(text.lines().any(|l| l.contains("unavailable")), "{text}");

    let doc = json_output(&td.board(&["linear", "project", "list", "--json"]));
    assert_eq!(doc["status"], "unavailable");
    assert!(doc["message"].is_string(), "{doc}");
}

#[test]
fn without_a_herdr_socket_spaces_are_unavailable_while_projects_answer_fully() {
    let td = daemon();

    let text = stdout_text(&td.board(&["linear", "space", "list"]));
    assert!(text.lines().any(|l| l.contains("unavailable")), "{text}");

    let doc = json_output(&td.board(&["linear", "project", "list", "--json"]));
    assert_eq!(doc["status"], "ok");
    assert_eq!(doc["rows"].as_array().unwrap().len(), 2);
}

#[test]
fn a_name_carrying_a_bidi_override_prints_stripped() {
    let td = daemon_with(|body| {
        if body.contains("projects(") {
            page(
                "projects",
                vec![
                    project_node("proj-1", "Exa\u{202e}mple", Some("EX")),
                    project_node("proj-2", "Sam\nple", Some("SA")),
                ],
            )
        } else {
            json!({"errors": [{"message": "unexpected query"}]})
        }
    });

    let text = stdout_text(&td.board(&["linear", "project", "list"]));
    assert!(!text.contains('\u{202e}'), "{text:?}");
    assert!(text.contains("Example"), "{text}");
    // The daemon keeps a newline in Linear text; the table must not split a row.
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(text.contains("Sample"), "{text}");

    let out = td.board(&["linear", "project", "list", "--json"]);
    let raw = String::from_utf8(out.stdout.clone()).unwrap();
    assert!(!raw.contains('\u{202e}') && !raw.contains("202e"), "{raw}");
    assert_eq!(json_output(&out)["rows"][0]["name"], "Example");
}

#[test]
fn a_project_with_no_team_prints_an_empty_team_cell_and_a_null_key() {
    let td = daemon_with(|body| {
        if body.contains("projects(") {
            page("projects", vec![project_node("proj-1", "Example", None)])
        } else {
            json!({"errors": [{"message": "unexpected query"}]})
        }
    });

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
    let td = TestDaemon::start(&[]);
    let doc = json_output(&td.board_with_env(
        &["linear", "snapshot", "--json"],
        &[("HERDR_WORKSPACE_ID", "wA")],
    ));
    assert_eq!(doc["workspace"]["id"], "wA");
}

#[test]
fn snapshot_with_neither_a_positional_nor_herdr_workspace_id_exits_64() {
    let td = TestDaemon::start(&[]);
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

/// Binding has no command-line verb; agents bind through `board mcp`.
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

mod session {
    use std::io::Write;
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

    fn is_minutes(text: &str) -> bool {
        text.strip_suffix('m')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    }

    #[test]
    fn a_bound_session_shows_issue_column_run_time_marks_and_pending_requests() {
        let url = super::super::fake_linear(linear_reply);
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
