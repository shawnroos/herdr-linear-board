//! Show-request expiry: reads already hide an overdue request, and this
//! sweep closes it as `expired` and announces its space so an open board
//! drops it while nothing else changes.

use std::sync::Arc;
use std::time::Duration;

use board_core::protocol::Event;

use crate::state::Daemon;

// A request outlives its TTL by at most this much on an idle board.
const SWEEP_INTERVAL: Duration = Duration::from_secs(10);

pub(super) async fn show_request_sweeper(d: Arc<Daemon>) {
    let mut rx = d.shutdown_rx();
    let mut iv = tokio::time::interval(SWEEP_INTERVAL);
    loop {
        tokio::select! {
            _ = iv.tick() => {
                sweep_show_requests_at(&d, crate::ops::now_secs(&d));
            }
            _ = rx.changed() => break,
        }
        if d.is_shutdown() {
            break;
        }
    }
}

/// Expires every request overdue at `now` and announces each affected space
/// once. Returns those spaces.
pub(crate) fn sweep_show_requests_at(d: &Arc<Daemon>, now: i64) -> Vec<String> {
    let spaces = match d.store.lock().expire_overdue(now) {
        Ok(spaces) => spaces,
        Err(_) => {
            tracing::warn!(
                error_category = "database",
                "show-request expiry sweep failed"
            );
            return Vec::new();
        }
    };
    for space in &spaces {
        d.emit(Event::LocalStateChanged {
            space: Some(space.clone()),
            snapshot: false,
        });
    }
    spaces
}
