//! `linear.import`: copies the work plugin's `~/.claude/work` store into
//! Linear-mode local state. Insert-only by natural key, so a second run adds
//! only what the plugin wrote since and never overwrites a board edit. The
//! JSON store is only ever read.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use board_core::db::{
    is_issue_identifier, is_scope_key, parse_json_refusing_duplicate_keys, Db, GroupingConfig,
    GroupingMapping, SessionScope, SpaceBinding, SpaceGrouping, WorktreeBinding,
    WorktreeBindingState,
};
use board_core::protocol::{
    LinearImportIgnored, LinearImportItem, LinearImportKind, LinearImportResult,
};
use board_core::text::{sanitise_json, strip_control_and_format};
use board_core::{Error, Result};
use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;
use serde_json::{Map, Value};

const STORE_ENV: &str = "HERDR_LINEAR_STORE_DIR";
/// The plugin keys a record written outside any named herdr session to the
/// flat `workspaces/` directory; `default` is the session name the plugin and
/// `linear.bind` give that case.
const FLAT_SESSION: &str = "default";
const MAX_RECORD_BYTES: u64 = 1 << 20;

const RETIRING_FIELDS: [&str; 7] = [
    "proposal",
    "consent",
    "consent_proposal",
    "pending_consent",
    "pending_placement",
    "declined",
    "pending_judgment",
];
const MAPPED_BINDING_FIELDS: [&str; 10] = [
    "version",
    "worktree_path",
    "state",
    "issue_identifier",
    "updated_at",
    "branch_at_confirmation",
    "tab",
    "display_name",
    "team_ids",
    "view",
];

pub struct ImportOutcome {
    pub result: LinearImportResult,
    pub changed_spaces: BTreeSet<Option<String>>,
}

pub fn store_dir_from_env() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(STORE_ENV).filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|home| PathBuf::from(home).join(".claude/work"))
        .ok_or_else(|| {
            Error::BadRequest(format!(
                "boardd cannot find the work store: neither {STORE_ENV} nor HOME is set in its environment"
            ))
        })
}

pub fn import_work_store(db: &Db, store: &Path, dry_run: bool) -> Result<ImportOutcome> {
    let mut run = Import {
        db,
        root: store,
        dry_run,
        euid: current_euid(),
        result: LinearImportResult {
            store_dir: store.to_string_lossy().into_owned(),
            dry_run,
            ..LinearImportResult::default()
        },
        changed: BTreeSet::new(),
        planned: Planned::default(),
    };
    match std::fs::metadata(store) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(run.finish()),
        Err(e) => {
            return Err(Error::BadRequest(format!(
                "the work store {} cannot be read: {e}",
                store.display()
            )))
        }
        Ok(meta) if !meta.is_dir() => {
            return Err(Error::BadRequest(format!(
                "the work store {} is not a directory",
                store.display()
            )))
        }
        Ok(_) => run.result.present = true,
    }

    let mut handled = BTreeSet::new();
    for (name, is_dir) in entries(store)? {
        match (name.as_str(), is_dir) {
            ("board.json", false) => {
                handled.insert(name);
            }
            ("workspaces" | "bindings" | "contexts" | "scopes", true) => {
                handled.insert(name);
            }
            _ => run.ignore(name.clone(), retired_reason(&name, is_dir)),
        }
    }
    if handled.contains("board.json") {
        run.board_config()?;
    }
    if handled.contains("workspaces") {
        run.workspaces()?;
    }
    if handled.contains("bindings") {
        run.bindings()?;
    }
    if handled.contains("contexts") {
        run.contexts()?;
    }
    if handled.contains("scopes") {
        run.scopes()?;
    }
    Ok(run.finish())
}

fn current_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn retired_reason(name: &str, is_dir: bool) -> String {
    match name {
        "board" | "sessions" => "herdr pane-sync state; the board does not move panes".into(),
        "layouts" => "herdr layout journals; the board does not move panes".into(),
        "descriptions" => "description backups from the retiring Linear write path".into(),
        "shadow.log" => "the retiring Linear write path's log".into(),
        "write-enabled" => "the retiring Linear write path's switch".into(),
        "board.json" | "workspaces" | "bindings" | "contexts" | "scopes" => {
            if is_dir {
                "expected a file, found a directory".into()
            } else {
                "expected a directory, found a file".into()
            }
        }
        _ => "not part of the work store".into(),
    }
}

