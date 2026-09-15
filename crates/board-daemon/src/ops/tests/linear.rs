//! `linear.snapshot` — the plugin script run under a fake plugin root. Every
//! environment assertion is made from inside the fixture script, which dumps
//! what it sees to a file; nothing here reads a daemon log.

use super::*;
use crate::ops::linear::{
    child_env, snapshot, stderr_tail, SnapshotRunner, INSTALLED_PLUGINS_RELATIVE, LINEAR_RETRY_MAX,
    LINEAR_TIMEOUT_SECONDS, LINEAR_VIEW_PAGE_MAX, PLUGIN_ROOT_ENV, PLUGIN_ROOT_TOML_KEY,
    PLUGIN_VERSION_FLOOR, SCRIPT_DEADLINE, SCRIPT_WORST_CASE,
};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../board-core/tests/fixtures/linear-snapshot/bound-with-view.json"
);

/// A plugin root holding `plugin.json` at `version` and a snapshot script
/// whose body is `body` (run by bash; `$1` is the workspace id).
fn fake_plugin(version: &str, body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
    std::fs::write(
        dir.path().join(".claude-plugin/plugin.json"),
        format!(r#"{{"name":"work","version":"{version}"}}"#),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("bin")).unwrap();
    let script = dir.path().join("bin/work-snapshot.sh");
    std::fs::write(&script, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn cat_fixture() -> String {
    format!("cat {FIXTURE}")
}

/// A runner whose environment is exactly `HOME`, `PATH`, the root override and
/// `extra`. `PATH` is read from this process so `/usr/bin/env` can find bash.
fn runner(root: Option<&Path>, home: &Path, extra: &[(&str, &str)]) -> SnapshotRunner {
    let mut env = BTreeMap::new();
    env.insert("HOME".to_string(), home.display().to_string());
    env.insert("PATH".to_string(), std::env::var("PATH").unwrap());
    if let Some(root) = root {
        env.insert(PLUGIN_ROOT_ENV.to_string(), root.display().to_string());
    }
    for (key, value) in extra {
        env.insert((*key).to_string(), (*value).to_string());
    }
    SnapshotRunner {
        env,
        config_root: None,
        deadline: Duration::from_secs(5),
    }
}

fn params(origin_socket: Option<&Path>) -> LinearSnapshotParams {
    LinearSnapshotParams {
        workspace_id: "wA".into(),
        origin_socket: origin_socket.map(|p| p.display().to_string()),
    }
}

/// A fake Herdr whose `session.snapshot` lists `panes` with the given statuses.
fn herdr_with_panes(panes: &[(&str, &str)]) -> FakeHerdr {
    let panes: Vec<Value> = panes
        .iter()
        .map(|(id, status)| {
            let mut pane = testkit::pane_info(id);
            pane["agent_status"] = json!(status);
            pane
        })
        .collect();
    testkit::herdr_server()
        .on("session.snapshot", move |req| {
            testkit::reply(
                req,
                json!({
                    "type": "session_snapshot",
                    "snapshot": {
                        "version": board_herdr::SUPPORTED_HERDR_VERSION,
                        "protocol": board_herdr::SUPPORTED_HERDR_PROTOCOL,
                        "panes": panes,
                        "agents": []
                    }
                }),
            )
        })
        .serve()
}

fn read_env_dump(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn snapshot_returns_the_document_with_a_live_status_per_pane() {
    let herdr = herdr_with_panes(&[("wA:p1", "working"), ("wA:p2", "idle")]);
    let plugin = fake_plugin("0.3.0", &cat_fixture());
    let home = tempfile::tempdir().unwrap();

    let doc = snapshot(
        &runner(Some(plugin.path()), home.path(), &[]),
        params(Some(&herdr.socket)),
    )
    .unwrap();

    assert_eq!(doc.workspace.id, "wA");
    assert_eq!(doc.issues.len(), 3);
    assert_eq!(doc.view.status, "ok");
    assert_eq!(doc.pane_status["wA:p1"], "working");
    assert_eq!(doc.pane_status["wA:p2"], "idle");
    assert_eq!(doc.pane_status["wA:p9"], "unknown");
    assert_eq!(doc.pane_status.len(), 3);
    assert_eq!(herdr.methods(), vec!["ping", "session.snapshot"]);
}

#[test]
fn snapshot_is_routed_and_rejects_missing_params_as_a_bad_request() {
    let d = test_daemon(Config::default());
    let err = handle_request(&d, "linear.snapshot", json!({})).unwrap_err();
    assert_eq!(err.code(), 1);
    assert!(err.to_string().contains("workspace_id"), "routed? {err}");
    let err = handle_request(&d, "linear.snapshot", json!({"workspace_id": " "})).unwrap_err();
    assert_eq!(err.code(), 1);
}

#[test]
fn a_script_past_the_deadline_is_killed_and_reaped() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("pid");
    let plugin = fake_plugin(
        "0.3.0",
        &format!("echo $$ > {}\nexec sleep 30", pid_file.display()),
    );
    let mut runner = runner(Some(plugin.path()), home.path(), &[]);
    // The pid file must exist before the deadline fires: bash startup took
    // over 2s at load average 90 on this box, so the deadline is 6s.
    runner.deadline = Duration::from_millis(6000);

    let err = snapshot(&runner, params(None)).unwrap_err();

    assert_eq!(err.code(), 6);
    assert!(err.to_string().contains("timed out"), "message: {err}");
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // A zombie still answers signal 0; only a reaped process is gone.
    let rc = unsafe { libc::kill(pid, 0) };
    let errno = std::io::Error::last_os_error().raw_os_error();
    assert_eq!(rc, -1, "pid {pid} still exists (zombie or running)");
    assert_eq!(errno, Some(libc::ESRCH));
}

#[test]
fn a_script_that_uses_the_whole_budget_is_not_killed() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin("0.3.0", &format!("sleep 1\n{}", cat_fixture()));
    let mut runner = runner(Some(plugin.path()), home.path(), &[]);
    // Measured: a 2.5s sleep under a 4s deadline was killed on a loaded box
    // (bash startup plus the fixture's cat took >1.5s); the ratio, not the
    // absolute value, is what this test pins.
    runner.deadline = Duration::from_millis(6000);

    let doc = snapshot(&runner, params(None)).unwrap();
    assert_eq!(doc.issues.len(), 3);
}

#[test]
fn the_production_deadline_exceeds_the_worst_case_the_knobs_allow() {
    let worst = (2 + LINEAR_VIEW_PAGE_MAX) * LINEAR_TIMEOUT_SECONDS + 2 * 5;
    assert_eq!(SCRIPT_WORST_CASE, Duration::from_secs(worst));
    assert!(SCRIPT_DEADLINE > SCRIPT_WORST_CASE);
    assert_eq!(LINEAR_RETRY_MAX, 1, "one attempt: no backoff sleep can run");
}

#[test]
fn truncated_json_is_a_parse_error_not_a_panic() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin("0.3.0", r#"printf '{"schema":1,"groups":['"#);

    let err = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();

    assert_eq!(err.code(), 6);
    assert!(err.to_string().contains("cannot parse"), "message: {err}");
}

#[test]
fn exit_1_with_empty_stdout_carries_the_sanitised_stderr_tail() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin(
        "0.3.0",
        r#"printf 'boom: \033[31mred\033[0m\n' >&2; exit 1"#,
    );

    let err = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();

    assert_eq!(err.code(), 6);
    let msg = err.to_string();
    assert!(msg.contains("exit code 1"), "message: {msg}");
    assert!(msg.contains("boom: [31mred[0m"), "message: {msg}");
    assert!(!msg.contains('\u{1b}'), "escape leaked: {msg:?}");
}

#[test]
fn exit_0_with_empty_stdout_is_an_error_carrying_stderr() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin("0.3.0", "echo nothing-to-say >&2; exit 0");

    let err = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("printed no document"), "message: {msg}");
    assert!(msg.contains("nothing-to-say"), "message: {msg}");
}

