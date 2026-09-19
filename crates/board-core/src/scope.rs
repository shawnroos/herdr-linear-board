//! Resolve the board scope from CLI/plugin context and a filesystem path.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::Result;

#[derive(Debug, Default, Deserialize)]
struct PluginContext {
    focused_pane_cwd: Option<String>,
    workspace_cwd: Option<String>,
    workspace_id: Option<String>,
    tab_id: Option<String>,
    focused_pane_id: Option<String>,
}

/// Where a plugin pane was opened from, as `HERDR_PLUGIN_CONTEXT_JSON` reports
/// it. Every field is nullable there, so every field is optional here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpaceOrigin {
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    pub focused_pane_id: Option<String>,
}

fn plugin_context(plugin_context_json: Option<&str>) -> PluginContext {
    plugin_context_json
        .and_then(|json| serde_json::from_str::<PluginContext>(json).ok())
        .unwrap_or_default()
}

pub fn space_origin(plugin_context_json: Option<&str>) -> SpaceOrigin {
    let context = plugin_context(plugin_context_json);
    SpaceOrigin {
        workspace_id: non_empty(context.workspace_id.as_deref()).map(str::to_string),
        tab_id: non_empty(context.tab_id.as_deref()).map(str::to_string),
        focused_pane_id: non_empty(context.focused_pane_id.as_deref()).map(str::to_string),
    }
}

/// The herdr space the board runs in: `HERDR_WORKSPACE_ID` first, then the
/// plugin context's `workspace_id`, never a directory. Invalid context JSON is
/// an absent context.
pub fn space_identity(
    herdr_workspace_id: Option<&str>,
    plugin_context_json: Option<&str>,
) -> Option<String> {
    if let Some(id) = non_empty(herdr_workspace_id) {
        return Some(id.to_string());
    }
    space_origin(plugin_context_json).workspace_id
}

/// Select the unnormalized scope candidate without reading process-global state.
///
/// Precedence: explicit non-empty override, focused pane cwd, workspace cwd,
/// then the supplied current directory. Invalid plugin JSON is treated as an
/// absent context so callers can safely fall back.
pub fn select_scope_candidate(
    override_path: Option<&str>,
    plugin_context_json: Option<&str>,
    current_dir: &Path,
) -> Result<PathBuf> {
    if let Some(path) = non_empty(override_path) {
        return Ok(PathBuf::from(path));
    }

    let context = plugin_context(plugin_context_json);
    if let Some(path) = non_empty(context.focused_pane_cwd.as_deref()) {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = non_empty(context.workspace_cwd.as_deref()) {
        return Ok(PathBuf::from(path));
    }
    Ok(current_dir.to_path_buf())
}

/// Canonicalize a candidate and use its Git root when it belongs to a repo.
/// A missing/failing Git command deliberately falls back to the canonical cwd.
pub fn resolve_scope_path(candidate: &Path) -> Result<PathBuf> {
    let canonical = candidate.canonicalize()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&canonical)
        .args(["rev-parse", "--show-toplevel"])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let root = String::from_utf8_lossy(&output.stdout);
            let root = root.trim();
            if !root.is_empty() {
                if let Ok(root) = Path::new(root).canonicalize() {
                    return Ok(root);
                }
            }
        }
    }
    Ok(canonical)
}

/// A project must point at an existing directory on disk: creating a project
/// registers its canonical path but never creates directories itself. Shared
/// by the daemon op and the fake client so protocol callers see one rule.
pub fn validate_existing_directory(path: &str) -> Result<()> {
    let p = Path::new(path);
    if !p.is_dir() {
        return Err(crate::Error::BadRequest(format!(
            "project path is not an existing directory: {path}"
        )));
    }
    Ok(())
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
