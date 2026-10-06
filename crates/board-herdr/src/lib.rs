//! board-herdr — a typed, blocking client for the herdr socket API.
//!
//! herdr speaks NDJSON over a unix socket (`~/.config/herdr/herdr.sock`, or
//! `$HERDR_SOCKET`). Each request is one line
//! `{"id","method","params"}` and each reply is one line
//! `{"id","result"}` or `{"id","error":{code,message}}`.
//!
//! Two connection types:
//! - [`HerdrClient`] — request/response (workspace/tab/agent/pane/
//!   notification/session calls). Blocking; wrap in `spawn_blocking` in async
//!   code.
//! - [`HerdrEvents`] — a separate persistent connection streaming
//!   [`HerdrEvent`]s via `events.subscribe`.
//!
//! Method and field names are verified against `herdr api schema --json`
//! (Herdr 0.9.0, protocol 22), captured in `docs/herdr-0.9.0-schema.json`
//! and re-checked against 0.9.3 (`docs/herdr.md`); the legacy fixture at
//! `tests/fixtures/schema.json` is protocol 20.

mod client;
mod envelope;
mod error;
mod events;
mod params;
mod transport;
mod types;

/// Herdr releases this client accepts, oldest first. Each must speak
/// [`SUPPORTED_HERDR_PROTOCOL`]; a release that changes the protocol is refused.
pub const SUPPORTED_HERDR_VERSIONS: &[&str] = &["0.9.0", "0.9.1", "0.9.2", "0.9.3"];
/// The oldest supported Herdr release; test fakes report it in `ping`.
pub const SUPPORTED_HERDR_VERSION: &str = SUPPORTED_HERDR_VERSIONS[0];
/// The supported releases as shown in error messages and docs.
pub const SUPPORTED_HERDR_RANGE: &str = "0.9.0\u{2013}0.9.3";
/// The only Herdr socket protocol supported by this client.
pub const SUPPORTED_HERDR_PROTOCOL: u32 = 22;

pub use client::HerdrClient;
pub use envelope::{ErrorBody, Request, Response};
pub use error::{HerdrError, Result};
pub use events::{
    layout_subscriptions, naming_subscriptions, parse_event_line, watch_subscriptions, Backoff,
    HerdrEvent, HerdrEvents, Subscription,
};
pub use params::{
    AgentPromptParams, AgentPromptWaitOptions, AgentStartParams, AgentWaitParams, PaneRenameParams,
    PaneSplitParams, PluginPaneOpenParams, TabCreateParams, TabRenameParams, WorkspaceCreateParams,
};
pub use transport::{default_socket_path, SocketDeadlines};
pub use types::{
    AgentInfo, AgentSession, AgentStarted, AgentStatus, Layout, LayoutPane, LayoutSplit,
    NotificationShown, NotificationSound, PaneInfo, PaneReadResult, PluginPaneInfo,
    PluginPanePlacement, Pong, ReadSource, Rect, SessionSnapshot, SplitDirection, TabCreated,
    TabInfo, WorkspaceCreated, WorkspaceInfo,
};
