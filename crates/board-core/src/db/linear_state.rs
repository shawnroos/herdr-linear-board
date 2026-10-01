//! Linear-mode local state: bindings, session scopes, scope repositories,
//! grouping config, marks, notes, show-requests, activity and the board panes
//! boardd opened. Grouping validation mirrors the work plugin's `board.json`
//! loader (`lib/board-config.sh`) rule for rule, default-deny.

use std::collections::{BTreeMap, HashSet};

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::Db;
use crate::{Error, Result};

/// Chosen, not measured: activity is a recent-history view, not an audit log.
pub const LINEAR_ACTIVITY_KEEP_PER_SPACE: usize = 500;
/// A count, not a time window: `acknowledged_at` comes from SQLite's own
/// clock, not the injected `now`, so a window would mix the two clocks.
pub const RECENT_RESOLVED_SHOW_REQUESTS: usize = 20;

pub const GROUPING_LEVELS: [&str; 4] = ["space", "tab", "column", "row"];
pub const GROUPING_FIELD_KINDS: [&str; 8] = [
    "team",
    "project",
    "milestone",
    "cycle",
    "assignee",
    "state",
    "priority",
    "parent",
];
pub const GROUPING_FILTER_KEYS: [&str; 10] = [
    "team",
    "project",
    "milestone",
    "cycle",
    "assignee",
    "state",
    "parent",
    "label",
    "state-type",
    "priority",
];
pub const LINEAR_STATE_TYPES: [&str; 6] = [
    "triage",
    "backlog",
    "unstarted",
    "started",
    "completed",
    "canceled",
];

const MAX_TOOL_NAME: usize = 128;

fn is_control(c: char) -> bool {
    matches!(c as u32, 0x00..=0x1f | 0x7f..=0x9f)
}

fn text_ok(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(is_control)
}

fn shown(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() <= 80 {
        text
    } else {
        format!("{}...", text.chars().take(77).collect::<String>())
    }
}

fn shown_str(value: &str) -> String {
    shown(&Value::String(value.to_owned()))
}

fn refuse(message: String) -> Error {
    Error::BadRequest(message)
}

fn require_text(value: &str, what: &str) -> Result<()> {
    if text_ok(value) {
        Ok(())
    } else {
        Err(refuse(format!(
            "{what} is {}; it must be non-empty text without control characters",
            shown_str(value)
        )))
    }
}

fn require_optional_text(value: Option<&str>, what: &str) -> Result<()> {
    value.map_or(Ok(()), |v| require_text(v, what))
}

fn require_absolute_path(value: &str, what: &str) -> Result<()> {
    require_text(value, what)?;
    if value.starts_with('/') {
        Ok(())
    } else {
        Err(refuse(format!(
            "{what} is {}; it must be an absolute path",
            shown_str(value)
        )))
    }
}

