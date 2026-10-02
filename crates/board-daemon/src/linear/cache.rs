//! One shared Linear read per space, and the debounced refetch that
//! follows a reported Linear write.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// A space is a herdr workspace id, and workspace ids repeat across herdr
/// sessions, so the session is part of the key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpaceKey {
    pub session: String,
    pub space: String,
}

pub struct CacheRead<T> {
    /// The newest successful read for this plan, and how old it is.
    pub good: Option<(Arc<T>, Duration)>,
    /// Why the newest attempt failed; `None` when it succeeded.
    pub error: Option<String>,
}

struct Entry<P, T> {
    plan: P,
    good: Option<(Arc<T>, Instant)>,
    error: Option<String>,
    attempted: Option<Instant>,
    invalidated: Option<Instant>,
    in_flight: bool,
}

impl<P: PartialEq, T> Entry<P, T> {
    fn fresh(&self, plan: &P, ttl: Duration, now: Instant) -> bool {
        let Some(attempted) = self.attempted else {
            return false;
        };
        self.plan == *plan
            && now.saturating_duration_since(attempted) < ttl
            && self.invalidated.is_none_or(|at| attempted > at)
    }

    fn read(&self, now: Instant) -> CacheRead<T> {
        CacheRead {
            good: self
                .good
                .as_ref()
                .map(|(value, at)| (value.clone(), now.saturating_duration_since(*at))),
            error: self.error.clone(),
        }
    }
}

pub struct SnapshotCache<P, T> {
    entries: Mutex<HashMap<SpaceKey, Entry<P, T>>>,
    settled: Condvar,
}

impl<P, T> Default for SnapshotCache<P, T> {
    fn default() -> Self {
        SnapshotCache {
            entries: Mutex::new(HashMap::new()),
            settled: Condvar::new(),
        }
    }
}

fn locked<V>(mutex: &Mutex<V>) -> MutexGuard<'_, V> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<P: Clone + PartialEq, T> SnapshotCache<P, T> {
    /// The cached read when it is fresh for `plan`; otherwise one caller
    /// fetches while every other caller of the same key waits for its answer.
    /// The lock is never held across `fetch`.
    pub fn read(
        &self,
        key: &SpaceKey,
        plan: &P,
        ttl: Duration,
        fetch: impl FnOnce(&P) -> Result<T, String>,
    ) -> CacheRead<T> {
        let mut entries = locked(&self.entries);
        loop {
            let now = Instant::now();
            match entries.get_mut(key) {
                Some(entry) if entry.fresh(plan, ttl, now) => return entry.read(now),
                Some(entry) if entry.in_flight => {
                    entries = self
                        .settled
                        .wait(entries)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                Some(entry) => {
                    if entry.plan != *plan {
                        entry.plan = plan.clone();
                        entry.good = None;
                        entry.error = None;
                    }
                    entry.in_flight = true;
                    break;
                }
                None => {
                    entries.insert(
                        key.clone(),
                        Entry {
                            plan: plan.clone(),
                            good: None,
                            error: None,
                            attempted: None,
                            invalidated: None,
                            in_flight: true,
                        },
                    );
                    break;
                }
            }
        }
        drop(entries);

        let guard = InFlight { cache: self, key };
        let started = Instant::now();
        let outcome = fetch(plan);
        std::mem::forget(guard);

        let mut entries = locked(&self.entries);
        let now = Instant::now();
        let read = match entries.get_mut(key) {
            Some(entry) => {
                entry.in_flight = false;
                // The start, not the finish: an invalidation that lands during
                // the fetch must leave this read stale.
                entry.attempted = Some(started);
                match outcome {
                    Ok(value) => {
                        entry.good = Some((Arc::new(value), now));
                        entry.error = None;
                    }
                    Err(error) => entry.error = Some(error),
                }
                entry.read(now)
            }
            None => match outcome {
                Ok(value) => CacheRead {
                    good: Some((Arc::new(value), Duration::ZERO)),
                    error: None,
                },
                Err(error) => CacheRead {
                    good: None,
                    error: Some(error),
                },
            },
        };
        self.settled.notify_all();
        read
    }

    /// Marks the key's read out of date. Returns its plan when a reader has
    /// read the space, which is when a refetch is worth scheduling.
    pub fn invalidate(&self, key: &SpaceKey) -> Option<P> {
        let mut entries = locked(&self.entries);
        let entry = entries.get_mut(key)?;
        entry.invalidated = Some(Instant::now());
        Some(entry.plan.clone())
    }

    pub fn plan(&self, key: &SpaceKey) -> Option<P> {
        locked(&self.entries)
            .get(key)
            .map(|entry| entry.plan.clone())
    }

    pub fn remove(&self, key: &SpaceKey) {
        locked(&self.entries).remove(key);
        self.settled.notify_all();
    }

    /// The key's last read when it succeeded, with its plan, without
    /// fetching or waiting on a fetch in flight.
    pub fn peek_good(&self, key: &SpaceKey) -> Option<(Arc<T>, P)> {
        let entries = locked(&self.entries);
        let entry = entries.get(key).filter(|entry| entry.error.is_none())?;
        let (value, _) = entry.good.as_ref()?;
        Some((value.clone(), entry.plan.clone()))
    }

    /// What `find` takes from the key's last good read, even when a later
    /// attempt failed.
    pub fn find_good<R>(&self, key: &SpaceKey, find: impl FnOnce(&T) -> Option<R>) -> Option<R> {
        let entries = locked(&self.entries);
        let (value, _) = entries.get(key)?.good.as_ref()?;
        find(value)
    }
}

