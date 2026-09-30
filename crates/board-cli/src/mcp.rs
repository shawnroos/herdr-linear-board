//! `board mcp`: a stdio MCP server that forwards each tool call to one boardd
//! method. It holds no state and validates nothing the daemon validates.
//! stdout carries only the MCP JSON-RPC stream, so nothing here may print
//! through `render.rs`; diagnostics go to stderr.

use anyhow::{anyhow, Context, Result};
use board_core::client::{BoardClient, RpcClientError, UnixClient};
use board_core::protocol::{
    ActivityClaims, BoardNotifyParams, BoardPaneCloseParams, BoardPaneContext, BoardPaneOpenParams,
    LinearActivityListParams, LinearBindParams, LinearMarkSetParams, LinearNoteSetParams,
    LinearShowRequestParams, LinearStateGetParams, LinearUnbindParams, MarkKind,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::connect_or_start;

/// Must not contain "linear": the work plugin's PostToolUse matcher is
/// `mcp__.*[Ll][Ii][Nn][Ee][Aa][Rr].*__.*`, and a match would report this
/// server's own tools back to the board as Linear writes.
const SERVER_NAME: &str = "board";
const ACTIVITY_LIMIT: usize = 500;

pub(crate) fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the MCP runtime")?;
    runtime.block_on(async {
        let service = BoardMcp
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|error| anyhow!("MCP initialize failed: {error}"))?;
        service
            .waiting()
            .await
            .map_err(|error| anyhow!("MCP server stopped: {error}"))?;
        Ok(())
    })
}

struct Caller {
    claims: ActivityClaims,
}

impl Caller {
    fn from_environment() -> Caller {
        Caller {
            claims: ActivityClaims {
                herdr_socket: env_text("HERDR_SOCKET_PATH"),
                herdr_pane_id: env_text("HERDR_PANE_ID"),
                herdr_workspace_id: env_text("HERDR_WORKSPACE_ID"),
                card_id: env_id("BOARD_CARD_ID"),
                run_id: env_id("BOARD_RUN_ID"),
            },
        }
    }

    fn space(&self, given: Option<String>) -> String {
        given
            .or_else(|| self.claims.herdr_workspace_id.clone())
            .unwrap_or_default()
    }

    fn author(&self) -> String {
        match (&self.claims.card_id, &self.claims.herdr_pane_id) {
            (Some(card), _) => format!("card {card}"),
            (None, Some(pane)) => format!("agent in pane {pane}"),
            (None, None) => "agent".to_string(),
        }
    }

    fn origin_socket(&self) -> String {
        self.claims.herdr_socket.clone().unwrap_or_default()
    }

    fn origin_pane(&self) -> String {
        self.claims.herdr_pane_id.clone().unwrap_or_default()
    }
}

