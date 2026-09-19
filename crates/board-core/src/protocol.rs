//! boardd socket protocol (v1) — serde types, the single source of truth.
//!
//! See `docs/protocol.md` for semantics. Transport is newline-delimited JSON over a
//! Unix socket; every request/response/event and every method's params/result is
//! represented here.

use serde::{
    de::{self, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};

use crate::model::{Board, Card, Column, Comment, CommentHistory, CommentRecord, Project, Run};

/// A nullable field in a partial update.
///
/// `Unchanged` is represented by an omitted JSON member, `Clear` by JSON
/// `null`, and `Set` by the value itself. This keeps protocol v1 compatible
/// while allowing clients to intentionally clear a stored nullable value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Patch<T> {
    #[default]
    Unchanged,
    Clear,
    Set(T),
}

impl<T> Patch<T> {
    pub fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }

    /// Build a patch from a CLI-style `--clear-x` / `--x <value>` pair, where an
    /// absent value means "leave the stored value alone".
    ///
    /// `clear` wins over `value`: `Clear`, else `Some(v)` → `Set(v)`, else
    /// `Unchanged`. Contrast [`Patch::from_option`], where `None` clears.
    pub fn from_flags(clear: bool, value: Option<T>) -> Patch<T> {
        if clear {
            Self::Clear
        } else {
            match value {
                Some(value) => Self::Set(value),
                None => Self::Unchanged,
            }
        }
    }

    /// Build a patch from a form-style optional field that always states the
    /// desired end state: `Some(v)` → `Set(v)`, `None` → `Clear`.
    ///
    /// This never yields `Unchanged` — an emptied field is an intentional
    /// clear. Contrast [`Patch::from_flags`], where a missing value means
    /// "unchanged".
    pub fn from_option(value: Option<T>) -> Patch<T> {
        match value {
            Some(value) => Self::Set(value),
            None => Self::Clear,
        }
    }
}

impl<T: Serialize> Serialize for Patch<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // Update fields use `skip_serializing_if` to omit this variant.
            // Serializing it directly as null is the least surprising fallback.
            Self::Unchanged | Self::Clear => serializer.serialize_none(),
            Self::Set(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PatchVisitor<T>(std::marker::PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for PatchVisitor<T> {
            type Value = Patch<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("null or a patch value")
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Patch::Clear)
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Patch::Clear)
            }

            fn visit_some<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Self::Value, D::Error> {
                T::deserialize(deserializer).map(Patch::Set)
            }
        }

        deserializer.deserialize_option(PatchVisitor(std::marker::PhantomData))
    }
}

// ---------------------------------------------------------------------------
// Shared enums
// ---------------------------------------------------------------------------

/// Column trigger: `auto` starts a run on entry, `manual` waits for a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    Manual,
    Auto,
}

/// Where a card's agent runs, within its herdr session.
///
/// - [`SpaceKind::Workspace`] — an ALREADY-OPEN workspace in the session;
///   `space_ref` is its workspace id (or, on dispatch, a case-insensitive label).
/// - [`SpaceKind::NewWorkspace`] — the daemon creates a workspace on first
///   dispatch (label = `space_ref`, cwd = `space_cwd`), reusing an existing
///   workspace with that label if one is already open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceKind {
    Workspace,
    NewWorkspace,
}

/// Live card status.
///
/// - [`CardStatus::Awaiting`] — the agent finished (or went idle past the
///   grace period) without an explicit `board done`; the run stays OPEN and
///   the column timeout is paused. Never becomes a failure on its own.
/// - [`CardStatus::Done`] — completion confirmed via `board done ok` with no
///   target column (with a target column the card moves instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardStatus {
    Idle,
    Queued,
    Running,
    Blocked,
    Failed,
    Awaiting,
    Done,
}

/// Why a card entered [`CardStatus::Awaiting`]. Set on entry, cleared (NULL)
/// on exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitingReason {
    /// herdr reported `agent_status=done` and no `board done` arrived.
    AgentDone,
    /// `agent_status=idle` sustained past `idle_grace_seconds`, no `board done`.
    IdleExpired,
}

/// Terminal outcome of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunOutcome {
    Ok,
    Fail,
    Cancelled,
    Lost,
}

