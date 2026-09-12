//! The `llm` adapter (FR-4.4, FR-4.8, DEC-17, DEC-18).
//!
//! One translation layer between the app's [`LlmPort`] and the `llm` crate, and it
//! exists to keep three rules in one place:
//!
//! - **routing (DEC-17).** A provider with a native backend uses it; anything else
//!   with a published base URL goes through the crate's OpenAI-compatible backend
//!   with that URL; a provider this build cannot reach never gets here, because the
//!   picker hides it.
//! - **thinking (FR-4.8).** The mapping is `.reasoning(bool)` /
//!   `.reasoning_effort(..)` / `.reasoning_budget_tokens(n)`, and it is *total*:
//!   [`ThinkingRequest`] has no variant the crate cannot express, because
//!   `domain::model` refuses the others before a request is built.
//! - **no trace is promised (DEC-18).** `ChatOutcome::thinking` is filled in from
//!   `ChatResponse::thinking`, which only the Anthropic backend implements today.
//!   It is passed through when present and left `None` otherwise; the UI never
//!   claims a trace exists.
//!
//! Streaming is driven on one current-thread runtime for the whole response, not one
//! per chunk: the deltas arrive on a job thread that is allowed to block, and the
//! caller's closure runs between polls (see `adapters/http.rs` for why a runtime is
//! not kept alive).

use futures::StreamExt;
use llm::builder::{LLMBackend, LLMBuilder};
use llm::chat::ChatProvider;
use llm::providers::openai_compatible::{OpenAICompatibleProvider, OpenAIProviderConfig};
// `chat`, `usage` and `thinking` come from `ChatProvider`/`ChatResponse`, which are
// supertraits of the `llm::LLMProvider` this module returns; naming them again here
// would be redundant.
use llm::chat::{ChatMessage, ReasoningEffort};

use crate::adapters::http::block_on;
use crate::domain::model::{EffortLevel, NativeBackend, Route, ThinkingRequest};
use crate::logging::{self, Level};
use crate::ports::Cancel;
use crate::ports::llm::{ChatOutcome, ChatRequest, DeltaHandler, LlmError, LlmPort, TokenUsage};

/// The `llm`-crate-backed implementation of [`LlmPort`].
#[derive(Debug, Default, Clone, Copy)]
pub struct LlmCrate;

impl LlmCrate {
    /// The adapter. It holds no state: every request carries its own configuration,
    /// so nothing to configure and nothing to leak between calls.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Builds a configured provider for one request.
    ///
    /// Two kinds, because the crate has two: its own backends through the builder, and
    /// the generic OpenAI-compatible provider for the passthrough route (DEC-17). Only
    /// [`ChatProvider`] is needed, which is what lets a passthrough provider be built
    /// directly instead of through the builder, whose `OpenAI` backend speaks the
    /// Responses API rather than `/chat/completions` — see [`passthrough`].
    fn provider(request: &ChatRequest) -> Result<Box<dyn ChatProvider>, LlmError> {
        if let Route::Passthrough { base_url } = &request.route {
            return passthrough(request, base_url);
        }
        let (backend, base_url) = route(request)?;
        let mut builder = LLMBuilder::new()
            .backend(backend)
            .model(request.model.clone())
            .api_key(request.api_key.expose())
            .timeout_seconds(request.timeout_secs);
        if let Some(url) = base_url {
            builder = builder.base_url(url);
        }
        if let Some(system) = &request.system {
            builder = builder.system(system.clone());
        }
        if let Some(max_tokens) = request.max_tokens {
            builder = builder.max_tokens(max_tokens);
        }
        if let Some(temperature) = request.temperature {
            builder = builder.temperature(temperature);
        }
        builder = match request.thinking {
            None => builder,
            Some(ThinkingRequest::Reasoning(on)) => builder.reasoning(on),
            Some(ThinkingRequest::Effort(level)) => builder.reasoning_effort(match level {
                EffortLevel::Low => ReasoningEffort::Low,
                EffortLevel::Medium => ReasoningEffort::Medium,
                EffortLevel::High => ReasoningEffort::High,
            }),
            Some(ThinkingRequest::BudgetTokens(tokens)) => builder.reasoning_budget_tokens(tokens),
        };
        // `LLMProvider` is a `ChatProvider`, and upcasting the box is stable since
        // Rust 1.86 (the MSRV here is 1.88).
        builder
            .build()
            .map(|provider| provider as Box<dyn ChatProvider>)
            .map_err(|error| translate(&request.provider, &error))
    }

