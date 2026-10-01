//! The native Linear read path (R4, R5, R8): local state from SQLite, Linear
//! through boardd's read-only client and its shared per-space cache (KTD11),
//! and the board view from `board_core::engine::grouping`. The result shapes
//! are the script era's, plus additive fields.
//!
//! A *space* is a herdr workspace id. The grouping config names spaces by the
//! work plugin's space name, which is the herdr workspace label, so the label
//! is what selects a space override and the global `space` level.

use std::collections::BTreeSet;

use board_core::db::{GroupingConfig, SpaceBinding, WorktreeBinding};
use board_core::engine::grouping::{self, GroupingIssue, ViewPreferences, WorkflowState};
use board_herdr::SessionSnapshot;

use crate::linear::{fetch, names_project, CacheRead, FetchPlan, SpaceKey, SpaceRead};

use super::*;

fn session_of(socket: Option<&str>) -> String {
    board_core::db::herdr_session(socket)
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn herdr_snapshot(origin: Option<&str>) -> Option<SessionSnapshot> {
    crate::herdr_conn::connect_checked(Path::new(origin?))
        .and_then(|mut client| client.session_snapshot())
        .ok()
}

struct Local {
    binding: Option<SpaceBinding>,
    config: Option<GroupingConfig>,
    worktrees: Vec<WorktreeBinding>,
    no_bindings: bool,
}

/// One short store lock, never held across a herdr or Linear call.
fn read_local(d: &Arc<Daemon>, session: &str, space: &str) -> Result<Local> {
    let db = d.store.lock();
    let worktrees = db.list_worktree_bindings()?;
    let no_bindings = worktrees.is_empty() && db.list_space_bindings()?.is_empty();
    Ok(Local {
        binding: db.space_binding(session, space)?,
        config: db.grouping_config()?,
        worktrees,
        no_bindings,
    })
}

fn space_label(
    herdr: Option<&SessionSnapshot>,
    binding: Option<&SpaceBinding>,
    space: &str,
) -> String {
    herdr
        .and_then(|s| s.workspaces.iter().find(|w| w.workspace_id == space))
        .map(|w| w.label.clone())
        .filter(|label| !label.is_empty())
        .or_else(|| binding.and_then(|b| b.display_name.clone()))
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| space.to_string())
}

fn mapping_in_force<'a>(
    config: &'a GroupingConfig,
    label: &str,
) -> (&'static str, &'a board_core::db::GroupingMapping) {
    match config.spaces.iter().find(|s| s.space == label) {
        Some(entry) => ("space", &entry.mapping),
        None => ("global", &config.global),
    }
}

fn plan_for(binding: &SpaceBinding, config: Option<&GroupingConfig>, label: &str) -> FetchPlan {
    let filter = config.map(|c| grouping::issue_filter(mapping_in_force(c, label).1));
    let view_id = match filter {
        Some(_) => None,
        None => binding.view.as_ref().and_then(|view| text(&view["id"])),
    };
    FetchPlan {
        project_id: binding.project_id.clone(),
        view_id,
        filter,
        label: label.to_string(),
    }
}

fn read_space(d: &Arc<Daemon>, key: &SpaceKey, plan: &FetchPlan) -> CacheRead<SpaceRead> {
    d.linear.cache.read(key, plan, d.linear.ttl(), |plan| {
        let client = d.linear.client().map_err(|e| e.to_string())?;
        fetch(&client, plan).map_err(|e| e.to_string())
    })
}

fn default_mapping() -> LinearMapping {
    LinearMapping {
        status: "ok".into(),
        source: "default".into(),
        space: "project".into(),
        tab: "work".into(),
        pane: "session".into(),
    }
}

