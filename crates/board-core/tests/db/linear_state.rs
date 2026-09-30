use super::mem;
use board_core::db::{
    is_issue_identifier, ActivityClaims, BoardPanePlacement, GroupingConfig, GroupingMapping,
    LinearOwner, MarkKind, NewActivity, NewBoardPane, NewMark, NewShowRequest, SessionScope,
    ShowOutcome, SpaceBinding, SpaceGrouping, WorktreeBinding, WorktreeBindingState,
    LINEAR_ACTIVITY_KEEP_PER_SPACE,
};
use board_core::Error;
use serde_json::{json, Value};

fn mapping(value: Value) -> GroupingMapping {
    serde_json::from_value(value).unwrap()
}

fn refusal(result: board_core::Result<()>) -> String {
    match result {
        Err(Error::BadRequest(message)) => message,
        other => panic!("expected a named refusal, got {other:?}"),
    }
}

fn base() -> GroupingMapping {
    mapping(json!({"levels": {"tab": "parent", "column": "ticket"}, "filter": {"team": "WEB"}}))
}

#[test]
fn grouping_refuses_two_levels_sharing_a_kind_and_names_both() {
    let db = mem();
    let bad = mapping(json!({
        "levels": {"tab": "project", "column": "project"},
        "filter": {"team": "WEB"}
    }));
    let message = refusal(db.set_grouping_global(&bad));
    assert!(message.contains("column"), "{message}");
    assert!(message.contains("tab"), "{message}");
    assert!(message.contains("same kind"), "{message}");
    assert!(
        db.grouping_config().unwrap().is_none(),
        "a refused write stores nothing"
    );
}

#[test]
fn grouping_refuses_an_empty_filter_and_state_type_not() {
    let db = mem();
    let empty = mapping(json!({"levels": {"column": "state"}, "filter": {}}));
    assert!(refusal(db.set_grouping_global(&empty)).contains("names no keys"));

    let computed = mapping(json!({
        "levels": {"column": "state"},
        "filter": {"team": "WEB", "state-type-not": ["triage"]}
    }));
    let message = refusal(db.set_grouping_global(&computed));
    assert!(message.contains("state-type-not"), "{message}");
    assert!(message.contains("unknown filter key"), "{message}");
    assert!(db.grouping_config().unwrap().is_none());
}

#[test]
fn grouping_validation_is_default_deny() {
    let cases = [
        (
            json!({"levels": {}, "filter": {"team": "WEB"}}),
            "names no level",
        ),
        (
            json!({"levels": {"lane": "team"}, "filter": {"team": "WEB"}}),
            "unknown level",
        ),
        (
            json!({"levels": {"row": "estimate"}, "filter": {"team": "WEB"}}),
            "not a level kind",
        ),
        (
            json!({"levels": {"row": "label-group:"}, "filter": {"team": "WEB"}}),
            "label-group",
        ),
        (
            json!({"levels": {"row": "team"}, "filter": {"team": ""}}),
            "empty string",
        ),
        (
            json!({"levels": {"row": "team"}, "filter": {"team": []}}),
            "empty list",
        ),
        (
            json!({"levels": {"row": "team"}, "filter": {"team": "W\u{1b}B"}}),
            "control",
        ),
        (
            json!({"levels": {"row": "team"}, "filter": {"priority": 5}}),
            "priority",
        ),
        (
            json!({"levels": {"row": "team"}, "filter": {"priority": true}}),
            "priority",
        ),
        (
            json!({"levels": {"row": "team"}, "filter": {"state-type": "done"}}),
            "state type",
        ),
        (
            json!({"levels": {"row": "team"}, "filter": {"team": 7}}),
            "non-empty string",
        ),
    ];
    for (value, reason) in cases {
        let message = refusal(mapping(value.clone()).validate());
        assert!(message.contains(reason), "{value}: {message}");
    }
    for kind in [
        "ticket",
        "sub-ticket",
        "label-group:Area",
        "milestone",
        "cycle",
    ] {
        mapping(
            json!({"levels": {"row": kind}, "filter": {"state-type": ["started"], "priority": 0}}),
        )
        .validate()
        .unwrap_or_else(|e| panic!("{kind}: {e}"));
    }
}

