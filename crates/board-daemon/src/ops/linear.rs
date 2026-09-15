//! `linear.snapshot`: run the work plugin's `bin/work-snapshot.sh` for one
//! herdr space and return the parsed document with live pane status attached.
//!
//! Nothing here logs the child's argv or environment: the environment carries
//! the Linear credential settings the plugin reads.

use super::*;

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use board_herdr::AgentStatus;

use crate::herdr_snapshot::snapshot_pane_statuses;

pub(crate) const PLUGIN_VERSION_FLOOR: &str = "0.3.0";
pub(crate) const PLUGIN_ROOT_ENV: &str = "BOARD_WORK_PLUGIN_ROOT";
pub(crate) const PLUGIN_ROOT_TOML_KEY: &str = "[daemon] work_plugin_root";
pub(crate) const INSTALLED_PLUGINS_RELATIVE: &str = ".claude/plugins/installed_plugins.json";
const PLUGIN_ID: &str = "work@shrimpshack";
const PLUGIN_MANIFEST_RELATIVE: &str = ".claude-plugin/plugin.json";
const SCRIPT_RELATIVE: &str = "bin/work-snapshot.sh";

// The knobs the daemon sets in the child. `HERDR_LINEAR_RETRY_MAX=1` is one
// attempt: `lib/linear.sh` returns rate-limited once `attempt >= RETRY_MAX`,
// so no backoff sleep ever runs. With the plugin defaults (3 attempts, 500ms
// base) one rate-limited query costs 25.5s, which no deadline can absorb.
pub(crate) const LINEAR_TIMEOUT_SECONDS: u64 = 8;
pub(crate) const LINEAR_RETRY_MAX: u64 = 1;
pub(crate) const LINEAR_VIEW_PAGE_MAX: u64 = 10;

// Worst case the knobs allow: the project and view reads plus one Linear call
// per issue page, each bounded by curl's --max-time; two herdr reads (space
// list and session snapshot) the plugin budgets at 5s each; and a margin for
// jq and process startup.
const LINEAR_CALLS_MAX: u64 = 2 + LINEAR_VIEW_PAGE_MAX;
const HERDR_CALLS_MAX: u64 = 2;
const HERDR_CALL_BUDGET_SECONDS: u64 = 5;
const DEADLINE_MARGIN_SECONDS: u64 = 10;
pub(crate) const SCRIPT_WORST_CASE: Duration = Duration::from_secs(
    LINEAR_CALLS_MAX * LINEAR_TIMEOUT_SECONDS + HERDR_CALLS_MAX * HERDR_CALL_BUDGET_SECONDS,
);
pub(crate) const SCRIPT_DEADLINE: Duration =
    Duration::from_secs(SCRIPT_WORST_CASE.as_secs() + DEADLINE_MARGIN_SECONDS);

const CHILD_POLL: Duration = Duration::from_millis(20);
const STDERR_TAIL_BYTES: usize = 2048;
const EXIT_ARGUMENT_REFUSED: i32 = 2;
const EXIT_NO_SUCH_SPACE: i32 = 3;

/// Everything the handler reads from outside the request. Production builds
/// it from the process; tests inject it so no test touches the process
/// environment.
pub(crate) struct SnapshotRunner {
    pub env: BTreeMap<String, String>,
    pub config_root: Option<PathBuf>,
    pub deadline: Duration,
}

impl SnapshotRunner {
    pub(crate) fn from_process(settings: &crate::settings::DaemonSettings) -> SnapshotRunner {
        SnapshotRunner {
            env: std::env::vars().collect(),
            config_root: settings.work_plugin_root.clone(),
            deadline: SCRIPT_DEADLINE,
        }
    }
}

pub(super) fn linear_snapshot(d: &Arc<Daemon>, p: LinearSnapshotParams) -> Result<Value> {
    let runner = SnapshotRunner::from_process(&d.settings);
    Ok(json!(snapshot(&runner, p)?))
}

pub(crate) fn snapshot(runner: &SnapshotRunner, p: LinearSnapshotParams) -> Result<LinearSnapshot> {
    if p.workspace_id.trim().is_empty() {
        return Err(Error::BadRequest(
            "linear.snapshot requires a non-empty workspace_id".into(),
        ));
    }
    let root = resolve_plugin_root(runner)?;
    let version = plugin_version(&root)?;
    if semver_triple(&version) < semver_triple(PLUGIN_VERSION_FLOOR) {
        return Err(Error::PluginUnavailable(format!(
            "work plugin {version} at {} is older than the {PLUGIN_VERSION_FLOOR} this board needs; \
             update the plugin or point {PLUGIN_ROOT_ENV} at a newer checkout (the daemon reads \
             {PLUGIN_ROOT_ENV} and the board config at start: run `board daemon stop` after changing them)",
            root.display()
        )));
    }
    let script = root.join(SCRIPT_RELATIVE);
    if !script.is_file() {
        return Err(Error::PluginUnavailable(format!(
            "work plugin {version} at {} has no {SCRIPT_RELATIVE}",
            root.display()
        )));
    }

    let origin_socket = p
        .origin_socket
        .as_deref()
        .map(|raw| {
            crate::herdr_conn::normalize_socket(Path::new(raw), "origin")
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| raw.to_string())
        })
        .filter(|s| !s.trim().is_empty());

    let mut document = run_script(&script, &p.workspace_id, origin_socket.as_deref(), runner)?;
    document.pane_status = pane_statuses(origin_socket.as_deref(), &document.pane_ids());
    Ok(document)
}

