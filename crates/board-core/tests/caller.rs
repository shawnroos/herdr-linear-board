//! The caller-match rule and the `caller.resolve` wire shape.

use board_core::engine::{match_caller, parse_pane_filter, CallerPane, PaneFilter};
use board_core::protocol::{
    CallerCandidate, CallerLocation, CallerResolveParams, CallerResolveResult,
};
use serde_json::json;

fn pane(session: &str, pane_id: &str, agent: &str, cwd: &str) -> CallerPane {
    let workspace_id = pane_id.split(':').next().unwrap_or_default().to_string();
    CallerPane {
        session: session.into(),
        socket: format!("/tmp/{session}.sock"),
        workspace_id: workspace_id.clone(),
        workspace_label: Some(format!("label-{workspace_id}")),
        tab_id: format!("{workspace_id}:t1"),
        pane_id: pane_id.into(),
        agent: Some(agent.into()),
        cwd: Some(cwd.into()),
        foreground_cwd: None,
        title: Some(format!("title {pane_id}")),
    }
}

fn candidate_values(result: &CallerResolveResult) -> Vec<String> {
    match result {
        CallerResolveResult::Unconfirmed { candidates } => {
            candidates.iter().map(|c| c.pane.clone()).collect()
        }
        other => panic!("expected unconfirmed, got {other:?}"),
    }
}

#[test]
fn one_claude_pane_in_the_folder_is_a_candidate_never_resolved() {
    let panes = vec![pane("default", "w2:p1", "claude", "/work/repo")];
    let result = match_caller(&panes, "/work/repo", None);
    match result {
        CallerResolveResult::Unconfirmed { candidates } => {
            assert_eq!(
                candidates,
                vec![CallerCandidate {
                    pane: "default/w2:p1".into(),
                    session: "default".into(),
                    socket: "/tmp/default.sock".into(),
                    workspace_id: "w2".into(),
                    workspace_label: Some("label-w2".into()),
                    tab_id: "w2:t1".into(),
                    pane_id: "w2:p1".into(),
                    title: Some("title w2:p1".into()),
                }]
            );
        }
        other => panic!("a folder match must not resolve: {other:?}"),
    }
}

#[test]
fn two_claude_panes_in_different_sessions_are_both_candidates() {
    let panes = vec![
        pane("work", "w1:p2", "claude", "/work/repo"),
        pane("default", "w1:p2", "claude", "/work/repo"),
    ];
    let result = match_caller(&panes, "/work/repo", None);
    assert_eq!(
        candidate_values(&result),
        vec!["default/w1:p2".to_string(), "work/w1:p2".to_string()]
    );
}

#[test]
fn a_non_claude_pane_in_the_folder_is_ignored() {
    let panes = vec![
        pane("default", "w1:p1", "shell", "/work/repo"),
        CallerPane {
            agent: None,
            ..pane("default", "w1:p3", "", "/work/repo")
        },
    ];
    assert_eq!(
        match_caller(&panes, "/work/repo", None),
        CallerResolveResult::NotInHerdr
    );
}

#[test]
fn a_pane_matching_only_on_foreground_cwd_is_a_candidate() {
    let panes = vec![CallerPane {
        foreground_cwd: Some("/work/repo".into()),
        ..pane("default", "w1:p1", "claude", "/somewhere/else")
    }];
    assert_eq!(
        candidate_values(&match_caller(&panes, "/work/repo", None)),
        vec!["default/w1:p1".to_string()]
    );
}

#[test]
fn a_symlinked_path_matches_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let panes = vec![pane("default", "w1:p1", "claude", real.to_str().unwrap())];
    assert_eq!(
        candidate_values(&match_caller(&panes, link.to_str().unwrap(), None)),
        vec!["default/w1:p1".to_string()]
    );
}

#[test]
fn a_trailing_slash_still_matches_on_an_existing_and_a_missing_path() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().to_str().unwrap().to_string();
    let panes = vec![pane("default", "w1:p1", "claude", &existing)];
    assert_eq!(
        candidate_values(&match_caller(&panes, &format!("{existing}/"), None)),
        vec!["default/w1:p1".to_string()]
    );

    let missing = "/no/such/dir/for/caller/test";
    let panes = vec![pane("default", "w1:p1", "claude", &format!("{missing}/"))];
    assert_eq!(
        candidate_values(&match_caller(&panes, missing, None)),
        vec!["default/w1:p1".to_string()]
    );
}

#[test]
fn a_different_folder_and_an_empty_cwd_never_match() {
    let panes = vec![
        pane("default", "w1:p1", "claude", "/work/other"),
        pane("default", "w1:p2", "claude", ""),
    ];
    assert_eq!(
        match_caller(&panes, "/work/repo", None),
        CallerResolveResult::NotInHerdr
    );
    assert_eq!(
        match_caller(&panes, "", None),
        CallerResolveResult::NotInHerdr
    );
}

#[test]
fn zero_panes_is_not_in_herdr() {
    assert_eq!(
        match_caller(&[], "/work/repo", None),
        CallerResolveResult::NotInHerdr
    );
}

#[test]
fn a_session_qualified_filter_resolves_exactly_that_pane() {
    let panes = vec![
        pane("work", "w1:p2", "claude", "/work/repo"),
        pane("default", "w1:p2", "claude", "/work/repo"),
    ];
    assert_eq!(
        match_caller(&panes, "/work/repo", Some("work/w1:p2")),
        CallerResolveResult::Resolved {
            location: CallerLocation {
                session: "work".into(),
                socket: "/tmp/work.sock".into(),
                workspace_id: "w1".into(),
                tab_id: "w1:t1".into(),
                pane_id: "w1:p2".into(),
            }
        }
    );
}