fn env_text(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// An empty or malformed id is no claim: a rescued pane carries an empty
/// `BOARD_RUN_ID` on purpose, so its calls attribute to the card only.
fn env_id(key: &str) -> Option<i64> {
    env_text(key).and_then(|value| value.trim().parse().ok())
}

fn cwd_or_current(given: Option<String>) -> String {
    given.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// Runs one daemon call on a blocking thread. The connection is dialed here,
/// never at startup, so `initialize` and `tools/list` do not start boardd.
async fn forward<T, F>(call: F) -> CallToolResult
where
    T: Serialize + Send + 'static,
    F: FnOnce(&mut UnixClient) -> Result<T> + Send + 'static,
{
    let outcome = tokio::task::spawn_blocking(move || -> Result<Value> {
        let mut client = connect_or_start()?;
        Ok(serde_json::to_value(call(&mut client)?)?)
    })
    .await
    .map_err(|error| anyhow!("board call did not finish: {error}"))
    .and_then(|result| result);
    match outcome {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => tool_error(&error),
    }
}

fn tool_error(error: &anyhow::Error) -> CallToolResult {
    let rpc = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RpcClientError>());
    let body = match rpc {
        Some(rpc) => json!({
            "code": rpc.code,
            "kind": rpc.kind,
            "message": rpc.message,
            "details": rpc.details,
        }),
        None => json!({"message": format!("{error:#}")}),
    };
    let mut result = CallToolResult::structured_error(body);
    let text = match rpc {
        Some(rpc) => format!("board refused (code {}): {}", rpc.code, rpc.message),
        None => format!("board unavailable: {error:#}"),
    };
    result.content = vec![rmcp::model::ContentBlock::text(text)];
    result
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SpaceArgs {
    /// herdr workspace id; defaults to this pane's HERDR_WORKSPACE_ID.
    space: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueArgs {
    /// Linear issue key, for example ENG-123.
    issue: String,
    /// herdr workspace id; defaults to this pane's HERDR_WORKSPACE_ID.
    space: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BindArgs {
    /// Linear issue key to bind this worktree to.
    issue: String,
    /// A path inside the git worktree to bind; defaults to this session's working directory.
    cwd: Option<String>,
    /// herdr workspace id; defaults to this pane's HERDR_WORKSPACE_ID.
    space: Option<String>,
    branch: Option<String>,
    tab: Option<String>,
    display_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UnbindArgs {
    /// A path inside the bound git worktree; defaults to this session's working directory.
    cwd: Option<String>,
    /// herdr workspace id; defaults to this pane's HERDR_WORKSPACE_ID.
    space: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MarkArgs {
    /// Linear issue key of the card to flag.
    issue: String,
    /// Short text shown on the card, for example "needs you", "done" or "has a question".
    text: Option<String>,
    /// herdr workspace id; defaults to this pane's HERDR_WORKSPACE_ID.
    space: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NoteArgs {
    /// Linear issue key of the card.
    issue: String,
    /// The note; it replaces this caller's earlier note on the card.
    body: String,
    /// herdr workspace id; defaults to this pane's HERDR_WORKSPACE_ID.
    space: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NotifyArgs {
    title: String,
    body: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AskToShowArgs {
    /// Linear issue key to ask the person to look at.
    issue: String,
    /// Why, in a few words.
    reason: Option<String>,
    /// herdr workspace id; defaults to this pane's HERDR_WORKSPACE_ID.
    space: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct OpenBoardArgs {
    /// "split" (beside this pane, the default) or "tab". The board refuses overlay, popup and zoomed.
    placement: Option<String>,
    /// herdr workspace id to show.
    space: Option<String>,
    /// Linear issue key to show.
    issue: Option<String>,
    /// Board card id to show.
    card: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CloseBoardArgs {
    /// The same space, issue and card the board was opened with.
    space: Option<String>,
    issue: Option<String>,
    card: Option<i64>,
}

#[derive(Debug, Clone)]
struct BoardMcp;

#[tool_router]
impl BoardMcp {
    #[tool(
        description = "Show the board's local state for a space: bindings, marks, notes and pending show-requests.",
        annotations(read_only_hint = true)
    )]
    async fn state(&self, Parameters(args): Parameters<SpaceArgs>) -> CallToolResult {
        let params = LinearStateGetParams {
            space: Caller::from_environment().space(args.space),
        };
        forward(move |c| c.linear_state_get(&params)).await
    }

    #[tool(
        description = "List the herdr panes that recorded Linear activity on an issue, with their session, workspace, card and run.",
        annotations(read_only_hint = true)
    )]
    async fn panes_for_issue(&self, Parameters(args): Parameters<IssueArgs>) -> CallToolResult {
        let params = LinearActivityListParams {
            space: Some(Caller::from_environment().space(args.space)),
            limit: Some(ACTIVITY_LIMIT),
        };
        let issue = args.issue;
        forward(move |c| {
            let listed = c.linear_activity_list(&params)?;
            Ok(panes_for(&issue, listed.activity))
        })
        .await
    }

    #[tool(
        description = "Bind the git worktree this session works in to a Linear issue. Returns the binding before and after; unbind undoes it.",
        annotations(read_only_hint = false)
    )]
    async fn bind(&self, Parameters(args): Parameters<BindArgs>) -> CallToolResult {
        let params = LinearBindParams {
            cwd: cwd_or_current(args.cwd),
            issue: args.issue,
            space: args.space,
            branch: args.branch,
            tab: args.tab,
            display_name: args.display_name,
            claims: Caller::from_environment().claims,
        };
        forward(move |c| c.linear_bind(&params)).await
    }

    #[tool(
        description = "Remove this worktree's issue binding. Returns the binding it removed.",
        annotations(read_only_hint = false)
    )]
    async fn unbind(&self, Parameters(args): Parameters<UnbindArgs>) -> CallToolResult {
        let params = LinearUnbindParams {
            cwd: cwd_or_current(args.cwd),
            space: args.space,
            claims: Caller::from_environment().claims,
        };
        forward(move |c| c.linear_unbind(&params)).await
    }

    #[tool(
        description = "Flag a card for the person's attention. Replaces the card's earlier flag and returns both.",
        annotations(read_only_hint = false)
    )]
    async fn mark(&self, Parameters(args): Parameters<MarkArgs>) -> CallToolResult {
        let caller = Caller::from_environment();
        let params = LinearMarkSetParams {
            space: caller.space(args.space),
            issue: args.issue,
            kind: MarkKind::Attention,
            text: args.text,
            created_by: Some(caller.author()),
            owner: Default::default(),
        };
        forward(move |c| c.linear_mark_set(&params)).await
    }

    #[tool(
        description = "Attach a short note to a card. Replaces this caller's earlier note and returns both.",
        annotations(read_only_hint = false)
    )]
    async fn note(&self, Parameters(args): Parameters<NoteArgs>) -> CallToolResult {
        let caller = Caller::from_environment();
        let params = LinearNoteSetParams {
            space: caller.space(args.space),
            issue: args.issue,
            body: args.body,
            author: caller.author(),
            owner: Default::default(),
        };
        forward(move |c| c.linear_note_set(&params)).await
    }

    #[tool(
        description = "Send a herdr notification in this session.",
        annotations(read_only_hint = false)
    )]
    async fn notify(&self, Parameters(args): Parameters<NotifyArgs>) -> CallToolResult {
        let params = BoardNotifyParams {
            origin_socket: Caller::from_environment().origin_socket(),
            title: args.title,
            body: args.body,
        };
        forward(move |c| c.board_notify(&params)).await
    }

    #[tool(
        description = "Ask the person to look at an issue. The board shows the request; the view moves only when the person accepts it.",
        annotations(read_only_hint = false)
    )]
    async fn ask_to_show(&self, Parameters(args): Parameters<AskToShowArgs>) -> CallToolResult {
        let caller = Caller::from_environment();
        let params = LinearShowRequestParams {
            space: caller.space(args.space),
            issue: args.issue,
            reason: args.reason,
            requested_by: Some(caller.author()),
            owner: Default::default(),
        };
        forward(move |c| c.linear_show_request(&params)).await
    }

    #[tool(
        description = "Open a board beside this pane (split) or in a new tab, showing a space, issue or card, without taking focus. Reuses a board already open for the same context.",
        annotations(read_only_hint = false)
    )]
    async fn open_board(&self, Parameters(args): Parameters<OpenBoardArgs>) -> CallToolResult {
        let caller = Caller::from_environment();
        let params = BoardPaneOpenParams {
            context: BoardPaneContext {
                space: args.space,
                issue: args.issue,
                card: args.card,
            },
            placement: args.placement.unwrap_or_else(|| "split".to_string()),
            origin_socket: caller.origin_socket(),
            origin_pane: caller.origin_pane(),
        };
        forward(move |c| c.board_pane_open(&params)).await
    }

    #[tool(
        description = "Close a board this server opened, named by the context it was opened with.",
        annotations(read_only_hint = false)
    )]
    async fn close_board(&self, Parameters(args): Parameters<CloseBoardArgs>) -> CallToolResult {
        let params = BoardPaneCloseParams {
            context: BoardPaneContext {
                space: args.space,
                issue: args.issue,
                card: args.card,
            },
            origin_socket: Caller::from_environment().origin_socket(),
        };
        forward(move |c| c.board_pane_close(&params)).await
    }
}

