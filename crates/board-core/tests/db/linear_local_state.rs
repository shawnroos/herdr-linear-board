use super::mem;
use board_core::db::{
    Db, LinearOwner, LocalStateError, LocalStateRejection, MarkKind, NewMark, ShowOutcome,
    SpaceBinding,
};
use board_core::protocol::{
    parse_timestamp, LinearBindParams, LinearMarkSetParams, LinearMarkUnmarkParams,
    LinearSessionGetParams, LinearShowRequestParams, LinearShowWithdrawParams,
};
use serde_json::json;

const SPACE: &str = "ws-1";
const TTL: i64 = 30 * 60;
const SOCKET: &str = "/tmp/hb/sessions/main/herdr.sock";

fn now() -> i64 {
    parse_timestamp("2026-09-30 12:00:00").unwrap()
}

fn owner(pane: &str) -> LinearOwner {
    LinearOwner {
        herdr_socket: Some(SOCKET.into()),
        herdr_pane_id: Some(pane.into()),
        claude_session_id: Some(format!("claude-{pane}")),
    }
}

fn rejection(error: LocalStateError) -> LocalStateRejection {
    match error {
        LocalStateError::Rejected(rejection) => rejection,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

fn mark(db: &Db, who: &LinearOwner, issue: &str, kind: MarkKind, text: &str) -> i64 {
    db.linear_mark_set(
        &LinearMarkSetParams {
            space: SPACE.into(),
            issue: issue.into(),
            kind,
            text: Some(text.into()),
            created_by: None,
            owner: who.clone(),
        },
        true,
    )
    .unwrap()
    .after
    .id
}

fn unmark(who: &LinearOwner, issue: &str, kind: Option<MarkKind>) -> LinearMarkUnmarkParams {
    LinearMarkUnmarkParams {
        space: SPACE.into(),
        issue: issue.into(),
        kind,
        owner: who.clone(),
    }
}

fn ask(who: &LinearOwner, issue: &str, reason: &str) -> LinearShowRequestParams {
    LinearShowRequestParams {
        space: SPACE.into(),
        issue: issue.into(),
        reason: Some(reason.into()),
        requested_by: None,
        owner: who.clone(),
    }
}

fn withdraw(who: &LinearOwner, issue: &str) -> LinearShowWithdrawParams {
    LinearShowWithdrawParams {
        space: SPACE.into(),
        issue: issue.into(),
        owner: who.clone(),
    }
}

#[test]
fn ae9_two_owners_hold_their_own_needs_you_and_unmark_removes_only_the_callers() {
    let db = mem();
    let (a, b) = (owner("p-a"), owner("p-b"));
    mark(&db, &a, "ENG-148", MarkKind::Attention, "from a");
    let b_mark = mark(&db, &b, "ENG-148", MarkKind::Attention, "from b");
    assert_eq!(db.list_marks(SPACE).unwrap().len(), 2);

    let removed = db
        .linear_mark_unmark(&unmark(&a, "ENG-148", None))
        .unwrap()
        .removed;
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].owner(), a);
    let left = db.list_marks(SPACE).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, b_mark);
    assert_eq!(left[0].owner_herdr_pane_id.as_deref(), Some("p-b"));
}

#[test]
fn one_owner_setting_a_kind_twice_keeps_one_mark_with_the_new_text() {
    let db = mem();
    let a = owner("p-a");
    mark(&db, &a, "ENG-148", MarkKind::Attention, "first");
    mark(&db, &a, "ENG-148", MarkKind::Attention, "second");
    let marks = db.list_marks(SPACE).unwrap();
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].text.as_deref(), Some("second"));
}

#[test]
fn owners_differing_only_in_claude_session_are_different_owners() {
    let db = mem();
    let a = owner("p-a");
    let nested = LinearOwner {
        claude_session_id: Some("claude-nested".into()),
        ..a.clone()
    };
    mark(&db, &a, "ENG-148", MarkKind::Question, "a");
    mark(&db, &nested, "ENG-148", MarkKind::Question, "nested");
    assert_eq!(db.list_marks(SPACE).unwrap().len(), 2);
}

