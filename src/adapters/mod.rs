//! Adapters implement the ports against the outside world (ARCH-3).
//!
//! The filesystem, clock and process adapters provide shared infrastructure for the
//! forge, Git, LLM, cache and durable-document adapters below.

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
