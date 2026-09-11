//! The on-disk cache (FR-2.3, NFR-4.1).
//!
//! One JSON file per entry under `<home>/cache/<host>/<owner>/<name>/...`, written
//! through a temporary file and an atomic rename so a crash can never leave a
//! half-written payload that later reads as corrupt.
//!
//! The envelope is deliberately trivial — `{"fetched_at":N,"body":"..."}` — so a
//! curious user can `cat` an entry and see when it was fetched and what came back.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::adapters::fs::write_atomic;
use crate::error::{Error, Result};
use crate::ports::cache::{CacheKey, CacheStore, Stored};

/// What is actually written to disk.
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    /// When the payload was fetched, in seconds since the Unix epoch.
    fetched_at: u64,
    /// The payload, kept as text so the cache never has to know what is inside.
    body: String,
}

/// A cache rooted at a directory.
#[derive(Debug, Clone)]
pub struct DiskCache {
    root: PathBuf,
}

impl DiskCache {
    /// Binds the cache to a directory, which is created on demand.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where the cache lives, for `:doctor`.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The file an entry is stored in.
    #[must_use]
    pub fn path_for(&self, key: &CacheKey) -> PathBuf {
        self.root.join(key.to_path())
    }
}

impl CacheStore for DiskCache {
    fn read(&self, key: &CacheKey) -> Result<Option<Stored>> {
        let path = self.path_for(key);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Error::io("read the cache entry", &path, error)),
        };

        // A corrupt entry is an error rather than a miss: the caller has to decide
        // whether to refetch, and silently pretending nothing was cached would hide
        // a real problem on every start.
        match serde_json::from_str::<Envelope>(&text) {
            Ok(envelope) => Ok(Some(Stored {
                body: envelope.body,
                fetched_at: envelope.fetched_at,
            })),
            Err(error) => Err(Error::cache(format!(
                "{} is not a readable cache entry ({error}); delete it to refetch",
                path.display()
            ))),
        }
    }

    fn write(&self, key: &CacheKey, body: &str, now: u64) -> Result<()> {
        let path = self.path_for(key);
        let envelope = serde_json::to_string(&Envelope {
            fetched_at: now,
            body: body.to_owned(),
        })
        .map_err(|error| Error::cache(format!("could not encode the cache entry: {error}")))?;

        write_atomic(&path, &envelope)
            .map_err(|error| Error::io("write the cache entry", &path, error))
    }

    fn remove(&self, key: &CacheKey) -> Result<()> {
        let path = self.path_for(key);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::io("remove the cache entry", &path, error)),
        }
    }

    fn clear_prefix(&self, prefix: &str) -> Result<u32> {
        // The prefix is a repository key, so it is validated the same way a cache
        // key is: this function deletes directories recursively and must never be
        // handed something that walks out of the cache.
        let key = CacheKey::new(prefix).map_err(|error| Error::cache(error.to_string()))?;
        let path = self.root.join(key.to_path());
        if !path.exists() {
            return Ok(0);
        }
        let removed = count_files(&path);
        std::fs::remove_dir_all(&path)
            .map_err(|error| Error::io("clear the cache", &path, error))?;
        Ok(removed)
    }
}

