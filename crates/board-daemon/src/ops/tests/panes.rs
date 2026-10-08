//! `pane.set_title` — the RPC the TUI plugin pane uses instead of shelling out
//! to `herdr pane rename` itself.

use super::*;
use std::path::Path;

/// A fake Herdr that answers `pane.rename` with the renamed pane, so the test
/// can read back the exact params the daemon sent.
fn renaming_herdr() -> FakeHerdr {
    testkit::herdr_server()
        .on("pane.rename", |req| {
            let pane_id = req["params"]["pane_id"].as_str().unwrap();
            let mut pane = testkit::pane_info(pane_id);
            pane["label"] = req["params"]["label"].clone();
            testkit::reply(req, json!({"type": "pane_info", "pane": pane}))
        })
        .serve()
}

#[test]
fn pane_set_title_renames_the_caller_pane_through_herdr() {
    let herdr = renaming_herdr();
    // No session registry on purpose: the pane belongs to the *caller's* Herdr,
    // named by `origin_socket`, so this must not depend on session enumeration.
    let d = test_daemon(Config::default());

    let v = handle_request(
        &d,
        "pane.set_title",
        json!({
            "pane_id": "w1:p9",
            "title": "Board [herdr-board · ACTIVE]",
            "origin_socket": herdr.socket,
        }),
    )
    .unwrap();

    assert_eq!(v, json!({"renamed": true}));
    // The protocol gate runs first, then exactly one rename — nothing else.
    assert_eq!(herdr.methods(), vec!["ping", "pane.rename"]);
    let sent = herdr.requests_for("pane.rename");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["params"]["pane_id"], "w1:p9");
    assert_eq!(sent[0]["params"]["label"], "Board [herdr-board · ACTIVE]");
}

#[test]
fn pane_set_title_rejects_a_socket_with_the_wrong_protocol() {
    let herdr = testkit::herdr_server()
        .protocol(board_herdr::SUPPORTED_HERDR_PROTOCOL - 1)
        .serve();
    let d = test_daemon(Config::default());

    let err = handle_request(
        &d,
        "pane.set_title",
        json!({"pane_id": "w1:p9", "title": "Board", "origin_socket": herdr.socket}),
    )
    .unwrap_err();

    assert_eq!(err.code(), 4);
    let msg = err.to_string();
    assert!(
        msg.contains(&format!(
            "Herdr {}.x with protocol {} is required",
            board_herdr::SUPPORTED_HERDR_SERIES,
            board_herdr::SUPPORTED_HERDR_PROTOCOL
        )),
        "message: {msg}"
    );
    // The gate is the first and only request: no rename reaches a socket the
    // daemon has not verified.
    assert_eq!(herdr.methods(), vec!["ping"]);
}

#[test]
fn pane_set_title_reports_an_unreachable_socket_as_herdr_unavailable() {
    let d = test_daemon(Config::default());
    let err = handle_request(
        &d,
        "pane.set_title",
        json!({"pane_id": "w1:p9", "title": "Board", "origin_socket": "/tmp/no-such-herdr.sock"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 4);
    assert!(
        err.to_string().contains("origin Herdr socket"),
        "message: {err}"
    );
}

#[test]
fn pane_set_title_rejects_an_empty_pane_id_before_touching_herdr() {
    let herdr = renaming_herdr();
    let d = test_daemon(Config::default());
    let err = handle_request(
        &d,
        "pane.set_title",
        json!({"pane_id": "  ", "title": "Board", "origin_socket": herdr.socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    assert!(herdr.methods().is_empty(), "herdr was contacted");
}

#[test]
fn pane_set_title_surfaces_a_herdr_refusal_rather_than_claiming_success() {
    let herdr = testkit::herdr_server()
        .on("pane.rename", |req| {
            testkit::error(req, "pane_not_found", "pane not found")
        })
        .serve();
    let d = test_daemon(Config::default());

    let err = handle_request(
        &d,
        "pane.set_title",
        json!({"pane_id": "w1:gone", "title": "Board", "origin_socket": herdr.socket}),
    )
    .unwrap_err();

    assert_eq!(err.code(), 4);
    let msg = err.to_string();
    assert!(msg.contains("pane.rename w1:gone"), "message: {msg}");
}

#[test]
fn pane_set_title_strips_control_and_format_characters_at_the_sink() {
    let herdr = renaming_herdr();
    let d = test_daemon(Config::default());

    // A client that never ran the TUI's own strip.
    let v = handle_request(
        &d,
        "pane.set_title",
        json!({
            "pane_id": "w1:p9",
            "title": "Linear: \u{1b}[31mEx\u{202E}ample\nLaunch\u{200B}\u{7}",
            "origin_socket": herdr.socket,
        }),
    )
    .unwrap();

    assert_eq!(v, json!({"renamed": true}));
    let sent = herdr.requests_for("pane.rename");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["params"]["label"], "Linear: [31mExampleLaunch");
}

/// A fake Herdr that lists exactly `live` panes for `pane.get` and records
/// every `pane.focus`.
fn focusing_herdr(live: &'static [&'static str]) -> FakeHerdr {
    testkit::herdr_server()
        .on("pane.get", move |req| {
            let pane_id = req["params"]["pane_id"].as_str().unwrap();
            if live.contains(&pane_id) {
                testkit::reply(
                    req,
                    json!({"type": "pane_info", "pane": testkit::pane_info(pane_id)}),
                )
            } else {
                testkit::error(req, "pane_not_found", "pane not found")
            }
        })
        .on("pane.focus", |req| {
            let pane_id = req["params"]["pane_id"].as_str().unwrap();
            let mut pane = testkit::pane_info(pane_id);
            pane["focused"] = json!(true);
            testkit::reply(req, json!({"type": "pane_info", "pane": pane}))
        })
        .serve()
}

#[test]
fn pane_focus_focuses_a_live_pane_exactly_once() {
    let herdr = focusing_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());

    let v = handle_request(
        &d,
        "pane.focus",
        json!({"pane_id": "wA:p1", "origin_socket": herdr.socket}),
    )
    .unwrap();

    assert_eq!(v, json!({"focused": true, "gone": false}));
    assert_eq!(herdr.methods(), vec!["ping", "pane.get", "pane.focus"]);
    let sent = herdr.requests_for("pane.focus");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["params"]["pane_id"], "wA:p1");
}

#[test]
fn pane_focus_reports_a_pane_the_session_does_not_list_as_gone_without_focusing() {
    let herdr = focusing_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());

    let v = handle_request(
        &d,
        "pane.focus",
        json!({"pane_id": "wB:p7", "origin_socket": herdr.socket}),
    )
    .unwrap();

    assert_eq!(v, json!({"focused": false, "gone": true}));
    assert_eq!(herdr.count("pane.focus"), 0);
    assert_eq!(herdr.methods(), vec!["ping", "pane.get"]);
}

