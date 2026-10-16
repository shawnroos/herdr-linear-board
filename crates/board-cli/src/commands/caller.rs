//! `board caller`: the resolver for shell callers such as the work plugin,
//! which cannot call MCP tools. It always exits 0 on an answer; the caller
//! branches on `state`.

use anyhow::{Context, Result};
use board_core::client::BoardClient;
use board_core::protocol::CallerResolveResult;
use serde_json::json;

use super::canonical_text;
use crate::caller::{env_text, resolve, CallerQuery, CLAUDE_SESSION_ENV};
use crate::daemon::connect_or_start;
use crate::render::{emit, emit_line};

pub(crate) fn cmd_caller(pane: Option<String>, json: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let query = CallerQuery::new(canonical_text(cwd), pane, env_text(CLAUDE_SESSION_ENV));
    let resolution = resolve(&query, env_text, |params| {
        connect_or_start()?.caller_resolve(params)
    })?;
    match (&resolution.result, query.pane()) {
        (CallerResolveResult::NotInHerdr, Some(pane)) => emit_line(
            &json!({"state": "not_in_herdr", "pane": pane}),
            json,
            format!("not in herdr: no running herdr session lists pane {pane}"),
        ),
        _ => emit(&resolution.result, json),
    }
}
