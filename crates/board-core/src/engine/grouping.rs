//! The grouping engine: pure, no I/O. Issues plus the grouping in force
//! become the board view: tabs in the strip, columns, and swimlane rows across
//! the columns (R6-R8). Ports the display half of the work plugin's
//! `lib/board-plan.sh` (group values, "No <field>" groups, sort order) and the
//! no-config grouping of its snapshot script; pane placement is not ported.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};

use crate::db::{GroupingConfig, GroupingMapping, LINEAR_STATE_TYPES};
use crate::protocol::{LinearGroup, LinearLane, LinearSnapshot, LinearTab};

fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// One issue as Linear's GraphQL returns it. Every field another process may
/// null reads as empty (`docs/solutions/integration-issues/serde-default-rejects-explicit-null.md`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupingIssue {
    #[serde(default, deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub identifier: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub title: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub updated_at: String,
    #[serde(default)]
    pub state: Option<IssueState>,
    #[serde(default)]
    pub team: Option<IssueTeam>,
    #[serde(default)]
    pub project: Option<IssueNamed>,
    #[serde(default)]
    pub project_milestone: Option<IssueNamed>,
    #[serde(default)]
    pub cycle: Option<IssueCycle>,
    #[serde(default)]
    pub assignee: Option<IssueNamed>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub parent: Option<IssueParent>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub labels: IssueLabels,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueState {
    #[serde(default, deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
    #[serde(default, rename = "type", deserialize_with = "null_as_default")]
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueTeam {
    #[serde(default, deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub key: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueNamed {
    #[serde(default, deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueCycle {
    #[serde(default, deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(default)]
    pub number: Option<i64>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueParent {
    #[serde(default, deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub identifier: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueLabels {
    #[serde(default, deserialize_with = "null_as_default")]
    pub nodes: Vec<IssueLabel>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueLabel {
    #[serde(default, deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
    #[serde(default)]
    pub parent: Option<IssueNamed>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowState {
    #[serde(default, deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
    #[serde(default, rename = "type", deserialize_with = "null_as_default")]
    pub kind: String,
}

/// A custom view's `viewPreferencesValues`: `issueGrouping` seeds the
/// columns and `issueSubGrouping` the lanes of the no-config board.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewPreferences {
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub issue_grouping: Option<String>,
    #[serde(default)]
    pub issue_sub_grouping: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub column_order_board: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub hidden_columns: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub hidden_rows: Vec<String>,
}

/// How one board is grouped: a configured mapping (with the space selector
/// the global mapping's `space` level filters on), or the no-config fallback
/// of a custom view's board preferences or the team's workflow states.
#[derive(Debug, Clone, Copy)]
pub enum BoardGrouping<'a> {
    Configured {
        mapping: &'a GroupingMapping,
        space: Option<&'a str>,
    },
    View {
        view: &'a ViewPreferences,
        states: &'a [WorkflowState],
    },
    Team {
        states: &'a [WorkflowState],
    },
}

/// A space override replaces the global mapping; nothing merges.
pub fn choose<'a>(
    config: Option<&'a GroupingConfig>,
    space: &'a str,
    view: Option<&'a ViewPreferences>,
    states: &'a [WorkflowState],
) -> BoardGrouping<'a> {
    match (config, view) {
        (Some(config), _) => match config.spaces.iter().find(|s| s.space == space) {
            Some(entry) => BoardGrouping::Configured {
                mapping: &entry.mapping,
                space: None,
            },
            None => BoardGrouping::Configured {
                mapping: &config.global,
                space: Some(space),
            },
        },
        (None, Some(view)) => BoardGrouping::View { view, states },
        (None, None) => BoardGrouping::Team { states },
    }
}

pub fn group_board(issues: &[GroupingIssue], grouping: BoardGrouping<'_>) -> Vec<LinearTab> {
    match grouping {
        BoardGrouping::Configured { mapping, space } => configured(issues, mapping, space),
        BoardGrouping::View { view, states } => fallback(issues, Some(view), states),
        BoardGrouping::Team { states } => fallback(issues, None, states),
    }
}

