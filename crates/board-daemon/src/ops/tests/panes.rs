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
