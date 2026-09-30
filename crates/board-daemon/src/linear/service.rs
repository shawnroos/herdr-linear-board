//! The daemon's one Linear service: the client, built from boardd's own
//! environment on first use, and the shared per-space cache (KTD11).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::cache::{Debouncer, SnapshotCache};
use super::client::{LinearClient, LinearError};
use super::read::{FetchPlan, SpaceRead};

// Chosen, not measured. The TTL bounds how stale a board's Linear half can be
// without a reported write; the debounce folds a burst of reported writes
// into one refetch.
const SNAPSHOT_TTL: Duration = Duration::from_secs(15);
const REFETCH_DEBOUNCE: Duration = Duration::from_millis(500);

struct Tuning {
    ttl: Duration,
    debounce: Duration,
    store_dir: Option<PathBuf>,
}

pub struct LinearService {
    client: Mutex<Option<Arc<LinearClient>>>,
    pub cache: SnapshotCache<FetchPlan, SpaceRead>,
    pub refetch: Arc<Debouncer>,
    tuning: Mutex<Tuning>,
}

impl Default for LinearService {
    fn default() -> Self {
        LinearService {
            client: Mutex::new(None),
            cache: SnapshotCache::default(),
            refetch: Arc::new(Debouncer::default()),
            tuning: Mutex::new(Tuning {
                ttl: SNAPSHOT_TTL,
                debounce: REFETCH_DEBOUNCE,
                store_dir: None,
            }),
        }
    }
}

impl LinearService {
    /// A client that failed to build (a refused `BOARD_LINEAR_API_URL`) is
    /// not kept, so each read reports why.
    pub fn client(&self) -> Result<Arc<LinearClient>, LinearError> {
        let mut slot = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(client) = slot.as_ref() {
            return Ok(client.clone());
        }
        let client = Arc::new(LinearClient::from_env()?);
        *slot = Some(client.clone());
        Ok(client)
    }

    pub fn ttl(&self) -> Duration {
        self.tuning().ttl
    }

    pub fn debounce(&self) -> Duration {
        self.tuning().debounce
    }

    /// The work plugin's store directory, as `board import work-store` reads it.
    pub fn store_dir(&self) -> Option<PathBuf> {
        let configured = self.tuning().store_dir.clone();
        configured.or_else(|| crate::import::store_dir_from_env().ok())
    }

    fn tuning(&self) -> std::sync::MutexGuard<'_, Tuning> {
        self.tuning.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub fn set_client(&self, client: LinearClient) {
        *self.client.lock().unwrap() = Some(Arc::new(client));
    }

    #[cfg(test)]
    pub fn set_timing(&self, ttl: Duration, debounce: Duration) {
        let mut tuning = self.tuning();
        tuning.ttl = ttl;
        tuning.debounce = debounce;
    }

    #[cfg(test)]
    pub fn set_store_dir(&self, dir: PathBuf) {
        self.tuning().store_dir = Some(dir);
    }
}
