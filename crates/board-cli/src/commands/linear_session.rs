//! `board linear session` and `board linear status-line`: one
//! `linear.session.get` read for the calling agent session. Neither verb
//! starts boardd: the status line runs on every Claude Code refresh, so a
//! down daemon must cost one failed connect, never a start.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use board_core::client::{BoardClient, UnixClient};
use board_core::paths;
use board_core::protocol::{
    parse_timestamp, CallerResolveResult, LinearSessionGetParams, LinearSessionGetResult, MarkKind,
};
use board_core::text::strip_control_and_format;
use serde_json::{json, Value};

use super::canonical_text;
use crate::caller::{candidate_list, env_text, hook_cwd, resolve, CallerQuery, HERDR_WORKSPACE_ID};
use crate::render::{emit, emit_line};

const SESSION_TIMEOUT: Duration = Duration::from_secs(5);
/// Claude Code waits on the command before it redraws; the read and the
/// stdin wait together stay well under a refresh.
const STATUS_LINE_TIMEOUT: Duration = Duration::from_millis(200);
const STDIN_WAIT: Duration = Duration::from_millis(100);

const GLYPH_ORDER: [MarkKind; 4] = [
    MarkKind::Attention,
    MarkKind::Question,
    MarkKind::Suggestion,
    MarkKind::Done,
];

pub(crate) fn session(workspace_id: Option<String>, json: bool) -> Result<()> {
    let space = workspace_id
        .filter(|id| !id.is_empty())
        .or_else(|| env_text(HERDR_WORKSPACE_ID));
    let path = paths::socket_path();
    let connect = || -> Result<UnixClient> {
        let mut client = UnixClient::connect(&path)
            .with_context(|| format!("boardd is not running at {}", path.display()))?;
        client.set_read_timeout(Some(SESSION_TIMEOUT))?;
        Ok(client)
    };
    let Some(space) = space else {
        return located_session(connect, json);
    };
    let params = session_params(space, None);
    let result = connect()?.linear_session_get(&params)?;
    emit(&result, json)
}

