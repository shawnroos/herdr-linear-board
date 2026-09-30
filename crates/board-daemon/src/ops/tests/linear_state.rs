//! Linear-mode local state over the socket: bindings, grouping, marks, notes,
//! show-requests and activity, each write returning its before and after and
//! announcing itself once as `local_state_changed`.

use super::*;

use std::path::Path;

const SPACE: &str = "space-1";
const SESSION_SOCKET: &str = "/tmp/hb-ls/sessions/alpha/herdr.sock";

struct Fixture {
    d: Arc<Daemon>,
    rx: broadcast::Receiver<Event>,
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let d = test_daemon(Config::default());
        let rx = d.events_tx.subscribe();
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        Fixture {
            d,
            rx,
            _dir: dir,
            root,
        }
    }

    /// A directory with a `.git` entry, plus a nested `src` directory to run from.
    fn worktree(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::create_dir_all(path.join(".git")).unwrap();
        std::fs::create_dir_all(path.join("src")).unwrap();
        path
    }

    fn bind_space(&self) {
        self.d
            .store
            .lock()
            .set_space_binding(&board_core::db::SpaceBinding {
                herdr_session: "alpha".into(),
                space: SPACE.into(),
                project_id: "project-1".into(),
                display_name: None,
                team_ids: vec![],
                view: None,
            })
            .unwrap();
    }

    fn call(&self, method: &str, params: Value) -> Result<Value> {
        handle_request(&self.d, method, params)
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        self.call(method, params)
            .unwrap_or_else(|e| panic!("{method} failed: {e}"))
    }

    fn events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

fn claims(pane: &str) -> Value {
    json!({
        "herdr_socket": SESSION_SOCKET,
        "herdr_pane_id": pane,
        "herdr_workspace_id": SPACE,
    })
}

fn changed(space: Option<&str>) -> Event {
    Event::LocalStateChanged {
        space: space.map(str::to_owned),
        snapshot: false,
    }
}