/// Counts the files under a directory, for the "cleared N entries" message.
fn count_files(path: &Path) -> u32 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() { count_files(&path) } else { 1 }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_home;

    fn key(text: &str) -> CacheKey {
        CacheKey::new(text).unwrap()
    }

    #[test]
    fn a_stored_entry_comes_back_with_the_moment_it_was_fetched() {
        let dir = temp_home();
        let cache = DiskCache::new(dir.path().join("cache"));
        let entry = key("github.com/acme/service/list/abc.json");

        assert!(cache.read(&entry).unwrap().is_none(), "nothing yet");
        cache.write(&entry, "{\"a\":1}", 1_000).unwrap();

        let stored = cache.read(&entry).unwrap().unwrap();
        assert_eq!(stored.body, "{\"a\":1}");
        assert_eq!(stored.fetched_at, 1_000);
        assert_eq!(stored.age_secs(1_030), 30);
        assert!(!stored.is_stale(1_030, 60));
    }

    #[test]
    fn entries_are_laid_out_per_repository() {
        let dir = temp_home();
        let cache = DiskCache::new(dir.path().join("cache"));
        let entry = key("github.com/acme/service/pr-141/detail.json");
        assert_eq!(
            cache.path_for(&entry),
            dir.path()
                .join("cache/github.com/acme/service/pr-141/detail.json")
        );
    }

    #[test]
    fn writing_creates_the_directories_it_needs() {
        let dir = temp_home();
        let cache = DiskCache::new(dir.path().join("cache"));
        cache
            .write(&key("github.com/acme/service/list/x.json"), "{}", 1)
            .unwrap();
        assert!(cache.root().join("github.com/acme/service/list").is_dir());
    }

    #[test]
    fn writing_twice_replaces_rather_than_appends() {
        let dir = temp_home();
        let cache = DiskCache::new(dir.path().join("cache"));
        let entry = key("github.com/acme/service/list/x.json");
        cache.write(&entry, "first", 1).unwrap();
        cache.write(&entry, "second", 2).unwrap();

        let stored = cache.read(&entry).unwrap().unwrap();
        assert_eq!(stored.body, "second");
        assert_eq!(stored.fetched_at, 2);
    }

    #[test]
    fn a_corrupt_entry_is_an_error_that_says_how_to_fix_it() {
        let dir = temp_home();
        let cache = DiskCache::new(dir.path().join("cache"));
        let entry = key("github.com/acme/service/list/x.json");
        cache.write(&entry, "{}", 1).unwrap();
        std::fs::write(cache.path_for(&entry), "not json").unwrap();

        let error = cache.read(&entry).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("not a readable cache entry"), "{text}");
        assert!(text.contains("delete it"), "{text}");
    }

    #[test]
    fn removing_an_entry_that_is_not_there_is_not_an_error() {
        let dir = temp_home();
        let cache = DiskCache::new(dir.path().join("cache"));
        cache
            .remove(&key("github.com/acme/service/list/x.json"))
            .unwrap();
    }

    #[test]
    fn clearing_a_repository_removes_only_that_repository() {
        let dir = temp_home();
        let cache = DiskCache::new(dir.path().join("cache"));
        cache
            .write(&key("github.com/acme/service/list/a.json"), "{}", 1)
            .unwrap();
        cache
            .write(&key("github.com/acme/service/pr-1/detail.json"), "{}", 1)
            .unwrap();
        cache
            .write(&key("github.com/acme/other/list/a.json"), "{}", 1)
            .unwrap();

        let removed = cache.clear_prefix("github.com/acme/service").unwrap();
        assert_eq!(removed, 2);
        assert!(
            cache
                .read(&key("github.com/acme/service/list/a.json"))
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .read(&key("github.com/acme/other/list/a.json"))
                .unwrap()
                .is_some(),
            "another repository is untouched"
        );

        assert_eq!(cache.clear_prefix("github.com/acme/service").unwrap(), 0);
    }

    #[test]
    fn clearing_refuses_a_prefix_that_would_escape_the_cache() {
        let dir = temp_home();
        let cache = DiskCache::new(dir.path().join("cache"));
        assert!(cache.clear_prefix("../..").is_err());
        assert!(cache.clear_prefix("/").is_err());
        assert!(dir.path().is_dir(), "the directory is still there");
    }

    #[test]
    fn a_body_with_newlines_or_quotes_survives_the_round_trip() {
        let dir = temp_home();
        let cache = DiskCache::new(dir.path().join("cache"));
        let entry = key("github.com/acme/service/list/x.json");
        let body = "line \"one\"\nline two\\n\n{}";
        cache.write(&entry, body, 7).unwrap();
        assert_eq!(cache.read(&entry).unwrap().unwrap().body, body);
    }
}