#[test]
fn grouping_mapping_refuses_unknown_top_level_keys() {
    let parsed: Result<GroupingMapping, _> = serde_json::from_value(
        json!({"levels": {"row": "team"}, "filter": {"team": "WEB"}, "extra": 1}),
    );
    assert!(parsed.is_err());
    let missing_filter: Result<GroupingMapping, _> =
        serde_json::from_value(json!({"levels": {"row": "team"}}));
    assert!(missing_filter.is_err());
}

#[test]
fn a_space_override_needs_a_global_mapping() {
    let db = mem();
    let message = refusal(db.set_grouping_space("ws-a", &base()));
    assert!(message.contains("global"), "{message}");
}

#[test]
fn a_space_override_replaces_the_global_mapping_on_read() {
    let db = mem();
    db.set_grouping_global(&mapping(json!({
        "levels": {"tab": "project", "column": "state", "row": "assignee"},
        "filter": {"team": "WEB", "priority": [1, 2]}
    })))
    .unwrap();
    let override_ = mapping(json!({"levels": {"column": "cycle"}, "filter": {"project": "P1"}}));
    db.set_grouping_space("ws-a", &override_).unwrap();

    let resolved = db.grouping_for_space("ws-a").unwrap().unwrap();
    assert_eq!(resolved.space.as_deref(), Some("ws-a"));
    assert_eq!(
        resolved.mapping, override_,
        "nothing from the global mapping merges in"
    );

    let other = db.grouping_for_space("ws-b").unwrap().unwrap();
    assert_eq!(other.space, None);
    assert_eq!(other.mapping.levels.len(), 3);
}

#[test]
fn space_overrides_read_back_in_position_order() {
    let db = mem();
    db.set_grouping_global(&base()).unwrap();
    for space in ["zeta", "alpha", "mid"] {
        db.set_grouping_space(space, &base()).unwrap();
    }
    db.set_grouping_space(
        "alpha",
        &mapping(json!({"levels": {"row": "team"}, "filter": {"team": "X"}})),
    )
    .unwrap();
    let order = |db: &board_core::db::Db| -> Vec<String> {
        db.grouping_config()
            .unwrap()
            .unwrap()
            .spaces
            .into_iter()
            .map(|s| s.space)
            .collect()
    };
    assert_eq!(
        order(&db),
        ["zeta", "alpha", "mid"],
        "an update keeps its position"
    );

    assert!(db.remove_grouping_space("alpha").unwrap());
    db.set_grouping_space("alpha", &base()).unwrap();
    assert_eq!(order(&db), ["zeta", "mid", "alpha"]);

    let config = GroupingConfig {
        global: base(),
        spaces: vec![
            SpaceGrouping {
                space: "b".into(),
                mapping: base(),
            },
            SpaceGrouping {
                space: "a".into(),
                mapping: base(),
            },
        ],
    };
    db.replace_grouping(&config).unwrap();
    assert_eq!(db.grouping_config().unwrap().unwrap(), config);
    assert_eq!(order(&db), ["b", "a"]);
}

#[test]
fn replace_grouping_is_all_or_nothing() {
    let db = mem();
    db.set_grouping_global(&base()).unwrap();
    db.set_grouping_space("kept", &base()).unwrap();
    let before = db.grouping_config().unwrap();
    let bad_space = GroupingConfig {
        global: base(),
        spaces: vec![SpaceGrouping {
            space: "broken".into(),
            mapping: mapping(json!({"levels": {"row": "team"}, "filter": {}})),
        }],
    };
    let message = refusal(db.replace_grouping(&bad_space));
    assert!(
        message.contains("broken"),
        "the fault names the space: {message}"
    );
    let duplicate = GroupingConfig {
        global: base(),
        spaces: vec![
            SpaceGrouping {
                space: "x".into(),
                mapping: base(),
            },
            SpaceGrouping {
                space: "x".into(),
                mapping: base(),
            },
        ],
    };
    assert!(refusal(db.replace_grouping(&duplicate)).contains("more than once"));
    assert_eq!(db.grouping_config().unwrap(), before);
}

