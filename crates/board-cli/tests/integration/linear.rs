//! `board linear snapshot`: the CLI-to-daemon-to-script read, driven end to
//! end against a real `board daemon --foreground` and a fake plugin root
//! (U13). Every plugin-side failure exits `6` through the CLI.

use std::process::Output;

use serde_json::Value;

use super::{fake_plugin_root, json_error, json_output, TestDaemon};

const PLUGIN_UNAVAILABLE: i32 = 6;

fn code(out: &Output) -> i32 {
    out.status.code().expect("board exits, never signals")
}

/// A daemon whose environment names `root`: the daemon resolves the plugin
/// root from its own process, never from the CLI's.
fn daemon_with_root(root: &std::path::Path) -> TestDaemon {
    TestDaemon::start(&[("BOARD_WORK_PLUGIN_ROOT", root.to_str().unwrap())])
}

fn snapshot(td: &TestDaemon) -> Output {
    td.board(&["linear", "snapshot", "wA", "--json"])
}

fn plugin_error(out: &Output) -> String {
    assert_eq!(
        code(out),
        PLUGIN_UNAVAILABLE,
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let error = json_error(out);
    assert_eq!(error["error"]["code"], PLUGIN_UNAVAILABLE);
    error["error"]["message"].as_str().unwrap().to_string()
}

/// (a) The fixture document comes back through the CLI with a `pane_status`
/// entry per named pane; the test daemon has no herdr, so every one is unknown.
#[test]
fn snapshot_returns_the_fixture_document_with_pane_statuses() {
    let root = fake_plugin_root("0.3.0", &[]);
    let td = daemon_with_root(root.path());
    let doc = json_output(&snapshot(&td));
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["workspace"]["id"], "wA");
    assert_eq!(doc["record"]["state"], "bound");
    let statuses = doc["pane_status"].as_object().expect("pane_status map");
    for pane in ["wA:p1", "wA:p2"] {
        assert_eq!(statuses[pane], "unknown", "{statuses:?}");
    }
    assert!(
        statuses.values().all(|status| status == "unknown"),
        "{statuses:?}"
    );
}

/// (b) A plugin below the floor is refused with both versions in the message.
#[test]
fn a_plugin_below_the_floor_is_refused_naming_both_versions() {
    let root = fake_plugin_root("0.2.9", &[]);
    let td = daemon_with_root(root.path());
    let message = plugin_error(&snapshot(&td));
    assert!(message.contains("0.2.9"), "{message}");
    assert!(message.contains("0.3.0"), "{message}");
}

/// (c) With no env, no TOML key, and no installed_plugins.json under the
/// daemon's HOME, the error names all three sources.
#[test]
fn no_root_anywhere_names_every_source() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["linear", "snapshot", "wA", "--json"]);
    let message = plugin_error(&out);
    let installed = td
        ._dir
        .path()
        .join(".claude/plugins/installed_plugins.json");
    assert!(!installed.exists());
    assert!(message.contains("BOARD_WORK_PLUGIN_ROOT"), "{message}");
    assert!(message.contains("[daemon] work_plugin_root"), "{message}");
    assert!(message.contains(installed.to_str().unwrap()), "{message}");
}

/// (d) Exit 3 from the script is the "no such space" answer.
#[test]
fn a_script_exit_3_names_no_such_space() {
    let root = fake_plugin_root(
        "0.3.0",
        &[
            ("FAKE_WORK_SNAPSHOT_EXIT", "3"),
            ("FAKE_WORK_SNAPSHOT_STDERR", "wA is not a space here"),
        ],
    );
    let td = daemon_with_root(root.path());
    let message = plugin_error(&snapshot(&td));
    assert!(message.contains("no such space"), "{message}");
    assert!(message.contains("wA is not a space here"), "{message}");
}

/// (e) `[daemon] work_plugin_root` alone resolves the root.
#[test]
fn the_toml_key_alone_resolves_the_root() {
    let root = fake_plugin_root("0.3.0", &[("FAKE_WORK_SNAPSHOT_FIXTURE", "unbound")]);
    let td = TestDaemon::start_with_config(
        &[],
        &format!("work_plugin_root = \"{}\"\n", root.path().display()),
    );
    let doc = json_output(&td.board(&["linear", "snapshot", "wA", "--json"]));
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["record"]["state"], "unbound");
}

/// The CLI's `HERDR_SOCKET_PATH` becomes the request's origin socket and
/// reaches the script as `HERDR_SOCKET_PATH`: the one proof that the
/// CLI-side argument path is wired, not only the daemon's.
#[test]
fn the_origin_socket_reaches_the_script() {
    let dump_dir = tempfile::tempdir().unwrap();
    let dump = dump_dir.path().join("script-env");
    let root = fake_plugin_root(
        "0.3.0",
        &[("FAKE_WORK_SNAPSHOT_ENV_FILE", dump.to_str().unwrap())],
    );
    let td = daemon_with_root(root.path());
    let socket = td._dir.path().join("origin-herdr.sock");
    let out = td.board_with_env(
        &["linear", "snapshot", "wA", "--json"],
        &[("HERDR_SOCKET_PATH", socket.to_str().unwrap())],
    );
    json_output(&out);
    let env = std::fs::read_to_string(&dump).unwrap();
    let line = format!("HERDR_SOCKET_PATH={}", socket.display());
    assert!(env.lines().any(|l| l == line), "{env}");
}

/// Without `--json` the document is still printed as JSON.
#[test]
fn snapshot_without_json_prints_the_document() {
    let root = fake_plugin_root("0.3.0", &[]);
    let td = daemon_with_root(root.path());
    let out = td.board(&["linear", "snapshot", "wA"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).expect("JSON document");
    assert_eq!(doc["workspace"]["id"], "wA");
}