/// `tabs[0].groups` stays in `groups`, so a client that predates `tabs`
/// renders the first tab's columns.
pub fn fill_snapshot(snapshot: &mut LinearSnapshot, tabs: Vec<LinearTab>) {
    snapshot.groups = tabs.first().map(|t| t.groups.clone()).unwrap_or_default();
    snapshot.tabs = tabs;
}

const DEFAULT_TAB: &str = "Board";
const UNGROUPED_COLUMN: &str = "Issues";
const DEFAULT_EXCLUDED_STATE_TYPES: [&str; 2] = ["triage", "backlog"];

fn identifier(issue: &GroupingIssue) -> &str {
    if issue.identifier.is_empty() {
        &issue.id
    } else {
        &issue.identifier
    }
}

fn natural(identifier: &str) -> (String, i64) {
    if let Some((prefix, number)) = identifier.rsplit_once('-') {
        if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(n) = number.parse() {
                return (prefix.to_owned(), n);
            }
        }
    }
    (identifier.to_owned(), 0)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
struct SortKey {
    none: bool,
    rank: i64,
    prefix: String,
    number: i64,
    text: String,
    id: String,
}

#[derive(Debug, Clone)]
struct Group {
    key: String,
    label: String,
    kind: Option<String>,
    sort: SortKey,
}

impl Group {
    fn order(&self) -> (&SortKey, &str, &str) {
        (&self.sort, &self.label, &self.key)
    }
}

fn none_group(kind: &str) -> Group {
    let field = kind.strip_prefix("label-group:").unwrap_or(kind);
    Group {
        key: String::new(),
        label: format!("No {field}"),
        kind: None,
        sort: SortKey {
            none: true,
            ..SortKey::default()
        },
    }
}

fn named(id: &str, name: &str, sort: SortKey) -> Group {
    let label = if name.is_empty() { id } else { name };
    Group {
        key: id.to_owned(),
        label: label.to_owned(),
        kind: None,
        sort: SortKey {
            text: label.to_lowercase(),
            id: id.to_owned(),
            ..sort
        },
    }
}

fn priority_name(p: i64) -> Option<&'static str> {
    match p {
        1 => Some("Urgent"),
        2 => Some("High"),
        3 => Some("Medium"),
        4 => Some("Low"),
        _ => None,
    }
}

/// The plugin's `Engine.value`: an issue's group for one field kind, with the
/// plugin's sort order. Group keys are Linear ids (priority as "1".."4").
fn field_group(issue: &GroupingIssue, kind: &str) -> Group {
    let group = match kind {
        "team" => issue
            .team
            .as_ref()
            .filter(|t| !t.id.is_empty())
            .map(|t| named(&t.id, &t.key, SortKey::default())),
        "project" => by_id(issue.project.as_ref()),
        "assignee" => by_id(issue.assignee.as_ref()),
        "milestone" => by_id(issue.project_milestone.as_ref()),
        "parent" => issue.parent.as_ref().filter(|p| !p.id.is_empty()).map(|p| {
            let label = if p.identifier.is_empty() {
                &p.id
            } else {
                &p.identifier
            };
            let (prefix, number) = natural(label);
            Group {
                key: p.id.clone(),
                label: label.clone(),
                kind: None,
                sort: SortKey {
                    prefix,
                    number,
                    id: p.id.clone(),
                    ..SortKey::default()
                },
            }
        }),
        "cycle" => issue.cycle.as_ref().filter(|c| !c.id.is_empty()).map(|c| {
            let name = match (c.name.is_empty(), c.number) {
                (false, _) => c.name.clone(),
                (true, Some(n)) => format!("Cycle {n}"),
                (true, None) => c.id.clone(),
            };
            named(
                &c.id,
                &name,
                SortKey {
                    rank: c.number.unwrap_or(0),
                    ..SortKey::default()
                },
            )
        }),
        "state" => issue.state.as_ref().filter(|s| !s.id.is_empty()).map(|s| {
            let mut group = named(
                &s.id,
                &s.name,
                SortKey {
                    rank: state_rank(&s.kind),
                    ..SortKey::default()
                },
            );
            group.kind = (!s.kind.is_empty()).then(|| s.kind.clone());
            group
        }),
        "priority" => issue.priority.and_then(|p| {
            priority_name(p).map(|name| Group {
                key: p.to_string(),
                label: name.to_owned(),
                kind: None,
                sort: SortKey {
                    rank: p,
                    ..SortKey::default()
                },
            })
        }),
        _ => kind
            .strip_prefix("label-group:")
            .and_then(|group| label_group(issue, group)),
    };
    group.unwrap_or_else(|| none_group(kind))
}

