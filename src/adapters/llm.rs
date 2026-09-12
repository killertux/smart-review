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
    /// the generic `OpenAI`-compatible provider for the passthrough route (DEC-17). Only
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
/// Built from the crate's generic `OpenAI`-compatible provider rather than from
/// `LLMBackend::OpenAI`, and that distinction is the whole point: the crate's `OpenAI`
/// backend talks to the **Responses API** (`/responses`) for chat and streaming, which
/// `OpenAI` itself implements and which almost no other "`OpenAI`-compatible" provider
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
    /// Asking for token usage in the stream (FR-4.8, FR-5.4).
    ///
    /// The crate sets `stream_options: {include_usage: true}` only for its *native*
    /// `OpenAI` backend — every other compatible provider leaves it off — and without it
    /// a provider reports no usage at all on a streamed answer. That would leave the
    /// per-answer tokens and the per-session cost of FR-5.4 empty for the whole
    /// passthrough route, which is most of the catalog. Providers that do not know the
    /// field ignore it; the ones that reject it say so, which is a better failure than
    /// silently reporting nothing.
    const SUPPORTS_STREAM_OPTIONS: bool = true;
}

impl LlmCrate {
    /// The providers that could answer this request, best first.
    ///
    /// The first is always the routed one (DEC-17). A second is added when a *native*
    /// route has a base URL, and that is the honest consequence of the pinned crate
    /// having no streaming for some of its own backends: the catalog publishes
    /// `https://api.deepseek.com` and `https://openrouter.ai/api/v1`, both of which
    /// speak `/chat/completions`, so the same question can be asked the
    /// `OpenAI`-compatible way and streamed. The native backend is still tried first, so
    /// a crate that grows streaming is preferred the day it is pinned.
    fn candidates(request: &ChatRequest) -> Result<Vec<Box<dyn ChatProvider>>, LlmError> {
        let mut candidates = vec![Self::provider(request)?];
        if matches!(request.route, Route::Native(_))
            && let Some(url) = request
                .base_url
                .as_deref()
                .filter(|url| !url.trim().is_empty())
            && let Ok(provider) = passthrough(request, url)
        {
            candidates.push(provider);
        }
        Ok(candidates)
    }

    /// Streams through the crate's structured stream: text *and* usage (FR-4.8).
    ///
    /// One runtime for the whole response: the stream is pulled inside a single
    /// `block_on`, so a long answer does not build a runtime per chunk.
    fn stream_structured(
        provider: &dyn ChatProvider,
        request: &ChatRequest,
        messages: &[ChatMessage],
        cancel: &Cancel,
        on_delta: &mut DeltaHandler<'_>,
    ) -> Attempt {
        let mut emitted = false;
        let outcome = block_on(async {
            let mut stream = provider
                .chat_stream_struct(messages)
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
                                emitted = true;
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
            Ok(ChatOutcome {
                text,
                usage: usage_of(usage),
                // Streaming carries no thinking blocks in this crate version, so the
                // field stays empty rather than pretending otherwise (DEC-18).
                thinking: None,
            })
        });
        Attempt::from(outcome, &request.provider, emitted)
    }

    /// Streams through the crate's *string* stream, which exists for backends that have
    /// text but no structured deltas (`Anthropic`, `Ollama`, `xAI`).
    ///
    /// The answer is complete and arrives as it is produced; what is lost is the usage
    /// numbers, since this stream carries nothing but text. The pane therefore shows a
    /// turn count without tokens rather than an invented zero (FR-5.4).
    fn stream_strings(
        provider: &dyn ChatProvider,
        request: &ChatRequest,
        messages: &[ChatMessage],
        cancel: &Cancel,
        on_delta: &mut DeltaHandler<'_>,
    ) -> Attempt {
        let mut emitted = false;
        let outcome = block_on(async {
            let mut stream = provider
                .chat_stream(messages)
                .await
                .map_err(|error| translate(&request.provider, &error))?;
            let mut text = String::new();
            while let Some(item) = stream.next().await {
                if cancel.is_cancelled() {
                    return Err(LlmError::Cancelled);
                }
                match item {
                    Ok(delta) => {
                        if !delta.is_empty() {
                            emitted = true;
                            text.push_str(&delta);
                            on_delta(&delta);
                        }
                    }
                    Err(error) => return Err(translate(&request.provider, &error)),
                }
            }
            Ok(ChatOutcome {
                text,
                usage: None,
                thinking: None,
            })
        });
        Attempt::from(outcome, &request.provider, emitted)
    }