/// Clears the in-flight mark if `fetch` unwinds, so waiters are not stranded.
struct InFlight<'a, P, T> {
    cache: &'a SnapshotCache<P, T>,
    key: &'a SpaceKey,
}

impl<P, T> Drop for InFlight<'_, P, T> {
    fn drop(&mut self) {
        if let Some(entry) = locked(&self.cache.entries).get_mut(self.key) {
            entry.in_flight = false;
        }
        self.cache.settled.notify_all();
    }
}

enum Pass {
    Waiting,
    Running { again: bool },
}

/// Coalesces bursts per key: a burst of `schedule` calls runs `work` once
/// after `delay`, and a call that lands while `work` runs gets one more pass.
/// `done` runs once, after the last pass, even when a pass panics.
#[derive(Default)]
pub struct Debouncer {
    passes: Mutex<HashMap<SpaceKey, Pass>>,
}

impl Debouncer {
    pub fn schedule(
        self: &Arc<Self>,
        key: SpaceKey,
        delay: Duration,
        work: impl Fn() + Send + 'static,
        done: impl FnOnce() + Send + 'static,
    ) {
        {
            let mut passes = locked(&self.passes);
            match passes.get_mut(&key) {
                Some(Pass::Waiting) => return,
                Some(Pass::Running { again }) => {
                    *again = true;
                    return;
                }
                None => {
                    passes.insert(key.clone(), Pass::Waiting);
                }
            }
        }
        let this = self.clone();
        std::thread::spawn(move || {
            // Declared before `_clear` so it drops after it: on a panic the
            // pass is cleared first, then `done` still runs.
            let _done = RunOnDrop(Some(done));
            let _clear = ClearPass {
                debouncer: &this,
                key: &key,
            };
            loop {
                std::thread::sleep(delay);
                locked(&this.passes).insert(key.clone(), Pass::Running { again: false });
                work();
                let mut passes = locked(&this.passes);
                if matches!(passes.get(&key), Some(Pass::Running { again: true })) {
                    passes.insert(key.clone(), Pass::Waiting);
                    continue;
                }
                passes.remove(&key);
                break;
            }
        });
    }
}

struct RunOnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for RunOnDrop<F> {
    fn drop(&mut self) {
        if let Some(done) = self.0.take() {
            done();
        }
    }
}

struct ClearPass<'a> {
    debouncer: &'a Debouncer,
    key: &'a SpaceKey,
}

impl Drop for ClearPass<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            locked(&self.debouncer.passes).remove(self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn done_still_runs_when_the_work_panics() {
        let debouncer = Arc::new(Debouncer::default());
        let announced = Arc::new(AtomicBool::new(false));
        let flag = announced.clone();
        let key = SpaceKey {
            session: "alpha".into(),
            space: "w1".into(),
        };
        debouncer.schedule(
            key.clone(),
            Duration::from_millis(5),
            || panic!("the refetch failed"),
            move || flag.store(true, Ordering::SeqCst),
        );
        let until = Instant::now() + Duration::from_secs(5);
        while !announced.load(Ordering::SeqCst) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(announced.load(Ordering::SeqCst));
        assert!(!locked(&debouncer.passes).contains_key(&key));
    }
}
