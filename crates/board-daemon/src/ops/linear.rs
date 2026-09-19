//! `linear.snapshot`: run the work plugin's `bin/work-snapshot.sh` for one
//! herdr space and return the parsed document with live pane status attached.
//!
//! Nothing here logs the child's argv or environment: the environment carries
//! the Linear credential settings the plugin reads.

use super::*;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
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
// per issue page, each bounded by curl's --max-time; the plugin's three herdr
// reads (server status, space list, session snapshot) and one keychain read,
// each bounded by the timeouts set below (the plugin skips the keychain for
// the rest of a run once that read times out); and a margin for process
// startup and the binding scan.
const LINEAR_CALLS_MAX: u64 = 2 + LINEAR_VIEW_PAGE_MAX;
pub(crate) const HERDR_CALLS_MAX: u64 = 3;
pub(crate) const HERDR_CALL_BUDGET_SECONDS: u64 = 5;
pub(crate) const KEYCHAIN_BUDGET_SECONDS: u64 = 5;
const DEADLINE_MARGIN_SECONDS: u64 = 10;
pub(crate) const SCRIPT_WORST_CASE: Duration = Duration::from_secs(
    LINEAR_CALLS_MAX * LINEAR_TIMEOUT_SECONDS
        + HERDR_CALLS_MAX * HERDR_CALL_BUDGET_SECONDS
        + KEYCHAIN_BUDGET_SECONDS,
);
pub(crate) const SCRIPT_DEADLINE: Duration =
    Duration::from_secs(SCRIPT_WORST_CASE.as_secs() + DEADLINE_MARGIN_SECONDS);
/// SIGTERM first so the script's EXIT trap removes its temp directory; SIGKILL
/// after this long.
pub(crate) const TERM_GRACE: Duration = Duration::from_secs(2);
/// A real project's document is about 200 KB; anything past this is not one.
pub(crate) const STDOUT_CAP_BYTES: u64 = 32 * 1024 * 1024;
/// How long the output pipe may stay open after the script and its group are
/// gone before the run is reported as failed.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

const CHILD_POLL: Duration = Duration::from_millis(20);
const EXIT_ARGUMENT_REFUSED: i32 = 2;
const EXIT_NO_SUCH_SPACE: i32 = 3;

