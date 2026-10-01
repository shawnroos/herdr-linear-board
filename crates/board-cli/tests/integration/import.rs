//! `board import work-store`: the CLI sends one `linear.import` request and
//! boardd reads the store named in its own environment and writes the rows.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use board_core::client::BoardClient;
use board_core::protocol::LinearStateGetParams;

use super::{json_output, TestDaemon};

/// A one-record store: a flat space record binding `wA` to `project`.
fn store(dir: &Path, project: &str) {
    let workspaces = dir.join("workspaces");
    std::fs::create_dir_all(&workspaces).unwrap();
    let record = workspaces.join("wA.json");
    std::fs::write(
        &record,
        serde_json::json!({
            "version": 1,
            "worktree_path": "workspace:wA",
            "state": "bound",
            "issue_identifier": project,
        })
        .to_string(),
    )
    .unwrap();
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn space_projects(td: &TestDaemon) -> Vec<(String, String)> {
    td.client()
        .linear_state_get(&LinearStateGetParams {
            space: "wA".into(),
            herdr_socket: None,
        })
        .unwrap()
        .space_bindings
        .into_iter()
        .map(|b| (b.herdr_session, b.project_id))
        .collect()
}

#[test]
fn the_cli_only_sends_the_request_and_the_daemon_imports_its_own_store() {
    let daemon_store = tempfile::tempdir().unwrap();
    store(daemon_store.path(), "project-daemon");
    let cli_store = tempfile::tempdir().unwrap();
    store(cli_store.path(), "project-cli");
    let cli_home = tempfile::tempdir().unwrap();
    let cli_db = cli_home.path().join("cli.db");
    let td = TestDaemon::start(&[(
        "HERDR_LINEAR_STORE_DIR",
        daemon_store.path().to_str().unwrap(),
    )]);
    let cli_env = [
        ("HERDR_LINEAR_STORE_DIR", cli_store.path().to_str().unwrap()),
        ("BOARD_DB", cli_db.to_str().unwrap()),
    ];

    let dry =
        json_output(&td.board_with_env(&["import", "work-store", "--dry-run", "--json"], &cli_env));
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["imported"][0]["key"], "default/wA");
    assert!(space_projects(&td).is_empty(), "a dry run wrote rows");

    let real = json_output(&td.board_with_env(&["import", "work-store", "--json"], &cli_env));
    assert_eq!(real["store_dir"], daemon_store.path().to_str().unwrap());
    assert_eq!(real["imported"][0]["key"], "default/wA");
    assert_eq!(
        space_projects(&td),
        vec![("default".to_string(), "project-daemon".to_string())]
    );
    assert!(
        !cli_db.exists(),
        "the CLI opened its own database instead of asking the daemon"
    );

    let again = json_output(&td.board_with_env(&["import", "work-store", "--json"], &cli_env));
    assert_eq!(again["imported"], serde_json::json!([]));
    assert_eq!(again["skipped"][0]["key"], "default/wA");
}

#[test]
fn a_missing_store_reports_nothing_to_import_and_exits_zero() {
    let home = tempfile::tempdir().unwrap();
    let absent = home.path().join("absent");
    let td = TestDaemon::start(&[("HERDR_LINEAR_STORE_DIR", absent.to_str().unwrap())]);
    let out = td.board(&["import", "work-store"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("nothing to import"), "{text}");
}
