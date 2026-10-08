//! `board linear report`: the Claude Code PostToolUse hook door. It
//! reads the hook payload on stdin, sends at most one `linear.activity.record`,
//! and exits 0 on every path so a board problem never fails the agent's tool
//! call. stdout stays empty; diagnostics go to stderr.

use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use board_core::client::BoardClient;
use board_core::db::is_issue_identifier;
use board_core::protocol::{ActivityClaims, LinearActivityRecordParams};
use serde_json::Value;

use crate::caller::{env_id, env_text};
use crate::daemon::connect_or_start;

/// Claude Code kills a hook at 30 s. This bounds the whole report, including
/// the up-to-3 s wait in `connect_or_start` for an auto-started boardd.
const REPORT_TIMEOUT: Duration = Duration::from_secs(5);

const READ_PREFIXES: [&str; 4] = ["get_", "list_", "search_", "extract_"];

pub(crate) fn run() {
    let deadline = Instant::now() + REPORT_TIMEOUT;
    let (done, outcome) = mpsc::channel();
    // Detached on purpose: returning from main at the deadline ends the
    // process, and with it a stdin read or daemon call that never finished.
    std::thread::spawn(move || {
        let _ = done.send(report(deadline));
    });
    match outcome.recv_timeout(REPORT_TIMEOUT) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("board linear report: {error:#}"),
        Err(_) => eprintln!(
            "board linear report: gave up after {}s",
            REPORT_TIMEOUT.as_secs()
        ),
    }
}

fn report(deadline: Instant) -> Result<()> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("reading the hook payload")?;
    let payload: Value = serde_json::from_str(&input).context("the hook payload is not JSON")?;
    let current_dir = std::env::current_dir().ok();
    let Some(params) = activity_params(&payload, claims_from_environment(), current_dir) else {
        return Ok(());
    };
    let mut client = connect_or_start()?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        bail!("no time left to reach boardd");
    }
    client.set_read_timeout(Some(remaining))?;
    client.linear_activity_record(&params)?;
    Ok(())
}

fn claims_from_environment() -> ActivityClaims {
    ActivityClaims {
        herdr_socket: env_text("HERDR_SOCKET_PATH"),
        herdr_pane_id: env_text("HERDR_PANE_ID"),
        herdr_workspace_id: env_text("HERDR_WORKSPACE_ID"),
        card_id: env_id("BOARD_CARD_ID"),
        run_id: env_id("BOARD_RUN_ID"),
    }
}

/// `None` means the payload is not a Linear write worth recording: a read, a
/// non-Linear server, or a `save_issue` whose response names no valid issue.
fn activity_params(
    payload: &Value,
    claims: ActivityClaims,
    current_dir: Option<PathBuf>,
) -> Option<LinearActivityRecordParams> {
    let tool_name = payload.get("tool_name")?.as_str()?;
    let (server, tool) = tool_name.strip_prefix("mcp__")?.rsplit_once("__")?;
    if !server.to_ascii_lowercase().contains("linear")
        || READ_PREFIXES.iter().any(|prefix| tool.starts_with(prefix))
    {
        return None;
    }
    let issue = if tool == "save_issue" {
        Some(saved_issue(payload.get("tool_response")?)?)
    } else {
        None
    };
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
        .or(current_dir)
        .map(|cwd| cwd.canonicalize().unwrap_or(cwd))
        .map(|cwd| cwd.to_string_lossy().into_owned());
    Some(LinearActivityRecordParams {
        tool_name: tool_name.to_string(),
        issue,
        space: None,
        cwd,
        claims,
    })
}

/// The claude.ai connector answers with the saved issue as JSON text inside
/// the MCP content array; the `mcp__linear__` server's shape is unverified, so
/// a bare object, a JSON string and a `content`/`structuredContent` wrapper
/// are accepted too.
fn saved_issue(response: &Value) -> Option<String> {
    match response {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .find_map(issue_in_text),
        Value::String(text) => issue_in_text(text),
        Value::Object(object) => issue_in_object(response).or_else(|| {
            ["structuredContent", "content", "issue"]
                .iter()
                .filter_map(|key| object.get(*key))
                .find_map(saved_issue)
        }),
        _ => None,
    }
}

fn issue_in_text(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text)
        .ok()
        .filter(Value::is_object)
        .and_then(|object| saved_issue(&object))
}

