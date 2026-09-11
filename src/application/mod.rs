//! Use cases (ARCH-1).
//!
//! Orchestrates work in terms of the ports and emits domain events. It must not
//! import `ratatui`, `crossterm` or spawn processes directly.
//!
//! Arriving with M1: `list_pull_requests`, `open_pull_request`. With M2:
//! `analyze_pull_request`, `build_context_bundle`. With M3/M4: `chat`,
//! `publish_review`.