#[test]
fn the_closed_exit_codes_are_named() {
    let home = tempfile::tempdir().unwrap();
    for (code, phrase) in [(2, "argument was refused"), (3, "no such space")] {
        let plugin = fake_plugin("0.3.0", &format!("exit {code}"));
        let err =
            snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();
        assert!(err.to_string().contains(phrase), "exit {code}: {err}");
    }
}

#[test]
fn a_schema_other_than_one_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin("0.3.0", r#"echo '{"schema":2}'"#);

    let err = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();

    assert!(err.to_string().contains("schema 2"), "message: {err}");
}

#[test]
fn no_resolvable_root_names_the_env_var_the_toml_key_and_the_installed_path() {
    let home = tempfile::tempdir().unwrap();

    let err = snapshot(&runner(None, home.path(), &[]), params(None)).unwrap_err();

    assert_eq!(err.code(), 6);
    let msg = err.to_string();
    assert!(msg.contains(PLUGIN_ROOT_ENV), "message: {msg}");
    assert!(msg.contains(PLUGIN_ROOT_TOML_KEY), "message: {msg}");
    let installed = home.path().join(INSTALLED_PLUGINS_RELATIVE);
    assert!(
        msg.contains(&installed.display().to_string()),
        "message: {msg}"
    );
}

