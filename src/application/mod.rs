//! Use cases (ARCH-1).
//!
//! Orchestrates work in terms of the ports and emits domain values. It must not
//! import `ratatui`, `crossterm` or spawn processes directly: everything that
//! touches the outside world goes through a trait, which is what lets every rule in
//! here be tested against an in-memory fake with no network, no repository and no
//! terminal (NFR-5.2).
//!
//! The functions are stateless: they take the ports they need, do one thing, and
//! return a value or a typed error. The event loop decides when to call them and
//! what to do with the answer.

pub mod analysis;
pub mod chat;
pub mod context;
pub mod environment;
pub mod models;
pub mod prs;

pub use environment::{DetectRequest, detect, gh_program};
pub use models::{
    CatalogState, ModelChoice, ProviderChoice, ResolvedSelection, SelectionError, check_thinking,
    clear_key, connection_check, key_status, load_catalog, model_choices, provider_choices,
    resolve_selection, save_key,
};
pub use prs::{CachePolicy, Cached, FetchOutcome, Prs};
