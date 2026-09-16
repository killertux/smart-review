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
use std::time::Duration;
// `chat`, `usage` and `thinking` come from `ChatProvider`/`ChatResponse`, which are
// supertraits of the `llm::LLMProvider` this module returns; naming them again here
// would be redundant.
use llm::chat::{ChatMessage, ReasoningEffort};

use crate::adapters::http::{AsyncWaitError, block_on_cancellable};
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
            .map_err(|error| translate(&request.provider, request.timeout_secs, &error))
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
    fn candidates(request: &ChatRequest) -> Result<Vec<Candidate>, LlmError> {
        let mut candidates = vec![Candidate::new(
            Self::provider(request)?,
            StreamingCapabilities::for_route(&request.route),
        )];
        if matches!(request.route, Route::Native(_))
            && let Some(url) = request
                .base_url
                .as_deref()
                .filter(|url| !url.trim().is_empty())
            && let Ok(provider) = passthrough(request, url)
        {
            candidates.push(Candidate::new(provider, StreamingCapabilities::STRUCTURED));
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
        let outcome = block_on_cancellable(
            async {
                let mut stream = provider.chat_stream_struct(messages).await?;
                let mut text = String::new();
                let mut usage = None;
                while let Some(item) = stream.next().await {
                    // Checked between chunks: cancellation is a flag, and the cost of
                    // finishing an answer nobody wants is the user's money (ARCH-5). The
                    // flag travels as an error like any other, and the caller recognizes it.
                    if cancel.is_cancelled() {
                        return Err(cancelled());
                    }
                    let response = item?;
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
                Ok(ChatOutcome {
                    text,
                    usage: usage_of(usage),
                    // Streaming carries no thinking blocks in this crate version, so the
                    // field stays empty rather than pretending otherwise (DEC-18).
                    thinking: None,
                })
            },
            cancel,
            Duration::from_secs(request.timeout_secs),
        );
        Attempt::from(outcome, &request.provider, request.timeout_secs)
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
        let outcome = block_on_cancellable(
            async {
                let mut stream = provider.chat_stream(messages).await?;
                let mut text = String::new();
                while let Some(item) = stream.next().await {
                    if cancel.is_cancelled() {
                        return Err(cancelled());
                    }
                    let delta = item?;
                    if !delta.is_empty() {
                        text.push_str(&delta);
                        on_delta(&delta);
                    }
                }
                Ok(ChatOutcome {
                    text,
                    usage: None,
                    thinking: None,
                })
            },
            cancel,
            Duration::from_secs(request.timeout_secs),
        );
        Attempt::from(outcome, &request.provider, request.timeout_secs)
    }

    /// One request, no streaming: what a provider that cannot stream at all gets
    /// (`Groq`, `Mistral`, and any provider with neither stream nor base URL).
    fn ask_once(
        provider: &dyn ChatProvider,
        request: &ChatRequest,
        messages: &[ChatMessage],
        cancel: &Cancel,
    ) -> Result<ChatOutcome, Failure> {
        if cancel.is_cancelled() {
            return Err(Failure::plain(LlmError::Cancelled));
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
        match Attempt::from(
            block_on_cancellable(
                async { provider.chat(messages).await },
                cancel,
                Duration::from_secs(request.timeout_secs),
            )
            .map(|answer| {
                answer.map(|answer| ChatOutcome {
                    text: answer.text().unwrap_or_default(),
                    usage: usage_of(answer.usage()),
                    thinking: answer.thinking(),
                })
            }),
            &request.provider,
            request.timeout_secs,
        ) {
            Attempt::Done(outcome) => Ok(*outcome),
            Attempt::Failed(failure) => Err(failure),
        }
    }
}

/// The stream methods the pinned crate implements for a routed provider.
///
/// This is deliberately local capability metadata, rather than an inference from an
/// upstream error. `LLMError::Generic` may describe a dispatched provider failure, so it
/// can never prove a retry is free (IR-02).
#[derive(Debug, Clone, Copy)]
struct StreamingCapabilities {
    structured: bool,
    strings: bool,
}

impl StreamingCapabilities {
    const STRUCTURED: Self = Self {
        structured: true,
        strings: false,
    };

    const STRINGS: Self = Self {
        structured: false,
        strings: true,
    };

    const NONE: Self = Self {
        structured: false,
        strings: false,
    };

    fn for_route(route: &Route) -> Self {
        match route {
            Route::Native(
                NativeBackend::OpenAI
                | NativeBackend::Google
                | NativeBackend::OpenRouter
                | NativeBackend::Groq
                | NativeBackend::Mistral,
            )
            | Route::Passthrough { .. } => Self::STRUCTURED,
            Route::Native(NativeBackend::Anthropic | NativeBackend::Xai) => Self::STRINGS,
            Route::Native(NativeBackend::DeepSeek) => Self::NONE,
        }
    }
}

/// One provider route and the methods this build can invoke on it.
struct Candidate {
    provider: Box<dyn ChatProvider>,
    capabilities: StreamingCapabilities,
}

impl Candidate {
    fn new(provider: Box<dyn ChatProvider>, capabilities: StreamingCapabilities) -> Self {
        Self {
            provider,
            capabilities,
        }
    }
}

/// The crate's error for "the caller stopped this", which is not a provider problem.
fn cancelled() -> llm::error::LLMError {
    llm::error::LLMError::Generic("the caller cancelled the request".to_owned())
}

impl Failure {
    /// A locally constructed failure.
    fn plain(error: LlmError) -> Self {
        Self { error }
    }
}

/// What one way of asking did (FR-4.4, FR-5.2).
///
/// Capability metadata chooses the first method to call. Any actual call failure ends the
/// request, because no error response proves that a dispatched request was free (IR-02).
enum Attempt {
    /// The answer arrived.
    Done(Box<ChatOutcome>),
    /// The request was made and failed.
    Failed(Failure),
}

/// A failed attempt.
struct Failure {
    /// What to tell the user if nothing else works.
    error: LlmError,
}

impl Attempt {
    /// Turns a driven stream into an attempt.
    ///
    /// A runtime that will not start is a transport failure, because that is what it is
    /// from the caller's side: nothing was sent.
    fn from(
        outcome: Result<Result<ChatOutcome, llm::error::LLMError>, AsyncWaitError>,
        provider: &str,
        timeout_secs: u64,
    ) -> Self {
        match outcome {
            Ok(Ok(outcome)) => Self::Done(Box::new(outcome)),
            Ok(Err(error)) => Self::Failed(Failure {
                error: translate(provider, timeout_secs, &error),
            }),
            Err(AsyncWaitError::Cancelled) => Self::Failed(Failure::plain(LlmError::Cancelled)),
            Err(AsyncWaitError::TimedOut) => Self::Failed(Failure::plain(LlmError::Timeout {
                provider: provider.to_owned(),
                timeout_secs,
            })),
            Err(AsyncWaitError::Runtime(error)) => Self::Failed(Failure {
                error: LlmError::Transport {
                    provider: provider.to_owned(),
                    reason: error.to_string(),
                },
            }),
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
fn translate(provider: &str, timeout_secs: u64, error: &llm::error::LLMError) -> LlmError {
    let text = error.to_string();
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("caller cancelled the request") {
        return LlmError::Cancelled;
    }
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
            timeout_secs,
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
            reason: provider_message(error),
        },
    }
}

/// What to show for a provider's refusal.
///
/// The crate wraps a provider error response as `ResponseFormatError { message, raw }`,
/// where `message` is the status line and `raw` is the provider's JSON body. The raw body
/// is mostly noise on screen — but its `error.message` is usually the only sentence that
/// tells a person what to fix ("Authentication Fails, Your api key … is invalid"), so it
/// is lifted out and the JSON is dropped (FR-9.1).
fn provider_message(error: &llm::error::LLMError) -> String {
    let llm::error::LLMError::ResponseFormatError {
        message,
        raw_response,
    } = error
    else {
        return first_line(&error.to_string());
    };
    let detail = serde_json::from_str::<serde_json::Value>(raw_response)
        .ok()
        .as_ref()
        .and_then(|body| body.pointer("/error/message"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    match detail {
        Some(detail) => format!("{}: {detail}", first_line(message)),
        None => first_line(message),
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
        Self::ask_once(&*provider, request, &messages, cancel).map_err(|failure| failure.error)
    }

    /// Answers a request by the best way the provider actually supports (FR-4.4,
    /// FR-5.2, FR-5.4).
    ///
    /// The pinned crate implements a different subset of streaming for each backend —
    /// structured deltas for `OpenAI`, `Google`, `Azure`, `Groq`, `Mistral` and `OpenRouter`;
    /// text-only deltas for `Anthropic`, `Ollama` and `xAI`; and neither for `DeepSeek` (the
    /// measurements are in REQUIREMENTS Appendix B). Locally recorded capability metadata picks
    /// the one supported method before dispatching; only `DeepSeek` reaches the passthrough or a
    /// plain request.
    ///
    /// Route capability metadata selects one stream method. A path that emits text, is
    /// cancelled, or fails with any provider/transport error ends the request: absence of
    /// a delta does not prove the request was not dispatched or billed.
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
        candidates: &[Candidate],
        request: &ChatRequest,
        cancel: &Cancel,
        on_delta: &mut DeltaHandler<'_>,
    ) -> Result<ChatOutcome, LlmError> {
        let messages = Self::messages(request);
        let mut attempts = 0_u32;
        for provider in candidates {
            if provider.capabilities.structured {
                attempts = attempts.saturating_add(1);
                match Self::stream_structured(
                    &*provider.provider,
                    request,
                    &messages,
                    cancel,
                    on_delta,
                ) {
                    Attempt::Done(outcome) => {
                        log_stream_success(request, attempts, "structured stream");
                        return Ok(*outcome);
                    }
                    Attempt::Failed(failure) => return Err(failure.error),
                }
            }

            if provider.capabilities.strings {
                attempts = attempts.saturating_add(1);
                match Self::stream_strings(
                    &*provider.provider,
                    request,
                    &messages,
                    cancel,
                    on_delta,
                ) {
                    Attempt::Done(outcome) => {
                        log_stream_success(request, attempts, "string stream");
                        return Ok(*outcome);
                    }
                    Attempt::Failed(failure) => return Err(failure.error),
                }
            }
        }

        // Nothing streamed. Every provider implements a plain request, so this is not a
        // last resort so much as the answer for a provider the crate cannot stream at
        // all (Groq, Mistral).
        if let Some(provider) = candidates.first() {
            attempts = attempts.saturating_add(1);
            match Self::ask_once(&*provider.provider, request, &messages, cancel) {
                Ok(outcome) => {
                    logging::log(
                        Level::Debug,
                        format!(
                            "{} attempt {} answered without streaming",
                            request.provider, attempts
                        ),
                    );
                    return Ok(outcome);
                }
                Err(failure) => {
                    // A plain request is never a capability probe: it may always have
                    // reached the endpoint, so no candidate is tried after it fails.
                    return Err(failure.error);
                }
            }
        }

        // Unreachable in practice — `candidates` is never empty and a plain request
        // either answers or errors — but an error is the right shape for it: the lints
        // here refuse `unreachable!()` and a panic in a job thread is worse than a
        // sentence the user can read.
        Err(LlmError::Request {
            provider: request.provider.clone(),
            reason: "no way to ask this provider produced an answer".to_owned(),
        })
    }
}

/// Records which attempt produced the accepted stream without logging request content.
fn log_stream_success(request: &ChatRequest, attempt: u32, path: &str) {
    logging::log(
        Level::Debug,
        format!(
            "{} attempt {attempt} answered with the {path} path",
            request.provider
        ),
    );
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
            120,
            &llm::error::LLMError::AuthError("401 Unauthorized: invalid api key".to_owned()),
        );
        assert!(matches!(error, LlmError::Auth { .. }), "{error:?}");
        assert!(error.to_string().contains("rejected"), "{error}");
    }

    #[test]
    fn a_transport_failure_is_not_reported_as_a_bad_request() {
        let error = translate(
            "openrouter",
            120,
            &llm::error::LLMError::HttpError(
                "error sending request: connection refused".to_owned(),
            ),
        );
        assert!(matches!(error, LlmError::Transport { .. }), "{error:?}");
    }

    #[test]
    fn ir_08_a_provider_timeout_reports_the_configured_deadline() {
        let error = translate(
            "openrouter",
            120,
            &llm::error::LLMError::HttpError("request timed out".to_owned()),
        );
        assert!(
            matches!(
                error,
                LlmError::Timeout {
                    timeout_secs: 120,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_provider_error_keeps_the_providers_own_words() {
        let error = translate(
            "deepseek",
            120,
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
            120,
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
        structured_generic: bool,
        strings: Option<Result<Vec<String>, &'static str>>,
        chat: Result<String, &'static str>,
        structured_calls: std::sync::atomic::AtomicUsize,
        string_calls: std::sync::atomic::AtomicUsize,
        chat_calls: std::sync::atomic::AtomicUsize,
    }

    impl Default for Stub {
        fn default() -> Self {
            Self {
                structured: None,
                structured_generic: false,
                strings: None,
                chat: Err("this provider was not asked to answer in one piece"),
                structured_calls: std::sync::atomic::AtomicUsize::new(0),
                string_calls: std::sync::atomic::AtomicUsize::new(0),
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
            self.structured_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
                Some(Err(reason)) if self.structured_generic => {
                    Err(llm::error::LLMError::Generic((*reason).to_owned()))
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
            self.string_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

    #[test]
    fn ir_02_openai_compatible_native_routes_use_structured_streaming() {
        for backend in [
            NativeBackend::OpenRouter,
            NativeBackend::Groq,
            NativeBackend::Mistral,
        ] {
            let capabilities = StreamingCapabilities::for_route(&Route::Native(backend));
            assert!(capabilities.structured, "{backend:?}");
            assert!(!capabilities.strings, "{backend:?}");
        }
    }

    fn drain(
        candidates: &[Candidate],
        request: &ChatRequest,
    ) -> (Result<ChatOutcome, LlmError>, Vec<String>) {
        let mut deltas: Vec<String> = Vec::new();
        let cancel = Cancel::new();
        let mut sink = |delta: &str| deltas.push(delta.to_owned());
        let outcome = LlmCrate::ask_streaming(candidates, request, &cancel, &mut sink);
        (outcome, deltas)
    }

    fn candidate(
        provider: impl ChatProvider + 'static,
        capabilities: StreamingCapabilities,
    ) -> Candidate {
        Candidate::new(Box::new(provider), capabilities)
    }

    #[test]
    fn a_provider_with_no_streaming_answers_in_one_request() {
        // The DeepSeek case: the crate's native backend implements neither stream, so
        // before the cascade the app asked for the structured one and reported the
        // crate's refusal as the failure. The answer is what the user wanted.
        let stub = candidate(
            Stub::answer("the whole answer"),
            StreamingCapabilities::NONE,
        );
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
        let stub = candidate(
            Stub {
                strings: Some(Ok(vec!["half ".to_owned(), "an answer".to_owned()])),
                chat: Ok("unused".to_owned()),
                ..Stub::default()
            },
            StreamingCapabilities::STRINGS,
        );
        let (outcome, deltas) = drain(&[stub], &cascade_request());
        let outcome = outcome.expect("streamed");
        assert_eq!(outcome.text, "half an answer");
        assert_eq!(deltas, vec!["half ".to_owned(), "an answer".to_owned()]);
        // No usage: this stream carries text only, and the pane says so by showing a
        // turn count without tokens rather than an invented zero (FR-5.4).
        assert_eq!(outcome.usage, None);
    }

    #[test]
    fn ir_02_a_structured_success_does_not_start_a_second_stream() {
        let provider = std::sync::Arc::new(Stub {
            structured: Some(Ok(vec!["STRUCTURED_SENTINEL".to_owned()])),
            strings: Some(Ok(vec!["STRING_SENTINEL".to_owned()])),
            chat: Ok("PLAIN_SENTINEL".to_owned()),
            ..Stub::default()
        });
        let (outcome, deltas) = drain(
            &[candidate(
                NativeHandle(provider.clone()),
                StreamingCapabilities::STRUCTURED,
            )],
            &cascade_request(),
        );

        assert_eq!(
            outcome.expect("structured answer").text,
            "STRUCTURED_SENTINEL"
        );
        assert_eq!(deltas, vec!["STRUCTURED_SENTINEL".to_owned()]);
        assert_eq!(
            provider
                .structured_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            provider
                .string_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a successful structured attempt must not start the string stream"
        );
        assert_eq!(
            provider
                .chat_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a successful structured attempt must not start a plain request"
        );
    }

    #[test]
    fn ir_02_an_unsupported_structured_stream_tries_the_string_stream_once() {
        let provider = std::sync::Arc::new(Stub {
            strings: Some(Ok(vec!["STRING_SENTINEL".to_owned()])),
            chat: Ok("PLAIN_SENTINEL".to_owned()),
            ..Stub::default()
        });
        let (outcome, deltas) = drain(
            &[candidate(
                NativeHandle(provider.clone()),
                StreamingCapabilities::STRINGS,
            )],
            &cascade_request(),
        );

        assert_eq!(outcome.expect("string answer").text, "STRING_SENTINEL");
        assert_eq!(deltas, vec!["STRING_SENTINEL".to_owned()]);
        assert_eq!(
            provider
                .structured_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            provider
                .string_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            provider
                .chat_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[test]
    fn ir_02_two_unsupported_streams_make_one_plain_request() {
        let provider = std::sync::Arc::new(Stub::answer("PLAIN_SENTINEL"));
        let (outcome, deltas) = drain(
            &[candidate(
                NativeHandle(provider.clone()),
                StreamingCapabilities::NONE,
            )],
            &cascade_request(),
        );

        assert_eq!(outcome.expect("plain answer").text, "PLAIN_SENTINEL");
        assert!(deltas.is_empty());
        assert_eq!(
            provider
                .structured_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            provider
                .string_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            provider
                .chat_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[test]
    fn the_second_provider_is_tried_when_the_first_cannot_stream() {
        // The DeepSeek case with a base URL: the native route cannot stream, the
        // OpenAI-compatible one can, and the answer therefore arrives in pieces.
        let native = std::sync::Arc::new(Stub::breaking());
        let compatible = Stub::streaming(&["streamed ", "through chat/completions"]);
        let candidates = vec![
            candidate(NativeHandle(native.clone()), StreamingCapabilities::NONE),
            candidate(compatible, StreamingCapabilities::STRUCTURED),
        ];
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
        let (outcome, deltas) = drain(
            &[candidate(
                Handle(provider.clone()),
                StreamingCapabilities::STRUCTURED,
            )],
            &cascade_request(),
        );
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
    fn ir_02_a_provider_refusal_before_text_does_not_retry() {
        // A provider refusal may have occurred after the request was dispatched even
        // though it emitted no text. It is not proof that a retry is free.
        let provider = std::sync::Arc::new(Stub {
            structured: Some(Err("authentication rejected")),
            chat: Ok("answered in one piece".to_owned()),
            ..Stub::default()
        });
        let (outcome, deltas) = drain(
            &[candidate(
                NativeHandle(provider.clone()),
                StreamingCapabilities::STRUCTURED,
            )],
            &cascade_request(),
        );
        assert!(matches!(outcome, Err(LlmError::Auth { .. })));
        assert!(deltas.is_empty());
        assert_eq!(
            provider
                .string_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            provider
                .chat_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[test]
    fn ir_02_cancellation_during_a_stream_does_not_start_another_attempt() {
        let provider = std::sync::Arc::new(Stub {
            structured: Some(Ok(vec!["PARTIAL_SENTINEL".to_owned(), "unused".to_owned()])),
            strings: Some(Ok(vec!["STRING_SENTINEL".to_owned()])),
            chat: Ok("PLAIN_SENTINEL".to_owned()),
            ..Stub::default()
        });
        let candidates = vec![candidate(
            NativeHandle(provider.clone()),
            StreamingCapabilities::STRUCTURED,
        )];
        let cancel = Cancel::new();
        let mut deltas = Vec::new();
        let mut sink = |delta: &str| {
            deltas.push(delta.to_owned());
            cancel.cancel();
        };

        let outcome = LlmCrate::ask_streaming(&candidates, &cascade_request(), &cancel, &mut sink);

        assert_eq!(outcome.expect_err("cancelled"), LlmError::Cancelled);
        assert_eq!(deltas, vec!["PARTIAL_SENTINEL".to_owned()]);
        assert_eq!(
            provider
                .string_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            provider
                .chat_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
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
    fn ir_02_a_generic_rate_limit_error_does_not_start_a_follow_up_request() {
        let provider = std::sync::Arc::new(Stub {
            structured: Some(Err("rate limited")),
            structured_generic: true,
            strings: Some(Ok(vec!["STRING_SENTINEL".to_owned()])),
            chat: Ok("PLAIN_SENTINEL".to_owned()),
            ..Stub::default()
        });
        let (outcome, _) = drain(
            &[candidate(
                NativeHandle(provider.clone()),
                StreamingCapabilities {
                    structured: true,
                    strings: true,
                },
            )],
            &cascade_request(),
        );

        assert!(matches!(outcome, Err(LlmError::Request { .. })));
        assert_eq!(
            provider
                .string_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            provider
                .chat_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
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
