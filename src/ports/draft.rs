//! Where review drafts live between runs (FR-6.1, NFR-4.1).
//!
//! A draft is the opposite of a cache entry: nobody can regenerate it, and it is the
//! record of what the user was in the middle of saying. It therefore lives in its own
//! directory next to `exports/` rather than under `cache/`, so nothing that cleans
//! cache has any business deleting it, and it is replaced atomically so a crash
//! mid-write cannot leave half a review behind (NFR-4.1).

use crate::domain::draft::Draft;
use crate::domain::repo::RepoId;

/// What a draft store can do.
pub trait DraftStorePort: std::fmt::Debug + Send + Sync {
    /// Reads the draft for a pull request, if there is one.
    ///
    /// # Errors
    ///
    /// Returns an error when the file exists but cannot be read or understood. A
    /// *missing* file is `Ok(None)`: most pull requests have no draft.
    fn load(&self, repo: &RepoId, number: u64) -> Result<Option<Draft>, DraftStoreError>;

    /// Replaces a draft, atomically.
    ///
    /// The pull request comes from the document rather than from a second argument:
    /// with both, a caller could write a file that disagrees with itself, and the
    /// file is what a later run reads.
    ///
    /// # Errors
    ///
    /// Returns an error when the document cannot be written.
    fn save(&self, repo: &RepoId, draft: &Draft) -> Result<(), DraftStoreError>;

    /// Deletes the draft for a pull request.
    ///
    /// Deleting a draft that is not there is not an error: `:draft clear` on an empty
    /// draft and a cleared draft after a publish mean the same thing.
    ///
    /// # Errors
    ///
    /// Returns an error when a file exists and cannot be removed.
    fn remove(&self, repo: &RepoId, number: u64) -> Result<(), DraftStoreError>;

    /// Every draft for a repository, with the pull request each belongs to.
    ///
    /// Used to say "you have unsent comments on three pull requests" at startup, and
    /// to list them in the draft panel.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be read.
    fn list(&self, repo: &RepoId) -> Result<Vec<Draft>, DraftStoreError>;
}

/// Why a draft could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum DraftStoreError {
    /// The file could not be read or written.
    #[error("could not {action} {path}: {source}")]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// The file.
        path: std::path::PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The file exists but is not a draft this build understands.
    #[error("{path} is not a usable draft: {reason}")]
    Malformed {
        /// The file.
        path: std::path::PathBuf,
        /// What was wrong with it.
        reason: String,
    },
}