/// Sorted `(name, is_dir)` pairs. A symlink is neither followed nor listed as
/// a directory.
fn entries(dir: &Path) -> Result<Vec<(String, bool)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let is_dir = entry.file_type()?.is_dir();
        out.push((entry.file_name().to_string_lossy().into_owned(), is_dir));
    }
    out.sort();
    Ok(out)
}

#[derive(Default)]
struct Planned {
    global: bool,
    spaces: BTreeSet<String>,
    space_bindings: BTreeSet<(String, String)>,
    worktrees: BTreeMap<String, String>,
    sessions: BTreeSet<String>,
    repos: BTreeSet<(String, String)>,
}

struct Import<'a> {
    db: &'a Db,
    root: &'a Path,
    dry_run: bool,
    euid: u32,
    result: LinearImportResult,
    changed: BTreeSet<Option<String>>,
    planned: Planned,
}

struct Skip {
    key: Option<String>,
    reason: String,
}

impl Skip {
    fn new(reason: impl fmt::Display) -> Skip {
        Skip {
            key: None,
            reason: reason.to_string(),
        }
    }

    fn keyed(key: &str, reason: impl fmt::Display) -> Skip {
        Skip {
            key: Some(key.to_owned()),
            reason: reason.to_string(),
        }
    }
}

const ALREADY: &str = "already in the board; the import never overwrites a row";