/// Which archived-card set a card list should expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardVisibility {
    Active,
    All,
    Archived,
}

/// Reasoning effort level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// Declare a variant's canonical wire/DB string, plus any accepted aliases
/// (`Variant => "canonical" | "alias"`). Only the canonical string is ever
/// emitted; aliases exist so a second vocabulary (e.g. the CLI's hyphenated
/// flag values) parses into the same variant.
macro_rules! str_enum {
    ($ty:ty { $($variant:ident => $s:literal $(| $alias:literal)*),+ $(,)? }) => {
        impl $ty {
            /// Canonical wire/DB string.
            pub fn as_str(&self) -> &'static str {
                match self { $( <$ty>::$variant => $s ),+ }
            }
            /// Parse from a wire/DB string (canonical form or a declared alias).
            pub fn parse_str(s: &str) -> Option<Self> {
                match s { $( $s $(| $alias)* => Some(<$ty>::$variant), )+ _ => None }
            }
        }
        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum!(Trigger { Manual => "manual", Auto => "auto" });
str_enum!(SpaceKind { Workspace => "workspace", NewWorkspace => "new_workspace" | "new-workspace" });
str_enum!(CardStatus {
    Idle => "idle", Queued => "queued", Running => "running",
    Blocked => "blocked", Failed => "failed",
    Awaiting => "awaiting", Done => "done",
});
str_enum!(AwaitingReason {
    AgentDone => "agent_done", IdleExpired => "idle_expired",
});
str_enum!(RunOutcome {
    Ok => "ok", Fail => "fail", Cancelled => "cancelled", Lost => "lost",
});
str_enum!(CardVisibility {
    Active => "active", All => "all", Archived => "archived",
});

/// Alias so board/project visibility reuses the same vocabulary.
pub type Visibility = CardVisibility;
str_enum!(Effort {
    Off => "off", Minimal => "minimal", Low => "low", Medium => "medium",
    High => "high", Xhigh => "xhigh", Max => "max",
});

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// A request line: `{"id","method","params"?}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// A response line: `{"id","result"}` or `{"id","error"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: impl Into<String>, result: serde_json::Value) -> Self {
        Response {
            id: id.into(),
            result: Some(result),
            error: None,
        }
    }
    pub fn err(id: impl Into<String>, code: i32, message: impl Into<String>) -> Self {
        Self::err_with_details(id, code, None::<String>, message, None)
    }

    /// Construct an additive structured error without changing the legacy
    /// `Response::err` call sites.
    pub fn err_with_details(
        id: impl Into<String>,
        code: i32,
        kind: Option<impl Into<String>>,
        message: impl Into<String>,
        details: Option<serde_json::Value>,
    ) -> Self {
        Response {
            id: id.into(),
            result: None,
            error: Some(RpcError {
                code,
                kind: kind.map(Into::into),
                message: message.into(),
                details,
            }),
        }
    }
}

/// Structured error payload. `kind` and `details` are optional additions so
/// protocol-v1 clients that only know `code` and `message` remain readable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Why the board changed (coarse; clients refetch `board.get`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardChangedReason {
    CardMoved,
    CardCreated,
    CardUpdated,
    CardDeleted,
    CardArchived,
    ColumnChanged,
    CommentAdded,
    RunStarted,
    RunEnded,
    RunBlocked,
    BoardArchived,
    BoardRestored,
    ProjectArchived,
    ProjectRestored,
}

/// Streamed to subscribers (no `id` field on the wire).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    BoardChanged {
        reason: BoardChangedReason,
        /// The board the change happened on. `None` = a coarse, board-agnostic
        /// refresh signal (subscribers refetch whatever boards they hold).
        /// A cross-board card transfer emits one event per affected board.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        board_id: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card_id: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column_id: Option<i64>,
    },
    RunEnded {
        card_id: i64,
        run_id: i64,
        outcome: RunOutcome,
    },
}

// ---------------------------------------------------------------------------
// daemon methods
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub version: String,
    pub db_path: String,
    pub herdr_connected: bool,
    pub active_runs: i64,
    pub queued_runs: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopResult {
    pub stopping: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscribeResult {
    pub subscribed: bool,
}

// ---------------------------------------------------------------------------
// board / column methods
// ---------------------------------------------------------------------------

/// `board.open` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardOpenParams {
    pub scope_path: String,
}

