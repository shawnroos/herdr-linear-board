//! Linear-mode local state over the socket. The units of work live in
//! `board_core::db` so the fake client runs the same code; this layer adds the
//! snapshot-cache seams and announces every successful write exactly once as
//! `local_state_changed` (KTD9).

use super::*;

use board_core::db::{claimed_space, clean_claims, LocalStateError};

use super::errors::local_state_error;

fn ls(error: LocalStateError) -> Error {
    local_state_error(error)
}

fn announce(d: &Arc<Daemon>, space: Option<String>) {
    d.emit(Event::LocalStateChanged { space });
}

/// Seam for the per-space snapshot cache (KTD11): whether the cached snapshot
/// lists `issue`. There is no cache yet, so only local state makes an issue
/// known.
fn snapshot_lists_issue(_d: &Arc<Daemon>, _space: &str, _issue: &str) -> bool {
    false
}

/// Seam for the per-space snapshot cache (KTD11): invalidate the space and
/// schedule its debounced refetch. There is no cache yet.
fn activity_recorded(_d: &Arc<Daemon>, _space: Option<&str>) {}

pub(super) fn state_get(d: &Arc<Daemon>, p: LinearStateGetParams) -> Result<Value> {
    Ok(json!(d.store.lock().linear_state(&p.space)?))
}

pub(super) fn bind(d: &Arc<Daemon>, p: LinearBindParams) -> Result<Value> {
    let space = claimed_space(p.space.as_deref(), &clean_claims(&p.claims));
    let change = d.store.lock().linear_bind(&p).map_err(ls)?;
    announce(d, space);
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
    announce(d, p.space);
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

pub(super) fn mark_clear(d: &Arc<Daemon>, p: LinearIdParams) -> Result<Value> {
    let change = d.store.lock().linear_mark_clear(p.id).map_err(ls)?;
    announce(d, change.before.as_ref().map(|m| m.space.clone()));
    Ok(json!(change))
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
    let change = d.store.lock().linear_show_request(&p, known).map_err(ls)?;
    announce(d, change.after.as_ref().map(|r| r.space.clone()));
    Ok(json!(change))
}

pub(super) fn show_answer(d: &Arc<Daemon>, p: LinearIdParams) -> Result<Value> {
    let change = d.store.lock().linear_show_answer(p.id).map_err(ls)?;
    announce(d, change.before.as_ref().map(|r| r.space.clone()));
    Ok(json!(change))
}

pub(super) fn activity_record(d: &Arc<Daemon>, p: LinearActivityRecordParams) -> Result<Value> {
    let result = d.store.lock().linear_activity_record(&p).map_err(ls)?;
    activity_recorded(d, result.activity.space.as_deref());
    announce(d, result.activity.space.clone());
    Ok(json!(result))
}

pub(super) fn activity_list(d: &Arc<Daemon>, p: LinearActivityListParams) -> Result<Value> {
    Ok(json!(d.store.lock().linear_activity_list(&p)?))
}
