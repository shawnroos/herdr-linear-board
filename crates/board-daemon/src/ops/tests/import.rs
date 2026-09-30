//! `linear.import`: the work plugin's store copied into local state, insert
//! only, against a synthetic copy of `tests/fixtures/work-store`.

use super::*;

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use board_core::db::{GroupingConfig, SessionScope, SpaceBinding, WorktreeBinding};
use board_core::protocol::{LinearImportItem, LinearImportKind, LinearImportResult};

const BINDING_A: &str = "bindings/0a1b2c3d4e5f6071.json";
const BINDING_B: &str = "bindings/1b2c3d4e5f607182.json";
const SCOPE_KEY: &str = "project-project-1.team-team-1";

struct Fixture {
    d: Arc<Daemon>,
    rx: broadcast::Receiver<Event>,
    _dir: tempfile::TempDir,
    store: PathBuf,
}

/// Git keeps neither file modes nor ownership, and the sandbox mounts the
/// repository read-only under another uid, so the trust rule would refuse
/// the checked-in fixture itself. Every test imports a private copy.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o700)).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

impl Fixture {
    fn new() -> Fixture {
        let d = test_daemon(Config::default());
        let rx = d.events_tx.subscribe();
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("work");
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/work-store"),
            &store,
        );
        Fixture {
            d,
            rx,
            _dir: dir,
            store,
        }
    }

    fn import(&self, dry_run: bool) -> LinearImportResult {
        let value = crate::ops::linear_import_at(&self.d, &self.store, dry_run)
            .unwrap_or_else(|e| panic!("import failed: {e}"));
        serde_json::from_value(value).unwrap()
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.store.join(rel);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn chmod(&self, rel: &str, mode: u32) {
        std::fs::set_permissions(self.store.join(rel), std::fs::Permissions::from_mode(mode))
            .unwrap();
    }

    fn rows(&self) -> Rows {
        let db = self.d.store.lock();
        Rows {
            grouping: db.grouping_config().unwrap(),
            spaces: db.list_space_bindings().unwrap(),
            worktrees: db.list_worktree_bindings().unwrap(),
            session: db.session_scope("alpha").unwrap(),
            repos: db.scope_repos(SCOPE_KEY).unwrap(),
        }
    }

    fn events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

#[derive(Debug, PartialEq)]
struct Rows {
    grouping: Option<GroupingConfig>,
    spaces: Vec<SpaceBinding>,
    worktrees: Vec<WorktreeBinding>,
    session: Option<SessionScope>,
    repos: Vec<String>,
}

impl Rows {
    fn is_empty(&self) -> bool {
        self.grouping.is_none()
            && self.spaces.is_empty()
            && self.worktrees.is_empty()
            && self.session.is_none()
            && self.repos.is_empty()
    }
}

fn keys(items: &[LinearImportItem], kind: LinearImportKind) -> BTreeSet<String> {
    items
        .iter()
        .filter(|i| i.kind == kind)
        .map(|i| i.key.clone())
        .collect()
}

fn find<'a>(items: &'a [LinearImportItem], source: &str) -> &'a LinearImportItem {
    items
        .iter()
        .find(|i| i.source == source)
        .unwrap_or_else(|| panic!("no item from {source}: {items:#?}"))
}

fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|v| (*v).to_string()).collect()
}

