//! Per-request cancellation for a handler running on the blocking pool.
//!
//! The connection layer owns the token and sets it when the client's side of
//! the socket closes; the handler thread reads it through [`current`]. A
//! thread-local keeps `ops::handle_request`'s signature, which every route and
//! test calls, unchanged.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub(crate) struct RequestCancel(Arc<AtomicBool>);

impl RequestCancel {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<RequestCancel>> = const { RefCell::new(None) };
}

/// Run `f` with `token` as this thread's request token. The blocking pool
/// reuses threads, so the token is cleared on the way out, panics included.
pub(crate) fn scoped<R>(token: RequestCancel, f: impl FnOnce() -> R) -> R {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            CURRENT.with(|current| *current.borrow_mut() = None);
        }
    }
    CURRENT.with(|current| *current.borrow_mut() = Some(token));
    let _clear = Clear;
    f()
}

/// The token of the request this thread is handling, if any.
pub(crate) fn current() -> Option<RequestCancel> {
    CURRENT.with(|current| current.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_is_visible_inside_the_scope_and_cleared_after_it() {
        let token = RequestCancel::default();
        assert!(current().is_none());
        scoped(token.clone(), || {
            let seen = current().expect("token inside the scope");
            assert!(!seen.is_cancelled());
            token.cancel();
            assert!(seen.is_cancelled());
        });
        assert!(current().is_none());
    }

    #[test]
    fn a_panic_inside_the_scope_still_clears_the_token() {
        let result = std::panic::catch_unwind(|| {
            scoped(RequestCancel::default(), || panic!("handler panic"));
        });
        assert!(result.is_err());
        assert!(current().is_none());
    }
}
