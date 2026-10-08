//! Pane operations in the **caller's own** herdr session: `pane.set_title`,
//! `pane.focus`, the agent-opened board (`board.pane.open`/`board.pane.close`)
//! and `board.notify`, plus `caller.resolve`, the read-only search for the
//! pane a caller runs in across every herdr session.
//!
//! The daemon owns every Herdr interaction (`AGENTS.md`), so a client that
//! wants its own pane border relabelled asks for it here instead of shelling
//! out to `herdr pane rename` itself. The only current caller is the TUI
//! plugin pane keeping its border in sync with the board it shows.

use super::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use board_core::db::NewBoardPane;
use board_core::engine::{match_caller, parse_pane_filter, CallerPane};
use board_herdr::{NotificationSound, PaneRenameParams, PluginPaneOpenParams, PluginPanePlacement};

/// Rename `pane_id` in the session `origin_socket` belongs to.
///
/// Takes no [`Daemon`](crate::state::Daemon): this touches no board state, only
/// the caller's own Herdr session. That session is named by `origin_socket`
/// exactly as it is for `run.focus`, because only the caller knows which Herdr
/// it is running inside; the socket is canonicalized and then opened through
/// [`crate::herdr_conn`], so the pinned Herdr 0.9.0 / protocol 22 gate applies
/// before the rename reaches it.
///
/// **A failure here is a real error (code 4), not a silent success.** The
/// daemon must not answer `{renamed:true}` for a rename that did not happen:
/// this method is also readable from the CLI, the logs, and e2e, and those
/// callers need the diagnosis. Treating a cosmetic pane title as non-fatal is
/// the *caller's* policy — the TUI drops the result, exactly as it dropped the
/// subprocess exit status before — and it stays in that one layer.
///
/// Control and format characters are stripped here rather than trusted to the
/// TUI, because any socket client can call this. Brackets survive: the kanban
/// title is `Board [scope · FILTER]`.
pub(super) fn pane_set_title(p: PaneSetTitleParams) -> Result<Value> {
    if p.pane_id.trim().is_empty() {
        return Err(Error::BadRequest(
            "pane.set_title requires a non-empty pane_id".into(),
        ));
    }
    let socket = crate::herdr_conn::normalize_socket(Path::new(&p.origin_socket), "origin")?;
    let mut client = crate::herdr_conn::connect_checked(&socket)
        .map_err(|e| Error::HerdrUnavailable(format!("connecting to Herdr: {e}")))?;
    client
        .pane_rename(&PaneRenameParams {
            pane_id: p.pane_id.clone(),
            label: board_core::text::strip_control_and_format(&p.title),
        })
        .map_err(|e| Error::HerdrUnavailable(format!("pane.rename {}: {e}", p.pane_id)))?;
    Ok(json!(PaneSetTitleResult { renamed: true }))
}

/// Focus `pane_id` in the caller's own session. There is no run row to check
/// against, so `pane.get` on that socket is the whole membership test: a pane
/// the session does not list (another session's pane included) is `gone`, and
/// `pane.focus` is never sent for it.
pub(super) fn pane_focus(p: PaneFocusParams) -> Result<Value> {
    if p.pane_id.trim().is_empty() {
        return Err(Error::BadRequest(
            "pane.focus requires a non-empty pane_id".into(),
        ));
    }
    let socket = crate::herdr_conn::normalize_socket(Path::new(&p.origin_socket), "origin")?;
    let mut client = crate::herdr_conn::connect_checked(&socket)
        .map_err(|e| Error::HerdrUnavailable(format!("connecting to Herdr: {e}")))?;
    let live = client
        .pane_get(&p.pane_id)
        .map_err(|e| Error::HerdrUnavailable(format!("pane.get {}: {e}", p.pane_id)))?;
    if live.is_none() {
        return Ok(json!(PaneFocusResult {
            focused: false,
            gone: true,
        }));
    }
    client
        .pane_focus(&p.pane_id)
        .map_err(|e| Error::HerdrUnavailable(format!("pane.focus {}: {e}", p.pane_id)))?;
    Ok(json!(PaneFocusResult {
        focused: true,
        gone: false,
    }))
}