#[test]
fn the_fixture_store_imports_every_record_kind() {
    let fx = Fixture::new();
    let result = fx.import(false);
    assert!(result.present);
    assert!(!result.dry_run);
    let imported = &result.imported;
    assert_eq!(
        keys(imported, LinearImportKind::Grouping),
        set(&["global", "space alpha", "space zeta"])
    );
    assert_eq!(
        keys(imported, LinearImportKind::SpaceBinding),
        set(&["alpha/wF", "default/wL"])
    );
    assert_eq!(
        keys(imported, LinearImportKind::WorktreeBinding),
        set(&["/tmp/hb-import/wt-a", "/tmp/hb-import/wt-b"])
    );
    assert_eq!(
        keys(imported, LinearImportKind::SessionScope),
        set(&["alpha"])
    );
    assert_eq!(
        keys(imported, LinearImportKind::ScopeRepo),
        set(&[
            "project-project-1.team-team-1 /tmp/hb-import/repo-a",
            "project-project-1.team-team-1 /tmp/hb-import/repo-b"
        ])
    );

    let rows = fx.rows();
    assert_eq!(
        rows.session,
        Some(SessionScope {
            session_id: "alpha".into(),
            team_id: "team-1".into(),
            team_key: Some("WEB".into()),
        })
    );
    assert_eq!(
        rows.repos,
        vec!["/tmp/hb-import/repo-a", "/tmp/hb-import/repo-b"]
    );
    let b = rows
        .worktrees
        .iter()
        .find(|w| w.worktree_path == "/tmp/hb-import/wt-b")
        .unwrap();
    assert_eq!(b.issue, "WEB-102");
    assert_eq!(b.state.as_str(), "misplaced");
    assert_eq!(b.branch.as_deref(), Some("feature/web-102"));
    assert_eq!(b.tab.as_deref(), Some("WEB-102"));
    assert_eq!(b.view.as_ref().unwrap()["id"], "view-1");
}

#[test]
fn importing_the_fixture_store_twice_yields_identical_rows() {
    let fx = Fixture::new();
    let first = fx.import(false);
    let after_first = fx.rows();
    assert!(!after_first.is_empty());

    let second = fx.import(false);
    assert_eq!(fx.rows(), after_first);
    assert!(second.imported.is_empty(), "{:#?}", second.imported);
    let first_keys: BTreeSet<_> = first.imported.iter().map(|i| (i.kind, &i.key)).collect();
    let skipped_existing: BTreeSet<_> = second
        .skipped
        .iter()
        .filter(|i| i.reason.as_deref().is_some_and(|r| r.contains("already")))
        .map(|i| (i.kind, &i.key))
        .collect();
    assert_eq!(skipped_existing, first_keys);
}

#[test]
fn a_row_edited_in_the_board_after_the_first_import_is_kept_and_reported_skipped() {
    let fx = Fixture::new();
    fx.import(false);
    fx.d.store
        .lock()
        .set_space_binding(&SpaceBinding {
            herdr_session: "alpha".into(),
            space: "wF".into(),
            project_id: "project-edited".into(),
            display_name: None,
            team_ids: vec![],
            view: None,
        })
        .unwrap();

    let second = fx.import(false);
    let binding =
        fx.d.store
            .lock()
            .space_binding("alpha", "wF")
            .unwrap()
            .unwrap();
    assert_eq!(binding.project_id, "project-edited");
    let item = find(&second.skipped, "workspaces/alpha/wF.json");
    assert_eq!(item.kind, LinearImportKind::SpaceBinding);
    assert_eq!(item.key, "alpha/wF");
    assert!(
        item.reason.as_deref().unwrap().contains("already"),
        "{item:?}"
    );
}

#[test]
fn a_binding_with_consent_and_proposal_fields_imports_without_them() {
    let fx = Fixture::new();
    let result = fx.import(false);
    let item = find(&result.imported, BINDING_A);
    for field in ["consent", "declined", "proposal"] {
        assert!(item.dropped.iter().any(|d| d == field), "{item:?}");
    }
    assert!(
        !item.dropped.iter().any(|d| d == "pending_consent"),
        "a null retiring field is not reported: {item:?}"
    );

    let binding =
        fx.d.store
            .lock()
            .worktree_binding("/tmp/hb-import/wt-a")
            .unwrap()
            .unwrap();
    assert_eq!(binding.issue, "WEB-101");
    assert_eq!(binding.state.as_str(), "bound");
    assert_eq!(binding.branch.as_deref(), Some("feature/web-101"));
    assert_eq!(binding.tab, None);
    assert_eq!(binding.team_ids, vec!["team-1"]);
    for retired in [
        "proposal",
        "consent",
        "consent_proposal",
        "pending_consent",
        "pending_placement",
        "declined",
        "pending_judgment",
    ] {
        assert!(
            !binding.carried.contains_key(retired),
            "{retired} was carried: {:?}",
            binding.carried
        );
    }
    assert_eq!(binding.carried["created_children"], json!(["WEB-150"]));
    assert_eq!(binding.carried["description_head"], "abc123");
    assert!(!binding.carried.contains_key("version"));
    assert!(!binding.carried.contains_key("worktree_path"));
}

