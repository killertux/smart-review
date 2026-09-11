//! The model catalog port (FR-4.7, ARCH-2).
//!
//! The catalog is a *cacheable remote document*: it arrives over the network, is
//! stored on disk, and is used long after it was fetched. Which of those three
//! happened is part of the answer, because the UI has to say so (§7.5: a stale
//! catalog is preferred over an empty picker, but never silently).

use std::fmt;

use crate::domain::model::Catalog;

/// How hard to try the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogPolicy {
    /// Use the cache when it is fresh, otherwise fetch. The default.
    CacheFirst,
    /// Fetch even when the cache is fresh (`:catalog refresh`).
    Refresh,
    /// Never touch the network; answer from the cache whatever its age.
    CacheOnly,
}

/// Where the catalog came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogSource {
    /// Fetched just now.
    Fetched,
    /// Read from disk, still inside its TTL.
    Cached {
        /// The cache's age in seconds.
        age_secs: u64,
    },
    /// Read from disk, past its TTL, because the network did not answer.
    Stale {
        /// The cache's age in seconds.
        age_secs: u64,
        /// Why the refresh failed, for the UI to report.
        reason: String,
    },
}

impl CatalogSource {
    /// Whether the answer came from the network.
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        matches!(self, Self::Fetched)
    }

    /// A short label for the status line and `:model show`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Fetched => "fetched now",
            Self::Cached { .. } => "cached",
            Self::Stale { .. } => "stale cache",
        }
    }

    /// The age in seconds, when the answer came from disk.
    #[must_use]
    pub fn age_secs(&self) -> Option<u64> {
        match self {
            Self::Fetched => None,
            Self::Cached { age_secs } | Self::Stale { age_secs, .. } => Some(*age_secs),
        }
    }
}

/// The catalog and how it was obtained.
#[derive(Debug, Clone)]
pub struct CatalogLoad {
    /// The parsed catalog.
    pub catalog: Catalog,
    /// Where it came from.
    pub source: CatalogSource,
}

/// Why no catalog could be produced at all.
///
/// A parse failure of the remote document and a failed fetch are different
/// problems with different advice, so they are different variants: one says "the
/// feed changed", the other says "you are offline".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogFetchError {
    /// The fetch failed and there was nothing usable on disk.
    #[error("the model catalog could not be fetched: {0}")]
    Unavailable(String),

    /// The document could not be parsed, and nothing usable was cached.
    #[error("the model catalog could not be read: {0}")]
    Malformed(String),
}

/// Anything that can produce a model catalog.
pub trait ModelCatalogPort: fmt::Debug + Send + Sync {
    /// Loads the catalog under the given policy.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogFetchError`] when no catalog can be produced at all. A
    /// stale cache is a successful answer, not an error: the caller learns about it
    /// from [`CatalogSource`].
    fn load(&self, policy: CatalogPolicy) -> Result<CatalogLoad, CatalogFetchError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_describe_themselves() {
        assert!(CatalogSource::Fetched.is_fresh());
        assert_eq!(CatalogSource::Fetched.age_secs(), None);
        let cached = CatalogSource::Cached { age_secs: 10 };
        assert!(!cached.is_fresh());
        assert_eq!(cached.label(), "cached");
        assert_eq!(cached.age_secs(), Some(10));

        let stale = CatalogSource::Stale {
            age_secs: 90,
            reason: "offline".to_owned(),
        };
        assert_eq!(stale.label(), "stale cache");
        assert_eq!(stale.age_secs(), Some(90));
    }

    #[test]
    fn the_two_failures_say_different_things() {
        let offline = CatalogFetchError::Unavailable("connection refused".to_owned());
        assert!(offline.to_string().contains("could not be fetched"));
        let malformed = CatalogFetchError::Malformed("expected value".to_owned());
        assert!(malformed.to_string().contains("could not be read"));
    }
}