fn by_id(node: Option<&IssueNamed>) -> Option<Group> {
    node.filter(|n| !n.id.is_empty())
        .map(|n| named(&n.id, &n.name, SortKey::default()))
}

fn state_rank(kind: &str) -> i64 {
    LINEAR_STATE_TYPES
        .iter()
        .position(|t| *t == kind)
        .unwrap_or(LINEAR_STATE_TYPES.len()) as i64
}

fn label_group(issue: &GroupingIssue, group: &str) -> Option<Group> {
    issue
        .labels
        .nodes
        .iter()
        .filter(|l| !l.id.is_empty() && l.parent.as_ref().is_some_and(|p| p.name == group))
        .map(|l| named(&l.id, &l.name, SortKey::default()))
        .min_by(|a, b| (&a.sort.text, &a.key).cmp(&(&b.sort.text, &b.key)))
}

/// A level set to `ticket` or `sub-ticket`, or left out, does not split the
/// board at that level (R7): the pane-per-ticket reading is the plugin's pane
/// placement, which the board view does not port.
fn field_level<'a>(mapping: &'a GroupingMapping, level: &str) -> Option<&'a str> {
    mapping
        .levels
        .get(level)
        .map(String::as_str)
        .filter(|kind| *kind != "ticket" && *kind != "sub-ticket")
}

fn distinct(groups: impl Iterator<Item = Group>) -> Vec<Group> {
    let mut by_key: BTreeMap<String, Group> = BTreeMap::new();
    for group in groups {
        by_key.entry(group.key.clone()).or_insert(group);
    }
    let mut out: Vec<Group> = by_key.into_values().collect();
    out.sort_by(|a, b| a.order().cmp(&b.order()));
    out
}

struct Placed<'a> {
    issue: &'a GroupingIssue,
    tab: Option<Group>,
    column: Option<Group>,
    row: Option<Group>,
}

fn configured(
    issues: &[GroupingIssue],
    mapping: &GroupingMapping,
    space: Option<&str>,
) -> Vec<LinearTab> {
    let space_kind = field_level(mapping, "space");
    let (tk, ck, rk) = (
        field_level(mapping, "tab"),
        field_level(mapping, "column"),
        field_level(mapping, "row"),
    );
    let sub_ticket = mapping.levels.values().any(|k| k == "sub-ticket");

    let mut sorted: Vec<&GroupingIssue> = issues.iter().collect();
    sorted.sort_by(|a, b| {
        cell_order(a, sub_ticket)
            .cmp(&cell_order(b, sub_ticket))
            .then_with(|| a.id.cmp(&b.id))
    });
    let placed: Vec<Placed> = sorted
        .into_iter()
        .filter(|issue| match (space_kind, space) {
            (Some(kind), Some(space)) => field_group(issue, kind).label == space,
            _ => true,
        })
        .map(|issue| Placed {
            issue,
            tab: tk.map(|k| field_group(issue, k)),
            column: ck.map(|k| field_group(issue, k)),
            row: rk.map(|k| field_group(issue, k)),
        })
        .collect();

    let key_of = |g: &Option<Group>| g.as_ref().map(|g| g.key.clone());
    let tabs = match tk {
        Some(_) => distinct(placed.iter().filter_map(|p| p.tab.clone()))
            .into_iter()
            .map(|g| (Some(g.key), g.label))
            .collect(),
        None => vec![(None, DEFAULT_TAB.to_owned())],
    };
    tabs.into_iter()
        .map(|(tab_key, tab_label)| {
            let in_tab: Vec<&Placed> = placed
                .iter()
                .filter(|p| key_of(&p.tab) == tab_key)
                .collect();
            let columns = match ck {
                Some(_) => distinct(in_tab.iter().filter_map(|p| p.column.clone()))
                    .into_iter()
                    .map(Some)
                    .collect(),
                None => vec![None],
            };
            let rows: Vec<Group> = distinct(in_tab.iter().filter_map(|p| p.row.clone()));
            let groups = columns
                .into_iter()
                .map(|column| {
                    let column_key = column.as_ref().map(|g| g.key.clone());
                    let in_column: Vec<&&Placed> = in_tab
                        .iter()
                        .filter(|p| key_of(&p.column) == column_key)
                        .collect();
                    let lanes = rows
                        .iter()
                        .map(|row| LinearLane {
                            key: row.key.clone(),
                            label: row.label.clone(),
                            issues: in_column
                                .iter()
                                .filter(|p| key_of(&p.row).as_deref() == Some(row.key.as_str()))
                                .map(|p| identifier(p.issue).to_owned())
                                .collect(),
                        })
                        .collect();
                    let (key, label, kind) = match column {
                        Some(g) => (g.key, g.label, g.kind),
                        None => (String::new(), UNGROUPED_COLUMN.to_owned(), None),
                    };
                    LinearGroup {
                        key,
                        label,
                        kind,
                        issues: in_column
                            .iter()
                            .map(|p| identifier(p.issue).to_owned())
                            .collect(),
                        lanes,
                    }
                })
                .collect();
            LinearTab {
                key: tab_key.unwrap_or_default(),
                label: tab_label,
                groups,
            }
        })
        .collect()
}

