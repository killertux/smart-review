//! Adapters implement the ports against the outside world (ARCH-3).
//!
//! M0 ships the filesystem and clock adapters and the process runner that every
//! other adapter is built on. The remaining modules are declared now so the
//! layout matches `ARCHITECTURE.md`, and are filled in by the milestone named in
//! each file.

pub mod analysis_cache;
pub mod cache;
pub mod catalog;
pub mod chat_store;
pub mod clock;
pub mod credentials;
pub mod draft_store;
pub mod fs;
pub mod gh;
pub mod git;
pub mod http;
pub mod llm;
pub mod mutation_store;
pub mod process;