impl Import<'_> {
    fn finish(self) -> ImportOutcome {
        ImportOutcome {
            result: self.result,
            changed_spaces: if self.dry_run {
                BTreeSet::new()
            } else {
                self.changed
            },
        }
    }

    fn ignore(&mut self, path: String, reason: impl Into<String>) {
        self.result.ignored.push(LinearImportIgnored {
            path,
            reason: reason.into(),
        });
    }

    fn skip(&mut self, kind: LinearImportKind, source: &str, skip: Skip) {
        self.result.skipped.push(LinearImportItem {
            kind,
            key: skip.key.unwrap_or_else(|| source.to_owned()),
            source: source.to_owned(),
            reason: Some(skip.reason),
            dropped: Vec::new(),
        });
    }

    /// Records one insert, performing it unless this is a dry run. A store
    /// refusal turns the insert into a skip.
    fn insert(
        &mut self,
        kind: LinearImportKind,
        source: &str,
        key: String,
        dropped: Vec<String>,
        space: Option<String>,
        write: impl FnOnce(&Db) -> Result<()>,
    ) -> bool {
        if !self.dry_run {
            if let Err(e) = write(self.db) {
                self.skip(kind, source, Skip::keyed(&key, reason_of(e)));
                return false;
            }
            self.changed.insert(space);
        }
        self.result.imported.push(LinearImportItem {
            kind,
            key,
            source: source.to_owned(),
            reason: None,
            dropped,
        });
        true
    }

    /// The plugin's trust rule: a record not owned by boardd's user, or
    /// writable by group or others, is refused. Opened without following a
    /// symlink and without blocking on a FIFO, then checked on the open file.
    fn read_record(&self, rel: &str) -> std::result::Result<(String, Value), Skip> {
        let path = self.root.join(rel);
        let mut file = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|e| {
                if e.raw_os_error() == Some(libc::ELOOP) {
                    Skip::new("is a symlink, not a regular file")
                } else {
                    Skip::new(format!("cannot be opened: {e}"))
                }
            })?;
        let meta = file
            .metadata()
            .map_err(|e| Skip::new(format!("cannot be read: {e}")))?;
        if !meta.is_file() {
            return Err(Skip::new("is not a regular file"));
        }
        if meta.uid() != self.euid {
            return Err(Skip::new(format!(
                "is owned by uid {}, not by boardd's user (uid {}); the plugin's trust rule refuses it",
                meta.uid(),
                self.euid
            )));
        }
        if meta.mode() & 0o020 != 0 {
            return Err(Skip::new(
                "is group-writable; the plugin's trust rule refuses it",
            ));
        }
        if meta.mode() & 0o002 != 0 {
            return Err(Skip::new(
                "is writable by others; the plugin's trust rule refuses it",
            ));
        }
        if meta.len() > MAX_RECORD_BYTES {
            return Err(Skip::new("is larger than 1 MiB"));
        }
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|e| Skip::new(format!("cannot be read: {e}")))?;
        let value = parse_json_refusing_duplicate_keys(&text)
            .map_err(|e| Skip::new(format!("is not valid JSON: {e}")))?;
        match value.get("version").and_then(Value::as_u64) {
            Some(1) if value.is_object() => Ok((text, value)),
            Some(v) if v > 1 => Err(Skip::new(format!(
                "is version {v}, newer than the version 1 this board reads"
            ))),
            _ => Err(Skip::new("is not a version 1 record")),
        }
    }

    fn json_files(&mut self, dir: &str) -> Result<Vec<String>> {
        let mut files = Vec::new();
        for (name, is_dir) in entries(&self.root.join(dir))? {
            let rel = format!("{dir}/{name}");
            if !is_dir && name.ends_with(".json") && !name.starts_with('.') {
                files.push(rel);
            } else {
                self.ignore(rel, "not a record file");
            }
        }
        Ok(files)
    }

    fn board_config(&mut self) -> Result<()> {
        const KIND: LinearImportKind = LinearImportKind::Grouping;
        let source = "board.json";
        let config = match self
            .read_record(source)
            .and_then(|(text, _)| parse_board(&text))
        {
            Ok(config) => config,
            Err(skip) => {
                self.skip(KIND, source, skip);
                return Ok(());
            }
        };
        let existing = self.db.grouping_config()?;
        if existing.is_some() || self.planned.global {
            self.skip(KIND, source, Skip::keyed("global", ALREADY));
        } else {
            let global = config.global.clone();
            if self.insert(KIND, source, "global".into(), vec![], None, |db| {
                db.set_grouping_global(&global)
            }) {
                self.planned.global = true;
            }
        }
        let existing: BTreeSet<String> = existing
            .map(|c| c.spaces.into_iter().map(|s| s.space).collect())
            .unwrap_or_default();
        for entry in config.spaces {
            let key = format!("space {}", entry.space);
            if existing.contains(&entry.space) || self.planned.spaces.contains(&entry.space) {
                self.skip(KIND, source, Skip::keyed(&key, ALREADY));
                continue;
            }
            let space = entry.space.clone();
            if self.insert(KIND, source, key, vec![], Some(space.clone()), |db| {
                db.set_grouping_space(&entry.space, &entry.mapping)
            }) {
                self.planned.spaces.insert(space);
            }
        }
        Ok(())
    }

    fn workspaces(&mut self) -> Result<()> {
        let mut keyed = Vec::new();
        let mut flat = Vec::new();
        for (name, is_dir) in entries(&self.root.join("workspaces"))? {
            let rel = format!("workspaces/{name}");
            if is_dir {
                for (file, file_is_dir) in entries(&self.root.join(&rel))? {
                    let file_rel = format!("{rel}/{file}");
                    match file.strip_suffix(".json") {
                        Some(ws) if !file_is_dir && !file.starts_with('.') => {
                            keyed.push((name.clone(), ws.to_owned(), file_rel));
                        }
                        _ => self.ignore(file_rel, "not a record file"),
                    }
                }
            } else {
                match name.strip_suffix(".json") {
                    Some(ws) if !name.starts_with('.') => flat.push((ws.to_owned(), rel)),
                    _ => self.ignore(rel, "not a record file"),
                }
            }
        }
        let keyed_spaces: BTreeSet<String> = keyed.iter().map(|(_, ws, _)| ws.clone()).collect();
        for (session, ws, rel) in keyed {
            self.space_record(&session, &ws, &rel)?;
        }
        for (ws, rel) in flat {
            if keyed_spaces.contains(&ws) {
                self.skip(
                    LinearImportKind::SpaceBinding,
                    &rel,
                    Skip::keyed(
                        &format!("{FLAT_SESSION}/{ws}"),
                        format!("superseded by a session-keyed record for workspace {ws}"),
                    ),
                );
            } else {
                self.space_record(FLAT_SESSION, &ws, &rel)?;
            }
        }
        Ok(())
    }

    fn space_record(&mut self, session: &str, ws: &str, rel: &str) -> Result<()> {
        const KIND: LinearImportKind = LinearImportKind::SpaceBinding;
        let key = format!("{session}/{ws}");
        let parsed = self.read_record(rel).and_then(|(_, record)| {
            if !text_ok(session) || !text_ok(ws) {
                return Err(Skip::new(
                    "its session or workspace name is not usable text",
                ));
            }
            expect_path(&record, &format!("workspace:{ws}"))?;
            require_state(&record, &["bound"], "space record")?;
            Ok((
                SpaceBinding {
                    herdr_session: session.to_owned(),
                    space: ws.to_owned(),
                    project_id: required_text(&record, "issue_identifier")?,
                    display_name: optional_text(&record, "display_name")?,
                    team_ids: team_ids(&record)?,
                    view: view(&record)?,
                },
                record,
            ))
        });
        let (binding, record) = match parsed {
            Ok(binding) => binding,
            Err(skip) => {
                self.skip(
                    KIND,
                    rel,
                    Skip {
                        key: Some(key),
                        ..skip
                    },
                );
                return Ok(());
            }
        };
        let natural = (session.to_owned(), ws.to_owned());
        if self.db.space_binding(session, ws)?.is_some()
            || self.planned.space_bindings.contains(&natural)
        {
            self.skip(KIND, rel, Skip::keyed(&key, ALREADY));
            return Ok(());
        }
        if self.insert(
            KIND,
            rel,
            key,
            dropped_fields(&record),
            Some(ws.to_owned()),
            |db| db.set_space_binding(&binding),
        ) {
            self.planned.space_bindings.insert(natural);
        }
        Ok(())
    }

    fn bindings(&mut self) -> Result<()> {
        const KIND: LinearImportKind = LinearImportKind::WorktreeBinding;
        for rel in self.json_files("bindings")? {
            let record = match self.read_record(&rel) {
                Ok((_, record)) => record,
                Err(skip) => {
                    self.skip(KIND, &rel, skip);
                    continue;
                }
            };
            let binding = match worktree_binding(&record) {
                Ok(binding) => binding,
                Err(skip) => {
                    let key = record
                        .get("worktree_path")
                        .and_then(Value::as_str)
                        .filter(|p| text_ok(p))
                        .map(str::to_owned);
                    self.skip(
                        KIND,
                        &rel,
                        Skip {
                            key: key.or(skip.key),
                            ..skip
                        },
                    );
                    continue;
                }
            };
            let key = binding.worktree_path.clone();
            if self.db.worktree_binding(&key)?.is_some()
                || self.planned.worktrees.contains_key(&key)
            {
                self.skip(KIND, &rel, Skip::keyed(&key, ALREADY));
                continue;
            }
            if let Some(holder) = self.issue_holder(&binding.issue, &key)? {
                self.skip(
                    KIND,
                    &rel,
                    Skip::keyed(
                        &key,
                        format!("issue {} is already bound to {holder}", binding.issue),
                    ),
                );
                continue;
            }
            let issue = binding.issue.clone();
            if self.insert(
                KIND,
                &rel,
                key.clone(),
                dropped_fields(&record),
                None,
                |db| db.set_worktree_binding(&binding),
            ) {
                self.planned.worktrees.insert(key, issue);
            }
        }
        Ok(())
    }

    fn issue_holder(&self, issue: &str, path: &str) -> Result<Option<String>> {
        if let Some((holder, _)) = self
            .planned
            .worktrees
            .iter()
            .find(|(p, i)| i.as_str() == issue && p.as_str() != path)
        {
            return Ok(Some(holder.clone()));
        }
        Ok(self
            .db
            .list_worktree_bindings()?
            .into_iter()
            .find(|b| b.issue == issue && b.worktree_path != path)
            .map(|b| b.worktree_path))
    }

    fn contexts(&mut self) -> Result<()> {
        const KIND: LinearImportKind = LinearImportKind::SessionScope;
        for rel in self.json_files("contexts")? {
            let name = rel.trim_start_matches("contexts/");
            let Some(session) = name
                .strip_prefix("session-")
                .and_then(|n| n.strip_suffix(".json"))
                .filter(|s| text_ok(s))
                .map(str::to_owned)
            else {
                self.ignore(rel, "not a session record");
                continue;
            };
            let parsed = self.read_record(&rel).and_then(|(_, record)| {
                expect_path(&record, &format!("session:{session}"))?;
                require_state(&record, &["bound"], "session record")?;
                Ok((
                    SessionScope {
                        session_id: session.clone(),
                        team_id: required_text(&record, "issue_identifier")?,
                        team_key: optional_text(&record, "display_name")?,
                    },
                    record,
                ))
            });
            let (scope, record) = match parsed {
                Ok(parsed) => parsed,
                Err(skip) => {
                    self.skip(
                        KIND,
                        &rel,
                        Skip {
                            key: Some(session),
                            ..skip
                        },
                    );
                    continue;
                }
            };
            if self.db.session_scope(&session)?.is_some()
                || self.planned.sessions.contains(&session)
            {
                self.skip(KIND, &rel, Skip::keyed(&session, ALREADY));
                continue;
            }
            if self.insert(
                KIND,
                &rel,
                session.clone(),
                dropped_fields(&record),
                None,
                |db| db.set_session_scope(&scope),
            ) {
                self.planned.sessions.insert(session);
            }
        }
        Ok(())
    }

    fn scopes(&mut self) -> Result<()> {
        const KIND: LinearImportKind = LinearImportKind::ScopeRepo;
        for rel in self.json_files("scopes")? {
            let scope_key = rel
                .trim_start_matches("scopes/")
                .trim_end_matches(".json")
                .to_owned();
            let parsed = self.read_record(&rel).and_then(|(_, record)| {
                if !is_scope_key(&scope_key) {
                    return Err(Skip::new(
                        "its name is not a scope key (project-<id>.team-<id>, team-<id> or project-<id>)",
                    ));
                }
                record
                    .get("repositories")
                    .and_then(Value::as_array)
                    .and_then(|repos| {
                        repos
                            .iter()
                            .map(|r| r.as_str().map(str::to_owned))
                            .collect::<Option<Vec<_>>>()
                    })
                    .ok_or_else(|| Skip::new("repositories is not a list of paths"))
            });
            let repos = match parsed {
                Ok(repos) => repos,
                Err(skip) => {
                    self.skip(
                        KIND,
                        &rel,
                        Skip {
                            key: Some(scope_key),
                            ..skip
                        },
                    );
                    continue;
                }
            };
            let existing = self.db.scope_repos(&scope_key)?;
            for repo in repos {
                let key = format!("{scope_key} {repo}");
                if !repo.starts_with('/') || !text_ok(&repo) {
                    self.skip(KIND, &rel, Skip::keyed(&key, "is not an absolute path"));
                    continue;
                }
                let natural = (scope_key.clone(), repo.clone());
                if existing.contains(&repo) || self.planned.repos.contains(&natural) {
                    self.skip(KIND, &rel, Skip::keyed(&key, ALREADY));
                    continue;
                }
                if self.insert(KIND, &rel, key, vec![], None, |db| {
                    db.add_scope_repo(&scope_key, &repo)
                }) {
                    self.planned.repos.insert(natural);
                }
            }
        }
        Ok(())
    }
}