fn path_str(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn bind_returns_the_new_binding_and_the_prior_one_and_unbind_restores_the_prior_state() {
    let fx = Fixture::new();
    let wt = fx.worktree("wt-a");
    let initial = fx.ok("linear.state.get", json!({"space": SPACE}));

    let first = fx.ok(
        "linear.bind",
        json!({"cwd": path_str(&wt.join("src")), "issue": "WEB-1", "claims": claims("w1:p1")}),
    );
    assert_eq!(first["before"], Value::Null);
    assert_eq!(first["after"]["worktree_path"], path_str(&wt));
    assert_eq!(first["after"]["issue"], "WEB-1");
    assert_eq!(first["after"]["state"], "bound");

    let second = fx.ok(
        "linear.bind",
        json!({"cwd": path_str(&wt), "issue": "WEB-2", "claims": claims("w1:p1")}),
    );
    assert_eq!(second["before"], first["after"]);
    assert_eq!(second["after"]["issue"], "WEB-2");

    let unbound = fx.ok(
        "linear.unbind",
        json!({"cwd": path_str(&wt), "claims": claims("w1:p1")}),
    );
    assert_eq!(unbound["before"], second["after"]);
    assert_eq!(unbound["after"], Value::Null);

    assert_eq!(fx.ok("linear.state.get", json!({"space": SPACE})), initial);
}

#[test]
fn bind_outside_a_git_worktree_is_refused() {
    let fx = Fixture::new();
    let plain = fx.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let err = fx
        .call(
            "linear.bind",
            json!({"cwd": path_str(&plain), "issue": "WEB-1"}),
        )
        .unwrap_err();
    assert_eq!(err.code(), 1, "{err}");
    assert!(err.to_string().contains("git worktree"), "{err}");
}

#[test]
fn binding_an_issue_bound_to_another_session_is_refused_naming_the_holder() {
    let mut fx = Fixture::new();
    let a = fx.worktree("wt-a");
    let b = fx.worktree("wt-b");
    fx.ok(
        "linear.bind",
        json!({"cwd": path_str(&a), "issue": "WEB-7", "claims": claims("w1:p1")}),
    );
    fx.events();

    let err = fx
        .call(
            "linear.bind",
            json!({"cwd": path_str(&b), "issue": "WEB-7", "claims": claims("w1:p2")}),
        )
        .unwrap_err();
    assert_eq!(err.code(), 3, "{err}");
    assert!(err.to_string().contains(path_str(&a)), "{err}");
    assert!(fx.events().is_empty(), "a refused write announced a change");
    let state = fx.ok("linear.state.get", json!({"space": SPACE}));
    assert_eq!(state["worktree_bindings"].as_array().unwrap().len(), 1);
}

#[test]
fn a_mark_on_an_unknown_card_is_refused() {
    let mut fx = Fixture::new();
    let err = fx
        .call(
            "linear.mark.set",
            json!({"space": SPACE, "issue": "WEB-404", "text": "look"}),
        )
        .unwrap_err();
    assert_eq!(err.code(), 2, "{err}");
    assert!(err.to_string().contains("WEB-404"), "{err}");
    assert!(fx.events().is_empty());

    fx.ok(
        "linear.activity.record",
        json!({"tool_name": "mcp__linear__save_comment", "issue": "WEB-404", "space": SPACE}),
    );
    fx.events();
    let set = fx.ok(
        "linear.mark.set",
        json!({"space": SPACE, "issue": "WEB-404", "text": "look"}),
    );
    assert_eq!(set["before"], json!([]));
    assert_eq!(set["after"]["kind"], "attention");
}

#[test]
fn a_mark_naming_a_malformed_issue_identifier_is_refused() {
    let mut fx = Fixture::new();
    for bad in ["web-1", "WEB-01", "WEB-1\u{1b}", "WEB 1", ""] {
        let err = fx
            .call(
                "linear.mark.set",
                json!({"space": SPACE, "issue": bad, "text": "x"}),
            )
            .unwrap_err();
        assert_eq!(err.code(), 1, "{bad:?}: {err}");
    }
    assert!(fx.events().is_empty());
    let state = fx.ok("linear.state.get", json!({"space": SPACE}));
    assert_eq!(state["marks"], json!([]));
}

#[test]
fn mark_set_replaces_the_mark_of_the_same_kind_and_clear_returns_it() {
    let fx = Fixture::new();
    let wt = fx.worktree("wt-a");
    fx.ok(
        "linear.bind",
        json!({"cwd": path_str(&wt), "issue": "WEB-3"}),
    );
    let first = fx.ok(
        "linear.mark.set",
        json!({"space": SPACE, "issue": "WEB-3", "text": "one"}),
    );
    let second = fx.ok(
        "linear.mark.set",
        json!({"space": SPACE, "issue": "WEB-3", "text": "two"}),
    );
    assert_eq!(second["before"], json!([first["after"].clone()]));
    let id = second["after"]["id"].as_i64().unwrap();
    let cleared = fx.ok("linear.mark.clear", json!({"id": id}));
    assert_eq!(cleared["before"], second["after"]);
    assert_eq!(cleared["after"], Value::Null);
    let err = fx.call("linear.mark.clear", json!({"id": id})).unwrap_err();
    assert_eq!(err.code(), 2, "{err}");
}

#[test]
fn a_note_containing_an_escape_sequence_is_stored_without_it() {
    let fx = Fixture::new();
    let wt = fx.worktree("wt-a");
    fx.ok(
        "linear.bind",
        json!({"cwd": path_str(&wt), "issue": "WEB-5"}),
    );
    let set = fx.ok(
        "linear.note.set",
        json!({
            "space": SPACE,
            "issue": "WEB-5",
            "body": "red\u{1b}[31m text\u{202E}\nline two\u{7}",
            "author": "agent\u{1b}]0;x\u{7}",
        }),
    );
    assert_eq!(set["after"]["body"], "red[31m text\nline two");
    assert_eq!(set["after"]["author"], "agent]0;x");
    let state = fx.ok("linear.state.get", json!({"space": SPACE}));
    let stored = state["notes"][0]["body"].as_str().unwrap();
    assert!(!stored.contains('\u{1b}'), "{stored:?}");

    let replaced = fx.ok(
        "linear.note.set",
        json!({"space": SPACE, "issue": "WEB-5", "body": "second", "author": "agent]0;x"}),
    );
    assert_eq!(replaced["before"], json!([set["after"].clone()]));
    let cleared = fx.ok(
        "linear.note.clear",
        json!({"id": replaced["after"]["id"].clone()}),
    );
    assert_eq!(cleared["before"], replaced["after"]);
    assert_eq!(cleared["after"], Value::Null);
}

#[test]
fn show_accept_on_a_dismissed_request_is_refused() {
    let mut fx = Fixture::new();
    let wt = fx.worktree("wt-a");
    fx.ok(
        "linear.bind",
        json!({"cwd": path_str(&wt), "issue": "WEB-8"}),
    );
    let requested = fx.ok(
        "linear.show.request",
        json!({"space": SPACE, "issue": "WEB-8", "reason": "please\u{1b}[2J look"}),
    );
    assert_eq!(requested["before"], Value::Null);
    assert_eq!(requested["after"]["reason"], "please[2J look");
    let id = requested["after"]["id"].as_i64().unwrap();

    let dismissed = fx.ok("linear.show.dismiss", json!({"id": id}));
    assert_eq!(dismissed["before"], requested["after"]);
    assert!(dismissed["after"]["acknowledged_at"].is_string());
    fx.events();

    let err = fx
        .call("linear.show.accept", json!({"id": id}))
        .unwrap_err();
    assert_eq!(err.code(), 3, "{err}");
    assert!(fx.events().is_empty());

    let err = fx
        .call("linear.show.accept", json!({"id": id + 100}))
        .unwrap_err();
    assert_eq!(err.code(), 2, "{err}");
}

#[test]
fn show_accept_returns_the_request_it_drained() {
    let fx = Fixture::new();
    let wt = fx.worktree("wt-a");
    fx.ok(
        "linear.bind",
        json!({"cwd": path_str(&wt), "issue": "WEB-8"}),
    );
    let requested = fx.ok(
        "linear.show.request",
        json!({"space": SPACE, "issue": "WEB-8"}),
    );
    let accepted = fx.ok(
        "linear.show.accept",
        json!({"id": requested["after"]["id"].clone()}),
    );
    assert_eq!(accepted["after"]["issue"], "WEB-8");
    assert!(accepted["after"]["acknowledged_at"].is_string());
    let state = fx.ok("linear.state.get", json!({"space": SPACE}));
    assert_eq!(state["show_requests"], json!([]));
}

fn expect_one(fx: &mut Fixture, method: &str, params: Value, space: Option<&str>) -> Value {
    let result = fx.ok(method, params);
    assert_eq!(fx.events(), vec![changed(space)], "{method}");
    result
}

#[test]
fn each_write_emits_exactly_one_local_state_changed_for_its_space() {
    let mut fx = Fixture::new();
    fx.bind_space();
    let wt = fx.worktree("wt-a");
    let space = Some(SPACE);

    expect_one(
        &mut fx,
        "linear.bind",
        json!({"cwd": path_str(&wt), "issue": "WEB-1", "claims": claims("w1:p1")}),
        space,
    );
    let mark = expect_one(
        &mut fx,
        "linear.mark.set",
        json!({"space": SPACE, "issue": "WEB-1"}),
        space,
    );
    expect_one(
        &mut fx,
        "linear.mark.clear",
        json!({"id": mark["after"]["id"].clone()}),
        space,
    );
    let note = expect_one(
        &mut fx,
        "linear.note.set",
        json!({"space": SPACE, "issue": "WEB-1", "body": "b", "author": "a"}),
        space,
    );
    expect_one(
        &mut fx,
        "linear.note.clear",
        json!({"id": note["after"]["id"].clone()}),
        space,
    );
    let show = expect_one(
        &mut fx,
        "linear.show.request",
        json!({"space": SPACE, "issue": "WEB-1"}),
        space,
    );
    expect_one(
        &mut fx,
        "linear.show.accept",
        json!({"id": show["after"]["id"].clone()}),
        space,
    );
    let show = expect_one(
        &mut fx,
        "linear.show.request",
        json!({"space": SPACE, "issue": "WEB-1"}),
        space,
    );
    expect_one(
        &mut fx,
        "linear.show.dismiss",
        json!({"id": show["after"]["id"].clone()}),
        space,
    );
    expect_one(
        &mut fx,
        "linear.activity.record",
        json!({"tool_name": "mcp__linear__save_comment", "issue": "WEB-1", "claims": claims("w1:p1")}),
        space,
    );
    expect_one(
        &mut fx,
        "linear.grouping.set",
        json!({"text": r#"{"global": {"levels": {"column": "state"}, "filter": {"team": "ENG"}}}"#}),
        None,
    );
    expect_one(
        &mut fx,
        "linear.grouping.set",
        json!({"space": SPACE, "text": r#"{"levels": {"column": "assignee"}, "filter": {"team": "ENG"}}"#}),
        None,
    );
    expect_one(
        &mut fx,
        "linear.unbind",
        json!({"cwd": path_str(&wt), "claims": claims("w1:p1")}),
        space,
    );

    fx.ok("linear.state.get", json!({"space": SPACE}));
    fx.ok("linear.grouping.get", json!({"space": SPACE}));
    fx.ok("linear.activity.list", json!({"space": SPACE}));
    fx.ok(
        "linear.grouping.preview",
        json!({"text": r#"{"global": {"levels": {"row": "cycle"}, "filter": {"team": "ENG"}}}"#}),
    );
    assert!(
        fx.events().is_empty(),
        "a read or a preview announced a change"
    );
}

#[test]
fn grouping_set_refuses_an_unknown_key_anywhere_in_a_whole_config() {
    let fx = Fixture::new();
    for text in [
        r#"{"global": {"levels": {"column": "state"}, "filter": {"team": "ENG"}}, "extra": 1}"#,
        r#"{"global": {"levels": {"column": "state"}, "filter": {"team": "ENG"}}, "spaces": [{"space": "s", "mapping": {"levels": {"row": "team"}, "filter": {"team": "ENG"}}, "position": 0}]}"#,
    ] {
        let err = fx
            .call("linear.grouping.set", json!({"text": text}))
            .unwrap_err();
        assert_eq!(err.code(), 1, "{text}: {err}");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }
    assert_eq!(
        fx.ok("linear.grouping.get", json!({}))["config"],
        Value::Null
    );
}

#[test]
fn grouping_set_refuses_duplicate_keys_in_its_raw_text() {
    let mut fx = Fixture::new();
    for text in [
        r#"{"global": {"levels": {"column": "state", "column": "team"}, "filter": {"team": "ENG"}}}"#,
        r#"{"global": {"levels": {"column": "state"}, "filter": {"team": "ENG", "team": "WEB"}}}"#,
        r#"{"global": {"levels": {"column": "state"}, "filter": {"team": "ENG"}}, "global": {"levels": {"row": "team"}, "filter": {"team": "X"}}}"#,
    ] {
        for method in ["linear.grouping.set", "linear.grouping.preview"] {
            let err = fx.call(method, json!({"text": text})).unwrap_err();
            assert_eq!(err.code(), 1, "{method} {text}: {err}");
            assert!(err.to_string().contains("duplicate key"), "{err}");
        }
    }
    assert!(fx.events().is_empty());
    let got = fx.ok("linear.grouping.get", json!({}));
    assert_eq!(got["config"], Value::Null);
}

#[test]
fn grouping_set_returns_the_config_before_and_after_and_preview_writes_nothing() {
    let fx = Fixture::new();
    let global = r#"{"global": {"levels": {"column": "state"}, "filter": {"team": "ENG"}}}"#;

    let preview = fx.ok("linear.grouping.preview", json!({"text": global}));
    assert_eq!(preview["before"], Value::Null);
    assert_eq!(preview["after"]["global"]["levels"]["column"], "state");
    assert_eq!(
        fx.ok("linear.grouping.get", json!({}))["config"],
        Value::Null
    );

    let set = fx.ok("linear.grouping.set", json!({"text": global}));
    assert_eq!(set, preview);

    let space = fx.ok(
        "linear.grouping.set",
        json!({"space": SPACE, "text": r#"{"levels": {"tab": "project"}, "filter": {"team": "ENG"}}"#}),
    );
    assert_eq!(space["before"], set["after"]);
    assert_eq!(space["after"]["spaces"][0]["space"], SPACE);

    let resolved = fx.ok("linear.grouping.get", json!({"space": SPACE}));
    assert_eq!(resolved["resolved"]["space"], SPACE);
    assert_eq!(resolved["resolved"]["mapping"]["levels"]["tab"], "project");

    let removed = fx.ok("linear.grouping.set", json!({"space": SPACE, "text": null}));
    assert_eq!(removed["before"], space["after"]);
    assert_eq!(removed["after"], set["after"]);

    let err = fx
        .call(
            "linear.grouping.set",
            json!({"text": r#"{"global": {"levels": {"column": "state", "row": "state"}, "filter": {"team": "ENG"}}}"#}),
        )
        .unwrap_err();
    assert_eq!(err.code(), 1, "{err}");
    assert!(err.to_string().contains("same kind"), "{err}");
}

// R13: a reported save_issue links only an unbound, known session.

#[test]
fn a_save_issue_report_links_a_known_unbound_session() {
    let mut fx = Fixture::new();
    fx.bind_space();
    let wt = fx.worktree("wt-a");

    let result = fx.ok(
        "linear.activity.record",
        json!({
            "tool_name": "mcp__linear__save_issue",
            "issue": "WEB-11",
            "cwd": path_str(&wt.join("src")),
            "claims": claims("w1:p1"),
        }),
    );
    assert_eq!(result["outcome"], "linked");
    assert_eq!(result["binding"]["before"], Value::Null);
    assert_eq!(result["binding"]["after"]["worktree_path"], path_str(&wt));
    assert_eq!(result["binding"]["after"]["issue"], "WEB-11");
    assert_eq!(result["mark"], Value::Null);
    assert_eq!(result["activity"]["issue"], "WEB-11");
    assert_eq!(result["activity"]["claims"]["herdr_pane_id"], "w1:p1");
    assert_eq!(fx.events(), vec![changed(Some(SPACE))]);

    let listed = fx.ok("linear.activity.list", json!({"space": SPACE}));
    assert_eq!(
        listed["activity"][0]["tool_name"],
        "mcp__linear__save_issue"
    );
}

#[test]
fn a_save_issue_report_from_a_bound_session_becomes_a_suggestion() {
    let fx = Fixture::new();
    fx.bind_space();
    let wt = fx.worktree("wt-a");
    let bound = fx.ok(
        "linear.bind",
        json!({"cwd": path_str(&wt), "issue": "WEB-1", "claims": claims("w1:p1")}),
    );

    let result = fx.ok(
        "linear.activity.record",
        json!({
            "tool_name": "mcp__linear__save_issue",
            "issue": "WEB-12",
            "cwd": path_str(&wt),
            "claims": claims("w1:p1"),
        }),
    );
    assert_eq!(result["outcome"], "suggested");
    assert_eq!(result["binding"], Value::Null);
    let mark = &result["mark"]["after"];
    assert_eq!(mark["kind"], "suggestion");
    assert_eq!(mark["issue"], "WEB-12");
    assert_eq!(mark["space"], SPACE);
    assert_eq!(mark["detail"]["worktree_path"], path_str(&wt));
    assert_eq!(mark["detail"]["herdr_pane_id"], "w1:p1");

    let state = fx.ok("linear.state.get", json!({"space": SPACE}));
    assert_eq!(state["worktree_bindings"], json!([bound["after"].clone()]));
}

#[test]
fn a_save_issue_report_from_an_unknown_session_becomes_a_suggestion() {
    let fx = Fixture::new();
    let wt = fx.worktree("wt-a");
    // No space binding for session `alpha`: the pane is not in a known space.
    let result = fx.ok(
        "linear.activity.record",
        json!({
            "tool_name": "mcp__linear__save_issue",
            "issue": "WEB-13",
            "cwd": path_str(&wt),
            "claims": claims("w1:p1"),
        }),
    );
    assert_eq!(result["outcome"], "suggested");
    assert_eq!(
        fx.ok("linear.state.get", json!({"space": SPACE}))["worktree_bindings"],
        json!([])
    );
}

#[test]
fn a_save_issue_report_without_pane_claims_becomes_a_suggestion() {
    let fx = Fixture::new();
    fx.bind_space();
    let wt = fx.worktree("wt-a");
    let result = fx.ok(
        "linear.activity.record",
        json!({
            "tool_name": "mcp__linear__save_issue",
            "issue": "WEB-14",
            "space": SPACE,
            "cwd": path_str(&wt),
        }),
    );
    assert_eq!(result["outcome"], "suggested");
}

#[test]
fn a_save_issue_report_for_an_issue_bound_elsewhere_becomes_a_suggestion() {
    let fx = Fixture::new();
    fx.bind_space();
    let other = fx.worktree("wt-other");
    let wt = fx.worktree("wt-a");
    fx.ok(
        "linear.bind",
        json!({"cwd": path_str(&other), "issue": "WEB-15"}),
    );
    let result = fx.ok(
        "linear.activity.record",
        json!({
            "tool_name": "mcp__linear__save_issue",
            "issue": "WEB-15",
            "cwd": path_str(&wt),
            "claims": claims("w1:p2"),
        }),
    );
    assert_eq!(result["outcome"], "suggested");
}

#[test]
fn other_linear_writes_record_activity_only() {
    let mut fx = Fixture::new();
    fx.bind_space();
    let wt = fx.worktree("wt-a");
    let result = fx.ok(
        "linear.activity.record",
        json!({
            "tool_name": "mcp__linear__save_comment",
            "issue": "WEB-16",
            "cwd": path_str(&wt),
            "claims": claims("w1:p1"),
        }),
    );
    assert_eq!(result["outcome"], "recorded");
    assert_eq!(result["binding"], Value::Null);
    assert_eq!(result["mark"], Value::Null);
    assert_eq!(fx.events(), vec![changed(Some(SPACE))]);
    let state = fx.ok("linear.state.get", json!({"space": SPACE}));
    assert_eq!(state["worktree_bindings"], json!([]));
    assert_eq!(state["marks"], json!([]));
}

#[test]
fn activity_record_refuses_a_malformed_identifier_and_cleans_its_claims() {
    let mut fx = Fixture::new();
    let err = fx
        .call(
            "linear.activity.record",
            json!({"tool_name": "mcp__linear__save_issue", "issue": "WEB-1\u{1b}[0m", "space": SPACE}),
        )
        .unwrap_err();
    assert_eq!(err.code(), 1, "{err}");
    assert!(fx.events().is_empty());

    let result = fx.ok(
        "linear.activity.record",
        json!({
            "tool_name": "mcp__linear__save_comment",
            "space": SPACE,
            "claims": {"herdr_pane_id": "w1:p1\u{1b}[31m", "herdr_workspace_id": SPACE},
        }),
    );
    assert_eq!(result["activity"]["claims"]["herdr_pane_id"], "w1:p1[31m");
}

#[test]
fn state_get_lists_the_space_marks_notes_and_pending_show_requests() {
    let fx = Fixture::new();
    fx.bind_space();
    let wt = fx.worktree("wt-a");
    fx.ok(
        "linear.bind",
        json!({"cwd": path_str(&wt), "issue": "WEB-1"}),
    );
    fx.ok(
        "linear.mark.set",
        json!({"space": SPACE, "issue": "WEB-1", "text": "needs you"}),
    );
    fx.ok(
        "linear.note.set",
        json!({"space": SPACE, "issue": "WEB-1", "body": "b", "author": "a"}),
    );
    fx.ok(
        "linear.show.request",
        json!({"space": SPACE, "issue": "WEB-1"}),
    );
    let state = fx.ok("linear.state.get", json!({"space": SPACE}));
    assert_eq!(state["space"], SPACE);
    assert_eq!(state["space_bindings"][0]["herdr_session"], "alpha");
    assert_eq!(state["marks"][0]["text"], "needs you");
    assert_eq!(state["notes"][0]["body"], "b");
    assert_eq!(state["show_requests"][0]["issue"], "WEB-1");
    assert_eq!(state["worktree_bindings"][0]["issue"], "WEB-1");
}