#[test]
fn marks_round_trip_with_their_card_and_space_keys() {
    let db = mem();
    let mark = db
        .add_mark(&NewMark {
            space: "ws-1",
            issue: "WEB-42",
            kind: MarkKind::Suggestion,
            text: Some("link this session?"),
            detail: Some(json!({"pane": "p-3"})),
            created_by: Some("agent"),
            owner: &LinearOwner::default(),
        })
        .unwrap();
    db.add_mark(&NewMark {
        space: "ws-2",
        issue: "WEB-43",
        kind: MarkKind::Attention,
        text: None,
        detail: None,
        created_by: None,
        owner: &LinearOwner::default(),
    })
    .unwrap();
    let marks = db.list_marks("ws-1").unwrap();
    assert_eq!(marks, vec![mark.clone()]);
    assert_eq!(mark.space, "ws-1");
    assert_eq!(mark.issue, "WEB-42");
    assert_eq!(mark.kind, MarkKind::Suggestion);
    assert_eq!(mark.detail, Some(json!({"pane": "p-3"})));
    assert!(db.remove_mark(mark.id).unwrap());
    assert!(!db.remove_mark(mark.id).unwrap());
    assert!(db.list_marks("ws-1").unwrap().is_empty());
}

#[test]
fn notes_round_trip_with_their_card_and_space_keys() {
    let db = mem();
    let uuid = "0b9f5a52-1c3e-4a7b-9d0e-2f6c8a1b3d4e";
    let note = db
        .add_note(
            "ws-1",
            uuid,
            "remember the flag",
            "user",
            &LinearOwner::default(),
        )
        .unwrap();
    db.add_note("ws-1", "WEB-9", "other", "user", &LinearOwner::default())
        .unwrap();
    assert_eq!(note.space, "ws-1");
    assert_eq!(note.issue, uuid);
    assert_eq!(
        db.list_notes("ws-1", Some(uuid)).unwrap(),
        vec![note.clone()]
    );
    assert_eq!(db.list_notes("ws-1", None).unwrap().len(), 2);
    assert!(db.remove_note(note.id).unwrap());
    assert_eq!(db.list_notes("ws-1", None).unwrap().len(), 1);
}

#[test]
fn show_requests_round_trip_and_drain_on_close() {
    let db = mem();
    let now = 1_790_000_000;
    let request = db
        .add_show_request(&NewShowRequest {
            space: "ws-1",
            issue: "WEB-7",
            reason: Some("review ready"),
            requested_by: Some("agent"),
            owner: &LinearOwner::default(),
            expires_at: now + 60,
        })
        .unwrap();
    assert_eq!(request.space, "ws-1");
    assert_eq!(request.issue, "WEB-7");
    assert_eq!(request.acknowledged_at, None);
    assert_eq!(
        db.pending_show_requests("ws-1", now).unwrap(),
        vec![request.clone()]
    );
    assert!(db.pending_show_requests("ws-2", now).unwrap().is_empty());
    assert!(db
        .close_show_request(request.id, ShowOutcome::Accepted)
        .unwrap());
    assert!(!db
        .close_show_request(request.id, ShowOutcome::Rejected)
        .unwrap());
    let closed = db.show_request(request.id).unwrap().unwrap();
    assert_eq!(closed.outcome, Some(ShowOutcome::Accepted));
    assert!(closed.acknowledged_at.is_some());
    assert!(db.pending_show_requests("ws-1", now).unwrap().is_empty());
}