#[test]
fn a_group_writable_record_is_skipped_and_reported_not_imported() {
    let fx = Fixture::new();
    fx.chmod(BINDING_B, 0o660);
    let result = fx.import(false);
    let item = find(&result.skipped, BINDING_B);
    assert_eq!(item.kind, LinearImportKind::WorktreeBinding);
    assert!(
        item.reason.as_deref().unwrap().contains("group"),
        "{item:?}"
    );
    assert!(fx
        .d
        .store
        .lock()
        .worktree_binding("/tmp/hb-import/wt-b")
        .unwrap()
        .is_none());
}

#[test]
fn an_other_writable_board_config_is_skipped_whole() {
    let fx = Fixture::new();
    fx.chmod("board.json", 0o602);
    let result = fx.import(false);
    let item = find(&result.skipped, "board.json");
    assert!(
        item.reason.as_deref().unwrap().contains("other"),
        "{item:?}"
    );
    assert!(fx.rows().grouping.is_none());
}

#[test]
fn a_symlinked_record_is_skipped_and_reported() {
    let fx = Fixture::new();
    std::fs::remove_file(fx.store.join(BINDING_B)).unwrap();
    std::os::unix::fs::symlink(fx.store.join(BINDING_A), fx.store.join(BINDING_B)).unwrap();
    let result = fx.import(false);
    let item = find(&result.skipped, BINDING_B);
    assert!(
        item.reason.as_deref().unwrap().contains("regular file"),
        "{item:?}"
    );
}

#[test]
fn a_record_newer_than_version_1_is_skipped_and_reported() {
    let fx = Fixture::new();
    fx.write(
        "bindings/3d4e5f6071829304.json",
        r#"{"version": 2, "worktree_path": "/tmp/hb-import/wt-d", "state": "bound", "issue_identifier": "WEB-104"}"#,
    );
    let result = fx.import(false);
    let item = find(&result.skipped, "bindings/3d4e5f6071829304.json");
    assert!(
        item.reason.as_deref().unwrap().contains("version 2"),
        "{item:?}"
    );
    assert!(fx
        .d
        .store
        .lock()
        .worktree_binding("/tmp/hb-import/wt-d")
        .unwrap()
        .is_none());
}

#[test]
fn proposed_records_are_skipped_with_their_state_named() {
    let fx = Fixture::new();
    let result = fx.import(false);
    for source in ["bindings/2c3d4e5f60718293.json", "workspaces/alpha/wP.json"] {
        let item = find(&result.skipped, source);
        assert!(
            item.reason.as_deref().unwrap().contains("proposed"),
            "{item:?}"
        );
    }
}

#[test]
fn tab_by_parent_and_column_by_ticket_imports_with_columns_ungrouped() {
    let fx = Fixture::new();
    fx.import(false);
    let config = fx.rows().grouping.expect("grouping imported");
    assert_eq!(
        config.global.levels,
        [("column", "ticket"), ("tab", "parent")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    );
    assert_eq!(config.global.filter["assignee"], json!(["me"]));
    let order: Vec<&str> = config.spaces.iter().map(|s| s.space.as_str()).collect();
    assert_eq!(order, ["zeta", "alpha"], "board.json order is kept");
    assert_eq!(config.spaces[1].mapping.levels["column"], "sub-ticket");
}

#[test]
fn a_board_config_with_a_duplicate_key_is_skipped_whole() {
    let fx = Fixture::new();
    fx.write(
        "board.json",
        r#"{"version": 1, "global": {"levels": {"tab": "parent", "tab": "state"}, "filter": {"assignee": "me"}}}"#,
    );
    let result = fx.import(false);
    let item = find(&result.skipped, "board.json");
    assert!(
        item.reason.as_deref().unwrap().contains("duplicate"),
        "{item:?}"
    );
    assert!(fx.rows().grouping.is_none());
}

#[test]
fn a_board_config_that_fails_validation_is_skipped_whole() {
    let fx = Fixture::new();
    fx.write(
        "board.json",
        r#"{"version": 1, "global": {"levels": {"tab": "parent"}, "filter": {"assignee": "me"}},
            "spaces": {"ok": {"levels": {"tab": "state"}, "filter": {"team": "t"}},
                       "bad": {"levels": {"tab": "state", "row": "state"}, "filter": {"team": "t"}}}}"#,
    );
    let result = fx.import(false);
    let item = find(&result.skipped, "board.json");
    assert!(
        item.reason.as_deref().unwrap().contains("same kind"),
        "{item:?}"
    );
    assert!(fx.rows().grouping.is_none());
}