fn reason_of(error: Error) -> String {
    match error {
        Error::BadRequest(message) => message,
        other => other.to_string(),
    }
}

fn text_ok(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_control)
}

fn expect_path(record: &Value, expected: &str) -> std::result::Result<(), Skip> {
    match record.get("worktree_path").and_then(Value::as_str) {
        Some(path) if path == expected => Ok(()),
        Some(path) => Err(Skip::new(format!(
            "names {path:?}, which does not match its file name ({expected})"
        ))),
        None => Err(Skip::new("has no worktree_path")),
    }
}

fn require_state<'a>(
    record: &'a Value,
    allowed: &[&str],
    what: &str,
) -> std::result::Result<&'a str, Skip> {
    match record.get("state").and_then(Value::as_str) {
        Some(state) if allowed.contains(&state) => Ok(state),
        Some(state) => Err(Skip::new(format!(
            "is in state {state:?}; a {what} imports only when {}",
            allowed.join(", ")
        ))),
        None => Err(Skip::new("has no state")),
    }
}

/// Plugin records store "no value" as an empty string.
fn optional_text(record: &Value, field: &str) -> std::result::Result<Option<String>, Skip> {
    match record.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = strip_control_and_format(text);
            Ok((!text.is_empty()).then_some(text))
        }
        Some(_) => Err(Skip::new(format!("{field} is not text"))),
    }
}