#[test]
fn pane_focus_rejects_an_empty_pane_id_before_touching_herdr() {
    let herdr = focusing_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());
    let err = handle_request(
        &d,
        "pane.focus",
        json!({"pane_id": "", "origin_socket": herdr.socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    assert!(herdr.methods().is_empty(), "herdr was contacted");
}

#[test]
fn pane_focus_reports_an_unreachable_socket_as_herdr_unavailable() {
    let d = test_daemon(Config::default());
    let err = handle_request(
        &d,
        "pane.focus",
        json!({"pane_id": "wA:p1", "origin_socket": "/tmp/no-such-herdr.sock"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 4);
}

/// A fake Herdr for the board-pane ops. `pane.get` answers for every pane in
/// `live` (its workspace is the id's prefix, so `wA:p1` lives in `wA`);
/// `plugin.pane.open` mints `wA:p10`, `wA:p11`, … and makes each one live;
/// `plugin.pane.close` removes the pane from `live`.
fn board_pane_herdr(live: &[&str]) -> (FakeHerdr, Arc<Mutex<Vec<String>>>) {
    let live = Arc::new(Mutex::new(
        live.iter().map(|p| (*p).to_string()).collect::<Vec<_>>(),
    ));
    let opened = Arc::new(std::sync::atomic::AtomicUsize::new(10));
    let (get_live, open_live, close_live) = (live.clone(), live.clone(), live.clone());
    let herdr = testkit::herdr_server()
        .on("pane.get", move |req| {
            let pane_id = req["params"]["pane_id"].as_str().unwrap();
            if get_live.lock().unwrap().iter().any(|p| p == pane_id) {
                let mut pane = testkit::pane_info(pane_id);
                pane["workspace_id"] = json!(pane_id.split(':').next().unwrap());
                testkit::reply(req, json!({"type": "pane_info", "pane": pane}))
            } else {
                testkit::error(req, "pane_not_found", "pane not found")
            }
        })
        .on("plugin.pane.open", move |req| {
            let n = opened.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let pane_id = format!("wA:p{n}");
            open_live.lock().unwrap().push(pane_id.clone());
            let mut pane = testkit::pane_info(&pane_id);
            pane["workspace_id"] = json!("wA");
            pane["tab_id"] = json!("wA:t2");
            testkit::reply(
                req,
                json!({"type": "plugin_pane_opened", "plugin_pane": {
                    "plugin_id": req["params"]["plugin_id"].clone(),
                    "entrypoint": req["params"]["entrypoint"].clone(),
                    "pane": pane
                }}),
            )
        })
        .on("plugin.pane.close", move |req| {
            let pane_id = req["params"]["pane_id"].as_str().unwrap().to_string();
            close_live.lock().unwrap().retain(|p| *p != pane_id);
            testkit::reply(
                req,
                json!({"type": "plugin_pane_closed", "pane_id": pane_id}),
            )
        })
        .on("notification.show", |req| {
            testkit::reply(
                req,
                json!({"type": "notification_shown", "shown": true, "reason": ""}),
            )
        })
        .serve();
    (herdr, live)
}

fn canonical(herdr: &FakeHerdr) -> String {
    herdr
        .socket
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn open_board(d: &Arc<Daemon>, herdr: &FakeHerdr, placement: &str) -> Result<Value> {
    handle_request(
        d,
        "board.pane.open",
        json!({
            "context": {"issue": "ENG-123", "space": "wA"},
            "placement": placement,
            "origin_socket": herdr.socket,
            "origin_pane": "wA:p1",
        }),
    )
}

fn seed_board_pane(d: &Arc<Daemon>, socket: &str, pane_id: &str, context_key: &str) {
    d.store
        .lock()
        .record_board_pane(&board_core::db::NewBoardPane {
            herdr_socket: socket,
            pane_id,
            context_key,
            placement: board_core::db::BoardPanePlacement::Split,
            workspace_id: Some("wA"),
            origin_pane_id: Some("wA:p1"),
        })
        .unwrap();
}

#[test]
fn board_pane_open_refuses_covering_placements_before_any_herdr_call() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());
    for placement in ["overlay", "popup", "zoomed"] {
        let err = open_board(&d, &herdr, placement).unwrap_err();
        assert_eq!(err.code(), 1, "{placement}");
        assert!(
            err.to_string().contains("tab or split"),
            "{placement}: {err}"
        );
    }
    assert!(herdr.methods().is_empty(), "herdr was contacted");
}

#[test]
fn board_pane_open_split_targets_the_origin_pane_without_focus_and_records_the_pane() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());

    let v = open_board(&d, &herdr, "split").unwrap();

    assert_eq!(
        v,
        json!({"pane_id": "wA:p10", "tab_id": "wA:t2", "workspace_id": "wA",
               "placement": "split", "reused": false})
    );
    assert_eq!(
        herdr.methods(),
        vec!["ping", "pane.get", "plugin.pane.open"]
    );
    let sent = herdr.requests_for("plugin.pane.open");
    assert_eq!(sent.len(), 1);
    let params = &sent[0]["params"];
    assert_eq!(params["plugin_id"], "herdr-board");
    assert_eq!(params["entrypoint"], "board");
    assert_eq!(params["placement"], "split");
    assert_eq!(params["focus"], json!(false));
    assert_eq!(params["target_pane_id"], "wA:p1");
    assert!(params.get("workspace_id").is_none(), "params: {params}");

    let row = d
        .store
        .lock()
        .board_pane_for_context(&canonical(&herdr), "space=wA;issue=ENG-123")
        .unwrap()
        .expect("recorded pane");
    assert_eq!(row.pane_id, "wA:p10");
    assert_eq!(row.origin_pane_id.as_deref(), Some("wA:p1"));
}

#[test]
fn board_pane_open_tab_sends_the_callers_workspace_and_focus_false() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());

    open_board(&d, &herdr, "tab").unwrap();

    let sent = herdr.requests_for("plugin.pane.open");
    assert_eq!(sent.len(), 1);
    let params = &sent[0]["params"];
    assert_eq!(params["placement"], "tab");
    assert_eq!(params["workspace_id"], "wA");
    assert_eq!(params["focus"], json!(false));
    assert!(params.get("target_pane_id").is_none(), "params: {params}");
}