/// `board.rename` params. The board id is stable; only its display name is
/// changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardRenameParams {
    #[serde(alias = "id")]
    pub board_id: i64,
    pub name: String,
}

/// `board.get` params. Omitted id preserves the legacy Global default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardGetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
}

/// `board.list` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardListResult {
    pub boards: Vec<Board>,
}

/// `board.list` params. An omitted `project_id` preserves the legacy listing
/// of every board across all projects; with it, only that project's boards.
/// `visibility` defaults to `active` when omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
}

/// `board.create` params: a named board inside one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardCreateParams {
    pub project_id: i64,
    pub name: String,
}

/// `board.select` params: persist this board (and its project) as the context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardSelectParams {
    pub board_id: i64,
}

/// `board.archive` params. `archived=true` archives, `false` restores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardArchiveParams {
    #[serde(alias = "id")]
    pub board_id: i64,
    pub archived: bool,
}

/// `project.archive` params. `archived=true` archives, `false` restores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectArchiveParams {
    pub scope_path: String,
    pub archived: bool,
}

/// `project.get` params: show one project without side effects.
/// `visibility` filters the boards inside the detail; omitted means `active`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectGetParams {
    pub scope_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
}

/// `project.list` params. Omitted `visibility` means `active`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
}

/// `project.open` / `project.create` params (identical shapes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectScopeParams {
    pub scope_path: String,
}

pub type ProjectOpenParams = ProjectScopeParams;
pub type ProjectCreateParams = ProjectScopeParams;

/// `project.select` params. `board_id` is an explicit board choice; omitted,
/// the project's persisted selected board (else its first board `main`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSelectParams {
    pub scope_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
}

/// `project.get` result: the project plus its boards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDetail {
    pub project: Project,
    pub boards: Vec<Board>,
    /// The project's persisted selected board, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_board: Option<Board>,
}

/// One project as served by `project.list`: its boards plus the selection and
/// recency data the pickers need, all in one call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub project: Project,
    pub boards: Vec<Board>,
    /// The project's persisted selected board id, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_board_id: Option<i64>,
    /// This project's recent board ids, most recent first, capped at 3,
    /// excluding its selected board.
    #[serde(default)]
    pub recent_board_ids: Vec<i64>,
}

/// `project.list` result: everything the project/board pickers need.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectListResult {
    /// Projects ordered by folder name; the special Global project last.
    pub projects: Vec<ProjectInfo>,
    /// The persisted selected project, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_project_id: Option<i64>,
    /// Recent project ids, most recent first, capped at 3, excluding the
    /// selected project.
    #[serde(default)]
    pub recent_project_ids: Vec<i64>,
}

/// `project.open` / `project.create` / `project.select` result: the project
/// plus the board snapshot the caller lands on (the chosen board, the
/// project's selected board, or its first board `main`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectOpenResult {
    pub project: Project,
    pub board: BoardSnapshot,
}

/// `project.selected` result: the persisted context, or both `None` when no
/// selection exists yet (e.g. right after migration, before the first
/// board-aware command bootstraps it from the current directory).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectSelectedResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Project>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<BoardSnapshot>,
}

/// A compact view of a run that is currently started and open on a board.
///
/// This is intentionally separate from [`Run`]: board snapshots only need the
/// card identity and start point required by clients to render live-run state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveRunSummary {
    pub card_id: i64,
    pub started_at: String,
}

/// `board.get` / `board.open` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardSnapshot {
    pub board: Board,
    pub columns: Vec<Column>,
    pub cards: Vec<Card>,
    /// Started, open runs for cards belonging to this board. The default keeps
    /// older v1 clients/snapshots readable when this additive field is absent.
    #[serde(default)]
    pub active_runs: Vec<ActiveRunSummary>,
}

/// `column.create` params.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnCreateParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_success_column_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_fail_column_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_session: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<i64>,
}