fn required_text(record: &Value, field: &str) -> std::result::Result<String, Skip> {
    optional_text(record, field)?.ok_or_else(|| Skip::new(format!("has no {field}")))
}

fn team_ids(record: &Value) -> std::result::Result<Vec<String>, Skip> {
    match record.get("team_ids") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(ids)) => ids
            .iter()
            .map(|id| {
                id.as_str()
                    .map(strip_control_and_format)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| Skip::new("team_ids holds something other than a team id"))
            })
            .collect(),
        Some(_) => Err(Skip::new("team_ids is not a list")),
    }
}

fn view(record: &Value) -> std::result::Result<Option<Value>, Skip> {
    match record.get("view") {
        None | Some(Value::Null) => Ok(None),
        Some(view @ Value::Object(_)) => Ok(Some(sanitise_json(view.clone()))),
        Some(_) => Err(Skip::new("view is not an object")),
    }
}

fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

fn dropped_fields(record: &Value) -> Vec<String> {
    RETIRING_FIELDS
        .iter()
        .filter(|field| record.get(**field).is_some_and(|v| !is_empty_value(v)))
        .map(|field| (*field).to_owned())
        .collect()
}

fn worktree_binding(record: &Value) -> std::result::Result<WorktreeBinding, Skip> {
    let path = match record.get("worktree_path").and_then(Value::as_str) {
        Some(path) if path.starts_with('/') && text_ok(path) => path.to_owned(),
        _ => return Err(Skip::new("worktree_path is not an absolute path")),
    };
    let state = match require_state(record, &["bound", "misplaced", "stale"], "binding")? {
        "bound" => WorktreeBindingState::Bound,
        "misplaced" => WorktreeBindingState::Misplaced,
        _ => WorktreeBindingState::Stale,
    };
    let issue = required_text(record, "issue_identifier")?;
    if !is_issue_identifier(&issue) {
        return Err(Skip::new(format!(
            "issue {issue:?} is neither a Linear issue key nor an issue UUID"
        )));
    }
    let carried: Map<String, Value> = record
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, value)| {
            !MAPPED_BINDING_FIELDS.contains(&key.as_str())
                && !RETIRING_FIELDS.contains(&key.as_str())
                && !value.is_null()
        })
        .map(|(key, value)| (key.clone(), sanitise_json(value.clone())))
        .collect();
    Ok(WorktreeBinding {
        worktree_path: path,
        issue,
        state,
        branch: optional_text(record, "branch_at_confirmation")?,
        tab: optional_text(record, "tab")?,
        display_name: optional_text(record, "display_name")?,
        team_ids: team_ids(record)?,
        view: view(record)?,
        carried,
    })
}