/// Cards in a cell read in natural identifier order; under a `sub-ticket`
/// level a parent leads its own children (the plugin's `cell_order`).
fn cell_order(issue: &GroupingIssue, sub_ticket: bool) -> ((String, i64), bool, (String, i64)) {
    let own = natural(identifier(issue));
    let parent = issue
        .parent
        .as_ref()
        .filter(|p| sub_ticket && !p.id.is_empty() && !p.identifier.is_empty());
    match parent {
        Some(p) => (natural(&p.identifier), true, own),
        None => (own.clone(), false, own),
    }
}

const VIEW_GROUPINGS: [&str; 5] = ["workflowState", "assignee", "priority", "label", "project"];

/// The script-era groups one issue belongs to under a view grouping, keys
/// included, so the no-config board is unchanged. Only `label` can place one issue in several groups.
fn view_keys(
    issue: &GroupingIssue,
    grouping: &str,
    states: &[WorkflowState],
) -> Vec<(String, String)> {
    match grouping {
        "assignee" => vec![match issue.assignee.as_ref().filter(|a| !a.id.is_empty()) {
            Some(a) => (a.id.clone(), or_id(&a.name, &a.id)),
            None => ("unassigned".into(), "Unassigned".into()),
        }],
        "priority" => {
            let p = issue.priority.unwrap_or(0);
            let label = match p {
                0 => "No priority".to_owned(),
                p => priority_name(p).map_or_else(|| p.to_string(), str::to_owned),
            };
            vec![(p.to_string(), label)]
        }
        "label" => {
            let keys: Vec<(String, String)> = issue
                .labels
                .nodes
                .iter()
                .filter(|l| !l.id.is_empty())
                .map(|l| (l.id.clone(), or_id(&l.name, &l.id)))
                .collect();
            if keys.is_empty() {
                vec![("nolabel".into(), "No label".into())]
            } else {
                keys
            }
        }
        "project" => vec![match issue.project.as_ref().filter(|p| !p.id.is_empty()) {
            Some(p) => (p.id.clone(), or_id(&p.name, &p.id)),
            None => ("noproject".into(), "No project".into()),
        }],
        _ => {
            let state = issue.state.clone().unwrap_or_default();
            let name = states
                .iter()
                .find(|s| s.id == state.id)
                .map(|s| s.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| or_id(&state.name, &state.id));
            vec![(state.id, name)]
        }
    }
}

fn or_id(name: &str, id: &str) -> String {
    if name.is_empty() { id } else { name }.to_owned()
}

/// An issue's identifier, its column keys, and its lane keys when the view
/// has lanes.
type Member = (String, Vec<String>, Option<Vec<String>>);