/// `column.update` params — any subset; `id` required.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnUpdateParams {
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub system_prompt: Patch<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub on_success_column_id: Patch<i64>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub on_fail_column_id: Patch<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_session: Option<bool>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub harness_override: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub model_override: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub effort_override: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub permission_override: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub timeout_minutes: Patch<i64>,
}

/// `column.reorder` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnReorderParams {
    pub id: i64,
    pub position: i64,
}

/// `column.delete` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDeleteParams {
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_cards_to: Option<i64>,
}

/// `{deleted:true}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeletedResult {
    pub deleted: bool,
}

/// `template.apply` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateApplyParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// card methods
// ---------------------------------------------------------------------------

/// `card.create` params.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CardCreateParams {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// herdr session name; `None` = the daemon's default session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_kind: Option<SpaceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_ref: Option<String>,
    /// Working directory for a `new_workspace` space (required for that kind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
}

/// `card.update` params — any subset; `id` required.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CardUpdateParams {
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub model: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub effort: Patch<Effort>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub permission_mode: Patch<String>,
    /// herdr session name; `null` clears the card's explicit session.
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub session: Patch<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_kind: Option<SpaceKind>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub space_ref: Patch<String>,
    /// Working directory for a `new_workspace` space.
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub space_cwd: Patch<String>,
}

/// `card.archive` params — archive (`true`) or restore (`false`) a card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardArchiveParams {
    pub id: i64,
    pub archived: bool,
}

/// `card.move` params — the dispatch trigger.
///
/// `board_id` declares the destination board for a cross-board transfer:
/// when present and different from the card's current board, the card is
/// transferred (its `cards.board_id`/`column_id` are moved atomically).
/// Omitted (or equal to the current board) keeps the historical intra-board
/// move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardMoveParams {
    pub id: i64,
    pub column_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
}

/// `card.get` / `card.delete` / etc. by-id params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardIdParams {
    pub id: i64,
}

/// `card.list` params.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CardListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_id: Option<i64>,
    /// Omitted preserves the active-only board view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<CardVisibility>,
}

/// `card.get` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardDetail {
    pub card: Card,
    pub comments: Vec<Comment>,
    pub runs: Vec<Run>,
}

/// Display labels for a card's optionals, stamped daemon-side. The wire fields
/// keep their `None`-means-default semantics; these are the resolved display
/// values: the session's real name (or the `default session` marker when
/// unset and unresolvable), and the effort / permission / model value or its
/// `default …` marker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardLabels {
    /// Resolved herdr session name, or `default session` when unset and
    /// nothing resolves.
    pub session: String,
    pub effort: String,
    pub permission: String,
    /// `default model` when unset (harness default).
    pub model: String,
}

// ---------------------------------------------------------------------------
// comment / run methods
// ---------------------------------------------------------------------------

/// `comment.add` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentAddParams {
    pub card_id: i64,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Optional daemon-supplied actor identity used to authorize agent-owned
    /// comments. It is additive and absent for human callers.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "run_id")]
    pub actor_run_id: Option<i64>,
    /// The caller's Herdr pane id (`HERDR_PANE_ID`); see [`RunDoneParams`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_pane_id: Option<String>,
}

/// `comment.get`, `comment.delete`, and `comment.history` id params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentIdParams {
    #[serde(alias = "comment_id")]
    pub id: i64,
}

pub type CommentGetParams = CommentIdParams;
pub type CommentHistoryParams = CommentIdParams;

/// `comment.delete` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentDeleteParams {
    #[serde(alias = "comment_id")]
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "run_id")]
    pub actor_run_id: Option<i64>,
}

/// `comment.update` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentUpdateParams {
    #[serde(alias = "comment_id")]
    pub id: i64,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "run_id")]
    pub actor_run_id: Option<i64>,
}

/// Current comment returned by management methods.
pub type CommentResult = CommentRecord;
pub type CommentGetResult = CommentRecord;
pub type CommentUpdateResult = CommentRecord;
pub type CommentDeleteResult = DeletedResult;

/// Audit snapshots returned by `comment.history`.
pub type CommentHistoryResult = Vec<CommentHistory>;