/// Find the herdr pane a caller runs in, read-only. A pane the caller names
/// resolves and is remembered for its Claude session; without one, the
/// remembered pane wins, and otherwise a folder lookup across every running
/// session only offers candidates.
pub(super) fn caller_resolve(d: &Arc<Daemon>, p: CallerResolveParams) -> Result<Value> {
    let claude_session = p
        .claude_session_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    if let Some(id) = claude_session {
        check_claude_session_id(id)?;
    }
    let pane = p.pane.as_deref().map(str::trim).filter(|v| !v.is_empty());

    if pane.is_none() {
        if let Some(id) = claude_session {
            let remembered = d
                .caller_locations
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(id)
                .cloned();
            if let Some(location) = remembered {
                return Ok(json!(CallerResolveResult::Resolved { location }));
            }
        }
    }

    let registry = d
        .session_registry
        .as_ref()
        .ok_or_else(|| Error::HerdrUnavailable("herdr not connected".into()))?;
    let entries = registry
        .list()
        .map_err(|e| Error::HerdrUnavailable(format!("listing herdr sessions: {e:#}")))?;
    let only_session = pane.and_then(|v| parse_pane_filter(v).session);
    let panes: Vec<CallerPane> = entries
        .iter()
        .filter(|e| e.running && !e.socket_path.is_empty())
        .filter(|e| only_session.as_ref().is_none_or(|s| *s == e.name))
        .flat_map(session_caller_panes)
        .collect();

    let result = match_caller(&panes, &p.cwd, pane);
    if let (Some(id), CallerResolveResult::Resolved { location }) = (claude_session, &result) {
        d.caller_locations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_string(), location.clone());
    }
    Ok(json!(result))
}

/// Every pane of one session, labelled with its workspace. A session that
/// cannot be reached or fails the gate is skipped: one dead session must not
/// hide the caller's pane in another.
fn session_caller_panes(entry: &crate::session::SessionEntry) -> Vec<CallerPane> {
    let listed = crate::herdr_conn::connect_checked(Path::new(&entry.socket_path))
        .and_then(|mut client| Ok((client.pane_list(None)?, client.workspace_list()?)));
    let (panes, workspaces) = match listed {
        Ok(listed) => listed,
        Err(error) => {
            tracing::warn!(
                session = %entry.name,
                socket = %entry.socket_path,
                %error,
                "caller.resolve skipped a herdr session"
            );
            return Vec::new();
        }
    };
    let labels: BTreeMap<String, String> = workspaces
        .into_iter()
        .filter(|w| !w.label.is_empty())
        .map(|w| (w.workspace_id, w.label))
        .collect();
    panes
        .into_iter()
        .map(|pane| CallerPane {
            session: entry.name.clone(),
            socket: entry.socket_path.clone(),
            workspace_label: labels.get(&pane.workspace_id).cloned(),
            workspace_id: pane.workspace_id,
            tab_id: pane.tab_id,
            pane_id: pane.pane_id,
            agent: pane.agent,
            cwd: pane.cwd,
            foreground_cwd: pane.foreground_cwd,
            title: pane.title,
        })
        .collect()
}

const BOARD_PLUGIN_ID: &str = "herdr-board";
const BOARD_ENTRYPOINT: &str = "board";
const SESSION_ENTRYPOINT: &str = "session";
const MAX_CWD: usize = 4096;
const MAX_CLAUDE_SESSION_ID: usize = 128;

// Held from the recorded-pane check through the record write, so two
// concurrent opens for one context cannot both open a pane.
static BOARD_PANES: Mutex<()> = Mutex::new(());

type ShowEnv = Vec<(&'static str, String)>;

/// Validate `context` and return its row key plus the `BOARD_SHOW_*` env it
/// contributes. The values reach another process's environment, so each field
/// is checked against its shape rather than cleaned.
fn checked_context(context: &BoardPaneContext) -> Result<(String, ShowEnv)> {
    let mut parts = Vec::new();
    let mut env = Vec::new();
    if let Some(space) = &context.space {
        if !board_core::db::is_space_id(space) {
            return Err(Error::BadRequest(format!(
                "board context space {space:?} is not a herdr workspace id"
            )));
        }
        parts.push(format!("space={space}"));
        env.push(("BOARD_SHOW_SPACE", space.clone()));
    }
    if let Some(issue) = &context.issue {
        if !board_core::db::is_issue_identifier(issue) {
            return Err(Error::BadRequest(format!(
                "board context issue {issue:?} is not a Linear issue key or UUID"
            )));
        }
        parts.push(format!("issue={issue}"));
        env.push(("BOARD_SHOW_ISSUE", issue.clone()));
    }
    if let Some(card) = context.card {
        if card <= 0 {
            return Err(Error::BadRequest(format!(
                "board context card {card} is not a card id"
            )));
        }
        parts.push(format!("card={card}"));
        env.push(("BOARD_SHOW_CARD", card.to_string()));
    }
    if parts.is_empty() {
        return Err(Error::BadRequest(
            "board context needs a space, an issue or a card".into(),
        ));
    }
    Ok((parts.join(";"), env))
}

/// A session pane shows its calling agent's own issue, so it takes no other
/// target, and one agent has one: the key is the agent's pane.
fn session_context(context: &BoardPaneContext, origin_pane: &str) -> Result<String> {
    if context.space.is_some() || context.issue.is_some() || context.card.is_some() {
        return Err(Error::BadRequest(
            "a session pane shows the calling agent's own issue; it takes no space, issue or card"
                .into(),
        ));
    }
    if origin_pane.trim().is_empty() {
        return Err(Error::BadRequest(
            "a session pane needs the agent's origin_pane".into(),
        ));
    }
    Ok(format!("session;pane={origin_pane}"))
}

fn context_key(context: &BoardPaneContext, origin_pane: Option<&str>) -> Result<(String, ShowEnv)> {
    if context.session {
        return Ok((
            session_context(context, origin_pane.unwrap_or_default())?,
            vec![],
        ));
    }
    checked_context(context)
}

/// The agent identity a session pane reads `linear.session.get` with. These
/// values reach another process's environment, so each is held to its shape.
fn session_identity(p: &BoardPaneOpenParams) -> Result<ShowEnv> {
    let mut env = vec![];
    if let Some(cwd) = &p.session_cwd {
        let ok =
            cwd.starts_with('/') && cwd.len() <= MAX_CWD && !cwd.chars().any(|c| c.is_control());
        if !ok {
            return Err(Error::BadRequest(format!(
                "session cwd {cwd:?} is not an absolute path"
            )));
        }
        env.push(("BOARD_SESSION_CWD", cwd.clone()));
    }
    if let Some(id) = &p.claude_session_id {
        check_claude_session_id(id)?;
        env.push(("BOARD_SESSION_CLAUDE", id.clone()));
    }
    Ok(env)
}

fn check_claude_session_id(id: &str) -> Result<()> {
    let ok = (1..=MAX_CLAUDE_SESSION_ID).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(Error::BadRequest(format!(
            "claude session id {id:?} is not a session id"
        )))
    }
}

