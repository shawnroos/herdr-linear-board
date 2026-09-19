//! `board tui` decides Linear mode from the herdr space id before it touches
//! the store (KTD2, KTD3). The TUI itself needs a terminal, so every run here
//! exits at raw-mode setup; what is provable is what reached the daemon and
//! the store before that point.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use board_core::protocol::{DaemonStatus, Request, Response};
use serde_json::Value;

use super::{json_output, TestDaemon};

fn scope_paths(td: &TestDaemon) -> Vec<Value> {
    json_output(&td.board(&["project", "list", "--json"]))["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["project"]["scope_path"].clone())
        .collect()
}

fn cwd_scope(td: &TestDaemon) -> Value {
    Value::String(
        td._dir
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    )
}

/// AE3, characterised: with no space id the upstream path opens the cwd
/// project, which persists a row, before the terminal is even set up.
#[test]
fn tui_without_a_space_id_writes_the_cwd_project_row() {
    let td = TestDaemon::start(&[]);
    assert_eq!(scope_paths(&td), vec![Value::Null], "only Global exists");
    let out = td.board(&["tui"]);
    assert!(!out.status.success(), "no terminal: the TUI cannot start");
    assert!(
        scope_paths(&td).contains(&cwd_scope(&td)),
        "the upstream path persisted the cwd project: {:?}",
        scope_paths(&td)
    );
}

/// AE2 / R7: a space id selects Linear mode and no project row is written.
#[test]
fn tui_with_a_space_id_writes_no_project_row() {
    let td = TestDaemon::start(&[]);
    let out = td.board_with_env(&["tui"], &[("HERDR_WORKSPACE_ID", "wA")]);
    assert!(!out.status.success(), "no terminal: the TUI cannot start");
    assert_eq!(scope_paths(&td), vec![Value::Null], "no row for the cwd");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("BOARD_SCOPE_PATH"),
        "no scope note without the variable: {stderr}"
    );
}

/// KTD2: `BOARD_SCOPE_PATH` is ignored in Linear mode with one stderr line.
#[test]
fn board_scope_path_is_ignored_in_linear_mode_with_one_stderr_line() {
    let td = TestDaemon::start(&[]);
    let elsewhere = td._dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let out = td.board_with_env(
        &["tui"],
        &[
            ("HERDR_WORKSPACE_ID", "wA"),
            ("BOARD_SCOPE_PATH", elsewhere.to_str().unwrap()),
        ],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.contains("BOARD_SCOPE_PATH is ignored in Linear mode"))
            .count(),
        1,
        "exactly one note: {stderr}"
    );
    assert_eq!(scope_paths(&td), vec![Value::Null], "no row anywhere");
}

/// The plugin context's `workspace_id` selects Linear mode too, while its
/// directories never do (a directory selects the upstream scope only).
#[test]
fn plugin_context_workspace_id_selects_linear_mode() {
    let td = TestDaemon::start(&[]);
    let context = format!(
        r#"{{"workspace_id": "wA", "focused_pane_cwd": "{}"}}"#,
        td._dir.path().display()
    );
    let out = td.board_with_env(&["tui"], &[("HERDR_PLUGIN_CONTEXT_JSON", &context)]);
    assert!(!out.status.success());
    assert_eq!(scope_paths(&td), vec![Value::Null]);
}

/// AE10 at the CLI boundary: a daemon that predates `linear.snapshot` (here a
/// listener answering every method but `daemon.status` with the protocol's
/// unknown-method error) receives no project request from `board tui`. The
/// method-not-found rendering itself is proven in the TUI suite.
#[test]
fn a_daemon_without_the_method_receives_no_project_request() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("old-daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_in_server = seen.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_in_server = stop.clone();
    let server = std::thread::spawn(move || {
        while !stop_in_server.load(std::sync::atomic::Ordering::SeqCst) {
            let (stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            };
            stream.set_nonblocking(false).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                let Ok(request) = serde_json::from_str::<Request>(line.trim_end()) else {
                    line.clear();
                    continue;
                };
                seen_in_server.lock().unwrap().push(request.method.clone());
                let response = match request.method.as_str() {
                    "daemon.status" => Response::ok(
                        request.id,
                        serde_json::to_value(DaemonStatus {
                            version: "0.16.0".to_string(),
                            db_path: "old".to_string(),
                            herdr_connected: false,
                            active_runs: 0,
                            queued_runs: 0,
                        })
                        .unwrap(),
                    ),
                    other => Response::err(
                        request.id,
                        1,
                        format!("bad request: unknown method: {other}"),
                    ),
                };
                let mut wire = serde_json::to_string(&response).unwrap();
                wire.push('\n');
                if writer.write_all(wire.as_bytes()).is_err() {
                    break;
                }
                let _ = writer.flush();
                line.clear();
            }
        }
    });

    let out = Command::new(super::BOARD_BIN)
        .arg("tui")
        .current_dir(dir.path())
        .env("BOARD_SOCKET", &socket)
        .env("BOARD_DB", dir.path().join("board.db"))
        .env("HERDR_BOARD_CONFIG", dir.path().join("missing-config.toml"))
        .env("HOME", dir.path())
        .env("HERDR_WORKSPACE_ID", "wA")
        .env_remove("BOARD_SCOPE_PATH")
        .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("BOARD_RUN_ID")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    server.join().unwrap();

    assert!(!out.status.success(), "no terminal: the TUI cannot start");
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .all(|m| m == "daemon.status" || m == "linear.snapshot" || m == "events.subscribe"),
        "only the version read, the snapshot and the event subscription may leave: {seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|m| m.starts_with("project.") || m.starts_with("board.")),
        "{seen:?}"
    );
}