/// Everything the handler reads from outside the request. Production builds
/// it from the process; tests inject it so no test touches the process
/// environment.
pub(crate) struct SnapshotRunner {
    pub env: BTreeMap<String, String>,
    pub config_root: Option<PathBuf>,
    pub deadline: Duration,
    /// Polled while the script runs: true stops it (the daemon is stopping, or
    /// the client that asked has gone).
    pub cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl SnapshotRunner {
    pub(crate) fn from_daemon(d: &Arc<Daemon>) -> SnapshotRunner {
        let daemon = d.clone();
        let request = crate::cancel::current();
        SnapshotRunner {
            env: utf8_env(std::env::vars_os()),
            config_root: fresh_config_root(&d.settings),
            deadline: SCRIPT_DEADLINE,
            cancelled: Arc::new(move || {
                daemon.is_shutdown() || request.as_ref().is_some_and(|r| r.is_cancelled())
            }),
        }
    }
}

/// The process environment without the pairs that are not UTF-8. `env::vars`
/// panics on one, and the panic would answer every snapshot with code 5 until
/// the daemon restarts; a variable that cannot be read as text is not one the
/// child is given anyway.
pub(crate) fn utf8_env(
    vars: impl Iterator<Item = (OsString, OsString)>,
) -> BTreeMap<String, String> {
    vars.filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// `[daemon] work_plugin_root` read from the board config now, not when the
/// daemon started, so the code-6 remedy of setting it applies without a
/// restart. The value the daemon started with stands when the file cannot be
/// read.
fn fresh_config_root(settings: &crate::settings::DaemonSettings) -> Option<PathBuf> {
    match board_core::config::RootConfig::load() {
        Ok(root) => root.daemon.work_plugin_root,
        Err(_) => settings.work_plugin_root.clone(),
    }
}

pub(super) fn linear_snapshot(d: &Arc<Daemon>, p: LinearSnapshotParams) -> Result<Value> {
    let runner = SnapshotRunner::from_daemon(d);
    Ok(json!(snapshot(&runner, p)?))
}

pub(crate) fn snapshot(runner: &SnapshotRunner, p: LinearSnapshotParams) -> Result<LinearSnapshot> {
    if p.workspace_id.trim().is_empty() {
        return Err(Error::BadRequest(
            "linear.snapshot requires a non-empty workspace_id".into(),
        ));
    }
    let root = resolve_plugin_root(runner, p.plugin_root.as_deref())?;
    let version = plugin_version(&root)?;
    if semver_triple(&version) < semver_triple(PLUGIN_VERSION_FLOOR) {
        return Err(Error::PluginUnavailable(format!(
            "work plugin {version} at {} is older than the {PLUGIN_VERSION_FLOOR} this board needs; \
             update the plugin or point {PLUGIN_ROOT_ENV} at a newer checkout (read on every request, \
             from the caller's environment first)",
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

/// The caller's `BOARD_WORK_PLUGIN_ROOT` (sent as `plugin_root`), then the
/// daemon's, then the board config, then the installed-plugins record. The
/// client is the same user over the same-user socket, so naming the script
/// the daemon runs grants it nothing it could not run itself.
fn resolve_plugin_root(runner: &SnapshotRunner, requested: Option<&str>) -> Result<PathBuf> {
    if let Some(root) = requested
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            runner
                .env
                .get(PLUGIN_ROOT_ENV)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
        })
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
             board config, or install `{PLUGIN_ID}` so {} lists it (each is read on every request, \
             {PLUGIN_ROOT_ENV} from the caller's environment first)",
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
    child.insert(
        "HERDR_LINEAR_HERDR_TIMEOUT_SECONDS".into(),
        HERDR_CALL_BUDGET_SECONDS.to_string(),
    );
    child.insert(
        "HERDR_LINEAR_KEYCHAIN_TIMEOUT_SECONDS".into(),
        KEYCHAIN_BUDGET_SECONDS.to_string(),
    );
    child
}

/// Whether the child has exited, without reaping it. An exited but unreaped
/// leader keeps its pid, and so its process-group id, from being reused, which
/// is what makes the group signals below safe to send.
fn exited_unreaped(child: &std::process::Child) -> bool {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    rc == 0 && siginfo_pid(&info) != 0
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn siginfo_pid(info: &libc::siginfo_t) -> libc::pid_t {
    unsafe { info.si_pid() }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn siginfo_pid(info: &libc::siginfo_t) -> libc::pid_t {
    info.si_pid
}

fn signal_group(child: &std::process::Child, signal: libc::c_int) {
    // The pid is the pgid: the child leads its group (`process_group(0)`).
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), signal);
    }
}

/// SIGTERM to the group so the script's EXIT trap runs, SIGKILL to whatever
/// is left after `TERM_GRACE`, then reap the leader.
fn stop_group(child: &mut std::process::Child) {
    signal_group(child, libc::SIGTERM);
    let until = Instant::now() + TERM_GRACE;
    while !exited_unreaped(child) && Instant::now() < until {
        std::thread::sleep(CHILD_POLL);
    }
    signal_group(child, libc::SIGKILL);
    let _ = child.wait();
}

/// Read up to `cap` bytes and keep draining past it, so a child writing more
/// cannot block on a full pipe; the second value says the cap was passed.
fn drain<R: Read + Send + 'static>(reader: Option<R>, cap: u64) -> mpsc::Receiver<(Vec<u8>, bool)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut over = false;
        if let Some(mut reader) = reader {
            let _ = (&mut reader).take(cap + 1).read_to_end(&mut buf);
            if buf.len() as u64 > cap {
                over = true;
                buf.truncate(cap as usize);
                let _ = std::io::copy(&mut reader, &mut std::io::sink());
            }
        }
        let _ = tx.send((buf, over));
    });
    rx
}

fn run_script(
    script: &Path,
    workspace_id: &str,
    origin_socket: Option<&str>,
    runner: &SnapshotRunner,
) -> Result<LinearSnapshot> {
    let mut command = Command::new(script);
    // Its own process group, so a stop reaches curl, jq and any other
    // grandchild holding the pipe. stderr is not captured: nothing of it is
    // shown, because a plugin tracing its own run would print the credential
    // it resolves, and no filter can know every shape of that.
    command
        .arg(workspace_id)
        .process_group(0)
        .env_clear()
        .envs(child_env(&runner.env, origin_socket))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| Error::PluginUnavailable(format!("running {}: {e}", script.display())))?;
    let stdout = drain(child.stdout.take(), STDOUT_CAP_BYTES);
    let by_hand = format!(
        "run `{} {workspace_id}` in a shell to see its output",
        script.display()
    );

    let deadline = Instant::now() + runner.deadline;
    loop {
        if exited_unreaped(&child) {
            break;
        }
        if (runner.cancelled)() {
            stop_group(&mut child);
            return Err(Error::PluginUnavailable(
                "work-snapshot.sh was stopped: the daemon is stopping or the client went away"
                    .into(),
            ));
        }
        if Instant::now() >= deadline {
            stop_group(&mut child);
            return Err(Error::PluginUnavailable(format!(
                "work-snapshot.sh timed out after {:?} and was stopped",
                runner.deadline
            )));
        }
        std::thread::sleep(CHILD_POLL);
    }
    // The script is done; a grandchild it left behind is not part of the
    // answer and would hold the pipe open.
    signal_group(&child, libc::SIGKILL);
    let status = child
        .wait()
        .map_err(|e| Error::PluginUnavailable(format!("waiting for work-snapshot.sh: {e}")))?;
    let (stdout, over_cap) = stdout.recv_timeout(DRAIN_GRACE).map_err(|_| {
        Error::PluginUnavailable(format!(
            "work-snapshot.sh {workspace_id} exited and its output stayed open; {by_hand}"
        ))
    })?;

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
            "work-snapshot.sh {workspace_id}: {reason}; {by_hand}"
        )));
    }
    if over_cap {
        return Err(Error::PluginUnavailable(format!(
            "work-snapshot.sh {workspace_id} printed more than {STDOUT_CAP_BYTES} bytes; {by_hand}"
        )));
    }
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Err(Error::PluginUnavailable(format!(
            "work-snapshot.sh {workspace_id} exited 0 and printed no document; {by_hand}"
        )));
    }
    let document: LinearSnapshot = serde_json::from_slice(&stdout).map_err(|e| {
        Error::PluginUnavailable(format!(
            "work-snapshot.sh {workspace_id} printed a document the board cannot parse: {e}; {by_hand}"
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