#[test]
fn board_pane_open_env_is_exactly_the_closed_set() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());

    handle_request(
        &d,
        "board.pane.open",
        json!({
            "context": {"issue": "ENG-123", "space": "wA", "card": 7},
            "placement": "split",
            "origin_socket": herdr.socket,
            "origin_pane": "wA:p1",
        }),
    )
    .unwrap();

    let sent = herdr.requests_for("plugin.pane.open");
    assert_eq!(
        sent[0]["params"]["env"],
        json!({
            "BOARD_SOCKET": "/tmp/board-test.sock",
            "BOARD_DB": "/tmp/board-test.db",
            "BOARD_SHOW_SPACE": "wA",
            "BOARD_SHOW_ISSUE": "ENG-123",
            "BOARD_SHOW_CARD": "7",
        })
    );
}

#[test]
fn board_pane_open_refuses_a_malformed_or_empty_context_before_any_herdr_call() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());
    for context in [
        json!({}),
        json!({"issue": "ENG-1\u{1b}[31m"}),
        json!({"issue": "not an issue"}),
        json!({"space": "wA\nBOARD_DB=/etc/passwd"}),
        json!({"card": 0}),
    ] {
        let err = handle_request(
            &d,
            "board.pane.open",
            json!({
                "context": context,
                "placement": "split",
                "origin_socket": herdr.socket,
                "origin_pane": "wA:p1",
            }),
        )
        .unwrap_err();
        assert_eq!(err.code(), 1, "{context}");
        assert!(err.to_string().contains("context"), "{context}: {err}");
    }
    assert!(herdr.methods().is_empty(), "herdr was contacted");
}

#[test]
fn board_pane_open_refuses_an_origin_pane_the_session_does_not_list() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());

    let err = handle_request(
        &d,
        "board.pane.open",
        json!({
            "context": {"issue": "ENG-123"},
            "placement": "split",
            "origin_socket": herdr.socket,
            "origin_pane": "wB:p7",
        }),
    )
    .unwrap_err();

    assert_eq!(err.code(), 2);
    assert!(err.to_string().contains("origin pane wB:p7"), "{err}");
    assert_eq!(herdr.methods(), vec!["ping", "pane.get"]);
}

