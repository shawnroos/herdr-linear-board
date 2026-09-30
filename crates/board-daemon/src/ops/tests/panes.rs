//! `pane.set_title` — the RPC the TUI plugin pane uses instead of shelling out
//! to `herdr pane rename` itself.

use super::*;

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
            "Herdr {} with protocol {} is required",
            board_herdr::SUPPORTED_HERDR_VERSION,
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
