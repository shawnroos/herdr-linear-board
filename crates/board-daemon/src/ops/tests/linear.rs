//! `linear.snapshot` — the plugin script run under a fake plugin root. Every
//! environment assertion is made from inside the fixture script, which dumps
//! what it sees to a file; nothing here reads a daemon log.

use super::*;
use crate::ops::linear::{
    child_env, snapshot, utf8_env, SnapshotRunner, HERDR_CALLS_MAX, HERDR_CALL_BUDGET_SECONDS,
    INSTALLED_PLUGINS_RELATIVE, KEYCHAIN_BUDGET_SECONDS, LINEAR_RETRY_MAX, LINEAR_TIMEOUT_SECONDS,
    LINEAR_VIEW_PAGE_MAX, PLUGIN_ROOT_ENV, PLUGIN_ROOT_TOML_KEY, PLUGIN_VERSION_FLOOR,
    SCRIPT_DEADLINE, SCRIPT_WORST_CASE, STDOUT_CAP_BYTES, TERM_GRACE,
};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        cancelled: Arc::new(|| false),
    }
}

fn gone(pid: i32) -> bool {
    // A zombie still answers signal 0; only a reaped process is gone.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn read_pid(path: &Path) -> i32 {
    std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn params(origin_socket: Option<&Path>) -> LinearSnapshotParams {
    LinearSnapshotParams {
        workspace_id: "wA".into(),
        origin_socket: origin_socket.map(|p| p.display().to_string()),
        plugin_root: None,
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
fn a_script_past_the_deadline_gets_sigterm_so_its_exit_trap_runs_and_is_reaped() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("pid");
    let trap_ran = home.path().join("trap-ran");
    let plugin = fake_plugin(
        "0.3.0",
        &format!(
            "trap 'touch {}' EXIT\necho $$ > {}\nsleep 30",
            trap_ran.display(),
            pid_file.display()
        ),
    );
    let mut runner = runner(Some(plugin.path()), home.path(), &[]);
    // The pid file must exist before the deadline fires: bash startup took
    // over 2s at load average 90 on this box, so the deadline is 6s.
    runner.deadline = Duration::from_millis(6000);

    let err = snapshot(&runner, params(None)).unwrap_err();

    assert_eq!(err.code(), 6);
    assert!(err.to_string().contains("timed out"), "message: {err}");
    let pid = read_pid(&pid_file);
    assert!(gone(pid), "pid {pid} still exists (zombie or running)");
    assert!(
        trap_ran.exists(),
        "the EXIT trap did not run: SIGKILL came first"
    );
}

#[test]
fn a_script_that_ignores_sigterm_is_killed_after_the_grace() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("pid");
    let plugin = fake_plugin(
        "0.3.0",
        &format!(
            "trap '' TERM\necho $$ > {}\nwhile :; do sleep 0.1; done",
            pid_file.display()
        ),
    );
    let mut runner = runner(Some(plugin.path()), home.path(), &[]);
    // As above: the pid file has to exist before the deadline on a loaded box.
    runner.deadline = Duration::from_millis(6000);

    let started = Instant::now();
    let err = snapshot(&runner, params(None)).unwrap_err();

    assert!(err.to_string().contains("timed out"), "message: {err}");
    assert!(gone(read_pid(&pid_file)));
    assert!(started.elapsed() < runner.deadline + TERM_GRACE + Duration::from_secs(3));
}

#[test]
fn a_cancelled_request_stops_the_script_before_its_deadline() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("pid");
    let plugin = fake_plugin(
        "0.3.0",
        &format!("echo $$ > {}\nsleep 60", pid_file.display()),
    );
    let mut runner = runner(Some(plugin.path()), home.path(), &[]);
    runner.deadline = Duration::from_secs(60);
    let flag = pid_file.clone();
    // Cancelled once the script has written its pid, as a client that went
    // away mid-run would be.
    runner.cancelled = Arc::new(move || flag.exists());

    let started = Instant::now();
    let err = snapshot(&runner, params(None)).unwrap_err();

    assert_eq!(err.code(), 6);
    assert!(err.to_string().contains("was stopped"), "message: {err}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert!(gone(read_pid(&pid_file)));
}

#[test]
fn a_grandchild_left_holding_the_output_does_not_hold_the_answer() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("straggler");
    let plugin = fake_plugin(
        "0.3.0",
        &format!(
            "sleep 30 &\necho $! > {}\n{}",
            pid_file.display(),
            cat_fixture()
        ),
    );
    let mut runner = runner(Some(plugin.path()), home.path(), &[]);
    runner.deadline = Duration::from_secs(20);

    let started = Instant::now();
    let doc = snapshot(&runner, params(None)).unwrap();

    assert_eq!(doc.issues.len(), 3);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let straggler = read_pid(&pid_file);
    let until = Instant::now() + Duration::from_secs(2);
    while !gone(straggler) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        gone(straggler),
        "the backgrounded sleep {straggler} outlived the run"
    );
}