#[test]
fn board_pane_open_reuses_the_recorded_pane_after_pane_get_confirms_it() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());

    open_board(&d, &herdr, "split").unwrap();
    let again = open_board(&d, &herdr, "tab").unwrap();

    assert_eq!(again["pane_id"], "wA:p10");
    assert_eq!(again["reused"], json!(true));
    assert_eq!(again["placement"], "split");
    assert_eq!(herdr.count("plugin.pane.open"), 1);
    let gets: Vec<Value> = herdr
        .requests_for("pane.get")
        .into_iter()
        .map(|r| r["params"]["pane_id"].clone())
        .collect();
    assert_eq!(gets, vec![json!("wA:p1"), json!("wA:p1"), json!("wA:p10")]);
}

#[test]
fn board_pane_open_replaces_a_recorded_pane_that_no_longer_exists() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());
    let socket = canonical(&herdr);
    seed_board_pane(&d, &socket, "wA:dead", "space=wA;issue=ENG-123");

    let v = open_board(&d, &herdr, "split").unwrap();

    assert_eq!(v["pane_id"], "wA:p10");
    assert_eq!(v["reused"], json!(false));
    assert_eq!(herdr.count("plugin.pane.open"), 1);
    assert_eq!(herdr.count("plugin.pane.close"), 0);
    let panes: Vec<String> = d
        .store
        .lock()
        .list_board_panes(&socket)
        .unwrap()
        .into_iter()
        .map(|p| p.pane_id)
        .collect();
    assert_eq!(panes, vec!["wA:p10".to_string()]);
}

#[test]
fn board_pane_close_refuses_a_context_with_no_recorded_pane_before_any_herdr_call() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());

    let err = handle_request(
        &d,
        "board.pane.close",
        json!({"context": {"issue": "ENG-123"}, "origin_socket": herdr.socket}),
    )
    .unwrap_err();

    assert_eq!(err.code(), 2);
    assert!(err.to_string().contains("no board pane"), "{err}");
    assert!(herdr.methods().is_empty(), "herdr was contacted");
}

#[test]
fn board_pane_close_closes_the_recorded_pane_and_clears_its_row() {
    let (herdr, live) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());
    open_board(&d, &herdr, "split").unwrap();

    let v = handle_request(
        &d,
        "board.pane.close",
        json!({"context": {"space": "wA", "issue": "ENG-123"}, "origin_socket": herdr.socket}),
    )
    .unwrap();

    assert_eq!(
        v,
        json!({"pane_id": "wA:p10", "closed": true, "gone": false})
    );
    let closes = herdr.requests_for("plugin.pane.close");
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0]["params"], json!({"pane_id": "wA:p10"}));
    assert!(!live.lock().unwrap().iter().any(|p| p == "wA:p10"));
    assert!(d
        .store
        .lock()
        .list_board_panes(&canonical(&herdr))
        .unwrap()
        .is_empty());
}

#[test]
fn board_pane_close_clears_a_dead_recorded_pane_without_a_close() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());
    let socket = canonical(&herdr);
    seed_board_pane(&d, &socket, "wA:dead", "issue=ENG-123");

    let v = handle_request(
        &d,
        "board.pane.close",
        json!({"context": {"issue": "ENG-123"}, "origin_socket": herdr.socket}),
    )
    .unwrap();

    assert_eq!(
        v,
        json!({"pane_id": "wA:dead", "closed": false, "gone": true})
    );
    assert_eq!(herdr.methods(), vec!["ping", "pane.get"]);
    assert!(d.store.lock().list_board_panes(&socket).unwrap().is_empty());
}

fn open_session(d: &Arc<Daemon>, herdr: &FakeHerdr, origin_pane: &str) -> Result<Value> {
    handle_request(
        d,
        "board.pane.open",
        json!({
            "context": {"session": true},
            "placement": "split",
            "origin_socket": herdr.socket,
            "origin_pane": origin_pane,
            "session_cwd": "/work/tree/src",
            "claude_session_id": "claude-1",
        }),
    )
}

#[test]
fn a_session_pane_opens_the_session_entrypoint_with_the_agents_identity_in_its_env() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());

    let v = open_session(&d, &herdr, "wA:p1").unwrap();

    assert_eq!(v["pane_id"], "wA:p10");
    let sent = herdr.requests_for("plugin.pane.open");
    assert_eq!(sent.len(), 1);
    let params = &sent[0]["params"];
    assert_eq!(params["entrypoint"], "session");
    assert_eq!(params["placement"], "split");
    assert_eq!(params["focus"], json!(false));
    assert_eq!(params["target_pane_id"], "wA:p1");
    // The socket goes as the agent's herdr exported it: the daemon keys its
    // snapshot cache and the session lookup on that raw string.
    assert_eq!(
        params["env"],
        json!({
            "BOARD_SOCKET": "/tmp/board-test.sock",
            "BOARD_DB": "/tmp/board-test.db",
            "BOARD_SESSION_SOCKET": herdr.socket.to_string_lossy(),
            "BOARD_SESSION_PANE": "wA:p1",
            "BOARD_SESSION_WORKSPACE": "wA",
            "BOARD_SESSION_CWD": "/work/tree/src",
            "BOARD_SESSION_CLAUDE": "claude-1",
        })
    );
    let row = d
        .store
        .lock()
        .board_pane_for_context(&canonical(&herdr), "session;pane=wA:p1")
        .unwrap()
        .expect("recorded pane");
    assert_eq!(row.pane_id, "wA:p10");
}

