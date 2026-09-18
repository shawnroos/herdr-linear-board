//! `linear.snapshot` — the plugin script run under a fake plugin root. Every
//! environment assertion is made from inside the fixture script, which dumps
//! what it sees to a file; nothing here reads a daemon log.

use super::*;
use crate::ops::linear::{
    child_env, issue, list, snapshot, utf8_env, ScriptRunner, HERDR_CALLS_MAX,
    HERDR_CALL_BUDGET_SECONDS, INSTALLED_PLUGINS_RELATIVE, ISSUE_SCRIPT_DEADLINE,
    ISSUE_SCRIPT_WORST_CASE, KEYCHAIN_BUDGET_SECONDS, LINEAR_RETRY_MAX, LINEAR_TIMEOUT_SECONDS,
    LINEAR_VIEW_PAGE_MAX, LIST_SCRIPT_DEADLINE, LIST_SCRIPT_WORST_CASE, PLUGIN_ROOT_ENV,
    PLUGIN_ROOT_TOML_KEY, PLUGIN_VERSION_FLOOR, SCRIPT_DEADLINE, SCRIPT_WORST_CASE,
    STDOUT_CAP_BYTES, TERM_GRACE,
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
    fake_plugin_scripts(version, &[("work-snapshot.sh", body)])
}

/// A plugin root holding `plugin.json` at `version` and one `bin/` script per
/// `(file name, body)` pair.
fn fake_plugin_scripts(version: &str, scripts: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
    std::fs::write(
        dir.path().join(".claude-plugin/plugin.json"),
        format!(r#"{{"name":"work","version":"{version}"}}"#),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("bin")).unwrap();
    for (name, body) in scripts {
        let script = dir.path().join("bin").join(name);
        std::fs::write(&script, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

fn cat_fixture() -> String {
    format!("cat {FIXTURE}")
}

/// A runner whose environment is exactly `HOME`, `PATH`, the root override and
/// `extra`. `PATH` is read from this process so `/usr/bin/env` can find bash.
fn runner(root: Option<&Path>, home: &Path, extra: &[(&str, &str)]) -> ScriptRunner {
    let mut env = BTreeMap::new();
    env.insert("HOME".to_string(), home.display().to_string());
    env.insert("PATH".to_string(), std::env::var("PATH").unwrap());
    if let Some(root) = root {
        env.insert(PLUGIN_ROOT_ENV.to_string(), root.display().to_string());
    }
    for (key, value) in extra {
        env.insert((*key).to_string(), (*value).to_string());
    }
    ScriptRunner {
        env,
        config_root: None,
        // Measured: at load average 260 a fake script that only cats the
        // fixture passed 5s. Tests that pin a deadline set their own.
        deadline: Duration::from_secs(30),
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
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, &cat_fixture());
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
        PLUGIN_VERSION_FLOOR,
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
        PLUGIN_VERSION_FLOOR,
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
        PLUGIN_VERSION_FLOOR,
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
        PLUGIN_VERSION_FLOOR,
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
        PLUGIN_VERSION_FLOOR,
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
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, &format!("sleep 1\n{}", cat_fixture()));
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
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, r#"printf '{"schema":1,"groups":['"#);

    let err = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();

    assert_eq!(err.code(), 6);
    assert!(err.to_string().contains("cannot parse"), "message: {err}");
}

#[test]
fn a_crash_names_its_exit_code_and_how_to_see_its_output_and_never_shows_stderr() {
    let home = tempfile::tempdir().unwrap();
    // What a plugin tracing its own run would print.
    let dump = home.path().join("stderr-is");
    let plugin = fake_plugin(
        PLUGIN_VERSION_FLOOR,
        &format!(
            r#"{}echo '+ curl -H "Authorization: lin_api_TRACEDTRACEDTRACED"' >&2; exit 1"#,
            stderr_probe(&dump)
        ),
    );

    let err = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();
    assert_eq!(std::fs::read_to_string(&dump).unwrap().trim(), "null");

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
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, "echo nothing-to-say >&2; exit 0");

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
    let new = fake_plugin(PLUGIN_VERSION_FLOOR, &cat_fixture());
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
        let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, &format!("exit {code}"));
        let err =
            snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();
        assert!(err.to_string().contains(phrase), "exit {code}: {err}");
    }
}

#[test]
fn a_schema_other_than_one_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, r#"echo '{"schema":2}'"#);

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
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, &cat_fixture());
    let mut runner = runner(None, home.path(), &[]);
    runner.config_root = Some(plugin.path().to_path_buf());

    let doc = snapshot(&runner, params(None)).unwrap();
    assert_eq!(doc.issues.len(), 3);
}