/// `run.done` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunDoneParams {
    pub card_id: i64,
    pub outcome: RunOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<i64>,
    /// The caller's Herdr pane id (`HERDR_PANE_ID`). When it matches the card's
    /// open-run pane, the caller is that run's (possibly reused across stages)
    /// agent pane, so a stale `BOARD_RUN_ID` from the pane's first stage does
    /// not reject the call. Additive; absent for human/non-pane callers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_pane_id: Option<String>,
}

/// Internal `run.pane_exited` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunPaneExitedParams {
    pub card_id: i64,
    pub run_id: i64,
}

/// `run.cancel` / `run.retry` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCardParams {
    pub card_id: i64,
}

/// `run.focus` params. `origin_socket` identifies the invoking Herdr session.
///
/// `run_id` is **required**: the daemon never implicitly picks a run. Callers
/// that want "the newest run with a pane" resolve that themselves from the
/// card's run list and pass the exact id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFocusParams {
    pub card_id: i64,
    pub run_id: i64,
    pub origin_socket: String,
}

/// What `run.focus` actually did, so the caller can tell an ordinary jump from
/// a rescue instead of inferring it from the pane id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFocusAction {
    /// The pane recorded on the run row was alive and got focus.
    FocusedRecordedPane,
    /// The recorded pane is gone, but an earlier rescue's pane for this run was
    /// still alive in the card tab, so that one got focus. No pane was created.
    FocusedRescuedPane,
    /// The recorded pane is gone; a **new** pane was created in the card tab and
    /// the harness conversation was resumed in it. The pane is ephemeral: it has
    /// no `runs` row, so the daemon does not own, watch, or time it out.
    Rescued,
}

/// `run.focus` result: the full identity of the run that was focused, so the
/// caller can say exactly *which* historical run it landed on, plus what the
/// daemon had to do to get there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFocusResult {
    /// Focus vs. rescue. Missing on older serialized payloads, which always
    /// meant an ordinary focus of the recorded pane.
    #[serde(default = "default_focus_action")]
    pub action: RunFocusAction,
    /// The pane the run row records, when it records one. On a rescue this is
    /// the **dead** pane, kept for diagnostics; `pane_id` is the live one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_pane_id: Option<String>,
    pub run_id: i64,
    pub card_id: i64,
    pub column_id: i64,
    pub harness: String,
    /// The **herdr session name** this run spawned into (`herdr --session <name>`),
    /// i.e. which Herdr instance/socket owns the pane. `None` = default session.
    /// This is NOT the harness conversation id — see `session_id`.
    pub session: Option<String>,
    /// The **harness conversation id** (Claude/Pi `--resume` id) recorded for
    /// this run. This is NOT a herdr session name — see `session`.
    pub session_id: Option<String>,
    /// The pane that now has focus — the recorded one, or the rescued one.
    pub pane_id: String,
}

fn default_focus_action() -> RunFocusAction {
    RunFocusAction::FocusedRecordedPane
}

/// `{run, card}` returned by run.done / run.cancel / run.retry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunActionResult {
    pub run: Run,
    pub card: Card,
}

// ---------------------------------------------------------------------------
// harness / space methods
// ---------------------------------------------------------------------------

/// `harness.capabilities` params. The result is a
/// [`HarnessCapabilities`](crate::capability::HarnessCapabilities); an unknown
/// harness yields error code 2 (not found).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessCapabilitiesParams {
    pub harness: String,
}

/// `harness.list` result: every harness the daemon knows about (built-ins
/// `pi`/`claude` plus every config-defined `[harness.NAME]`), sorted. Drives
/// the TUI harness/harness-override selects so they include config-defined
/// harnesses without a separate config read on the client.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessListResult {
    pub harnesses: Vec<String>,
}

/// A run space (herdr workspace) as surfaced by `space.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceInfo {
    pub id: String,
    pub label: String,
}

/// `space.list` params. `session` (`None` = default) scopes the listed
/// workspaces to that herdr session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// `space.list` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceListResult {
    pub spaces: Vec<SpaceInfo>,
}

/// A herdr session as surfaced by `session.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub default: bool,
    pub running: bool,
}

/// `session.list` result (no params).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionInfo>,
    /// The `default session` marker, daemon-sent so clients never format it
    /// themselves. Missing on older serialized payloads, so default to empty.
    #[serde(default)]
    pub default_label: String,
}

