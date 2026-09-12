//! The LLM port (FR-4.4, FR-4.8, ARCH-5).
//!
//! The shape follows what the app actually needs to guarantee:
//!
//! - **cancellation is a flag, not a channel** (ARCH-5): a request that is no longer
//!   wanted is abandoned by polling [`Cancel`], so a cancelled call cannot deadlock
//!   waiting for someone to read its output;
//! - **tokens are reported, not guessed** (FR-4.8): every answer carries the
//!   provider's own usage numbers, including reasoning tokens;
//! - **no thinking trace is promised** (DEC-18): [`ChatOutcome::thinking`] is
//!   populated only when a backend actually returns one, and the UI says nothing
//!   about a trace otherwise.
//!
//! M2a uses [`LlmPort::complete`] for the picker's connection check, which is the
//! only way to know a selection works before spending an analysis on it. M2b uses
//! [`LlmPort::stream`] for the analysis itself.

use std::fmt;

use crate::domain::model::{Route, ThinkingRequest};
use crate::ports::Cancel;
use crate::ports::secret::ApiKey;

/// Where to send a request, and what to send.
#[derive(Clone)]
pub struct ChatRequest {
    /// Provider id, for error messages and usage accounting.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// How the provider is reached (DEC-17).
    pub route: Route,
    /// The provider's base URL, when it has one. The passthrough needs it.
    pub base_url: Option<String>,
    /// The key, with its source.
    pub api_key: ApiKey,
    /// System prompt.
    pub system: Option<String>,
    /// Earlier turns, oldest first, as `(role, text)` (FR-5.2, FR-5.3).
    ///
    /// Empty for one-shot requests (an analysis, a connection check). A chat sends the
    /// conversation it wants the model to have, already trimmed
    /// ([`crate::domain::chat::replayable_history`]) — the port carries roles rather
    /// than a flattened transcript because providers accept roles, and flattening them
    /// here would throw away the distinction every provider makes.
    pub history: Vec<(crate::domain::chat::Role, String)>,
    /// The user message.
    pub prompt: String,
    /// Output cap. The catalog's `limit.output` bounds it (FR-4.7).
    pub max_tokens: Option<u32>,
    /// Sampling temperature, when the model accepts one.
    pub temperature: Option<f32>,
    /// Thinking settings, already mapped and validated (FR-4.8).
    pub thinking: Option<ThinkingRequest>,
    /// Per-request timeout.
    pub timeout_secs: u64,
}

impl fmt::Debug for ChatRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `ApiKey` already redacts itself; this impl exists to keep the prompt out
        // of traces, because the prompt contains the user's code (NFR-3.1).
        f.debug_struct("ChatRequest")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("route", &self.route)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key)
            .field("system", &self.system.as_ref().map(|_| "<set>"))
            // The history is the user's own words and the model's answers, so it is
            // described rather than printed, for the same reason the prompt is.
            .field("history", &format!("{} messages", self.history.len()))
            .field("prompt_bytes", &self.prompt.len())
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("thinking", &self.thinking)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

impl ChatRequest {
    /// A request with defaults filled in from the catalog bounds.
    #[must_use]
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        route: Route,
        api_key: ApiKey,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            route,
            base_url: None,
            api_key,
            system: None,
            history: Vec::new(),
            prompt: prompt.into(),
            max_tokens: None,
            temperature: None,
            thinking: None,
            timeout_secs: 120,
        }
    }
}

/// The provider's own token accounting.
///
/// Serializable because a chat session stores what each answer cost next to the
/// answer (FR-5.4): a cost that could only be recomputed while the process was alive
/// would make the figure on screen a claim rather than a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    /// Prompt tokens.
    #[serde(default)]
    pub prompt: u32,
    /// Completion tokens.
    #[serde(default)]
    pub completion: u32,
    /// Total, as the provider reports it.
    #[serde(default)]
    pub total: u32,
    /// Reasoning tokens, when the provider breaks them out (FR-4.8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u32>,
}

impl TokenUsage {
    /// Adds another answer's usage to this one, for a session total (FR-5.4).
    pub fn add(&mut self, other: Self) {
        self.prompt = self.prompt.saturating_add(other.prompt);
        self.completion = self.completion.saturating_add(other.completion);
        self.total = self.total.saturating_add(other.total);
        self.reasoning = match (self.reasoning, other.reasoning) {
            (Some(left), Some(right)) => Some(left.saturating_add(right)),
            (Some(left), None) | (None, Some(left)) => Some(left),
            (None, None) => None,
        };
    }
}