#[test]
fn each_agent_gets_its_own_session_pane_and_closes_only_its_own() {
    let (herdr, _) = board_pane_herdr(&["wA:p1", "wA:p2"]);
    let d = test_daemon(Config::default());

    let first = open_session(&d, &herdr, "wA:p1").unwrap();
    let second = open_session(&d, &herdr, "wA:p2").unwrap();
    assert_ne!(first["pane_id"], second["pane_id"]);
    assert_eq!(second["reused"], json!(false));

    let err = handle_request(
        &d,
        "board.pane.close",
        json!({"context": {"session": true}, "origin_socket": herdr.socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1, "{err}");

    let v = handle_request(
        &d,
        "board.pane.close",
        json!({"context": {"session": true}, "origin_socket": herdr.socket,
               "origin_pane": "wA:p2"}),
    )
    .unwrap();
    assert_eq!(v["pane_id"], second["pane_id"]);
    assert_eq!(
        d.store
            .lock()
            .list_board_panes(&canonical(&herdr))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_session_context_refuses_a_target_or_a_malformed_identity_before_any_herdr_call() {
    let (herdr, _) = board_pane_herdr(&["wA:p1"]);
    let d = test_daemon(Config::default());
    for (context, cwd, claude) in [
        (
            json!({"session": true, "issue": "ENG-123"}),
            json!("/w"),
            json!("c"),
        ),
        (json!({"session": true, "card": 7}), json!("/w"), json!("c")),
        (json!({"session": true}), json!("relative/dir"), json!("c")),
        (
            json!({"session": true}),
            json!("/w\nBOARD_DB=/x"),
            json!("c"),
        ),
        (json!({"session": true}), json!("/w"), json!("c d\u{1b}")),
    ] {
        let err = handle_request(
            &d,
            "board.pane.open",
            json!({
                "context": context,
                "placement": "split",
                "origin_socket": herdr.socket,
                "origin_pane": "wA:p1",
                "session_cwd": cwd,
                "claude_session_id": claude,
            }),
        )
        .unwrap_err();
        assert_eq!(err.code(), 1, "{context} {cwd} {claude}");
        assert!(err.to_string().contains("session"), "{err}");
    }
    assert!(herdr.methods().is_empty(), "herdr was contacted");
}

#[test]
fn board_notify_sends_one_cleaned_notification() {
    let (herdr, _) = board_pane_herdr(&[]);
    let d = test_daemon(Config::default());

    let v = handle_request(
        &d,
        "board.notify",
        json!({
            "origin_socket": herdr.socket,
            "title": "ENG-123\u{1b}[2J ready",
            "body": "review\u{202E} please\u{7}",
        }),
    )
    .unwrap();

    assert_eq!(v, json!({"shown": true}));
    assert_eq!(herdr.methods(), vec!["ping", "notification.show"]);
    let sent = herdr.requests_for("notification.show");
    assert_eq!(sent[0]["params"]["title"], "ENG-123[2J ready");
    assert_eq!(sent[0]["params"]["body"], "review please");
    assert_eq!(sent[0]["params"]["sound"], "none");
}

#[test]
fn board_notify_refuses_an_empty_title_before_any_herdr_call() {
    let (herdr, _) = board_pane_herdr(&[]);
    let d = test_daemon(Config::default());
    let err = handle_request(
        &d,
        "board.notify",
        json!({"origin_socket": herdr.socket, "title": "\u{1b}\u{7} ", "body": "x"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    assert!(err.to_string().contains("title"), "{err}");
    assert!(herdr.methods().is_empty(), "herdr was contacted");
}

const CALLER_DIR: &str = "/work/repo-d";
const CALLER_READS: &[&str] = &["ping", "pane.get", "pane.list", "workspace.list"];

fn claude_pane(pane_id: &str, workspace_id: &str, cwd: &str) -> Value {
    let mut pane = testkit::pane_info(pane_id);
    pane["workspace_id"] = json!(workspace_id);
    pane["tab_id"] = json!(format!("{workspace_id}:t1"));
    pane["agent"] = json!("claude");
    pane["cwd"] = json!(cwd);
    pane["title"] = json!(format!("claude in {pane_id}"));
    pane
}

/// A fake herdr session that lists `panes` and one workspace per
/// `(id, label)`, and panics on anything else, so a mutation fails the test.
fn caller_session(panes: Vec<Value>, workspaces: &[(&str, &str)]) -> FakeHerdr {
    let workspaces: Vec<Value> = workspaces
        .iter()
        .map(|(id, label)| {
            json!({
                "workspace_id": id, "label": label, "number": 1,
                "focused": false, "active_tab_id": "", "agent_status": "idle"
            })
        })
        .collect();
    let listed = panes.clone();
    testkit::herdr_server()
        .on("pane.get", move |req| {
            match listed
                .iter()
                .find(|p| p["pane_id"] == req["params"]["pane_id"])
            {
                Some(pane) => testkit::reply(req, json!({"type": "pane_info", "pane": pane})),
                None => testkit::error(req, "pane_not_found", "pane not found"),
            }
        })
        .on("pane.list", move |req| {
            testkit::reply(req, json!({"type": "pane_list", "panes": panes}))
        })
        .on("workspace.list", move |req| {
            testkit::reply(req, json!({"workspaces": workspaces}))
        })
        .serve()
}

/// One session holding a single Claude pane `w1:p2` in [`CALLER_DIR`].
fn one_claude_session(label: &str) -> FakeHerdr {
    caller_session(
        vec![claude_pane("w1:p2", "w1", CALLER_DIR)],
        &[("w1", label)],
    )
}

fn socket_of(herdr: &FakeHerdr) -> String {
    herdr.socket.to_string_lossy().into_owned()
}

fn session_entry(name: &str, socket: &Path) -> SessionEntry {
    SessionEntry {
        name: name.into(),
        default: name == "default",
        running: true,
        socket_path: socket.to_string_lossy().into_owned(),
    }
}

fn caller_daemon(entries: Vec<SessionEntry>) -> Arc<Daemon> {
    let default_socket = entries
        .first()
        .map(|e| PathBuf::from(&e.socket_path))
        .unwrap_or_else(|| PathBuf::from("/tmp/board-test.sock"));
    test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::with_entries(default_socket, entries)),
    )
}

fn resolve(d: &Arc<Daemon>, params: Value) -> Value {
    handle_request(d, "caller.resolve", params).unwrap()
}

fn assert_only_reads(herdr: &FakeHerdr) {
    for method in herdr.methods() {
        assert!(
            CALLER_READS.contains(&method.as_str()),
            "caller.resolve sent {method} to herdr"
        );
    }
}

#[test]
fn caller_resolve_offers_a_claude_pane_from_every_session_with_its_socket() {
    let one = one_claude_session("Alpha");
    let two = caller_session(
        vec![claude_pane("w3:p1", "w3", CALLER_DIR)],
        &[("w3", "Beta")],
    );
    let d = caller_daemon(vec![
        session_entry("default", &one.socket),
        session_entry("work", &two.socket),
    ]);

    let v = resolve(&d, json!({"cwd": CALLER_DIR}));

    assert_eq!(v["state"], "unconfirmed", "{v}");
    let candidates = v["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2, "{v}");
    assert_eq!(candidates[0]["pane"], "default/w1:p2");
    assert_eq!(candidates[0]["socket"], socket_of(&one));
    assert_eq!(candidates[0]["workspace_label"], "Alpha");
    assert_eq!(candidates[0]["title"], "claude in w1:p2");
    assert_eq!(candidates[1]["pane"], "work/w3:p1");
    assert_eq!(candidates[1]["socket"], socket_of(&two));
    assert_eq!(candidates[1]["workspace_label"], "Beta");
    // The whole session is listed, not one workspace.
    assert_eq!(one.requests_for("pane.list")[0]["params"], json!({}));
    assert_only_reads(&one);
    assert_only_reads(&two);
}

#[test]
fn caller_resolve_skips_an_unreachable_session() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![
        session_entry("default", &one.socket),
        session_entry("gone", Path::new("/tmp/hb-caller-no-such-herdr.sock")),
    ]);

    let v = resolve(&d, json!({"cwd": CALLER_DIR}));

    assert_eq!(v["state"], "unconfirmed", "{v}");
    let candidates = v["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1, "{v}");
    assert_eq!(candidates[0]["pane"], "default/w1:p2");
}

#[test]
fn caller_resolve_never_asks_a_wrong_protocol_session_for_panes() {
    let one = one_claude_session("Alpha");
    let old = testkit::herdr_server()
        .protocol(board_herdr::SUPPORTED_HERDR_PROTOCOL - 1)
        .serve();
    let d = caller_daemon(vec![
        session_entry("default", &one.socket),
        session_entry("old", &old.socket),
    ]);

    let v = resolve(&d, json!({"cwd": CALLER_DIR}));

    assert_eq!(v["candidates"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(old.methods(), vec!["ping"]);
}

#[test]
fn caller_resolve_ignores_a_stopped_session() {
    let one = one_claude_session("Alpha");
    let stopped = caller_session(vec![claude_pane("w1:p2", "w1", CALLER_DIR)], &[]);
    let mut entry = session_entry("stopped", &stopped.socket);
    entry.running = false;
    let d = caller_daemon(vec![session_entry("default", &one.socket), entry]);

    let v = resolve(&d, json!({"cwd": CALLER_DIR}));

    assert_eq!(v["candidates"].as_array().unwrap().len(), 1, "{v}");
    assert!(
        stopped.methods().is_empty(),
        "a stopped session was contacted"
    );
}

#[test]
fn caller_resolve_reports_not_in_herdr_when_nothing_matches() {
    let mut shell = testkit::pane_info("w1:p1");
    shell["cwd"] = json!(CALLER_DIR);
    let one = caller_session(
        vec![shell, claude_pane("w1:p2", "w1", "/work/other")],
        &[("w1", "Alpha")],
    );
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);

    let v = resolve(&d, json!({"cwd": CALLER_DIR}));

    assert_eq!(v, json!({"state": "not_in_herdr"}));
}

#[test]
fn caller_resolve_session_qualified_pane_picks_one_of_two_repeated_ids() {
    let one = one_claude_session("Alpha");
    let two = one_claude_session("Beta");
    let d = caller_daemon(vec![
        session_entry("default", &one.socket),
        session_entry("work", &two.socket),
    ]);

    let v = resolve(&d, json!({"cwd": CALLER_DIR, "pane": "work/w1:p2"}));

    assert_eq!(
        v,
        json!({
            "state": "resolved",
            "location": {
                "session": "work",
                "socket": two.socket.to_string_lossy(),
                "workspace_id": "w1",
                "tab_id": "w1:t1",
                "pane_id": "w1:p2",
            }
        })
    );
    // Naming the session means no other session is asked.
    assert!(one.methods().is_empty(), "{:?}", one.methods());
    assert_only_reads(&two);
}

#[test]
fn caller_resolve_bare_pane_repeated_across_sessions_stays_unconfirmed() {
    let one = one_claude_session("Alpha");
    let two = one_claude_session("Beta");
    let d = caller_daemon(vec![
        session_entry("default", &one.socket),
        session_entry("work", &two.socket),
    ]);

    let v = resolve(
        &d,
        json!({"cwd": CALLER_DIR, "pane": "w1:p2", "claude_session_id": "sess-s"}),
    );

    assert_eq!(v["state"], "unconfirmed", "{v}");
    assert_eq!(v["candidates"].as_array().unwrap().len(), 2, "{v}");
    assert!(d.caller_locations.lock().unwrap().is_empty());
}

#[test]
fn caller_resolve_pane_that_matches_nothing_is_not_in_herdr() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);

    let v = resolve(
        &d,
        json!({"cwd": CALLER_DIR, "pane": "default/w9:p9", "claude_session_id": "sess-s"}),
    );

    assert_eq!(v, json!({"state": "not_in_herdr"}));
    assert!(d.caller_locations.lock().unwrap().is_empty());
}

#[test]
fn caller_resolve_remembers_a_confirmed_pane_for_its_claude_session() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);

    let confirmed = resolve(
        &d,
        json!({"cwd": CALLER_DIR, "pane": "default/w1:p2", "claude_session_id": "sess-s"}),
    );
    assert_eq!(confirmed["state"], "resolved", "{confirmed}");
    let asked = one.methods().len();

    // A different folder proves the answer comes from memory, not a lookup.
    let remembered = resolve(
        &d,
        json!({"cwd": "/elsewhere", "claude_session_id": "sess-s"}),
    );

    assert_eq!(remembered, confirmed);
    assert_eq!(&one.methods()[asked..], ["ping", "pane.get"]);
    assert_eq!(
        one.requests_for("pane.get")[0]["params"],
        json!({"pane_id": "w1:p2"})
    );
}

fn remember(d: &Arc<Daemon>, id: &str, socket: &str, pane_id: &str) {
    d.caller_locations.lock().unwrap().insert(
        id.to_string(),
        CallerLocation {
            session: "default".into(),
            socket: socket.into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            pane_id: pane_id.into(),
        },
    );
}

#[test]
fn caller_resolve_drops_a_remembered_pane_that_closed_and_looks_up_the_folder() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);
    remember(&d, "sess-s", &socket_of(&one), "w1:p9");

    let v = resolve(
        &d,
        json!({"cwd": CALLER_DIR, "claude_session_id": "sess-s"}),
    );

    assert_eq!(v["state"], "unconfirmed", "{v}");
    assert_eq!(v["candidates"][0]["pane"], "default/w1:p2", "{v}");
    assert!(d.caller_locations.lock().unwrap().is_empty());
    assert_eq!(one.count("pane.get"), 1);
    assert_only_reads(&one);
}