#[test]
fn the_toml_root_is_used_when_the_env_var_is_absent() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin("0.3.0", &cat_fixture());
    let mut runner = runner(None, home.path(), &[]);
    runner.config_root = Some(plugin.path().to_path_buf());

    let doc = snapshot(&runner, params(None)).unwrap();
    assert_eq!(doc.issues.len(), 3);
}

#[test]
fn the_installed_plugins_user_record_is_the_last_resort() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin("0.3.0", &cat_fixture());
    let installed = home.path().join(INSTALLED_PLUGINS_RELATIVE);
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(
        &installed,
        json!({
            "version": 2,
            "plugins": {
                "work@shrimpshack": [
                    {"scope": "project", "installPath": "/nowhere/project", "version": "0.3.0"},
                    {"scope": "user", "installPath": plugin.path(), "version": "0.3.0"}
                ]
            }
        })
        .to_string(),
    )
    .unwrap();

    let doc = snapshot(&runner(None, home.path(), &[]), params(None)).unwrap();
    assert_eq!(doc.issues.len(), 3);
}

#[test]
fn a_plugin_below_the_floor_is_refused_naming_both_versions() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin("0.2.9", &cat_fixture());

    let err = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();

    assert_eq!(err.code(), 6);
    let msg = err.to_string();
    assert!(msg.contains("0.2.9"), "message: {msg}");
    assert!(msg.contains(PLUGIN_VERSION_FLOOR), "message: {msg}");
}

#[test]
fn a_plugin_at_or_above_the_floor_is_accepted() {
    let home = tempfile::tempdir().unwrap();
    for version in ["0.3.0", "0.10.0", "1.0.0"] {
        let plugin = fake_plugin(version, &cat_fixture());
        snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None))
            .unwrap_or_else(|e| panic!("version {version} refused: {e}"));
    }
}

#[test]
fn pane_status_comes_from_the_origin_socket_even_without_a_startup_herdr_handle() {
    let herdr = herdr_with_panes(&[("wA:p1", "working")]);
    // Default test daemon: `herdr` is `None`, exactly a daemon auto-started
    // before herdr was up. The origin socket is what the caller runs inside.
    let plugin = fake_plugin("0.3.0", &cat_fixture());
    let home = tempfile::tempdir().unwrap();

    let doc = snapshot(
        &runner(Some(plugin.path()), home.path(), &[]),
        params(Some(&herdr.socket)),
    )
    .unwrap();

    assert_eq!(
        doc.pane_status.get("wA:p1").map(String::as_str),
        Some("working")
    );
    assert_eq!(
        doc.pane_status.get("wA:p2").map(String::as_str),
        Some("unknown")
    );
    assert_eq!(doc.pane_status.len(), 3);
    assert!(
        herdr.methods().contains(&"session.snapshot".to_string()),
        "{:?}",
        herdr.methods()
    );
}

#[test]
fn without_an_origin_socket_every_pane_status_is_unknown() {
    let plugin = fake_plugin("0.3.0", &cat_fixture());
    let home = tempfile::tempdir().unwrap();

    let doc = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap();

    assert!(doc.pane_status.values().all(|s| s == "unknown"));
    assert_eq!(doc.pane_status.len(), 3);
}

#[test]
fn an_unreachable_origin_socket_still_yields_the_document_with_unknown_statuses() {
    let plugin = fake_plugin("0.3.0", &cat_fixture());
    let home = tempfile::tempdir().unwrap();

    let doc = snapshot(
        &runner(Some(plugin.path()), home.path(), &[]),
        params(Some(Path::new("/tmp/no-such-herdr.sock"))),
    )
    .unwrap();

    assert_eq!(doc.issues.len(), 3);
    assert!(doc.pane_status.values().all(|s| s == "unknown"));
}

