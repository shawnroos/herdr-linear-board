//! boardd's `gh` reads of the pull requests on Linear boards: the client,
//! the cache keyed by PR, and the decision of which PRs to read again.

mod client;
pub mod parse;

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use board_core::protocol::{
    LinearCardPr, LinearGhSource, LinearPrDetail, LinearPrDocument, CI_PENDING, GH_OK,
    GH_UNAVAILABLE, GH_UNKNOWN,
};

pub use client::{GhClient, GhConfig, GhError, Output, MAX_OUTPUT_BYTES};

use crate::linear::{Debouncer, SpaceKey};
use crate::sync::locked;

/// Chosen, not measured: a PR with pending checks is read again after
/// two minutes, any other after ten, and only while a board is subscribed.
pub const PENDING_REFRESH_SECS: i64 = 120;
pub const STALE_AFTER_SECS: i64 = 600;
pub const PASS_CONCURRENCY: usize = 4;
const PASS_DEBOUNCE: Duration = Duration::from_millis(500);

pub const GITHUB_HOST: &str = "github.com";

/// A pull request named by a URL that passed validation. Only these parts
/// ever reach `gh`'s argv, never the URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrRef {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

fn name_ok(part: &str) -> bool {
    !matches!(part, "" | "." | "..")
        && !part.starts_with('-')
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

pub fn host_ok(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with(['-', '.'])
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

impl PrRef {
    /// `https://<host>/<owner>/<repo>/pull/<n>` with host `github.com` or
    /// `extra_host`, and nothing else, so a commit or file link under a PR is
    /// not one. Linear attachment URLs are written by anyone in the workspace.
    pub fn parse(url: &str, extra_host: Option<&str>) -> Option<PrRef> {
        let pr = PrRef::shape(url)?;
        let allowed = pr.host == GITHUB_HOST
            || extra_host
                .is_some_and(|extra| host_ok(extra) && extra.eq_ignore_ascii_case(&pr.host));
        allowed.then_some(pr)
    }

    /// A pull request URL on any host: what [`PrRef::parse`] accepts before it
    /// checks the host. Never passed to `gh`.
    pub fn shape(url: &str) -> Option<PrRef> {
        let rest = url.strip_prefix("https://")?;
        let (host, path) = rest.split_once('/')?;
        let host = host.to_ascii_lowercase();
        if !host_ok(&host) {
            return None;
        }
        let path = path.split(['?', '#']).next()?.trim_end_matches('/');
        let [owner, repo, "pull", number] = path.split('/').collect::<Vec<_>>()[..] else {
            return None;
        };
        if !(name_ok(owner)
            && name_ok(repo)
            && (1..=18).contains(&number.len())
            && number.chars().all(|c| c.is_ascii_digit()))
        {
            return None;
        }
        Some(PrRef {
            host,
            owner: owner.to_string(),
            repo: repo.to_string(),
            number: number.parse().ok()?,
        })
    }

    pub fn repo(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// `gh -R`'s `HOST/OWNER/REPO`, explicit so `GH_HOST` cannot redirect a
    /// `github.com` PR.
    pub fn repo_arg(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.repo)
    }

    pub fn url(&self) -> String {
        format!(
            "https://{}/{}/{}/pull/{}",
            self.host, self.owner, self.repo, self.number
        )
    }
}

#[derive(Debug, Clone, Default)]
struct Entry {
    /// The last good read; kept when a later read fails.
    detail: Option<LinearPrDetail>,
    error: Option<String>,
    fetched_at: i64,
}

impl Entry {
    fn pending(&self) -> bool {
        self.detail
            .as_ref()
            .and_then(|d| d.ci.as_ref())
            .is_some_and(|ci| ci.state == CI_PENDING)
    }
}

#[derive(Debug, Default)]
pub struct GhState {
    entries: HashMap<PrRef, Entry>,
    boards: HashMap<SpaceKey, BTreeSet<PrRef>>,
    queue: BTreeSet<PrRef>,
    unavailable: Option<(String, i64)>,
    ever_read: bool,
}

impl GhState {
    pub fn observe(&mut self, space: SpaceKey, prs: BTreeSet<PrRef>, now: i64) -> bool {
        if self.backing_off(now) {
            self.boards.insert(space, prs);
            return false;
        }
        let before = self.boards.get(&space);
        let new: Vec<PrRef> = prs
            .iter()
            .filter(|pr| before.is_none_or(|b| !b.contains(*pr)) || !self.entries.contains_key(*pr))
            .cloned()
            .collect();
        self.boards.insert(space, prs);
        let queued = !new.is_empty();
        self.queue.extend(new);
        queued
    }

    // A pass that finds `gh` missing still announces, and the snapshot that
    // answers it must not queue the same PRs straight back.
    fn backing_off(&self, now: i64) -> bool {
        self.unavailable
            .as_ref()
            .is_some_and(|(_, at)| now - at < STALE_AFTER_SECS)
    }

    pub fn queue_due(&mut self, now: i64) -> bool {
        if self.backing_off(now) {
            return false;
        }
        let on_board: BTreeSet<&PrRef> = self.boards.values().flatten().collect();
        let due: Vec<PrRef> = on_board
            .into_iter()
            .filter(|pr| match self.entries.get(*pr) {
                None => true,
                Some(entry) => {
                    let age = now - entry.fetched_at;
                    (entry.pending() && age >= PENDING_REFRESH_SECS) || age >= STALE_AFTER_SECS
                }
            })
            .cloned()
            .collect();
        let queued = !due.is_empty();
        self.queue.extend(due);
        queued
    }

    pub fn take_queue(&mut self) -> Vec<PrRef> {
        std::mem::take(&mut self.queue).into_iter().collect()
    }

    pub fn record(&mut self, pr: &PrRef, result: &Result<LinearPrDetail, GhError>, now: i64) {
        match result {
            Ok(detail) => {
                self.unavailable = None;
                self.ever_read = true;
                self.entries.insert(
                    pr.clone(),
                    Entry {
                        detail: Some(detail.clone()),
                        error: None,
                        fetched_at: now,
                    },
                );
            }
            Err(GhError::Unavailable(message)) => {
                self.unavailable = Some((message.clone(), now));
            }
            Err(error) => {
                self.ever_read = true;
                let entry = self.entries.entry(pr.clone()).or_default();
                entry.error = Some(error.to_string());
                entry.fetched_at = now;
            }
        }
    }

    pub fn merge_into(&self, pr: &PrRef, card: &mut LinearCardPr) {
        let Some(entry) = self.entries.get(pr) else {
            return;
        };
        card.gh_error = entry.error.clone();
        let Some(detail) = &entry.detail else {
            return;
        };
        card.state = detail.state.clone();
        card.draft = detail.draft.or(card.draft);
        card.ci = detail.ci.clone();
        card.additions = detail.additions;
        card.deletions = detail.deletions;
        card.preview = detail.preview.clone();
        card.blocker = detail.blocker.clone();
    }

    pub fn source(&self) -> LinearGhSource {
        match &self.unavailable {
            Some((message, _)) => LinearGhSource {
                status: GH_UNAVAILABLE.into(),
                message: Some(message.clone()),
            },
            None if self.ever_read => LinearGhSource {
                status: GH_OK.into(),
                message: None,
            },
            None => LinearGhSource {
                status: GH_UNKNOWN.into(),
                message: None,
            },
        }
    }

    pub fn document(&self, pr: &PrRef, now: i64, fresh_secs: i64) -> Option<LinearPrDocument> {
        let entry = self
            .entries
            .get(pr)
            .filter(|e| now - e.fetched_at < fresh_secs)?;
        Some(document(entry))
    }

    pub fn current_document(&self, pr: &PrRef) -> Option<LinearPrDocument> {
        self.entries.get(pr).map(document)
    }

    pub fn spaces_showing(&self, prs: &[PrRef]) -> BTreeSet<String> {
        self.boards
            .iter()
            .filter(|(_, shown)| prs.iter().any(|pr| shown.contains(pr)))
            .map(|(key, _)| key.space.clone())
            .collect()
    }
}

fn document(entry: &Entry) -> LinearPrDocument {
    LinearPrDocument {
        schema: 1,
        status: if entry.error.is_some() {
            "unavailable"
        } else {
            "ok"
        }
        .into(),
        message: entry.error.clone(),
        fetched_at: Some(entry.fetched_at),
        pr: entry.detail.clone(),
    }
}

/// The daemon's one `gh` service. Unconfigured, every read is unavailable;
/// the daemon configures it from its own environment at start.
pub struct GhService {
    client: Mutex<Option<Arc<GhClient>>>,
    state: Mutex<GhState>,
    pub pass: Arc<Debouncer>,
    debounce: Mutex<Duration>,
}

impl Default for GhService {
    fn default() -> Self {
        GhService {
            client: Mutex::new(None),
            state: Mutex::new(GhState::default()),
            pass: Arc::new(Debouncer::default()),
            debounce: Mutex::new(PASS_DEBOUNCE),
        }
    }
}

/// The one debouncer key: passes are global, so one runs at a time.
pub fn pass_key() -> SpaceKey {
    SpaceKey {
        session: String::new(),
        space: "gh-enrichment".into(),
    }
}

impl GhService {
    pub fn configure(&self, config: GhConfig) {
        *locked(&self.client) = Some(Arc::new(GhClient::new(config)));
    }

    pub fn host(&self) -> Option<String> {
        locked(&self.client)
            .as_ref()
            .and_then(|c| c.config().host.clone())
    }

    pub fn fetch(&self, pr: &PrRef) -> Result<LinearPrDetail, GhError> {
        let client = locked(&self.client).clone();
        match client {
            Some(client) => client.fetch(pr),
            None => Err(GhError::Unavailable("gh is not configured".into())),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, GhState> {
        locked(&self.state)
    }

    pub fn debounce(&self) -> Duration {
        *locked(&self.debounce)
    }

    pub fn set_debounce(&self, debounce: Duration) {
        *locked(&self.debounce) = debounce;
    }
}

pub fn read_all(
    service: &GhService,
    prs: Vec<PrRef>,
    done: impl Fn(&PrRef, Result<LinearPrDetail, GhError>) + Sync,
) {
    let work = Mutex::new(prs);
    // gh missing or logged out fails every read alike, so the rest of the
    // pass is not spawned; nothing is recorded for those PRs either way.
    let unavailable = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..PASS_CONCURRENCY {
            scope.spawn(|| loop {
                if unavailable.load(Ordering::Relaxed) {
                    break;
                }
                let Some(pr) = locked(&work).pop() else {
                    break;
                };
                let result = service.fetch(&pr);
                if matches!(result, Err(GhError::Unavailable(_))) {
                    unavailable.store(true, Ordering::Relaxed);
                }
                done(&pr, result);
            });
        }
    });
}
