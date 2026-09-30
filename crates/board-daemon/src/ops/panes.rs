//! Pane operations in the **caller's own** herdr session: `pane.set_title`,
//! `pane.focus`, the agent-opened board (`board.pane.open`/`board.pane.close`)
//! and `board.notify`.
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

const BOARD_PLUGIN_ID: &str = "herdr-board";
const BOARD_ENTRYPOINT: &str = "board";
const MAX_SPACE_ID: usize = 64;

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
        let ok = (1..=MAX_SPACE_ID).contains(&space.len())
            && space
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'));
        if !ok {
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
    let (context_key, show_env) = checked_context(&p.context)?;
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

    let recorded = d
        .store
        .lock()
        .board_pane_for_context(&socket, &context_key)?;
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
    let opened = client
        .plugin_pane_open(&PluginPaneOpenParams {
            plugin_id: BOARD_PLUGIN_ID.into(),
            entrypoint: BOARD_ENTRYPOINT.into(),
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
        context_key: &context_key,
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
    let (context_key, _) = checked_context(&p.context)?;
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