pub fn is_space_id(space: &str) -> bool {
    (1..=64).contains(&space.len())
        && space
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

/// A Linear issue key (`WEB-123`) or an issue UUID in its hyphenated form.
pub fn is_issue_identifier(value: &str) -> bool {
    if value.len() == 36 {
        return uuid::Uuid::try_parse(value).is_ok();
    }
    let Some((team, number)) = value.split_once('-') else {
        return false;
    };
    let team = team.as_bytes();
    let number = number.as_bytes();
    (1..=10).contains(&team.len())
        && team[0].is_ascii_uppercase()
        && team
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && (1..=9).contains(&number.len())
        && number[0] != b'0'
        && number.iter().all(u8::is_ascii_digit)
}

fn require_issue(value: &str) -> Result<()> {
    if is_issue_identifier(value) {
        Ok(())
    } else {
        Err(refuse(format!(
            "issue {} is neither a Linear issue key (like WEB-123) nor an issue UUID",
            shown_str(value)
        )))
    }
}

/// `project-<id>.team-<id>`, `team-<id>` or `project-<id>`.
pub fn is_scope_key(value: &str) -> bool {
    let id_ok = |id: &str| {
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if let Some(rest) = value.strip_prefix("project-") {
        return match rest.split_once(".team-") {
            Some((project, team)) => id_ok(project) && id_ok(team),
            None => id_ok(rest) && !rest.contains('.'),
        };
    }
    value.strip_prefix("team-").is_some_and(id_ok)
}

fn to_json_text(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn from_json_text<T: serde::de::DeserializeOwned>(text: &str) -> rusqlite::Result<T> {
    serde_json::from_str(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn optional_json(text: Option<String>) -> rusqlite::Result<Option<Value>> {
    text.as_deref().map(from_json_text).transpose()
}

// -- grouping -----------------------------------------------------------------

/// One mapping of levels to kinds plus the filter that selects tickets. Level
/// kinds and filter values stay as the plugin's JSON spells them; `validate`
/// is the only gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupingMapping {
    pub levels: BTreeMap<String, String>,
    pub filter: Map<String, Value>,
}

impl GroupingMapping {
    pub fn validate(&self) -> Result<()> {
        self.validate_as("the mapping")
    }

    fn validate_as(&self, whose: &str) -> Result<()> {
        if self.levels.is_empty() {
            return Err(refuse(format!(
                "{whose} levels names no level; a mapping needs at least one level"
            )));
        }
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for (level, kind) in &self.levels {
            if !GROUPING_LEVELS.contains(&level.as_str()) {
                return Err(refuse(format!(
                    "{whose} has unknown level {}; the levels are {}",
                    shown_str(level),
                    GROUPING_LEVELS.join(", ")
                )));
            }
            check_kind(kind, &format!("{whose} level {level}"))?;
            if let Some(first) = seen.insert(kind, level) {
                return Err(refuse(format!(
                    "{whose} sets levels {first} and {level} to the same kind {}; levels must be distinct",
                    shown_str(kind)
                )));
            }
        }
        check_filter(&self.filter, &format!("{whose} filter"))
    }
}

fn allowed_kinds() -> String {
    format!(
        "a single-valued field ({}), a label group (label-group:<name>), ticket, or sub-ticket",
        GROUPING_FIELD_KINDS.join(", ")
    )
}

fn check_kind(kind: &str, at: &str) -> Result<()> {
    if GROUPING_FIELD_KINDS.contains(&kind) || kind == "ticket" || kind == "sub-ticket" {
        return Ok(());
    }
    if let Some(name) = kind.strip_prefix("label-group:") {
        if text_ok(name) {
            return Ok(());
        }
        return Err(refuse(format!(
            "{at} is {}, a label-group with no usable name; a level is {}",
            shown_str(kind),
            allowed_kinds()
        )));
    }
    Err(refuse(format!(
        "{at} is {}, which is not a level kind; a level is {}",
        shown_str(kind),
        allowed_kinds()
    )))
}

fn check_filter(filter: &Map<String, Value>, whose: &str) -> Result<()> {
    if filter.is_empty() {
        return Err(refuse(format!(
            "{whose} names no keys; a board filter must select something"
        )));
    }
    for (key, value) in filter {
        if !GROUPING_FILTER_KEYS.contains(&key.as_str()) {
            return Err(refuse(format!(
                "{whose} has unknown filter key {}; allowed: {}",
                shown_str(key),
                GROUPING_FILTER_KEYS.join(", ")
            )));
        }
        let at = format!("{whose} key {}", shown_str(key));
        let items: &[Value] = match value {
            Value::Array(items) if items.is_empty() => {
                return Err(refuse(format!(
                    "{at} is an empty list; leave the key out instead"
                )));
            }
            Value::Array(items) => items,
            single => std::slice::from_ref(single),
        };
        for item in items {
            check_filter_value(key, item, &at)?;
        }
    }
    Ok(())
}

fn check_filter_value(key: &str, item: &Value, at: &str) -> Result<()> {
    if key == "priority" {
        return match item.as_u64() {
            Some(0..=4) => Ok(()),
            _ => Err(refuse(format!(
                "{at} holds {}; a priority is an integer from 0 to 4",
                shown(item)
            ))),
        };
    }
    match item.as_str() {
        Some("") => Err(refuse(format!(
            "{at} holds an empty string, which is refused rather than read as absent"
        ))),
        Some(text) if text_ok(text) => {
            if key == "state-type" && !LINEAR_STATE_TYPES.contains(&text) {
                Err(refuse(format!(
                    "{at} holds {}; a state type is one of {}",
                    shown(item),
                    LINEAR_STATE_TYPES.join(", ")
                )))
            } else {
                Ok(())
            }
        }
        _ => Err(refuse(format!(
            "{at} holds {}; a value is a non-empty string without control characters",
            shown(item)
        ))),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceGrouping {
    pub space: String,
    pub mapping: GroupingMapping,
}

/// The whole grouping config: `spaces` in configuration order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupingConfig {
    pub global: GroupingMapping,
    #[serde(default)]
    pub spaces: Vec<SpaceGrouping>,
}

impl GroupingConfig {
    pub fn validate(&self) -> Result<()> {
        self.global.validate_as("the global mapping")?;
        let mut names = HashSet::new();
        for entry in &self.spaces {
            validate_space_name(&entry.space)?;
            if !names.insert(entry.space.as_str()) {
                return Err(refuse(format!(
                    "space {} is mapped more than once",
                    shown_str(&entry.space)
                )));
            }
            entry.mapping.validate_as(&format!(
                "the mapping for space {}",
                shown_str(&entry.space)
            ))?;
        }
        Ok(())
    }
}

fn validate_space_name(space: &str) -> Result<()> {
    if text_ok(space) {
        Ok(())
    } else {
        Err(refuse(format!(
            "space name {} is refused; a space name is non-empty text without control characters",
            shown_str(space)
        )))
    }
}

/// The mapping in force for one space: `space` is `None` when it is the
/// global mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedGrouping {
    pub space: Option<String>,
    pub mapping: GroupingMapping,
}

// -- bindings, scopes, repositories -------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceBinding {
    pub herdr_session: String,
    pub space: String,
    pub project_id: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub team_ids: Vec<String>,
    pub view: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeBindingState {
    Bound,
    Misplaced,
    Stale,
}

impl WorktreeBindingState {
    pub fn as_str(self) -> &'static str {
        match self {
            WorktreeBindingState::Bound => "bound",
            WorktreeBindingState::Misplaced => "misplaced",
            WorktreeBindingState::Stale => "stale",
        }
    }

    fn parse(text: &str) -> rusqlite::Result<Self> {
        match text {
            "bound" => Ok(WorktreeBindingState::Bound),
            "misplaced" => Ok(WorktreeBindingState::Misplaced),
            "stale" => Ok(WorktreeBindingState::Stale),
            _ => Err(super::conv_err("linear_worktree_bindings.state")),
        }
    }
}

/// `carried` keeps plugin fields the board does not interpret (created
/// children and documents, prior bindings, description head).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeBinding {
    pub worktree_path: String,
    pub issue: String,
    pub state: WorktreeBindingState,
    pub branch: Option<String>,
    pub tab: Option<String>,
    pub display_name: Option<String>,
    #[serde(default)]
    pub team_ids: Vec<String>,
    pub view: Option<Value>,
    #[serde(default)]
    pub carried: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionScope {
    pub session_id: String,
    pub team_id: String,
    pub team_key: Option<String>,
}

// -- marks, notes, show-requests ----------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkKind {
    Attention,
    Question,
    Done,
    Suggestion,
}

impl MarkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MarkKind::Attention => "attention",
            MarkKind::Question => "question",
            MarkKind::Done => "done",
            MarkKind::Suggestion => "suggestion",
        }
    }

    fn parse(text: &str) -> rusqlite::Result<Self> {
        match text {
            "attention" => Ok(MarkKind::Attention),
            "question" => Ok(MarkKind::Question),
            "done" => Ok(MarkKind::Done),
            "suggestion" => Ok(MarkKind::Suggestion),
            _ => Err(super::conv_err("linear_marks.kind")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShowOutcome {
    Accepted,
    Rejected,
    Withdrawn,
    Expired,
}

impl ShowOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            ShowOutcome::Accepted => "accepted",
            ShowOutcome::Rejected => "rejected",
            ShowOutcome::Withdrawn => "withdrawn",
            ShowOutcome::Expired => "expired",
        }
    }

    fn parse(text: &str) -> rusqlite::Result<Self> {
        match text {
            "accepted" => Ok(ShowOutcome::Accepted),
            "rejected" => Ok(ShowOutcome::Rejected),
            "withdrawn" => Ok(ShowOutcome::Withdrawn),
            "expired" => Ok(ShowOutcome::Expired),
            _ => Err(super::conv_err("linear_show_requests.outcome")),
        }
    }
}

