//! The model catalog and thinking settings (FR-4.7, FR-4.8, DEC-17).
//!
//! Everything here is pure: it parses the models.dev payload, decides which
//! providers this build can actually reach, and works out which thinking controls a
//! model may be given. It knows nothing about HTTP, the `llm` crate or the UI, which
//! is what makes all of it testable against the committed fixture in
//! `tests/fixtures/models/providers.json`.
//!
//! Two rules from the requirements shape the design:
//!
//! - a provider this build cannot reach is **hidden**, not shown as broken
//!   (FR-4.7), so [`Provider::route`] answers `None` rather than an error;
//! - a thinking control the `llm` crate cannot express is **refused with an
//!   explanation**, never silently downgraded (FR-4.8), so
//!   [`CatalogModel::thinking_choices`] returns choices carrying their own refusal
//!   reason instead of filtering them out.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A parsed `https://models.dev/api.json` (FR-4.7).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    providers: BTreeMap<String, Provider>,
    skipped: usize,
}

/// Why the catalog payload could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    /// The payload was not the JSON object of providers that models.dev serves.
    #[error("the catalog is not a JSON object of providers: {0}")]
    Malformed(String),
}

impl Catalog {
    /// Parses the models.dev payload.
    ///
    /// The feed is somebody else's to change, so parsing is **per entry**: only the
    /// outer object has to be JSON. A provider or a model that does not match what
    /// this build expects is skipped and counted, because the alternative — failing
    /// the whole document — is what turned one provider's `"min": -1` into an empty
    /// model picker for every user (FR-4.7: catalog metadata is advisory).
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::Malformed`] only when the text is not a JSON object of
    /// providers at all.
    pub fn from_json(text: &str) -> Result<Self, CatalogError> {
        let raw: BTreeMap<String, serde_json::Value> = serde_json::from_str(text)
            .map_err(|error| CatalogError::Malformed(error.to_string()))?;
        let mut providers = BTreeMap::new();
        let mut skipped = 0_usize;
        for (id, value) in raw {
            match Provider::from_value(id.clone(), value) {
                Ok(provider) => {
                    skipped += provider.skipped_models;
                    providers.insert(id, provider);
                }
                Err(error) => {
                    skipped += 1;
                    crate::logging::log(
                        crate::logging::Level::Debug,
                        format!("catalog: skipping provider {id}: {error}"),
                    );
                }
            }
        }
        Ok(Self { providers, skipped })
    }

    /// How many entries the feed published that this build could not read.
    ///
    /// Reported rather than swallowed: a provider silently missing from the picker
    /// is a puzzle, and the count is one sentence that solves it.
    #[must_use]
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// Every provider in the feed, in id order.
    pub fn providers(&self) -> impl Iterator<Item = &Provider> {
        self.providers.values()
    }

    /// The providers this build can reach, in id order (FR-4.7).
    pub fn reachable(&self) -> impl Iterator<Item = &Provider> {
        self.providers.values().filter(|p| p.route().is_some())
    }

    /// Looks a provider up by its catalog id.
    #[must_use]
    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.providers.get(id)
    }

    /// Looks a model up by provider id and model id.
    #[must_use]
    pub fn model(&self, provider: &str, model: &str) -> Option<&CatalogModel> {
        self.provider(provider)?.models.get(model)
    }

    /// How many providers the feed lists, reachable or not.
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Whether the feed lists no providers at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

/// A backend the `llm` crate implements natively (DEC-17).
///
/// The list is exactly the set of crate features this build enables, so a provider
/// whose native backend is not compiled in falls through to the passthrough route
/// instead of being promised and then failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeBackend {
    /// `openai`
    OpenAI,
    /// `anthropic`
    Anthropic,
    /// `openrouter`
    OpenRouter,
    /// `deepseek`
    DeepSeek,
    /// `google`
    Google,
    /// `groq`
    Groq,
    /// `mistral`
    Mistral,
    /// `xai`
    Xai,
}

impl NativeBackend {
    /// Every native backend, for the mapping table and the tests.
    pub const ALL: &'static [Self] = &[
        Self::OpenAI,
        Self::Anthropic,
        Self::OpenRouter,
        Self::DeepSeek,
        Self::Google,
        Self::Groq,
        Self::Mistral,
        Self::Xai,
    ];

    /// The catalog provider id this backend serves.
    #[must_use]
    pub fn provider_id(self) -> &'static str {
        match self {
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::OpenRouter => "openrouter",
            Self::DeepSeek => "deepseek",
            Self::Google => "google",
            Self::Groq => "groq",
            Self::Mistral => "mistral",
            Self::Xai => "xai",
        }
    }

    /// The backend serving a catalog provider id, if there is one.
    #[must_use]
    pub fn for_provider(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|b| b.provider_id() == id)
    }
}

/// How a provider is reached (DEC-17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// A backend the `llm` crate implements for this provider.
    Native(NativeBackend),
    /// The provider's own OpenAI-compatible endpoint, using its base URL.
    Passthrough {
        /// The base URL the catalog publishes for this provider.
        base_url: String,
    },
}

/// One provider in the catalog.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Provider {
    /// Provider id, as published.
    #[serde(default)]
    pub id: String,
    /// Human-readable name.
    #[serde(default)]
    pub name: String,
    /// Environment variables the provider documents for its key, in the feed's order.
    #[serde(default)]
    pub env: Vec<String>,
    /// OpenAI-compatible base URL, when the provider publishes one.
    #[serde(default)]
    pub api: Option<String>,
    /// Documentation URL, shown in the picker.
    #[serde(default)]
    pub doc: Option<String>,
    /// Models, keyed by model id.
    #[serde(default)]
    pub models: BTreeMap<String, CatalogModel>,
    /// How many of this provider's models the feed published in a shape this build
    /// could not read. Not part of the payload; filled in while parsing.
    #[serde(skip)]
    pub skipped_models: usize,
}

impl Provider {
    /// Parses a provider, tolerating models this build cannot read.
    ///
    /// # Errors
    ///
    /// Returns the reason when the provider's own fields do not match, so the caller
    /// can skip it and say so.
    fn from_value(id: String, mut value: serde_json::Value) -> Result<Self, String> {
        let models = value
            .get("models")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        // The models are taken out before the provider's own fields are parsed, so
        // that one unreadable model cannot fail the provider that contains it.
        if let Some(object) = value.as_object_mut() {
            object.remove("models");
        }
        let mut provider: Self =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        let mut readable = BTreeMap::new();
        let mut skipped = 0;
        for (model_id, model) in models {
            match serde_json::from_value::<CatalogModel>(model) {
                Ok(model) => {
                    readable.insert(model_id, model);
                }
                Err(error) => {
                    skipped += 1;
                    crate::logging::log(
                        crate::logging::Level::Debug,
                        format!("catalog: skipping {id}/{model_id}: {error}"),
                    );
                }
            }
        }
        provider.id = id;
        provider.models = readable;
        provider.skipped_models = skipped;
        Ok(provider)
    }
}

impl Provider {
    /// How this provider is reached, or `None` when this build cannot reach it.
    #[must_use]
    pub fn route(&self) -> Option<Route> {
        if let Some(native) = NativeBackend::for_provider(&self.id) {
            return Some(Route::Native(native));
        }
        self.api
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(|base_url| Route::Passthrough {
                base_url: base_url.to_owned(),
            })
    }

    /// The environment variable that overrides a stored key (FR-4.5, §7.4).
    ///
    /// The first name the provider documents, matching the convention the feeds and
    /// the providers themselves use.
    #[must_use]
    pub fn env_var(&self) -> Option<&str> {
        self.env
            .first()
            .map(String::as_str)
            .filter(|name| !name.trim().is_empty())
    }

    /// Models sorted by id, which is also how the picker lists them.
    pub fn models(&self) -> impl Iterator<Item = &CatalogModel> {
        self.models.values()
    }

    /// The provider's display name, falling back to its id.
    #[must_use]
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

/// One model in the catalog.
///
/// The four booleans are four independent capabilities the feed publishes
/// (`reasoning`, `tool_call`, `structured_output`, `temperature`). Packing them
/// into flags or an enum would only obscure that they are separate answers to
/// separate questions, so the lint is allowed here on purpose.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct CatalogModel {
    /// Model id, as the provider expects it in a request.
    #[serde(default)]
    pub id: String,
    /// Human-readable name.
    #[serde(default)]
    pub name: String,
    /// Model family, used by search (FR-4.7).
    #[serde(default)]
    pub family: Option<String>,
    /// Whether the model reasons at all.
    #[serde(default)]
    pub reasoning: bool,
    /// The thinking controls the model declares (FR-4.8).
    #[serde(default)]
    pub reasoning_options: Option<Vec<RawReasoningOption>>,
    /// Whether the model supports tool calls.
    #[serde(default)]
    pub tool_call: bool,
    /// Whether the model supports structured output.
    #[serde(default)]
    pub structured_output: bool,
    /// Whether the model accepts a temperature.
    #[serde(default)]
    pub temperature: bool,
    /// Context and output limits.
    #[serde(default)]
    pub limit: Limit,
    /// Per-1M-token prices, when published.
    #[serde(default)]
    pub cost: Option<Cost>,
    /// Release date, as published (an ISO date).
    #[serde(default)]
    pub release_date: Option<String>,
    /// Last update, as published.
    #[serde(default)]
    pub last_updated: Option<String>,
}

impl CatalogModel {
    /// The context window, when the feed states a usable one.
    #[must_use]
    pub fn context_limit(&self) -> Option<u32> {
        tokens(self.limit.context)
    }

    /// The output cap, when the feed states a usable one.
    #[must_use]
    pub fn output_limit(&self) -> Option<u32> {
        tokens(self.limit.output)
    }

    /// The model's declared reasoning options, with the feed's nulls dropped.
    ///
    /// Empty for a model that does not reason, so callers cannot accidentally offer
    /// a control the model rejects.
    #[must_use]
    pub fn declared_options(&self) -> Vec<ReasoningOption> {
        if !self.reasoning {
            return Vec::new();
        }
        self.reasoning_options
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|option| match option {
                RawReasoningOption::Toggle => Some(ReasoningOption::Toggle),
                RawReasoningOption::Effort { values } => Some(ReasoningOption::Effort {
                    values: values
                        .iter()
                        .filter_map(|value| value.as_deref())
                        .map(|value| value.trim().to_owned())
                        .filter(|value| !value.is_empty())
                        .collect(),
                }),
                RawReasoningOption::BudgetTokens { min, max } => {
                    Some(ReasoningOption::BudgetTokens {
                        min: tokens(*min),
                        max: tokens(*max),
                    })
                }
                RawReasoningOption::Unknown => None,
            })
            .collect()
    }

    /// The thinking controls declared for this model (FR-4.8).
    ///
    /// A model that does not reason gets none, even if the feed lists options for
    /// it: `reasoning: false` is the field that decides, and offering a control the
    /// model rejects would be exactly the silent downgrade the requirements forbid.
    #[must_use]
    pub fn thinking_choices(&self) -> Vec<ThinkingChoice> {
        // Built from `declared_options` so the choices offered and the choices
        // `Thinking::request` accepts can never drift apart.
        let mut choices = Vec::new();
        for option in self.declared_options() {
            match option {
                ReasoningOption::Toggle => {
                    choices.push(ThinkingChoice::on(Thinking::Toggle { value: true }));
                    choices.push(ThinkingChoice::on(Thinking::Toggle { value: false }));
                }
                ReasoningOption::Effort { values } => {
                    for value in values {
                        choices.push(effort_choice(&value));
                    }
                }
                ReasoningOption::BudgetTokens { min, max } => {
                    // The offered starting point has to be inside the range the model
                    // declares, or the first thing the picker shows is invalid.
                    let value = min
                        .unwrap_or(DEFAULT_BUDGET_TOKENS)
                        .min(max.unwrap_or(u32::MAX));
                    choices.push(ThinkingChoice {
                        thinking: Thinking::BudgetTokens { value },
                        label: format!("budget {value} tokens"),
                        refusal: None,
                        needs_input: true,
                    });
                }
            }
        }
        choices
    }

    /// The model's name for display, falling back to its id.
    #[must_use]
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    /// A one-line summary of the capabilities worth showing in a list (FR-4.7).
    #[must_use]
    pub fn badges(&self) -> String {
        let mut parts = Vec::new();
        if self.reasoning {
            parts.push("reasoning".to_owned());
        }
        if let Some(context) = self.context_limit() {
            parts.push(format!("ctx {}", compact_tokens(context)));
        }
        if let Some(cost) = &self.cost
            && let Some(input) = cost.input
        {
            parts.push(format!("${input}/M in"));
        }
        if self.tool_call {
            parts.push("tools".to_owned());
        }
        parts.join(" · ")
    }
}

/// The reasoning options block, as the feed publishes it.
///
/// Kept separate from [`ReasoningOption`] because the feed is sloppier than the
/// domain: `effort.values` contains nulls in the real payload, and unknown option
/// types exist to be skipped.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RawReasoningOption {
    /// A boolean on/off switch.
    Toggle,
    /// A named effort level.
    Effort {
        /// The levels the model declares, which may include nulls to drop.
        #[serde(default)]
        values: Vec<Option<String>>,
    },
    /// An explicit token budget.
    ///
    /// The bounds are read as signed integers because the live feed publishes `-1`
    /// to mean "no minimum" (three models did when this was written), and a `u32`
    /// would reject the whole document for it. Sanitising happens in
    /// [`tokens`], which is also where a non-positive bound becomes "none".
    BudgetTokens {
        /// Smallest accepted budget, as published.
        #[serde(default)]
        min: Option<i64>,
        /// Largest accepted budget, as published.
        #[serde(default)]
        max: Option<i64>,
    },
    /// Anything this build does not model; skipped.
    #[serde(other)]
    Unknown,
}

/// A parsed reasoning option, with the nulls dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningOption {
    /// A boolean on/off switch.
    Toggle,
    /// The effort levels the model declares.
    Effort {
        /// Level names, in the feed's order, nulls removed.
        values: Vec<String>,
    },
    /// A token budget, with the bounds the model declares.
    BudgetTokens {
        /// Smallest accepted budget, absent when the feed states none.
        min: Option<u32>,
        /// Largest accepted budget, absent when the feed states none.
        max: Option<u32>,
    },
}

/// Context and output limits.
///
/// Signed for the same reason the budget bounds are: a published `-1` or `0` means
/// "unstated", and it must not be able to fail the parse.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Limit {
    /// Context window in tokens, as published.
    #[serde(default)]
    pub context: Option<i64>,
    /// Maximum output tokens, as published.
    #[serde(default)]
    pub output: Option<i64>,
}

/// A published token count, or `None` when it is not a usable number.
///
/// Zero and negatives are the feed's way of saying "no limit": treating `-1` as a
/// floor of zero would offer a budget nobody can set, and treating `0` as a context
/// window would truncate every prompt to nothing.
fn tokens(value: Option<i64>) -> Option<u32> {
    let value = value.filter(|value| *value > 0)?;
    u32::try_from(value).ok()
}

/// Prices per 1M tokens, used only for labelled estimates (§7.5).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Cost {
    /// Input price per 1M tokens.
    #[serde(default)]
    pub input: Option<f64>,
    /// Output price per 1M tokens.
    #[serde(default)]
    pub output: Option<f64>,
    /// Reasoning price per 1M tokens, when published separately.
    #[serde(default)]
    pub reasoning: Option<f64>,
    /// Cached-input price per 1M tokens.
    #[serde(default)]
    pub cache_read: Option<f64>,
}

/// The thinking setting for a selection (FR-4.8).
///
/// The shape is the catalog's: `toggle` is a boolean, `effort` a named level,
/// `budget_tokens` a token count. Serialised into `config.toml` as
/// `reasoning = { type = "…", value = … }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Thinking {
    /// Send thinking on or off.
    Toggle {
        /// The switch.
        value: bool,
    },
    /// Ask for a named effort level.
    Effort {
        /// The level, as the model named it.
        value: String,
    },
    /// Ask for an explicit token budget.
    BudgetTokens {
        /// The budget in tokens.
        value: u32,
    },
}

/// The default budget offered when the model declares none.
pub const DEFAULT_BUDGET_TOKENS: u32 = 4096;

impl Thinking {
    /// The catalog option type this setting uses.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Toggle { .. } => "toggle",
            Self::Effort { .. } => "effort",
            Self::BudgetTokens { .. } => "budget_tokens",
        }
    }

    /// The value, rendered for the status line (FR-4.8).
    #[must_use]
    pub fn value_label(&self) -> String {
        match self {
            Self::Toggle { value } => if *value { "on" } else { "off" }.to_owned(),
            Self::Effort { value } => value.clone(),
            Self::BudgetTokens { value } => value.to_string(),
        }
    }

    /// What to ask the `llm` crate for, or why the crate cannot express it.
    ///
    /// This is the mapping FR-4.8 requires to be explicit. It returns domain values
    /// rather than crate types so the rule stays testable without the crate, and so
    /// the refusal messages are written once.
    ///
    /// # Errors
    ///
    /// Returns [`ThinkingError::NotOffered`] when the model does not declare this
    /// control, [`ThinkingError::Unmappable`] for an `effort` level the crate does
    /// not model, and [`ThinkingError::BudgetOutOfRange`] for a budget outside the
    /// model's declared bounds.
    pub fn request(&self, model: &CatalogModel) -> Result<ThinkingRequest, ThinkingError> {
        let options = model.declared_options();
        match self {
            Self::Toggle { value } => {
                if options.contains(&ReasoningOption::Toggle) {
                    Ok(ThinkingRequest::Reasoning(*value))
                } else {
                    Err(self.not_offered(model))
                }
            }
            Self::Effort { value } => {
                let declared = options.iter().find_map(|option| match option {
                    ReasoningOption::Effort { values } => Some(values),
                    _ => None,
                });
                match declared {
                    Some(values) if values.iter().any(|declared| declared == value.trim()) => {
                        match effort_level(value) {
                            Some(level) => Ok(ThinkingRequest::Effort(level)),
                            None => Err(ThinkingError::Unmappable {
                                value: value.trim().to_owned(),
                                because: "the llm crate models only low, medium and high"
                                    .to_owned(),
                            }),
                        }
                    }
                    _ => Err(self.not_offered(model)),
                }
            }
            Self::BudgetTokens { value } => {
                let bounds = options.iter().find_map(|option| match option {
                    ReasoningOption::BudgetTokens { min, max } => Some((*min, *max)),
                    _ => None,
                });
                match bounds {
                    Some((min, max)) => {
                        if min.is_some_and(|min| *value < min)
                            || max.is_some_and(|max| *value > max)
                        {
                            return Err(ThinkingError::BudgetOutOfRange {
                                value: *value,
                                min,
                                max,
                            });
                        }
                        Ok(ThinkingRequest::BudgetTokens(*value))
                    }
                    None => Err(self.not_offered(model)),
                }
            }
        }
    }

    fn not_offered(&self, model: &CatalogModel) -> ThinkingError {
        ThinkingError::NotOffered {
            model: model.label().to_owned(),
            kind: self.kind().to_owned(),
        }
    }
}