fn fallback(
    issues: &[GroupingIssue],
    view: Option<&ViewPreferences>,
    states: &[WorkflowState],
) -> Vec<LinearTab> {
    let usable = view
        .and_then(|v| v.issue_grouping.as_deref())
        .is_some_and(|g| VIEW_GROUPINGS.contains(&g));
    let view = view.filter(|_| usable);
    let grouping = view
        .and_then(|v| v.issue_grouping.as_deref())
        .unwrap_or("workflowState");
    let sub_grouping = view
        .and_then(|v| v.issue_sub_grouping.as_deref())
        .filter(|g| VIEW_GROUPINGS.contains(g));
    let hidden_columns: BTreeSet<&str> = view
        .map(|v| v.hidden_columns.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let hidden_rows: BTreeSet<&str> = view
        .map(|v| v.hidden_rows.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let state_ids: BTreeSet<&str> = states.iter().map(|s| s.id.as_str()).collect();

    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let mut appearance: Vec<String> = Vec::new();
    let mut members: Vec<Member> = Vec::new();
    let mut row_labels: BTreeMap<String, String> = BTreeMap::new();
    let mut row_appearance: Vec<String> = Vec::new();
    for issue in issues {
        let columns = view_keys(issue, grouping, states);
        for (key, label) in &columns {
            if !labels.contains_key(key) {
                labels.insert(key.clone(), label.clone());
                appearance.push(key.clone());
            }
        }
        let rows = sub_grouping.map(|g| {
            let rows = view_keys(issue, g, states);
            for (key, label) in &rows {
                if !row_labels.contains_key(key) {
                    row_labels.insert(key.clone(), label.clone());
                    row_appearance.push(key.clone());
                }
            }
            rows.into_iter().map(|(k, _)| k).collect()
        });
        members.push((
            identifier(issue).to_owned(),
            columns.into_iter().map(|(k, _)| k).collect(),
            rows,
        ));
    }

    let mut order: Vec<String> = view
        .map(|v| v.column_order_board.clone())
        .unwrap_or_default();
    if grouping != "workflowState" {
        order.retain(|k| !state_ids.contains(k.as_str()));
    } else if order.is_empty() {
        order = states
            .iter()
            .filter(|s| usable || s.kind != "canceled")
            .map(|s| s.id.clone())
            .collect();
    }
    for key in appearance {
        if !order.contains(&key) {
            order.push(key);
        }
    }

    let lanes: Option<Vec<String>> = sub_grouping.map(|g| {
        let mut rows: Vec<String> = Vec::new();
        if g == "workflowState" {
            rows.extend(
                states
                    .iter()
                    .filter(|s| row_labels.contains_key(&s.id))
                    .map(|s| s.id.clone()),
            );
        }
        for key in &row_appearance {
            if !rows.contains(key) {
                rows.push(key.clone());
            }
        }
        rows.retain(|k| !hidden_rows.contains(k.as_str()));
        rows
    });

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let groups = order
        .iter()
        .filter(|k| seen.insert(k.as_str()) && !hidden_columns.contains(k.as_str()))
        .map(|key| {
            let label = labels.get(key).cloned().unwrap_or_else(|| {
                states
                    .iter()
                    .find(|s| &s.id == key)
                    .map(|s| or_id(&s.name, key))
                    .unwrap_or_else(|| key.clone())
            });
            let kind = (grouping == "workflowState")
                .then(|| states.iter().find(|s| &s.id == key))
                .flatten()
                .map(|s| s.kind.clone())
                .filter(|k| !k.is_empty());
            let here: Vec<&Member> = members
                .iter()
                .filter(|(_, columns, _)| columns.contains(key))
                .collect();
            let lanes: Vec<LinearLane> = lanes
                .iter()
                .flatten()
                .map(|row| LinearLane {
                    key: row.clone(),
                    label: row_labels.get(row).cloned().unwrap_or_default(),
                    issues: here
                        .iter()
                        .filter(|(_, _, rows)| rows.iter().flatten().any(|r| r == row))
                        .map(|(id, _, _)| id.clone())
                        .collect(),
                })
                .collect();
            let issues = here
                .iter()
                .filter(|(_, _, rows)| {
                    rows.as_ref()
                        .is_none_or(|rows| rows.iter().any(|r| !hidden_rows.contains(r.as_str())))
                })
                .map(|(id, _, _)| id.clone())
                .collect();
            LinearGroup {
                key: key.clone(),
                label,
                kind,
                issues,
                lanes,
            }
        })
        .collect();
    vec![LinearTab {
        key: String::new(),
        label: DEFAULT_TAB.to_owned(),
        groups,
    }]
}

/// The mapping's filter as a Linear `IssueFilter` (the plugin's
/// `board-linear.sh clause`), with the default exclusion of triage and
/// backlog when the filter names neither `state` nor `state-type`
/// (`board-config.sh resolved`). A value names an entity by its Linear id or
/// by the handle a person reads on the board.
pub fn issue_filter(mapping: &GroupingMapping) -> Value {
    let filter = &mapping.filter;
    let mut clauses = Vec::new();
    for key in [
        "team",
        "project",
        "milestone",
        "cycle",
        "assignee",
        "state",
        "parent",
        "label",
        "state-type",
    ] {
        if let Some(raw) = filter.get(key) {
            clauses.push(clause(key, raw));
        }
    }
    if !filter.contains_key("state") && !filter.contains_key("state-type") {
        clauses.push(json!({"state": {"type": {"nin": DEFAULT_EXCLUDED_STATE_TYPES}}}));
    }
    if let Some(raw) = filter.get("priority") {
        clauses.push(json!({"priority": {"in": values(raw)}}));
    }
    json!({ "and": clauses })
}

fn values(raw: &Value) -> Vec<Value> {
    match raw {
        Value::Array(items) => items.clone(),
        single => vec![single.clone()],
    }
}

fn strings(raw: &Value) -> Vec<String> {
    values(raw)
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

fn id_or(field: &str, values: &[String]) -> Value {
    json!({"or": [{"id": {"in": values}}, {field: {"in": values}}]})
}

fn clause(key: &str, raw: &Value) -> Value {
    let values = strings(raw);
    match key {
        "team" => json!({"team": {"or": [
            {"id": {"in": values}}, {"key": {"in": values}}, {"name": {"in": values}}
        ]}}),
        "project" => json!({"project": id_or("name", &values)}),
        "milestone" => json!({"projectMilestone": id_or("name", &values)}),
        "cycle" => json!({"cycle": id_or("name", &values)}),
        "state" => json!({"state": id_or("name", &values)}),
        "label" => json!({"labels": {"some": id_or("name", &values)}}),
        "state-type" => json!({"state": {"type": {"in": values}}}),
        "assignee" => {
            let mut alts = Vec::new();
            if values.iter().any(|v| v == "me") {
                alts.push(json!({"isMe": {"eq": true}}));
            }
            let rest: Vec<&String> = values.iter().filter(|v| *v != "me").collect();
            if !rest.is_empty() {
                alts.extend([
                    json!({"id": {"in": rest}}),
                    json!({"name": {"in": rest}}),
                    json!({"displayName": {"in": rest}}),
                    json!({"email": {"in": rest}}),
                ]);
            }
            if alts.len() == 1 {
                json!({"assignee": alts.remove(0)})
            } else {
                json!({"assignee": {"or": alts}})
            }
        }
        _ => {
            // IssueFilter has no identifier comparator; an identifier is a
            // team key and a number.
            let ids: Vec<&String> = values.iter().filter(|v| parent_key(v).is_none()).collect();
            let mut alts = Vec::new();
            if !ids.is_empty() {
                alts.push(json!({"id": {"in": ids}}));
            }
            for (team, number) in values.iter().filter_map(|v| parent_key(v)) {
                alts.push(json!({"and": [
                    {"team": {"key": {"eq": team}}}, {"number": {"eq": number}}
                ]}));
            }
            json!({"parent": {"or": alts}})
        }
    }
}

fn parent_key(value: &str) -> Option<(String, u64)> {
    let (team, number) = value.split_once('-')?;
    let team_ok = (1..=10).contains(&team.len())
        && team.as_bytes()[0].is_ascii_alphabetic()
        && team.bytes().all(|b| b.is_ascii_alphanumeric());
    let number_ok = (1..=9).contains(&number.len()) && number.bytes().all(|b| b.is_ascii_digit());
    if team_ok && number_ok {
        Some((team.to_ascii_uppercase(), number.parse().ok()?))
    } else {
        None
    }
}
