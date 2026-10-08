//! Where a calling agent sits in herdr: an explicit pane, then the
//! `HERDR_*` environment, then the daemon's `caller.resolve` (a remembered
//! choice for the Claude session, then a folder lookup that only offers
//! candidates). Each surface passes its own cwd and Claude session id.

use std::path::PathBuf;

use anyhow::Result;
use board_core::client::RpcClientError;
use board_core::paths::session_name_from_socket;
use board_core::protocol::{
    CallerCandidate, CallerLocation, CallerResolveParams, CallerResolveResult,
};
use board_core::text::strip_control_and_format;

use crate::commands::canonical_text;

pub(crate) const HERDR_SOCKET_PATH: &str = "HERDR_SOCKET_PATH";
pub(crate) const HERDR_PANE_ID: &str = "HERDR_PANE_ID";
pub(crate) const HERDR_WORKSPACE_ID: &str = "HERDR_WORKSPACE_ID";
const HERDR_TAB_ID: &str = "HERDR_TAB_ID";
const DEFAULT_SESSION: &str = "default";
const HERDR_UNAVAILABLE: i32 = 4;

pub(crate) fn env_text(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// An empty or malformed id is no claim: a rescued pane carries an empty
/// `BOARD_RUN_ID` on purpose, so its calls attribute to the card only.
pub(crate) fn env_id(key: &str) -> Option<i64> {
    env_text(key).and_then(|value| value.trim().parse().ok())
}

/// What a caller asks: its own directory, an optional `<session>/<pane id>`
/// it names on purpose, and its Claude session id.
#[derive(Debug, Clone, Default)]
pub(crate) struct CallerQuery {
    pub(crate) cwd: String,
    pub(crate) pane: Option<String>,
    pub(crate) claude_session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Env,
    Daemon,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolution {
    pub(crate) result: CallerResolveResult,
    pub(crate) source: Source,
}

/// A hook's directory for the folder lookup: the session's launch directory
/// first, because herdr's pane cwd is where `claude` started and the session
/// may have moved into a subdirectory or worktree since.
pub(crate) fn hook_cwd(payload_cwd: Option<&str>) -> String {
    let cwd = env_text("CLAUDE_PROJECT_DIR")
        .or_else(|| {
            payload_cwd
                .filter(|cwd| !cwd.is_empty())
                .map(str::to_string)
        })
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    canonical_text(cwd)
}

/// Candidates on one line for text a person or an agent reads. Titles are
/// terminal titles, so every field is stripped of control characters.
pub(crate) fn candidate_list(candidates: &[CallerCandidate]) -> String {
    candidates
        .iter()
        .map(|candidate| {
            format!(
                "{} (workspace {}, title {:?})",
                strip_control_and_format(&candidate.pane),
                strip_control_and_format(
                    candidate
                        .workspace_label
                        .as_deref()
                        .unwrap_or(&candidate.workspace_id)
                ),
                strip_control_and_format(candidate.title.as_deref().unwrap_or_default())
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The env location, only when all three herdr variables are set. A named
/// session's socket names its session; any other socket is the default one.
pub(crate) fn env_location(env: impl Fn(&str) -> Option<String>) -> Option<CallerLocation> {
    let socket = env(HERDR_SOCKET_PATH)?;
    let pane_id = env(HERDR_PANE_ID)?;
    let workspace_id = env(HERDR_WORKSPACE_ID)?;
    Some(CallerLocation {
        session: session_name_from_socket(Some(&socket))
            .unwrap_or_else(|| DEFAULT_SESSION.to_string()),
        socket,
        workspace_id,
        tab_id: env(HERDR_TAB_ID).unwrap_or_default(),
        pane_id,
    })
}

/// Explicit pane → env → daemon. `daemon` runs only when no full env applies,
/// so a caller with env never reaches boardd here. Without a pane, a daemon
/// that has no herdr (error 4) means the caller is not in herdr.
pub(crate) fn resolve(
    query: &CallerQuery,
    env: impl Fn(&str) -> Option<String>,
    daemon: impl FnOnce(&CallerResolveParams) -> Result<CallerResolveResult>,
) -> Result<Resolution> {
    let pane = query
        .pane
        .as_deref()
        .map(str::trim)
        .filter(|pane| !pane.is_empty());
    if pane.is_none() {
        if let Some(location) = env_location(env) {
            return Ok(Resolution {
                result: CallerResolveResult::Resolved { location },
                source: Source::Env,
            });
        }
    }
    let params = CallerResolveParams {
        cwd: query.cwd.clone(),
        pane: pane.map(str::to_string),
        claude_session_id: query.claude_session_id.clone(),
    };
    let result = match daemon(&params) {
        Ok(result) => result,
        Err(error) if pane.is_none() && is_herdr_unavailable(&error) => {
            CallerResolveResult::NotInHerdr
        }
        Err(error) => return Err(error),
    };
    Ok(Resolution {
        result,
        source: Source::Daemon,
    })
}

fn is_herdr_unavailable(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<RpcClientError>())
        .any(|rpc| rpc.code == HERDR_UNAVAILABLE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::cell::Cell;

    fn env<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            values
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    const FULL: [(&str, &str); 3] = [
        (HERDR_SOCKET_PATH, "/h/sessions/work/herdr.sock"),
        (HERDR_PANE_ID, "w1:p2"),
        (HERDR_WORKSPACE_ID, "w1"),
    ];

    fn query(pane: Option<&str>) -> CallerQuery {
        CallerQuery {
            cwd: "/repo".into(),
            pane: pane.map(Into::into),
            claude_session_id: Some("sess".into()),
        }
    }

    #[test]
    fn full_env_resolves_without_the_daemon() {
        let resolution = resolve(&query(None), env(&FULL), |_| {
            panic!("full env must not reach the daemon")
        })
        .unwrap();
        assert_eq!(resolution.source, Source::Env);
        let CallerResolveResult::Resolved { location } = resolution.result else {
            panic!("{resolution:?}");
        };
        assert_eq!(location.session, "work");
        assert_eq!(location.pane_id, "w1:p2");
    }

    #[test]
    fn partial_env_asks_the_daemon_with_the_cwd_and_session_id() {
        let asked = Cell::new(false);
        let resolution = resolve(&query(None), env(&FULL[1..]), |params| {
            asked.set(true);
            assert_eq!(params.cwd, "/repo");
            assert_eq!(params.pane, None);
            assert_eq!(params.claude_session_id.as_deref(), Some("sess"));
            Ok(CallerResolveResult::NotInHerdr)
        })
        .unwrap();
        assert!(asked.get());
        assert_eq!(resolution.source, Source::Daemon);
    }

    #[test]
    fn an_explicit_pane_wins_over_full_env() {
        let resolution = resolve(&query(Some(" s/w9:p9 ")), env(&FULL), |params| {
            assert_eq!(params.pane.as_deref(), Some("s/w9:p9"));
            Ok(CallerResolveResult::NotInHerdr)
        })
        .unwrap();
        assert_eq!(resolution.source, Source::Daemon);
    }

    #[test]
    fn herdr_unavailable_is_not_in_herdr_only_without_a_pane() {
        let unavailable = || -> Result<CallerResolveResult> {
            Err(anyhow!(RpcClientError::new(
                4,
                None,
                "no herdr".into(),
                None
            )))
        };
        let resolution = resolve(&query(None), env(&[]), |_| unavailable()).unwrap();
        assert_eq!(resolution.result, CallerResolveResult::NotInHerdr);
        assert!(resolve(&query(Some("s/w1:p1")), env(&[]), |_| unavailable()).is_err());
        let bad = resolve(&query(None), env(&[]), |_| {
            Err(anyhow!(RpcClientError::new(1, None, "bad".into(), None)))
        });
        assert!(bad.is_err());
    }

    #[test]
    fn an_empty_or_malformed_id_is_no_claim() {
        std::env::set_var("BOARD_CALLER_TEST_EMPTY_ID", "");
        std::env::set_var("BOARD_CALLER_TEST_BAD_ID", "x7");
        std::env::set_var("BOARD_CALLER_TEST_ID", "7");
        assert_eq!(env_id("BOARD_CALLER_TEST_EMPTY_ID"), None);
        assert_eq!(env_id("BOARD_CALLER_TEST_BAD_ID"), None);
        assert_eq!(env_id("BOARD_CALLER_TEST_ID"), Some(7));
    }
}