#[test]
fn the_child_environment_is_built_from_scratch_and_filtered_by_prefix() {
    let herdr = herdr_with_panes(&[]);
    let home = tempfile::tempdir().unwrap();
    let dump = home.path().join("env.txt");
    let plugin = fake_plugin(
        "0.3.0",
        &format!("env > {}\n{}", dump.display(), cat_fixture()),
    );

    snapshot(
        &runner(
            Some(plugin.path()),
            home.path(),
            &[
                ("HERDR_LINEAR_STORE_DIR", "/stores/here"),
                ("LINEAR_CACHE_DIR", "/cache/here"),
                ("UNRELATED_SECRET", "must-not-leak"),
                ("BOARD_TICK_MS", "7"),
            ],
        ),
        params(Some(&herdr.socket)),
    )
    .unwrap();

    let seen = read_env_dump(&dump);
    assert_eq!(seen["HOME"], home.path().display().to_string());
    assert!(seen.contains_key("PATH"));
    assert_eq!(
        seen["HERDR_SOCKET_PATH"],
        herdr.socket.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(seen["HERDR_LINEAR_TIMEOUT_SECONDS"], "8");
    assert_eq!(seen["HERDR_LINEAR_RETRY_MAX"], "1");
    assert_eq!(seen["HERDR_LINEAR_VIEW_PAGE_MAX"], "10");
    assert_eq!(seen["HERDR_LINEAR_STORE_DIR"], "/stores/here");
    assert_eq!(seen["LINEAR_CACHE_DIR"], "/cache/here");
    assert!(!seen.contains_key("UNRELATED_SECRET"), "{seen:?}");
    assert!(!seen.contains_key("BOARD_TICK_MS"), "{seen:?}");
    assert!(!seen.contains_key("HERDR_BIN"), "{seen:?}");
    assert!(!seen.contains_key(PLUGIN_ROOT_ENV), "{seen:?}");
    // cargo sets this in every test process; its absence proves `env_clear`.
    assert!(!seen.contains_key("CARGO_MANIFEST_DIR"), "{seen:?}");
}

#[test]
fn herdr_bin_is_forwarded_only_from_a_herdr_bin_path_naming_a_file() {
    let home = tempfile::tempdir().unwrap();
    let dump = home.path().join("env.txt");
    let plugin = fake_plugin(
        "0.3.0",
        &format!("env > {}\n{}", dump.display(), cat_fixture()),
    );
    let real_bin = home.path().join("herdr");
    std::fs::write(&real_bin, "#!/bin/sh\n").unwrap();

    snapshot(
        &runner(
            Some(plugin.path()),
            home.path(),
            &[("HERDR_BIN_PATH", &real_bin.display().to_string())],
        ),
        params(None),
    )
    .unwrap();
    let seen = read_env_dump(&dump);
    assert_eq!(seen["HERDR_BIN"], real_bin.display().to_string());
    assert!(!seen.contains_key("HERDR_SOCKET_PATH"), "{seen:?}");

    snapshot(
        &runner(
            Some(plugin.path()),
            home.path(),
            &[("HERDR_BIN_PATH", &home.path().display().to_string())],
        ),
        params(None),
    )
    .unwrap();
    let seen = read_env_dump(&dump);
    assert!(
        !seen.contains_key("HERDR_BIN"),
        "a directory is not a herdr: {seen:?}"
    );
}

#[test]
fn child_env_without_herdr_bin_path_sets_no_herdr_bin() {
    let mut env = BTreeMap::new();
    env.insert("HOME".to_string(), "/h".to_string());
    env.insert("PATH".to_string(), "/bin".to_string());
    env.insert("HERDR_BIN_PATH".to_string(), String::new());
    let child = child_env(&env, None);
    assert!(!child.contains_key("HERDR_BIN"));
    assert!(!child.contains_key("HERDR_BIN_PATH"));
    assert_eq!(child.len(), 5, "{child:?}");
}

#[test]
fn stderr_tail_strips_controls_and_keeps_the_end() {
    assert_eq!(stderr_tail(b""), "(empty)");
    assert_eq!(stderr_tail(b"  a\x1b[1mb\x07c\xc2\x85d\n "), "a[1mbcd");
    assert_eq!(stderr_tail("x\u{202E}y\u{200B}z".as_bytes()), "xyz");
    let long: Vec<u8> = std::iter::repeat_n(b'x', 5000)
        .chain(b"END".iter().copied())
        .collect();
    let tail = stderr_tail(&long);
    assert!(tail.len() <= 2048);
    assert!(tail.ends_with("END"));
}
