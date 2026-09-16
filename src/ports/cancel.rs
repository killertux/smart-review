//! Cancellation handles for long-running work (ARCH-5, NFR-1.4).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[derive(Default)]
struct State {
    flag: AtomicBool,
    changed: Notify,
}

/// A shareable "stop what you are doing" flag.
///
/// The event loop keeps one and hands a clone to whatever is running: a process
/// adapter polls it while waiting for a child and kills it, and a job thread
/// checks it before publishing a result. It is deliberately a flag rather than a
/// channel so that a cancelled job cannot deadlock waiting for a reader.
#[derive(Clone, Default)]
pub struct Cancel {
    state: Arc<State>,
}

impl Cancel {
    /// A flag that nobody has raised.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Raises the flag. Idempotent, and safe to call from any thread.
    pub fn cancel(&self) {
        self.state.flag.store(true, Ordering::SeqCst);
        self.state.changed.notify_waiters();
    }

    /// Whether the flag has been raised.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.flag.load(Ordering::SeqCst)
    }

    /// Waits until cancellation is requested.
    ///
    /// The check before and after subscribing closes the race where cancellation is
    /// requested between a caller's initial check and its async `select!`.
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = self.state.changed.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

impl fmt::Debug for Cancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cancel")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_is_visible_across_clones() {
        let cancel = Cancel::new();
        let clone = cancel.clone();
        assert!(!cancel.is_cancelled());

        clone.cancel();
        assert!(cancel.is_cancelled(), "the clone shares the flag");
        assert!(format!("{cancel:?}").contains("cancelled: true"));
    }

    #[test]
    fn cancelling_twice_is_fine() {
        let cancel = Cancel::new();
        cancel.cancel();
        cancel.cancel();
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn async_waiter_observes_cancellation() {
        let cancel = Cancel::new();
        let waiter = cancel.clone();
        let thread = std::thread::spawn(move || {
            let result = crate::adapters::http::block_on(async { waiter.cancelled().await });
            assert!(result.is_ok());
        });
        std::thread::sleep(std::time::Duration::from_millis(10));
        cancel.cancel();
        assert!(thread.join().is_ok());
    }
}
