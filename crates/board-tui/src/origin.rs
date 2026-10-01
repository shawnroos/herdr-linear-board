//! The invoking Herdr/plugin context, read once at the composition root.

/// Explicit context supplied by the composition root to the TUI.
///
/// Test and embedded drivers use [`Default::default`] so ambient Herdr/plugin
/// variables cannot affect their state. Only [`crate::Driver::new`] and
/// [`crate::run_with_board`] construct this from the process environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OriginContext {
    pub origin_socket: Option<String>,
    pub session: Option<String>,
    pub plugin_id: Option<String>,
    pub pane_id: Option<String>,
    /// `BOARD_WORK_PLUGIN_ROOT` as the board was started with it, sent with
    /// every `linear.snapshot` so the daemon need not be restarted to see it.
    pub plugin_root: Option<String>,
}

impl OriginContext {
    /// Read the invoking Herdr/plugin context at the production boundary.
    pub fn from_environment() -> OriginContext {
        let origin_socket = std::env::var("HERDR_SOCKET_PATH")
            .ok()
            .filter(|socket| !socket.is_empty());
        OriginContext {
            session: board_core::paths::session_name_from_socket(origin_socket.as_deref()),
            origin_socket,
            plugin_id: std::env::var("HERDR_PLUGIN_ID")
                .ok()
                .filter(|value| !value.is_empty()),
            pane_id: std::env::var("HERDR_PANE_ID")
                .ok()
                .filter(|value| !value.is_empty()),
            plugin_root: std::env::var("BOARD_WORK_PLUGIN_ROOT")
                .ok()
                .filter(|value| !value.is_empty()),
        }
    }
}

/// What an agent-opened board was asked to show: the daemon's
/// `BOARD_SHOW_SPACE`, `BOARD_SHOW_ISSUE` and `BOARD_SHOW_CARD`. Each value is
/// held to the shape `board.pane.open` checked before setting it, so a value
/// that fails is dropped rather than trusted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShowContext {
    pub space: Option<String>,
    pub issue: Option<String>,
    pub card: Option<i64>,
}

/// One place a board can land, in the order [`ShowContext::targets`] tries them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    Card(i64),
    Issue(String),
}

impl ShowContext {
    pub fn from_environment() -> ShowContext {
        let var = |name: &str| std::env::var(name).ok();
        ShowContext::parse(
            var("BOARD_SHOW_SPACE").as_deref(),
            var("BOARD_SHOW_ISSUE").as_deref(),
            var("BOARD_SHOW_CARD").as_deref(),
        )
    }

    pub fn parse(space: Option<&str>, issue: Option<&str>, card: Option<&str>) -> ShowContext {
        ShowContext {
            space: space.filter(|s| is_space_id(s)).map(str::to_string),
            issue: issue
                .filter(|i| board_core::db::is_issue_identifier(i))
                .map(str::to_string),
            card: card.and_then(|c| c.parse::<i64>().ok()).filter(|&c| c > 0),
        }
    }

    /// The space this board shows: a valid `BOARD_SHOW_SPACE`, else herdr's.
    pub fn workspace(&self, herdr_workspace_id: Option<&str>) -> Option<String> {
        self.space
            .as_deref()
            .or(herdr_workspace_id)
            .map(str::to_string)
    }

    /// A card before an issue.
    pub fn targets(&self) -> Vec<Landing> {
        self.card
            .map(Landing::Card)
            .into_iter()
            .chain(self.issue.clone().map(Landing::Issue))
            .collect()
    }
}

/// `board.pane.open`'s rule for a herdr workspace id (`checked_context` in
/// the daemon's `ops/panes.rs`); the two must agree.
fn is_space_id(space: &str) -> bool {
    (1..=64).contains(&space.len())
        && space
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

/// The agent a session pane is beside (KTD10), as the daemon passed it in
/// `BOARD_SESSION_*`. The pane's own `HERDR_PANE_ID` names the split, not the
/// agent, so none of this is read from herdr's variables except the space.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionIdentity {
    pub space: String,
    pub herdr_socket: Option<String>,
    pub herdr_pane_id: Option<String>,
    pub claude_session_id: Option<String>,
    pub cwd: Option<String>,
}

impl SessionIdentity {
    /// `None` without a space to read; the split shares its agent's space, so
    /// herdr's own workspace id stands in for a missing one.
    pub fn from_environment() -> Option<SessionIdentity> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let space = var("BOARD_SESSION_WORKSPACE").or_else(|| var("HERDR_WORKSPACE_ID"))?;
        Some(SessionIdentity {
            space,
            herdr_socket: var("BOARD_SESSION_SOCKET"),
            herdr_pane_id: var("BOARD_SESSION_PANE"),
            claude_session_id: var("BOARD_SESSION_CLAUDE"),
            cwd: var("BOARD_SESSION_CWD"),
        })
    }

    pub fn params(&self) -> board_core::protocol::LinearSessionGetParams {
        board_core::protocol::LinearSessionGetParams {
            space: self.space.clone(),
            herdr_socket: self.herdr_socket.clone(),
            herdr_pane_id: self.herdr_pane_id.clone(),
            claude_session_id: self.claude_session_id.clone(),
            cwd: self.cwd.clone(),
        }
    }
}