#[test]
fn caller_resolve_drops_a_remembered_pane_whose_session_is_gone() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);
    remember(&d, "sess-s", "/tmp/hb-caller-no-such-herdr.sock", "w1:p2");

    let v = resolve(
        &d,
        json!({"cwd": CALLER_DIR, "claude_session_id": "sess-s"}),
    );

    assert_eq!(v["state"], "unconfirmed", "{v}");
    assert!(d.caller_locations.lock().unwrap().is_empty());
}

#[test]
fn caller_resolve_does_not_hand_one_sessions_memory_to_another() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);
    resolve(
        &d,
        json!({"cwd": CALLER_DIR, "pane": "default/w1:p2", "claude_session_id": "sess-s"}),
    );

    let other = resolve(
        &d,
        json!({"cwd": CALLER_DIR, "claude_session_id": "sess-t"}),
    );

    assert_eq!(other["state"], "unconfirmed", "{other}");
}

#[test]
fn caller_resolve_with_pane_and_no_session_id_remembers_nothing() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);

    let v = resolve(&d, json!({"cwd": CALLER_DIR, "pane": "default/w1:p2"}));

    assert_eq!(v["state"], "resolved", "{v}");
    assert!(d.caller_locations.lock().unwrap().is_empty());
}

#[test]
fn caller_resolve_explicit_pane_replaces_a_remembered_one() {
    let one = caller_session(
        vec![
            claude_pane("w1:p2", "w1", CALLER_DIR),
            claude_pane("w1:p3", "w1", CALLER_DIR),
        ],
        &[("w1", "Alpha")],
    );
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);
    resolve(
        &d,
        json!({"cwd": CALLER_DIR, "pane": "default/w1:p2", "claude_session_id": "sess-s"}),
    );
    resolve(
        &d,
        json!({"cwd": CALLER_DIR, "pane": "default/w1:p3", "claude_session_id": "sess-s"}),
    );

    let v = resolve(
        &d,
        json!({"cwd": CALLER_DIR, "claude_session_id": "sess-s"}),
    );

    assert_eq!(v["location"]["pane_id"], "w1:p3", "{v}");
}