#[test]
fn output_past_the_cap_is_refused_and_the_run_still_ends() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin(
        "0.3.0",
        &format!("head -c {} /dev/zero", STDOUT_CAP_BYTES + 100),
    );
    let mut runner = runner(Some(plugin.path()), home.path(), &[]);
    runner.deadline = Duration::from_secs(30);

    let err = snapshot(&runner, params(None)).unwrap_err();

    assert_eq!(err.code(), 6);
    assert!(err.to_string().contains("more than"), "message: {err}");
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
    // Three herdr reads (server status, space list, session snapshot) and one
    // keychain read, each bounded by a timeout the daemon sets in the child.
    assert_eq!(HERDR_CALLS_MAX, 3);
    let worst = (2 + LINEAR_VIEW_PAGE_MAX) * LINEAR_TIMEOUT_SECONDS
        + HERDR_CALLS_MAX * HERDR_CALL_BUDGET_SECONDS
        + KEYCHAIN_BUDGET_SECONDS;
    assert_eq!(SCRIPT_WORST_CASE, Duration::from_secs(worst));
    assert!(SCRIPT_DEADLINE > SCRIPT_WORST_CASE);
    assert_eq!(LINEAR_RETRY_MAX, 1, "one attempt: no backoff sleep can run");
}

#[test]
fn a_client_waits_longer_than_the_daemon_can_take_to_answer() {
    let daemon_longest = SCRIPT_DEADLINE + TERM_GRACE + Duration::from_secs(2);
    assert!(
        board_core::protocol::LINEAR_SNAPSHOT_CLIENT_TIMEOUT > daemon_longest,
        "client {:?} vs daemon {:?}",
        board_core::protocol::LINEAR_SNAPSHOT_CLIENT_TIMEOUT,
        daemon_longest
    );
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
fn a_crash_names_its_exit_code_and_how_to_see_its_output_and_never_shows_stderr() {
    let home = tempfile::tempdir().unwrap();
    // What a plugin tracing its own run would print.
    let plugin = fake_plugin(
        "0.3.0",
        r#"echo '+ curl -H "Authorization: lin_api_TRACEDTRACEDTRACED"' >&2; exit 1"#,
    );

    let err = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();

    assert_eq!(err.code(), 6);
    let msg = err.to_string();
    assert!(msg.contains("exit code 1"), "message: {msg}");
    assert!(
        msg.contains("work-snapshot.sh wA` in a shell"),
        "message: {msg}"
    );
    assert!(!msg.contains("TRACED"), "stderr reached the error: {msg}");
    assert!(
        !msg.contains("Authorization"),
        "stderr reached the error: {msg}"
    );
}

#[test]
fn exit_0_with_empty_stdout_is_an_error_that_shows_no_stderr() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin("0.3.0", "echo nothing-to-say >&2; exit 0");

    let err = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("printed no document"), "message: {msg}");
    assert!(!msg.contains("nothing-to-say"), "message: {msg}");
}

#[test]
fn a_non_utf8_environment_value_is_dropped_rather_than_panicking() {
    let env = utf8_env(
        vec![
            (OsString::from("HOME"), OsString::from("/h")),
            (
                OsString::from("LINEAR_BAD"),
                OsString::from_vec(vec![0x66, 0xff, 0x6f]),
            ),
            (OsString::from_vec(vec![0xfe]), OsString::from("value")),
        ]
        .into_iter(),
    );
    assert_eq!(env.len(), 1, "{env:?}");
    assert_eq!(env["HOME"], "/h");
}

#[test]
fn the_callers_plugin_root_is_preferred_over_the_daemons() {
    let home = tempfile::tempdir().unwrap();
    let old = fake_plugin("0.2.0", &cat_fixture());
    let new = fake_plugin("0.3.0", &cat_fixture());
    let runner = runner(Some(old.path()), home.path(), &[]);
    let mut p = params(None);
    p.plugin_root = Some(new.path().display().to_string());

    let doc = snapshot(&runner, p).unwrap();
    assert_eq!(doc.issues.len(), 3);

    let err = snapshot(&runner, params(None)).unwrap_err();
    assert!(
        err.to_string().contains("0.2.0"),
        "the daemon's own root is still the fallback: {err}"
    );
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
    assert_eq!(seen["HERDR_LINEAR_HERDR_TIMEOUT_SECONDS"], "5");
    assert_eq!(seen["HERDR_LINEAR_KEYCHAIN_TIMEOUT_SECONDS"], "5");
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
    assert_eq!(child.len(), 7, "{child:?}");
}