#[test]
fn the_installed_plugins_user_record_is_the_last_resort() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, &cat_fixture());
    let installed = home.path().join(INSTALLED_PLUGINS_RELATIVE);
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(
        &installed,
        json!({
            "version": 2,
            "plugins": {
                "work@shrimpshack": [
                    {"scope": "project", "installPath": "/nowhere/project", "version": PLUGIN_VERSION_FLOOR},
                    {"scope": "user", "installPath": plugin.path(), "version": PLUGIN_VERSION_FLOOR}
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
    // 0.3.x shipped the snapshot script but none of the list scripts.
    let plugin = fake_plugin("0.3.9", &cat_fixture());

    let err = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap_err();

    assert_eq!(err.code(), 6);
    let msg = err.to_string();
    assert!(msg.contains("0.3.9"), "message: {msg}");
    assert!(msg.contains("0.4.0"), "message: {msg}");
    assert_eq!(PLUGIN_VERSION_FLOOR, "0.4.0");
}

#[test]
fn a_plugin_at_or_above_the_floor_is_accepted() {
    let home = tempfile::tempdir().unwrap();
    for version in ["0.4.0", "0.10.0", "1.0.0"] {
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
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, &cat_fixture());
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
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, &cat_fixture());
    let home = tempfile::tempdir().unwrap();

    let doc = snapshot(&runner(Some(plugin.path()), home.path(), &[]), params(None)).unwrap();

    assert!(doc.pane_status.values().all(|s| s == "unknown"));
    assert_eq!(doc.pane_status.len(), 3);
}

#[test]
fn an_unreachable_origin_socket_still_yields_the_document_with_unknown_statuses() {
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, &cat_fixture());
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
        PLUGIN_VERSION_FLOOR,
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
        PLUGIN_VERSION_FLOOR,
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

// -- linear.list ------------------------------------------------------------

const SPACES_ENVELOPE: &str = r#"{"status":"ok","message":null,"rows":[{"id":"wA","label":"alpha","live":true,"state":"bound","project_id":"proj-1","project_name":"Example"},{"id":"wB","label":"wB","live":false,"state":"unbound","project_id":null,"project_name":null}]}"#;
const PROJECTS_ENVELOPE: &str = r#"{"status":"partial","message":"listed the first pages only","rows":[{"id":"proj-1","name":"Example","team_key":"EX"}]}"#;

/// A body that refuses any argument with exit 2, as the no-argument scripts do.
fn no_argument_script(then: &str) -> String {
    format!("[ \"$#\" -eq 0 ] || exit 2\n{then}")
}

fn echo_envelope(envelope: &str) -> String {
    format!("printf '%s\\n' '{envelope}'")
}

/// A plugin holding all three list scripts. The views script echoes its one
/// argument back as the row id, so a test sees what the daemon passed.
fn list_plugin(version: &str) -> tempfile::TempDir {
    let views = "[ \"$#\" -eq 1 ] || exit 2\nprintf '{\"status\":\"ok\",\"message\":null,\"rows\":[{\"id\":\"%s\",\"name\":\"Board\"}]}\\n' \"$1\"";
    fake_plugin_scripts(
        version,
        &[
            (
                "work-spaces.sh",
                &no_argument_script(&echo_envelope(SPACES_ENVELOPE)),
            ),
            (
                "work-projects.sh",
                &no_argument_script(&echo_envelope(PROJECTS_ENVELOPE)),
            ),
            ("work-views.sh", views),
        ],
    )
}

fn list_params(kind: LinearListKind, id: Option<&str>) -> LinearListParams {
    LinearListParams {
        kind,
        id: id.map(str::to_string),
        origin_socket: None,
        plugin_root: None,
    }
}

fn script_name(kind: LinearListKind) -> &'static str {
    match kind {
        LinearListKind::Spaces => "work-spaces.sh",
        LinearListKind::Projects => "work-projects.sh",
        LinearListKind::Views => "work-views.sh",
    }
}

fn id_for(kind: LinearListKind) -> Option<&'static str> {
    (kind == LinearListKind::Views).then_some("proj-1")
}

const KINDS: [LinearListKind; 3] = [
    LinearListKind::Spaces,
    LinearListKind::Projects,
    LinearListKind::Views,
];

#[test]
fn each_list_kind_returns_its_scripts_envelope() {
    let home = tempfile::tempdir().unwrap();
    let plugin = list_plugin(PLUGIN_VERSION_FLOOR);
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    match list(&runner, list_params(LinearListKind::Spaces, None)).unwrap() {
        LinearListResult::Spaces(spaces) => {
            assert_eq!(spaces.status, LinearListStatus::Ok);
            assert_eq!(spaces.rows.len(), 2);
            assert_eq!(spaces.rows[0].project_name.as_deref(), Some("Example"));
            assert_eq!(spaces.rows[1].live, Some(false));
        }
        other => panic!("{other:?}"),
    }
    match list(&runner, list_params(LinearListKind::Projects, None)).unwrap() {
        LinearListResult::Projects(projects) => {
            assert_eq!(projects.status, LinearListStatus::Partial);
            assert_eq!(projects.rows[0].team_key.as_deref(), Some("EX"));
            assert_eq!(
                projects.message.as_deref(),
                Some("listed the first pages only")
            );
        }
        other => panic!("{other:?}"),
    }
    match list(&runner, list_params(LinearListKind::Views, Some("proj-1"))).unwrap() {
        LinearListResult::Views(views) => {
            assert_eq!(views.status, LinearListStatus::Ok);
            assert_eq!(views.rows[0].id, "proj-1", "the id is the one argument");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn list_is_routed_and_refuses_a_views_list_without_a_project_id() {
    let d = test_daemon(Config::default());
    let err = handle_request(&d, "linear.list", json!({"kind": "views"})).unwrap_err();
    assert_eq!(err.code(), 1, "routed? {err}");
    assert!(err.to_string().contains("project id"), "message: {err}");
    let err = handle_request(&d, "linear.list", json!({"kind": "issues"})).unwrap_err();
    assert_eq!(err.code(), 1, "{err}");
}

#[test]
fn an_id_outside_the_identifier_shape_is_refused_before_any_spawn() {
    let home = tempfile::tempdir().unwrap();
    let marker = home.path().join("spawned");
    let touch = format!("touch {}\n", marker.display());
    let plugin = fake_plugin_scripts(
        PLUGIN_VERSION_FLOOR,
        &[
            (
                "work-spaces.sh",
                &format!("{touch}{}", echo_envelope(SPACES_ENVELOPE)),
            ),
            (
                "work-projects.sh",
                &format!("{touch}{}", echo_envelope(PROJECTS_ENVELOPE)),
            ),
            (
                "work-views.sh",
                &format!("{touch}{}", echo_envelope(PROJECTS_ENVELOPE)),
            ),
        ],
    );
    let runner = runner(Some(plugin.path()), home.path(), &[]);
    let too_long = "a".repeat(65);
    let refused: Vec<(LinearListKind, Option<&str>)> = vec![
        (LinearListKind::Views, None),
        (LinearListKind::Views, Some("")),
        (LinearListKind::Views, Some("-rf")),
        (LinearListKind::Views, Some("_leading")),
        (LinearListKind::Views, Some("a.b")),
        (LinearListKind::Views, Some("a b")),
        (LinearListKind::Views, Some("a/b")),
        (LinearListKind::Views, Some("proj\n1")),
        (LinearListKind::Views, Some("pröj")),
        (LinearListKind::Views, Some(&too_long)),
        (LinearListKind::Spaces, Some("wA")),
        (LinearListKind::Projects, Some("proj-1")),
    ];
    for (kind, id) in refused {
        let err = list(&runner, list_params(kind, id)).unwrap_err();
        assert_eq!(err.code(), 1, "{kind:?} {id:?}: {err}");
        assert!(!marker.exists(), "{kind:?} {id:?} spawned the script");
    }

    let longest = "a".repeat(64);
    for id in ["p", "Proj_1-x", longest.as_str()] {
        list(&runner, list_params(LinearListKind::Views, Some(id)))
            .unwrap_or_else(|e| panic!("{id} refused: {e}"));
    }
    assert!(marker.exists(), "an accepted id reaches the script");
}

#[test]
fn a_list_script_exiting_non_zero_is_code_6_naming_that_script() {
    let home = tempfile::tempdir().unwrap();
    for kind in KINDS {
        let name = script_name(kind);
        for (code, phrase) in [(1, "exit code 1"), (2, "argument was refused")] {
            let plugin =
                fake_plugin_scripts(PLUGIN_VERSION_FLOOR, &[(name, &format!("exit {code}"))]);
            let err = list(
                &runner(Some(plugin.path()), home.path(), &[]),
                list_params(kind, id_for(kind)),
            )
            .unwrap_err();
            assert_eq!(err.code(), 6, "{kind:?} exit {code}: {err}");
            let msg = err.to_string();
            assert!(msg.contains(phrase), "{kind:?} exit {code}: {msg}");
            assert!(msg.contains(name), "{kind:?} exit {code}: {msg}");
            assert!(!msg.contains("work-snapshot.sh"), "{msg}");
            assert!(!msg.contains("no such space"), "{msg}");
        }
    }
}

#[test]
fn a_list_script_printing_non_json_or_an_unknown_status_is_code_6() {
    let home = tempfile::tempdir().unwrap();
    for body in [
        "echo not-json",
        r#"printf '{"status":"ok","rows":['"#,
        r#"echo '{"status":"fine","rows":[]}'"#,
    ] {
        let plugin = fake_plugin_scripts(PLUGIN_VERSION_FLOOR, &[("work-projects.sh", body)]);
        let err = list(
            &runner(Some(plugin.path()), home.path(), &[]),
            list_params(LinearListKind::Projects, None),
        )
        .unwrap_err();
        assert_eq!(err.code(), 6, "{body}: {err}");
        let msg = err.to_string();
        assert!(msg.contains("cannot parse"), "{body}: {msg}");
        assert!(msg.contains("work-projects.sh"), "{body}: {msg}");
    }
}

#[test]
fn a_list_from_a_plugin_below_the_floor_is_refused_naming_both_versions() {
    let home = tempfile::tempdir().unwrap();
    let plugin = list_plugin("0.3.9");

    let err = list(
        &runner(Some(plugin.path()), home.path(), &[]),
        list_params(LinearListKind::Spaces, None),
    )
    .unwrap_err();

    assert_eq!(err.code(), 6);
    let msg = err.to_string();
    assert!(msg.contains("0.3.9"), "message: {msg}");
    assert!(msg.contains("0.4.0"), "message: {msg}");
}

/// A plugin that is installed and current but ships no script for this op is
/// its own code, not the retryable one: the remedy is to update the plugin, and
/// a reader that offers a retry for it retries forever.
#[test]
fn a_plugin_without_the_list_script_names_the_missing_script() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, &cat_fixture());

    let err = list(
        &runner(Some(plugin.path()), home.path(), &[]),
        list_params(LinearListKind::Views, Some("proj-1")),
    )
    .unwrap_err();

    assert_eq!(err.code(), 7);
    assert!(
        err.to_string().contains("bin/work-views.sh"),
        "message: {err}"
    );
}

#[test]
fn a_list_script_that_ignores_sigterm_is_killed_after_the_grace() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("pid");
    let plugin = fake_plugin_scripts(
        PLUGIN_VERSION_FLOOR,
        &[(
            "work-spaces.sh",
            &format!(
                "trap '' TERM\necho $$ > {}\nwhile :; do sleep 0.1; done",
                pid_file.display()
            ),
        )],
    );
    let mut runner = runner(Some(plugin.path()), home.path(), &[]);
    // Long enough for bash to write the pid file on a loaded box.
    runner.deadline = Duration::from_millis(6000);

    let started = Instant::now();
    let err = list(&runner, list_params(LinearListKind::Spaces, None)).unwrap_err();

    assert_eq!(err.code(), 6);
    let msg = err.to_string();
    assert!(msg.contains("work-spaces.sh timed out"), "message: {msg}");
    assert!(gone(read_pid(&pid_file)));
    assert!(started.elapsed() < runner.deadline + TERM_GRACE + Duration::from_secs(3));
}

#[test]
fn a_cancelled_list_stops_its_script() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("pid");
    let plugin = fake_plugin_scripts(
        PLUGIN_VERSION_FLOOR,
        &[(
            "work-views.sh",
            &format!("echo $$ > {}\nsleep 60", pid_file.display()),
        )],
    );
    let mut runner = runner(Some(plugin.path()), home.path(), &[]);
    runner.deadline = Duration::from_secs(60);
    let flag = pid_file.clone();
    runner.cancelled = Arc::new(move || flag.exists());

    let started = Instant::now();
    let err = list(&runner, list_params(LinearListKind::Views, Some("proj-1"))).unwrap_err();

    assert_eq!(err.code(), 6);
    let msg = err.to_string();
    assert!(msg.contains("work-views.sh was stopped"), "message: {msg}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert!(gone(read_pid(&pid_file)));
}

#[test]
fn list_output_past_the_cap_is_refused_and_the_run_still_ends() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin_scripts(
        PLUGIN_VERSION_FLOOR,
        &[(
            "work-projects.sh",
            &format!("head -c {} /dev/zero", STDOUT_CAP_BYTES + 100),
        )],
    );
    let mut runner = runner(Some(plugin.path()), home.path(), &[]);
    runner.deadline = Duration::from_secs(30);

    let err = list(&runner, list_params(LinearListKind::Projects, None)).unwrap_err();

    assert_eq!(err.code(), 6);
    assert!(err.to_string().contains("more than"), "message: {err}");
}

