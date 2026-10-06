//! Generated names for the tabs and splits of a Linear-bound space.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    pub issue: Option<String>,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub folder: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Tab,
    Split,
}

pub const MAX_CELLS: usize = 40;

const FALLBACK: &str = "shell";

fn clean(part: Option<&str>) -> Option<String> {
    let text = board_core::text::strip_control_and_format(part?);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn joined(head: String, tail: Option<String>) -> String {
    match tail {
        Some(tail) => format!("{head} · {tail}"),
        None => head,
    }
}

/// A tab names its issue's title, a split names its agent, and a pane with
/// no bound issue names where it runs. Never empty.
pub fn derive(target: Target, scope: &Scope) -> String {
    let agent = clean(scope.agent.as_deref());
    let name = match clean(scope.issue.as_deref()) {
        Some(issue) => match target {
            Target::Tab => joined(issue, clean(scope.title.as_deref())),
            Target::Split => joined(issue, agent),
        },
        None => match clean(scope.folder.as_deref()) {
            Some(folder) => joined(folder, agent),
            None => agent.unwrap_or_else(|| FALLBACK.to_string()),
        },
    };
    capped(name)
}

fn capped(name: String) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if name.width() <= MAX_CELLS {
        return name;
    }
    let mut out = String::new();
    let mut cells = 0;
    for c in name.chars() {
        let w = c.width().unwrap_or(0);
        if cells + w > MAX_CELLS - 1 {
            break;
        }
        cells += w;
        out.push(c);
    }
    let mut out = out.trim_end().to_string();
    out.push('…');
    out
}

/// herdr does not record who set a label, so only a blank label, herdr's
/// default, or the label the board itself last wrote may be replaced.
pub fn decide(
    current: Option<&str>,
    default: Option<&str>,
    stored: Option<&str>,
    derived: &str,
) -> bool {
    let Some(current) = current.map(str::trim).filter(|c| !c.is_empty()) else {
        return true;
    };
    if current == derived {
        return false;
    }
    Some(current) == default || Some(current) == stored
}

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use board_core::db::{herdr_session, resolve_worktree, LabelKind};
use board_herdr::{PaneInfo, PaneRenameParams, TabInfo, TabRenameParams};

use crate::linear::SpaceKey;
use crate::state::Daemon;
use crate::sync::locked;

/// Agent starts in panes the daemon does not run raise no event it can
/// subscribe to without naming each pane, so a periodic pass catches them.
const SWEEP_PERIOD: Duration = Duration::from_secs(10);
/// Lets a burst of layout events land before one pass reads the session.
const SETTLE: Duration = Duration::from_millis(250);

#[derive(Default)]
pub struct Naming {
    kicked: Mutex<bool>,
    wake: Condvar,
    labels: Mutex<()>,
}

impl Naming {
    pub fn kick(&self) {
        *locked(&self.kicked) = true;
        self.wake.notify_all();
    }

    fn wait(&self, timeout: Duration) -> bool {
        let guard = locked(&self.kicked);
        let (guard, _) = self
            .wake
            .wait_timeout_while(guard, timeout, |kicked| !*kicked)
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }

    #[cfg(test)]
    pub(crate) fn pending(&self) -> bool {
        *locked(&self.kicked)
    }

    fn take(&self) {
        *locked(&self.kicked) = false;
    }

    /// Held by a pass from reading a session's labels until it has written
    /// them, and by `pane.set_title`: a board's own title then never lands
    /// between a pass's read and its rename.
    pub(crate) fn labels(&self) -> MutexGuard<'_, ()> {
        locked(&self.labels)
    }
}

pub(crate) fn naming_thread(d: Arc<Daemon>) {
    while !d.is_shutdown() {
        if d.naming.wait(SWEEP_PERIOD) {
            std::thread::sleep(SETTLE);
            d.naming.take();
        }
        if d.is_shutdown() {
            break;
        }
        sweep_all(&d);
    }
}

fn known_sockets(d: &Arc<Daemon>) -> BTreeSet<PathBuf> {
    let mut sockets = BTreeSet::new();
    sockets.insert(d.default_herdr_socket());
    if let Some(registry) = &d.session_registry {
        if let Ok(sessions) = registry.list() {
            sockets.extend(
                sessions
                    .into_iter()
                    .filter(|s| s.running && !s.socket_path.is_empty())
                    .map(|s| PathBuf::from(s.socket_path)),
            );
        }
    }
    sockets.extend(locked(&d.watch).level_sockets.iter().cloned());
    sockets
        .into_iter()
        .filter_map(|socket| socket.canonicalize().ok())
        .collect()
}

pub(crate) fn sweep_all(d: &Arc<Daemon>) {
    let mut named = HashSet::new();
    for socket in known_sockets(d) {
        match sweep_socket(d, &socket) {
            Ok(true) => {
                named.insert(socket);
            }
            Ok(false) => {}
            Err(error) => tracing::debug!(error = %error, "naming pass skipped a session"),
        }
    }
    locked(&d.watch).name_sockets = named;
}

