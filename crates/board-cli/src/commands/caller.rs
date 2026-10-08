//! `board caller`: the resolver for shell callers such as the work plugin,
//! which cannot call MCP tools. It always exits 0 on an answer; the caller
//! branches on `state`.

use anyhow::{Context, Result};
use board_core::client::BoardClient;
use board_core::protocol::CallerResolveResult;
use serde_json::json;

use super::canonical_text;
use crate::caller::{env_text, resolve, CallerQuery};
use crate::daemon::connect_or_start;
use crate::render::{emit, emit_line};

pub(crate) fn cmd_caller(pane: Option<String>, json: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let query = CallerQuery {
        cwd: canonical_text(cwd),
        pane: pane
            .map(|pane| pane.trim().to_string())
            .filter(|pane| !pane.is_empty()),
        claude_session_id: env_text("CLAUDE_CODE_SESSION_ID"),
    };
    let resolution = resolve(&query, env_text, |params| {
        connect_or_start()?.caller_resolve(params)
    })?;
    match (&resolution.result, &query.pane) {
        (CallerResolveResult::NotInHerdr, Some(pane)) => emit_line(
            &json!({"state": "not_in_herdr", "pane": pane}),
            json,
            format!("not in herdr: no running herdr session lists pane {pane}"),
        ),
        _ => emit(&resolution.result, json),
    }
}
