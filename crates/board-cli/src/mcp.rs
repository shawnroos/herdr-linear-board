//! `board mcp`: a stdio MCP server that forwards each tool call to one boardd
//! method. Its only state is the caller's resolved herdr pane; it validates
//! nothing the daemon validates.
//! stdout carries only the MCP JSON-RPC stream, so nothing here may print
//! through `render.rs`; diagnostics go to stderr.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{anyhow, Context, Result};
use board_core::client::{BoardClient, RpcClientError, UnixClient};
use board_core::db::clean_owner;
use board_core::protocol::{
    ActivityClaims, BoardNotifyParams, BoardPaneCloseParams, BoardPaneContext, BoardPaneOpenParams,
    LinearActivityListParams, LinearBindParams, LinearMarkSetParams, LinearMarkUnmarkParams,
    LinearNoteSetParams, LinearOwner, LinearShowRequestParams, LinearShowWithdrawParams,
    LinearState, LinearStateGetParams, LinearUnbindParams, MarkKind,
};
use board_core::protocol::{CallerCandidate, CallerLocation, CallerResolveResult};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::caller::{
    env_id, env_text, resolve, CallerQuery, Source, HERDR_PANE_ID, HERDR_SOCKET_PATH,
    HERDR_WORKSPACE_ID,
};
use crate::commands::canonical_text;
use crate::daemon::connect_or_start;

/// Must not contain "linear": the work plugin's PostToolUse matcher is
/// `mcp__.*[Ll][Ii][Nn][Ee][Aa][Rr].*__.*`, and a match would report this
/// server's own tools back to the board as Linear writes.
const SERVER_NAME: &str = "board";
const ACTIVITY_LIMIT: usize = 500;
const CLAUDE_SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";
const NOT_FOUND: i32 = 2;

pub(crate) fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the MCP runtime")?;
    runtime.block_on(async {
        let service = BoardMcp::default()
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
    owner: LinearOwner,
    /// The location came from boardd's memory or this process's cache, not
    /// from env or a pane named in this call, so it may name a closed pane.
    remembered: bool,
    /// Folder candidates nobody has confirmed. Only a tool that needs the
    /// caller's pane or space, and was not given one, refuses on them.
    candidates: Option<Vec<CallerCandidate>>,
}

/// Why a call cannot learn its caller's pane. Rendered as its own tool error,
/// never as "board unavailable".
#[derive(Debug)]
enum CallerError {
    Unconfirmed(Vec<CallerCandidate>),
    UnknownPane(String),
}

impl std::fmt::Display for CallerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallerError::Unconfirmed(candidates) => {
                writeln!(
                    f,
                    "board could not confirm which herdr pane this session runs in. \
                     These Claude panes run in this session's folder:"
                )?;
                for candidate in candidates {
                    writeln!(
                        f,
                        "- pane {:?}: workspace {:?}, title {:?}",
                        candidate.pane,
                        candidate
                            .workspace_label
                            .as_deref()
                            .unwrap_or(&candidate.workspace_id),
                        candidate.title.as_deref().unwrap_or_default()
                    )?;
                }
                write!(
                    f,
                    "Ask the person whether this session runs in one of these panes, or in herdr \
                     at all. Only after they confirm one, call this tool again with `pane` set \
                     to that pane value."
                )
            }
            CallerError::UnknownPane(pane) => write!(
                f,
                "no running herdr session lists pane {pane:?}. Ask the person which pane this \
                 session runs in before calling again with `pane`."
            ),
        }
    }
}

impl std::error::Error for CallerError {}

