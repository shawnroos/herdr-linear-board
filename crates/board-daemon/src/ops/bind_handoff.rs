//! `linear.bind_handoff`: open an unfocused `bind` tab in the caller's own herdr
//! session and start an interactive Claude whose first input is the plugin's
//! bind command. The daemon writes nothing; the skill's confirmation is the
//! only write gate.
//!
//! The command travels in `agent.start`'s argv, not through `agent.prompt`:
//! plain `claude` opens Claude Code's agent view, which refuses a typed slash
//! command, and argv cannot be dropped by a prompt sent before the session
//! settles. The agent is addressed by pane id only, because herdr can drop the
//! agent name after its startup timeout while Claude keeps running.

use super::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use board_herdr::{AgentStartParams, HerdrClient, HerdrError, TabCreateParams};

use super::linear::{is_list_identifier, utf8_env};
use crate::spawner::is_pane_not_found;

pub(crate) const BIND_TAB_LABEL: &str = "bind";
pub(crate) const BIND_AGENT_KIND: &str = "claude";
// The plugin's argument form. The tests spell the expected line out rather than
// reading this, so a change here or in the plugin fails a board test.
const BIND_LINE_TEMPLATE: &str = "/work:bind --space <space> --project <project>";
const AGENT_NAME_PREFIX: &str = "bind-";
pub(crate) const AGENT_NAME_MAX: usize = 32;

// Measured on this machine under heavy load: a fresh tab's login shell took
// about 60 s to accept `agent.start`, answering `agent_pane_busy` until then.
// The card-run budget (about 3.1 s) was sized for a split of a warm tab and
// gives up long before that. Do not lower below that 60 s measurement.
pub(crate) const AGENT_START_BUSY_BUDGET: Duration = Duration::from_secs(90);
const AGENT_START_BUSY_FIRST: Duration = Duration::from_millis(250);
const AGENT_START_BUSY_STEP_MAX: Duration = Duration::from_secs(5);
const ERR_AGENT_PANE_BUSY: &str = "agent_pane_busy";

const PROJECTS_ROOT_ENV: &str = "HERDR_LINEAR_PROJECTS_ROOT";
const PROJECTS_ROOT_DEPRECATED_ENV: &str = "HERDR_LINEAR_SLATE_ROOT";
const WORKTREES_ROOT_ENV: &str = "HERDR_LINEAR_WORKTREES_ROOT";

/// Everything the handler reads from outside the request, injected so tests
/// neither sleep nor read the process environment.
pub(crate) struct HandoffRunner {
    pub env: BTreeMap<String, String>,
    pub sleep: Arc<dyn Fn(Duration) + Send + Sync>,
    /// True stops the busy retry: the daemon is stopping or the caller left.
    pub cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl HandoffRunner {
    fn from_daemon(d: &Arc<Daemon>) -> HandoffRunner {
        let daemon = d.clone();
        let request = crate::cancel::current();
        HandoffRunner {
            env: utf8_env(std::env::vars_os()),
            sleep: Arc::new(std::thread::sleep),
            cancelled: Arc::new(move || {
                daemon.is_shutdown() || request.as_ref().is_some_and(|r| r.is_cancelled())
            }),
        }
    }
}

pub(super) fn linear_bind_handoff(d: &Arc<Daemon>, p: LinearBindHandoffParams) -> Result<Value> {
    let runner = HandoffRunner::from_daemon(d);
    Ok(json!(bind_handoff(&runner, p)?))
}

pub(crate) fn bind_handoff(
    runner: &HandoffRunner,
    p: LinearBindHandoffParams,
) -> Result<LinearBindHandoffResult> {
    let line = bind_line(&p)?;
    let cwd = working_directory(&runner.env, p.working_directory.as_deref())?;

    let socket = crate::herdr_conn::normalize_socket(Path::new(&p.origin_socket), "origin")?;
    let mut client = crate::herdr_conn::connect_checked(&socket)
        .map_err(|e| Error::HerdrUnavailable(format!("connecting to Herdr: {e}")))?;
    let spaces = client
        .workspace_list()
        .map_err(|e| Error::HerdrUnavailable(format!("workspace.list: {e}")))?;
    if !spaces.iter().any(|s| s.workspace_id == p.space) {
        return Err(Error::NotFound(format!(
            "space {} is not in the caller's herdr session",
            p.space
        )));
    }

    let created = client
        .tab_create(&TabCreateParams {
            workspace_id: Some(p.space.clone()),
            cwd: Some(cwd.display().to_string()),
            label: Some(BIND_TAB_LABEL.to_string()),
            env: BTreeMap::new(),
            focus: false,
        })
        .map_err(|e| Error::HerdrUnavailable(format!("tab.create in {}: {e}", p.space)))?;
    let tab_id = created.tab.tab_id;
    let pane_id = created.root_pane.pane_id;

    let start = AgentStartParams {
        name: agent_name(&tab_id),
        kind: BIND_AGENT_KIND.to_string(),
        pane_id: pane_id.clone(),
        args: vec![line],
        timeout_ms: None,
    };
    if let Err(error) = start_retrying_busy(&mut client, &start, runner) {
        let message = format!("agent.start {} on {pane_id}: {error}", start.name);
        return Err(Error::HerdrUnavailable(close_new_tab(
            &mut client,
            &pane_id,
            message,
        )));
    }
    Ok(LinearBindHandoffResult { tab_id, pane_id })
}

fn bind_line(p: &LinearBindHandoffParams) -> Result<String> {
    for (field, value) in [
        ("space", Some(p.space.as_str())),
        ("project", Some(p.project.as_str())),
        ("view", p.view.as_deref()),
        ("issue", p.issue.as_deref()),
    ] {
        if value.is_some_and(|id| !is_list_identifier(id)) {
            return Err(Error::BadRequest(format!(
                "linear.bind_handoff {field} must be 1 to 64 ASCII letters, digits, `_` or `-`, \
                 starting with a letter or digit"
            )));
        }
    }
    let mut line = BIND_LINE_TEMPLATE
        .replace("<space>", &p.space)
        .replace("<project>", &p.project);
    match (&p.view, &p.issue) {
        (Some(_), Some(_)) => {
            return Err(Error::BadRequest(
                "linear.bind_handoff takes a view or an issue, not both".into(),
            ))
        }
        (Some(view), None) => line.push_str(&format!(" --view {view}")),
        (None, Some(issue)) => line.push_str(&format!(" --issue {issue}")),
        (None, None) => {}
    }
    Ok(line)
}

/// herdr refuses a name an open agent holds, so the name comes from the new
/// tab's id, mapped onto herdr's rule: lowercase letters, digits, `-` or `_`,
/// at most 32 characters.
pub(crate) fn agent_name(tab_id: &str) -> String {
    let mut name = AGENT_NAME_PREFIX.to_string();
    name.extend(tab_id.chars().map(|c| {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
            c
        } else {
            '-'
        }
    }));
    name.truncate(AGENT_NAME_MAX);
    name
}