fn resolve_plugin_root(runner: &SnapshotRunner) -> Result<PathBuf> {
    if let Some(root) = runner
        .env
        .get(PLUGIN_ROOT_ENV)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return Ok(PathBuf::from(root));
    }
    if let Some(root) = runner.config_root.as_ref() {
        return Ok(root.clone());
    }
    let installed = runner
        .env
        .get("HOME")
        .map(|home| Path::new(home).join(INSTALLED_PLUGINS_RELATIVE));
    let candidate = installed.as_deref().and_then(installed_user_root);
    match candidate {
        Some(root) => Ok(root),
        None => Err(Error::PluginUnavailable(format!(
            "work plugin root not found: set {PLUGIN_ROOT_ENV}, or {PLUGIN_ROOT_TOML_KEY} in the \
             board config, or install `{PLUGIN_ID}` so {} lists it (the daemon reads {PLUGIN_ROOT_ENV} and \
             the board config at start: run `board daemon stop` after changing them)",
            installed
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| format!("$HOME/{INSTALLED_PLUGINS_RELATIVE}"))
        ))),
    }
}

fn installed_user_root(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: Value = serde_json::from_str(&text).ok()?;
    let records = doc.get("plugins")?.get(PLUGIN_ID)?.as_array()?;
    records
        .iter()
        .find(|record| record.get("scope").and_then(Value::as_str) == Some("user"))
        .and_then(|record| record.get("installPath").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn plugin_version(root: &Path) -> Result<String> {
    let manifest = root.join(PLUGIN_MANIFEST_RELATIVE);
    let text = std::fs::read_to_string(&manifest).map_err(|e| {
        Error::PluginUnavailable(format!(
            "work plugin root {} has no readable {PLUGIN_MANIFEST_RELATIVE}: {e}",
            root.display()
        ))
    })?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| {
        Error::PluginUnavailable(format!("{} is not JSON: {e}", manifest.display()))
    })?;
    doc.get("version")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            Error::PluginUnavailable(format!("{} has no `version` string", manifest.display()))
        })
}

/// `major.minor.patch` as a comparable triple; a missing or non-numeric
/// component reads as 0, so a malformed version compares below the floor.
fn semver_triple(version: &str) -> (u64, u64, u64) {
    let mut parts = version
        .trim()
        .split('.')
        .map(|part| part.trim().parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// The child environment, built from scratch: the prefix-forwarded Linear
/// settings keep the daemon's run identical to an agent's run of the same
/// script. `HERDR_BIN` is set only from a usable `HERDR_BIN_PATH` because the
/// plugin treats a set-but-unusable override as "no herdr" without falling
/// back to PATH.
pub(crate) fn child_env(
    env: &BTreeMap<String, String>,
    origin_socket: Option<&str>,
) -> BTreeMap<String, String> {
    let mut child = BTreeMap::new();
    for key in ["HOME", "PATH"] {
        if let Some(value) = env.get(key) {
            child.insert(key.to_string(), value.clone());
        }
    }
    for (key, value) in env {
        if key.starts_with("HERDR_LINEAR_") || key.starts_with("LINEAR_") {
            child.insert(key.clone(), value.clone());
        }
    }
    if let Some(socket) = origin_socket {
        child.insert("HERDR_SOCKET_PATH".into(), socket.to_string());
    }
    if let Some(bin) = env
        .get("HERDR_BIN_PATH")
        .filter(|bin| !bin.is_empty() && Path::new(bin).is_file())
    {
        child.insert("HERDR_BIN".into(), bin.clone());
    }
    child.insert(
        "HERDR_LINEAR_TIMEOUT_SECONDS".into(),
        LINEAR_TIMEOUT_SECONDS.to_string(),
    );
    child.insert(
        "HERDR_LINEAR_RETRY_MAX".into(),
        LINEAR_RETRY_MAX.to_string(),
    );
    child.insert(
        "HERDR_LINEAR_VIEW_PAGE_MAX".into(),
        LINEAR_VIEW_PAGE_MAX.to_string(),
    );
    child
}

fn kill_group(child: &mut std::process::Child) {
    // SIGKILL to the group the child leads (`process_group(0)` above); the pid
    // is the pgid. Then reap the leader so no zombie outlives the request.
    let pgid = child.id() as libc::pid_t;
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn drain<R: Read + Send + 'static>(reader: Option<R>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut reader) = reader {
            let _ = reader.read_to_end(&mut buf);
        }
        buf
    })
}