/// The SessionStart hook in a session whose env lacks herdr's variables: it
/// uses the pane this Claude session confirmed earlier and never confirms one
/// itself. Every other outcome is a notice and exit 0, so the session still
/// starts with what the board could tell it.
fn located_session(connect: impl Fn() -> Result<UnixClient>, json: bool) -> Result<()> {
    let input = claude_input();
    let field = |key: &str| {
        input
            .as_ref()
            .and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let query = CallerQuery {
        cwd: hook_cwd(field("cwd").as_deref()),
        pane: None,
        claude_session_id: field("session_id").or_else(|| env_text("CLAUDE_CODE_SESSION_ID")),
    };
    let resolution = connect().and_then(|mut client| {
        let resolution = resolve(&query, env_text, |params| client.caller_resolve(params))?;
        Ok((client, resolution.result))
    });
    match resolution {
        Ok((mut client, CallerResolveResult::Resolved { location })) => {
            let mut params = session_params(location.workspace_id, input.as_ref());
            params.herdr_socket = Some(location.socket);
            params.herdr_pane_id = Some(location.pane_id);
            emit(&client.linear_session_get(&params)?, json)
        }
        Ok((_, CallerResolveResult::Unconfirmed { candidates })) => emit_line(
            &json!({"state": "unconfirmed", "candidates": candidates}),
            json,
            format!(
                "board: this session's herdr pane is not confirmed. Claude panes in its folder: \
                 {}. Ask the person which one is this session, then confirm it with `pane` \
                 (`board caller --pane <PANE>` or the `pane` argument of any board tool).",
                candidate_list(&candidates)
            ),
        ),
        Ok((_, CallerResolveResult::NotInHerdr)) => no_space(json, None),
        Err(error) => no_space(json, Some(format!("{error:#}"))),
    }
}

fn no_space(json: bool, reason: Option<String>) -> Result<()> {
    if let Some(reason) = &reason {
        eprintln!("board linear session: {reason}");
    }
    emit_line(
        &json!({"state": "not_in_herdr", "reason": reason}),
        json,
        "board: no herdr space for this session, so there is no board context.",
    )
}

pub(crate) fn status_line(json: bool) {
    let (Some(space), Some(_)) = (
        env_text("HERDR_WORKSPACE_ID"),
        env_text("HERDR_SOCKET_PATH"),
    ) else {
        return;
    };
    let params = session_params(space, claude_input().as_ref());
    let line = match UnixClient::connect(&paths::socket_path()) {
        Err(_) => "board: daemon down".to_string(),
        Ok(mut client) => match client
            .set_read_timeout(Some(STATUS_LINE_TIMEOUT))
            .and_then(|()| client.linear_session_get(&params))
        {
            Ok(result) => line_for(&result, now_secs()),
            Err(_) => "board: no answer".to_string(),
        },
    };
    let _ = emit_line(&json!({ "line": line }), json, line);
}

/// The socket is sent exactly as herdr exported it: the daemon keys its
/// snapshot cache on that raw string, as the TUI's `origin_socket` does.
fn session_params(space: String, input: Option<&Value>) -> LinearSessionGetParams {
    let field = |pointer: &str| {
        input
            .and_then(|v| v.pointer(pointer))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let cwd = field("/workspace/current_dir")
        .or_else(|| field("/cwd"))
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .map(canonical_text);
    LinearSessionGetParams {
        space,
        herdr_socket: env_text("HERDR_SOCKET_PATH"),
        herdr_pane_id: env_text("HERDR_PANE_ID"),
        claude_session_id: field("/session_id").or_else(|| env_text("CLAUDE_CODE_SESSION_ID")),
        cwd,
    }
}

/// The JSON Claude Code pipes to a status-line command. A terminal on stdin
/// (someone running the verb by hand) is never read, and a writer that never
/// closes the pipe costs at most [`STDIN_WAIT`].
fn claude_input() -> Option<Value> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let (done, input) = mpsc::channel();
    // Detached on purpose: a blocked read must not hold the process open.
    std::thread::spawn(move || {
        let mut text = String::new();
        if std::io::stdin().read_to_string(&mut text).is_ok() {
            let _ = done.send(text);
        }
    });
    let text = input.recv_timeout(STDIN_WAIT).ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
}

fn line_for(result: &LinearSessionGetResult, now: i64) -> String {
    let Some(binding) = result.binding.as_ref().filter(|_| result.space_bound) else {
        return "board: not bound".to_string();
    };
    let mut parts = vec![strip_control_and_format(&binding.issue)];
    if let Some(column) = &result.column {
        parts.push(strip_control_and_format(column));
    }
    if let Some(start) = binding.bound_at.as_deref().and_then(parse_timestamp) {
        parts.push(elapsed(now - start));
    }
    let glyphs: String = GLYPH_ORDER
        .iter()
        .filter(|kind| result.marks.iter().any(|m| m.kind == **kind))
        .map(|kind| board_tui::app::mark_glyph(*kind))
        .collect();
    if !glyphs.is_empty() {
        parts.push(glyphs);
    }
    if result.pending_requests > 0 {
        parts.push(format!("◉{}", result.pending_requests));
    }
    parts.join(" · ")
}

fn elapsed(seconds: i64) -> String {
    let minutes = seconds.max(0) / 60;
    match (minutes / 1440, minutes / 60 % 24, minutes % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h{m:02}m"),
        (d, h, _) => format!("{d}d{h}h"),
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::protocol::{LinearSessionBinding, Mark};

    const BOUND_AT: &str = "2026-09-30 10:00:00";

    fn mark(kind: MarkKind) -> Mark {
        serde_json::from_value(json!({
            "id": 1, "space": "wA", "issue": "ENG-148", "kind": kind.as_str(),
            "text": null, "detail": null, "created_by": null, "created_at": BOUND_AT,
            "owner_herdr_socket": null, "owner_herdr_pane_id": null,
            "owner_claude_session_id": null,
        }))
        .unwrap()
    }

    fn bound(column: Option<&str>, marks: &[MarkKind], pending: u32) -> LinearSessionGetResult {
        LinearSessionGetResult {
            space: "wA".into(),
            space_bound: true,
            binding: Some(LinearSessionBinding {
                worktree_path: "/wt".into(),
                issue: "ENG-148".into(),
                bound_at: Some(BOUND_AT.into()),
            }),
            column: column.map(str::to_string),
            marks: marks.iter().copied().map(mark).collect(),
            pending_requests: pending,
        }
    }

    fn at(minutes: i64) -> i64 {
        parse_timestamp(BOUND_AT).unwrap() + minutes * 60
    }

    #[test]
    fn a_bound_session_reads_issue_column_run_time_marks_and_requests() {
        let result = bound(
            Some("In progress"),
            &[
                MarkKind::Question,
                MarkKind::Done,
                MarkKind::Attention,
                MarkKind::Question,
            ],
            1,
        );
        assert_eq!(
            line_for(&result, at(12)),
            "ENG-148 · In progress · 12m · !?✓ · ◉1"
        );
    }

    #[test]
    fn absent_parts_are_left_out() {
        assert_eq!(line_for(&bound(None, &[], 0), at(0)), "ENG-148 · 0m");
        let mut no_start = bound(None, &[MarkKind::Suggestion], 0);
        no_start.binding.as_mut().unwrap().bound_at = None;
        assert_eq!(line_for(&no_start, at(5)), "ENG-148 · ◇");
    }

    #[test]
    fn unbound_reads_as_not_bound() {
        let mut no_binding = bound(None, &[], 3);
        no_binding.binding = None;
        assert_eq!(line_for(&no_binding, at(1)), "board: not bound");
        assert_eq!(
            line_for(&LinearSessionGetResult::default(), at(1)),
            "board: not bound"
        );
    }

    #[test]
    fn linear_text_is_stripped_of_control_characters() {
        let result = bound(Some("Doing\u{1b}[31m\u{202e}"), &[], 0);
        assert_eq!(line_for(&result, at(1)), "ENG-148 · Doing[31m · 1m");
    }

    #[test]
    fn run_time_grows_into_hours_and_days() {
        assert_eq!(elapsed(59), "0m");
        assert_eq!(elapsed(-30), "0m");
        assert_eq!(elapsed(65 * 60), "1h05m");
        assert_eq!(elapsed((26 * 60 + 7) * 60), "1d2h");
    }
}