#[test]
fn caller_resolve_refuses_a_malformed_session_id_before_herdr() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);

    let err = handle_request(
        &d,
        "caller.resolve",
        json!({"cwd": CALLER_DIR, "claude_session_id": "bad id\n"}),
    )
    .unwrap_err();

    assert_eq!(err.code(), 1);
    assert!(err.to_string().contains("session id"), "{err}");
    assert!(one.methods().is_empty(), "herdr was contacted");
}

#[test]
fn caller_resolve_without_a_session_registry_is_herdr_unavailable() {
    let d = test_daemon(Config::default());
    let err = handle_request(&d, "caller.resolve", json!({"cwd": CALLER_DIR})).unwrap_err();
    assert_eq!(err.code(), 4);
}

#[test]
fn caller_resolve_remembered_only_without_memory_lists_no_session() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);

    let v = resolve(
        &d,
        json!({"cwd": CALLER_DIR, "claude_session_id": "sess-s", "remembered_only": true}),
    );

    assert_eq!(v, json!({"state": "not_in_herdr"}));
    assert!(one.methods().is_empty(), "{:?}", one.methods());
}

#[test]
fn caller_resolve_remembered_only_drops_a_closed_pane_without_a_lookup() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);
    remember(&d, "sess-s", &socket_of(&one), "w1:p9");

    let v = resolve(
        &d,
        json!({"cwd": CALLER_DIR, "claude_session_id": "sess-s", "remembered_only": true}),
    );

    assert_eq!(v, json!({"state": "not_in_herdr"}));
    assert_eq!(one.methods(), ["ping", "pane.get"]);
    assert!(d.caller_locations.lock().unwrap().is_empty());
}

