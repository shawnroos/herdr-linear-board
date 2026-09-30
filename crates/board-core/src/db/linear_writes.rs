//! Request-level units of work for Linear-mode local state. boardd and the
//! fake client both run these, so the two cannot drift. Every write cleans
//! the free text it stores (KTD13) and returns what it changed.

use std::fmt;

use rusqlite::{params, OptionalExtension};
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{json, Map, Value};

use super::linear_state::{
    is_issue_identifier, ActivityClaims, GroupingConfig, GroupingMapping, Mark, MarkKind,
    NewActivity, NewMark, SpaceGrouping, WorktreeBinding, WorktreeBindingState,
    LINEAR_ACTIVITY_KEEP_PER_SPACE,
};
use super::Db;
use crate::protocol::{
    LinearActivityListParams, LinearActivityListResult, LinearActivityOutcome,
    LinearActivityRecordParams, LinearActivityRecordResult, LinearBindParams, LinearChange,
    LinearGroupingGetParams, LinearGroupingGetResult, LinearGroupingSetParams, LinearMarkSetParams,
    LinearNoteSetParams, LinearReplace, LinearShowRequestParams, LinearState, LinearUnbindParams,
    Note, ShowRequest,
};
use crate::text::{sanitise_json, strip_control_and_format, strip_control_keep_lines};
use crate::Error;

const ACTIVITY_LIST_DEFAULT: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LocalStateRejection {
    #[error("{0}")]
    Refused(String),
    #[error("issue {issue} is not a card the board knows in space {space}")]
    UnknownIssue { space: String, issue: String },
    #[error("issue {issue} is already bound to {holder}; unbind it there first")]
    IssueBoundElsewhere { issue: String, holder: String },
    #[error("{0}")]
    Missing(String),
    #[error("show request {id} was already answered")]
    ShowRequestAnswered { id: i64 },
}

#[derive(Debug, thiserror::Error)]
pub enum LocalStateError {
    #[error(transparent)]
    Rejected(#[from] LocalStateRejection),
    #[error(transparent)]
    Store(#[from] Error),
}

impl From<rusqlite::Error> for LocalStateError {
    fn from(e: rusqlite::Error) -> Self {
        LocalStateError::Store(e.into())
    }
}

type LsResult<T> = std::result::Result<T, LocalStateError>;

fn refused(message: String) -> LocalStateError {
    LocalStateRejection::Refused(message).into()
}

fn check_issue(issue: &str) -> LsResult<()> {
    if is_issue_identifier(issue) {
        Ok(())
    } else {
        Err(refused(format!(
            "issue {issue:?} is neither a Linear issue key (like WEB-123) nor an issue UUID"
        )))
    }
}

fn check_space(space: &str) -> LsResult<()> {
    if space.is_empty() || space.chars().any(char::is_control) {
        Err(refused(format!(
            "space {space:?} is refused; a space is non-empty text without control characters"
        )))
    } else {
        Ok(())
    }
}

fn line(text: &str) -> String {
    strip_control_and_format(text)
}

fn opt_line(text: Option<&str>) -> Option<String> {
    text.map(line)
}

pub fn clean_claims(claims: &ActivityClaims) -> ActivityClaims {
    ActivityClaims {
        herdr_socket: opt_line(claims.herdr_socket.as_deref()),
        herdr_pane_id: opt_line(claims.herdr_pane_id.as_deref()),
        herdr_workspace_id: opt_line(claims.herdr_workspace_id.as_deref()),
        card_id: claims.card_id,
        run_id: claims.run_id,
    }
}

/// The space a change is announced for: the explicit one, else the claimed
/// herdr workspace.
pub fn claimed_space(space: Option<&str>, claims: &ActivityClaims) -> Option<String> {
    space
        .or(claims.herdr_workspace_id.as_deref())
        .map(line)
        .filter(|s| !s.is_empty())
}

fn claimed_session(claims: &ActivityClaims) -> Option<String> {
    claims.herdr_socket.as_deref().map(|socket| {
        crate::paths::session_name_from_socket(Some(socket)).unwrap_or_else(|| "default".into())
    })
}

/// The root of the git worktree containing `cwd`, canonicalised.
pub fn resolve_worktree(cwd: &str) -> std::result::Result<String, LocalStateRejection> {
    if !cwd.starts_with('/') || cwd.chars().any(char::is_control) {
        return Err(LocalStateRejection::Refused(format!(
            "cwd {cwd:?} is refused; it must be an absolute path"
        )));
    }
    let canonical = std::fs::canonicalize(cwd).map_err(|e| {
        LocalStateRejection::Refused(format!("cwd {cwd:?} cannot be resolved: {e}"))
    })?;
    let root = canonical
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .ok_or_else(|| {
            LocalStateRejection::Refused(format!(
                "cwd {} is not inside a git worktree",
                canonical.display()
            ))
        })?;
    root.to_str().map(str::to_owned).ok_or_else(|| {
        LocalStateRejection::Refused(format!("worktree {} is not UTF-8", root.display()))
    })
}

fn is_save_issue(tool_name: &str) -> bool {
    tool_name == "save_issue" || tool_name.ends_with("__save_issue")
}

/// Parses JSON text, refusing an object that repeats a key. `serde_json`
/// keeps the last duplicate silently, which would hide a config fault.
pub fn parse_json_refusing_duplicate_keys(text: &str) -> std::result::Result<Value, String> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = StrictValue
        .deserialize(&mut deserializer)
        .map_err(|e| e.to_string())?;
    deserializer.end().map_err(|e| e.to_string())?;
    Ok(value)
}

struct StrictValue;

impl<'de> DeserializeSeed<'de> for StrictValue {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("a number JSON cannot hold"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(StrictValue)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate key {key:?}")));
            }
            let value = map.next_value_seed(StrictValue)?;
            out.insert(key, value);
        }
        Ok(Value::Object(out))
    }
}

