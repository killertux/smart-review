//! Filesystem-backed configuration and state (ARCH-3).

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::{self, Loaded};
use crate::error::Result;
use crate::ports::{ConfigStore, StateStore};
use crate::state::{self, AppState};

/// Writes `contents` to `path` through a temporary file and an atomic rename, so
/// a crash can never leave a half-written file behind (NFR-4.1).
pub(crate) fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = temporary_path(path);
    {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

/// `config.toml` on disk (FR-8.2).
#[derive(Debug)]
pub struct TomlConfigStore {
    path: PathBuf,
}

impl TomlConfigStore {
    /// Binds the store to a file.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl ConfigStore for TomlConfigStore {
    fn load(&self) -> Result<Loaded> {
        Ok(config::load(&self.path)?)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

/// `state.toml` on disk (FR-8.5).
#[derive(Debug)]
pub struct TomlStateStore {
    path: PathBuf,
}

impl TomlStateStore {
    /// Binds the store to a file.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl StateStore for TomlStateStore {
    fn load(&self) -> Result<AppState> {
        Ok(state::load(&self.path)?)
    }

    fn save(&self, value: &AppState) -> Result<()> {
        Ok(state::save(&self.path, value)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_home;

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_temp_file() {
        let dir = temp_home();
        let path = dir.path().join("file.txt");
        write_atomic(&path, "first").unwrap();
        write_atomic(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        assert!(!temporary_path(&path).exists());
    }

    #[test]
    fn atomic_write_creates_missing_directories() {
        let dir = temp_home();
        let path = dir.path().join("a/b/c.txt");
        write_atomic(&path, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn state_store_round_trips() {
        let dir = temp_home();
        let store = TomlStateStore::new(dir.path().join("state.toml"));
        let value = AppState {
            theme: Some("light".to_owned()),
            ..AppState::default()
        };
        store.save(&value).unwrap();
        assert_eq!(store.load().unwrap(), value);
    }

    #[test]
    fn config_store_reports_its_path() {
        let dir = temp_home();
        let path = dir.path().join("config.toml");
        let store = TomlConfigStore::new(path.clone());
        assert_eq!(store.path(), path.as_path());
        assert!(!store.load().unwrap().exists);
    }
}
