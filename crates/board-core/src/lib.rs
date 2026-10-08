//! board-core — the shared heart of herdr-board (OWNED BY PHASE A).
//!
//! Pure, synchronous building blocks used by every other crate:
//! - [`protocol`]: serde types for the boardd socket protocol (source of truth).
//! - [`capability`]: static harness capability catalog + run-pane naming.
//! - [`model`]: SQLite-row structs (Board/Column/Card/Comment/Run).
//! - [`db`]: rusqlite store, migrations, CRUD, position management, queries.
//! - [`engine`]: pure column-engine transition/entry/validation decisions.
//! - [`prompt`]: prompt assembly and effective-settings resolution.
//! - [`harness`]: argv/env builders for the built-in Pi/Claude/Codex/OpenCode
//!   Antigravity and config harnesses.
//! - [`config`]: `~/.config/herdr-board/config.toml` loader.
//! - [`paths`]: db/socket/log/config path resolution.
//! - [`client`]: blocking NDJSON `BoardClient` (+ `FakeBoardClient` behind a feature).
//! - [`launch`]: durable, harness-neutral execution specifications.

pub mod agy_catalog;
pub mod capability;
pub mod client;
pub mod codex_catalog;
pub mod config;
pub mod db;
pub mod engine;
pub mod harness;
pub mod labels;
pub mod launch;
pub mod model;
pub mod opencode_catalog;
pub mod paths;
pub mod pi_catalog;
pub mod prompt;
pub mod protocol;
pub mod scope;
pub mod text;

pub use engine::ValidationError;

/// Crate-wide error type. `anyhow` is used at the process edges (CLI/daemon);
/// this `thiserror` enum carries the structured cases the daemon maps onto the
/// protocol's numeric error codes.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("config: {0}")]
    Config(String),

    /// Bad request / unknown method (protocol code 1).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Entity not found (protocol code 2).
    #[error("not found: {0}")]
    NotFound(String),

    /// Invalid state, e.g. delete a column with a running card (protocol code 3).
    #[error("invalid state: {0}")]
    InvalidState(String),

    /// herdr unavailable (protocol code 4).
    #[error("herdr unavailable: {0}")]
    HerdrUnavailable(String),

    /// The work plugin's scripts could not answer (protocol code 6). The
    /// daemon no longer runs them; the code stays so its meaning is stable.
    #[error("plugin unavailable: {0}")]
    PluginUnavailable(String),

    /// The installed work plugin did not ship the script an op needed
    /// (protocol code 7). No longer produced; kept so an older daemon's code 7
    /// still reads as "update the plugin" rather than a retryable failure.
    #[error("plugin op unsupported: {0}")]
    PluginOpUnsupported(String),

    #[error(transparent)]
    Validation(#[from] ValidationError),
}

/// The protocol's numeric error codes (see `docs/protocol.md`), as
/// [`Error::code`] returns them.
pub mod error_code {
    pub const BAD_REQUEST: i32 = 1;
    pub const NOT_FOUND: i32 = 2;
    pub const INVALID_STATE: i32 = 3;
    pub const HERDR_UNAVAILABLE: i32 = 4;
    pub const INTERNAL: i32 = 5;
    pub const PLUGIN_UNAVAILABLE: i32 = 6;
    pub const PLUGIN_OP_UNSUPPORTED: i32 = 7;
}

impl Error {
    /// Map onto the protocol's numeric error codes (see `docs/protocol.md`).
    pub fn code(&self) -> i32 {
        match self {
            Error::BadRequest(_) => error_code::BAD_REQUEST,
            Error::NotFound(_) => error_code::NOT_FOUND,
            Error::InvalidState(_) => error_code::INVALID_STATE,
            Error::Validation(v) => v.code(),
            Error::HerdrUnavailable(_) => error_code::HERDR_UNAVAILABLE,
            Error::PluginUnavailable(_) => error_code::PLUGIN_UNAVAILABLE,
            Error::PluginOpUnsupported(_) => error_code::PLUGIN_OP_UNSUPPORTED,
            Error::Sqlite(_) | Error::Json(_) | Error::Io(_) | Error::Config(_) => {
                error_code::INTERNAL
            }
        }
    }
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;