    /// One request, no streaming: what a provider that cannot stream at all gets
    /// (`Groq`, `Mistral`, and any provider with neither stream nor base URL).
    fn ask_once(
        provider: &dyn ChatProvider,
        request: &ChatRequest,
        messages: &[ChatMessage],
        cancel: &Cancel,
    ) -> Result<ChatOutcome, LlmError> {
        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        logging::log(
            Level::Debug,
            format!(
                "asking {} for {} ({} prompt bytes, no streaming)",
                request.provider,
                request.model,
                request.prompt.len()
            ),
        );
        let answer = block_on(async { provider.chat(messages).await })
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
}

/// What one way of asking did (FR-4.4, FR-5.2).
///
/// The distinction that matters is not *which* method failed but whether anything was
/// already paid for: an attempt that emitted text and then failed is the end of the
/// request, while one that failed before the first token has cost nothing and the next
/// way of asking is free to try. Nothing here inspects the crate's error *messages* to
/// decide whether a provider "supports" streaming — the next way of asking is simply
/// tried, which is robust against a crate that rewords its errors and costs at most one
/// extra round trip on a provider that is genuinely broken.
enum Attempt {
    /// The answer arrived.
    Done(Box<ChatOutcome>),
    /// The request was made and failed, with or without text first.
    Failed {
        /// What to tell the user if nothing else works.
        error: LlmError,
        /// Whether any text arrived before the failure.
        emitted: bool,
    },
}

impl Attempt {
    /// Turns a driven stream into an attempt.
    ///
    /// A runtime that will not start is a transport failure, because that is what it is
    /// from the caller's side: nothing was sent.
    fn from(
        outcome: Result<Result<ChatOutcome, LlmError>, crate::adapters::http::RuntimeError>,
        provider: &str,
        emitted: bool,
    ) -> Self {
        match outcome {
            Ok(Ok(outcome)) => Self::Done(Box::new(outcome)),
            Ok(Err(error)) => Self::Failed { error, emitted },
            Err(error) => Self::Failed {
                error: LlmError::Transport {
                    provider: provider.to_owned(),
                    reason: error.to_string(),
                },
                emitted,
            },
        }
    }
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
        Self::ask_once(&*provider, request, &messages, cancel)
    }

    /// Answers a request by the best way the provider actually supports (FR-4.4,
    /// FR-5.2, FR-5.4).
    ///
    /// The pinned crate implements a different subset of streaming for each backend —
    /// structured deltas for `OpenAI`, `Google` and `Azure`; text-only deltas for `Anthropic`,
    /// `Ollama` and `xAI`; nothing at all for `DeepSeek`, `Groq`, `Mistral` and `OpenRouter` (the
    /// measurements are in REQUIREMENTS Appendix B). Asking every provider for the
    /// structured stream and reporting what comes back is how a `DeepSeek` user got
    /// "Structured streaming not supported for this provider" instead of an answer, so
    /// the ways of asking are tried in order of what they give up:
    ///
    /// 1. **structured stream** on the routed provider — text as it arrives, with usage;
    /// 2. **string stream** on the same provider — text as it arrives, no usage;
    /// 3. **structured stream through the `OpenAI`-compatible passthrough**, when the
    ///    catalog gave a base URL for a native backend that has no streaming of its own
    ///    (`DeepSeek`, `OpenRouter`), which gets the deltas *and* the usage back;
    /// 4. **one request, no streaming** — the answer arrives whole, with usage.
    ///
    /// A path that fails *after* emitting text ends the request: that answer has been
    /// paid for, and asking again would bill it twice. A path that fails before the
    /// first token has cost nothing, so the next one is tried; the first error is the
    /// one reported, because it is the one about the route the user chose.
    fn stream(
        &self,
        request: &ChatRequest,
        cancel: &Cancel,
        on_delta: &mut DeltaHandler<'_>,
    ) -> Result<ChatOutcome, LlmError> {
        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        // The candidate list is built here and the cascade is a separate function so the
        // cascade can be tested against stub providers: what goes wrong with a real
        // provider is which *methods* it implements, and that is a property of the stub.
        Self::ask_streaming(&Self::candidates(request)?, request, cancel, on_delta)
    }
}

