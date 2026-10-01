//! What one space's board needs from Linear, read in one pass: the view (when
//! the binding names one), the issues, and the project with its teams'
//! workflow states.

use serde_json::Value;

use super::client::{LinearClient, LinearError};

/// What to read for a space. Two reads with equal plans share one cache
/// entry; a rebind or a grouping change yields a new plan and so a new read.
#[derive(Clone, Debug, PartialEq)]
pub struct FetchPlan {
    pub project_id: String,
    /// The binding's custom view; `None` under a grouping config.
    pub view_id: Option<String>,
    /// The grouping config's compiled `IssueFilter`, when one is in force.
    pub filter: Option<Value>,
    /// The space name (herdr workspace label) the grouping was chosen for.
    pub label: String,
}

#[derive(Clone, Debug, Default)]
pub struct SpaceRead {
    pub project: Option<Value>,
    /// `none`, `ok`, `archived`, `not_in_project`, `unsupported_grouping` or
    /// `not_found`, as the plugin's snapshot names them.
    pub view_status: &'static str,
    pub view: Option<Value>,
    pub issues: Vec<Value>,
    pub partial: bool,
}

const SUPPORTED_VIEW_GROUPINGS: [&str; 5] =
    ["workflowState", "assignee", "priority", "label", "project"];

pub fn fetch(client: &LinearClient, plan: &FetchPlan) -> Result<SpaceRead, LinearError> {
    let mut read = SpaceRead {
        view_status: "none",
        ..SpaceRead::default()
    };
    if let (Some(view_id), None) = (plan.view_id.as_deref(), plan.filter.as_ref()) {
        match client.view(view_id) {
            Ok(view) => {
                read.view_status = view_status(&view, &plan.project_id);
                read.view = Some(view);
            }
            Err(LinearError::NotFound) => read.view_status = "not_found",
            Err(error) => return Err(error),
        }
    }
    let page = match (&plan.filter, &read.view) {
        (Some(filter), _) => client.issues_matching(filter.clone())?,
        (None, Some(view)) if read.view_status == "ok" => client.issues_in_view(view)?,
        _ => client.project_issues(&plan.project_id)?,
    };
    read.issues = page.nodes;
    read.partial = page.partial;
    read.project = match client.project(&plan.project_id) {
        Ok(project) => Some(project),
        Err(LinearError::NotFound) => None,
        Err(error) => return Err(error),
    };
    Ok(read)
}

fn view_status(view: &Value, project_id: &str) -> &'static str {
    let grouping = view["viewPreferencesValues"]["issueGrouping"]
        .as_str()
        .unwrap_or("");
    if !view["archivedAt"].is_null() {
        "archived"
    } else if !names_project(&view["filterData"], project_id) {
        // Checked on every read: a filter edited in Linear to drop the
        // project would otherwise board another project's issues.
        "not_in_project"
    } else if SUPPORTED_VIEW_GROUPINGS.contains(&grouping) {
        "ok"
    } else {
        "unsupported_grouping"
    }
}

/// Whether a view's `filterData` names the project, as the plugin's
/// `names_project` reads it: `project.id.eq` or `project.id.in`, at any depth
/// of `and` and `or`.
pub fn names_project(filter: &Value, project_id: &str) -> bool {
    match filter {
        Value::Array(nodes) => nodes.iter().any(|n| names_project(n, project_id)),
        Value::Object(map) => map.iter().any(|(key, value)| match key.as_str() {
            "project" => {
                let id = &value["id"];
                id["eq"].as_str() == Some(project_id)
                    || id["in"]
                        .as_array()
                        .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(project_id)))
            }
            "and" | "or" => names_project(value, project_id),
            _ => false,
        }),
        _ => false,
    }
}