/// A thinking control a model declares, with the reason it cannot be used (FR-4.8).
#[derive(Debug, Clone, PartialEq)]
pub struct ThinkingChoice {
    /// The setting this choice stands for.
    pub thinking: Thinking,
    /// What to show in the picker.
    pub label: String,
    /// Why the choice is unavailable, when it is.
    pub refusal: Option<String>,
    /// Whether the picker must ask for a number before using this choice.
    pub needs_input: bool,
}

impl ThinkingChoice {
    fn on(thinking: Thinking) -> Self {
        let label = match &thinking {
            Thinking::Toggle { value: true } => "on".to_owned(),
            Thinking::Toggle { value: false } => "off".to_owned(),
            other => other.value_label(),
        };
        Self {
            thinking,
            label,
            refusal: None,
            needs_input: false,
        }
    }

    /// Whether the choice can be used as it stands.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.refusal.is_none()
    }
}

/// What to send the provider, in domain terms (FR-4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingRequest {
    /// `.reasoning(bool)`
    Reasoning(bool),
    /// `.reasoning_effort(..)`
    Effort(EffortLevel),
    /// `.reasoning_budget_tokens(u32)`
    BudgetTokens(u32),
}

/// The effort levels the `llm` crate models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortLevel {
    /// `low`
    Low,
    /// `medium`
    Medium,
    /// `high`
    High,
}