    /// The conversation as the crate's message types.
    ///
    /// The system prompt is *not* here: `ChatRole` has no system variant, so the crate
    /// takes it through the builder, which is why the chat path puts the context bundle
    /// there rather than in the first message (FR-5.3).
    fn messages(request: &ChatRequest) -> Vec<ChatMessage> {
        let mut messages: Vec<ChatMessage> = request
            .history
            .iter()
            .map(|(role, text)| match role {
                crate::domain::chat::Role::User => {
                    ChatMessage::user().content(text.clone()).build()
                }
                crate::domain::chat::Role::Assistant => {
                    ChatMessage::assistant().content(text.clone()).build()
                }
            })
            .collect();
        messages.push(ChatMessage::user().content(request.prompt.clone()).build());
        messages
    }
}

/// A provider for the passthrough route, speaking `/chat/completions` (DEC-17).
///
/// Built from the crate's generic OpenAI-compatible provider rather than from
/// `LLMBackend::OpenAI`, and that distinction is the whole point: the crate's `OpenAI`
/// backend talks to the **Responses API** (`/responses`) for chat and streaming, which
/// `OpenAI` itself implements and which almost no other "OpenAI-compatible" provider
/// does. Pointing that backend at another provider's base URL would 404 on every
/// request, so the passthrough builds the compatible provider directly.
fn passthrough(request: &ChatRequest, base_url: &str) -> Result<Box<dyn ChatProvider>, LlmError> {
    let url = base_url.trim();
    if url.is_empty() {
        return Err(LlmError::Unroutable {
            provider: request.provider.clone(),
        });
    }
    let reasoning_effort = match request.thinking {
        Some(ThinkingRequest::Effort(level)) => Some(level.as_str().to_owned()),
        _ => None,
    };
    let provider = OpenAICompatibleProvider::<Compatible>::new(
        request.api_key.expose(),
        Some(url.to_owned()),
        Some(request.model.clone()),
        request.max_tokens,
        request.temperature,
        Some(request.timeout_secs),
        request.system.clone(),
        None,
        None,
        None,
        None,
        reasoning_effort,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    Ok(Box::new(provider))
}

/// The configuration of a provider reached by passthrough.
///
/// Everything here is a default, because the catalog gives the app a base URL and a
/// model and nothing else: what a provider supports is discovered from its answers, not
/// from local knowledge, which is why the capabilities are stated as absences.
struct Compatible;

impl OpenAIProviderConfig for Compatible {
    const PROVIDER_NAME: &'static str = "OpenAI-compatible";
    const DEFAULT_BASE_URL: &'static str = "https://example.invalid/";
    const DEFAULT_MODEL: &'static str = "";
    /// `/chat/completions`, which is what makes this route useful (DEC-17).
    const CHAT_ENDPOINT: &'static str = "chat/completions";
    /// Reasoning *toggles* are not expressed here: the crate's compatible provider
    /// accepts an effort string, and the domain refuses an option it cannot send
    /// (FR-4.8) rather than this adapter guessing.
    const SUPPORTS_REASONING_EFFORT: bool = true;
}

/// The crate's backend and the base URL to use, if any (DEC-17).
fn route(request: &ChatRequest) -> Result<(LLMBackend, Option<String>), LlmError> {
    match &request.route {
        Route::Native(NativeBackend::OpenAI) => Ok((LLMBackend::OpenAI, request.base_url.clone())),
        Route::Native(NativeBackend::Anthropic) => Ok((LLMBackend::Anthropic, None)),
        Route::Native(NativeBackend::OpenRouter) => Ok((LLMBackend::OpenRouter, None)),
        Route::Native(NativeBackend::DeepSeek) => Ok((LLMBackend::DeepSeek, None)),
        Route::Native(NativeBackend::Google) => Ok((LLMBackend::Google, None)),
        Route::Native(NativeBackend::Groq) => Ok((LLMBackend::Groq, None)),
        Route::Native(NativeBackend::Mistral) => Ok((LLMBackend::Mistral, None)),
        Route::Native(NativeBackend::Xai) => Ok((LLMBackend::XAI, None)),
        // Handled before this function is reached: a passthrough provider is built
        // rather than named (see `passthrough`).
        Route::Passthrough { .. } => Err(LlmError::Unroutable {
            provider: request.provider.clone(),
        }),
    }
}

/// Turns a crate error into one the UI can act on.
fn translate(provider: &str, error: &llm::error::LLMError) -> LlmError {
    let text = error.to_string();
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("401")
        || lowered.contains("unauthorized")
        || lowered.contains("invalid api key")
        || lowered.contains("authentication")
        || lowered.contains("auth error")
    {
        return LlmError::Auth {
            provider: provider.to_owned(),
            reason: first_line(&text),
        };
    }
    if lowered.contains("timed out") || lowered.contains("timeout") {
        return LlmError::Timeout {
            provider: provider.to_owned(),
            timeout_secs: 0,
        };
    }
    // Everything else is the provider refusing or misbehaving, which the user reads
    // in the provider's own words: only a transport failure is a different kind of
    // problem (the network), and only auth and timeouts are worth their own names.
    match error {
        llm::error::LLMError::HttpError(_) => LlmError::Transport {
            provider: provider.to_owned(),
            reason: first_line(&text),
        },
        _ => LlmError::Request {
            provider: provider.to_owned(),
            reason: first_line(&text),
        },
    }
}

/// The first line of a provider message: the rest is usually a JSON body or a
/// backtrace, and the first line is what names the problem.
fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(text)
        .trim()
        .to_owned()
}

