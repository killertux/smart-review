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

pub mod analysis;
pub mod chat;
pub mod context;
pub mod diff;
pub mod draft;
pub mod environment;
pub mod model;
pub mod mutation;
pub mod plan;
pub mod pr;
pub mod query;
pub mod repo;
pub mod time;

pub use analysis::{
    ANALYSIS_VERSION, Analysis, AnalysisUsage, Coverage, Evidence, FileNote, PROMPT_VERSION,
    ParseFailure, PathIndex, PlanGroup, RiskArea, Severity, UNCLASSIFIED,
};
pub use chat::{
    CHAT_PROMPT_VERSION, Message, Pruned, Reference, Role, Session, SessionMeta, Totals,
};
pub use context::{
    Bundle, BundleInputs, BundlePolicy, Disposition, Segment, SegmentKind, estimate_tokens,
};
pub use diff::{
    DiffLine, DiffSource, FileDiff, FileKind, FileStatus, Hunk, LineKind, Patch, PatchStats,
    RelPath,
};
pub use environment::{Environment, EnvironmentError, GhInstall, RunMode};
pub use model::{
    Catalog, CatalogError, CatalogModel, Cost, EffortLevel, Limit, NativeBackend, Provider,
    ReasoningOption, Route, Thinking, ThinkingChoice, ThinkingError, ThinkingRequest,
};
pub use mutation::{MUTATION_VERSION, MutationKind, MutationOperation, MutationState};
pub use plan::{FileReview, OrderMode, Plan, PlanSource, ReviewStatus};
pub use pr::{
    CheckRun, CheckState, CheckSummary, Commit, PrState, PullRequestDetail, PullRequestRef,
    PullRequestSummary, Review, ReviewComment, ReviewDecision, ReviewState,
};
pub use query::{Filter, FilterError, PrQuery, PrSort, PrStateFilter, ReviewFilter};
pub use repo::{RepoId, RepoIdError};
pub use time::{Timestamp, from_unix_secs};