/// Writes `null` to `dump` when the script's fd 2 is `/dev/null`, `other`
/// otherwise. The error text cannot show this: an inherited stderr reaches the
/// daemon's own output without touching any message.
fn stderr_probe(dump: &Path) -> String {
    format!(
        "if [ /dev/fd/2 -ef /dev/null ]; then echo null; else echo other; fi > {}\n",
        dump.display()
    )
}

#[test]
fn a_list_scripts_stderr_reaches_neither_the_result_nor_the_daemons_output() {
    let home = tempfile::tempdir().unwrap();
    let dump = home.path().join("stderr-is");
    let body = format!(
        "{}echo '+ curl -H \"Authorization: lin_api_TRACEDTRACEDTRACED\"' >&2\n{}",
        stderr_probe(&dump),
        echo_envelope(PROJECTS_ENVELOPE)
    );
    let plugin = fake_plugin_scripts(PLUGIN_VERSION_FLOOR, &[("work-projects.sh", &body)]);

    let result = list(
        &runner(Some(plugin.path()), home.path(), &[]),
        list_params(LinearListKind::Projects, None),
    )
    .unwrap();

    let text = serde_json::to_string(&result).unwrap();
    assert!(!text.contains("TRACED"), "{text}");
    assert_eq!(std::fs::read_to_string(&dump).unwrap().trim(), "null");
}