/// The crate's usage numbers, in the app's shape (FR-4.8).
fn usage_of(usage: Option<llm::chat::Usage>) -> Option<TokenUsage> {
    usage.map(|usage| TokenUsage {
        prompt: usage.prompt_tokens,
        completion: usage.completion_tokens,
        total: usage.total_tokens,
        reasoning: usage
            .completion_tokens_details
            .and_then(|details| details.reasoning_tokens),
    })
}

impl LlmPort for LlmCrate {
    fn complete(&self, request: &ChatRequest, cancel: &Cancel) -> Result<ChatOutcome, LlmError> {
        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        let provider = Self::provider(request)?;
        let messages = Self::messages(request);
        logging::log(
            Level::Debug,
            format!(
                "asking {} for {} ({} prompt bytes)",
                request.provider,
                request.model,
                request.prompt.len()
            ),
        );

        let answer = block_on(async { provider.chat(&messages).await })
            .map_err(|error| LlmError::Transport {
                provider: request.provider.clone(),
                reason: error.to_string(),
            })?
            .map_err(|error| translate(&request.provider, &error))?;

        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        Ok(ChatOutcome {
            text: answer.text().unwrap_or_default(),
            usage: usage_of(answer.usage()),
            thinking: answer.thinking(),
        })
    }