#[test]
fn unmark_with_a_kind_leaves_the_callers_other_kinds() {
    let db = mem();
    let a = owner("p-a");
    mark(&db, &a, "ENG-148", MarkKind::Attention, "look");
    mark(&db, &a, "ENG-148", MarkKind::Question, "which?");
    mark(&db, &a, "ENG-149", MarkKind::Question, "other issue");
    let removed = db
        .linear_mark_unmark(&unmark(&a, "ENG-148", Some(MarkKind::Question)))
        .unwrap()
        .removed;
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].kind, MarkKind::Question);
    let kinds: Vec<(String, MarkKind)> = db
        .list_marks(SPACE)
        .unwrap()
        .into_iter()
        .map(|m| (m.issue, m.kind))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("ENG-148".into(), MarkKind::Attention),
            ("ENG-149".into(), MarkKind::Question)
        ]
    );
}

#[test]
fn an_anonymous_caller_cannot_unmark_or_withdraw() {
    let db = mem();
    mark(
        &db,
        &LinearOwner::default(),
        "ENG-148",
        MarkKind::Attention,
        "tui",
    );
    let blank = LinearOwner {
        herdr_socket: Some(String::new()),
        ..LinearOwner::default()
    };
    for who in [LinearOwner::default(), blank] {
        let refused = rejection(
            db.linear_mark_unmark(&unmark(&who, "ENG-148", None))
                .unwrap_err(),
        );
        assert!(
            matches!(&refused, LocalStateRejection::Refused(m) if m.contains("cleared on the board")),
            "{refused:?}"
        );
        let refused = rejection(
            db.linear_show_withdraw(&withdraw(&who, "ENG-148"), now())
                .unwrap_err(),
        );
        assert!(matches!(refused, LocalStateRejection::Refused(_)));
    }
    assert_eq!(db.list_marks(SPACE).unwrap().len(), 1);
}

#[test]
fn the_generic_mark_path_refuses_a_suggestion() {
    let db = mem();
    let refused = rejection(
        db.linear_mark_set(
            &LinearMarkSetParams {
                space: SPACE.into(),
                issue: "ENG-148".into(),
                kind: MarkKind::Suggestion,
                text: None,
                created_by: None,
                owner: owner("p-a"),
            },
            true,
        )
        .unwrap_err(),
    );
    assert!(
        matches!(&refused, LocalStateRejection::Refused(m) if m.contains("suggestion")),
        "{refused:?}"
    );
    assert!(db.list_marks(SPACE).unwrap().is_empty());
}

#[test]
fn ae11_a_re_ask_refreshes_the_pending_request_and_withdraw_closes_it() {
    let db = mem();
    let a = owner("p-a");
    let first = db
        .linear_show_request(&ask(&a, "ENG-160", "look"), true, now(), TTL)
        .unwrap()
        .after
        .unwrap();
    let again = db
        .linear_show_request(&ask(&a, "ENG-160", "look again"), true, now() + 600, TTL)
        .unwrap();
    assert_eq!(again.before.as_ref().map(|r| r.id), Some(first.id));
    let refreshed = again.after.unwrap();
    assert_eq!(refreshed.id, first.id);
    assert_eq!(refreshed.created_at, first.created_at);
    assert_eq!(refreshed.reason.as_deref(), Some("look again"));
    let expiry =
        |r: &board_core::db::ShowRequest| parse_timestamp(r.expires_at.as_deref().unwrap());
    assert_eq!(expiry(&first), Some(now() + TTL));
    assert_eq!(expiry(&refreshed), Some(now() + 600 + TTL));
    assert_eq!(
        db.pending_show_requests(SPACE, now() + 600).unwrap().len(),
        1
    );

    let withdrawn = db
        .linear_show_withdraw(&withdraw(&a, "ENG-160"), now() + 600)
        .unwrap()
        .after
        .unwrap();
    assert_eq!(withdrawn.outcome, Some(ShowOutcome::Withdrawn));
    assert!(withdrawn.acknowledged_at.is_some());
    assert!(db
        .pending_show_requests(SPACE, now() + 600)
        .unwrap()
        .is_empty());
}