#[test]
fn an_explicit_filter_ignores_agent_and_folder() {
    let panes = vec![pane("default", "w3:p1", "shell", "/elsewhere")];
    assert!(matches!(
        match_caller(&panes, "/work/repo", Some("default/w3:p1")),
        CallerResolveResult::Resolved { .. }
    ));
}

#[test]
fn a_bare_pane_id_in_two_sessions_returns_both_and_in_one_resolves() {
    let panes = vec![
        pane("work", "w1:p2", "claude", "/work/repo"),
        pane("default", "w1:p2", "claude", "/work/repo"),
        pane("default", "w4:p1", "claude", "/work/other"),
    ];
    assert_eq!(
        candidate_values(&match_caller(&panes, "/work/repo", Some("w1:p2"))),
        vec!["default/w1:p2".to_string(), "work/w1:p2".to_string()]
    );
    assert!(matches!(
        match_caller(&panes, "/work/repo", Some("w4:p1")),
        CallerResolveResult::Resolved { location } if location.session == "default"
    ));
}

#[test]
fn a_filter_naming_no_pane_is_not_in_herdr() {
    let panes = vec![pane("default", "w1:p2", "claude", "/work/repo")];
    for filter in ["work/w1:p2", "default/w9:p9", "w9:p9"] {
        assert_eq!(
            match_caller(&panes, "/work/repo", Some(filter)),
            CallerResolveResult::NotInHerdr,
            "{filter}"
        );
    }
}

#[test]
fn the_pane_filter_splits_the_session_at_the_last_slash() {
    assert_eq!(
        parse_pane_filter("default/w2:p2"),
        PaneFilter {
            session: Some("default".into()),
            pane_id: "w2:p2".into()
        }
    );
    assert_eq!(
        parse_pane_filter("team/a/w2:p2"),
        PaneFilter {
            session: Some("team/a".into()),
            pane_id: "w2:p2".into()
        }
    );
    assert_eq!(
        parse_pane_filter("w2:p2"),
        PaneFilter {
            session: None,
            pane_id: "w2:p2".into()
        }
    );
    assert_eq!(
        parse_pane_filter(" /w2:p2 "),
        PaneFilter {
            session: None,
            pane_id: "w2:p2".into()
        }
    );
}

#[test]
fn caller_resolve_wire_shape_is_pinned() {
    let params = CallerResolveParams {
        cwd: "/work/repo".into(),
        pane: None,
        claude_session_id: None,
    };
    assert_eq!(
        serde_json::to_value(&params).unwrap(),
        json!({"cwd": "/work/repo"})
    );
    let params: CallerResolveParams = serde_json::from_value(json!({
        "cwd": "/work/repo", "pane": "default/w1:p1", "claude_session_id": "abc"
    }))
    .unwrap();
    assert_eq!(params.pane.as_deref(), Some("default/w1:p1"));
    assert_eq!(params.claude_session_id.as_deref(), Some("abc"));

    let resolved = CallerResolveResult::Resolved {
        location: CallerLocation {
            session: "default".into(),
            socket: "/tmp/h.sock".into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            pane_id: "w1:p1".into(),
        },
    };
    assert_eq!(
        serde_json::to_value(&resolved).unwrap(),
        json!({"state": "resolved", "location": {
            "session": "default", "socket": "/tmp/h.sock", "workspace_id": "w1",
            "tab_id": "w1:t1", "pane_id": "w1:p1"
        }})
    );

    let unconfirmed = CallerResolveResult::Unconfirmed {
        candidates: vec![CallerCandidate {
            pane: "default/w1:p1".into(),
            session: "default".into(),
            socket: "/tmp/h.sock".into(),
            workspace_id: "w1".into(),
            workspace_label: Some("repo".into()),
            tab_id: "w1:t1".into(),
            pane_id: "w1:p1".into(),
            title: None,
        }],
    };
    assert_eq!(
        serde_json::to_value(&unconfirmed).unwrap(),
        json!({"state": "unconfirmed", "candidates": [{
            "pane": "default/w1:p1", "session": "default", "socket": "/tmp/h.sock",
            "workspace_id": "w1", "workspace_label": "repo", "tab_id": "w1:t1",
            "pane_id": "w1:p1", "title": null
        }]})
    );

    assert_eq!(
        serde_json::to_value(CallerResolveResult::NotInHerdr).unwrap(),
        json!({"state": "not_in_herdr"})
    );

    for value in [&resolved, &unconfirmed, &CallerResolveResult::NotInHerdr] {
        let back: CallerResolveResult =
            serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap();
        assert_eq!(&back, value);
    }
}

#[cfg(feature = "fake-client")]
#[test]
fn the_fake_client_answers_caller_resolve_with_the_seeded_result() {
    use board_core::client::{BoardClient, FakeBoardClient};

    let params = CallerResolveParams {
        cwd: "/work/repo".into(),
        pane: None,
        claude_session_id: Some("abc".into()),
    };
    let mut fake = FakeBoardClient::new().unwrap();
    assert_eq!(
        fake.caller_resolve(&params).unwrap(),
        CallerResolveResult::NotInHerdr
    );

    let seeded = CallerResolveResult::Resolved {
        location: CallerLocation {
            session: "default".into(),
            socket: "/tmp/h.sock".into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            pane_id: "w1:p1".into(),
        },
    };
    let mut fake = FakeBoardClient::new()
        .unwrap()
        .with_caller_resolve(seeded.clone());
    assert_eq!(fake.caller_resolve(&params).unwrap(), seeded);

    let mut fake = FakeBoardClient::new()
        .unwrap()
        .with_caller_resolve_error("herdr unreachable");
    assert!(fake.caller_resolve(&params).is_err());
}