fn run_script(
    script: &Path,
    workspace_id: &str,
    origin_socket: Option<&str>,
    runner: &SnapshotRunner,
) -> Result<LinearSnapshot> {
    let mut command = Command::new(script);
    // Its own process group, so the deadline kill below reaches curl, jq and
    // any other grandchild holding the pipes; `child.kill()` alone would not.
    command
        .arg(workspace_id)
        .process_group(0)
        .env_clear()
        .envs(child_env(&runner.env, origin_socket))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| Error::PluginUnavailable(format!("running {}: {e}", script.display())))?;

    // Drained on threads so a child that fills a pipe cannot stall until the
    // deadline; the threads are not joined on the kill path so a grandchild
    // holding the write end cannot extend it.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let deadline = Instant::now() + runner.deadline;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                kill_group(&mut child);
                return Err(Error::PluginUnavailable(format!(
                    "work-snapshot.sh timed out after {:?} and was killed",
                    runner.deadline
                )));
            }
            Ok(None) => std::thread::sleep(CHILD_POLL),
            Err(e) => {
                kill_group(&mut child);
                return Err(Error::PluginUnavailable(format!(
                    "waiting for work-snapshot.sh: {e}"
                )));
            }
        }
    };

    let stdout = stdout.join().unwrap_or_default();
    let stderr_tail = stderr_tail(&stderr.join().unwrap_or_default());

    if !status.success() {
        let reason = match status.code() {
            Some(EXIT_ARGUMENT_REFUSED) => "the space id argument was refused".to_string(),
            Some(EXIT_NO_SUCH_SPACE) => {
                "herdr lists no such space and no record exists".to_string()
            }
            Some(code) => format!("the script crashed with exit code {code}"),
            None => "the script was killed by a signal".to_string(),
        };
        return Err(Error::PluginUnavailable(format!(
            "work-snapshot.sh {workspace_id}: {reason}; stderr: {stderr_tail}"
        )));
    }
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Err(Error::PluginUnavailable(format!(
            "work-snapshot.sh {workspace_id} exited 0 and printed no document; stderr: {stderr_tail}"
        )));
    }
    let document: LinearSnapshot = serde_json::from_slice(&stdout).map_err(|e| {
        Error::PluginUnavailable(format!(
            "work-snapshot.sh {workspace_id} printed a document the board cannot parse: {e}; \
             stderr: {stderr_tail}"
        ))
    })?;
    if document.schema != 1 {
        return Err(Error::PluginUnavailable(format!(
            "work-snapshot.sh {workspace_id} printed schema {} and this board reads schema 1",
            document.schema
        )));
    }
    Ok(document)
}

/// The last ~2 KB of stderr with every C0/C1 control except newline removed,
/// so the tail can be shown in a terminal and logged without escape sequences.
fn is_format_or_bidi(c: char) -> bool {
    matches!(
        c as u32,
        0xAD | 0x061C | 0x180E | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0xFEFF
            | 0xFFF9..=0xFFFB | 0xE0000..=0xE007F
    )
}

pub(crate) fn stderr_tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    // Cc plus the format and bidi codepoints the TUI's sanitiser strips: the
    // tail reaches the CLI's error output raw.
    let cleaned: String = text
        .chars()
        .filter(|c| *c == '\n' || !(c.is_control() || is_format_or_bidi(*c)))
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return "(empty)".to_string();
    }
    let mut start = trimmed.len().saturating_sub(STDERR_TAIL_BYTES);
    while start > 0 && !trimmed.is_char_boundary(start) {
        start += 1;
    }
    trimmed[start..].to_string()
}

fn agent_status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Unknown => "unknown",
    }
}

/// Best effort: with no origin socket or any failure, every named pane is
/// `unknown` rather than an error, so a partial document renders. The origin
/// socket is what the caller runs inside; the daemon's own startup handle may
/// point elsewhere or be absent, so it is not consulted.
fn pane_statuses(origin_socket: Option<&str>, pane_ids: &[String]) -> BTreeMap<String, String> {
    let live = match origin_socket {
        Some(socket) if !pane_ids.is_empty() => {
            crate::herdr_conn::connect_checked(Path::new(socket))
                .and_then(|mut client| client.session_snapshot())
                .map(snapshot_pane_statuses)
                .ok()
        }
        _ => None,
    };
    pane_ids
        .iter()
        .map(|id| {
            let status = live
                .as_ref()
                .and_then(|statuses| statuses.get(id).copied())
                .map(agent_status_name)
                .unwrap_or("unknown");
            (id.clone(), status.to_string())
        })
        .collect()
}