impl LlmCrate {
    /// Tries each way of asking, in order (FR-4.4, FR-5.2).
    fn ask_streaming(
        candidates: &[Box<dyn ChatProvider>],
        request: &ChatRequest,
        cancel: &Cancel,
        on_delta: &mut DeltaHandler<'_>,
    ) -> Result<ChatOutcome, LlmError> {
        let messages = Self::messages(request);
        let mut first_error: Option<LlmError> = None;

        for provider in candidates {
            let attempts = [
                Self::stream_structured(&**provider, request, &messages, cancel, on_delta),
                Self::stream_strings(&**provider, request, &messages, cancel, on_delta),
            ];
            for (index, attempt) in attempts.into_iter().enumerate() {
                match attempt {
                    Attempt::Done(outcome) => {
                        logging::log(
                            Level::Debug,
                            format!(
                                "{} answered with the {} path",
                                request.provider,
                                if index == 0 {
                                    "structured stream"
                                } else {
                                    "string stream"
                                }
                            ),
                        );
                        return Ok(*outcome);
                    }
                    Attempt::Failed {
                        error,
                        emitted: true,
                    } => return Err(error),
                    Attempt::Failed {
                        error,
                        emitted: false,
                    } => {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }

        // Nothing streamed. Every provider implements a plain request, so this is not a
        // last resort so much as the answer for a provider the crate cannot stream at
        // all (Groq, Mistral).
        for provider in candidates {
            match Self::ask_once(&**provider, request, &messages, cancel) {
                Ok(outcome) => {
                    logging::log(
                        Level::Debug,
                        format!("{} answered without streaming", request.provider),
                    );
                    return Ok(outcome);
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }

        // Unreachable in practice — `candidates` is never empty and a plain request
        // either answers or errors — but an error is the right shape for it: the lints
        // here refuse `unreachable!()` and a panic in a job thread is worse than a
        // sentence the user can read.
        Err(first_error.unwrap_or_else(|| LlmError::Request {
            provider: request.provider.clone(),
            reason: "no way to ask this provider produced an answer".to_owned(),
        }))
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

    // -- the streaming cascade (FR-4.4, FR-5.2) --------------------------------
    //
    // These are about which *methods a provider implements*, which is the only thing
    // that decides how an answer is obtained. A real provider's method set is a
    // property of the pinned crate (Appendix B), so the cascade is tested against
    // stand-ins that implement exactly one of them.

    /// A stand-in for a provider. `structured` and `strings` say which streaming it
    /// implements; `chat` is the text a plain request returns, and `calls` counts the
    /// requests that would have been sent — which is how "the next way of asking was
    /// not tried" becomes an assertion.
    struct Stub {
        structured: Option<Result<Vec<String>, &'static str>>,
        strings: Option<Result<Vec<String>, &'static str>>,
        chat: Result<String, &'static str>,
        chat_calls: std::sync::atomic::AtomicUsize,
    }

    impl Default for Stub {
        fn default() -> Self {
            Self {
                structured: None,
                strings: None,
                chat: Err("this provider was not asked to answer in one piece"),
                chat_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    /// The `ChatResponse` a stub hands back: text and nothing else.
    #[derive(Debug)]
    struct TextAnswer(String);

    impl std::fmt::Display for TextAnswer {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    impl llm::chat::ChatResponse for TextAnswer {
        fn text(&self) -> Option<String> {
            Some(self.0.clone())
        }

        fn tool_calls(&self) -> Option<Vec<llm::ToolCall>> {
            None
        }
    }

    impl Stub {
        fn answer(text: &str) -> Self {
            Self {
                chat: Ok(text.to_owned()),
                ..Self::default()
            }
        }

        fn streaming(deltas: &[&str]) -> Self {
            Self {
                structured: Some(Ok(deltas.iter().map(|d| (*d).to_owned()).collect())),
                ..Self::default()
            }
        }

        fn breaking() -> Self {
            Self {
                chat: Err("the provider refused"),
                ..Self::default()
            }
        }
    }

    #[llm::async_trait]
    impl ChatProvider for Stub {
        async fn chat(
            &self,
            _messages: &[ChatMessage],
        ) -> Result<Box<dyn llm::chat::ChatResponse>, llm::error::LLMError> {
            self.chat_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match &self.chat {
                Ok(text) => {
                    Ok(Box::new(TextAnswer(text.clone())) as Box<dyn llm::chat::ChatResponse>)
                }
                Err(reason) => Err(llm::error::LLMError::ProviderError((*reason).to_owned())),
            }
        }

        async fn chat_with_tools(
            &self,
            messages: &[ChatMessage],
            _tools: Option<&[llm::chat::Tool]>,
        ) -> Result<Box<dyn llm::chat::ChatResponse>, llm::error::LLMError> {
            self.chat(messages).await
        }

        async fn chat_stream_struct(
            &self,
            _messages: &[ChatMessage],
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures::Stream<
                            Item = Result<llm::chat::StreamResponse, llm::error::LLMError>,
                        > + Send,
                >,
            >,
            llm::error::LLMError,
        > {
            match &self.structured {
                None => Err(llm::error::LLMError::Generic(
                    "Structured streaming not supported for this provider".to_owned(),
                )),
                Some(Ok(deltas)) => {
                    let chunks: Vec<_> = deltas
                        .iter()
                        .map(|delta| {
                            Ok(llm::chat::StreamResponse {
                                choices: vec![llm::chat::StreamChoice {
                                    delta: llm::chat::StreamDelta {
                                        content: Some(delta.clone()),
                                        tool_calls: None,
                                    },
                                }],
                                usage: None,
                            })
                        })
                        .collect();
                    Ok(Box::pin(futures::stream::iter(chunks)))
                }
                Some(Err(reason)) => Err(llm::error::LLMError::ProviderError((*reason).to_owned())),
            }
        }

        async fn chat_stream(
            &self,
            _messages: &[ChatMessage],
        ) -> Result<
            std::pin::Pin<
                Box<dyn futures::Stream<Item = Result<String, llm::error::LLMError>> + Send>,
            >,
            llm::error::LLMError,
        > {
            match &self.strings {
                None => Err(llm::error::LLMError::Generic(
                    "Streaming not supported for this provider".to_owned(),
                )),
                Some(Ok(deltas)) => {
                    let chunks: Vec<_> = deltas.iter().map(|delta| Ok(delta.clone())).collect();
                    Ok(Box::pin(futures::stream::iter(chunks)))
                }
                Some(Err(reason)) => Err(llm::error::LLMError::ProviderError((*reason).to_owned())),
            }
        }
    }

    /// A request that needs no catalog and no network: the cascade never looks at the
    /// route, only at the providers it is handed.
    fn cascade_request() -> ChatRequest {
        request("deepseek", "deepseek-v4-pro")
    }

    fn drain(
        candidates: &[Box<dyn ChatProvider>],
        request: &ChatRequest,
    ) -> (Result<ChatOutcome, LlmError>, Vec<String>) {
        let mut deltas: Vec<String> = Vec::new();
        let cancel = Cancel::new();
        let mut sink = |delta: &str| deltas.push(delta.to_owned());
        let outcome = LlmCrate::ask_streaming(candidates, request, &cancel, &mut sink);
        (outcome, deltas)
    }

    #[test]
    fn a_provider_with_no_streaming_answers_in_one_request() {
        // The DeepSeek case: the crate's native backend implements neither stream, so
        // before the cascade the app asked for the structured one and reported the
        // crate's refusal as the failure. The answer is what the user wanted.
        let stub = Box::new(Stub::answer("the whole answer"));
        let (outcome, deltas) = drain(&[stub], &cascade_request());
        let outcome = outcome.expect("answered");
        assert_eq!(outcome.text, "the whole answer");
        assert!(
            deltas.is_empty(),
            "nothing streamed, so nothing was handed out"
        );
    }

    #[test]
    fn a_provider_with_only_a_string_stream_still_streams() {
        let stub = Box::new(Stub {
            strings: Some(Ok(vec!["half ".to_owned(), "an answer".to_owned()])),
            chat: Ok("unused".to_owned()),
            ..Stub::default()
        });
        let (outcome, deltas) = drain(&[stub], &cascade_request());
        let outcome = outcome.expect("streamed");
        assert_eq!(outcome.text, "half an answer");
        assert_eq!(deltas, vec!["half ".to_owned(), "an answer".to_owned()]);
        // No usage: this stream carries text only, and the pane says so by showing a
        // turn count without tokens rather than an invented zero (FR-5.4).
        assert_eq!(outcome.usage, None);
    }

    #[test]
    fn the_second_provider_is_tried_when_the_first_cannot_stream() {
        // The DeepSeek case with a base URL: the native route cannot stream, the
        // OpenAI-compatible one can, and the answer therefore arrives in pieces.
        let native = std::sync::Arc::new(Stub::breaking());
        let compatible = Stub::streaming(&["streamed ", "through chat/completions"]);
        let candidates: Vec<Box<dyn ChatProvider>> =
            vec![Box::new(NativeHandle(native.clone())), Box::new(compatible)];
        let (outcome, deltas) = drain(&candidates, &cascade_request());
        let outcome = outcome.expect("answered");
        assert_eq!(outcome.text, "streamed through chat/completions");
        assert_eq!(deltas.len(), 2);
        assert_eq!(
            native.chat_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the provider that cannot stream was not asked for a plain answer"
        );
    }

    #[test]
    fn a_failure_after_text_is_not_asked_again() {
        // Money was spent on the part that arrived. Repeating the question would bill
        // it twice, so a partial answer is the end of the request (ARCH-5) — and that
        // holds even though this provider *could* answer a plain request.
        let provider = std::sync::Arc::new(MidStreamFailure::default());
        let (outcome, deltas) = drain(&[Box::new(Handle(provider.clone()))], &cascade_request());
        let error = outcome.expect_err("the failure is reported");
        assert!(error.to_string().contains("connection reset"), "{error}");
        assert_eq!(deltas, vec!["the first token".to_owned()]);
        assert_eq!(
            provider
                .plain_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the question was asked again after text had already arrived"
        );
    }

    #[test]
    fn a_failure_before_any_text_still_reaches_the_plain_request() {
        // The other half of that rule: a stream that never produced a token cost
        // nothing, so the provider gets one more chance to answer.
        let stub = Stub {
            structured: Some(Err("the stream would not open")),
            chat: Ok("answered in one piece".to_owned()),
            ..Stub::default()
        };
        let (outcome, deltas) = drain(
            &[Box::new(stub) as Box<dyn ChatProvider>],
            &cascade_request(),
        );
        let outcome = outcome.expect("answered");
        assert_eq!(outcome.text, "answered in one piece");
        assert!(deltas.is_empty());
    }

    /// A shareable handle to a stub, so a test can look at what was asked of it after
    /// the cascade has taken ownership of the box.
    struct NativeHandle(std::sync::Arc<Stub>);

    #[llm::async_trait]
    impl ChatProvider for NativeHandle {
        async fn chat(
            &self,
            messages: &[ChatMessage],
        ) -> Result<Box<dyn llm::chat::ChatResponse>, llm::error::LLMError> {
            self.0.chat(messages).await
        }

        async fn chat_with_tools(
            &self,
            messages: &[ChatMessage],
            tools: Option<&[llm::chat::Tool]>,
        ) -> Result<Box<dyn llm::chat::ChatResponse>, llm::error::LLMError> {
            self.0.chat_with_tools(messages, tools).await
        }

        async fn chat_stream_struct(
            &self,
            messages: &[ChatMessage],
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures::Stream<
                            Item = Result<llm::chat::StreamResponse, llm::error::LLMError>,
                        > + Send,
                >,
            >,
            llm::error::LLMError,
        > {
            self.0.chat_stream_struct(messages).await
        }

        async fn chat_stream(
            &self,
            messages: &[ChatMessage],
        ) -> Result<
            std::pin::Pin<
                Box<dyn futures::Stream<Item = Result<String, llm::error::LLMError>> + Send>,
            >,
            llm::error::LLMError,
        > {
            self.0.chat_stream(messages).await
        }
    }

    /// A provider that hands out one delta, then breaks — and would have answered a
    /// plain request, so a retry is visible in `plain_calls`.
    #[derive(Default)]
    struct MidStreamFailure {
        plain_calls: std::sync::atomic::AtomicUsize,
    }

    /// The same handle, for the type that is not a `Stub`.
    struct Handle(std::sync::Arc<MidStreamFailure>);

    #[llm::async_trait]
    impl ChatProvider for Handle {
        async fn chat(
            &self,
            messages: &[ChatMessage],
        ) -> Result<Box<dyn llm::chat::ChatResponse>, llm::error::LLMError> {
            self.0.chat(messages).await
        }

        async fn chat_with_tools(
            &self,
            messages: &[ChatMessage],
            tools: Option<&[llm::chat::Tool]>,
        ) -> Result<Box<dyn llm::chat::ChatResponse>, llm::error::LLMError> {
            self.0.chat_with_tools(messages, tools).await
        }

        async fn chat_stream_struct(
            &self,
            messages: &[ChatMessage],
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures::Stream<
                            Item = Result<llm::chat::StreamResponse, llm::error::LLMError>,
                        > + Send,
                >,
            >,
            llm::error::LLMError,
        > {
            self.0.chat_stream_struct(messages).await
        }
    }

    #[llm::async_trait]
    impl ChatProvider for MidStreamFailure {
        async fn chat(
            &self,
            _messages: &[ChatMessage],
        ) -> Result<Box<dyn llm::chat::ChatResponse>, llm::error::LLMError> {
            self.plain_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(
                Box::new(TextAnswer("a second answer nobody asked for".to_owned()))
                    as Box<dyn llm::chat::ChatResponse>,
            )
        }

        async fn chat_with_tools(
            &self,
            messages: &[ChatMessage],
            _tools: Option<&[llm::chat::Tool]>,
        ) -> Result<Box<dyn llm::chat::ChatResponse>, llm::error::LLMError> {
            self.chat(messages).await
        }

        async fn chat_stream_struct(
            &self,
            _messages: &[ChatMessage],
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures::Stream<
                            Item = Result<llm::chat::StreamResponse, llm::error::LLMError>,
                        > + Send,
                >,
            >,
            llm::error::LLMError,
        > {
            let chunks = vec![
                Ok(llm::chat::StreamResponse {
                    choices: vec![llm::chat::StreamChoice {
                        delta: llm::chat::StreamDelta {
                            content: Some("the first token".to_owned()),
                            tool_calls: None,
                        },
                    }],
                    usage: None,
                }),
                Err(llm::error::LLMError::ProviderError(
                    "connection reset after the first token".to_owned(),
                )),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }
    }

    #[test]
    fn a_deepseek_route_offers_the_passthrough_as_a_second_way_to_ask() {
        // The candidate list is the routing decision, and it is what makes the cascade
        // reach DeepSeek's `/chat/completions`: the catalog publishes a base URL for it.
        let with_url = request("deepseek", "deepseek-v4-pro");
        assert_eq!(LlmCrate::candidates(&with_url).expect("built").len(), 2);

        // A provider the catalog gives no base URL for has one way to ask: Groq, which
        // is why its answers arrive whole rather than streaming.
        let mut without_url = request("deepseek", "deepseek-v4-pro");
        without_url.base_url = None;
        assert_eq!(LlmCrate::candidates(&without_url).expect("built").len(), 1);

        // A passthrough route is already the compatible provider; it is not doubled.
        let mut passthrough_route = with_url.clone();
        passthrough_route.route = Route::Passthrough {
            base_url: "https://openrouter.ai/api/v1".to_owned(),
        };
        assert_eq!(
            LlmCrate::candidates(&passthrough_route)
                .expect("built")
                .len(),
            1
        );
    }
}