#[test]
fn issue_keys_are_refused_unless_linear_shaped() {
    let db = mem();
    for bad in [
        "",
        "web-1",
        "WEB-0",
        "WEB-01",
        "WEB",
        "-WEB-1",
        "WEB-1 ",
        "WEB-1\n",
        "{0b9f5a52-1c3e-4a7b-9d0e-2f6c8a1b3d4e}",
    ] {
        assert!(!is_issue_identifier(bad), "{bad:?}");
        assert!(
            matches!(
                db.add_note("ws", bad, "x", "user", &LinearOwner::default()),
                Err(Error::BadRequest(_))
            ),
            "{bad:?}"
        );
    }
    for good in ["WEB-1", "A1-99", "0b9f5a52-1c3e-4a7b-9d0e-2f6c8a1b3d4e"] {
        assert!(is_issue_identifier(good), "{good}");
    }
}

fn claims() -> ActivityClaims {
    ActivityClaims {
        herdr_socket: Some("/tmp/herdr.sock".into()),
        herdr_pane_id: Some("p-1".into()),
        herdr_workspace_id: Some("ws-1".into()),
        card_id: Some(3),
        run_id: None,
    }
}

#[test]
fn activity_keeps_only_the_newest_rows_per_space() {
    let db = mem();
    let claims = claims();
    db.record_activity(&NewActivity {
        space: Some("other"),
        tool_name: "mcp__linear__save_issue",
        issue: Some("WEB-1"),
        claims: &claims,
    })
    .unwrap();
    let first = db
        .record_activity(&NewActivity {
            space: Some("ws-1"),
            tool_name: "mcp__linear__save_issue",
            issue: Some("WEB-1"),
            claims: &claims,
        })
        .unwrap();
    assert_eq!(first.tool_name, "mcp__linear__save_issue");
    assert_eq!(first.claims, claims);
    for n in 0..LINEAR_ACTIVITY_KEEP_PER_SPACE {
        db.record_activity(&NewActivity {
            space: Some("ws-1"),
            tool_name: "mcp__claude_ai_Linear__save_comment",
            issue: Some(&format!("WEB-{}", n + 2)),
            claims: &ActivityClaims::default(),
        })
        .unwrap();
    }
    let rows = db
        .list_activity(Some("ws-1"), LINEAR_ACTIVITY_KEEP_PER_SPACE + 10)
        .unwrap();
    assert_eq!(rows.len(), LINEAR_ACTIVITY_KEEP_PER_SPACE);
    assert!(
        rows.iter().all(|row| row.id != first.id),
        "the oldest row is pruned"
    );
    assert_eq!(
        rows[0].issue.as_deref(),
        Some(format!("WEB-{}", LINEAR_ACTIVITY_KEEP_PER_SPACE + 1).as_str())
    );
    assert_eq!(
        db.list_activity(Some("other"), 10).unwrap().len(),
        1,
        "another space is untouched"
    );
}

#[test]
fn activity_refuses_a_bad_identifier_or_tool_name() {
    let db = mem();
    let claims = ActivityClaims::default();
    let bad_issue = db.record_activity(&NewActivity {
        space: None,
        tool_name: "mcp__linear__save_issue",
        issue: Some("not an issue"),
        claims: &claims,
    });
    assert!(matches!(bad_issue, Err(Error::BadRequest(_))));
    let bad_tool = db.record_activity(&NewActivity {
        space: None,
        tool_name: "save issue; rm",
        issue: None,
        claims: &claims,
    });
    assert!(matches!(bad_tool, Err(Error::BadRequest(_))));
    let unattributed = db
        .record_activity(&NewActivity {
            space: None,
            tool_name: "mcp__linear__create_comment",
            issue: None,
            claims: &claims,
        })
        .unwrap();
    assert_eq!(db.list_activity(None, 5).unwrap(), vec![unattributed]);
}