fn origin_socket_key(origin_socket: &str) -> Result<(PathBuf, String)> {
    let socket = crate::herdr_conn::normalize_socket(Path::new(origin_socket), "origin")?;
    let key = socket.to_string_lossy().into_owned();
    Ok((socket, key))
}

fn connect_origin(socket: &Path) -> Result<board_herdr::HerdrClient> {
    crate::herdr_conn::connect_checked(socket)
        .map_err(|e| Error::HerdrUnavailable(format!("connecting to Herdr: {e}")))
}

/// Open a board beside the caller's own pane, or return the one already
/// recorded for this context once `pane.get` confirms it is still there.
pub(super) fn board_pane_open(d: &Arc<Daemon>, p: BoardPaneOpenParams) -> Result<Value> {
    let placement = match p.placement.as_str() {
        "tab" => BoardPanePlacement::Tab,
        "split" => BoardPanePlacement::Split,
        other => {
            return Err(Error::BadRequest(format!(
                "board.pane.open placement {other:?} is refused; a board opens in a tab or split"
            )))
        }
    };
    let session = p.context.session;
    let (key, mut show_env) = context_key(&p.context, Some(&p.origin_pane))?;
    if session {
        show_env.extend(session_identity(&p)?);
    }
    if p.origin_pane.trim().is_empty() {
        return Err(Error::BadRequest(
            "board.pane.open requires a non-empty origin_pane".into(),
        ));
    }
    let (socket_path, socket) = origin_socket_key(&p.origin_socket)?;
    let _serial = BOARD_PANES.lock().unwrap_or_else(PoisonError::into_inner);
    let mut client = connect_origin(&socket_path)?;
    let origin = client
        .pane_get(&p.origin_pane)
        .map_err(|e| Error::HerdrUnavailable(format!("pane.get {}: {e}", p.origin_pane)))?
        .ok_or_else(|| {
            Error::NotFound(format!(
                "origin pane {} is not in the caller's herdr session",
                p.origin_pane
            ))
        })?;

    let recorded = d.store.lock().board_pane_for_context(&socket, &key)?;
    if let Some(row) = recorded {
        let live = client
            .pane_get(&row.pane_id)
            .map_err(|e| Error::HerdrUnavailable(format!("pane.get {}: {e}", row.pane_id)))?;
        match live {
            Some(pane) => {
                return Ok(json!(BoardPaneOpenResult {
                    pane_id: pane.pane_id,
                    tab_id: pane.tab_id,
                    workspace_id: pane.workspace_id,
                    placement: row.placement,
                    reused: true,
                }))
            }
            None => {
                d.store.lock().remove_board_pane(&socket, &row.pane_id)?;
            }
        }
    }

    let (target_pane_id, workspace_id) = match placement {
        BoardPanePlacement::Split => (Some(origin.pane_id.clone()), None),
        BoardPanePlacement::Tab => {
            // An absent workspace would let Herdr open the tab in whichever
            // workspace the person has focused.
            if origin.workspace_id.is_empty() {
                return Err(Error::HerdrUnavailable(format!(
                    "pane.get {} named no workspace",
                    origin.pane_id
                )));
            }
            (None, Some(origin.workspace_id.clone()))
        }
    };
    let mut env = BTreeMap::new();
    env.insert(
        "BOARD_SOCKET".to_string(),
        d.socket_path.to_string_lossy().into_owned(),
    );
    env.insert(
        "BOARD_DB".to_string(),
        d.db_path.to_string_lossy().into_owned(),
    );
    env.extend(show_env.into_iter().map(|(k, v)| (k.to_string(), v)));
    if session {
        if origin.workspace_id.is_empty() {
            return Err(Error::HerdrUnavailable(format!(
                "pane.get {} named no workspace",
                origin.pane_id
            )));
        }
        // Raw, as the agent's herdr exported it: `linear.session.get` keys
        // the cached snapshot on this exact string.
        env.insert("BOARD_SESSION_SOCKET".into(), p.origin_socket.clone());
        env.insert("BOARD_SESSION_PANE".into(), origin.pane_id.clone());
        env.insert(
            "BOARD_SESSION_WORKSPACE".into(),
            origin.workspace_id.clone(),
        );
    }
    let opened = client
        .plugin_pane_open(&PluginPaneOpenParams {
            plugin_id: BOARD_PLUGIN_ID.into(),
            entrypoint: if session {
                SESSION_ENTRYPOINT
            } else {
                BOARD_ENTRYPOINT
            }
            .into(),
            placement: Some(match placement {
                BoardPanePlacement::Tab => PluginPanePlacement::Tab,
                BoardPanePlacement::Split => PluginPanePlacement::Split,
            }),
            focus: false,
            target_pane_id,
            workspace_id,
            env,
        })
        .map_err(|e| Error::HerdrUnavailable(format!("plugin.pane.open: {e}")))?;
    let pane = opened.pane;
    d.store.lock().record_board_pane(&NewBoardPane {
        herdr_socket: &socket,
        pane_id: &pane.pane_id,
        context_key: &key,
        placement,
        workspace_id: Some(pane.workspace_id.as_str()).filter(|w| !w.is_empty()),
        origin_pane_id: Some(&origin.pane_id),
    })?;
    Ok(json!(BoardPaneOpenResult {
        pane_id: pane.pane_id,
        tab_id: pane.tab_id,
        workspace_id: pane.workspace_id,
        placement,
        reused: false,
    }))
}

