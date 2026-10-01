//! Linear-mode local state over the socket. The units of work live in
//! `board_core::db` so the fake client runs the same code; this layer adds the
//! snapshot-cache seams and announces every successful write exactly once as
//! `local_state_changed`.

use super::*;

use std::collections::BTreeSet;

use board_core::db::{
    claimed_space, clean_claims, clean_owner, herdr_session, ActivityClaims, LinearOwner,
    LocalStateError, Mark,
};

use super::errors::local_state_error;

fn ls(error: LocalStateError) -> Error {
    local_state_error(error)
}

fn announce(d: &Arc<Daemon>, space: Option<String>) {
    d.emit(Event::LocalStateChanged {
        space,
        snapshot: false,
    });
}

/// Once per distinct space.
fn announce_spaces<'a>(d: &Arc<Daemon>, spaces: impl IntoIterator<Item = &'a str>) {
    let distinct: BTreeSet<&str> = spaces.into_iter().collect();
    for space in distinct {
        announce(d, Some(space.to_string()));
    }
}

fn mark_spaces(marks: &[Mark]) -> impl Iterator<Item = &str> {
    marks.iter().map(|m| m.space.as_str())
}

/// The spaces of `cleared` other than `primary`, which the caller announced
/// already. A `None` primary was announced for every space, so nothing is left.
fn announce_cleared(d: &Arc<Daemon>, primary: Option<&str>, cleared: &[Mark]) {
    if let Some(primary) = primary {
        announce_spaces(d, mark_spaces(cleared).filter(|space| *space != primary));
    }
}

/// A card listed in the caller's session's cached read of the space is known.
/// A UUID it lists is rewritten to the issue's identifier, the key
/// the board renders local state by; one it does not list is left as given.
fn resolve_known(d: &Arc<Daemon>, socket: Option<&str>, space: &str, issue: &mut String) -> bool {
    match super::linear::native::resolve_issue(d, socket, space, issue) {
        Some(identifier) => {
            *issue = identifier;
            true
        }
        None => false,
    }
}

fn owner_socket(owner: &LinearOwner) -> Option<String> {
    clean_owner(owner).herdr_socket
}

/// A reported Linear write makes the space's cached read out of date.
fn activity_recorded(d: &Arc<Daemon>, claims: &ActivityClaims, space: Option<&str>) {
    let Some(space) = space else {
        return;
    };
    let session = herdr_session(claims.herdr_socket.as_deref());
    super::linear::native::schedule_refetch(d, session, space.to_string());
}

pub(super) fn state_get(d: &Arc<Daemon>, p: LinearStateGetParams) -> Result<Value> {
    let session = herdr_session(p.herdr_socket.as_deref());
    Ok(json!(d.store.lock().linear_state(
        &session,
        &p.space,
        now_secs(d)
    )?))
}

pub(super) fn bind(d: &Arc<Daemon>, p: LinearBindParams) -> Result<Value> {
    let space = claimed_space(p.space.as_deref(), &clean_claims(&p.claims));
    let bound = d.store.lock().linear_bind(&p).map_err(ls)?;
    announce(d, space.clone());
    announce_cleared(d, space.as_deref(), &bound.cleared_suggestions);
    Ok(json!(bound))
}

pub(super) fn space_bind(d: &Arc<Daemon>, p: LinearSpaceBindParams) -> Result<Value> {
    let change = d.store.lock().linear_space_bind(&p).map_err(ls)?;
    announce(d, Some(p.space.clone()));
    Ok(json!(change))
}

pub(super) fn unbind(d: &Arc<Daemon>, p: LinearUnbindParams) -> Result<Value> {
    let space = claimed_space(p.space.as_deref(), &clean_claims(&p.claims));
    let change = d.store.lock().linear_unbind(&p).map_err(ls)?;
    announce(d, space);
    Ok(json!(change))
}

pub(super) fn grouping_get(d: &Arc<Daemon>, p: LinearGroupingGetParams) -> Result<Value> {
    Ok(json!(d.store.lock().linear_grouping_get(&p)?))
}

pub(super) fn grouping_set(d: &Arc<Daemon>, p: LinearGroupingSetParams) -> Result<Value> {
    let change = d
        .store
        .lock()
        .linear_grouping_change(&p, true)
        .map_err(ls)?;
    // A grouping space is a workspace label, which any workspace may carry,
    // so the change is announced for every space.
    announce(d, None);
    Ok(json!(change))
}

pub(super) fn grouping_preview(d: &Arc<Daemon>, p: LinearGroupingSetParams) -> Result<Value> {
    Ok(json!(d
        .store
        .lock()
        .linear_grouping_change(&p, false)
        .map_err(ls)?))
}

pub(super) fn mark_set(d: &Arc<Daemon>, mut p: LinearMarkSetParams) -> Result<Value> {
    let socket = owner_socket(&p.owner);
    let known = resolve_known(d, socket.as_deref(), &p.space, &mut p.issue);
    let change = d.store.lock().linear_mark_set(&p, known).map_err(ls)?;
    announce(d, Some(change.after.space.clone()));
    Ok(json!(change))
}