// ---------------------------------------------------------------------------
// pane methods
// ---------------------------------------------------------------------------

/// `pane.set_title` params: set the Herdr border label of one pane in the
/// **caller's own** herdr session.
///
/// `origin_socket` identifies that session exactly as it does for
/// [`RunFocusParams`] — the daemon owns every Herdr call, so a client that
/// wants its own pane renamed says which pane, in which session, and what to
/// call it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSetTitleParams {
    pub pane_id: String,
    pub title: String,
    pub origin_socket: String,
}

/// `pane.set_title` result: `{renamed:true}`, the same pure acknowledgement
/// shape as [`DeletedResult`] and [`StopResult`]. A rename that did not happen
/// is an error, never a `false` here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSetTitleResult {
    pub renamed: bool,
}

/// `pane.focus` params: focus one live pane in the caller's own herdr session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneFocusParams {
    pub origin_socket: String,
    pub pane_id: String,
}

/// `pane.focus` result. `gone` is the typed answer for a pane the origin
/// session no longer lists (including a pane of another session); it is not
/// an error because the snapshot that named the pane may simply be stale.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneFocusResult {
    pub focused: bool,
    pub gone: bool,
}

// ---------------------------------------------------------------------------
// linear methods (the work plugin's space snapshot)
// ---------------------------------------------------------------------------

/// `linear.snapshot` params. `origin_socket` is the caller's herdr socket;
/// absent means the script runs against the plugin's own default resolution
/// and every pane status is `unknown`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearSnapshotParams {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_socket: Option<String>,
    /// The caller's `BOARD_WORK_PLUGIN_ROOT`, preferred over the daemon's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_root: Option<String>,
}

/// How long a client waits for a `linear.snapshot` answer. Longer than the
/// daemon's script deadline plus its stop grace, so a slow run is answered by
/// the daemon; only a daemon that never answers reaches it.
pub const LINEAR_SNAPSHOT_CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(150);

/// How long a client waits for a `linear.list` answer: longer than the
/// daemon's list deadline plus its stop grace, as for the snapshot.
pub const LINEAR_LIST_CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(130);

/// `linear.issue` params: one issue, read whole for the issue page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearIssueParams {
    /// The issue's identifier (`WEB-3318`) or id, as the board holds it.
    pub issue: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_socket: Option<String>,
    /// The caller's `BOARD_WORK_PLUGIN_ROOT`, preferred over the daemon's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_root: Option<String>,
}

/// How long a client waits for a `linear.issue` answer. Shorter than the
/// snapshot's because the read is one Linear call by contract, not one per
/// issue page.
pub const LINEAR_ISSUE_CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The document `bin/work-issue.sh` prints. Every field the plugin can print as
/// `null` is an `Option`, and every connection defaults to empty, because an
/// explicit null from another process is not the same as an absent key to
/// serde (`docs/solutions/integration-issues/serde-default-rejects-explicit-null.md`).
// No `Eq`: the issue carries an estimate, which Linear types as a number and
// this reads as `f64` so a fractional one still parses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LinearIssueDocument {
    #[serde(default)]
    pub schema: i64,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub status: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub truncated: Vec<String>,
    #[serde(default)]
    pub issue: Option<LinearIssueDetail>,
}

/// One issue, with everything the issue page shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LinearIssueDetail {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub identifier: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub due_date: Option<String>,
    #[serde(default)]
    pub estimate: Option<f64>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub state: LinearIssueState,
    #[serde(default)]
    pub assignee: Option<LinearAssignee>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub project: Option<LinearNamed>,
    #[serde(default)]
    pub milestone: Option<LinearNamed>,
    #[serde(default)]
    pub cycle: Option<LinearCycle>,
    #[serde(default)]
    pub parent: Option<LinearLinkedIssue>,
    #[serde(default)]
    pub children: Vec<LinearLinkedIssue>,
    #[serde(default)]
    pub relations: Vec<LinearRelation>,
    #[serde(default)]
    pub comments: Vec<LinearComment>,
    #[serde(default)]
    pub history: Vec<LinearHistoryEvent>,
}

/// An id-and-name pair: a project or a milestone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearNamed {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearCycle {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub number: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
}