#[test]
fn every_string_in_a_list_envelope_is_sanitised_on_arrival() {
    let home = tempfile::tempdir().unwrap();
    // ESC, a bidi override, a zero-width space and BEL, written by printf.
    let body = r#"printf '{"status":"unavailable","message":"Linear \\u001b[31mrefused\\u202e","rows":[{"id":"v1","name":"Board\\u200b view\\u0007\\tnext\\nline"}]}\n'"#;
    let plugin = fake_plugin_scripts(PLUGIN_VERSION_FLOOR, &[("work-views.sh", body)]);

    let result = list(
        &runner(Some(plugin.path()), home.path(), &[]),
        list_params(LinearListKind::Views, Some("proj-1")),
    )
    .unwrap();

    match result {
        LinearListResult::Views(views) => {
            assert_eq!(views.message.as_deref(), Some("Linear [31mrefused"));
            assert_eq!(views.rows[0].name, "Board view\tnext\nline");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_list_deadline_exceeds_its_worst_case_and_fits_the_tuis_client_timeout() {
    let pages = LINEAR_VIEW_PAGE_MAX * LINEAR_TIMEOUT_SECONDS + KEYCHAIN_BUDGET_SECONDS;
    let herdr = HERDR_CALLS_MAX * HERDR_CALL_BUDGET_SECONDS;
    assert!(LIST_SCRIPT_WORST_CASE >= Duration::from_secs(pages.max(herdr)));
    assert!(LIST_SCRIPT_DEADLINE > LIST_SCRIPT_WORST_CASE);
    // The TUI reads lists under the snapshot client timeout.
    assert!(LIST_SCRIPT_DEADLINE <= SCRIPT_DEADLINE);
}

#[test]
fn a_list_client_waits_longer_than_the_daemon_can_take_to_answer() {
    let daemon_longest = LIST_SCRIPT_DEADLINE + TERM_GRACE + Duration::from_secs(2);
    assert!(
        board_core::protocol::LINEAR_LIST_CLIENT_TIMEOUT > daemon_longest,
        "client {:?} vs daemon {:?}",
        board_core::protocol::LINEAR_LIST_CLIENT_TIMEOUT,
        daemon_longest
    );
}

// ---------------------------------------------------------------------------
// linear.issue — one issue read whole for the board's issue page
// ---------------------------------------------------------------------------

/// The shape `bin/work-issue.sh` prints, trimmed to what a test needs. The
/// contract it follows is `plugins/work/docs/issue.md` in the work plugin.
fn issue_document_json() -> &'static str {
    r###"{
  "schema": 1,
  "status": "ok",
  "message": null,
  "truncated": [],
  "issue": {
    "id": "i1", "identifier": "WEB-3318", "title": "Drawer is blank",
    "url": "https://linear.app/example/issue/WEB-3318/x",
    "description": "## What\n\nbody",
    "updated_at": "2026-09-04T15:55:10.206Z",
    "due_date": "2026-09-30", "estimate": 3, "priority": 2,
    "state": {"id": "s1", "name": "In Progress", "type": "started"},
    "assignee": {"id": "u1", "name": "Example User"},
    "labels": ["Bug"],
    "project": {"id": "p1", "name": "AI Canvas Tools"},
    "milestone": {"id": "m1", "name": "M2"},
    "cycle": {"id": "c1", "number": 14, "name": "Cycle 14"},
    "parent": {"id": "x1", "identifier": "WEB-2870", "title": "Parent",
               "state": {"id": "s2", "name": "Dev Done", "type": "started"}},
    "children": [{"id": "x2", "identifier": "WEB-3319", "title": "Child",
                  "state": {"id": "s3", "name": "Done", "type": "completed"}}],
    "relations": [{"type": "blocks", "direction": "outward",
                   "issue": {"id": "x3", "identifier": "WEB-3400", "title": "Other",
                             "state": {"id": "s4", "name": "Todo", "type": "unstarted"}}}],
    "comments": [{"id": "cm1", "body": "Repro", "created_at": "2026-09-05T09:00:00.000Z",
                  "author": "Example User", "parent_id": null},
                 {"id": "cm2", "body": "Same", "created_at": "2026-09-05T10:00:00.000Z",
                  "author": "Other", "parent_id": "cm1"}],
    "history": [{"id": "h1", "created_at": "2026-09-05T08:00:00.000Z", "actor": "Example User",
                 "from_state": "Backlog", "to_state": "In Progress",
                 "from_assignee": null, "to_assignee": null,
                 "from_priority": null, "to_priority": null,
                 "added_labels": [], "removed_labels": []}]
  }
}"###
}

fn issue_params(id: &str) -> LinearIssueParams {
    LinearIssueParams {
        issue: id.to_string(),
        origin_socket: None,
        plugin_root: None,
    }
}

/// A plugin root whose issue script prints `body` verbatim.
fn issue_plugin(body: &str) -> tempfile::TempDir {
    fake_plugin_scripts(
        PLUGIN_VERSION_FLOOR,
        &[(
            "work-issue.sh",
            &format!("cat <<'JSON_EOF'\n{body}\nJSON_EOF"),
        )],
    )
}

#[test]
fn an_issue_read_returns_every_section_the_page_shows() {
    let home = tempfile::tempdir().unwrap();
    let plugin = issue_plugin(issue_document_json());
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    let doc = issue(&runner, issue_params("WEB-3318")).unwrap();

    assert_eq!(doc.status, "ok");
    assert!(doc.truncated.is_empty());
    let issue = doc.issue.expect("a document with status ok carries an issue");
    assert_eq!(issue.identifier, "WEB-3318");
    assert_eq!(issue.estimate, Some(3.0));
    assert_eq!(issue.due_date.as_deref(), Some("2026-09-30"));
    assert_eq!(issue.milestone.unwrap().name.as_deref(), Some("M2"));
    assert_eq!(issue.cycle.unwrap().number, Some(14));
    assert_eq!(issue.children.len(), 1);
    assert_eq!(issue.comments.len(), 2);
    assert_eq!(issue.history.len(), 1);
    assert!(issue.description.unwrap().contains("## What"));
}

/// R9 — every linked row has to carry enough to open its own page, so the
/// board never needs a second read just to draw the row it came from.
#[test]
fn every_linked_row_carries_identifier_title_and_state() {
    let home = tempfile::tempdir().unwrap();
    let plugin = issue_plugin(issue_document_json());
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    let issue = issue(&runner, issue_params("WEB-3318"))
        .unwrap()
        .issue
        .unwrap();

    let rows = [
        issue.parent.clone().unwrap(),
        issue.children[0].clone(),
        issue.relations[0].issue.clone(),
    ];
    for row in rows {
        assert!(!row.identifier.is_empty(), "{row:?}");
        assert!(!row.title.is_empty(), "{row:?}");
        assert!(row.state.name.is_some(), "{row:?}");
    }
    assert_eq!(issue.relations[0].direction, "outward");
}

/// R4 — the page draws an empty marker for a property Linear has no value for,
/// so an explicit null has to parse as absent rather than fail the document.
#[test]
fn an_explicit_null_in_every_nullable_field_parses() {
    let home = tempfile::tempdir().unwrap();
    let plugin = issue_plugin(
        r#"{"schema":1,"status":"ok","message":null,"truncated":[],
            "issue":{"id":null,"identifier":"WEB-1","title":"t","url":null,
                     "description":null,"updated_at":null,"due_date":null,"estimate":null,
                     "priority":null,"state":{"id":null,"name":null,"type":null},
                     "assignee":null,"labels":[],"project":null,"milestone":null,"cycle":null,
                     "parent":null,"children":[],"relations":[],"comments":[],"history":[]}}"#,
    );
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    let issue = issue(&runner, issue_params("WEB-1")).unwrap().issue.unwrap();

    assert_eq!(issue.identifier, "WEB-1");
    assert!(issue.description.is_none());
    assert!(issue.milestone.is_none());
    assert!(issue.parent.is_none());
    assert!(issue.children.is_empty());
}

