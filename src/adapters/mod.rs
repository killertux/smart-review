//! Adapters implement the ports against the outside world (ARCH-3).
//!
//! M0 ships the filesystem and clock adapters. The remaining modules are
//! declared now so the layout matches `ARCHITECTURE.md`, and are filled in by
//! the milestone named in each file.

pub mod clock;
pub mod fs;
pub mod gh;
pub mod git;
pub mod llm;