/// A sub-issue, the parent, or the other end of a relation. Carries enough to
/// open its own page from the row alone, which is why `title` and `state` are
/// here and not just an identifier.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearLinkedIssue {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub identifier: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub title: String,
    #[serde(default)]
    pub state: LinearIssueState,
}

/// One relation. `direction` separates the two ends of one Linear edge:
/// `outward` is what this issue points at, `inward` what points at it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearRelation {
    #[serde(default, deserialize_with = "null_as_empty")]
    pub r#type: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub direction: String,
    #[serde(default)]
    pub issue: LinearLinkedIssue,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearComment {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub body: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// The thread root this is a reply to; `None` on a root.
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// One history event, already filtered by the plugin to those that changed
/// something the page shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearHistoryEvent {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub from_state: Option<String>,
    #[serde(default)]
    pub to_state: Option<String>,
    #[serde(default)]
    pub from_assignee: Option<String>,
    #[serde(default)]
    pub to_assignee: Option<String>,
    #[serde(default)]
    pub from_priority: Option<i64>,
    #[serde(default)]
    pub to_priority: Option<i64>,
    #[serde(default)]
    pub added_labels: Vec<String>,
    #[serde(default)]
    pub removed_labels: Vec<String>,
}

/// Which plugin list `linear.list` runs: `bin/work-spaces.sh`,
/// `bin/work-projects.sh` or `bin/work-views.sh`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinearListKind {
    #[default]
    Spaces,
    Projects,
    Views,
}

/// `linear.list` params. `id` is the one argument a kind needs (the project
/// id for `views`); `origin_socket` and `plugin_root` mean what they mean for
/// [`LinearSnapshotParams`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearListParams {
    pub kind: LinearListKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_socket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_root: Option<String>,
}

/// A closed vocabulary, unlike the snapshot's string statuses: a picker must
/// never read a value it does not know as a successful empty list, so an
/// unrecognised status fails to parse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinearListStatus {
    Ok,
    Unavailable,
    Partial,
    #[default]
    Unknown,
}

/// The envelope every plugin list script prints. The scripts write
/// `"message": null` when there is nothing to say, hence `Option`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearListEnvelope<R> {
    #[serde(default)]
    pub status: LinearListStatus,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub rows: Vec<R>,
}

pub type LinearSpacesList = LinearListEnvelope<LinearSpaceRow>;
pub type LinearProjectsList = LinearListEnvelope<LinearProjectRow>;
pub type LinearViewsList = LinearListEnvelope<LinearViewRow>;

fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearSpaceRow {
    #[serde(default, deserialize_with = "null_as_empty")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub label: String,
    #[serde(default)]
    pub live: Option<bool>,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub state: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearProjectRow {
    #[serde(default, deserialize_with = "null_as_empty")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub name: String,
    #[serde(default)]
    pub team_key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearViewRow {
    #[serde(default, deserialize_with = "null_as_empty")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub name: String,
}

/// A `linear.list` answer decoded by the kind that was asked for. The wire
/// carries no kind tag, and a views row would also parse as a projects row, so
/// the request's kind is the only safe discriminator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum LinearListResult {
    Spaces(LinearSpacesList),
    Projects(LinearProjectsList),
    Views(LinearViewsList),
}

impl LinearListResult {
    pub fn kind(&self) -> LinearListKind {
        match self {
            LinearListResult::Spaces(_) => LinearListKind::Spaces,
            LinearListResult::Projects(_) => LinearListKind::Projects,
            LinearListResult::Views(_) => LinearListKind::Views,
        }
    }

    pub fn from_value(kind: LinearListKind, value: serde_json::Value) -> serde_json::Result<Self> {
        Ok(match kind {
            LinearListKind::Spaces => LinearListResult::Spaces(serde_json::from_value(value)?),
            LinearListKind::Projects => LinearListResult::Projects(serde_json::from_value(value)?),
            LinearListKind::Views => LinearListResult::Views(serde_json::from_value(value)?),
        })
    }
}

/// How long a client waits for a `linear.bind_handoff` answer: longer than the
/// daemon's busy retry on a slow new pane plus the herdr calls around it.
pub const LINEAR_BIND_HANDOFF_CLIENT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(300);

/// `linear.bind_handoff` params: ids and a directory only, never names. The
/// daemon validates every field before any herdr call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearBindHandoffParams {
    pub space: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    pub origin_socket: String,
}

