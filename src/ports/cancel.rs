//! Cancellation handles for long-running work (ARCH-5, NFR-1.4).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A shareable "stop what you are doing" flag.
///
/// The event loop keeps one and hands a clone to whatever is running: a process
/// adapter polls it while waiting for a child and kills it, and a job thread
/// checks it before publishing a result. It is deliberately a flag rather than a
/// channel so that a cancelled job cannot deadlock waiting for a reader.
#[derive(Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
}

impl Cancel {
    /// A flag that nobody has raised.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Raises the flag. Idempotent, and safe to call from any thread.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether the flag has been raised.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
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
}