impl EffortLevel {
    /// The wire name, as the crate spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Why a thinking setting cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ThinkingError {
    /// The model does not declare this control.
    #[error("{model} does not offer a `{kind}` thinking control")]
    NotOffered {
        /// The model's display name.
        model: String,
        /// The control kind that was asked for.
        kind: String,
    },

    /// The control exists but the crate cannot send this value.
    #[error("cannot send effort `{value}`: {because}")]
    Unmappable {
        /// The value the catalog declares.
        value: String,
        /// Why it cannot be sent.
        because: String,
    },

    /// The budget is outside the model's declared bounds.
    #[error("a budget of {value} tokens is outside the model's range{}", range(*min, *max))]
    BudgetOutOfRange {
        /// The requested budget.
        value: u32,
        /// Smallest accepted budget.
        min: Option<u32>,
        /// Largest accepted budget.
        max: Option<u32>,
    },
}

fn range(min: Option<u32>, max: Option<u32>) -> String {
    match (min, max) {
        (Some(min), Some(max)) => format!(" ({min}–{max})"),
        (Some(min), None) => format!(" (at least {min})"),
        (None, Some(max)) => format!(" (at most {max})"),
        (None, None) => String::new(),
    }
}

fn effort_choice(value: &str) -> ThinkingChoice {
    let trimmed = value.trim();
    let refusal = effort_level(trimmed).is_none().then(|| {
        format!("`{trimmed}` cannot be sent: the llm crate models only low, medium and high")
    });
    ThinkingChoice {
        thinking: Thinking::Effort {
            value: trimmed.to_owned(),
        },
        label: trimmed.to_owned(),
        refusal,
        needs_input: false,
    }
}