/// The unfocused `bind` tab the handoff created and the pane running Claude.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearBindHandoffResult {
    pub tab_id: String,
    pub pane_id: String,
}

/// The document `bin/work-snapshot.sh` prints, plus the daemon-attached
/// `pane_status`. Mirrors `plugins/work/docs/snapshot.md`. Every section
/// defaults so a partial document still parses; statuses stay strings because
/// the plugin may add values the board does not know.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LinearSnapshot {
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub workspace: LinearWorkspace,
    #[serde(default)]
    pub mapping: LinearMapping,
    #[serde(default)]
    pub record: LinearRecord,
    #[serde(default)]
    pub project: LinearProject,
    #[serde(default)]
    pub view: LinearView,
    #[serde(default)]
    pub linear: LinearSource,
    #[serde(default)]
    pub herdr: LinearHerdr,
    #[serde(default)]
    pub groups: Vec<LinearGroup>,
    #[serde(default)]
    pub issues: std::collections::BTreeMap<String, LinearIssue>,
    #[serde(default)]
    pub unmapped: Vec<LinearUnmappedTab>,
    /// Live agent status per pane id named in `issues[].bindings[].panes` and
    /// `unmapped[].panes`, read by the daemon after the script ran.
    #[serde(default)]
    pub pane_status: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearWorkspace {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub live: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearMapping {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub space: String,
    #[serde(default)]
    pub tab: String,
    #[serde(default)]
    pub pane: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearRecord {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearProject {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub team_key: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearView {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub layout: Option<LinearViewLayout>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearViewLayout {
    #[serde(default)]
    pub grouping: String,
    #[serde(default)]
    pub column_order: Vec<String>,
    #[serde(default)]
    pub hidden: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearSource {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub cache_age_seconds: Option<i64>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearHerdr {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearGroup {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub label: String,
    /// The Linear workflow state type this column groups, when it groups by
    /// state at all; `None` under any other grouping or from the cache.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LinearIssue {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub identifier: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub state: LinearIssueState,
    #[serde(default)]
    pub assignee: Option<LinearAssignee>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub bindings: Vec<LinearBinding>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearIssueState {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearAssignee {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearBinding {
    #[serde(default)]
    pub worktree_path: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub tab: Option<LinearTabRef>,
    #[serde(default)]
    pub panes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearTabRef {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearUnmappedTab {
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub panes: Vec<String>,
}

impl LinearSnapshot {
    /// Every pane id the document names, in document order, deduplicated.
    pub fn pane_ids(&self) -> Vec<String> {
        let mut seen = std::collections::BTreeSet::new();
        let mut ids = Vec::new();
        let bound = self
            .issues
            .values()
            .flat_map(|issue| issue.bindings.iter())
            .flat_map(|binding| binding.panes.iter());
        let unmapped = self.unmapped.iter().flat_map(|tab| tab.panes.iter());
        for id in bound.chain(unmapped) {
            if seen.insert(id.as_str()) {
                ids.push(id.clone());
            }
        }
        ids
    }
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

/// Parse a wire timestamp (`YYYY-MM-DD HH:MM:SS`, UTC) to epoch seconds.
///
/// Every timestamp the daemon persists and serializes comes from SQLite's
/// `datetime('now')`, so this format is part of protocol v1 rather than a
/// presentation detail. The seconds field may be omitted (treated as `0`);
/// anything else that does not parse yields `None`.
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let (date, time) = s.split_once(' ')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mm: i64 = t.next()?.parse().ok()?;
    let ss: i64 = t.next().unwrap_or("0").parse().ok()?;
    Some(days_from_civil(year, month, day) * 86400 + hh * 3600 + mm * 60 + ss)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::days_from_civil;

    #[test]
    fn days_from_civil_anchors_epoch_and_leap_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        // 2000 is a leap year, 1900 is not.
        assert_eq!(
            days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28),
            2
        );
        assert_eq!(
            days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28),
            1
        );
    }
}