#[test]
fn another_owner_asking_for_the_same_issue_adds_its_own_request() {
    let db = mem();
    db.linear_show_request(&ask(&owner("p-a"), "ENG-160", "a"), true, now(), TTL)
        .unwrap();
    db.linear_show_request(&ask(&owner("p-b"), "ENG-160", "b"), true, now(), TTL)
        .unwrap();
    assert_eq!(db.pending_show_requests(SPACE, now()).unwrap().len(), 2);
}

#[test]
fn accept_dismiss_and_withdraw_each_record_their_outcome_and_refuse_a_second_answer() {
    let db = mem();
    let a = owner("p-a");
    let id = |issue: &str| {
        db.linear_show_request(&ask(&a, issue, "r"), true, now(), TTL)
            .unwrap()
            .after
            .unwrap()
            .id
    };
    let (accepted, rejected) = (id("ENG-1"), id("ENG-2"));
    id("ENG-3");
    let outcome = |change: board_core::protocol::LinearChange<board_core::db::ShowRequest>| {
        change.after.unwrap().outcome
    };
    assert_eq!(
        outcome(db.linear_show_accept(accepted, now()).unwrap()),
        Some(ShowOutcome::Accepted)
    );
    assert_eq!(
        outcome(db.linear_show_dismiss(rejected, now()).unwrap()),
        Some(ShowOutcome::Rejected)
    );
    assert_eq!(
        outcome(
            db.linear_show_withdraw(&withdraw(&a, "ENG-3"), now())
                .unwrap()
        ),
        Some(ShowOutcome::Withdrawn)
    );

    let refused = rejection(db.linear_show_dismiss(accepted, now()).unwrap_err());
    assert_eq!(
        refused,
        LocalStateRejection::ShowRequestAnswered {
            id: accepted,
            outcome: Some(ShowOutcome::Accepted)
        }
    );
    assert!(refused.to_string().contains("accepted"), "{refused}");
    let refused = rejection(db.linear_show_accept(rejected, now()).unwrap_err());
    assert!(refused.to_string().contains("rejected"), "{refused}");
    assert!(matches!(
        rejection(db.linear_show_accept(9_999, now()).unwrap_err()),
        LocalStateRejection::Missing(_)
    ));
}

#[test]
fn owner_b_cannot_withdraw_owner_as_request() {
    let db = mem();
    let request = db
        .linear_show_request(&ask(&owner("p-a"), "ENG-160", "a"), true, now(), TTL)
        .unwrap()
        .after
        .unwrap();
    let refused = rejection(
        db.linear_show_withdraw(&withdraw(&owner("p-b"), "ENG-160"), now())
            .unwrap_err(),
    );
    assert!(
        matches!(refused, LocalStateRejection::Missing(_)),
        "{refused:?}"
    );
    assert_eq!(
        db.pending_show_requests(SPACE, now()).unwrap(),
        vec![request]
    );
}

#[test]
fn ae5_an_overdue_request_is_not_pending_and_the_sweep_marks_it_expired() {
    let db = mem();
    let request = db
        .linear_show_request(&ask(&owner("p-a"), "ENG-160", "a"), true, now(), TTL)
        .unwrap()
        .after
        .unwrap();
    db.linear_show_request(&ask(&owner("p-a"), "ENG-161", "a"), true, now() + 60, TTL)
        .unwrap();
    let later = now() + TTL;
    let pending: Vec<String> = db
        .pending_show_requests(SPACE, later)
        .unwrap()
        .into_iter()
        .map(|r| r.issue)
        .collect();
    assert_eq!(pending, vec!["ENG-161".to_string()]);

    let refused = rejection(db.linear_show_accept(request.id, later).unwrap_err());
    assert!(refused.to_string().contains("expired"), "{refused}");

    assert_eq!(db.expire_overdue(later).unwrap(), vec![SPACE.to_string()]);
    let expired = db.show_request(request.id).unwrap().unwrap();
    assert_eq!(expired.outcome, Some(ShowOutcome::Expired));
    assert!(expired.acknowledged_at.is_some());
    assert!(db.expire_overdue(later).unwrap().is_empty());
    assert_eq!(db.pending_show_requests(SPACE, later).unwrap().len(), 1);
}