/// R8a — the read stops at its page cap and says so rather than draining, which
/// is what keeps it one Linear call.
#[test]
fn a_truncated_read_is_partial_and_names_what_was_cut() {
    let home = tempfile::tempdir().unwrap();
    let plugin = issue_plugin(
        r#"{"schema":1,"status":"partial","message":"read the first 50 of comments",
            "truncated":["comments","history"],
            "issue":{"identifier":"WEB-1","title":"t","state":{},"labels":[],
                     "children":[],"relations":[],"comments":[],"history":[]}}"#,
    );
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    let doc = issue(&runner, issue_params("WEB-1")).unwrap();

    assert_eq!(doc.status, "partial");
    assert_eq!(doc.truncated, vec!["comments", "history"]);
    assert!(doc.message.unwrap().contains("first 50"));
}

/// R16 — "this plugin cannot do the read" and "the read ran and failed" have
/// opposite remedies, so they cannot arrive as the same code. A reader that
/// offers a retry for a missing script retries forever.
#[test]
fn a_plugin_without_the_issue_script_is_its_own_error_code() {
    let home = tempfile::tempdir().unwrap();
    // A current plugin that ships the snapshot script but not the issue one.
    let plugin = fake_plugin(PLUGIN_VERSION_FLOOR, &cat_fixture());
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    let error = issue(&runner, issue_params("WEB-1")).unwrap_err();

    assert_eq!(error.code(), 7, "{error}");
    assert!(error.to_string().contains("work-issue.sh"), "{error}");

    // The snapshot on the same root still works: one missing script does not
    // take the rest of Linear mode down with it.
    assert!(snapshot(&runner, params(None)).is_ok());
}

