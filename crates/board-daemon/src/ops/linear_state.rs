//! Linear-mode local state over the socket. The units of work live in
//! `board_core::db` so the fake client runs the same code; this layer adds the
//! snapshot-cache seams and announces every successful write exactly once as
//! `local_state_changed` (KTD9).

use super::*;

use std::collections::BTreeSet;

use board_core::db::{claimed_space, clean_claims, ActivityClaims, LocalStateError, Mark};

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

/// A card listed in a reader's cached read of the space is known (KTD11).
fn snapshot_lists_issue(d: &Arc<Daemon>, space: &str, issue: &str) -> bool {
    super::linear::native::lists_issue(d, space, issue)
}

/// A reported Linear write makes the space's cached read out of date. Returns
/// whether a debounced refetch was scheduled; that refetch announces the space
/// once it lands, so the write must not announce it as well.
fn activity_recorded(d: &Arc<Daemon>, claims: &ActivityClaims, space: Option<&str>) -> bool {
    let Some(space) = space else {
        return false;
    };
    let session = board_core::paths::session_name_from_socket(claims.herdr_socket.as_deref())
        .unwrap_or_else(|| "default".into());
    super::linear::native::schedule_refetch(d, session, space.to_string())
}

pub(super) fn state_get(d: &Arc<Daemon>, p: LinearStateGetParams) -> Result<Value> {
    Ok(json!(d.store.lock().linear_state(&p.space, now_secs(d))?))
}

pub(super) fn bind(d: &Arc<Daemon>, p: LinearBindParams) -> Result<Value> {
    let space = claimed_space(p.space.as_deref(), &clean_claims(&p.claims));
    let bound = d.store.lock().linear_bind(&p).map_err(ls)?;
    announce(d, space.clone());
    announce_cleared(d, space.as_deref(), &bound.cleared_suggestions);
    Ok(json!(bound))
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

pub(super) fn mark_set(d: &Arc<Daemon>, p: LinearMarkSetParams) -> Result<Value> {
    let known = snapshot_lists_issue(d, &p.space, &p.issue);
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

pub(super) fn mark_unmark(d: &Arc<Daemon>, p: LinearMarkUnmarkParams) -> Result<Value> {
    let removed = d.store.lock().linear_mark_unmark(&p).map_err(ls)?;
    announce_spaces(d, mark_spaces(&removed.removed));
    Ok(json!(removed))
}

pub(super) fn note_set(d: &Arc<Daemon>, p: LinearNoteSetParams) -> Result<Value> {
    let known = snapshot_lists_issue(d, &p.space, &p.issue);
    let change = d.store.lock().linear_note_set(&p, known).map_err(ls)?;
    announce(d, Some(change.after.space.clone()));
    Ok(json!(change))
}

pub(super) fn note_clear(d: &Arc<Daemon>, p: LinearIdParams) -> Result<Value> {
    let change = d.store.lock().linear_note_clear(p.id).map_err(ls)?;
    announce(d, change.before.as_ref().map(|n| n.space.clone()));
    Ok(json!(change))
}

pub(super) fn show_request(d: &Arc<Daemon>, p: LinearShowRequestParams) -> Result<Value> {
    let known = snapshot_lists_issue(d, &p.space, &p.issue);
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

pub(super) fn show_withdraw(d: &Arc<Daemon>, p: LinearShowWithdrawParams) -> Result<Value> {
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

pub(super) fn activity_record(d: &Arc<Daemon>, p: LinearActivityRecordParams) -> Result<Value> {
    let result = d.store.lock().linear_activity_record(&p).map_err(ls)?;
    let space = result.activity.space.clone();
    if !activity_recorded(d, &clean_claims(&p.claims), space.as_deref()) {
        announce(d, space.clone());
    }
    announce_cleared(d, space.as_deref(), &result.cleared_suggestions);
    Ok(json!(result))
}

pub(super) fn activity_list(d: &Arc<Daemon>, p: LinearActivityListParams) -> Result<Value> {
    Ok(json!(d.store.lock().linear_activity_list(&p)?))
}