/// The plugin's pane level became the board's columns (R7), so `pane`
/// reports the column level.
fn mapping_doc(config: Option<&GroupingConfig>, label: &str) -> LinearMapping {
    let Some(config) = config else {
        return default_mapping();
    };
    let (source, mapping) = mapping_in_force(config, label);
    let level = |name: &str| mapping.levels.get(name).cloned().unwrap_or_default();
    LinearMapping {
        status: "ok".into(),
        source: source.into(),
        space: level("space"),
        tab: level("tab"),
        pane: level("column"),
    }
}

pub(in crate::ops) fn snapshot(d: &Arc<Daemon>, p: LinearSnapshotParams) -> Result<LinearSnapshot> {
    let space = checked_workspace_id(&p)?.to_string();
    // The raw path, as `linear.activity.record` reads its claim: a cache key
    // built from a rewritten path would never meet the reporter's.
    let session = session_of(p.origin_socket.as_deref());
    let origin = normalized_origin(p.origin_socket.as_deref());
    let herdr = herdr_snapshot(origin.as_deref());
    let local = read_local(d, &session, &space)?;
    let label = space_label(herdr.as_ref(), local.binding.as_ref(), &space);

    let mut doc = LinearSnapshot {
        schema: 1,
        workspace: LinearWorkspace {
            id: space.clone(),
            label: label.clone(),
            live: herdr
                .as_ref()
                .map(|s| s.workspaces.iter().any(|w| w.workspace_id == space)),
        },
        mapping: mapping_doc(local.config.as_ref(), &label),
        view: LinearView {
            status: "none".into(),
            ..LinearView::default()
        },
        linear: LinearSource {
            status: "unknown".into(),
            ..LinearSource::default()
        },
        herdr: LinearHerdr {
            status: if herdr.is_some() { "ok" } else { "unavailable" }.into(),
            version: herdr
                .as_ref()
                .map(|s| s.version.clone())
                .filter(|v| !v.is_empty()),
        },
        ..LinearSnapshot::default()
    };

    let Some(binding) = local.binding else {
        doc.record = LinearRecord {
            status: "missing".into(),
            state: Some("unbound".into()),
            project_id: None,
        };
        if local.no_bindings {
            if let Some(dir) = d.linear.store_dir().filter(|dir| dir.is_dir()) {
                doc.linear.status = "not_imported".into();
                doc.linear.message = Some(format!(
                    "the board holds no bindings and the work plugin's store at {} has not \
                     been imported; run `board import work-store`",
                    dir.display()
                ));
            }
        }
        return Ok(doc);
    };

    doc.record = LinearRecord {
        status: "ok".into(),
        state: Some("bound".into()),
        project_id: Some(binding.project_id.clone()),
    };
    doc.project.id = Some(binding.project_id.clone());
    let plan = plan_for(&binding, local.config.as_ref(), &label);
    if plan.view_id.is_some() {
        doc.view.id = plan.view_id.clone();
        doc.view.name = binding.view.as_ref().and_then(|view| text(&view["name"]));
        doc.view.status = "unreadable".into();
    }

    let key = SpaceKey { session, space };
    if p.force {
        d.linear.cache.invalidate(&key);
    }
    let read = read_space(d, &key, &plan);
    fill_from_linear(&mut doc, &read, local.config.as_ref(), &plan);
    attach_bindings(&mut doc, &local.worktrees, herdr.as_ref(), &key.space);
    let ids = doc.pane_ids();
    doc.pane_status = pane_status(herdr, &ids);
    Ok(doc)
}

