//! Test-only helpers shared between unit tests inside the crate.
//!
//! Deliberately dependency-free: temporary directories and fake clocks are
//! implemented here rather than pulling in another crate.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A unique temporary directory that removes itself when dropped.
#[derive(Debug)]
pub(crate) struct TempHome {
    path: PathBuf,
}

impl TempHome {
    /// Creates a fresh temporary directory.
    pub(crate) fn new() -> Self {
        let unique = format!(
            "smart-review-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
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