#[test]
fn a_flat_and_a_session_keyed_record_for_one_workspace_import_as_one_row() {
    let fx = Fixture::new();
    let result = fx.import(false);
    let spaces: Vec<(String, String, String)> = fx
        .rows()
        .spaces
        .into_iter()
        .map(|b| (b.herdr_session, b.space, b.project_id))
        .collect();
    assert_eq!(
        spaces,
        vec![
            ("alpha".into(), "wF".into(), "project-session".into()),
            ("default".into(), "wL".into(), "project-legacy".into()),
        ]
    );
    let flat = find(&result.skipped, "workspaces/wF.json");
    assert!(
        flat.reason.as_deref().unwrap().contains("session-keyed"),
        "{flat:?}"
    );
}

#[test]
fn a_dry_run_writes_nothing_and_lists_what_it_would_import() {
    let mut fx = Fixture::new();
    let dry = fx.import(true);
    assert!(dry.dry_run);
    assert!(fx.rows().is_empty(), "{:?}", fx.rows());
    assert!(fx.events().is_empty());

    let real = fx.import(false);
    let plan: BTreeSet<_> = dry.imported.iter().map(|i| (i.kind, &i.key)).collect();
    let done: BTreeSet<_> = real.imported.iter().map(|i| (i.kind, &i.key)).collect();
    assert_eq!(plan, done);
    assert!(!done.is_empty());
}

#[test]
fn a_missing_store_reports_nothing_to_import() {
    let fx = Fixture::new();
    let missing = fx.store.join("absent");
    let value = crate::ops::linear_import_at(&fx.d, &missing, false).unwrap();
    let result: LinearImportResult = serde_json::from_value(value).unwrap();
    assert!(!result.present);
    assert!(result.imported.is_empty());
    assert!(result.skipped.is_empty());
    assert!(result.ignored.is_empty());
    assert_eq!(result.store_dir, missing.to_str().unwrap());
}

#[test]
fn retired_store_entries_are_ignored_and_named() {
    let fx = Fixture::new();
    let result = fx.import(false);
    let ignored: BTreeSet<String> = result.ignored.iter().map(|i| i.path.clone()).collect();
    for path in [
        "board",
        "descriptions",
        "layouts",
        "sessions",
        "shadow.log",
        "write-enabled",
    ] {
        assert!(ignored.contains(path), "{path} not ignored: {ignored:?}");
    }
    assert!(result.ignored.iter().all(|i| !i.reason.is_empty()));
}

#[test]
fn a_real_import_announces_each_affected_space_once_and_a_rerun_announces_nothing() {
    let mut fx = Fixture::new();
    fx.import(false);
    let spaces: Vec<Option<String>> = fx
        .events()
        .into_iter()
        .map(|ev| match ev {
            Event::LocalStateChanged { space } => space,
            other => panic!("unexpected event {other:?}"),
        })
        .collect();
    let distinct: BTreeSet<_> = spaces.iter().cloned().collect();
    assert_eq!(spaces.len(), distinct.len(), "{spaces:?}");
    assert_eq!(
        distinct,
        [None, Some("alpha"), Some("wF"), Some("wL"), Some("zeta")]
            .into_iter()
            .map(|s| s.map(str::to_string))
            .collect()
    );

    fx.import(false);
    assert!(fx.events().is_empty());
}