    fn stream(
        &self,
        request: &ChatRequest,
        cancel: &Cancel,
        on_delta: &mut DeltaHandler<'_>,
    ) -> Result<ChatOutcome, LlmError> {
        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        let provider = Self::provider(request)?;
        let messages = Self::messages(request);

        // One runtime for the whole response: the stream is pulled inside a single
        // `block_on`, so a long answer does not build a runtime per chunk.
        let outcome = block_on(async {
            let mut stream = provider
                .chat_stream_struct(&messages)
                .await
                .map_err(|error| translate(&request.provider, &error))?;
            let mut text = String::new();
            let mut usage = None;
            while let Some(item) = stream.next().await {
                // Checked between chunks: cancellation is a flag, and the cost of
                // finishing an answer nobody wants is the user's money (ARCH-5).
                if cancel.is_cancelled() {
                    return Err(LlmError::Cancelled);
                }
                match item {
                    Ok(response) => {
                        for choice in response.choices {
                            if let Some(content) = choice.delta.content
                                && !content.is_empty()
                            {
                                text.push_str(&content);
                                on_delta(&content);
                            }
                        }
                        if response.usage.is_some() {
                            usage = response.usage;
                        }
                    }
                    Err(error) => return Err(translate(&request.provider, &error)),
                }
            }
            Ok((text, usage_of(usage)))
        })
        .map_err(|error| LlmError::Transport {
            provider: request.provider.clone(),
            reason: error.to_string(),
        })??;

        Ok(ChatOutcome {
            text: outcome.0,
            usage: outcome.1,
            // Streaming carries no thinking blocks in this crate version, so the
            // field stays empty rather than pretending otherwise (DEC-18).
            thinking: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{Catalog, NativeBackend};
    use crate::ports::secret::{ApiKey, KeySource};

    fn catalog() -> Catalog {
        Catalog::from_json(include_str!("../../tests/fixtures/models/providers.json"))
            .expect("the fixture parses")
    }

    fn request(provider: &str, model: &str) -> ChatRequest {
        let catalog = catalog();
        let provider_entry = catalog
            .provider(provider)
            .expect("a provider in the fixture");
        let route = provider_entry.route().expect("reachable");
        let mut request = ChatRequest::new(
            provider,
            model,
            route,
            ApiKey::new("sk-test", KeySource::File),
            "say ready",
        );
        request.base_url = provider_entry.api.clone();
        request
    }

    #[test]
    fn native_providers_use_their_own_backend() {
        let cases = [
            ("openai", LLMBackend::OpenAI),
            ("anthropic", LLMBackend::Anthropic),
            ("deepseek", LLMBackend::DeepSeek),
            ("openrouter", LLMBackend::OpenRouter),
            ("google", LLMBackend::Google),
            ("groq", LLMBackend::Groq),
        ];
        for (provider, expected) in cases {
            let (backend, url) = route(&request(provider, "m")).expect("routed");
            assert_eq!(backend, expected, "{provider}");
            assert!(url.is_none(), "{provider} needs no base URL");
        }
    }

    #[test]
    fn a_provider_with_only_a_base_url_goes_through_the_compatible_passthrough() {
        // Why this matters: the crate's OpenAI backend talks to the *Responses API*
        // (`/responses`) for both chat and streaming, which OpenAI implements and
        // almost no other OpenAI-compatible provider does. Pointing that backend at
        // another provider's base URL 404s on every request, so the passthrough builds
        // the compatible provider instead — and the endpoint below is the guarantee.
        assert_eq!(Compatible::CHAT_ENDPOINT, "chat/completions");
        let request = request("lmstudio", "qwen/qwen3-coder-30b");
        assert!(
            passthrough(&request, "http://127.0.0.1:1234/v1").is_ok(),
            "the compatible provider builds for any base URL"
        );
        // It is reachable through the adapter, which is what the app calls.
        assert!(LlmCrate::provider(&request).is_ok());
        // And `route` never sees a passthrough: a named backend is the other path.
        assert!(
            matches!(route(&request), Err(LlmError::Unroutable { .. })),
            "the passthrough is built, not named"
        );
    }

    #[test]
    fn a_passthrough_without_a_url_is_refused_rather_than_sent_somewhere() {
        let request = request("lmstudio", "m");
        // A blank base URL is not a provider: falling back to a default endpoint would
        // be a request to somebody else's service.
        assert!(matches!(
            passthrough(&request, "   "),
            Err(LlmError::Unroutable { .. })
        ));
        let mut blank = request.clone();
        blank.route = crate::domain::model::Route::Passthrough {
            base_url: "   ".to_owned(),
        };
        assert!(LlmCrate::provider(&blank).is_err());
    }

    #[test]
    fn the_backend_mapping_covers_every_native_backend() {
        // A new variant in `NativeBackend` must be routed deliberately, not fall
        // through to the passthrough by accident.
        for backend in NativeBackend::ALL {
            let mut request = request("openai", "m");
            request.route = crate::domain::model::Route::Native(*backend);
            assert!(route(&request).is_ok(), "{backend:?} has no crate backend");
        }
    }

    #[test]
    fn a_missing_key_is_reported_as_a_credential_problem() {
        let error = translate(
            "deepseek",
            &llm::error::LLMError::AuthError("401 Unauthorized: invalid api key".to_owned()),
        );
        assert!(matches!(error, LlmError::Auth { .. }), "{error:?}");
        assert!(error.to_string().contains("rejected"), "{error}");
    }

    #[test]
    fn a_transport_failure_is_not_reported_as_a_bad_request() {
        let error = translate(
            "openrouter",
            &llm::error::LLMError::HttpError(
                "error sending request: connection refused".to_owned(),
            ),
        );
        assert!(matches!(error, LlmError::Transport { .. }), "{error:?}");
    }

    #[test]
    fn a_provider_error_keeps_the_providers_own_words() {
        let error = translate(
            "deepseek",
            &llm::error::LLMError::ProviderError("model not found: gpt-9".to_owned()),
        );
        let message = error.to_string();
        assert!(message.contains("model not found: gpt-9"), "{message}");
        assert!(message.contains("deepseek"), "{message}");
    }

    #[test]
    fn only_the_first_line_of_a_long_provider_message_is_kept() {
        let error = translate(
            "openrouter",
            &llm::error::LLMError::ResponseFormatError {
                message: "no choices in response".to_owned(),
                raw_response: "{\"a\": 1}\n{\"b\": 2}".to_owned(),
            },
        );
        assert!(!error.to_string().contains("{\"b\""), "{error}");
    }

    #[test]
    fn usage_is_translated_including_reasoning_tokens() {
        let usage = llm::chat::Usage {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
            completion_tokens_details: Some(llm::chat::CompletionTokensDetails {
                reasoning_tokens: Some(7),
                audio_tokens: None,
            }),
            prompt_tokens_details: None,
        };
        let translated = usage_of(Some(usage)).expect("translated");
        assert_eq!(translated.prompt, 10);
        assert_eq!(translated.completion, 20);
        assert_eq!(translated.total, 30);
        assert_eq!(translated.reasoning, Some(7));
        assert_eq!(usage_of(None), None);
    }

    #[test]
    fn a_cancelled_request_is_not_sent_at_all() {
        let cancel = Cancel::new();
        cancel.cancel();
        let request = request("deepseek", "deepseek-v4-pro");
        let error = LlmCrate.complete(&request, &cancel).expect_err("not sent");
        assert_eq!(error, LlmError::Cancelled);
        let mut sink = |_: &str| {};
        let error = LlmCrate
            .stream(&request, &cancel, &mut sink)
            .expect_err("not sent");
        assert_eq!(error, LlmError::Cancelled);
    }

    #[test]
    fn a_thinking_request_maps_onto_the_crate_without_a_new_code_path() {
        // The mapping is data, not a branch per option: if a request can be built, it
        // can be sent (FR-4.8's "no silent downgrade" is enforced in `domain::model`).
        let request = request("deepseek", "deepseek-v4-pro");
        let model = catalog()
            .model("deepseek", "deepseek-v4-pro")
            .expect("a model")
            .clone();
        for choice in model.thinking_choices() {
            if let Ok(mapped) = choice.thinking.request(&model) {
                let mut with_thinking = request.clone();
                with_thinking.thinking = Some(mapped);
                assert!(
                    LlmCrate::provider(&with_thinking).is_ok(),
                    "{:?} could not be built",
                    choice.thinking
                );
            }
        }
    }
}
