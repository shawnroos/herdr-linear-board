//! The native `linear.snapshot`, `linear.list` and `linear.issue`: SQLite local
//! state, the read-only GraphQL client against the in-process fake Linear, the
//! grouping engine, and the shared per-space cache with its debounced refetch.

use super::*;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::linear::fake::{FakeLinear, Recorded, Reply};
use crate::linear::{CredentialResolver, LinearClient, LinearConfig, NoKeychain};

const SPACE: &str = "wS";
const SOCKET: &str = "/tmp/hb-u7-none/sessions/alpha/herdr.sock";

fn query(request: &Recorded) -> &str {
    request.body["query"].as_str().unwrap_or("")
}

fn node(identifier: &str, state: (&str, &str, &str), assignee: Option<(&str, &str)>) -> Value {
    json!({
        "id": format!("id-{identifier}"),
        "identifier": identifier,
        "title": format!("title {identifier}"),
        "url": format!("https://linear.app/x/{identifier}"),
        "state": {"id": state.0, "name": state.1, "type": state.2},
        "team": {"id": "team-1", "key": "WEB", "name": "Web"},
        "assignee": assignee.map(|(id, name)| json!({"id": id, "name": name})),
        "priority": 2,
        "labels": {"nodes": [{"id": "l1", "name": "Bug", "parent": null}]},
    })
}

fn issues_reply() -> Reply {
    Reply::ok(json!({"data": {"issues": {
        "nodes": [
            node("WEB-1", ("s-todo", "Todo", "unstarted"), Some(("u1", "Ada"))),
            node("WEB-2", ("s-doing", "In Progress", "started"), None),
        ],
        "pageInfo": {"hasNextPage": false, "endCursor": null}
    }}}))
}

fn project_reply() -> Reply {
    Reply::ok(json!({"data": {"project": {
        "id": "project-1", "name": "Frame Effects", "url": "https://linear.app/p",
        "teams": {"nodes": [{"id": "team-1", "key": "WEB", "name": "Web", "states": {"nodes": [
            {"id": "s-todo", "name": "Todo", "type": "unstarted"},
            {"id": "s-doing", "name": "In Progress", "type": "started"},
            {"id": "s-done", "name": "Done", "type": "completed"}
        ]}}]}
    }}}))
}

fn view_reply() -> Reply {
    Reply::ok(json!({"data": {"customView": {
        "id": "view-1", "name": "Mine", "archivedAt": null,
        "filterData": {"and": [{"project": {"id": {"eq": "project-1"}}}]},
        "viewPreferencesValues": {"layout": "board", "issueGrouping": "assignee",
            "columnOrderBoard": [], "hiddenColumns": [], "hiddenRows": []}
    }}}))
}

fn board_linear(request: &Recorded) -> Reply {
    let q = query(request);
    if q.contains("issues(") {
        issues_reply()
    } else if q.contains("project(id") {
        project_reply()
    } else if q.contains("customView(id") {
        view_reply()
    } else {
        Reply::status(500, json!({"errors": [{"message": "unexpected query"}]}))
    }
}

fn client(fake: &FakeLinear) -> LinearClient {
    LinearClient::new(
        LinearConfig {
            api_url: fake.url(),
            timeout: Duration::from_secs(3),
            rate_limit_retries: 0,
            rate_limit_backoff: Duration::from_millis(10),
            page_cap: 10,
            page_size: 50,
        },
        CredentialResolver::new(Box::new(NoKeychain), Some("lin_api_u7test".into()), None),
    )
    .unwrap()
}

struct Board {
    d: Arc<Daemon>,
    rx: broadcast::Receiver<Event>,
    fake: FakeLinear,
    store: tempfile::TempDir,
}

impl Board {
    fn new(handler: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static) -> Board {
        let (d, rx, _dispatch) = testkit::daemon().events_capacity(64).build_parts();
        let fake = FakeLinear::start(handler);
        d.linear.set_client(client(&fake));
        d.linear
            .set_timing(Duration::from_secs(60), Duration::from_millis(30));
        let store = tempfile::tempdir().unwrap();
        d.linear.set_store_dir(store.path().join("absent"));
        Board { d, rx, fake, store }
    }