/// The repo a worktree belongs to, else the folder: a linked worktree's
/// `.git` file points into `<repo>/.git/worktrees/<name>`.
pub(crate) fn folder_of(cwd: &str) -> Option<String> {
    let cwd = Path::new(cwd);
    let base = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
    for dir in cwd.ancestors() {
        let git = dir.join(".git");
        if git.is_dir() {
            return base(dir);
        }
        if git.is_file() {
            let linked = std::fs::read_to_string(&git).ok().and_then(|text| {
                let gitdir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
                let worktrees = gitdir.parent()?;
                (worktrees.file_name()? == "worktrees")
                    .then(|| worktrees.parent()?.parent().and_then(base))
                    .flatten()
            });
            return linked.or_else(|| base(dir));
        }
    }
    base(cwd)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    pub renames: Vec<Rename>,
    pub forget: Vec<(LabelKind, String)>,
    pub adopt: Vec<(LabelKind, String, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Rename {
    pub kind: LabelKind,
    pub id: String,
    pub label: String,
    pub observed: Option<String>,
}

type Stored = HashMap<(LabelKind, String), String>;

impl Plan {
    fn consider(
        &mut self,
        stored: &Stored,
        kind: LabelKind,
        id: &str,
        current: Option<&str>,
        default: Option<&str>,
        derived: String,
    ) {
        let mine = stored.get(&(kind, id.to_string())).map(String::as_str);
        if decide(current, default, mine, &derived) {
            self.renames.push(Rename {
                kind,
                id: id.to_string(),
                label: derived,
                observed: current.map(str::to_string),
            });
        } else if let Some(mine) = mine {
            let current = current.map(str::trim).unwrap_or_default();
            if current == derived {
                if current != mine {
                    self.adopt.push((kind, id.to_string(), derived));
                }
            } else if current != mine {
                self.forget.push((kind, id.to_string()));
            }
        }
    }
}

/// What a pass over one session's bound spaces would rename. Board panes are
/// never named; a tab holding only boards is left alone.
pub(crate) fn plan(
    spaces: &[String],
    tabs: &[TabInfo],
    panes: &[PaneInfo],
    boards: &HashSet<String>,
    stored: &Stored,
    scope_of: impl Fn(&PaneInfo) -> Scope,
) -> Plan {
    let mut plan = Plan::default();
    for tab in tabs.iter().filter(|t| spaces.contains(&t.workspace_id)) {
        let scopes: Vec<Scope> = panes
            .iter()
            .filter(|p| p.tab_id == tab.tab_id && !boards.contains(&p.pane_id))
            .map(|pane| {
                let scope = scope_of(pane);
                plan.consider(
                    stored,
                    LabelKind::Pane,
                    &pane.pane_id,
                    pane.label.as_deref(),
                    None,
                    derive(Target::Split, &scope),
                );
                scope
            })
            .collect();
        let Some(scope) = scopes
            .iter()
            .find(|s| s.issue.is_some())
            .or_else(|| scopes.first())
        else {
            continue;
        };
        plan.consider(
            stored,
            LabelKind::Tab,
            &tab.tab_id,
            Some(&tab.label),
            Some(&tab.number.to_string()),
            derive(Target::Tab, scope),
        );
    }
    plan
}

/// One pass over the session at `socket`: `Ok(false)` when none of its
/// spaces is bound, so it needs no watching.
pub(crate) fn sweep_socket(d: &Arc<Daemon>, socket: &Path) -> anyhow::Result<bool> {
    let key = socket.to_string_lossy().into_owned();
    let session = herdr_session(Some(&key));
    let spaces: Vec<String> = d
        .store
        .lock()
        .list_space_bindings()?
        .into_iter()
        .filter(|b| b.herdr_session == session)
        .map(|b| b.space)
        .collect();
    if spaces.is_empty() {
        return Ok(false);
    }
    let mut client = crate::herdr_conn::connect_checked(socket)?;
    let _labels = d.naming.labels();
    let tabs = client.tab_list(None)?;
    let panes = client.pane_list(None)?;
    let (stored, boards) = {
        let store = d.store.lock();
        let stored: Stored = store
            .generated_labels(&key)?
            .into_iter()
            .map(|(kind, id, label)| ((kind, id), label))
            .collect();
        let boards: HashSet<String> = store
            .list_board_panes(&key)?
            .into_iter()
            .map(|row| row.pane_id)
            .collect();
        (stored, boards)
    };
    // Outside the store lock: the scope lookups read the filesystem, and each
    // binding read takes the lock on its own.
    let memo = PassMemo::default();
    let plan = plan(&spaces, &tabs, &panes, &boards, &stored, |pane| {
        scope_of(d, &memo, &session, pane)
    });
    let now = current_labels(&mut client, &plan.renames)?;
    for Rename {
        kind,
        id,
        label,
        observed,
    } in plan.renames
    {
        if now.get(&(kind, id.clone())) != Some(&observed) {
            continue;
        }
        let renamed = match kind {
            LabelKind::Tab => client.tab_rename(&TabRenameParams {
                tab_id: id.clone(),
                label: label.clone(),
            }),
            LabelKind::Pane => client
                .pane_rename(&PaneRenameParams {
                    pane_id: id.clone(),
                    label: label.clone(),
                })
                .map(|_| ()),
        };
        match renamed {
            Ok(()) => d
                .store
                .lock()
                .set_generated_label(&key, kind, &id, &label)?,
            Err(error) => tracing::debug!(error = %error, "naming rename failed"),
        }
    }
    let store = d.store.lock();
    for (kind, id, label) in plan.adopt {
        store.set_generated_label(&key, kind, &id, &label)?;
    }
    for (kind, id) in plan.forget {
        store.forget_generated_label(&key, kind, &id)?;
    }
    // `stored` was read before this pass's writes, which only touch live
    // targets, and the labels lock keeps any other writer out meanwhile.
    let live_tabs: HashSet<&str> = tabs.iter().map(|t| t.tab_id.as_str()).collect();
    let live_panes: HashSet<&str> = panes.iter().map(|p| p.pane_id.as_str()).collect();
    for (kind, id) in stored.keys() {
        let live = match kind {
            LabelKind::Tab => &live_tabs,
            LabelKind::Pane => &live_panes,
        };
        if !live.contains(id.as_str()) {
            store.forget_generated_label(&key, *kind, id)?;
        }
    }
    Ok(true)
}

/// The planning lookups take long enough for a hand rename to land, so each
/// target's label is read again right before the writes.
fn current_labels(
    client: &mut board_herdr::HerdrClient,
    renames: &[Rename],
) -> anyhow::Result<HashMap<(LabelKind, String), Option<String>>> {
    let mut now = HashMap::new();
    if renames.iter().any(|r| r.kind == LabelKind::Tab) {
        for tab in client.tab_list(None)? {
            now.insert((LabelKind::Tab, tab.tab_id), Some(tab.label));
        }
    }
    if renames.iter().any(|r| r.kind == LabelKind::Pane) {
        for pane in client.pane_list(None)? {
            now.insert((LabelKind::Pane, pane.pane_id), pane.label);
        }
    }
    Ok(now)
}

/// One pass's lookups, so panes sharing a directory or an issue share them.
#[derive(Default)]
struct PassMemo {
    issues: RefCell<HashMap<String, Option<String>>>,
    titles: RefCell<HashMap<(String, String), Option<String>>>,
    folders: RefCell<HashMap<String, Option<String>>>,
}

fn memoized<K: Eq + std::hash::Hash, V: Clone>(
    cell: &RefCell<HashMap<K, V>>,
    key: K,
    make: impl FnOnce() -> V,
) -> V {
    if let Some(value) = cell.borrow().get(&key) {
        return value.clone();
    }
    let value = make();
    cell.borrow_mut().insert(key, value.clone());
    value
}

fn scope_of(d: &Arc<Daemon>, memo: &PassMemo, session: &str, pane: &PaneInfo) -> Scope {
    let cwd = pane.working_dir();
    let issue = cwd.and_then(|cwd| {
        memoized(&memo.issues, cwd.to_string(), || {
            resolve_worktree(cwd)
                .ok()
                .and_then(|path| d.store.lock().worktree_binding(&path).ok().flatten())
                .map(|binding| binding.issue)
                .filter(|issue| !issue.is_empty())
        })
    });
    let title = issue.as_deref().and_then(|issue| {
        memoized(
            &memo.titles,
            (pane.workspace_id.clone(), issue.to_string()),
            || {
                let key = SpaceKey {
                    session: session.to_string(),
                    space: pane.workspace_id.clone(),
                };
                d.linear.cache.find_good(&key, |read| {
                    read.issues
                        .iter()
                        .find(|node| node["identifier"].as_str() == Some(issue))
                        .and_then(|node| node["title"].as_str())
                        .map(str::to_string)
                })
            },
        )
    });
    // `derive` reads the folder only when the issue cleans to nothing.
    let folder = if clean(issue.as_deref()).is_none() {
        cwd.and_then(|cwd| memoized(&memo.folders, cwd.to_string(), || folder_of(cwd)))
    } else {
        None
    };
    Scope {
        issue,
        title,
        agent: pane.agent.clone(),
        folder,
    }
}

/// `pane.set_title` hands the pane's label to its client for good.
pub(crate) fn titled_by_client(d: &Arc<Daemon>, socket: &Path, pane_id: &str) {
    let key = socket.to_string_lossy();
    if let Err(error) = d
        .store
        .lock()
        .forget_generated_label(&key, LabelKind::Pane, pane_id)
    {
        tracing::debug!(error = %error, "could not forget a generated pane label");
    }
}

#[cfg(test)]
mod tests;