impl Caller {
    /// Explicit pane, then env, then the location this process confirmed,
    /// then boardd. Only a resolved location is cached; an unconfirmed or
    /// missing one is asked again on the next call.
    fn locate(
        confirmed: &Mutex<Option<CallerLocation>>,
        pane: Option<String>,
        client: &mut UnixClient,
    ) -> Result<Caller> {
        let query = CallerQuery {
            cwd: canonical_text(PathBuf::from(cwd_or_current(None))),
            pane: pane
                .map(|pane| pane.trim().to_string())
                .filter(|pane| !pane.is_empty()),
            claude_session_id: env_text(CLAUDE_SESSION_ENV),
        };
        let cached = confirmed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let resolution = resolve(&query, env_text, |params| match (&params.pane, cached) {
            (None, Some(location)) => Ok(CallerResolveResult::Resolved { location }),
            _ => client.caller_resolve(params),
        })?;
        let named = query.pane.is_some();
        match resolution.result {
            CallerResolveResult::Resolved { location } => {
                if resolution.source == Source::Daemon {
                    *confirmed.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(location.clone());
                }
                let remembered = resolution.source == Source::Daemon && !named;
                Ok(Caller::at(Some(location), remembered))
            }
            CallerResolveResult::Unconfirmed { candidates } => Ok(Caller {
                candidates: Some(candidates),
                ..Caller::at(None, false)
            }),
            CallerResolveResult::NotInHerdr => match query.pane {
                Some(pane) => Err(CallerError::UnknownPane(pane).into()),
                None => Ok(Caller::at(None, false)),
            },
        }
    }

    fn at(location: Option<CallerLocation>, remembered: bool) -> Caller {
        let (herdr_socket, herdr_pane_id, herdr_workspace_id) = match location {
            Some(location) => (
                Some(location.socket),
                Some(location.pane_id),
                Some(location.workspace_id),
            ),
            None => (
                env_text(HERDR_SOCKET_PATH),
                env_text(HERDR_PANE_ID),
                env_text(HERDR_WORKSPACE_ID),
            ),
        };
        Caller {
            claims: ActivityClaims {
                herdr_socket: herdr_socket.clone(),
                herdr_pane_id: herdr_pane_id.clone(),
                herdr_workspace_id,
                card_id: env_id("BOARD_CARD_ID"),
                run_id: env_id("BOARD_RUN_ID"),
            },
            owner: LinearOwner {
                herdr_socket,
                herdr_pane_id,
                claude_session_id: env_text(CLAUDE_SESSION_ENV),
            },
            remembered,
            candidates: None,
        }
    }

    fn require_pane(&self) -> Result<()> {
        match &self.candidates {
            Some(candidates) => Err(CallerError::Unconfirmed(candidates.clone()).into()),
            None => Ok(()),
        }
    }

    fn space_or_refuse(&self, given: Option<String>) -> Result<String> {
        if given.is_none() {
            self.require_pane()?;
        }
        Ok(self.space(given))
    }

    /// A call that proceeds without a confirmed pane still tells the agent
    /// which panes it could confirm.
    fn with_note<T: Serialize>(&self, result: T) -> Result<Value> {
        let mut value = serde_json::to_value(result)?;
        if let (Some(candidates), Some(object)) = (&self.candidates, value.as_object_mut()) {
            let message = CallerError::Unconfirmed(candidates.clone()).to_string();
            object.insert(
                "caller".into(),
                json!({"state": "unconfirmed", "candidates": candidates, "message": message}),
            );
        }
        Ok(value)
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

/// Agents set needs_you, question and done; a suggestion is the board's own,
/// set only from a reported Linear write.
const AGENT_KINDS: [(&str, MarkKind); 3] = [
    ("needs_you", MarkKind::Attention),
    ("question", MarkKind::Question),
    ("done", MarkKind::Done),
];

fn agent_kind(given: &str) -> Result<MarkKind, CallToolResult> {
    AGENT_KINDS
        .iter()
        .find(|(name, _)| *name == given)
        .map(|(_, kind)| *kind)
        .ok_or_else(|| {
            refused(&format!(
                "mark kind {given:?} is not one an agent can set; use needs_you, question or done"
            ))
        })
}

fn kind_name(kind: MarkKind) -> &'static str {
    AGENT_KINDS
        .iter()
        .find(|(_, agent)| *agent == kind)
        .map_or(kind.as_str(), |(name, _)| name)
}

fn refused(message: &str) -> CallToolResult {
    let mut result = CallToolResult::structured_error(json!({"message": message}));
    result.content = vec![rmcp::model::ContentBlock::text(format!(
        "board refused: {message}"
    ))];
    result
}

fn rpc_code(error: &anyhow::Error) -> Option<i32> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RpcClientError>())
        .map(|rpc| rpc.code)
}

