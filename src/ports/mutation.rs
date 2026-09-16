//! Durable operation records for remote GitHub mutations (IR-07).

use crate::domain::mutation::MutationOperation;
use crate::domain::repo::RepoId;

/// Failures while recording or recovering a remote mutation.
#[derive(Debug, thiserror::Error)]
pub enum MutationStoreError {
    /// The operation document could not be read or written.
    #[error("could not {action} {path}: {source}")]
    Io {
        /// The attempted operation.
        action: &'static str,
        /// The affected path.
        path: std::path::PathBuf,
        /// The underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The stored operation cannot be understood safely.
    #[error("{path} is not a readable mutation record: {reason}")]
    Malformed {
        /// The affected path.
        path: std::path::PathBuf,
        /// The parse or compatibility refusal.
        reason: String,
    },
    /// An operation id already exists and may belong to another dispatch.
    #[error(
        "a mutation operation with this id already exists; reopen the pull request before retrying"
    )]
    Conflict,
}

/// Stores confirmed remote-mutation snapshots independently of transient UI state.
pub trait MutationStorePort: std::fmt::Debug + Send + Sync {
    /// Persists a newly confirmed operation before network dispatch.
    ///
    /// # Errors
    ///
    /// Returns [`MutationStoreError::Conflict`] if that id already exists, or an IO or
    /// parse error that must prevent dispatch.
    fn create(&self, operation: &MutationOperation) -> Result<(), MutationStoreError>;

    /// Replaces an existing operation after a state transition.
    ///
    /// # Errors
    ///
    /// Returns an error when the transition cannot be made durable.
    fn save(&self, operation: &MutationOperation) -> Result<(), MutationStoreError>;

    /// Lists unfinished operations for one pull request so startup can reconcile them.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be read. Individual malformed files
    /// are left in place and reported through the error rather than silently discarded.
    fn unresolved(
        &self,
        repo: &RepoId,
        pr: u64,
    ) -> Result<Vec<MutationOperation>, MutationStoreError>;
}
