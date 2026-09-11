//! Smart Review — a terminal client for reviewing GitHub pull requests.
//!
//! The crate is split along the layers described in `ARCHITECTURE.md`:
//!
//! - [`domain`] holds pure types and invariants and depends on nothing else.
//! - [`ports`] declares the traits the application layer talks to.
//! - [`application`] orchestrates use cases in terms of those ports.
//! - [`adapters`] implement the ports against the outside world.
//! - [`tui`] renders application state and turns key events into actions.
//!
//! The dependency rule is one-directional: `tui` → `application` → `ports`, and
//! `adapters` → `ports`. Nothing in `domain` or `application` may import
//! `ratatui`, `crossterm`, or spawn processes.
//!
//! Requirements are tracked in `REQUIREMENTS.md`; the reference for each item is
//! written in the doc comment of the code that implements it.

// Tests are allowed to unwrap and to print; production code is not (see `[lints]`).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr
    )
)]
#![forbid(unsafe_code)]

pub mod adapters;
pub mod application;
pub mod bootstrap;
pub mod cli;
pub mod config;
pub mod doctor;
pub mod domain;
pub mod error;
pub mod logging;
pub mod paths;
pub mod ports;
pub mod state;
pub mod tui;

#[cfg(test)]
pub(crate) mod test_support;

pub use bootstrap::Startup;
pub use error::{Error, Result};
