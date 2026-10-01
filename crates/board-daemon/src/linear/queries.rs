use serde_json::{json, Value};

use super::client::{LinearClient, LinearError, Page};
use super::ApiKey;

const ISSUE_FIELDS: &str = "id identifier title url branchName updatedAt priority \
    state { id name type } parent { id identifier title } project { id name } \
    projectMilestone { id name } cycle { id number name } \
    team { id key name } assignee { id name } \
    labels(first: 10) { nodes { id name parent { id name } } }";

// One call per issue page: each connection is read once at 50 and its
// `hasNextPage` reported, never drained (the plugin's `fetch_issue_detail`).
const ISSUE_DETAIL_FIELDS: &str = "description dueDate estimate \
    parent { state { id name type } } \
    children(first: 50) { nodes { id identifier title state { id name type } } pageInfo { hasNextPage } } \
    relations(first: 50) { nodes { id type relatedIssue { id identifier title state { id name type } } } pageInfo { hasNextPage } } \
    inverseRelations(first: 50) { nodes { id type issue { id identifier title state { id name type } } } pageInfo { hasNextPage } } \
    comments(first: 50) { nodes { id body createdAt user { id name } parent { id } } pageInfo { hasNextPage } } \
    history(first: 50) { nodes { id createdAt actor { id name } fromState { name } toState { name } fromAssignee { name } toAssignee { name } fromPriority toPriority addedLabels { name } removedLabels { name } } pageInfo { hasNextPage } }";

const STATES: &str = "states(first: 100) { nodes { id name type } }";

const VIEW_FIELDS: &str = "id name modelName archivedAt filterData \
    viewPreferencesValues { layout issueGrouping issueSubGrouping issueNesting \
    hiddenColumns hiddenRows columnOrderBoard }";

fn issues_query() -> String {
    format!(
        "query($n:Int,$after:String,$filter:IssueFilter){{issues(first:$n,after:$after,filter:$filter)\
         {{nodes{{{ISSUE_FIELDS}}} pageInfo{{hasNextPage endCursor}}}}}}"
    )
}

impl LinearClient {
    /// Issues in a project, cancelled ones left out.
    pub fn project_issues(&self, project_id: &str) -> Result<Page, LinearError> {
        self.project_issues_with(&self.key()?, project_id)
    }

    pub(super) fn project_issues_with(
        &self,
        key: &ApiKey,
        project_id: &str,
    ) -> Result<Page, LinearError> {
        let filter = json!({
            "project": {"id": {"eq": project_id}},
            "state": {"type": {"neq": "canceled"}},
        });
        self.issues_matching_with(key, filter)
    }

    /// Issues matching a Linear `IssueFilter`, passed through as a variable
    /// and never rewritten.
    pub fn issues_matching(&self, filter: Value) -> Result<Page, LinearError> {
        self.issues_matching_with(&self.key()?, filter)
    }

    pub(super) fn issues_matching_with(
        &self,
        key: &ApiKey,
        filter: Value,
    ) -> Result<Page, LinearError> {
        self.paged(key, &issues_query(), "issues", json!({"filter": filter}))
    }

    /// Issues in a custom view, read through the view's own `filterData`.
    pub fn view_issues(&self, view_id: &str) -> Result<Page, LinearError> {
        let key = self.key()?;
        let view = self.view_with(&key, view_id)?;
        self.issues_in_view_with(&key, &view)
    }

    /// Issues in a view already read, so its `filterData` is not fetched twice.
    pub fn issues_in_view(&self, view: &Value) -> Result<Page, LinearError> {
        self.issues_in_view_with(&self.key()?, view)
    }

    pub(super) fn issues_in_view_with(
        &self,
        key: &ApiKey,
        view: &Value,
    ) -> Result<Page, LinearError> {
        self.paged(key, &issues_query(), "issues", view_filter(view))
    }

    /// One project with every team's workflow states, in Linear's order.
    pub fn project(&self, project_id: &str) -> Result<Value, LinearError> {
        self.project_with(&self.key()?, project_id)
    }

    pub(super) fn project_with(
        &self,
        key: &ApiKey,
        project_id: &str,
    ) -> Result<Value, LinearError> {
        let query = format!(
            "query($id:String!){{project(id:$id){{id name url \
             teams(first:50){{nodes{{id key name {STATES}}}}}}}}}"
        );
        let data = self.execute(key, &query, json!({"id": project_id}))?;
        found(&data["project"])
    }

    /// Projects the key's owner is a member of.
    pub fn projects(&self) -> Result<Page, LinearError> {
        let key = self.key()?;
        let query = "query($n:Int,$after:String,$filter:ProjectFilter){projects(first:$n,after:$after,filter:$filter)\
            {nodes{id name teams(first:5){nodes{id key name}}} pageInfo{hasNextPage endCursor}}}";
        let filter = json!({"members": {"some": {"isMe": {"eq": true}}}});
        self.paged(&key, query, "projects", json!({"filter": filter}))
    }

    pub fn views(&self) -> Result<Page, LinearError> {
        let key = self.key()?;
        let query = format!(
            "query($n:Int,$after:String){{customViews(first:$n,after:$after)\
             {{nodes{{{VIEW_FIELDS}}} pageInfo{{hasNextPage endCursor}}}}}}"
        );
        self.paged(&key, &query, "customViews", json!({}))
    }

    pub fn view(&self, view_id: &str) -> Result<Value, LinearError> {
        let key = self.key()?;
        self.view_with(&key, view_id)
    }

    pub fn teams(&self) -> Result<Page, LinearError> {
        let key = self.key()?;
        let query = format!(
            "query($n:Int,$after:String){{teams(first:$n,after:$after)\
             {{nodes{{id key name {STATES}}} pageInfo{{hasNextPage endCursor}}}}}}"
        );
        self.paged(&key, &query, "teams", json!({}))
    }

    /// One issue by id or identifier, with the fields the issue page shows.
    pub fn issue(&self, id: &str) -> Result<Value, LinearError> {
        let key = self.key()?;
        let query =
            format!("query($id:String!){{issue(id:$id){{{ISSUE_FIELDS} {ISSUE_DETAIL_FIELDS}}}}}");
        let data = self.execute(&key, &query, json!({"id": id}))?;
        found(&data["issue"])
    }

    pub(super) fn view_with(&self, key: &ApiKey, view_id: &str) -> Result<Value, LinearError> {
        let query = format!("query($id:String!){{customView(id:$id){{{VIEW_FIELDS}}}}}");
        let data = self.execute(key, &query, json!({"id": view_id}))?;
        found(&data["customView"])
    }
}

fn view_filter(view: &Value) -> Value {
    let filter = match &view["filterData"] {
        Value::Object(_) => view["filterData"].clone(),
        _ => json!({}),
    };
    json!({ "filter": filter })
}

fn found(value: &Value) -> Result<Value, LinearError> {
    if value.is_object() {
        Ok(value.clone())
    } else {
        Err(LinearError::NotFound)
    }
}