/// Containment as the work plugin's `lib/contain.sh` answers it: the directory
/// and each root are fully resolved before comparing, which defeats a symlink
/// out of a root and a sibling whose name starts with the root's. An absent
/// directory means the projects root, where a space or view bind runs.
fn working_directory(env: &BTreeMap<String, String>, requested: Option<&str>) -> Result<PathBuf> {
    let projects = projects_root(env);
    let raw = match requested {
        Some(dir) => PathBuf::from(dir),
        None => projects.clone().ok_or_else(|| {
            Error::BadRequest(
                "linear.bind_handoff has no working_directory and no projects root is set".into(),
            )
        })?,
    };
    let refuse = |why: &str| {
        Error::BadRequest(format!(
            "linear.bind_handoff working_directory {} {why}",
            raw.display()
        ))
    };
    // Checked before canonicalising: a relative path would resolve against the
    // daemon's own cwd and could pass.
    if !raw.is_absolute() {
        return Err(refuse("is not an absolute path"));
    }
    let resolved = std::fs::canonicalize(&raw)
        .ok()
        .filter(|p| p.is_dir())
        .ok_or_else(|| refuse("is not an existing directory"))?;
    let inside = [projects, worktrees_root(env)]
        .into_iter()
        .flatten()
        .filter(|root| root.is_absolute())
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .any(|root| resolved.starts_with(root));
    if !inside {
        return Err(refuse(
            "is not inside the work plugin's projects root or worktrees root",
        ));
    }
    Ok(resolved)
}

/// The shell's `${VAR:-default}`: an empty value reads as unset.
fn env_value<'a>(env: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(String::as_str).filter(|v| !v.is_empty())
}

fn projects_root(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    env_value(env, PROJECTS_ROOT_ENV)
        .or_else(|| env_value(env, PROJECTS_ROOT_DEPRECATED_ENV))
        .map(PathBuf::from)
        .or_else(|| env_value(env, "HOME").map(|home| Path::new(home).join("projects")))
}

fn worktrees_root(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    env_value(env, WORKTREES_ROOT_ENV)
        .map(PathBuf::from)
        .or_else(|| env_value(env, "HOME").map(|home| Path::new(home).join("worktrees")))
}

fn is_protocol_error(error: &HerdrError, wanted: &str) -> bool {
    matches!(error, HerdrError::Protocol { code, .. } if code == wanted)
}

fn start_retrying_busy(
    client: &mut HerdrClient,
    start: &AgentStartParams,
    runner: &HandoffRunner,
) -> std::result::Result<(), HerdrError> {
    let mut waited = Duration::ZERO;
    let mut step = AGENT_START_BUSY_FIRST;
    loop {
        match client.agent_start(start) {
            Err(error) if is_protocol_error(&error, ERR_AGENT_PANE_BUSY) => {
                let wait = step.min(AGENT_START_BUSY_BUDGET.saturating_sub(waited));
                if wait.is_zero() || (runner.cancelled)() {
                    return Err(error);
                }
                (runner.sleep)(wait);
                waited += wait;
                step = (step * 2).min(AGENT_START_BUSY_STEP_MAX);
            }
            result => return result.map(|_| ()),
        }
    }
}

/// `cleanup_new_card_tab`'s compensation: closing a new tab's only pane closes
/// the tab, a pane already gone counts as closed, and a failed close is added
/// to the message rather than replacing it.
fn close_new_tab(client: &mut HerdrClient, pane_id: &str, message: String) -> String {
    match client.pane_close(pane_id) {
        Ok(()) => message,
        Err(error) if is_pane_not_found(&error) => message,
        Err(error) => format!(
            "{message}; additionally failed to close the new bind tab's pane {pane_id}: {error}"
        ),
    }
}
