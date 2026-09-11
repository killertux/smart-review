//! Domain types and invariants (ARCH-1).
//!
//! This layer depends on nothing else in the crate and knows nothing about
//! terminals, processes or HTTP. It may use `serde` for persistence and `chrono`
//! for time, both of which are data utilities rather than IO.
//!
//! Everything here is a value: no type in this module performs IO, holds a
//! resource, or knows where its data came from. That is what lets the diff
//! parser, the filter grammar and the check roll-up be tested without a network,
//! a repository, or a terminal (NFR-5.2).

pub mod diff;
pub mod environment;
pub mod pr;
pub mod query;
pub mod repo;
pub mod time;

pub use diff::{
    DiffLine, FileDiff, FileKind, FileStatus, Hunk, LineKind, Patch, PatchStats, RelPath,
};
pub use environment::{Environment, EnvironmentError, GhInstall, RunMode};
pub use pr::{
    CheckRun, CheckState, CheckSummary, Commit, PrState, PullRequestDetail, PullRequestRef,
    PullRequestSummary, Review, ReviewComment, ReviewDecision, ReviewState,
};
pub use query::{Filter, FilterError, PrQuery, PrSort, PrStateFilter, ReviewFilter};
pub use repo::{RepoId, RepoIdError};
pub use time::Timestamp;
