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

pub mod environment;
pub mod prs;

pub use environment::{DetectRequest, detect, gh_program};
pub use prs::{CachePolicy, Cached, FetchOutcome, Prs};
