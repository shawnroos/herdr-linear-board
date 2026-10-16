use std::path::{Path, PathBuf};

use crate::protocol::{CallerCandidate, CallerLocation, CallerResolveResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerPane {
    pub session: String,
    pub socket: String,
    pub workspace_id: String,
    pub workspace_label: Option<String>,
    pub tab_id: String,
    pub pane_id: String,
    pub agent: Option<String>,
    pub cwd: Option<String>,
    pub foreground_cwd: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneFilter {
    pub session: Option<String>,
    pub pane_id: String,
}

/// Pane ids (`w2:p2`) never hold a `/`, so the last `/` separates the session.
pub fn parse_pane_filter(value: &str) -> PaneFilter {
    let value = value.trim();
    let (session, pane_id) = value.rsplit_once('/').unwrap_or(("", value));
    PaneFilter {
        session: (!session.is_empty()).then(|| session.to_string()),
        pane_id: pane_id.to_string(),
    }
}

/// Resolve symlinks when the path exists; otherwise normalize it lexically,
/// which still drops a trailing slash and `.` segments.
pub fn canonical_dir(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| Path::new(path).components().collect())
}

/// Decide where a caller sits. With `pane`, only that pane's identity counts
/// and a single hit resolves. Without it, Claude panes whose `cwd` or
/// `foreground_cwd` is the caller's folder are offered as candidates and
/// never resolve, so a session outside herdr cannot take over a pane.
pub fn match_caller(panes: &[CallerPane], cwd: &str, pane: Option<&str>) -> CallerResolveResult {
    let mut hits: Vec<&CallerPane> = match pane {
        Some(value) => {
            let filter = parse_pane_filter(value);
            panes
                .iter()
                .filter(|p| {
                    p.pane_id == filter.pane_id
                        && filter.session.as_ref().is_none_or(|s| *s == p.session)
                })
                .collect()
        }
        None => folder_matches(panes, cwd),
    };
    hits.sort_by(|a, b| (&a.session, &a.pane_id).cmp(&(&b.session, &b.pane_id)));

    match (pane.is_some(), hits.as_slice()) {
        (_, []) => CallerResolveResult::NotInHerdr,
        (true, [only]) => CallerResolveResult::Resolved {
            location: location(only),
        },
        _ => CallerResolveResult::Unconfirmed {
            candidates: hits.into_iter().map(candidate).collect(),
        },
    }
}

fn folder_matches<'a>(panes: &'a [CallerPane], cwd: &str) -> Vec<&'a CallerPane> {
    if cwd.trim().is_empty() {
        return Vec::new();
    }
    let caller = canonical_dir(cwd);
    let same = |path: &Option<String>| {
        path.as_deref()
            .filter(|p| !p.trim().is_empty())
            .is_some_and(|p| canonical_dir(p) == caller)
    };
    panes
        .iter()
        .filter(|p| p.agent.as_deref() == Some("claude"))
        .filter(|p| same(&p.cwd) || same(&p.foreground_cwd))
        .collect()
}

fn location(pane: &CallerPane) -> CallerLocation {
    CallerLocation {
        session: pane.session.clone(),
        socket: pane.socket.clone(),
        workspace_id: pane.workspace_id.clone(),
        tab_id: pane.tab_id.clone(),
        pane_id: pane.pane_id.clone(),
    }
}

fn candidate(pane: &CallerPane) -> CallerCandidate {
    CallerCandidate {
        pane: format!("{}/{}", pane.session, pane.pane_id),
        session: pane.session.clone(),
        socket: pane.socket.clone(),
        workspace_id: pane.workspace_id.clone(),
        workspace_label: pane.workspace_label.clone(),
        tab_id: pane.tab_id.clone(),
        pane_id: pane.pane_id.clone(),
        title: pane.title.clone(),
    }
}