#[test]
fn caller_resolve_remembered_only_returns_a_live_remembered_pane() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);
    remember(&d, "sess-s", &socket_of(&one), "w1:p2");

    let v = resolve(
        &d,
        json!({"cwd": CALLER_DIR, "claude_session_id": "sess-s", "remembered_only": true}),
    );

    assert_eq!(v["state"], "resolved", "{v}");
    assert_eq!(v["location"]["pane_id"], "w1:p2", "{v}");
    assert_eq!(one.methods(), ["ping", "pane.get"]);
}

#[test]
fn caller_resolve_remembered_only_still_checks_an_explicit_pane() {
    let one = one_claude_session("Alpha");
    let d = caller_daemon(vec![session_entry("default", &one.socket)]);

    let v = resolve(
        &d,
        json!({"cwd": CALLER_DIR, "pane": "default/w1:p2", "remembered_only": true}),
    );

    assert_eq!(v["state"], "resolved", "{v}");
}

#[test]
fn caller_resolve_skips_workspace_list_for_a_session_without_a_claude_pane() {
    let one = one_claude_session("Alpha");
    let mut shell = testkit::pane_info("w3:p1");
    shell["cwd"] = json!(CALLER_DIR);
    let plain = caller_session(vec![shell], &[("w3", "Shells")]);
    let d = caller_daemon(vec![
        session_entry("default", &one.socket),
        session_entry("plain", &plain.socket),
    ]);

    let v = resolve(&d, json!({"cwd": CALLER_DIR}));

    assert_eq!(v["candidates"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(plain.methods(), ["ping", "pane.list"]);
    assert_eq!(one.count("workspace.list"), 1);
}

#[test]
fn caller_resolve_explicit_shell_pane_still_lists_workspaces() {
    let mut shell = testkit::pane_info("w3:p1");
    shell["workspace_id"] = json!("w3");
    let plain = caller_session(vec![shell], &[("w3", "Shells")]);
    let d = caller_daemon(vec![session_entry("plain", &plain.socket)]);

    let v = resolve(&d, json!({"cwd": CALLER_DIR, "pane": "plain/w3:p1"}));

    assert_eq!(v["state"], "resolved", "{v}");
    assert_eq!(v["location"]["workspace_id"], "w3", "{v}");
    assert_eq!(plain.count("workspace.list"), 1);
}