#[test]
fn bulk_clear_removes_only_the_given_ids_and_skips_ids_already_gone() {
    let db = mem();
    let a = owner("p-a");
    let one = mark(&db, &a, "ENG-1", MarkKind::Attention, "1");
    let two = mark(&db, &a, "ENG-2", MarkKind::Question, "2");
    let keep = mark(&db, &a, "ENG-3", MarkKind::Done, "3");
    db.linear_mark_clear(two).unwrap();
    let removed = db
        .linear_mark_clear_ids(&[one, two, 9_999])
        .unwrap()
        .removed;
    assert_eq!(removed.iter().map(|m| m.id).collect::<Vec<_>>(), vec![one]);
    let left: Vec<i64> = db.list_marks(SPACE).unwrap().iter().map(|m| m.id).collect();
    assert_eq!(left, vec![keep]);
}

fn worktree(dir: &tempfile::TempDir, name: &str) -> String {
    let root = std::fs::canonicalize(dir.path()).unwrap().join(name);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("src/deep")).unwrap();
    root.to_str().unwrap().to_string()
}

fn suggest(db: &Db, space: &str, issue: &str, worktree: &str) {
    db.add_mark(&NewMark {
        space,
        issue,
        kind: MarkKind::Suggestion,
        text: None,
        detail: Some(json!({"worktree_path": worktree, "cwd": worktree})),
        created_by: Some("mcp__linear__save_issue"),
        owner: &LinearOwner::default(),
    })
    .unwrap();
}

fn bind(cwd: &str, issue: &str) -> LinearBindParams {
    LinearBindParams {
        cwd: cwd.into(),
        issue: issue.into(),
        ..LinearBindParams::default()
    }
}

#[test]
fn binding_another_worktree_leaves_the_suggestion() {
    let db = mem();
    let dir = tempfile::tempdir().unwrap();
    let (named, other) = (worktree(&dir, "named"), worktree(&dir, "other"));
    suggest(&db, SPACE, "ENG-153", &named);
    let bound = db.linear_bind(&bind(&other, "ENG-153")).unwrap();
    assert!(bound.cleared_suggestions.is_empty());
    assert_eq!(db.list_marks(SPACE).unwrap().len(), 1);
}

#[test]
fn binding_the_suggested_worktree_clears_every_suggestion_on_the_issue() {
    let db = mem();
    let dir = tempfile::tempdir().unwrap();
    let (named, other) = (worktree(&dir, "named"), worktree(&dir, "other"));
    suggest(&db, SPACE, "ENG-153", &named);
    suggest(&db, "ws-2", "ENG-153", &other);
    suggest(&db, SPACE, "ENG-154", &named);
    mark(&db, &owner("p-a"), "ENG-153", MarkKind::Attention, "stays");

    let bound = db.linear_bind(&bind(&named, "ENG-153")).unwrap();
    assert_eq!(bound.change.after.unwrap().issue, "ENG-153");
    let mut cleared: Vec<String> = bound
        .cleared_suggestions
        .iter()
        .map(|m| m.space.clone())
        .collect();
    cleared.sort();
    assert_eq!(cleared, vec!["ws-1".to_string(), "ws-2".to_string()]);
    let left: Vec<(String, MarkKind)> = db
        .list_marks(SPACE)
        .unwrap()
        .into_iter()
        .map(|m| (m.issue, m.kind))
        .collect();
    assert_eq!(
        left,
        vec![
            ("ENG-154".into(), MarkKind::Suggestion),
            ("ENG-153".into(), MarkKind::Attention)
        ]
    );
    assert!(db.list_marks("ws-2").unwrap().is_empty());
}