#[test]
fn a_script_that_exits_non_zero_is_a_retryable_failure_not_an_unsupported_op() {
    let home = tempfile::tempdir().unwrap();
    let plugin = fake_plugin_scripts(PLUGIN_VERSION_FLOOR, &[("work-issue.sh", "exit 4")]);
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    let error = issue(&runner, issue_params("WEB-1")).unwrap_err();

    assert_eq!(error.code(), 6, "{error}");
}

#[test]
fn an_id_of_the_wrong_shape_is_refused_before_any_process_starts() {
    let home = tempfile::tempdir().unwrap();
    let marker = home.path().join("ran");
    let plugin = fake_plugin_scripts(
        PLUGIN_VERSION_FLOOR,
        &[("work-issue.sh", &format!("touch {}", marker.display()))],
    );
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    for bad in ["", "   ", "-oProxyCommand=x", "../etc/passwd", "a b"] {
        let error = issue(&runner, issue_params(bad)).unwrap_err();
        assert_eq!(error.code(), 1, "{bad:?} gave {error}");
    }
    assert!(!marker.exists(), "a refused id still ran the script");
}

#[test]
fn an_unparseable_document_names_the_script_and_how_to_run_it_by_hand() {
    let home = tempfile::tempdir().unwrap();
    let plugin =
        fake_plugin_scripts(PLUGIN_VERSION_FLOOR, &[("work-issue.sh", "printf 'not json'")]);
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    let error = issue(&runner, issue_params("WEB-1")).unwrap_err();

    assert_eq!(error.code(), 6, "{error}");
    let text = error.to_string();
    assert!(text.contains("work-issue.sh"), "{text}");
    assert!(text.contains("WEB-1"), "{text}");
}