fn tool_error(error: &anyhow::Error) -> CallToolResult {
    if let Some(caller) = error.downcast_ref::<CallerError>() {
        let message = caller.to_string();
        let body = match caller {
            CallerError::Unconfirmed(candidates) => {
                json!({"state": "unconfirmed", "candidates": candidates, "message": message})
            }
            CallerError::UnknownPane(pane) => {
                json!({"state": "not_in_herdr", "pane": pane, "message": message})
            }
        };
        let mut result = CallToolResult::structured_error(body);
        result.content = vec![rmcp::model::ContentBlock::text(message)];
        return result;
    }
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
        Some(rpc) if error.chain().count() > 1 => {
            format!("board refused (code {}): {error:#}", rpc.code)
        }
        Some(rpc) => format!("board refused (code {}): {}", rpc.code, rpc.message),
        None => format!("board unavailable: {error:#}"),
    };
    result.content = vec![rmcp::model::ContentBlock::text(text)];
    result
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SpaceArgs {
    /// herdr workspace id; defaults to the workspace of the pane this session runs in.
    space: Option<String>,
    /// The `<session>/<pane id>` this session runs in, once the person confirmed it from an unconfirmed error's candidates. Overrides the environment.
    pane: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueArgs {
    /// Linear issue key, for example ENG-123.
    issue: String,
    /// herdr workspace id; defaults to the workspace of the pane this session runs in.
    space: Option<String>,
    /// The `<session>/<pane id>` this session runs in, once the person confirmed it from an unconfirmed error's candidates. Overrides the environment.
    pane: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BindArgs {
    /// Linear issue key to bind this worktree to.
    issue: String,
    /// A path inside the git worktree to bind; defaults to this session's working directory.
    cwd: Option<String>,
    /// herdr workspace id; defaults to the workspace of the pane this session runs in.
    space: Option<String>,
    /// The `<session>/<pane id>` this session runs in, once the person confirmed it from an unconfirmed error's candidates. Overrides the environment.
    pane: Option<String>,
    branch: Option<String>,
    tab: Option<String>,
    display_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UnbindArgs {
    /// A path inside the bound git worktree; defaults to this session's working directory.
    cwd: Option<String>,
    /// herdr workspace id; defaults to the workspace of the pane this session runs in.
    space: Option<String>,
    /// The `<session>/<pane id>` this session runs in, once the person confirmed it from an unconfirmed error's candidates. Overrides the environment.
    pane: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MarkArgs {
    /// Linear issue key of the card to flag.
    issue: String,
    /// One of needs_you, question or done.
    kind: String,
    /// Optional detail shown with the mark, for example the question itself.
    text: Option<String>,
    /// herdr workspace id; defaults to the workspace of the pane this session runs in.
    space: Option<String>,
    /// The `<session>/<pane id>` this session runs in, once the person confirmed it from an unconfirmed error's candidates. Overrides the environment.
    pane: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UnmarkArgs {
    /// Linear issue key of the card.
    issue: String,
    /// needs_you, question or done; without it every mark this caller set on the card clears.
    kind: Option<String>,
    /// herdr workspace id; defaults to the workspace of the pane this session runs in.
    space: Option<String>,
    /// The `<session>/<pane id>` this session runs in, once the person confirmed it from an unconfirmed error's candidates. Overrides the environment.
    pane: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NoteArgs {
    /// Linear issue key of the card.
    issue: String,
    /// The note; it replaces this caller's earlier note on the card.
    body: String,
    /// herdr workspace id; defaults to the workspace of the pane this session runs in.
    space: Option<String>,
    /// The `<session>/<pane id>` this session runs in, once the person confirmed it from an unconfirmed error's candidates. Overrides the environment.
    pane: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NotifyArgs {
    title: String,
    body: Option<String>,
    /// The `<session>/<pane id>` this session runs in, once the person confirmed it from an unconfirmed error's candidates. Overrides the environment.
    pane: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AskToShowArgs {
    /// Linear issue key to ask the person to look at.
    issue: String,
    /// Why, in a few words.
    reason: Option<String>,
    /// herdr workspace id; defaults to the workspace of the pane this session runs in.
    space: Option<String>,
    /// The `<session>/<pane id>` this session runs in, once the person confirmed it from an unconfirmed error's candidates. Overrides the environment.
    pane: Option<String>,
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
    /// true opens this session's side pane instead: your bound issue, your lane, or the bind hint. Takes no space, issue or card.
    session: Option<bool>,
    /// The `<session>/<pane id>` this session runs in, once the person confirmed it from an unconfirmed error's candidates. Overrides the environment.
    pane: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CloseBoardArgs {
    /// The same space, issue and card the board was opened with.
    space: Option<String>,
    issue: Option<String>,
    card: Option<i64>,
    /// true closes this session's side pane.
    session: Option<bool>,
    /// The `<session>/<pane id>` this session runs in, once the person confirmed it from an unconfirmed error's candidates. Overrides the environment.
    pane: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct BoardMcp {
    confirmed: Arc<Mutex<Option<CallerLocation>>>,
}

impl BoardMcp {
    async fn forward_as<T, F>(&self, pane: Option<String>, call: F) -> CallToolResult
    where
        T: Serialize + Send + 'static,
        F: FnOnce(&mut UnixClient, Caller) -> Result<T> + Send + 'static,
    {
        let confirmed = Arc::clone(&self.confirmed);
        forward(move |client| {
            let caller = Caller::locate(&confirmed, pane, client)?;
            call(client, caller)
        })
        .await
    }
}

#[tool_router]
impl BoardMcp {
    #[tool(
        description = "Show the board's local state for a space: bindings, marks with their kind, notes, and pending show-requests with their expiry. `yours` flags the marks and requests this caller set; `your_resolved_requests` lists this caller's recently closed requests with their outcome (accepted, rejected, withdrawn or expired).",
        annotations(read_only_hint = true)
    )]
    async fn state(&self, Parameters(args): Parameters<SpaceArgs>) -> CallToolResult {
        self.forward_as(args.pane, move |c, caller| {
            let params = LinearStateGetParams {
                space: caller.space_or_refuse(args.space)?,
                herdr_socket: caller.owner.herdr_socket.clone(),
            };
            state_for(&caller.owner, c.linear_state_get(&params)?)
        })
        .await
    }

    #[tool(
        description = "List the herdr panes that recorded Linear activity on an issue, with their session, workspace, card and run.",
        annotations(read_only_hint = true)
    )]
    async fn panes_for_issue(&self, Parameters(args): Parameters<IssueArgs>) -> CallToolResult {
        self.forward_as(args.pane, move |c, caller| {
            let params = LinearActivityListParams {
                space: Some(caller.space_or_refuse(args.space)?),
                limit: Some(ACTIVITY_LIMIT),
                herdr_socket: caller.owner.herdr_socket,
            };
            let listed = c.linear_activity_list(&params)?;
            Ok(panes_for(&args.issue, listed.activity))
        })
        .await
    }

    #[tool(
        description = "Bind the git worktree this session works in to a Linear issue. Returns the binding before and after; unbind undoes it.",
        annotations(read_only_hint = false)
    )]
    async fn bind(&self, Parameters(args): Parameters<BindArgs>) -> CallToolResult {
        self.forward_as(args.pane, move |c, caller| {
            let change = c.linear_bind(&LinearBindParams {
                cwd: cwd_or_current(args.cwd),
                issue: args.issue,
                space: args.space,
                branch: args.branch,
                tab: args.tab,
                display_name: args.display_name,
                claims: caller.claims.clone(),
            })?;
            caller.with_note(change)
        })
        .await
    }

    #[tool(
        description = "Remove this worktree's issue binding. Returns the binding it removed.",
        annotations(read_only_hint = false)
    )]
    async fn unbind(&self, Parameters(args): Parameters<UnbindArgs>) -> CallToolResult {
        self.forward_as(args.pane, move |c, caller| {
            let change = c.linear_unbind(&LinearUnbindParams {
                cwd: cwd_or_current(args.cwd),
                space: args.space,
                claims: caller.claims.clone(),
            })?;
            caller.with_note(change)
        })
        .await
    }

    #[tool(
        description = "Mark a card for the person: kind needs_you, question or done. Replaces this caller's earlier mark of the same kind on the card and returns both; other agents' marks stay.",
        annotations(read_only_hint = false)
    )]
    async fn mark(&self, Parameters(args): Parameters<MarkArgs>) -> CallToolResult {
        let kind = match agent_kind(&args.kind) {
            Ok(kind) => kind,
            Err(refusal) => return refusal,
        };
        self.forward_as(args.pane, move |c, caller| {
            c.linear_mark_set(&LinearMarkSetParams {
                space: caller.space_or_refuse(args.space)?,
                issue: args.issue,
                kind,
                text: args.text,
                created_by: Some(caller.author()),
                owner: caller.owner,
            })
        })
        .await
    }

    #[tool(
        description = "Clear this caller's own marks on a card, of one kind or of every kind. Other agents' marks stay. Returns the marks it removed.",
        annotations(read_only_hint = false)
    )]
    async fn unmark(&self, Parameters(args): Parameters<UnmarkArgs>) -> CallToolResult {
        let kind = match args.kind.as_deref().map(agent_kind).transpose() {
            Ok(kind) => kind,
            Err(refusal) => return refusal,
        };
        self.forward_as(args.pane, move |c, caller| {
            c.linear_mark_unmark(&LinearMarkUnmarkParams {
                space: caller.space_or_refuse(args.space)?,
                issue: args.issue,
                kind,
                owner: caller.owner,
            })
        })
        .await
    }

    #[tool(
        description = "Attach a short note to a card. Replaces this caller's earlier note and returns both.",
        annotations(read_only_hint = false)
    )]
    async fn note(&self, Parameters(args): Parameters<NoteArgs>) -> CallToolResult {
        self.forward_as(args.pane, move |c, caller| {
            c.linear_note_set(&LinearNoteSetParams {
                space: caller.space_or_refuse(args.space)?,
                issue: args.issue,
                body: args.body,
                author: caller.author(),
                owner: caller.owner,
            })
        })
        .await
    }

    #[tool(
        description = "Send a herdr notification in this session.",
        annotations(read_only_hint = false)
    )]
    async fn notify(&self, Parameters(args): Parameters<NotifyArgs>) -> CallToolResult {
        self.forward_as(args.pane, move |c, caller| {
            c.board_notify(&BoardNotifyParams {
                origin_socket: caller.origin_socket(),
                title: args.title,
                body: args.body,
            })
        })
        .await
    }

    #[tool(
        description = "Ask the person to look at an issue. The board shows the request until the person accepts or rejects it, or it expires; the view moves only when the person accepts. Asking again for the same issue refreshes this caller's request.",
        annotations(read_only_hint = false)
    )]
    async fn ask_to_show(&self, Parameters(args): Parameters<AskToShowArgs>) -> CallToolResult {
        self.forward_as(args.pane, move |c, caller| {
            c.linear_show_request(&LinearShowRequestParams {
                space: caller.space_or_refuse(args.space)?,
                issue: args.issue,
                reason: args.reason,
                requested_by: Some(caller.author()),
                owner: caller.owner,
            })
        })
        .await
    }

    #[tool(
        description = "Withdraw this caller's own pending request to show an issue. The view does not move. Returns the request before and after, with outcome withdrawn.",
        annotations(read_only_hint = false)
    )]
    async fn withdraw_show(&self, Parameters(args): Parameters<IssueArgs>) -> CallToolResult {
        self.forward_as(args.pane, move |c, caller| {
            c.linear_show_withdraw(&LinearShowWithdrawParams {
                space: caller.space_or_refuse(args.space)?,
                issue: args.issue,
                owner: caller.owner,
            })
        })
        .await
    }

    #[tool(
        description = "Open a board beside this pane (split) or in a new tab, showing a space, issue or card, without taking focus. With session: true it opens this session's side pane instead: your bound issue, your lane list, or the bind hint. Reuses a board already open for the same context.",
        annotations(read_only_hint = false)
    )]
    async fn open_board(&self, Parameters(args): Parameters<OpenBoardArgs>) -> CallToolResult {
        let session = args.session.unwrap_or(false);
        // The session pane runs in the plugin root, so it reads the agent's
        // worktree from here; canonical, as the status line sends it.
        let session_cwd = session.then(|| canonical_text(PathBuf::from(cwd_or_current(None))));
        let confirmed = Arc::clone(&self.confirmed);
        self.forward_as(args.pane, move |c, caller| {
            caller.require_pane()?;
            let params = BoardPaneOpenParams {
                context: BoardPaneContext {
                    space: args.space,
                    issue: args.issue,
                    card: args.card,
                    session,
                },
                placement: args.placement.unwrap_or_else(|| "split".to_string()),
                origin_socket: caller.origin_socket(),
                origin_pane: caller.origin_pane(),
                session_cwd,
                claude_session_id: session
                    .then(|| caller.owner.claude_session_id.clone())
                    .flatten(),
            };
            c.board_pane_open(&params).map_err(|error| {
                if caller.remembered && rpc_code(&error) == Some(NOT_FOUND) {
                    *confirmed.lock().unwrap_or_else(PoisonError::into_inner) = None;
                    error.context(
                        "the pane this session confirmed earlier may have closed; ask the \
                         person which pane this session runs in, then call again with `pane`",
                    )
                } else {
                    error
                }
            })
        })
        .await
    }

    #[tool(
        description = "Close a board this server opened, named by the context it was opened with.",
        annotations(read_only_hint = false)
    )]
    async fn close_board(&self, Parameters(args): Parameters<CloseBoardArgs>) -> CallToolResult {
        let session = args.session.unwrap_or(false);
        self.forward_as(args.pane, move |c, caller| {
            caller.require_pane()?;
            c.board_pane_close(&BoardPaneCloseParams {
                context: BoardPaneContext {
                    space: args.space,
                    issue: args.issue,
                    card: args.card,
                    session,
                },
                origin_socket: caller.origin_socket(),
                origin_pane: session.then(|| caller.origin_pane()),
            })
        })
        .await
    }
}

#[tool_handler]
impl ServerHandler for BoardMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
    }
}

/// Ownership compares all three owner parts; a caller with no claims
/// owns nothing, matching the daemon's refusal of an anonymous unmark.
fn state_for(caller: &LinearOwner, state: LinearState) -> Result<Value> {
    let caller = clean_owner(caller);
    let yours = |owner: LinearOwner| !caller.is_anonymous() && owner == caller;
    let marks = state
        .marks
        .iter()
        .map(|mark| {
            let mut row = serde_json::to_value(mark)?;
            row["kind"] = json!(kind_name(mark.kind));
            row["yours"] = json!(yours(mark.owner()));
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    let pending = state
        .show_requests
        .iter()
        .map(|request| {
            let mut row = serde_json::to_value(request)?;
            row["yours"] = json!(yours(request.owner()));
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    let resolved: Vec<_> = state
        .resolved_show_requests
        .iter()
        .filter(|request| yours(request.owner()))
        .collect();
    let mut value = serde_json::to_value(&state)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("resolved_show_requests");
        object.insert("marks".into(), json!(marks));
        object.insert("show_requests".into(), json!(pending));
        object.insert("your_resolved_requests".into(), json!(resolved));
    }
    Ok(value)
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
}