/// `{id}` is the single clear; `{ids}` the bulk clear a detail screen sends
/// for the marks it showed.
pub(super) fn mark_clear(d: &Arc<Daemon>, p: LinearMarkClearParams) -> Result<Value> {
    match p.target()? {
        MarkClearTarget::One(id) => {
            let change = d.store.lock().linear_mark_clear(id).map_err(ls)?;
            announce(d, change.before.as_ref().map(|m| m.space.clone()));
            Ok(json!(change))
        }
        MarkClearTarget::Many(ids) => {
            let removed = d.store.lock().linear_mark_clear_ids(&ids).map_err(ls)?;
            announce_spaces(d, mark_spaces(&removed.removed));
            Ok(json!(removed))
        }
    }
}

pub(super) fn mark_unmark(d: &Arc<Daemon>, mut p: LinearMarkUnmarkParams) -> Result<Value> {
    let socket = owner_socket(&p.owner);
    resolve_known(d, socket.as_deref(), &p.space, &mut p.issue);
    let removed = d.store.lock().linear_mark_unmark(&p).map_err(ls)?;
    announce_spaces(d, mark_spaces(&removed.removed));
    Ok(json!(removed))
}

pub(super) fn note_set(d: &Arc<Daemon>, mut p: LinearNoteSetParams) -> Result<Value> {
    let socket = owner_socket(&p.owner);
    let known = resolve_known(d, socket.as_deref(), &p.space, &mut p.issue);
    let change = d.store.lock().linear_note_set(&p, known).map_err(ls)?;
    announce(d, Some(change.after.space.clone()));
    Ok(json!(change))
}

pub(super) fn note_clear(d: &Arc<Daemon>, p: LinearIdParams) -> Result<Value> {
    let change = d.store.lock().linear_note_clear(p.id).map_err(ls)?;
    announce(d, change.before.as_ref().map(|n| n.space.clone()));
    Ok(json!(change))
}

pub(super) fn show_request(d: &Arc<Daemon>, mut p: LinearShowRequestParams) -> Result<Value> {
    let socket = owner_socket(&p.owner);
    let known = resolve_known(d, socket.as_deref(), &p.space, &mut p.issue);
    let change = d
        .store
        .lock()
        .linear_show_request(&p, known, now_secs(d), d.settings.show_request_ttl_secs)
        .map_err(ls)?;
    announce(d, change.after.as_ref().map(|r| r.space.clone()));
    Ok(json!(change))
}

pub(super) fn show_accept(d: &Arc<Daemon>, p: LinearIdParams) -> Result<Value> {
    let change = d
        .store
        .lock()
        .linear_show_accept(p.id, now_secs(d))
        .map_err(ls)?;
    announce(d, change.before.as_ref().map(|r| r.space.clone()));
    Ok(json!(change))
}

pub(super) fn show_dismiss(d: &Arc<Daemon>, p: LinearIdParams) -> Result<Value> {
    let change = d
        .store
        .lock()
        .linear_show_dismiss(p.id, now_secs(d))
        .map_err(ls)?;
    announce(d, change.before.as_ref().map(|r| r.space.clone()));
    Ok(json!(change))
}

pub(super) fn show_withdraw(d: &Arc<Daemon>, mut p: LinearShowWithdrawParams) -> Result<Value> {
    let socket = owner_socket(&p.owner);
    resolve_known(d, socket.as_deref(), &p.space, &mut p.issue);
    let change = d
        .store
        .lock()
        .linear_show_withdraw(&p, now_secs(d))
        .map_err(ls)?;
    announce(d, change.before.as_ref().map(|r| r.space.clone()));
    Ok(json!(change))
}

/// The SQLite half plus the column from the snapshot already cached for the
/// space; never a Linear or herdr call.
pub(super) fn session_get(d: &Arc<Daemon>, p: LinearSessionGetParams) -> Result<Value> {
    let mut result = d.store.lock().linear_session_get(&p, now_secs(d))?;
    if let Some(binding) = &result.binding {
        result.column = super::linear::native::cached_column(
            d,
            p.herdr_socket.as_deref(),
            &p.space,
            &binding.issue,
        )?;
    }
    Ok(json!(result))
}

pub(super) fn activity_record(d: &Arc<Daemon>, mut p: LinearActivityRecordParams) -> Result<Value> {
    let claims = clean_claims(&p.claims);
    if let (Some(space), Some(issue)) = (claimed_space(p.space.as_deref(), &claims), &mut p.issue) {
        resolve_known(d, claims.herdr_socket.as_deref(), &space, issue);
    }
    let result = d.store.lock().linear_activity_record(&p).map_err(ls)?;
    let space = result.activity.space.clone();
    // The local write is announced now; the refetch announces again with the
    // snapshot flag once Linear answers, which can take seconds.
    activity_recorded(d, &claims, space.as_deref());
    announce(d, space.clone());
    announce_cleared(d, space.as_deref(), &result.cleared_suggestions);
    Ok(json!(result))
}

pub(super) fn activity_list(d: &Arc<Daemon>, p: LinearActivityListParams) -> Result<Value> {
    Ok(json!(d.store.lock().linear_activity_list(&p)?))
}