fn fill_from_linear(
    doc: &mut LinearSnapshot,
    read: &CacheRead<SpaceRead>,
    config: Option<&GroupingConfig>,
    plan: &FetchPlan,
) {
    let Some((data, age)) = &read.good else {
        doc.linear.status = "unavailable".into();
        doc.linear.message = read.error.clone();
        return;
    };
    let stale = read.error.is_some();
    if stale {
        doc.linear.status = "unavailable".into();
        doc.linear.cache_age_seconds = Some(age.as_secs().min(i64::MAX as u64) as i64);
        doc.linear.message = read.error.clone();
    } else {
        doc.linear.status = if data.partial { "truncated" } else { "ok" }.into();
        doc.linear.truncated = data.partial;
    }

    let prefs = &data
        .view
        .as_ref()
        .map(|v| v["viewPreferencesValues"].clone());
    if plan.view_id.is_some() {
        doc.view.status = data.view_status.into();
        if let Some(prefs) = prefs
            .as_ref()
            .filter(|_| matches!(data.view_status, "ok" | "archived" | "unsupported_grouping"))
        {
            let full = data.view_status != "unsupported_grouping";
            let list = |v: &Value| -> Vec<String> {
                v.as_array()
                    .map(|a| a.iter().filter_map(text).collect())
                    .unwrap_or_default()
            };
            doc.view.layout = Some(LinearViewLayout {
                grouping: prefs["issueGrouping"].as_str().unwrap_or("").to_string(),
                column_order: if full {
                    list(&prefs["columnOrderBoard"])
                } else {
                    vec![]
                },
                hidden: if full {
                    list(&prefs["hiddenColumns"])
                } else {
                    vec![]
                },
            });
        }
    }

    let first_team = data
        .project
        .as_ref()
        .map(|p| p["teams"]["nodes"][0].clone())
        .unwrap_or(Value::Null);
    if let Some(project) = &data.project {
        doc.project.name = text(&project["name"]);
        doc.project.url = text(&project["url"]);
    }
    doc.project.team_key = text(&first_team["key"]).or_else(|| {
        data.issues
            .iter()
            .find_map(|issue| text(&issue["team"]["key"]))
    });
    let states: Vec<WorkflowState> =
        serde_json::from_value(first_team["states"]["nodes"].clone()).unwrap_or_default();
    let view_prefs: Option<ViewPreferences> = prefs
        .as_ref()
        .filter(|_| data.view_status == "ok")
        .and_then(|p| serde_json::from_value(p.clone()).ok());

    let issues: Vec<GroupingIssue> = data
        .issues
        .iter()
        .filter_map(|node| serde_json::from_value::<GroupingIssue>(node.clone()).ok())
        .filter(|issue| !issue.identifier.is_empty())
        .collect();
    let tabs = grouping::group_board(
        &issues,
        grouping::choose(config, &plan.label, view_prefs.as_ref(), &states),
    );
    let listed: BTreeSet<&str> = tabs
        .iter()
        .flat_map(|tab| tab.groups.iter())
        .flat_map(|group| group.issues.iter().map(String::as_str))
        .collect();
    for node in &data.issues {
        let Some(identifier) = text(&node["identifier"]) else {
            continue;
        };
        if listed.contains(identifier.as_str()) {
            doc.issues
                .insert(identifier.clone(), linear_issue(node, identifier, stale));
        }
    }
    grouping::fill_snapshot(doc, tabs);
}

fn issue_state(value: &Value) -> LinearIssueState {
    LinearIssueState {
        id: text(&value["id"]),
        name: text(&value["name"]),
        kind: text(&value["type"]),
    }
}

fn int(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_f64().map(|f| f as i64))
}

fn names(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|nodes| nodes.iter().filter_map(|n| text(&n["name"])).collect())
        .unwrap_or_default()
}

fn linear_issue(node: &Value, identifier: String, stale: bool) -> LinearIssue {
    LinearIssue {
        id: text(&node["id"]),
        identifier,
        title: node["title"].as_str().unwrap_or("").to_string(),
        url: text(&node["url"]),
        state: issue_state(&node["state"]),
        assignee: node["assignee"].is_object().then(|| LinearAssignee {
            id: text(&node["assignee"]["id"]),
            name: text(&node["assignee"]["name"]),
        }),
        priority: int(&node["priority"]),
        labels: names(&node["labels"]["nodes"]),
        stale,
        bindings: vec![],
    }
}

