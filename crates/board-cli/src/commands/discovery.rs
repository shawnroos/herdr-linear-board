use anyhow::{anyhow, bail, Result};
use board_core::client::{BoardClient, UnixClient};
use board_core::paths;
use board_core::protocol::{
    CallerLocation, LinearIssueParams, LinearListKind, LinearListParams, LinearSnapshotParams,
    LINEAR_ISSUE_CLIENT_TIMEOUT, LINEAR_LIST_CLIENT_TIMEOUT, LINEAR_SNAPSHOT_CLIENT_TIMEOUT,
};
use serde_json::json;

use super::canonical_text;

use crate::args::{
    HarnessCmd, LinearCmd, LinearProjectCmd, LinearSpaceCmd, LinearViewCmd, SessionCmd, SpaceCmd,
};
use crate::caller::{
    env_text, resolve_remembered, CLAUDE_SESSION_ENV, HERDR_SOCKET_PATH, HERDR_WORKSPACE_ID,
};
use crate::context::Ctx;
use crate::helpers::{efforts_str, harness_capabilities, union_efforts};
use crate::render::{emit, emit_line};

pub(crate) fn cmd_status(ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    let status = ctx.client()?.daemon_status()?;
    emit(&status, json)
}

pub(crate) fn cmd_harness(sub: HarnessCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        HarnessCmd::List => {
            let names = ctx.client()?.harness_list()?.harnesses;
            emit(&names, json)
        }
        HarnessCmd::Models { harness } => {
            let caps = harness_capabilities(ctx.client()?, &harness)?;
            emit(&caps, json)
        }
        HarnessCmd::Efforts { harness, model } => {
            let caps = harness_capabilities(ctx.client()?, &harness)?;
            let (efforts, known) = match caps.models.iter().find(|m| m.id == model) {
                Some(m) => (m.efforts.clone(), true),
                None if caps.model_freeform => (union_efforts(&caps), false),
                None => bail!("model '{model}' not known to harness '{harness}'"),
            };
            let mut text = efforts_str(&efforts);
            if !known {
                text.push_str(&format!(
                    "\n\n(model '{model}' unknown to {harness} but accepted; \
                     showing all known efforts)"
                ));
            }
            let efforts: Vec<&str> = efforts.iter().map(|e| e.as_str()).collect();
            emit_line(
                &json!({ "model": model, "efforts": efforts, "known": known }),
                json,
                text,
            )
        }
        HarnessCmd::Permissions { harness } => {
            let caps = harness_capabilities(ctx.client()?, &harness)?;
            emit(&caps.permission_modes, json)
        }
    }
}

pub(crate) fn cmd_space(sub: SpaceCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        SpaceCmd::List { session } => {
            let spaces = ctx.client()?.space_list(session.as_deref())?;
            emit(&spaces, json)
        }
    }
}

pub(crate) fn cmd_session(sub: SessionCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        SessionCmd::List => {
            let sessions = ctx.client()?.session_list()?;
            emit(&sessions, json)
        }
    }
}

const RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Asked of a running boardd only: a snapshot never starts one.
fn remembered_location() -> Option<CallerLocation> {
    let cwd = canonical_text(std::env::current_dir().ok()?);
    let mut client = UnixClient::connect(&paths::socket_path()).ok()?;
    client.set_read_timeout(Some(RESOLVE_TIMEOUT)).ok()?;
    resolve_remembered(cwd, env_text(CLAUDE_SESSION_ENV), |params| {
        client.caller_resolve(params)
    })
    .unwrap_or_else(|error| {
        eprintln!("board linear snapshot: pane lookup failed: {error:#}");
        None
    })
}

pub(crate) fn cmd_linear(sub: LinearCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        LinearCmd::Snapshot { workspace_id } => {
            // Resolved before connecting, so a missing id never starts a daemon.
            let (workspace_id, origin_socket) = match workspace_id
                .filter(|id| !id.is_empty())
                .or_else(|| env_text(HERDR_WORKSPACE_ID))
            {
                Some(id) => (id, env_text(HERDR_SOCKET_PATH)),
                None => remembered_location()
                    .map(|location| (location.workspace_id, Some(location.socket)))
                    .ok_or_else(|| anyhow!("no space id given and $HERDR_WORKSPACE_ID is unset"))?,
            };
            let document = with_read_timeout(ctx, LINEAR_SNAPSHOT_CLIENT_TIMEOUT, |client| {
                client.linear_snapshot(&LinearSnapshotParams {
                    workspace_id,
                    origin_socket,
                    force: false,
                })
            })?;
            let text = serde_json::to_string_pretty(&document)?;
            emit_line(&document, json, text)
        }
        LinearCmd::Issue { issue } => {
            let document = with_read_timeout(ctx, LINEAR_ISSUE_CLIENT_TIMEOUT, |client| {
                client.linear_issue(&LinearIssueParams {
                    issue,
                    origin_socket: env_text(HERDR_SOCKET_PATH),
                })
            })?;
            let text = serde_json::to_string_pretty(&document)?;
            emit_line(&document, json, text)
        }
        LinearCmd::Space {
            sub: LinearSpaceCmd::List,
        } => linear_list(ctx, LinearListKind::Spaces, None),
        LinearCmd::Project {
            sub: LinearProjectCmd::List,
        } => linear_list(ctx, LinearListKind::Projects, None),
        LinearCmd::View {
            sub: LinearViewCmd::List { project_id },
        } => linear_list(ctx, LinearListKind::Views, Some(project_id)),
        LinearCmd::Report => {
            super::linear_report::run();
            Ok(())
        }
        LinearCmd::Session { workspace_id } => super::linear_session::session(workspace_id, json),
        LinearCmd::StatusLine => {
            super::linear_session::status_line(json);
            Ok(())
        }
    }
}

fn linear_list(ctx: &mut Ctx, kind: LinearListKind, id: Option<String>) -> Result<()> {
    let json = ctx.json();
    let list = with_read_timeout(ctx, LINEAR_LIST_CLIENT_TIMEOUT, |client| {
        client.linear_list(&LinearListParams {
            kind,
            id,
            origin_socket: env_text(HERDR_SOCKET_PATH),
        })
    })?;
    emit(&list, json)
}

/// A plugin script can outlast the client's default read timeout; the timeout
/// is put back before the call's error is returned.
fn with_read_timeout<T>(
    ctx: &mut Ctx,
    timeout: std::time::Duration,
    call: impl FnOnce(&mut UnixClient) -> Result<T>,
) -> Result<T> {
    let client = ctx.client()?;
    client.set_read_timeout(Some(timeout))?;
    let result = call(client);
    client.set_read_timeout(None)?;
    result
}
