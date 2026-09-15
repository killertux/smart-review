//! Filesystem-backed configuration and state (ARCH-3).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{self, Loaded};
use crate::error::Result;
use crate::ports::{ConfigStore, StateStore};
use crate::state::{self, AppState};

/// Writes `contents` to `path` through an exclusive, synchronised sibling and an
/// atomic rename (NFR-4.1).
///
/// This protects readers from a process crash between writing and publishing. On Unix
/// the containing directory is also synchronised after the rename, so the rename is
/// durable once this function succeeds. No filesystem API can promise that a physical
/// device has survived power loss beyond the guarantees its filesystem provides.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    write_atomic_with_mode(path, contents, None)
}

/// Writes `contents` to `path` atomically with an explicit file mode.
///
/// The mode is applied to the temporary file *before* the rename, so a secret is
/// never briefly world-readable (NFR-3.1). On platforms without POSIX modes the
/// mode argument is ignored.
pub(crate) fn write_atomic_with_mode(
    path: &Path,
    contents: &str,
    mode: Option<u32>,
) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a document path needs a parent",
        )
    })?;
    create_private_parents(parent)?;
    let temporary = unique_temporary_path(path);
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let destination_mode = std::fs::metadata(path)
                .ok()
                .map(|metadata| metadata.permissions().mode() & 0o777);
            let effective_mode = mode.or(destination_mode).unwrap_or(0o600);
            options.mode(effective_mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        let mut file = options.open(&temporary)?;
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        // This operation created the unique name, so it can never remove another
        // writer's in-progress replacement.
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn unique_temporary_path(path: &Path) -> PathBuf {
    static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    name.push(format!(".tmp-{}-{sequence}", std::process::id()));
    path.with_file_name(name)
}

pub(crate) fn create_private_parents(parent: &Path) -> std::io::Result<()> {
    let mut missing = Vec::new();
    let mut cursor = parent;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        cursor = cursor.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no existing parent directory")
        })?;
    }
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    for directory in missing {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// `config.toml` on disk (FR-8.2).
#[derive(Debug, Clone)]
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
///
/// Cloning is cheap: it is a path. The event loop keeps a copy while the
/// interface owns the original.
#[derive(Debug, Clone)]
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
        assert!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp-"))
        );
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

    #[test]
    fn the_state_store_contract_holds_for_both_implementations() {
        // Proves the port is a real seam: an in-memory fake satisfies the same
        // contract the on-disk store does (ARCH-2, NFR-5.2).
        use crate::test_support::InMemoryStateStore;

        let dir = temp_home();
        let on_disk = TomlStateStore::new(dir.path().join("state.toml"));
        let in_memory = InMemoryStateStore::default();
        let stores: [&dyn StateStore; 2] = [&on_disk, &in_memory];

        for store in stores {
            assert_eq!(store.load().unwrap(), AppState::default());
            let value = AppState {
                theme: Some("light".to_owned()),
                focus: Some("diff".to_owned()),
                ..AppState::default()
            };
            store.save(&value).unwrap();
            assert_eq!(store.load().unwrap(), value);
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_atomic_write_can_set_a_private_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_home();
        let path = dir.path().join("credentials.toml");
        write_atomic_with_mode(&path, "secret", Some(0o600)).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the secret must never be world readable"
        );
    }

    #[test]
    fn concurrent_atomic_writers_never_publish_interleaved_bytes() {
        use std::sync::{Arc, Barrier};

        let dir = temp_home();
        let path = dir.path().join("shared.json");
        let start = Arc::new(Barrier::new(3));
        let writers: Vec<_> = ["a".repeat(32_768), "b".repeat(32_768)]
            .into_iter()
            .map(|contents| {
                let start = Arc::clone(&start);
                let path = path.clone();
                std::thread::spawn(move || {
                    start.wait();
                    write_atomic(&path, &contents)
                })
            })
            .collect();
        start.wait();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }
        let published = std::fs::read_to_string(path).unwrap();
        assert!(published == "a".repeat(32_768) || published == "b".repeat(32_768));
    }
}