#[tool_handler]
impl ServerHandler for BoardMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
    }
}

fn panes_for(issue: &str, activity: Vec<board_core::protocol::Activity>) -> Value {
    let rows: Vec<_> = activity
        .into_iter()
        .filter(|row| row.issue.as_deref() == Some(issue))
        .collect();
    let mut panes: Vec<Value> = Vec::new();
    for row in &rows {
        let claims = &row.claims;
        if claims.herdr_pane_id.is_none() {
            continue;
        }
        let seen = panes.iter().any(|pane| {
            pane["herdr_socket"] == json!(claims.herdr_socket)
                && pane["herdr_pane_id"] == json!(claims.herdr_pane_id)
        });
        if !seen {
            panes.push(json!({
                "herdr_socket": claims.herdr_socket,
                "herdr_pane_id": claims.herdr_pane_id,
                "herdr_workspace_id": claims.herdr_workspace_id,
                "card_id": claims.card_id,
                "run_id": claims.run_id,
                "last_tool": row.tool_name,
                "last_at": row.created_at,
            }));
        }
    }
    json!({"issue": issue, "panes": panes, "activity": rows})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(id: i64, issue: &str, pane: Option<&str>) -> board_core::protocol::Activity {
        board_core::protocol::Activity {
            id,
            space: Some("w1".into()),
            tool_name: "mcp__linear__save_issue".into(),
            issue: Some(issue.into()),
            claims: ActivityClaims {
                herdr_socket: Some("/tmp/h.sock".into()),
                herdr_pane_id: pane.map(Into::into),
                ..ActivityClaims::default()
            },
            created_at: format!("2026-09-30T00:00:0{id}Z"),
        }
    }

    #[test]
    fn panes_for_keeps_the_issue_rows_and_lists_each_pane_once_newest_first() {
        let rows = vec![
            activity(3, "ENG-1", Some("w1:p2")),
            activity(2, "ENG-2", Some("w1:p9")),
            activity(1, "ENG-1", Some("w1:p2")),
            activity(0, "ENG-1", None),
        ];
        let result = panes_for("ENG-1", rows);
        assert_eq!(result["activity"].as_array().unwrap().len(), 3);
        let panes = result["panes"].as_array().unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0]["herdr_pane_id"], "w1:p2");
        assert_eq!(panes[0]["last_at"], "2026-09-30T00:00:03Z");
    }

    #[test]
    fn an_empty_or_malformed_id_is_no_claim() {
        std::env::set_var("BOARD_MCP_TEST_EMPTY_ID", "");
        std::env::set_var("BOARD_MCP_TEST_BAD_ID", "x7");
        std::env::set_var("BOARD_MCP_TEST_ID", "7");
        assert_eq!(env_id("BOARD_MCP_TEST_EMPTY_ID"), None);
        assert_eq!(env_id("BOARD_MCP_TEST_BAD_ID"), None);
        assert_eq!(env_id("BOARD_MCP_TEST_ID"), Some(7));
    }
}