fn effort_level(value: &str) -> Option<EffortLevel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Some(EffortLevel::Low),
        "medium" => Some(EffortLevel::Medium),
        "high" => Some(EffortLevel::High),
        _ => None,
    }
}

fn compact_tokens(tokens: u32) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", f64::from(tokens) / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        Catalog::from_json(include_str!("../../tests/fixtures/models/providers.json"))
            .expect("the fixture parses")
    }

    #[test]
    fn the_fixture_parses_and_normalises_ids() {
        let catalog = catalog();
        assert!(catalog.len() >= 8);
        let deepseek = catalog.provider("deepseek").expect("deepseek is listed");
        assert_eq!(deepseek.id, "deepseek");
        assert_eq!(deepseek.label(), "DeepSeek");
        assert_eq!(deepseek.env_var(), Some("DEEPSEEK_API_KEY"));
    }

    #[test]
    fn the_payloads_unknown_fields_are_ignored() {
        // Trimming fields must not empty the picker: the feed is not ours to freeze.
        let text = r#"{
            "thing": {"id": "thing", "name": "Thing", "npmpackage": "@x/y",
                      "models": {"m": {"id": "m", "name": "M", "weird": {"a": 1}}}}
        }"#;
        let catalog = Catalog::from_json(text).expect("parses");
        let model = catalog.model("thing", "m").expect("the model survives");
        assert_eq!(model.name, "M");
    }

    #[test]
    fn a_negative_budget_bound_is_read_as_no_bound() {
        // The live feed publishes `"min": -1` to mean "no minimum". Parsing it as a
        // `u32` fails the whole document, which is exactly what emptied the picker for
        // every user: one provider's oddity became nobody's model list.
        let catalog = catalog();
        let model = catalog
            .model("nvidia", "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning")
            .expect("the provider whose bound is -1");
        let options = model.declared_options();
        let min = options.iter().find_map(|option| match option {
            ReasoningOption::BudgetTokens { min, .. } => Some(*min),
            _ => None,
        });
        assert_eq!(min, Some(None), "a floor of -1 means no floor");
        // And the choice it offers is usable, with the ceiling the feed states.
        let choice = model
            .thinking_choices()
            .into_iter()
            .find(|choice| choice.needs_input)
            .expect("a budget control");
        assert!(choice.is_usable(), "{:?}", choice.refusal);
        assert_eq!(choice.thinking, Thinking::BudgetTokens { value: 4096 });
        assert!(
            Thinking::BudgetTokens { value: 40_000 }
                .request(model)
                .is_err(),
            "the ceiling of 32768 still applies"
        );
    }

    #[test]
    fn a_model_that_cannot_be_read_does_not_take_its_provider_with_it() {
        let text = r#"{"p": {"id": "p", "name": "P", "api": "https://p/v1", "models": {
            "good": {"id": "good", "name": "Good"},
            "broken": {"id": "broken", "name": "Broken", "reasoning_options": 5},
            "also-broken": {"id": "also-broken", "limit": "none"}}}}"#;
        let catalog = Catalog::from_json(text).expect("the provider is still readable");
        assert_eq!(catalog.len(), 1);
        assert!(
            catalog.model("p", "good").is_some(),
            "the good model survives"
        );
        assert!(catalog.model("p", "broken").is_none());
        assert_eq!(catalog.skipped(), 2, "and the feed's oddities are counted");
    }

    #[test]
    fn a_provider_that_cannot_be_read_does_not_take_the_catalog_with_it() {
        let text = r#"{
            "good": {"id": "good", "name": "Good", "api": "https://g/v1", "models": {}},
            "broken": {"id": "broken", "name": 42, "models": {}}}"#;
        let catalog = Catalog::from_json(text).expect("the catalog is still readable");
        assert_eq!(catalog.len(), 1);
        assert!(catalog.provider("good").is_some());
        assert_eq!(catalog.skipped(), 1);
    }

    #[test]
    fn malformed_payloads_are_reported_not_panicked_on() {
        assert!(Catalog::from_json("[]").is_err());
        assert!(Catalog::from_json("not json").is_err());
        // An empty object is a valid, empty catalog: an empty feed is not an error.
        assert!(Catalog::from_json("{}").expect("parses").is_empty());
    }

    #[test]
    fn a_provider_is_reached_natively_when_the_backend_is_compiled_in() {
        let catalog = catalog();
        for id in [
            "openai",
            "anthropic",
            "google",
            "groq",
            "openrouter",
            "deepseek",
        ] {
            let route = catalog.provider(id).expect(id).route();
            assert!(
                matches!(route, Some(Route::Native(_))),
                "{id} should be native, got {route:?}"
            );
        }
    }

    #[test]
    fn a_provider_with_a_base_url_is_reached_through_the_passthrough() {
        let catalog = catalog();
        let route = catalog.provider("lmstudio").expect("lmstudio").route();
        assert_eq!(
            route,
            Some(Route::Passthrough {
                base_url: "http://127.0.0.1:1234/v1".to_owned()
            })
        );
    }

    #[test]
    fn a_provider_this_build_cannot_reach_is_hidden() {
        let catalog = catalog();
        // watsonx publishes no OpenAI-compatible base URL and has no native backend.
        assert!(
            catalog
                .provider("watsonx")
                .expect("watsonx")
                .route()
                .is_none()
        );
        assert!(!catalog.reachable().any(|p| p.id == "watsonx"));
        // …and the ones that can be reached are all still there.
        assert!(catalog.reachable().count() < catalog.len());
    }

    #[test]
    fn a_provider_with_a_blank_base_url_is_hidden_rather_than_used() {
        let text =
            r#"{"p": {"id": "p", "name": "P", "api": "  ", "env": ["P_KEY"], "models": {}}}"#;
        let catalog = Catalog::from_json(text).expect("parses");
        assert!(catalog.provider("p").expect("p").route().is_none());
    }

    #[test]
    fn a_model_that_does_not_reason_offers_no_thinking_controls() {
        let catalog = catalog();
        let model = catalog
            .model("groq", "whisper-large-v3")
            .expect("the transcription model");
        assert!(!model.reasoning);
        assert!(model.thinking_choices().is_empty());
    }

    #[test]
    fn the_declared_effort_values_become_choices_with_refusals_for_the_unsendable() {
        let catalog = catalog();
        let model = catalog
            .model("deepseek", "deepseek-v4-pro")
            .expect("a reasoning model");
        let choices = model.thinking_choices();
        let labels: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["on", "off", "high", "max"]);

        let high = choices
            .iter()
            .find(|c| c.label == "high")
            .expect("high is offered");
        assert!(high.is_usable());
        assert_eq!(
            high.thinking.request(model).expect("mappable"),
            ThinkingRequest::Effort(EffortLevel::High)
        );

        let max = choices.iter().find(|c| c.label == "max").expect("listed");
        assert!(!max.is_usable());
        assert!(
            max.refusal
                .as_deref()
                .unwrap_or("")
                .contains("low, medium and high")
        );
        // Selecting it anyway is refused rather than silently downgraded.
        assert!(matches!(
            max.thinking.request(model),
            Err(ThinkingError::Unmappable { .. })
        ));
    }

    #[test]
    fn null_values_in_the_feed_are_dropped_rather_than_offered() {
        let catalog = catalog();
        let model = catalog
            .model("anthropic", "claude-opus-4-5")
            .expect("a reasoning model");
        let labels: Vec<String> = model
            .thinking_choices()
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert!(!labels.iter().any(String::is_empty));
        assert!(labels.contains(&"high".to_owned()));
    }

    #[test]
    fn a_budget_option_asks_the_picker_for_a_number() {
        let text = r#"{"p": {"id": "p", "name": "P", "models": {"m": {
            "id": "m", "name": "M", "reasoning": true,
            "reasoning_options": [{"type": "budget_tokens", "min": 1024, "max": 8192}]}}}}"#;
        let catalog = Catalog::from_json(text).expect("parses");
        let model = catalog.model("p", "m").expect("the model");
        let choices = model.thinking_choices();
        assert_eq!(choices.len(), 1);
        assert!(choices[0].needs_input);
        assert_eq!(choices[0].thinking, Thinking::BudgetTokens { value: 1024 });
        assert!(choices[0].is_usable());
    }

    #[test]
    fn a_budget_outside_the_declared_range_is_refused_with_the_range() {
        let text = r#"{"p": {"id": "p", "name": "P", "models": {"m": {
            "id": "m", "name": "M", "reasoning": true,
            "reasoning_options": [{"type": "budget_tokens", "min": 1024, "max": 8192}]}}}}"#;
        let catalog = Catalog::from_json(text).expect("parses");
        let model = catalog.model("p", "m").expect("the model");
        let too_big = Thinking::BudgetTokens { value: 100_000 };
        let error = too_big.request(model).expect_err("refused");
        assert!(error.to_string().contains("1024"), "{error}");
        assert!(error.to_string().contains("8192"), "{error}");
        assert_eq!(
            Thinking::BudgetTokens { value: 2048 }
                .request(model)
                .expect("in range"),
            ThinkingRequest::BudgetTokens(2048)
        );
    }

    #[test]
    fn a_control_the_model_does_not_declare_is_refused_by_name() {
        let catalog = catalog();
        let model = catalog
            .model("deepseek", "deepseek-v4-pro")
            .expect("a model with a toggle and an effort");
        let error = Thinking::BudgetTokens { value: 4096 }
            .request(model)
            .expect_err("no budget control is declared");
        assert!(matches!(error, ThinkingError::NotOffered { .. }));
        assert!(error.to_string().contains("budget_tokens"));
    }

    #[test]
    fn a_zero_context_limit_is_treated_as_unknown() {
        let text = r#"{"p": {"id": "p", "name": "P", "models": {"m": {
            "id": "m", "name": "M", "limit": {"context": 0, "output": 4096}}}}}"#;
        let catalog = Catalog::from_json(text).expect("parses");
        let model = catalog.model("p", "m").expect("the model");
        assert_eq!(model.context_limit(), None);
        assert_eq!(model.output_limit(), Some(4096));
    }

    #[test]
    fn thinking_round_trips_through_the_config_shape() {
        let thinking = Thinking::Effort {
            value: "high".to_owned(),
        };
        let text = toml::to_string(&thinking).expect("serialises");
        assert!(text.contains("type = \"effort\""), "{text}");
        let back: Thinking = toml::from_str(&text).expect("deserialises");
        assert_eq!(back, thinking);
        assert_eq!(back.value_label(), "high");
    }

    #[test]
    fn badges_name_what_the_feed_publishes() {
        let catalog = catalog();
        let model = catalog
            .model("deepseek", "deepseek-v4-pro")
            .expect("a model with limits and prices");
        let badges = model.badges();
        assert!(badges.contains("reasoning"), "{badges}");
        assert!(badges.contains("ctx 1.0M"), "{badges}");
        assert!(badges.contains('$'), "{badges}");
    }
}