#[test]
fn board_panes_are_keyed_by_socket_and_pane() {
    let db = mem();
    let pane = |socket: &'static str, pane_id: &'static str, context: &'static str| NewBoardPane {
        herdr_socket: socket,
        pane_id,
        context_key: context,
        placement: BoardPanePlacement::Split,
        workspace_id: Some("ws-1"),
        origin_pane_id: Some("p-origin"),
    };
    db.record_board_pane(&pane("/tmp/a.sock", "p-1", "issue:WEB-1"))
        .unwrap();
    db.record_board_pane(&pane("/tmp/b.sock", "p-1", "issue:WEB-2"))
        .unwrap();
    let found = db
        .board_pane_for_context("/tmp/a.sock", "issue:WEB-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        (found.herdr_socket.as_str(), found.pane_id.as_str()),
        ("/tmp/a.sock", "p-1")
    );
    assert_eq!(found.placement, BoardPanePlacement::Split);
    assert!(db
        .board_pane_for_context("/tmp/a.sock", "issue:WEB-2")
        .unwrap()
        .is_none());
    db.record_board_pane(&pane("/tmp/a.sock", "p-1", "issue:WEB-3"))
        .unwrap();
    assert_eq!(
        db.list_board_panes("/tmp/a.sock").unwrap().len(),
        1,
        "same key replaces"
    );
    assert!(db.remove_board_pane("/tmp/a.sock", "p-1").unwrap());
    assert_eq!(db.list_board_panes("/tmp/b.sock").unwrap().len(), 1);
}

#[test]
fn bindings_scopes_and_repos_round_trip() {
    let db = mem();
    let space = SpaceBinding {
        herdr_session: "default".into(),
        space: "ws-1".into(),
        project_id: "proj-uuid".into(),
        display_name: Some("Board".into()),
        team_ids: vec!["team-1".into()],
        view: Some(json!({"id": "v1", "name": "Mine"})),
    };
    db.set_space_binding(&space).unwrap();
    assert_eq!(
        db.space_binding("default", "ws-1").unwrap(),
        Some(space.clone())
    );
    assert_eq!(db.list_space_bindings().unwrap(), vec![space]);
    assert!(db.remove_space_binding("default", "ws-1").unwrap());

    let worktree = WorktreeBinding {
        worktree_path: "/repo/worktrees/feature".into(),
        issue: "WEB-12".into(),
        state: WorktreeBindingState::Bound,
        branch: Some("feature/x".into()),
        tab: None,
        display_name: None,
        team_ids: vec![],
        view: None,
        carried: serde_json::Map::new(),
    };
    db.set_worktree_binding(&worktree).unwrap();
    assert_eq!(
        db.worktree_binding("/repo/worktrees/feature").unwrap(),
        Some(worktree.clone())
    );
    let relative = WorktreeBinding {
        worktree_path: "repo/x".into(),
        ..worktree.clone()
    };
    assert!(matches!(
        db.set_worktree_binding(&relative),
        Err(Error::BadRequest(_))
    ));
    assert!(db
        .remove_worktree_binding("/repo/worktrees/feature")
        .unwrap());

    let scope = SessionScope {
        session_id: "sess-1".into(),
        team_id: "team-1".into(),
        team_key: Some("WEB".into()),
    };
    db.set_session_scope(&scope).unwrap();
    assert_eq!(db.session_scope("sess-1").unwrap(), Some(scope));

    db.add_scope_repo("project-p1.team-t1", "/repo/b").unwrap();
    db.add_scope_repo("project-p1.team-t1", "/repo/a").unwrap();
    db.add_scope_repo("project-p1.team-t1", "/repo/b").unwrap();
    assert_eq!(
        db.scope_repos("project-p1.team-t1").unwrap(),
        ["/repo/b", "/repo/a"]
    );
    assert!(matches!(
        db.add_scope_repo("bogus", "/repo/a"),
        Err(Error::BadRequest(_))
    ));
    assert!(db
        .remove_scope_repo("project-p1.team-t1", "/repo/b")
        .unwrap());
    assert_eq!(db.scope_repos("project-p1.team-t1").unwrap(), ["/repo/a"]);
}