/// What a request produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatOutcome {
    /// The assistant's text.
    pub text: String,
    /// Token usage, when the provider reported it.
    pub usage: Option<TokenUsage>,
    /// A thinking trace, when the backend returns one.
    ///
    /// `None` is the normal case (DEC-18). The field exists so that a backend which
    /// does return one is not thrown away, not because the UI may promise it.
    pub thinking: Option<String>,
}

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    /// The key is missing or was rejected.
    #[error("{provider} rejected the request: {reason}")]
    Auth {
        /// The provider.
        provider: String,
        /// The provider's message.
        reason: String,
    },

    /// The provider refused the request for another reason.
    #[error("{provider} refused the request: {reason}")]
    Request {
        /// The provider.
        provider: String,
        /// The provider's message.
        reason: String,
    },

    /// The request timed out.
    #[error("{provider} did not answer within {timeout_secs}s")]
    Timeout {
        /// The provider.
        provider: String,
        /// The timeout that elapsed.
        timeout_secs: u64,
    },

    /// The caller cancelled it.
    #[error("the request was cancelled")]
    Cancelled,

    /// The provider is not reachable through this build (DEC-17).
    #[error("{provider} cannot be reached by this build")]
    Unroutable {
        /// The provider.
        provider: String,
    },

    /// The transport failed.
    #[error("the request to {provider} failed: {reason}")]
    Transport {
        /// The provider.
        provider: String,
        /// The underlying message.
        reason: String,
    },
}

/// A streamed delta, handed to the caller as it arrives (M2b).
pub type DeltaHandler<'a> = dyn FnMut(&str) + Send + 'a;

/// Anything that can talk to an LLM provider.
pub trait LlmPort: fmt::Debug + Send + Sync {
    /// One non-streamed completion.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError`] describing what the provider said, never a bare panic:
    /// a rejected key and an unreachable endpoint are normal outcomes here.
    fn complete(&self, request: &ChatRequest, cancel: &Cancel) -> Result<ChatOutcome, LlmError>;

    /// The same request, with text handed to `on_delta` as it arrives.
    ///
    /// The returned outcome is the whole answer, so a caller that only wants the
    /// final text can ignore the deltas and get the same value.
    ///
    /// # Errors
    ///
    /// As [`LlmPort::complete`].
    fn stream(
        &self,
        request: &ChatRequest,
        cancel: &Cancel,
        on_delta: &mut DeltaHandler<'_>,
    ) -> Result<ChatOutcome, LlmError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::NativeBackend;
    use crate::ports::secret::KeySource;

    fn request() -> ChatRequest {
        ChatRequest::new(
            "deepseek",
            "deepseek-v4-pro",
            Route::Native(NativeBackend::DeepSeek),
            ApiKey::new("sk-secret", KeySource::File),
            "review this",
        )
    }

    #[test]
    fn a_request_describes_its_history_rather_than_printing_it() {
        let mut request = request();
        request.history = vec![
            (
                crate::domain::chat::Role::User,
                "what does money() do?".to_owned(),
            ),
            (
                crate::domain::chat::Role::Assistant,
                "it sums lines".to_owned(),
            ),
        ];
        let debug = format!("{request:?}");
        assert!(debug.contains("2 messages"), "{debug}");
        assert!(!debug.contains("sums lines"), "{debug}");
        assert!(!debug.contains("money()"), "{debug}");
    }

    #[test]
    fn a_request_does_not_print_its_key_or_its_prompt() {
        let mut request = request();
        request.system = Some("system prompt".to_owned());
        let debug = format!("{request:?}");
        assert!(!debug.contains("sk-secret"), "{debug}");
        assert!(!debug.contains("review this"), "{debug}");
        assert!(debug.contains("prompt_bytes: 11"), "{debug}");
        assert!(debug.contains("<set>"), "{debug}");
    }

    #[test]
    fn a_new_request_has_sane_defaults() {
        let request = request();
        assert_eq!(request.timeout_secs, 120);
        assert!(request.thinking.is_none());
        assert!(request.base_url.is_none(), "the adapter fills this in");
    }

    #[test]
    fn errors_name_the_provider_and_the_next_step() {
        let error = LlmError::Auth {
            provider: "deepseek".to_owned(),
            reason: "401 Unauthorized".to_owned(),
        };
        let message = error.to_string();
        assert!(message.contains("deepseek"), "{message}");
        assert!(message.contains("401"), "{message}");
        assert_eq!(LlmError::Cancelled.to_string(), "the request was cancelled");
    }

    #[test]
    fn usage_carries_reasoning_tokens_when_the_provider_reports_them() {
        let usage = TokenUsage {
            prompt: 100,
            completion: 50,
            total: 150,
            reasoning: Some(1200),
        };
        assert_eq!(usage.reasoning, Some(1200));
        assert_eq!(TokenUsage::default().reasoning, None);
    }
}
