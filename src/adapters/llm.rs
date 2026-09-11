//! LLM adapter, implemented in M2 (FR-4.4, FR-4.8, FR-5.2).
//!
//! It will implement `LlmPort` on top of the `llm` crate, using the
//! `openrouter` and `deepseek` features. Two constraints are already known from
//! the crate (Appendix B of the requirements) and shape this module:
//!
//! - reasoning is supported outbound (`reasoning`, `reasoning_effort`,
//!   `reasoning_budget_tokens`) and reported as `usage.reasoning_tokens`;
//! - the reasoning *text* is not returned, so the UI must never promise a
//!   visible thinking trace (DEC-18).