#[test]
fn a_document_of_another_schema_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let plugin = issue_plugin(r#"{"schema":2,"status":"ok","truncated":[],"issue":null}"#);
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    let error = issue(&runner, issue_params("WEB-1")).unwrap_err();

    assert_eq!(error.code(), 6, "{error}");
    assert!(error.to_string().contains("schema 2"), "{error}");
}

/// A reachability failure is carried in the document, not as an error, so the
/// page can keep what it already shows and offer a retry.
#[test]
fn an_unreachable_linear_is_a_document_not_an_error() {
    let home = tempfile::tempdir().unwrap();
    let plugin = issue_plugin(
        r#"{"schema":1,"status":"unavailable","message":"Linear could not be reached",
            "truncated":[],"issue":null}"#,
    );
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    let doc = issue(&runner, issue_params("WEB-1")).unwrap();

    assert_eq!(doc.status, "unavailable");
    assert!(doc.issue.is_none());
    assert!(doc.message.unwrap().contains("could not be reached"));
}

/// Display controls are stripped from every string, as they are for the
/// snapshot and the lists: this document reaches a terminal.
#[test]
fn display_controls_are_stripped_from_the_document() {
    let home = tempfile::tempdir().unwrap();
    // Written with escapes rather than the characters themselves: a raw ESC in
    // source is invisible in a diff, and rustc refuses a bidi codepoint in a
    // literal outright.
    let doc = concat!(
        r#"{"schema":1,"status":"ok","truncated":[],"#,
        r#""issue":{"identifier":"WEB-1","title":"\u001b[31mred\u202etitle","state":{},"#,
        r#""labels":[],"children":[],"relations":[],"#,
        r#""comments":[{"id":"c","body":"\u001bbody","parent_id":null}],"#,
        r#""history":[]}}"#,
    );
    let plugin = issue_plugin(doc);
    let runner = runner(Some(plugin.path()), home.path(), &[]);

    let issue = issue(&runner, issue_params("WEB-1")).unwrap().issue.unwrap();

    assert_eq!(issue.title, "[31mredtitle");
    assert_eq!(issue.comments[0].body, "body");
}

#[test]
fn the_issue_deadline_is_sized_for_one_call_and_fits_its_client_timeout() {
    // One Linear call by contract, not one per page: that is the whole reason a
    // busy issue's page opens as fast as an empty one.
    assert!(ISSUE_SCRIPT_WORST_CASE >= Duration::from_secs(LINEAR_TIMEOUT_SECONDS));
    assert!(ISSUE_SCRIPT_WORST_CASE < LIST_SCRIPT_WORST_CASE);
    assert!(ISSUE_SCRIPT_DEADLINE > ISSUE_SCRIPT_WORST_CASE);

    let daemon_longest = ISSUE_SCRIPT_DEADLINE + TERM_GRACE + Duration::from_secs(2);
    assert!(
        board_core::protocol::LINEAR_ISSUE_CLIENT_TIMEOUT > daemon_longest,
        "client {:?} vs daemon {:?}",
        board_core::protocol::LINEAR_ISSUE_CLIENT_TIMEOUT,
        daemon_longest
    );
}