/// `id` is the human key on the claude.ai connector and `uuid` Linear's own
/// id; either shape is accepted, anything else is refused rather than cleaned.
fn issue_in_object(object: &Value) -> Option<String> {
    ["identifier", "id", "uuid"]
        .iter()
        .filter_map(|key| object.get(*key).and_then(Value::as_str))
        .find(|value| is_issue_identifier(value))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const UUID: &str = "3f1c1b0e-9f3a-4c61-8d0e-2a4b5c6d7e8f";

    fn params(payload: Value) -> Option<LinearActivityRecordParams> {
        activity_params(&payload, ActivityClaims::default(), None)
    }

    fn content(value: Value) -> Value {
        json!([{"type": "text", "text": value.to_string()}])
    }

    fn save_issue(tool_name: &str, response: Value) -> Value {
        json!({"tool_name": tool_name, "cwd": "/nowhere/at/all", "tool_response": response})
    }

    #[test]
    fn a_connector_save_issue_yields_its_human_identifier() {
        let got = params(save_issue(
            "mcp__claude_ai_Linear__save_issue",
            content(json!({"id": "WEB-3318", "uuid": UUID, "createdAt": "t", "updatedAt": "t"})),
        ))
        .unwrap();
        assert_eq!(got.tool_name, "mcp__claude_ai_Linear__save_issue");
        assert_eq!(got.issue.as_deref(), Some("WEB-3318"));
        assert_eq!(got.cwd.as_deref(), Some("/nowhere/at/all"));
        assert_eq!(got.space, None);
    }

    #[test]
    fn a_uuid_only_response_yields_the_uuid() {
        let got = params(save_issue(
            "mcp__linear__save_issue",
            json!({"id": "opaque", "uuid": UUID}),
        ))
        .unwrap();
        assert_eq!(got.issue.as_deref(), Some(UUID));
    }

    #[test]
    fn wrapped_and_string_responses_are_read_defensively() {
        for response in [
            json!({"content": content(json!({"id": "ENG-1"}))}),
            json!({"structuredContent": {"identifier": "ENG-1"}}),
            json!({"issue": {"identifier": "ENG-1"}}),
            json!(json!({"id": "ENG-1"}).to_string()),
        ] {
            let got = params(save_issue("mcp__linear__save_issue", response.clone()));
            assert_eq!(
                got.and_then(|p| p.issue).as_deref(),
                Some("ENG-1"),
                "{response}"
            );
        }
    }

    #[test]
    fn a_save_issue_without_a_valid_identifier_is_not_reported() {
        for response in [
            content(json!({"id": "WEB-1\u{1b}[31m"})),
            content(json!({"id": "web-1"})),
            json!([{"type": "text", "text": "Error: not found"}]),
            json!(null),
        ] {
            assert!(
                params(save_issue(
                    "mcp__claude_ai_Linear__save_issue",
                    response.clone()
                ))
                .is_none(),
                "{response}"
            );
        }
        assert!(params(json!({"tool_name": "mcp__linear__save_issue"})).is_none());
    }

    #[test]
    fn only_writes_on_a_linear_server_are_kept() {
        for tool_name in [
            "mcp__claude_ai_Linear__get_issue",
            "mcp__claude_ai_Linear__list_comments",
            "mcp__linear__search_documentation",
            "mcp__linear__extract_images",
            "mcp__github__save_issue",
            "mcp__board__mark",
            "Bash",
            "mcp__nosplit",
        ] {
            assert!(
                params(json!({"tool_name": tool_name, "tool_response": []})).is_none(),
                "{tool_name}"
            );
        }
        let comment = params(json!({
            "tool_name": "mcp__plugin_LINEAR_x__save_comment",
            "tool_response": content(json!({"id": "ENG-1"})),
        }))
        .unwrap();
        assert_eq!(comment.issue, None, "only save_issue carries an issue");
    }

    #[test]
    fn cwd_falls_back_to_the_current_directory_and_is_canonicalised() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        let canonical = dir.path().canonicalize().unwrap();

        let from_payload = activity_params(
            &json!({"tool_name": "mcp__linear__save_comment", "cwd": link}),
            ActivityClaims::default(),
            None,
        )
        .unwrap();
        assert_eq!(from_payload.cwd.as_deref(), canonical.to_str());

        let from_process = activity_params(
            &json!({"tool_name": "mcp__linear__save_comment", "cwd": ""}),
            ActivityClaims::default(),
            Some(link),
        )
        .unwrap();
        assert_eq!(from_process.cwd.as_deref(), canonical.to_str());
    }
}
