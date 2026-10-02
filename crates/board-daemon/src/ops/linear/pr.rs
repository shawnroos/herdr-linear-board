//! `gh` data for the PRs on Linear boards: the snapshot merge, the debounced
//! enrichment pass, the periodic re-read, and `linear.pr`.

use std::collections::BTreeSet;

use crate::gh::{read_all, GhError, PrRef};
use crate::linear::SpaceKey;

use super::*;

const DETAIL_FRESH_SECS: i64 = 60;

pub(in crate::ops) fn enrich(
    d: &Arc<Daemon>,
    key: SpaceKey,
    doc: &mut LinearSnapshot,
    gh_host: Option<&str>,
) {
    let shown: Vec<(String, PrRef)> = doc
        .issues
        .iter()
        .filter_map(|(id, issue)| {
            Some((id.clone(), PrRef::parse(&issue.pr.as_ref()?.url, gh_host)?))
        })
        .collect();
    let queued = {
        let mut state = d.gh.state();
        let prs = shown.iter().map(|(_, pr)| pr.clone()).collect();
        let queued = state.observe(key, prs, now_secs(d));
        for (id, pr) in &shown {
            if let Some(card) = doc.issues.get_mut(id).and_then(|i| i.pr.as_mut()) {
                state.merge_into(pr, card);
            }
        }
        doc.gh = Some(state.source());
        queued
    };
    if queued {
        schedule_pass(d);
    }
}

pub(crate) fn schedule_pass(d: &Arc<Daemon>) {
    let worker = d.clone();
    d.gh.pass.schedule(
        crate::gh::pass_key(),
        d.gh.debounce(),
        move || run_pass(&worker),
        || {},
    );
}

/// The snapshot flag: the next plain snapshot already carries the new data.
fn run_pass(d: &Arc<Daemon>) {
    if d.is_shutdown() {
        return;
    }
    let prs = d.gh.state().take_queue();
    if prs.is_empty() {
        return;
    }
    let before = d.gh.state().source();
    let read = std::sync::atomic::AtomicBool::new(false);
    read_all(&d.gh, prs.clone(), |pr, result| {
        if !matches!(result, Err(GhError::Unavailable(_))) {
            read.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        d.gh.state().record(pr, &result, now_secs(d));
    });
    if read.into_inner() || d.gh.state().source() != before {
        announce(d, &prs);
    }
}

fn announce(d: &Arc<Daemon>, prs: &[PrRef]) {
    if d.is_shutdown() {
        return;
    }
    let spaces: BTreeSet<String> = d.gh.state().spaces_showing(prs);
    for space in spaces {
        d.emit(Event::LocalStateChanged {
            space: Some(space),
            snapshot: true,
        });
    }
}

pub(crate) fn tick(d: &Arc<Daemon>, now: i64) {
    if !d.subscribed() {
        return;
    }
    if d.gh.state().queue_due(now) {
        schedule_pass(d);
    }
}

pub(in crate::ops) fn pr(d: &Arc<Daemon>, p: LinearPrParams) -> Result<LinearPrDocument> {
    let host = d.gh.host();
    let pr = PrRef::parse(p.url.trim(), host.as_deref()).ok_or_else(|| {
        Error::BadRequest(
            "linear.pr requires a https://github.com/<owner>/<repo>/pull/<number> URL \
             (or the configured GH_HOST)"
                .into(),
        )
    })?;
    if !p.force {
        if let Some(doc) = d.gh.state().document(&pr, now_secs(d), DETAIL_FRESH_SECS) {
            return Ok(doc);
        }
    }
    let result = d.gh.fetch(&pr);
    let doc = {
        let mut state = d.gh.state();
        state.record(&pr, &result, now_secs(d));
        state.current_document(&pr)
    };
    match result {
        Err(GhError::Unavailable(message)) => Err(Error::GhUnavailable(message)),
        _ => {
            announce(d, std::slice::from_ref(&pr));
            Ok(doc.unwrap_or_default())
        }
    }
}
