//! The cache port (FR-2.3, ARCH-2).
//!
//! Deliberately byte-oriented: the cache stores a payload and the moment it was
//! fetched, and knows nothing about what is inside. That keeps the TTL policy, the
//! key layout and the (de)serialization in the application layer where they can be
//! tested against an in-memory fake, and keeps the adapter down to file IO.

use std::fmt;

/// Where a cache entry lives.
///
/// The key is `host/owner/name/variant` built by the application, so two clones of
/// one repository share entries and two repositories can never collide (FR-1.3).
/// It is validated on construction because a key becomes a file path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheKey(String);

/// Why a cache key was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CacheKeyError {
    #[error("'{0}' is not a usable cache key")]
    Invalid(String),
}

impl CacheKey {
    /// Validates a key.
    ///
    /// # Errors
    ///
    /// Returns [`CacheKeyError`] when the key is empty, absolute, contains a
    /// component that would escape the cache directory, or contains a character
    /// that is not safe in a file name on both Linux and macOS.
    pub fn new(key: impl Into<String>) -> Result<Self, CacheKeyError> {
        let key = key.into();
        let invalid = key.is_empty()
            || key.starts_with('/')
            || key.contains('\\')
            || key.contains('\0')
            || key
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..");
        let unsafe_chars = !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c));
        if invalid || unsafe_chars {
            return Err(CacheKeyError::Invalid(key));
        }
        Ok(Self(key))
    }

    /// The key as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The key as a relative path.
    #[must_use]
    pub fn to_path(&self) -> std::path::PathBuf {
        self.0.split('/').collect()
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A cache entry that was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// The payload, exactly as it was written.
    pub body: String,
    /// When it was written, in seconds since the Unix epoch.
    pub fetched_at: u64,
}

impl Stored {
    /// How old the entry is, given the current time (FR-2.3).
    #[must_use]
    pub fn age_secs(&self, now: u64) -> u64 {
        now.saturating_sub(self.fetched_at)
    }

    /// Whether the entry is older than `ttl`.
    #[must_use]
    pub fn is_stale(&self, now: u64, ttl_secs: u64) -> bool {
        self.age_secs(now) > ttl_secs
    }
}

/// Reads and writes cached payloads.
pub trait CacheStore: fmt::Debug + Send + Sync {
    /// Reads an entry.
    ///
    /// A missing entry is `Ok(None)`: absence is the normal case, not a failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry exists but cannot be read or decoded. A
    /// corrupt entry is reported so the caller can decide to refetch, rather than
    /// being silently treated as absent.
    fn read(&self, key: &CacheKey) -> crate::Result<Option<Stored>>;

    /// Writes an entry, atomically.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry cannot be written.
    fn write(&self, key: &CacheKey, body: &str, now: u64) -> crate::Result<()>;

    /// Removes one entry, ignoring a missing file.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry exists but cannot be removed.
    fn remove(&self, key: &CacheKey) -> crate::Result<()>;

    /// Removes everything under a repository prefix, which is what
    /// `:refresh --all` and a workspace reset need.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory exists but cannot be removed.
    fn clear_prefix(&self, prefix: &str) -> crate::Result<u32>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_key_is_accepted_and_mapped_to_a_path() {
        let key = CacheKey::new("github.com/acme/service/list/4f2a1b.json").unwrap();
        assert_eq!(
            key.to_path(),
            std::path::PathBuf::from("github.com/acme/service/list/4f2a1b.json")
        );
    }

    #[test]
    fn a_key_that_would_escape_the_cache_directory_is_refused() {
        for key in [
            "",
            "/etc/passwd",
            "../outside",
            "host/../../etc/passwd",
            "host//name",
            "host/./name",
            "host/name\\evil",
            "host/na me",
            "host/na:me",
        ] {
            assert!(CacheKey::new(key).is_err(), "{key} should be refused");
        }
    }

    #[test]
    fn staleness_is_measured_against_the_injected_clock() {
        let entry = Stored {
            body: "{}".to_owned(),
            fetched_at: 1_000,
        };
        assert_eq!(entry.age_secs(1_060), 60);
        assert!(!entry.is_stale(1_060, 60), "exactly the TTL is still fresh");
        assert!(entry.is_stale(1_061, 60));
        // A clock that moved backwards must not produce a huge age.
        assert_eq!(entry.age_secs(900), 0);
    }
}