/// Worktree bindings of listed issues, with their herdr tab and panes; tabs
/// in this workspace that no binding claims are `unmapped`.
fn attach_bindings(
    doc: &mut LinearSnapshot,
    worktrees: &[WorktreeBinding],
    herdr: Option<&SessionSnapshot>,
    space: &str,
) {
    let tab_label = |tab: &str| {
        herdr
            .and_then(|s| s.tabs.iter().find(|t| t.tab_id == tab))
            .map(|t| t.label.clone())
            .filter(|label| !label.is_empty())
    };
    let panes_in = |tab: &str| -> Vec<String> {
        herdr
            .map(|s| {
                s.panes
                    .iter()
                    .filter(|p| p.tab_id == tab)
                    .map(|p| p.pane_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut claimed = BTreeSet::new();
    let mut unlisted_claims = BTreeMap::new();
    let mut sorted: Vec<&WorktreeBinding> = worktrees.iter().collect();
    sorted.sort_by(|a, b| a.worktree_path.cmp(&b.worktree_path));
    for wt in sorted {
        let tab = wt.tab.as_deref().filter(|t| !t.is_empty());
        match doc.issues.get_mut(&wt.issue) {
            Some(issue) => {
                issue.bindings.push(LinearBinding {
                    worktree_path: wt.worktree_path.clone(),
                    state: wt.state.as_str().to_string(),
                    tab: tab.map(|t| LinearTabRef {
                        id: t.to_string(),
                        label: tab_label(t),
                    }),
                    panes: tab.map(panes_in).unwrap_or_default(),
                });
                if let Some(t) = tab {
                    claimed.insert(t.to_string());
                }
            }
            None => {
                if let Some(t) = tab {
                    unlisted_claims
                        .entry(t.to_string())
                        .or_insert_with(|| wt.state.as_str().to_string());
                }
            }
        }
    }
    let Some(herdr) = herdr else {
        return;
    };
    doc.unmapped = herdr
        .tabs
        .iter()
        .filter(|t| t.workspace_id == space && !t.tab_id.is_empty())
        .filter(|t| !claimed.contains(&t.tab_id))
        .map(|t| LinearUnmappedTab {
            tab_id: t.tab_id.clone(),
            label: Some(t.label.clone()).filter(|l| !l.is_empty()),
            reason: unlisted_claims
                .get(&t.tab_id)
                .cloned()
                .unwrap_or_else(|| "no_binding".into()),
            panes: panes_in(&t.tab_id),
        })
        .collect();
}

fn pane_status(herdr: Option<SessionSnapshot>, ids: &[String]) -> BTreeMap<String, String> {
    let live = herdr.map(crate::herdr_snapshot::snapshot_pane_statuses);
    ids.iter()
        .map(|id| {
            let status = live
                .as_ref()
                .and_then(|statuses| statuses.get(id).copied())
                .map(agent_status_name)
                .unwrap_or("unknown");
            (id.clone(), status.to_string())
        })
        .collect()
}

/// The identifier of `issue`, a key or a UUID, when the session's cached read
/// of `space` lists it, which makes the card known for a mark, note or
/// show-request. The board matches local state by identifier only.
pub(in crate::ops) fn resolve_issue(
    d: &Arc<Daemon>,
    socket: Option<&str>,
    space: &str,
    issue: &str,
) -> Option<String> {
    let key = SpaceKey {
        session: session_of(socket),
        space: space.to_string(),
    };
    d.linear.cache.find_good(&key, |read| {
        read.issues
            .iter()
            .find(|node| {
                node["identifier"].as_str() == Some(issue) || node["id"].as_str() == Some(issue)
            })
            .and_then(|node| node["identifier"].as_str())
            .map(str::to_string)
    })
}

/// The column `issue` sits in on the board as this session last read it.
/// `None` on a cold or failed cache: the status line never waits on Linear
/// or herdr.
pub(in crate::ops) fn cached_column(
    d: &Arc<Daemon>,
    socket: Option<&str>,
    space: &str,
    issue: &str,
) -> Result<Option<String>> {
    let key = SpaceKey {
        session: session_of(socket),
        space: space.to_string(),
    };
    let Some((data, plan)) = d.linear.cache.peek_good(&key) else {
        return Ok(None);
    };
    let config = d.store.lock().grouping_config()?;
    let mut doc = LinearSnapshot::default();
    let read = CacheRead {
        good: Some((data, std::time::Duration::ZERO)),
        error: None,
    };
    fill_from_linear(&mut doc, &read, config.as_ref(), &plan);
    Ok(doc
        .tabs
        .iter()
        .flat_map(|tab| tab.groups.iter())
        .find(|group| {
            group.issues.iter().any(|id| id == issue)
                || group
                    .lanes
                    .iter()
                    .any(|lane| lane.issues.iter().any(|id| id == issue))
        })
        .map(|group| group.label.clone()))
}

/// After a reported Linear write: when a reader has read the space, mark its
/// read out of date and schedule one debounced refetch that announces the
/// space with the snapshot flag once it lands. The caller announces the
/// local write itself.
pub(in crate::ops) fn schedule_refetch(d: &Arc<Daemon>, session: String, space: String) {
    let key = SpaceKey { session, space };
    if d.linear.cache.invalidate(&key).is_none() {
        return;
    }
    let (worker, announcer) = (d.clone(), d.clone());
    let (work_key, space) = (key.clone(), key.space.clone());
    d.linear.refetch.schedule(
        key,
        d.linear.debounce(),
        move || refetch(&worker, &work_key),
        move || {
            if !announcer.is_shutdown() {
                announcer.emit(Event::LocalStateChanged {
                    space: Some(space),
                    snapshot: true,
                });
            }
        },
    );
}

/// The plan is rebuilt from local state, because the write being reported may
/// have linked or unbound the space; the label is the last reader's.
fn refetch(d: &Arc<Daemon>, key: &SpaceKey) {
    if d.is_shutdown() {
        return;
    }
    let Some(previous) = d.linear.cache.plan(key) else {
        return;
    };
    let Ok(local) = read_local(d, &key.session, &key.space) else {
        return;
    };
    match local.binding {
        Some(binding) => {
            let plan = plan_for(&binding, local.config.as_ref(), &previous.label);
            read_space(d, key, &plan);
        }
        None => d.linear.cache.remove(key),
    }
}

pub(in crate::ops) fn list(d: &Arc<Daemon>, p: LinearListParams) -> Result<LinearListResult> {
    let id = checked_list_id(&p)?.map(str::to_string);
    Ok(match p.kind {
        LinearListKind::Spaces => LinearListResult::Spaces(spaces(d, p.origin_socket.as_deref())?),
        LinearListKind::Projects => LinearListResult::Projects(projects(d)),
        LinearListKind::Views => LinearListResult::Views(views(d, &id.unwrap_or_default())),
    })
}

fn envelope<R>(
    status: LinearListStatus,
    message: Option<String>,
    rows: Vec<R>,
) -> LinearListEnvelope<R> {
    LinearListEnvelope {
        status,
        message,
        rows,
    }
}

fn spaces(d: &Arc<Daemon>, raw_origin: Option<&str>) -> Result<LinearSpacesList> {
    let session = session_of(raw_origin);
    let Some(socket) = normalized_origin(raw_origin) else {
        return Ok(envelope(
            LinearListStatus::Unavailable,
            Some("no herdr session to list: the request named no origin socket".into()),
            vec![],
        ));
    };
    let live = match crate::herdr_conn::connect_checked(Path::new(&socket))
        .and_then(|mut client| client.workspace_list())
    {
        Ok(live) => live,
        Err(_) => {
            return Ok(envelope(
                LinearListStatus::Unavailable,
                Some("herdr did not list its spaces".into()),
                vec![],
            ))
        }
    };
    let bindings: BTreeMap<String, SpaceBinding> = d
        .store
        .lock()
        .list_space_bindings()?
        .into_iter()
        .filter(|b| b.herdr_session == session)
        .map(|b| (b.space.clone(), b))
        .collect();
    let row = |id: &str, label: &str, live: bool| {
        let binding = bindings.get(id);
        LinearSpaceRow {
            id: id.to_string(),
            label: if label.is_empty() { id } else { label }.to_string(),
            live: Some(live),
            state: if binding.is_some() {
                "bound"
            } else {
                "unbound"
            }
            .into(),
            project_id: binding.map(|b| b.project_id.clone()),
            project_name: binding.and_then(|b| b.display_name.clone()),
        }
    };
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for w in &live {
        if !w.workspace_id.is_empty() && seen.insert(w.workspace_id.as_str()) {
            rows.push(row(&w.workspace_id, &w.label, true));
        }
    }
    for id in bindings.keys() {
        if !seen.contains(id.as_str()) {
            rows.push(row(id, "", false));
        }
    }
    Ok(envelope(LinearListStatus::Ok, None, rows))
}

fn paged_status(partial: bool, what: &str) -> (LinearListStatus, Option<String>) {
    if partial {
        (
            LinearListStatus::Partial,
            Some(format!(
                "listed the first pages of {what} only; one past them can be chosen by its id"
            )),
        )
    } else {
        (LinearListStatus::Ok, None)
    }
}

fn projects(d: &Arc<Daemon>) -> LinearProjectsList {
    match d.linear.client().and_then(|client| client.projects()) {
        Ok(page) => {
            let (status, message) = paged_status(page.partial, "projects");
            let rows = page
                .nodes
                .iter()
                .filter_map(|node| {
                    Some(LinearProjectRow {
                        id: text(&node["id"])?,
                        name: node["name"].as_str().unwrap_or("").to_string(),
                        team_key: text(&node["teams"]["nodes"][0]["key"]),
                    })
                })
                .collect();
            envelope(status, message, rows)
        }
        Err(error) => envelope(
            LinearListStatus::Unavailable,
            Some(error.to_string()),
            vec![],
        ),
    }
}

fn views(d: &Arc<Daemon>, project_id: &str) -> LinearViewsList {
    match d.linear.client().and_then(|client| client.views()) {
        Ok(page) => {
            let (status, message) = paged_status(page.partial, "views");
            let rows = page
                .nodes
                .iter()
                .filter(|node| node["archivedAt"].is_null())
                .filter(|node| names_project(&node["filterData"], project_id))
                .filter_map(|node| {
                    Some(LinearViewRow {
                        id: text(&node["id"])?,
                        name: node["name"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect();
            envelope(status, message, rows)
        }
        Err(error) => envelope(
            LinearListStatus::Unavailable,
            Some(error.to_string()),
            vec![],
        ),
    }
}

/// One GraphQL call. Every failure, "no such issue" included, is a document
/// with `status: "unavailable"`, as the plugin's `docs/issue.md` defines it.
pub(in crate::ops) fn issue(d: &Arc<Daemon>, p: LinearIssueParams) -> Result<LinearIssueDocument> {
    let id = checked_issue_id(&p)?;
    Ok(
        match d.linear.client().and_then(|client| client.issue(id)) {
            Ok(node) => {
                let (detail, truncated) = issue_detail(&node);
                LinearIssueDocument {
                    schema: 1,
                    status: if truncated.is_empty() {
                        "ok"
                    } else {
                        "partial"
                    }
                    .into(),
                    message: None,
                    truncated,
                    issue: Some(detail),
                }
            }
            Err(error) => LinearIssueDocument {
                schema: 1,
                status: "unavailable".into(),
                message: Some(error.to_string()),
                truncated: vec![],
                issue: None,
            },
        },
    )
}

fn named(value: &Value) -> Option<LinearNamed> {
    value.is_object().then(|| LinearNamed {
        id: text(&value["id"]),
        name: text(&value["name"]),
    })
}

fn linked(value: &Value) -> LinearLinkedIssue {
    LinearLinkedIssue {
        id: text(&value["id"]),
        identifier: value["identifier"].as_str().unwrap_or("").to_string(),
        title: value["title"].as_str().unwrap_or("").to_string(),
        state: issue_state(&value["state"]),
    }
}

fn nodes(value: &Value) -> &[Value] {
    value["nodes"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn issue_detail(node: &Value) -> (LinearIssueDetail, Vec<String>) {
    let mut truncated: Vec<String> = Vec::new();
    for (name, connection) in [
        ("children", "children"),
        ("relations", "relations"),
        ("relations", "inverseRelations"),
        ("comments", "comments"),
        ("history", "history"),
    ] {
        if node[connection]["pageInfo"]["hasNextPage"].as_bool() == Some(true)
            && !truncated.iter().any(|t| t == name)
        {
            truncated.push(name.to_string());
        }
    }
    let relations = nodes(&node["relations"])
        .iter()
        .map(|r| (r, "outward", &r["relatedIssue"]))
        .chain(
            nodes(&node["inverseRelations"])
                .iter()
                .map(|r| (r, "inward", &r["issue"])),
        )
        .map(|(r, direction, other)| LinearRelation {
            r#type: r["type"].as_str().unwrap_or("").to_string(),
            direction: direction.to_string(),
            issue: linked(other),
        })
        .collect();
    let history = nodes(&node["history"])
        .iter()
        .map(|h| LinearHistoryEvent {
            id: text(&h["id"]),
            created_at: text(&h["createdAt"]),
            actor: text(&h["actor"]["name"]),
            from_state: text(&h["fromState"]["name"]),
            to_state: text(&h["toState"]["name"]),
            from_assignee: text(&h["fromAssignee"]["name"]),
            to_assignee: text(&h["toAssignee"]["name"]),
            from_priority: int(&h["fromPriority"]),
            to_priority: int(&h["toPriority"]),
            added_labels: names(&h["addedLabels"]),
            removed_labels: names(&h["removedLabels"]),
        })
        .filter(|h| {
            h.from_state.is_some()
                || h.to_state.is_some()
                || h.from_assignee.is_some()
                || h.to_assignee.is_some()
                || h.from_priority != h.to_priority
                || !h.added_labels.is_empty()
                || !h.removed_labels.is_empty()
        })
        .collect();
    let detail = LinearIssueDetail {
        id: text(&node["id"]),
        identifier: node["identifier"].as_str().unwrap_or("").to_string(),
        title: node["title"].as_str().unwrap_or("").to_string(),
        url: text(&node["url"]),
        description: text(&node["description"]),
        updated_at: text(&node["updatedAt"]),
        due_date: text(&node["dueDate"]),
        estimate: node["estimate"].as_f64(),
        priority: int(&node["priority"]),
        state: issue_state(&node["state"]),
        assignee: node["assignee"].is_object().then(|| LinearAssignee {
            id: text(&node["assignee"]["id"]),
            name: text(&node["assignee"]["name"]),
        }),
        labels: names(&node["labels"]["nodes"]),
        project: named(&node["project"]),
        milestone: named(&node["projectMilestone"]),
        cycle: node["cycle"].is_object().then(|| LinearCycle {
            id: text(&node["cycle"]["id"]),
            number: int(&node["cycle"]["number"]),
            name: text(&node["cycle"]["name"]),
        }),
        parent: node["parent"].is_object().then(|| linked(&node["parent"])),
        children: nodes(&node["children"]).iter().map(linked).collect(),
        relations,
        comments: nodes(&node["comments"])
            .iter()
            .map(|c| LinearComment {
                id: text(&c["id"]),
                body: c["body"].as_str().unwrap_or("").to_string(),
                created_at: text(&c["createdAt"]),
                author: text(&c["user"]["name"]),
                parent_id: text(&c["parent"]["id"]),
            })
            .collect(),
        history,
    };
    (detail, truncated)
}
