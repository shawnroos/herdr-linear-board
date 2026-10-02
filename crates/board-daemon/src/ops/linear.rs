//! `linear.snapshot`, `linear.list` and `linear.issue`, read natively
//! (`native`). A `plugin_root` an older client still sends is ignored.

use super::*;

pub(super) mod native;

use std::collections::BTreeMap;
use std::path::Path;

use board_herdr::AgentStatus;

pub(super) fn linear_snapshot(d: &Arc<Daemon>, p: LinearSnapshotParams) -> Result<Value> {
    Ok(json!(native::snapshot(d, p)?))
}

pub(super) fn linear_list(d: &Arc<Daemon>, p: LinearListParams) -> Result<Value> {
    Ok(json!(native::list(d, p)?))
}

pub(super) fn linear_issue(d: &Arc<Daemon>, p: LinearIssueParams) -> Result<Value> {
    Ok(json!(native::issue(d, p)?))
}

pub(super) fn checked_issue_id(p: &LinearIssueParams) -> Result<&str> {
    let id = p.issue.trim();
    if !is_list_identifier(id) {
        return Err(Error::BadRequest(
            "linear.issue requires an issue id of 1 to 64 ASCII letters, digits, `_` or `-`, \
             starting with a letter or digit"
                .into(),
        ));
    }
    Ok(id)
}

pub(super) fn checked_workspace_id(p: &LinearSnapshotParams) -> Result<&str> {
    let id = p.workspace_id.trim();
    if id.is_empty() {
        return Err(Error::BadRequest(
            "linear.snapshot requires a non-empty workspace_id".into(),
        ));
    }
    Ok(id)
}

/// The list's one id, refused before anything runs when the kind forbids or
/// requires it or its shape is wrong.
pub(super) fn checked_list_id(p: &LinearListParams) -> Result<Option<&str>> {
    let id = p.id.as_deref().filter(|id| !id.is_empty());
    match (p.kind, id) {
        (LinearListKind::Views, None) => Err(Error::BadRequest(
            "linear.list kind views requires a project id".into(),
        )),
        (LinearListKind::Views, Some(id)) if !is_list_identifier(id) => Err(Error::BadRequest(
            "linear.list project id must be 1 to 64 ASCII letters, digits, `_` or `-`, \
             starting with a letter or digit"
                .into(),
        )),
        (LinearListKind::Spaces | LinearListKind::Projects, Some(_)) => Err(Error::BadRequest(
            format!("linear.list kind {} takes no id", kind_name(p.kind)),
        )),
        (_, id) => Ok(id),
    }
}

/// Identifier shape: `^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$`.
pub(super) fn is_list_identifier(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

fn kind_name(kind: LinearListKind) -> &'static str {
    match kind {
        LinearListKind::Spaces => "spaces",
        LinearListKind::Projects => "projects",
        LinearListKind::Views => "views",
    }
}

fn normalized_origin(raw: Option<&str>) -> Option<String> {
    raw.map(|raw| {
        crate::herdr_conn::normalize_socket(Path::new(raw), "origin")
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| raw.to_string())
    })
    .filter(|s| !s.trim().is_empty())
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