fn parse_grouping<T: serde::de::DeserializeOwned>(text: &str, what: &str) -> LsResult<T> {
    let value = parse_json_refusing_duplicate_keys(text)
        .map_err(|e| refused(format!("{what} is not valid JSON: {e}")))?;
    serde_json::from_value(sanitise_json(value))
        .map_err(|e| refused(format!("{what} is not a grouping {what}: {e}")))
}

/// `GroupingConfig` as written by a person: an unknown key is a fault, not
/// something to ignore.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictConfig {
    global: GroupingMapping,
    #[serde(default)]
    spaces: Vec<StrictSpace>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictSpace {
    space: String,
    mapping: GroupingMapping,
}

impl From<StrictConfig> for GroupingConfig {
    fn from(c: StrictConfig) -> Self {
        GroupingConfig {
            global: c.global,
            spaces: c
                .spaces
                .into_iter()
                .map(|e| SpaceGrouping {
                    space: e.space,
                    mapping: e.mapping,
                })
                .collect(),
        }
    }
}

impl Db {
    pub fn linear_state(&self, space: &str) -> crate::Result<LinearState> {
        Ok(LinearState {
            space: space.to_owned(),
            space_bindings: self
                .list_space_bindings()?
                .into_iter()
                .filter(|b| b.space == space)
                .collect(),
            worktree_bindings: self.list_worktree_bindings()?,
            grouping: self.grouping_for_space(space)?,
            marks: self.list_marks(space)?,
            notes: self.list_notes(space, None)?,
            show_requests: self.pending_show_requests(space)?,
        })
    }