/// Who wrote a mark, note or request. Attribution, not access
/// control: the fields are the caller's own claims. All three `None` is no
/// owner, which only the person can clear.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearOwner {
    #[serde(default)]
    pub herdr_socket: Option<String>,
    #[serde(default)]
    pub herdr_pane_id: Option<String>,
    #[serde(default)]
    pub claude_session_id: Option<String>,
}

impl LinearOwner {
    pub fn is_anonymous(&self) -> bool {
        self.herdr_socket.is_none()
            && self.herdr_pane_id.is_none()
            && self.claude_session_id.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct NewMark<'a> {
    pub session: &'a str,
    pub space: &'a str,
    pub issue: &'a str,
    pub kind: MarkKind,
    pub text: Option<&'a str>,
    pub detail: Option<Value>,
    pub created_by: Option<&'a str>,
    pub owner: &'a LinearOwner,
}

#[derive(Debug, Clone)]
pub struct NewShowRequest<'a> {
    pub session: &'a str,
    pub space: &'a str,
    pub issue: &'a str,
    pub reason: Option<&'a str>,
    pub requested_by: Option<&'a str>,
    pub owner: &'a LinearOwner,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub id: i64,
    pub space: String,
    pub issue: String,
    pub kind: MarkKind,
    pub text: Option<String>,
    pub detail: Option<Value>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub owner_herdr_socket: Option<String>,
    pub owner_herdr_pane_id: Option<String>,
    pub owner_claude_session_id: Option<String>,
}