    fn bind(&self, space: &str, view: Option<Value>) {
        self.d
            .store
            .lock()
            .set_space_binding(&board_core::db::SpaceBinding {
                herdr_session: "alpha".into(),
                space: space.into(),
                project_id: "project-1".into(),
                display_name: Some("Frame Effects".into()),
                team_ids: vec![],
                view,
            })
            .unwrap();
    }

    fn snapshot_of(&self, space: &str) -> LinearSnapshot {
        let value = handle_request(
            &self.d,
            "linear.snapshot",
            json!({"workspace_id": space, "origin_socket": SOCKET}),
        )
        .unwrap();
        serde_json::from_value(value).unwrap()
    }

    fn snapshot(&self) -> LinearSnapshot {
        self.snapshot_of(SPACE)
    }

    fn count(&self, needle: &str) -> usize {
        self.fake
            .requests()
            .iter()
            .filter(|r| query(r).contains(needle))
            .count()
    }

    fn events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn wait_for_event(&mut self, within: Duration) -> Vec<Event> {
        let until = Instant::now() + within;
        loop {
            let events = self.events();
            if !events.is_empty() || Instant::now() >= until {
                return events;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn report_save_issue(&self) {
        handle_request(
            &self.d,
            "linear.activity.record",
            json!({
                "tool_name": "mcp__linear__save_issue",
                "issue": "WEB-1",
                "claims": {"herdr_socket": SOCKET, "herdr_pane_id": "wS:p1", "herdr_workspace_id": SPACE},
            }),
        )
        .unwrap();
    }
}

fn group_keys(doc: &LinearSnapshot) -> Vec<String> {
    doc.groups.iter().map(|g| g.key.clone()).collect()
}

#[test]
fn a_bound_space_returns_grouped_issues_with_the_bound_record() {
    let board = Board::new(|r, _| board_linear(r));
    board.bind(SPACE, None);

    let doc = board.snapshot();

    assert_eq!(doc.schema, 1);
    assert_eq!(doc.record.state.as_deref(), Some("bound"));
    assert_eq!(doc.record.status, "ok");
    assert_eq!(doc.record.project_id.as_deref(), Some("project-1"));
    assert_eq!(doc.linear.status, "ok");
    assert_eq!(doc.project.name.as_deref(), Some("Frame Effects"));
    assert_eq!(doc.project.team_key.as_deref(), Some("WEB"));
    assert_eq!(
        doc.issues.keys().cloned().collect::<Vec<_>>(),
        ["WEB-1", "WEB-2"]
    );
    assert_eq!(doc.issues["WEB-1"].state.name.as_deref(), Some("Todo"));
    assert_eq!(doc.issues["WEB-1"].labels, ["Bug"]);
    assert!(!doc.issues["WEB-1"].stale);
    assert_eq!(doc.tabs.len(), 1);
    assert_eq!(doc.groups, doc.tabs[0].groups);
    let todo = doc.groups.iter().find(|g| g.key == "s-todo").unwrap();
    assert_eq!(todo.issues, ["WEB-1"]);
    let doing = doc.groups.iter().find(|g| g.key == "s-doing").unwrap();
    assert_eq!(doing.issues, ["WEB-2"]);
    assert_eq!(
        doc.workspace.label, "Frame Effects",
        "label falls back to the binding"
    );
    assert_eq!(doc.herdr.status, "unavailable");

    let issues = board
        .fake
        .requests()
        .into_iter()
        .find(|r| query(r).contains("issues("))
        .unwrap();
    assert_eq!(
        issues.body["variables"]["filter"]["project"]["id"]["eq"],
        "project-1"
    );
}

#[test]
fn a_bound_view_groups_the_board_by_the_views_grouping() {
    let board = Board::new(|r, _| board_linear(r));
    board.bind(SPACE, Some(json!({"id": "view-1", "name": "Mine"})));

    let doc = board.snapshot();

    assert_eq!(doc.view.status, "ok");
    assert_eq!(doc.view.id.as_deref(), Some("view-1"));
    assert_eq!(doc.view.layout.as_ref().unwrap().grouping, "assignee");
    assert_eq!(group_keys(&doc), ["u1", "unassigned"]);
    assert_eq!(board.count("customView(id"), 1, "the view is read once");
    let issues = board
        .fake
        .requests()
        .into_iter()
        .find(|r| query(r).contains("issues("))
        .unwrap();
    assert_eq!(
        issues.body["variables"]["filter"]["and"][0]["project"]["id"]["eq"], "project-1",
        "the issues come through the view's own filter"
    );
}

#[test]
fn a_grouping_config_reads_its_filter_and_groups_by_its_levels() {
    let board = Board::new(|r, _| board_linear(r));
    board.bind(SPACE, None);
    handle_request(
        &board.d,
        "linear.grouping.set",
        json!({"text": r#"{"global": {"levels": {"column": "assignee"}, "filter": {"team": "WEB"}}}"#}),
    )
    .unwrap();

    let doc = board.snapshot();

    assert_eq!(doc.mapping.source, "global");
    assert_eq!(doc.mapping.pane, "assignee");
    assert_eq!(doc.groups.len(), 2);
    assert!(doc.groups.iter().any(|g| g.issues == ["WEB-1"]));
    let issues = board
        .fake
        .requests()
        .into_iter()
        .find(|r| query(r).contains("issues("))
        .unwrap();
    assert!(
        issues.body["variables"]["filter"]["and"].is_array(),
        "the config's compiled filter, not the project filter: {}",
        issues.body["variables"]
    );
}

#[test]
fn an_unbound_space_returns_the_unbound_record_with_no_linear_call() {
    let board = Board::new(|r, _| board_linear(r));
    board.bind("elsewhere", None);

    let doc = board.snapshot();

    assert_eq!(doc.record.state.as_deref(), Some("unbound"));
    assert_eq!(doc.record.status, "missing");
    assert_eq!(doc.linear.status, "unknown");
    assert!(doc.linear.message.is_none());
    assert!(doc.issues.is_empty());
    assert!(board.fake.requests().is_empty(), "Linear was called");
}

#[test]
fn an_empty_board_with_a_store_directory_warns_to_run_the_import() {
    let board = Board::new(|r, _| board_linear(r));
    let store = board.store.path().join("work");
    std::fs::create_dir_all(&store).unwrap();
    board.d.linear.set_store_dir(store);

    let doc = board.snapshot();

    assert_eq!(doc.record.state.as_deref(), Some("unbound"));
    assert_eq!(doc.linear.status, "not_imported");
    let message = doc.linear.message.unwrap();
    assert!(message.contains("board import work-store"), "{message}");
    assert!(board.fake.requests().is_empty());

    board.bind("elsewhere", None);
    let doc = board.snapshot();
    assert_eq!(
        doc.linear.status, "unknown",
        "a board with bindings needs no import"
    );
}

#[test]
fn linear_unreachable_returns_the_last_known_read_with_a_source_warning() {
    let down = Arc::new(AtomicBool::new(false));
    let flag = down.clone();
    let board = Board::new(move |r, _| {
        if flag.load(Ordering::SeqCst) {
            Reply::status(503, json!({"error": "down"}))
        } else {
            board_linear(r)
        }
    });
    board
        .d
        .linear
        .set_timing(Duration::ZERO, Duration::from_millis(30));
    board.bind(SPACE, None);
    assert_eq!(board.snapshot().linear.status, "ok");

    down.store(true, Ordering::SeqCst);
    let doc = board.snapshot();

    assert_eq!(doc.linear.status, "unavailable");
    assert!(doc.linear.message.as_deref().unwrap().contains("HTTP 503"));
    assert!(doc.linear.cache_age_seconds.is_some());
    assert_eq!(
        doc.issues.len(),
        2,
        "the last-known issues stay on the board"
    );
    assert!(doc.issues.values().all(|issue| issue.stale));
    assert_eq!(doc.record.state.as_deref(), Some("bound"));
}

#[test]
fn linear_unreachable_on_the_first_read_is_a_warning_not_an_error() {
    let board = Board::new(|_, _| Reply::status(503, json!({})));
    board.bind(SPACE, None);

    let doc = board.snapshot();

    assert_eq!(doc.linear.status, "unavailable");
    assert!(doc.linear.message.is_some());
    assert!(doc.issues.is_empty());
    assert!(doc.linear.cache_age_seconds.is_none());
}

#[test]
fn two_readers_of_one_space_cause_one_graphql_fetch() {
    let board = Board::new(|r, _| {
        if query(r).contains("issues(") {
            std::thread::sleep(Duration::from_millis(200));
        }
        board_linear(r)
    });
    board.bind(SPACE, None);

    let readers: Vec<_> = (0..2)
        .map(|_| {
            let d = board.d.clone();
            std::thread::spawn(move || {
                handle_request(
                    &d,
                    "linear.snapshot",
                    json!({"workspace_id": SPACE, "origin_socket": SOCKET}),
                )
                .unwrap()
            })
        })
        .collect();
    let docs: Vec<Value> = readers.into_iter().map(|r| r.join().unwrap()).collect();

    assert_eq!(docs[0]["issues"], docs[1]["issues"]);
    assert_eq!(board.count("issues("), 1);
    assert_eq!(board.count("project(id"), 1);

    board.snapshot();
    assert_eq!(
        board.count("issues("),
        1,
        "a later reader shares the cached read"
    );
}

#[test]
fn a_reported_save_issue_causes_one_refetch_then_one_local_state_changed() {
    let mut board = Board::new(|r, _| board_linear(r));
    board.bind(SPACE, None);
    board.snapshot();
    assert_eq!(board.count("issues("), 1);
    board.events();

    board.report_save_issue();
    let events = board.wait_for_event(Duration::from_secs(5));

    assert_eq!(board.count("issues("), 2, "one refetch");
    assert!(
        matches!(events.as_slice(), [Event::LocalStateChanged { space: Some(s), .. }] if s == SPACE),
        "{events:?}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(board.events().is_empty(), "a second announcement");
    assert_eq!(board.count("issues("), 2);
    board.snapshot();
    assert_eq!(board.count("issues("), 2, "readers get the refetched read");
}

#[test]
fn a_burst_of_reports_is_folded_into_one_refetch_and_one_announcement() {
    let mut board = Board::new(|r, _| board_linear(r));
    board
        .d
        .linear
        .set_timing(Duration::from_secs(60), Duration::from_millis(150));
    board.bind(SPACE, None);
    board.snapshot();
    board.events();

    for _ in 0..3 {
        board.report_save_issue();
    }
    let events = board.wait_for_event(Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(300));
    let later = board.events();

    assert_eq!(events.len() + later.len(), 1, "{events:?} {later:?}");
    assert_eq!(board.count("issues("), 2);
}

#[test]
fn a_report_for_a_space_nobody_reads_announces_at_once_and_fetches_nothing() {
    let mut board = Board::new(|r, _| board_linear(r));
    board.bind(SPACE, None);

    board.report_save_issue();

    assert_eq!(board.events().len(), 1);
    std::thread::sleep(Duration::from_millis(100));
    assert!(board.events().is_empty());
    assert!(board.fake.requests().is_empty());
}

#[test]
fn a_card_the_cached_read_lists_is_known_for_a_mark() {
    let board = Board::new(|r, _| board_linear(r));
    board.bind(SPACE, None);
    let mark = json!({"space": SPACE, "issue": "WEB-2"});
    assert_eq!(
        handle_request(&board.d, "linear.mark.set", mark.clone())
            .unwrap_err()
            .code(),
        2
    );

    board.snapshot();

    handle_request(&board.d, "linear.mark.set", mark).unwrap();
}

fn issue_detail_reply() -> Reply {
    let mut issue = node(
        "WEB-7",
        ("s-doing", "In Progress", "started"),
        Some(("u1", "Ada")),
    );
    let detail = json!({
        "description": "why", "dueDate": "2026-10-01", "estimate": 2.5,
        "parent": {"id": "id-WEB-1", "identifier": "WEB-1", "title": "parent", "state": {"id": "s-todo", "name": "Todo", "type": "unstarted"}},
        "projectMilestone": {"id": "m1", "name": "Beta"},
        "cycle": {"id": "c1", "number": 4, "name": null},
        "project": {"id": "project-1", "name": "Frame Effects"},
        "children": {"nodes": [{"id": "id-WEB-8", "identifier": "WEB-8", "title": "child", "state": {"id": "s-todo", "name": "Todo", "type": "unstarted"}}], "pageInfo": {"hasNextPage": false}},
        "relations": {"nodes": [{"id": "r1", "type": "blocks", "relatedIssue": {"id": "id-WEB-9", "identifier": "WEB-9", "title": "blocked", "state": {}}}], "pageInfo": {"hasNextPage": false}},
        "inverseRelations": {"nodes": [{"id": "r2", "type": "related", "issue": {"id": "id-WEB-3", "identifier": "WEB-3", "title": "points here", "state": {}}}], "pageInfo": {"hasNextPage": false}},
        "comments": {"nodes": [{"id": "k1", "body": "hi", "createdAt": "2026-09-01T00:00:00Z", "user": {"id": "u1", "name": "Ada"}, "parent": null}], "pageInfo": {"hasNextPage": true}},
        "history": {"nodes": [
            {"id": "h1", "createdAt": "2026-09-02T00:00:00Z", "actor": {"name": "Ada"}, "fromState": {"name": "Todo"}, "toState": {"name": "In Progress"}, "fromAssignee": null, "toAssignee": null, "fromPriority": null, "toPriority": null, "addedLabels": [], "removedLabels": []},
            {"id": "h2", "createdAt": "2026-09-03T00:00:00Z", "actor": {"name": "Ada"}, "fromState": null, "toState": null, "fromAssignee": null, "toAssignee": null, "fromPriority": null, "toPriority": null, "addedLabels": [], "removedLabels": []}
        ], "pageInfo": {"hasNextPage": false}}
    });
    issue
        .as_object_mut()
        .unwrap()
        .extend(detail.as_object().unwrap().clone());
    Reply::ok(json!({"data": {"issue": issue}}))
}

#[test]
fn linear_issue_makes_exactly_one_request() {
    let board = Board::new(|_, _| issue_detail_reply());

    let value = handle_request(&board.d, "linear.issue", json!({"issue": "WEB-7"})).unwrap();
    let doc: LinearIssueDocument = serde_json::from_value(value).unwrap();

    assert_eq!(board.fake.requests().len(), 1);
    assert_eq!(doc.schema, 1);
    assert_eq!(doc.status, "partial");
    assert_eq!(doc.truncated, ["comments"]);
    let issue = doc.issue.unwrap();
    assert_eq!(issue.identifier, "WEB-7");
    assert_eq!(issue.description.as_deref(), Some("why"));
    assert_eq!(issue.estimate, Some(2.5));
    assert_eq!(issue.milestone.unwrap().name.as_deref(), Some("Beta"));
    assert_eq!(issue.cycle.unwrap().number, Some(4));
    assert_eq!(issue.parent.unwrap().state.name.as_deref(), Some("Todo"));
    assert_eq!(issue.children[0].identifier, "WEB-8");
    let directions: Vec<(&str, &str)> = issue
        .relations
        .iter()
        .map(|r| (r.direction.as_str(), r.issue.identifier.as_str()))
        .collect();
    assert_eq!(directions, [("outward", "WEB-9"), ("inward", "WEB-3")]);
    assert_eq!(issue.comments[0].author.as_deref(), Some("Ada"));
    assert_eq!(
        issue.history.len(),
        1,
        "an event that changed nothing shown is dropped"
    );
    assert_eq!(issue.history[0].to_state.as_deref(), Some("In Progress"));
}

#[test]
fn an_issue_linear_cannot_read_is_an_unavailable_document() {
    let board = Board::new(|_, _| {
        Reply::ok(json!({"errors": [{"message": "x", "extensions": {"code": "NOT_FOUND"}}]}))
    });

    let value = handle_request(&board.d, "linear.issue", json!({"issue": "WEB-404"})).unwrap();
    let doc: LinearIssueDocument = serde_json::from_value(value).unwrap();

    assert_eq!(doc.status, "unavailable");
    assert!(doc.issue.is_none());
    assert!(doc.message.is_some());
    assert_eq!(board.fake.requests().len(), 1);
}

#[test]
fn projects_and_views_list_through_the_native_client() {
    let board = Board::new(|r, _| {
        let q = query(r);
        if q.contains("projects(") {
            Reply::ok(json!({"data": {"projects": {"nodes": [
                {"id": "project-1", "name": "Frame Effects", "teams": {"nodes": [{"id": "t", "key": "WEB", "name": "Web"}]}},
                {"id": "project-2", "name": "Solo", "teams": {"nodes": []}}
            ], "pageInfo": {"hasNextPage": false, "endCursor": null}}}}))
        } else {
            Reply::ok(json!({"data": {"customViews": {"nodes": [
                {"id": "view-1", "name": "Mine", "archivedAt": null, "filterData": {"project": {"id": {"in": ["project-1"]}}}},
                {"id": "view-2", "name": "Old", "archivedAt": "2026-01-01", "filterData": {"project": {"id": {"eq": "project-1"}}}},
                {"id": "view-3", "name": "Other", "archivedAt": null, "filterData": {"project": {"id": {"eq": "project-9"}}}}
            ], "pageInfo": {"hasNextPage": false, "endCursor": null}}}}))
        }
    });

    let projects = handle_request(&board.d, "linear.list", json!({"kind": "projects"})).unwrap();
    assert_eq!(projects["status"], "ok");
    assert_eq!(projects["rows"][0]["team_key"], "WEB");
    assert_eq!(projects["rows"][1]["team_key"], Value::Null);

    let views = handle_request(
        &board.d,
        "linear.list",
        json!({"kind": "views", "id": "project-1"}),
    )
    .unwrap();
    assert_eq!(views["status"], "ok");
    assert_eq!(views["rows"], json!([{"id": "view-1", "name": "Mine"}]));
}

#[test]
fn a_spaces_list_without_an_origin_socket_is_unavailable_not_an_error() {
    let board = Board::new(|r, _| board_linear(r));
    let value = handle_request(&board.d, "linear.list", json!({"kind": "spaces"})).unwrap();
    let list = LinearListResult::from_value(LinearListKind::Spaces, value).unwrap();
    match list {
        LinearListResult::Spaces(list) => {
            assert_eq!(list.status, LinearListStatus::Unavailable);
            assert!(list.message.is_some());
        }
        other => panic!("{other:?}"),
    }
    assert!(board.fake.requests().is_empty());
}

#[test]
fn a_named_plugin_root_keeps_the_script_path() {
    let board = Board::new(|r, _| board_linear(r));
    let missing = board.store.path().join("no-plugin");
    let err = handle_request(
        &board.d,
        "linear.snapshot",
        json!({"workspace_id": SPACE, "plugin_root": missing}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 6, "{err}");
}

#[test]
fn old_snapshot_fixtures_still_parse_against_the_new_dtos() {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../board-core/tests/fixtures/linear-snapshot");
    let mut parsed = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let doc: LinearSnapshot =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc.schema, 1, "{}", path.display());
        assert!(doc.linear.message.is_none());
        parsed += 1;
    }
    assert_eq!(parsed, 9);

    let null_message: LinearSource =
        serde_json::from_value(json!({"status": "ok", "message": null})).unwrap();
    assert!(null_message.message.is_none());
    let unset = serde_json::to_value(LinearSource::default()).unwrap();
    assert!(
        unset.get("message").is_none(),
        "an old reader sees no new key"
    );
}
