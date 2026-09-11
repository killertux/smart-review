//! Test-only helpers shared between unit tests inside the crate.
//!
//! Deliberately dependency-free: temporary directories and fake clocks are
//! implemented here rather than pulling in another crate.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Hands out a distinct number per temporary directory in this process.
///
/// The clock alone is not enough: macOS reports time at microsecond resolution,
/// so two tests starting in the same microsecond would share a directory, and one
/// deleting it on drop would pull the ground out from under the other (which is
/// exactly what happened on the macOS runner).
static NEXT_HOME: AtomicU64 = AtomicU64::new(0);

/// A unique temporary directory that removes itself when dropped.
#[derive(Debug)]
pub(crate) struct TempHome {
    path: PathBuf,
}

impl TempHome {
    /// Creates a fresh temporary directory.
    pub(crate) fn new() -> Self {
        let unique = format!(
            "smart-review-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default(),
            NEXT_HOME.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        let _ = std::fs::create_dir_all(&path);
        Self { path }
    }

    /// The directory path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Writes `contents` to `name` inside the directory and returns the path.
    pub(crate) fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.path.join(name);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, contents);
        path
    }
}

impl Default for TempHome {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Shorthand for [`TempHome::new`].
pub(crate) fn temp_home() -> TempHome {
    TempHome::new()
}

/// An in-memory [`CacheStore`](crate::ports::CacheStore) for tests.
///
/// Exists so the cache-first and offline paths of the application layer can be
/// exercised without touching a disk (NFR-5.2).
#[derive(Debug, Default)]
pub(crate) struct InMemoryCache {
    entries: Mutex<std::collections::BTreeMap<String, crate::ports::Stored>>,
}

impl crate::ports::CacheStore for InMemoryCache {
    fn read(&self, key: &crate::ports::CacheKey) -> crate::Result<Option<crate::ports::Stored>> {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(guard.get(key.as_str()).cloned())
    }

    fn write(&self, key: &crate::ports::CacheKey, body: &str, now: u64) -> crate::Result<()> {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(
            key.as_str().to_owned(),
            crate::ports::Stored {
                body: body.to_owned(),
                fetched_at: now,
            },
        );
        Ok(())
    }

    fn remove(&self, key: &crate::ports::CacheKey) -> crate::Result<()> {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.remove(key.as_str());
        Ok(())
    }

    fn clear_prefix(&self, prefix: &str) -> crate::Result<u32> {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = guard.len();
        guard.retain(|key, _| !key.starts_with(prefix));
        Ok(u32::try_from(before - guard.len()).unwrap_or(u32::MAX))
    }
}

/// An in-memory [`StateStore`](crate::ports::StateStore) for tests.
///
/// Exists so the port is a real seam rather than a single implementation behind
/// a trait: application and loop tests can substitute it for the on-disk store.
#[derive(Debug, Default)]
pub(crate) struct InMemoryStateStore {
    state: Mutex<Option<crate::state::AppState>>,
}

impl crate::ports::StateStore for InMemoryStateStore {
    fn load(&self) -> crate::Result<crate::state::AppState> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(guard.clone().unwrap_or_default())
    }

    fn save(&self, value: &crate::state::AppState) -> crate::Result<()> {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(value.clone());
        Ok(())
    }
}