/// Close the board recorded for this context. Only a recorded pane is ever
/// closed; one that is already gone just loses its row.
pub(super) fn board_pane_close(d: &Arc<Daemon>, p: BoardPaneCloseParams) -> Result<Value> {
    let (context_key, _) = context_key(&p.context, p.origin_pane.as_deref())?;
    let (socket_path, socket) = origin_socket_key(&p.origin_socket)?;
    let _serial = BOARD_PANES.lock().unwrap_or_else(PoisonError::into_inner);
    let row = d
        .store
        .lock()
        .board_pane_for_context(&socket, &context_key)?
        .ok_or_else(|| {
            Error::NotFound(format!(
                "no board pane is recorded for context {context_key} in this herdr session"
            ))
        })?;
    let mut client = connect_origin(&socket_path)?;
    let live = client
        .pane_get(&row.pane_id)
        .map_err(|e| Error::HerdrUnavailable(format!("pane.get {}: {e}", row.pane_id)))?
        .is_some();
    if live {
        client.plugin_pane_close(&row.pane_id).map_err(|e| {
            Error::HerdrUnavailable(format!("plugin.pane.close {}: {e}", row.pane_id))
        })?;
    }
    d.store.lock().remove_board_pane(&socket, &row.pane_id)?;
    Ok(json!(BoardPaneCloseResult {
        pane_id: row.pane_id,
        closed: live,
        gone: !live,
    }))
}

/// Send one notification to the caller's own herdr session.
pub(super) fn board_notify(p: BoardNotifyParams) -> Result<Value> {
    let title = board_core::text::strip_control_and_format(&p.title);
    if title.trim().is_empty() {
        return Err(Error::BadRequest(
            "board.notify requires a non-empty title".into(),
        ));
    }
    let body = p
        .body
        .as_deref()
        .map(board_core::text::strip_control_and_format)
        .filter(|b| !b.trim().is_empty());
    let (socket_path, _) = origin_socket_key(&p.origin_socket)?;
    let mut client = connect_origin(&socket_path)?;
    let shown = client
        .notification_show(&title, body.as_deref(), NotificationSound::None)
        .map_err(|e| Error::HerdrUnavailable(format!("notification.show: {e}")))?;
    Ok(json!(BoardNotifyResult { shown: shown.shown }))
}