impl Mark {
    pub fn owner(&self) -> LinearOwner {
        LinearOwner {
            herdr_socket: self.owner_herdr_socket.clone(),
            herdr_pane_id: self.owner_herdr_pane_id.clone(),
            claude_session_id: self.owner_claude_session_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: i64,
    pub space: String,
    pub issue: String,
    pub body: String,
    pub author: String,
    pub created_at: String,
    pub owner_herdr_socket: Option<String>,
    pub owner_herdr_pane_id: Option<String>,
    pub owner_claude_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowRequest {
    pub id: i64,
    pub space: String,
    pub issue: String,
    pub reason: Option<String>,
    pub requested_by: Option<String>,
    pub created_at: String,
    pub acknowledged_at: Option<String>,
    pub owner_herdr_socket: Option<String>,
    pub owner_herdr_pane_id: Option<String>,
    pub owner_claude_session_id: Option<String>,
    pub expires_at: Option<String>,
    pub outcome: Option<ShowOutcome>,
}

impl ShowRequest {
    pub fn owner(&self) -> LinearOwner {
        LinearOwner {
            herdr_socket: self.owner_herdr_socket.clone(),
            herdr_pane_id: self.owner_herdr_pane_id.clone(),
            claude_session_id: self.owner_claude_session_id.clone(),
        }
    }

    /// A request with no readable `expires_at` never expires.
    pub fn is_overdue(&self, now: i64) -> bool {
        self.expires_at
            .as_deref()
            .and_then(crate::protocol::parse_timestamp)
            .is_some_and(|expires| now >= expires)
    }

    pub fn is_pending(&self, now: i64) -> bool {
        self.outcome.is_none() && self.acknowledged_at.is_none() && !self.is_overdue(now)
    }
}

// -- activity and board panes -------------------------------------------------

/// Who a caller says it is. Claims, not proof: they come from the caller's
/// own environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityClaims {
    pub herdr_socket: Option<String>,
    pub herdr_pane_id: Option<String>,
    pub herdr_workspace_id: Option<String>,
    pub card_id: Option<i64>,
    pub run_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewActivity<'a> {
    pub session: &'a str,
    pub space: Option<&'a str>,
    pub tool_name: &'a str,
    pub issue: Option<&'a str>,
    pub claims: &'a ActivityClaims,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub id: i64,
    pub space: Option<String>,
    pub tool_name: String,
    pub issue: Option<String>,
    pub claims: ActivityClaims,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardPanePlacement {
    Tab,
    Split,
}

impl BoardPanePlacement {
    pub fn as_str(self) -> &'static str {
        match self {
            BoardPanePlacement::Tab => "tab",
            BoardPanePlacement::Split => "split",
        }
    }

    fn parse(text: &str) -> rusqlite::Result<Self> {
        match text {
            "tab" => Ok(BoardPanePlacement::Tab),
            "split" => Ok(BoardPanePlacement::Split),
            _ => Err(super::conv_err("linear_board_panes.placement")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewBoardPane<'a> {
    pub herdr_socket: &'a str,
    pub pane_id: &'a str,
    pub context_key: &'a str,
    pub placement: BoardPanePlacement,
    pub workspace_id: Option<&'a str>,
    pub origin_pane_id: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardPane {
    pub herdr_socket: String,
    pub pane_id: String,
    pub context_key: String,
    pub placement: BoardPanePlacement,
    pub workspace_id: Option<String>,
    pub origin_pane_id: Option<String>,
    pub created_at: String,
}

fn is_tool_name(value: &str) -> bool {
    (1..=MAX_TOOL_NAME).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

const MARK_SELECT: &str = "SELECT id, space, issue_identifier, kind, text, detail_json, created_by, created_at, owner_herdr_socket, owner_herdr_pane_id, owner_claude_session_id FROM linear_marks";
const NOTE_SELECT: &str =
    "SELECT id, space, issue_identifier, body, author, created_at, owner_herdr_socket, owner_herdr_pane_id, owner_claude_session_id FROM linear_notes";
const SHOW_SELECT: &str = "SELECT id, space, issue_identifier, reason, requested_by, created_at, acknowledged_at, owner_herdr_socket, owner_herdr_pane_id, owner_claude_session_id, expires_at, outcome FROM linear_show_requests";
const ACTIVITY_SELECT: &str = "SELECT id, space, tool_name, issue_identifier, herdr_socket, herdr_pane_id, herdr_workspace_id, card_id, run_id, created_at FROM linear_activity";
const PANE_SELECT: &str = "SELECT herdr_socket, pane_id, context_key, placement, workspace_id, origin_pane_id, created_at FROM linear_board_panes";
const SPACE_BINDING_SELECT: &str = "SELECT herdr_session, space, project_id, display_name, team_ids_json, view_json FROM linear_space_bindings";
const WORKTREE_BINDING_SELECT: &str = "SELECT worktree_path, issue_identifier, state, branch, tab, display_name, team_ids_json, view_json, carried_json FROM linear_worktree_bindings";

fn row_to_mark(r: &Row) -> rusqlite::Result<Mark> {
    Ok(Mark {
        id: r.get(0)?,
        space: r.get(1)?,
        issue: r.get(2)?,
        kind: MarkKind::parse(&r.get::<_, String>(3)?)?,
        text: r.get(4)?,
        detail: optional_json(r.get(5)?)?,
        created_by: r.get(6)?,
        created_at: r.get(7)?,
        owner_herdr_socket: r.get(8)?,
        owner_herdr_pane_id: r.get(9)?,
        owner_claude_session_id: r.get(10)?,
    })
}

fn row_to_note(r: &Row) -> rusqlite::Result<Note> {
    Ok(Note {
        id: r.get(0)?,
        space: r.get(1)?,
        issue: r.get(2)?,
        body: r.get(3)?,
        author: r.get(4)?,
        created_at: r.get(5)?,
        owner_herdr_socket: r.get(6)?,
        owner_herdr_pane_id: r.get(7)?,
        owner_claude_session_id: r.get(8)?,
    })
}

fn row_to_show(r: &Row) -> rusqlite::Result<ShowRequest> {
    Ok(ShowRequest {
        id: r.get(0)?,
        space: r.get(1)?,
        issue: r.get(2)?,
        reason: r.get(3)?,
        requested_by: r.get(4)?,
        created_at: r.get(5)?,
        acknowledged_at: r.get(6)?,
        owner_herdr_socket: r.get(7)?,
        owner_herdr_pane_id: r.get(8)?,
        owner_claude_session_id: r.get(9)?,
        expires_at: r.get(10)?,
        outcome: r
            .get::<_, Option<String>>(11)?
            .map(|text| ShowOutcome::parse(&text))
            .transpose()?,
    })
}

fn row_to_activity(r: &Row) -> rusqlite::Result<Activity> {
    Ok(Activity {
        id: r.get(0)?,
        space: r.get(1)?,
        tool_name: r.get(2)?,
        issue: r.get(3)?,
        claims: ActivityClaims {
            herdr_socket: r.get(4)?,
            herdr_pane_id: r.get(5)?,
            herdr_workspace_id: r.get(6)?,
            card_id: r.get(7)?,
            run_id: r.get(8)?,
        },
        created_at: r.get(9)?,
    })
}

fn row_to_pane(r: &Row) -> rusqlite::Result<BoardPane> {
    Ok(BoardPane {
        herdr_socket: r.get(0)?,
        pane_id: r.get(1)?,
        context_key: r.get(2)?,
        placement: BoardPanePlacement::parse(&r.get::<_, String>(3)?)?,
        workspace_id: r.get(4)?,
        origin_pane_id: r.get(5)?,
        created_at: r.get(6)?,
    })
}

fn row_to_space_binding(r: &Row) -> rusqlite::Result<SpaceBinding> {
    Ok(SpaceBinding {
        herdr_session: r.get(0)?,
        space: r.get(1)?,
        project_id: r.get(2)?,
        display_name: r.get(3)?,
        team_ids: from_json_text(&r.get::<_, String>(4)?)?,
        view: optional_json(r.get(5)?)?,
    })
}

fn row_to_worktree_binding(r: &Row) -> rusqlite::Result<WorktreeBinding> {
    Ok(WorktreeBinding {
        worktree_path: r.get(0)?,
        issue: r.get(1)?,
        state: WorktreeBindingState::parse(&r.get::<_, String>(2)?)?,
        branch: r.get(3)?,
        tab: r.get(4)?,
        display_name: r.get(5)?,
        team_ids: from_json_text(&r.get::<_, String>(6)?)?,
        view: optional_json(r.get(7)?)?,
        carried: from_json_text(&r.get::<_, String>(8)?)?,
    })
}

fn row_to_mapping(r: &Row) -> rusqlite::Result<GroupingMapping> {
    Ok(GroupingMapping {
        levels: from_json_text(&r.get::<_, String>(0)?)?,
        filter: from_json_text(&r.get::<_, String>(1)?)?,
    })
}

fn validate_team_ids(team_ids: &[String]) -> Result<()> {
    team_ids
        .iter()
        .try_for_each(|id| require_text(id, "team id"))
}

impl Db {
    // -- grouping -------------------------------------------------------------

    pub fn grouping_config(&self) -> Result<Option<GroupingConfig>> {
        let Some(global) = self
            .conn
            .query_row(
                "SELECT levels_json, filter_json FROM linear_grouping WHERE space IS NULL",
                [],
                row_to_mapping,
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut statement = self.conn.prepare(
            "SELECT levels_json, filter_json, space FROM linear_grouping
             WHERE space IS NOT NULL ORDER BY position",
        )?;
        let spaces = statement
            .query_map([], |r| {
                Ok(SpaceGrouping {
                    mapping: row_to_mapping(r)?,
                    space: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(GroupingConfig { global, spaces }))
    }

    /// A space override is returned whole; the global mapping never fills
    /// its gaps.
    pub fn grouping_for_space(&self, space: &str) -> Result<Option<ResolvedGrouping>> {
        let own = self
            .conn
            .query_row(
                "SELECT levels_json, filter_json FROM linear_grouping WHERE space = ?1",
                params![space],
                row_to_mapping,
            )
            .optional()?;
        if let Some(mapping) = own {
            return Ok(Some(ResolvedGrouping {
                space: Some(space.to_owned()),
                mapping,
            }));
        }
        Ok(self
            .conn
            .query_row(
                "SELECT levels_json, filter_json FROM linear_grouping WHERE space IS NULL",
                [],
                row_to_mapping,
            )
            .optional()?
            .map(|mapping| ResolvedGrouping {
                space: None,
                mapping,
            }))
    }

    pub fn set_grouping_global(&self, mapping: &GroupingMapping) -> Result<()> {
        mapping.validate_as("the global mapping")?;
        self.conn.execute(
            "INSERT INTO linear_grouping (space, position, levels_json, filter_json)
             VALUES (NULL, 0, ?1, ?2)
             ON CONFLICT ((space IS NULL)) WHERE space IS NULL DO UPDATE SET
               levels_json = excluded.levels_json,
               filter_json = excluded.filter_json,
               updated_at = datetime('now')",
            params![
                to_json_text(&mapping.levels)?,
                to_json_text(&mapping.filter)?
            ],
        )?;
        Ok(())
    }

    /// Upserts one override. An existing space keeps its position; a new one
    /// goes last.
    pub fn set_grouping_space(&self, space: &str, mapping: &GroupingMapping) -> Result<()> {
        validate_space_name(space)?;
        mapping.validate_as(&format!("the mapping for space {}", shown_str(space)))?;
        let tx = self.conn.unchecked_transaction()?;
        let has_global: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM linear_grouping WHERE space IS NULL)",
            [],
            |r| r.get(0),
        )?;
        if !has_global {
            return Err(refuse(format!(
                "space {} cannot be mapped before a global mapping exists; a space mapping replaces the global one, so one must exist",
                shown_str(space)
            )));
        }
        tx.execute(
            "INSERT INTO linear_grouping (space, position, levels_json, filter_json)
             VALUES (?1, (SELECT coalesce(max(position) + 1, 0) FROM linear_grouping
                          WHERE space IS NOT NULL), ?2, ?3)
             ON CONFLICT (space) WHERE space IS NOT NULL DO UPDATE SET
               levels_json = excluded.levels_json,
               filter_json = excluded.filter_json,
               updated_at = datetime('now')",
            params![
                space,
                to_json_text(&mapping.levels)?,
                to_json_text(&mapping.filter)?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_grouping_space(&self, space: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM linear_grouping WHERE space = ?1",
            params![space],
        )? > 0)
    }

    /// Replaces the whole config in one transaction, after validating all of
    /// it: one fault anywhere refuses the write and leaves the stored config.
    pub fn replace_grouping(&self, config: &GroupingConfig) -> Result<()> {
        config.validate()?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM linear_grouping", [])?;
        tx.execute(
            "INSERT INTO linear_grouping (space, position, levels_json, filter_json)
             VALUES (NULL, 0, ?1, ?2)",
            params![
                to_json_text(&config.global.levels)?,
                to_json_text(&config.global.filter)?
            ],
        )?;
        for (position, entry) in config.spaces.iter().enumerate() {
            tx.execute(
                "INSERT INTO linear_grouping (space, position, levels_json, filter_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    entry.space,
                    i64::try_from(position).map_err(|e| Error::InvalidState(e.to_string()))?,
                    to_json_text(&entry.mapping.levels)?,
                    to_json_text(&entry.mapping.filter)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // -- space and worktree bindings --------------------------------------------

    pub fn set_space_binding(&self, binding: &SpaceBinding) -> Result<()> {
        require_text(&binding.herdr_session, "herdr session")?;
        require_text(&binding.space, "space")?;
        require_text(&binding.project_id, "project id")?;
        require_optional_text(binding.display_name.as_deref(), "display name")?;
        validate_team_ids(&binding.team_ids)?;
        self.conn.execute(
            "INSERT INTO linear_space_bindings
               (herdr_session, space, project_id, display_name, team_ids_json, view_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (herdr_session, space) DO UPDATE SET
               project_id = excluded.project_id,
               display_name = excluded.display_name,
               team_ids_json = excluded.team_ids_json,
               view_json = excluded.view_json,
               updated_at = datetime('now')",
            params![
                binding.herdr_session,
                binding.space,
                binding.project_id,
                binding.display_name,
                to_json_text(&binding.team_ids)?,
                binding.view.as_ref().map(to_json_text).transpose()?
            ],
        )?;
        Ok(())
    }

    pub fn space_binding(&self, herdr_session: &str, space: &str) -> Result<Option<SpaceBinding>> {
        Ok(self
            .conn
            .query_row(
                &format!("{SPACE_BINDING_SELECT} WHERE herdr_session = ?1 AND space = ?2"),
                params![herdr_session, space],
                row_to_space_binding,
            )
            .optional()?)
    }

    pub fn list_space_bindings(&self) -> Result<Vec<SpaceBinding>> {
        let mut statement = self.conn.prepare(&format!(
            "{SPACE_BINDING_SELECT} ORDER BY herdr_session, space"
        ))?;
        let rows = statement
            .query_map([], row_to_space_binding)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn remove_space_binding(&self, herdr_session: &str, space: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM linear_space_bindings WHERE herdr_session = ?1 AND space = ?2",
            params![herdr_session, space],
        )? > 0)
    }

    pub fn set_worktree_binding(&self, binding: &WorktreeBinding) -> Result<()> {
        require_absolute_path(&binding.worktree_path, "worktree path")?;
        require_issue(&binding.issue)?;
        require_optional_text(binding.branch.as_deref(), "branch")?;
        require_optional_text(binding.tab.as_deref(), "tab")?;
        require_optional_text(binding.display_name.as_deref(), "display name")?;
        validate_team_ids(&binding.team_ids)?;
        self.conn.execute(
            "INSERT INTO linear_worktree_bindings
               (worktree_path, issue_identifier, state, branch, tab, display_name,
                team_ids_json, view_json, carried_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (worktree_path) DO UPDATE SET
               issue_identifier = excluded.issue_identifier,
               state = excluded.state,
               branch = excluded.branch,
               tab = excluded.tab,
               display_name = excluded.display_name,
               team_ids_json = excluded.team_ids_json,
               view_json = excluded.view_json,
               carried_json = excluded.carried_json,
               updated_at = datetime('now')",
            params![
                binding.worktree_path,
                binding.issue,
                binding.state.as_str(),
                binding.branch,
                binding.tab,
                binding.display_name,
                to_json_text(&binding.team_ids)?,
                binding.view.as_ref().map(to_json_text).transpose()?,
                to_json_text(&binding.carried)?
            ],
        )?;
        Ok(())
    }

    pub fn worktree_binding(&self, worktree_path: &str) -> Result<Option<WorktreeBinding>> {
        Ok(self
            .conn
            .query_row(
                &format!("{WORKTREE_BINDING_SELECT} WHERE worktree_path = ?1"),
                params![worktree_path],
                row_to_worktree_binding,
            )
            .optional()?)
    }

    pub fn list_worktree_bindings(&self) -> Result<Vec<WorktreeBinding>> {
        let mut statement = self
            .conn
            .prepare(&format!("{WORKTREE_BINDING_SELECT} ORDER BY worktree_path"))?;
        let rows = statement
            .query_map([], row_to_worktree_binding)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// When the binding last changed. There is no creation column, so a
    /// re-bind of the same worktree restarts this time.
    pub fn worktree_binding_since(&self, worktree_path: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT updated_at FROM linear_worktree_bindings WHERE worktree_path = ?1",
                params![worktree_path],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn remove_worktree_binding(&self, worktree_path: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM linear_worktree_bindings WHERE worktree_path = ?1",
            params![worktree_path],
        )? > 0)
    }

    // -- session scopes and scope repositories ----------------------------------

    pub fn set_session_scope(&self, scope: &SessionScope) -> Result<()> {
        require_text(&scope.session_id, "session id")?;
        require_text(&scope.team_id, "team id")?;
        require_optional_text(scope.team_key.as_deref(), "team key")?;
        self.conn.execute(
            "INSERT INTO linear_session_scopes (session_id, team_id, team_key)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (session_id) DO UPDATE SET
               team_id = excluded.team_id,
               team_key = excluded.team_key,
               updated_at = datetime('now')",
            params![scope.session_id, scope.team_id, scope.team_key],
        )?;
        Ok(())
    }

    pub fn session_scope(&self, session_id: &str) -> Result<Option<SessionScope>> {
        Ok(self
            .conn
            .query_row(
                "SELECT session_id, team_id, team_key FROM linear_session_scopes
                 WHERE session_id = ?1",
                params![session_id],
                |r| {
                    Ok(SessionScope {
                        session_id: r.get(0)?,
                        team_id: r.get(1)?,
                        team_key: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn remove_session_scope(&self, session_id: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM linear_session_scopes WHERE session_id = ?1",
            params![session_id],
        )? > 0)
    }

    /// Appends `repo_path` to the scope's list; a path already listed keeps
    /// its place.
    pub fn add_scope_repo(&self, scope_key: &str, repo_path: &str) -> Result<()> {
        if !is_scope_key(scope_key) {
            return Err(refuse(format!(
                "scope key {} is refused; a scope key is project-<id>.team-<id>, team-<id> or project-<id>",
                shown_str(scope_key)
            )));
        }
        require_absolute_path(repo_path, "repository path")?;
        self.conn.execute(
            "INSERT INTO linear_scope_repos (scope_key, repo_path, position)
             VALUES (?1, ?2, (SELECT coalesce(max(position) + 1, 0) FROM linear_scope_repos
                              WHERE scope_key = ?1))
             ON CONFLICT (scope_key, repo_path) DO NOTHING",
            params![scope_key, repo_path],
        )?;
        Ok(())
    }

    pub fn scope_repos(&self, scope_key: &str) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare(
            "SELECT repo_path FROM linear_scope_repos WHERE scope_key = ?1 ORDER BY position",
        )?;
        let rows = statement
            .query_map(params![scope_key], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    pub fn remove_scope_repo(&self, scope_key: &str, repo_path: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM linear_scope_repos WHERE scope_key = ?1 AND repo_path = ?2",
            params![scope_key, repo_path],
        )? > 0)
    }

    // -- marks, notes, show-requests --------------------------------------------

    pub fn add_mark(&self, mark: &NewMark<'_>) -> Result<Mark> {
        require_text(mark.space, "space")?;
        require_issue(mark.issue)?;
        require_optional_text(mark.created_by, "mark author")?;
        require_text(mark.session, "herdr session")?;
        self.conn.execute(
            "INSERT INTO linear_marks (space, issue_identifier, kind, text, detail_json, created_by,
               owner_herdr_socket, owner_herdr_pane_id, owner_claude_session_id, herdr_session)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                mark.space,
                mark.issue,
                mark.kind.as_str(),
                mark.text,
                mark.detail.as_ref().map(to_json_text).transpose()?,
                mark.created_by,
                mark.owner.herdr_socket,
                mark.owner.herdr_pane_id,
                mark.owner.claude_session_id,
                mark.session
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(self.conn.query_row(
            &format!("{MARK_SELECT} WHERE id = ?1"),
            params![id],
            row_to_mark,
        )?)
    }

    pub fn list_marks(&self, session: &str, space: &str) -> Result<Vec<Mark>> {
        let mut statement = self.conn.prepare(&format!(
            "{MARK_SELECT} WHERE herdr_session = ?1 AND space = ?2 ORDER BY id"
        ))?;
        let rows = statement
            .query_map(params![session, space], row_to_mark)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn space_issue_marks(&self, session: &str, space: &str, issue: &str) -> Result<Vec<Mark>> {
        let mut statement = self.conn.prepare(&format!(
            "{MARK_SELECT} WHERE herdr_session = ?1 AND space = ?2 AND issue_identifier = ?3
             ORDER BY id"
        ))?;
        let rows = statement
            .query_map(params![session, space, issue], row_to_mark)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn issue_marks(&self, issue: &str) -> Result<Vec<Mark>> {
        let mut statement = self.conn.prepare(&format!(
            "{MARK_SELECT} WHERE issue_identifier = ?1 ORDER BY id"
        ))?;
        let rows = statement
            .query_map(params![issue], row_to_mark)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn mark(&self, id: i64) -> Result<Option<Mark>> {
        Ok(self
            .conn
            .query_row(
                &format!("{MARK_SELECT} WHERE id = ?1"),
                params![id],
                row_to_mark,
            )
            .optional()?)
    }

    pub fn note(&self, id: i64) -> Result<Option<Note>> {
        Ok(self
            .conn
            .query_row(
                &format!("{NOTE_SELECT} WHERE id = ?1"),
                params![id],
                row_to_note,
            )
            .optional()?)
    }

    pub fn show_request(&self, id: i64) -> Result<Option<ShowRequest>> {
        Ok(self
            .conn
            .query_row(
                &format!("{SHOW_SELECT} WHERE id = ?1"),
                params![id],
                row_to_show,
            )
            .optional()?)
    }

    pub fn remove_mark(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM linear_marks WHERE id = ?1", params![id])?
            > 0)
    }

    pub fn add_note(
        &self,
        session: &str,
        space: &str,
        issue: &str,
        body: &str,
        author: &str,
        owner: &LinearOwner,
    ) -> Result<Note> {
        require_text(space, "space")?;
        require_issue(issue)?;
        require_text(author, "note author")?;
        require_text(session, "herdr session")?;
        if body.is_empty() {
            return Err(refuse("a note body must not be empty".into()));
        }
        self.conn.execute(
            "INSERT INTO linear_notes (space, issue_identifier, body, author,
               owner_herdr_socket, owner_herdr_pane_id, owner_claude_session_id, herdr_session)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                space,
                issue,
                body,
                author,
                owner.herdr_socket,
                owner.herdr_pane_id,
                owner.claude_session_id,
                session
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(self.conn.query_row(
            &format!("{NOTE_SELECT} WHERE id = ?1"),
            params![id],
            row_to_note,
        )?)
    }

    pub fn list_notes(&self, session: &str, space: &str, issue: Option<&str>) -> Result<Vec<Note>> {
        let mut statement = self.conn.prepare(&format!(
            "{NOTE_SELECT} WHERE herdr_session = ?1 AND space = ?2
               AND (?3 IS NULL OR issue_identifier = ?3) ORDER BY id"
        ))?;
        let rows = statement
            .query_map(params![session, space, issue], row_to_note)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn remove_note(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM linear_notes WHERE id = ?1", params![id])?
            > 0)
    }

    pub fn add_show_request(&self, request: &NewShowRequest<'_>) -> Result<ShowRequest> {
        require_text(request.space, "space")?;
        require_issue(request.issue)?;
        require_optional_text(request.requested_by, "requester")?;
        require_text(request.session, "herdr session")?;
        self.conn.execute(
            "INSERT INTO linear_show_requests (space, issue_identifier, reason, requested_by,
               owner_herdr_socket, owner_herdr_pane_id, owner_claude_session_id, expires_at,
               herdr_session)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime(?8, 'unixepoch'), ?9)",
            params![
                request.space,
                request.issue,
                request.reason,
                request.requested_by,
                request.owner.herdr_socket,
                request.owner.herdr_pane_id,
                request.owner.claude_session_id,
                request.expires_at,
                request.session
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(self.conn.query_row(
            &format!("{SHOW_SELECT} WHERE id = ?1"),
            params![id],
            row_to_show,
        )?)
    }

    /// Keeps the request's id and `created_at`, so its place in the pinned
    /// line does not move.
    pub fn refresh_show_request(
        &self,
        id: i64,
        reason: Option<&str>,
        requested_by: Option<&str>,
        expires_at: i64,
    ) -> Result<bool> {
        require_optional_text(requested_by, "requester")?;
        Ok(self.conn.execute(
            "UPDATE linear_show_requests
             SET reason = ?2, requested_by = ?3, expires_at = datetime(?4, 'unixepoch')
             WHERE id = ?1 AND outcome IS NULL AND acknowledged_at IS NULL",
            params![id, reason, requested_by, expires_at],
        )? > 0)
    }

    /// Requests with no outcome yet, including overdue ones the sweep has
    /// not reached.
    fn open_show_requests(&self, scope: Option<(&str, &str)>) -> Result<Vec<ShowRequest>> {
        let (session, space) = scope.unzip();
        let mut statement = self.conn.prepare(&format!(
            "{SHOW_SELECT} WHERE (?1 IS NULL OR (herdr_session = ?1 AND space = ?2))
               AND outcome IS NULL AND acknowledged_at IS NULL ORDER BY id"
        ))?;
        let rows = statement
            .query_map(params![session, space], row_to_show)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn pending_show_requests(
        &self,
        session: &str,
        space: &str,
        now: i64,
    ) -> Result<Vec<ShowRequest>> {
        let mut rows = self.open_show_requests(Some((session, space)))?;
        rows.retain(|r| r.is_pending(now));
        Ok(rows)
    }

    pub fn recent_resolved_show_requests(
        &self,
        session: &str,
        space: &str,
    ) -> Result<Vec<ShowRequest>> {
        let mut statement = self.conn.prepare(&format!(
            "{SHOW_SELECT} WHERE herdr_session = ?1 AND space = ?2 AND outcome IS NOT NULL
             ORDER BY acknowledged_at DESC, id DESC LIMIT ?3"
        ))?;
        let rows = statement
            .query_map(
                params![session, space, RECENT_RESOLVED_SHOW_REQUESTS as i64],
                row_to_show,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Sets `outcome` and `acknowledged_at` together: readers that predate
    /// `outcome` still treat only `acknowledged_at IS NULL` as pending.
    /// False when the request is unknown or already closed.
    pub fn close_show_request(&self, id: i64, outcome: ShowOutcome) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE linear_show_requests
             SET outcome = ?2, acknowledged_at = datetime('now')
             WHERE id = ?1 AND outcome IS NULL AND acknowledged_at IS NULL",
            params![id, outcome.as_str()],
        )? > 0)
    }

    /// Marks every overdue request `expired` and returns the spaces that
    /// changed, sorted and once each.
    pub fn expire_overdue(&self, now: i64) -> Result<Vec<String>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut spaces = std::collections::BTreeSet::new();
        for request in self.open_show_requests(None)? {
            if request.is_overdue(now)
                && self.close_show_request(request.id, ShowOutcome::Expired)?
            {
                spaces.insert(request.space);
            }
        }
        tx.commit()?;
        Ok(spaces.into_iter().collect())
    }

    // -- activity ---------------------------------------------------------------

    /// Records one Linear write and prunes the session's space to the newest
    /// [`LINEAR_ACTIVITY_KEEP_PER_SPACE`] rows in the same transaction.
    pub fn record_activity(&self, activity: &NewActivity<'_>) -> Result<Activity> {
        require_optional_text(activity.space, "space")?;
        require_text(activity.session, "herdr session")?;
        if !is_tool_name(activity.tool_name) {
            return Err(refuse(format!(
                "tool name {} is refused; a tool name is 1 to {MAX_TOOL_NAME} ASCII letters, digits, `_`, `-`, `.` or `:`",
                shown_str(activity.tool_name)
            )));
        }
        if let Some(issue) = activity.issue {
            require_issue(issue)?;
        }
        let claims = activity.claims;
        require_optional_text(claims.herdr_socket.as_deref(), "herdr socket claim")?;
        require_optional_text(claims.herdr_pane_id.as_deref(), "herdr pane claim")?;
        require_optional_text(
            claims.herdr_workspace_id.as_deref(),
            "herdr workspace claim",
        )?;
        let keep = i64::try_from(LINEAR_ACTIVITY_KEEP_PER_SPACE)
            .map_err(|e| Error::InvalidState(e.to_string()))?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO linear_activity
               (space, tool_name, issue_identifier, herdr_socket, herdr_pane_id,
                herdr_workspace_id, card_id, run_id, herdr_session)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                activity.space,
                activity.tool_name,
                activity.issue,
                claims.herdr_socket,
                claims.herdr_pane_id,
                claims.herdr_workspace_id,
                claims.card_id,
                claims.run_id,
                activity.session
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "DELETE FROM linear_activity WHERE herdr_session = ?1 AND space IS ?2 AND id NOT IN (
               SELECT id FROM linear_activity WHERE herdr_session = ?1 AND space IS ?2
               ORDER BY id DESC LIMIT ?3)",
            params![activity.session, activity.space, keep],
        )?;
        let row = tx.query_row(
            &format!("{ACTIVITY_SELECT} WHERE id = ?1"),
            params![id],
            row_to_activity,
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Newest first. `space` `None` lists unattributed activity.
    pub fn list_activity(&self, space: Option<&str>, limit: usize) -> Result<Vec<Activity>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.conn.prepare(&format!(
            "{ACTIVITY_SELECT} WHERE space IS ?1 ORDER BY id DESC LIMIT ?2"
        ))?;
        let rows = statement
            .query_map(params![space, limit], row_to_activity)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // -- board panes --------------------------------------------------------------

    pub fn record_board_pane(&self, pane: &NewBoardPane<'_>) -> Result<BoardPane> {
        require_text(pane.herdr_socket, "herdr socket")?;
        require_text(pane.pane_id, "pane id")?;
        require_text(pane.context_key, "board context")?;
        require_optional_text(pane.workspace_id, "workspace id")?;
        require_optional_text(pane.origin_pane_id, "origin pane id")?;
        self.conn.execute(
            "INSERT OR REPLACE INTO linear_board_panes
               (herdr_socket, pane_id, context_key, placement, workspace_id, origin_pane_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                pane.herdr_socket,
                pane.pane_id,
                pane.context_key,
                pane.placement.as_str(),
                pane.workspace_id,
                pane.origin_pane_id
            ],
        )?;
        Ok(self.conn.query_row(
            &format!("{PANE_SELECT} WHERE herdr_socket = ?1 AND pane_id = ?2"),
            params![pane.herdr_socket, pane.pane_id],
            row_to_pane,
        )?)
    }

    /// The newest recorded pane for a context. A caller must still confirm
    /// the pane is live before reusing it.
    pub fn board_pane_for_context(
        &self,
        herdr_socket: &str,
        context_key: &str,
    ) -> Result<Option<BoardPane>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "{PANE_SELECT} WHERE herdr_socket = ?1 AND context_key = ?2
                     ORDER BY rowid DESC LIMIT 1"
                ),
                params![herdr_socket, context_key],
                row_to_pane,
            )
            .optional()?)
    }

    pub fn list_board_panes(&self, herdr_socket: &str) -> Result<Vec<BoardPane>> {
        let mut statement = self.conn.prepare(&format!(
            "{PANE_SELECT} WHERE herdr_socket = ?1 ORDER BY rowid"
        ))?;
        let rows = statement
            .query_map(params![herdr_socket], row_to_pane)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn remove_board_pane(&self, herdr_socket: &str, pane_id: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM linear_board_panes WHERE herdr_socket = ?1 AND pane_id = ?2",
            params![herdr_socket, pane_id],
        )? > 0)
    }
}