/// `board.json` as the plugin writes it: `spaces` is an object whose key
/// order is the configuration order, so it is read straight from the text.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoardJson {
    #[serde(rename = "version")]
    _version: u64,
    global: GroupingMapping,
    #[serde(default)]
    spaces: OrderedSpaces,
}

#[derive(Default)]
struct OrderedSpaces(Vec<(String, GroupingMapping)>);

impl<'de> Deserialize<'de> for OrderedSpaces {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct SpacesVisitor;
        impl<'de> Visitor<'de> for SpacesVisitor {
            type Value = OrderedSpaces;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object of space mappings")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<OrderedSpaces, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = map.next_entry::<String, GroupingMapping>()? {
                    out.push(entry);
                }
                Ok(OrderedSpaces(out))
            }
        }
        deserializer.deserialize_map(SpacesVisitor)
    }
}

/// The raw text has already passed the duplicate-key check.
fn parse_board(text: &str) -> std::result::Result<GroupingConfig, Skip> {
    let board: BoardJson = serde_json::from_str(text)
        .map_err(|e| Skip::keyed("global", format!("is not a board config: {e}")))?;
    let clean = |mapping: GroupingMapping| -> std::result::Result<GroupingMapping, Skip> {
        let value = serde_json::to_value(mapping).map_err(|e| Skip::new(e.to_string()))?;
        serde_json::from_value(sanitise_json(value)).map_err(|e| Skip::new(e.to_string()))
    };
    let mut spaces = Vec::new();
    for (space, mapping) in board.spaces.0 {
        spaces.push(SpaceGrouping {
            space: strip_control_and_format(&space),
            mapping: clean(mapping)?,
        });
    }
    let config = GroupingConfig {
        global: clean(board.global)?,
        spaces,
    };
    config
        .validate()
        .map_err(|e| Skip::keyed("global", reason_of(e)))?;
    Ok(config)
}
