//! Ports: the traits the rest of the application depends on (ARCH-2).
//!
//! Each port represents a real external boundary with an adapter and test fake. The
//! crate deliberately avoids traits that merely wrap internal implementation details.

pub mod analysis;
pub mod cache;
pub mod cancel;
pub mod catalog;
pub mod chat;
pub mod draft;
pub mod forge;
pub mod llm;
pub mod mutation;
pub mod secret;
pub mod workspace;

pub use analysis::{AnalysisCacheError, AnalysisCachePort, AnalysisKey, StoredAnalysis};
pub use cache::{CacheKey, CacheKeyError, CacheStore, Stored};
pub use cancel::Cancel;
pub use catalog::{CatalogFetchError, CatalogLoad, CatalogPolicy, CatalogSource, ModelCatalogPort};
pub use chat::{ChatStoreError, ChatStorePort};
pub use draft::{DraftStoreError, DraftStorePort};
pub use forge::{
    CommentPosted, ForgeCapabilities, ForgeFactory, ForgePort, ForgeProbe, ForgeStatus,
    PullRequestPage, ReviewPosted,
};
pub use llm::{ChatOutcome, ChatRequest, DeltaHandler, LlmError, LlmPort, TokenUsage};
pub use mutation::{MutationStoreError, MutationStorePort};
pub use secret::{ApiKey, KeySource, KeyStatus, SecretError, SecretStore};
pub use workspace::{
    DiffOptions, DiffRequest, Remote, RepoInfo, Workspace, WorkspaceEntry, WorkspaceError,
    WorkspacePort, WorkspaceRequest,
};

use crate::config::Loaded;
use crate::error::Result;
use crate::state::AppState;

/// Time, injected so cache lifetimes and relative timestamps are testable
/// (NFR-5.2).
pub trait Clock: std::fmt::Debug + Send + Sync {
    /// Seconds since the Unix epoch.
    fn now_unix_secs(&self) -> u64;
}

/// Reads the user's configuration (FR-8.2).
///
/// Model-selection write-back uses the preserved configuration document rather than
/// this read-oriented port (DEC-19).
pub trait ConfigStore: std::fmt::Debug {
    /// Reads the file, applying built-in defaults for anything missing.
    ///
    /// # Errors
    ///
    /// Returns an error when the configuration cannot be read or parsed.
    fn load(&self) -> Result<Loaded>;

    /// Where the configuration lives.
    fn path(&self) -> &std::path::Path;
}

/// Reads and writes small persisted state (FR-8.5).
pub trait StateStore: std::fmt::Debug + Send + Sync {
    /// Reads the state, returning defaults when the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error when the state cannot be read or parsed.
    fn load(&self) -> Result<AppState>;

    /// Writes the state atomically.
    ///
    /// # Errors
    ///
    /// Returns an error when the state cannot be written.
    fn save(&self, state: &AppState) -> Result<()>;
}