    /// Whether local state already names `issue`: a worktree binding, or
    /// activity recorded in `space`.
    pub fn issue_has_local_state(&self, space: &str, issue: &str) -> crate::Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM linear_worktree_bindings WHERE issue_identifier = ?2)
                 OR EXISTS(SELECT 1 FROM linear_activity
                           WHERE space = ?1 AND issue_identifier = ?2)",
            params![space, issue],
            |r| r.get(0),
        )?)
    }

    fn require_known(&self, space: &str, issue: &str, known_elsewhere: bool) -> LsResult<()> {
        check_space(space)?;
        check_issue(issue)?;
        if known_elsewhere || self.issue_has_local_state(space, issue)? {
            Ok(())
        } else {
            Err(LocalStateRejection::UnknownIssue {
                space: space.to_owned(),
                issue: issue.to_owned(),
            }
            .into())
        }
    }

    fn issue_holder(&self, issue: &str, except_path: &str) -> crate::Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT worktree_path FROM linear_worktree_bindings
                 WHERE issue_identifier = ?1 AND worktree_path != ?2
                 ORDER BY worktree_path LIMIT 1",
                params![issue, except_path],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn linear_bind(&self, p: &LinearBindParams) -> LsResult<LinearChange<WorktreeBinding>> {
        check_issue(&p.issue)?;
        let path = resolve_worktree(&p.cwd)?;
        let tx = self.conn.unchecked_transaction()?;
        if let Some(holder) = self.issue_holder(&p.issue, &path)? {
            return Err(LocalStateRejection::IssueBoundElsewhere {
                issue: p.issue.clone(),
                holder,
            }
            .into());
        }
        let before = self.worktree_binding(&path)?;
        let kept = before.clone();
        self.set_worktree_binding(&WorktreeBinding {
            worktree_path: path.clone(),
            issue: p.issue.clone(),
            state: WorktreeBindingState::Bound,
            branch: opt_line(p.branch.as_deref()),
            tab: opt_line(p.tab.as_deref()),
            display_name: opt_line(p.display_name.as_deref()),
            team_ids: kept
                .as_ref()
                .map(|b| b.team_ids.clone())
                .unwrap_or_default(),
            view: kept.as_ref().and_then(|b| b.view.clone()),
            carried: kept.map(|b| b.carried).unwrap_or_default(),
        })?;
        let after = self.worktree_binding(&path)?;
        tx.commit()?;
        Ok(LinearChange { before, after })
    }

    /// A worktree that no longer exists cannot be canonicalised, so its
    /// binding is also found by the path exactly as given.
    pub fn linear_unbind(&self, p: &LinearUnbindParams) -> LsResult<LinearChange<WorktreeBinding>> {
        let path = match resolve_worktree(&p.cwd) {
            Ok(path) => path,
            Err(rejection) => match self.worktree_binding(p.cwd.trim_end_matches('/'))? {
                Some(binding) => binding.worktree_path,
                None => return Err(rejection.into()),
            },
        };
        let before = self.worktree_binding(&path)?.ok_or_else(|| {
            LocalStateRejection::Missing(format!("worktree {path} has no binding"))
        })?;
        self.remove_worktree_binding(&path)?;
        Ok(LinearChange {
            before: Some(before),
            after: None,
        })
    }

    pub fn linear_grouping_get(
        &self,
        p: &LinearGroupingGetParams,
    ) -> crate::Result<LinearGroupingGetResult> {
        Ok(LinearGroupingGetResult {
            config: self.grouping_config()?,
            resolved: match p.space.as_deref() {
                Some(space) => self.grouping_for_space(space)?,
                None => None,
            },
        })
    }

    /// `write: false` is a preview: the same parse and validation, no write.
    pub fn linear_grouping_change(
        &self,
        p: &LinearGroupingSetParams,
        write: bool,
    ) -> LsResult<LinearChange<GroupingConfig>> {
        let before = self.grouping_config()?;
        let after = match (p.space.as_deref(), p.text.as_deref()) {
            (None, None) => {
                return Err(refused(
                    "a grouping change without a space needs text: a whole grouping config".into(),
                ))
            }
            (None, Some(text)) => parse_grouping::<StrictConfig>(text, "config")?.into(),
            (Some(space), Some(text)) => {
                check_space(space)?;
                let mapping = parse_grouping::<GroupingMapping>(text, "mapping")?;
                let mut config = before.clone().ok_or_else(|| {
                    refused(format!(
                        "space {space:?} cannot be mapped before a global mapping exists; a space mapping replaces the global one, so one must exist"
                    ))
                })?;
                match config.spaces.iter_mut().find(|e| e.space == space) {
                    Some(entry) => entry.mapping = mapping,
                    None => config.spaces.push(SpaceGrouping {
                        space: space.to_owned(),
                        mapping,
                    }),
                }
                config
            }
            (Some(space), None) => {
                let mut config = before
                    .clone()
                    .filter(|c| c.spaces.iter().any(|e| e.space == space))
                    .ok_or_else(|| {
                        LocalStateRejection::Missing(format!(
                            "space {space:?} has no grouping override"
                        ))
                    })?;
                config.spaces.retain(|e| e.space != space);
                config
            }
        };
        after.validate()?;
        if !write {
            return Ok(LinearChange {
                before,
                after: Some(after),
            });
        }
        self.replace_grouping(&after)?;
        Ok(LinearChange {
            before,
            after: self.grouping_config()?,
        })
    }

    fn replace_mark(&self, mark: &NewMark<'_>) -> LsResult<LinearReplace<Mark>> {
        let tx = self.conn.unchecked_transaction()?;
        let before: Vec<Mark> = self
            .list_marks(mark.space)?
            .into_iter()
            .filter(|m| m.issue == mark.issue && m.kind == mark.kind)
            .collect();
        for old in &before {
            self.remove_mark(old.id)?;
        }
        let after = self.add_mark(mark)?;
        tx.commit()?;
        Ok(LinearReplace { before, after })
    }

    /// Replaces the issue's mark of the same kind. `known_elsewhere` is the
    /// caller's own knowledge of the issue, such as a cached snapshot.
    pub fn linear_mark_set(
        &self,
        p: &LinearMarkSetParams,
        known_elsewhere: bool,
    ) -> LsResult<LinearReplace<Mark>> {
        self.require_known(&p.space, &p.issue, known_elsewhere)?;
        let text = p.text.as_deref().map(strip_control_keep_lines);
        let created_by = opt_line(p.created_by.as_deref());
        self.replace_mark(&NewMark {
            space: &p.space,
            issue: &p.issue,
            kind: p.kind,
            text: text.as_deref(),
            detail: None,
            created_by: created_by.as_deref(),
        })
    }

    pub fn linear_mark_clear(&self, id: i64) -> LsResult<LinearChange<Mark>> {
        let before = self
            .mark(id)?
            .ok_or_else(|| LocalStateRejection::Missing(format!("mark {id} does not exist")))?;
        self.remove_mark(id)?;
        Ok(LinearChange {
            before: Some(before),
            after: None,
        })
    }

    /// Replaces the author's note on the issue.
    pub fn linear_note_set(
        &self,
        p: &LinearNoteSetParams,
        known_elsewhere: bool,
    ) -> LsResult<LinearReplace<Note>> {
        self.require_known(&p.space, &p.issue, known_elsewhere)?;
        let body = strip_control_keep_lines(&p.body);
        let author = line(&p.author);
        let tx = self.conn.unchecked_transaction()?;
        let before: Vec<Note> = self
            .list_notes(&p.space, Some(&p.issue))?
            .into_iter()
            .filter(|n| n.author == author)
            .collect();
        for old in &before {
            self.remove_note(old.id)?;
        }
        let after = self.add_note(&p.space, &p.issue, &body, &author)?;
        tx.commit()?;
        Ok(LinearReplace { before, after })
    }

    pub fn linear_note_clear(&self, id: i64) -> LsResult<LinearChange<Note>> {
        let before = self
            .note(id)?
            .ok_or_else(|| LocalStateRejection::Missing(format!("note {id} does not exist")))?;
        self.remove_note(id)?;
        Ok(LinearChange {
            before: Some(before),
            after: None,
        })
    }

    pub fn linear_show_request(
        &self,
        p: &LinearShowRequestParams,
        known_elsewhere: bool,
    ) -> LsResult<LinearChange<ShowRequest>> {
        self.require_known(&p.space, &p.issue, known_elsewhere)?;
        let reason = opt_line(p.reason.as_deref());
        let requested_by = opt_line(p.requested_by.as_deref());
        let after = self.add_show_request(
            &p.space,
            &p.issue,
            reason.as_deref(),
            requested_by.as_deref(),
        )?;
        Ok(LinearChange {
            before: None,
            after: Some(after),
        })
    }

    /// Accept and dismiss both drain the request; neither answers it twice.
    pub fn linear_show_answer(&self, id: i64) -> LsResult<LinearChange<ShowRequest>> {
        let before = self.show_request(id)?.ok_or_else(|| {
            LocalStateRejection::Missing(format!("show request {id} does not exist"))
        })?;
        if before.acknowledged_at.is_some() || !self.acknowledge_show_request(id)? {
            return Err(LocalStateRejection::ShowRequestAnswered { id }.into());
        }
        Ok(LinearChange {
            before: Some(before),
            after: self.show_request(id)?,
        })
    }

    /// R13: a `save_issue` links the calling session only when its claims
    /// resolve to a pane in a bound space, running in an unbound worktree,
    /// for an issue no other worktree holds. Otherwise it leaves a
    /// suggestion mark; any other tool records activity only.
    pub fn linear_activity_record(
        &self,
        p: &LinearActivityRecordParams,
    ) -> LsResult<LinearActivityRecordResult> {
        if let Some(issue) = p.issue.as_deref() {
            check_issue(issue)?;
        }
        let claims = clean_claims(&p.claims);
        let space = claimed_space(p.space.as_deref(), &claims);
        let activity = self.record_activity(&NewActivity {
            space: space.as_deref(),
            tool_name: &p.tool_name,
            issue: p.issue.as_deref(),
            claims: &claims,
        })?;
        let recorded = |activity| LinearActivityRecordResult {
            activity,
            outcome: LinearActivityOutcome::Recorded,
            binding: None,
            mark: None,
        };
        let (Some(issue), Some(space)) = (p.issue.as_deref(), space.as_deref()) else {
            return Ok(recorded(activity));
        };
        if !is_save_issue(&p.tool_name) {
            return Ok(recorded(activity));
        }
        let worktree = p.cwd.as_deref().and_then(|cwd| resolve_worktree(cwd).ok());
        let known_session = match (claimed_session(&claims), &claims.herdr_pane_id) {
            (Some(session), Some(_)) => self.space_binding(&session, space)?.is_some(),
            _ => false,
        };
        if let (true, Some(path)) = (known_session, worktree.as_deref()) {
            if self.worktree_binding(path)?.is_none() && self.issue_holder(issue, path)?.is_none() {
                let binding = self.linear_bind(&LinearBindParams {
                    cwd: path.to_owned(),
                    issue: issue.to_owned(),
                    space: Some(space.to_owned()),
                    claims: claims.clone(),
                    ..LinearBindParams::default()
                })?;
                return Ok(LinearActivityRecordResult {
                    activity,
                    outcome: LinearActivityOutcome::Linked,
                    binding: Some(binding),
                    mark: None,
                });
            }
        }
        let detail = sanitise_json(json!({
            "worktree_path": worktree,
            "cwd": p.cwd,
            "herdr_socket": claims.herdr_socket,
            "herdr_pane_id": claims.herdr_pane_id,
            "herdr_workspace_id": claims.herdr_workspace_id,
            "card_id": claims.card_id,
            "run_id": claims.run_id,
        }));
        let mark = self.replace_mark(&NewMark {
            space,
            issue,
            kind: MarkKind::Suggestion,
            text: None,
            detail: Some(detail),
            created_by: Some(&p.tool_name),
        })?;
        Ok(LinearActivityRecordResult {
            activity,
            outcome: LinearActivityOutcome::Suggested,
            binding: None,
            mark: Some(mark),
        })
    }

    pub fn linear_activity_list(
        &self,
        p: &LinearActivityListParams,
    ) -> crate::Result<LinearActivityListResult> {
        let limit = p
            .limit
            .unwrap_or(ACTIVITY_LIST_DEFAULT)
            .min(LINEAR_ACTIVITY_KEEP_PER_SPACE);
        Ok(LinearActivityListResult {
            activity: self.list_activity(p.space.as_deref(), limit)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_parse_refuses_a_repeated_key_at_any_depth_and_keeps_the_rest() {
        assert!(parse_json_refusing_duplicate_keys(r#"{"a": 1, "a": 2}"#)
            .unwrap_err()
            .contains("duplicate key \"a\""));
        assert!(
            parse_json_refusing_duplicate_keys(r#"[{"b": {"c": 1, "c": 1}}]"#)
                .unwrap_err()
                .contains("duplicate key \"c\"")
        );
        assert!(parse_json_refusing_duplicate_keys(r#"{"a": 1} x"#).is_err());
        assert_eq!(
            parse_json_refusing_duplicate_keys(r#"{"a": [1, -2, 1.5, "s", null, true], "b": {}}"#)
                .unwrap(),
            json!({"a": [1, -2, 1.5, "s", null, true], "b": {}})
        );
    }

    #[test]
    fn only_a_save_issue_tool_can_link() {
        assert!(is_save_issue("save_issue"));
        assert!(is_save_issue("mcp__linear__save_issue"));
        assert!(is_save_issue("mcp__claude_ai_Linear__save_issue"));
        assert!(!is_save_issue("mcp__linear__save_issue_label"));
        assert!(!is_save_issue("mcp__linear__save_comment"));
    }
}