fn bind_space(db: &Db) {
    db.set_space_binding(&SpaceBinding {
        herdr_session: "main".into(),
        space: SPACE.into(),
        project_id: "proj-1".into(),
        display_name: None,
        team_ids: Vec::new(),
        view: None,
    })
    .unwrap();
}

fn session(cwd: &str, socket: &str) -> LinearSessionGetParams {
    LinearSessionGetParams {
        space: SPACE.into(),
        herdr_socket: Some(socket.into()),
        herdr_pane_id: Some("p-a".into()),
        claude_session_id: Some("claude-p-a".into()),
        cwd: Some(cwd.into()),
    }
}

#[test]
fn session_read_for_an_unknown_socket_returns_nothing() {
    let db = mem();
    bind_space(&db);
    let dir = tempfile::tempdir().unwrap();
    let wt = worktree(&dir, "wt");
    db.linear_bind(&bind(&wt, "ENG-148")).unwrap();
    for socket in ["/tmp/hb/sessions/other/herdr.sock", ""] {
        let read = db.linear_session_get(&session(&wt, socket), now()).unwrap();
        assert!(!read.space_bound, "{socket:?}");
        assert_eq!(read.binding, None);
        assert!(read.marks.is_empty());
        assert_eq!(read.pending_requests, 0);
    }
    let mut no_socket = session(&wt, SOCKET);
    no_socket.herdr_socket = None;
    assert!(
        !db.linear_session_get(&no_socket, now())
            .unwrap()
            .space_bound
    );
}

#[test]
fn session_read_for_an_unbound_worktree_returns_no_issue_but_the_pending_count() {
    let db = mem();
    bind_space(&db);
    let dir = tempfile::tempdir().unwrap();
    let wt = worktree(&dir, "wt");
    db.linear_show_request(&ask(&owner("p-b"), "ENG-160", "r"), true, now(), TTL)
        .unwrap();
    for cwd in [wt.as_str(), "/definitely/not/a/worktree", "relative"] {
        let read = db.linear_session_get(&session(cwd, SOCKET), now()).unwrap();
        assert!(read.space_bound);
        assert_eq!(read.binding, None, "{cwd}");
        assert_eq!(read.pending_requests, 1);
    }
}

#[test]
fn session_read_from_a_subdirectory_of_a_bound_worktree_returns_the_bound_issue() {
    let db = mem();
    bind_space(&db);
    let dir = tempfile::tempdir().unwrap();
    let wt = worktree(&dir, "wt");
    db.linear_bind(&bind(&wt, "ENG-148")).unwrap();
    mark(&db, &owner("p-a"), "ENG-148", MarkKind::Question, "which?");
    mark(
        &db,
        &owner("p-a"),
        "ENG-149",
        MarkKind::Attention,
        "other card",
    );
    db.linear_show_request(&ask(&owner("p-a"), "ENG-160", "r"), true, now() - TTL, TTL)
        .unwrap();
    db.linear_show_request(&ask(&owner("p-b"), "ENG-161", "r"), true, now(), TTL)
        .unwrap();

    let read = db
        .linear_session_get(&session(&format!("{wt}/src/deep"), SOCKET), now())
        .unwrap();
    let binding = read.binding.unwrap();
    assert_eq!(binding.issue, "ENG-148");
    assert_eq!(binding.worktree_path, wt);
    assert!(binding
        .bound_at
        .as_deref()
        .and_then(parse_timestamp)
        .is_some());
    assert_eq!(read.marks.len(), 1);
    assert_eq!(read.marks[0].kind, MarkKind::Question);
    assert_eq!(
        read.pending_requests, 1,
        "the overdue request does not count"
    );
    assert_eq!(read.column, None);
}
