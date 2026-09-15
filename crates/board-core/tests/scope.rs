use std::path::{Path, PathBuf};

use board_core::scope::{
    resolve_scope_path, select_scope_candidate, space_identity, space_origin, SpaceOrigin,
};

#[test]
fn candidate_precedence_uses_override_then_focused_then_workspace_then_cwd() {
    let cwd = Path::new("/current");
    let context = r#"{"focused_pane_cwd":"/focused","workspace_cwd":"/workspace"}"#;

    assert_eq!(
        select_scope_candidate(Some("/override"), Some(context), cwd).unwrap(),
        PathBuf::from("/override")
    );
    assert_eq!(
        select_scope_candidate(Some("  "), Some(context), cwd).unwrap(),
        PathBuf::from("/focused")
    );
    assert_eq!(
        select_scope_candidate(None, Some(r#"{"workspace_cwd":"/workspace"}"#), cwd).unwrap(),
        PathBuf::from("/workspace")
    );
    assert_eq!(select_scope_candidate(None, Some("{}"), cwd).unwrap(), cwd);
}

#[test]
fn malformed_plugin_context_falls_back_to_cwd() {
    let cwd = Path::new("/current");
    assert_eq!(
        select_scope_candidate(None, Some("not json"), cwd).unwrap(),
        cwd
    );
}

#[test]
fn git_subdirectory_resolves_to_canonical_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let subdir = root.join("nested/deep");
    std::fs::create_dir_all(&subdir).unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(
        resolve_scope_path(&subdir).unwrap(),
        root.canonicalize().unwrap()
    );
}

#[test]
fn non_git_directory_resolves_to_canonical_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plain");
    std::fs::create_dir(&dir).unwrap();

    assert_eq!(
        resolve_scope_path(&dir).unwrap(),
        dir.canonicalize().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn fallback_and_git_root_are_canonicalized() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    assert_eq!(
        resolve_scope_path(&link).unwrap(),
        real.canonicalize().unwrap()
    );
}

#[test]
fn space_identity_prefers_the_workspace_env_over_the_plugin_context() {
    let context = r#"{"workspace_id":"w-ctx","tab_id":"w-ctx:t1","focused_pane_id":"w-ctx:p1"}"#;
    assert_eq!(
        space_identity(Some("w-env"), Some(context)).as_deref(),
        Some("w-env")
    );
}

#[test]
fn space_identity_falls_back_to_the_plugin_context_workspace_id() {
    let context = r#"{"workspace_id":"w-ctx","focused_pane_cwd":"/focused"}"#;
    assert_eq!(
        space_identity(None, Some(context)).as_deref(),
        Some("w-ctx")
    );
    assert_eq!(
        space_identity(Some("  "), Some(context)).as_deref(),
        Some("w-ctx")
    );
}

#[test]
fn space_identity_is_none_without_either_source() {
    assert_eq!(space_identity(None, None), None);
    assert_eq!(space_identity(None, Some("{}")), None);
    assert_eq!(space_identity(None, Some(r#"{"workspace_id":null}"#)), None);
    assert_eq!(space_identity(None, Some(r#"{"workspace_id":""}"#)), None);
}

#[test]
fn space_identity_treats_invalid_context_json_as_absent() {
    assert_eq!(
        space_identity(Some("w-env"), Some("not json")).as_deref(),
        Some("w-env")
    );
    assert_eq!(space_identity(None, Some("not json")), None);
}

#[test]
fn space_origin_carries_the_nullable_plugin_context_fields() {
    let origin = space_origin(Some(
        r#"{"workspace_id":"w1","tab_id":null,"focused_pane_id":"w1:p2"}"#,
    ));
    assert_eq!(
        origin,
        SpaceOrigin {
            workspace_id: Some("w1".into()),
            tab_id: None,
            focused_pane_id: Some("w1:p2".into()),
        }
    );
    assert_eq!(space_origin(Some("nope")), SpaceOrigin::default());
}
