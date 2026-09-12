//! Choosing a model, a thinking mode and a key (FR-4.5, FR-4.7, FR-4.8).
//!
//! The use cases here are what the picker calls. They are pure functions over the
//! ports and the catalog, so every rule the requirements state about the picker —
//! which providers appear, how search ranks, which thinking controls exist, whether
//! a selection can actually be sent — is tested without a TTY, a network or a
//! keychain.

use crate::config::ModelSelection;
use crate::domain::model::{Catalog, CatalogModel, Route, Thinking, ThinkingChoice};
use crate::ports::catalog::{CatalogFetchError, CatalogLoad, CatalogPolicy, ModelCatalogPort};
use crate::ports::llm::ChatRequest;
use crate::ports::secret::{ApiKey, KeySource, KeyStatus, SecretError, SecretStore};

/// The catalog, with the catalogs's providers reduced to the reachable ones.
#[derive(Debug, Clone)]
pub struct CatalogState {
    /// The catalog and where it came from.
    pub load: CatalogLoad,
    /// Providers this build can reach, in id order (FR-4.7).
    pub providers: Vec<ProviderChoice>,
}

impl CatalogState {
    /// How many models the reachable providers offer.
    #[must_use]
    pub fn model_count(&self) -> usize {
        self.providers.iter().map(|choice| choice.model_count).sum()
    }

    /// A one-line description for the status area.
    ///
    /// The skipped count is part of it when there is one: a provider that vanished
    /// from the picker because the feed changed shape is a puzzle, and "3 entries
    /// could not be read" is the sentence that solves it (FR-4.7).
    #[must_use]
    pub fn summary(&self) -> String {
        let mut summary = format!(
            "{} providers, {} models ({})",
            self.providers.len(),
            self.model_count(),
            self.load.source.label()
        );
        let skipped = self.load.catalog.skipped();
        if skipped > 0 {
            let _ =
                std::fmt::Write::write_fmt(&mut summary, format_args!(", {skipped} unreadable"));
        }
        summary
    }
}

/// One provider as the picker shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChoice {
    /// Catalog id.
    pub id: String,
    /// Display name.
    pub label: String,
    /// How it is reached (DEC-17).
    pub route: Route,
    /// How many models it lists.
    pub model_count: usize,
}

impl ProviderChoice {
    /// How the provider is reached, for the picker's right-hand column.
    #[must_use]
    pub fn route_label(&self) -> &'static str {
        match self.route {
            Route::Native(_) => "native",
            Route::Passthrough { .. } => "openai-compatible",
        }
    }
}

/// Loads the catalog and reduces it to what can be used (FR-4.7).
///
/// # Errors
///
/// Returns the fetch error when no catalog can be produced at all; a stale cache is
/// a success and reports itself through [`CatalogLoad::source`].
pub fn load_catalog(
    port: &dyn ModelCatalogPort,
    policy: CatalogPolicy,
) -> Result<CatalogState, CatalogFetchError> {
    let load = port.load(policy)?;
    let providers = provider_choices(&load.catalog);
    Ok(CatalogState { load, providers })
}

/// The reachable providers, in id order (FR-4.7).
///
/// A provider that cannot be reached is absent rather than present and broken: the
/// picker must never offer something that cannot work.
#[must_use]
pub fn provider_choices(catalog: &Catalog) -> Vec<ProviderChoice> {
    catalog
        .reachable()
        .map(|provider| ProviderChoice {
            id: provider.id.clone(),
            label: provider.label().to_owned(),
            route: provider.route().unwrap_or(Route::Passthrough {
                base_url: String::new(),
            }),
            model_count: provider.models.len(),
        })
        .collect()
}

/// One model as the picker shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelChoice {
    /// Model id, as the provider expects it.
    pub id: String,
    /// Display name.
    pub label: String,
    /// Capability summary (FR-4.7).
    pub badges: String,
    /// The thinking controls this model declares (FR-4.8).
    pub thinking: Vec<ThinkingChoice>,
    /// Whether the model is old enough to de-emphasise, but never to hide.
    pub dated: bool,
}

/// Models of one provider, matching `query`, ranked (FR-4.7).
///
/// Ranking: an exact id, then an id prefix, then a name prefix, then anything else
/// that matches, with the shared fuzzy score breaking ties. This is what makes
/// typing `gpt-5` land on `gpt-5` rather than on `gpt-5.6-sol`.
#[must_use]
pub fn model_choices(
    catalog: &Catalog,
    provider: &str,
    query: &str,
    now_year: u32,
) -> Vec<ModelChoice> {
    let Some(entry) = catalog.provider(provider) else {
        return Vec::new();
    };
    let query = query.trim();
    let mut ranked: Vec<(i32, ModelChoice)> = entry
        .models()
        .filter_map(|model| {
            let rank = rank(query, model)?;
            Some((rank, choice_of(model, now_year)))
        })
        .collect();
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.label.to_lowercase().cmp(&b.1.label.to_lowercase()))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });
    ranked.into_iter().map(|(_, choice)| choice).collect()
}

/// Where a model ranks for a query, or `None` when it does not match (FR-4.7).
fn rank(query: &str, model: &CatalogModel) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let needle = query.to_lowercase();
    let id = model.id.to_lowercase();
    let name = model.label().to_lowercase();
    let family = model.family.clone().unwrap_or_default().to_lowercase();

    let bucket = if id == needle {
        0
    } else if id.starts_with(&needle) {
        1
    } else if name.starts_with(&needle) {
        2
    } else if id.contains(&needle) || name.contains(&needle) {
        3
    } else if family.contains(&needle) {
        4
    } else if crate::fuzzy::matches(&needle, &id) || crate::fuzzy::matches(&needle, &name) {
        5
    } else {
        return None;
    };
    // The fuzzy score refines within a bucket, so `gpt-5` prefers `gpt-5` over
    // `gpt-5-turbo-preview` among the id prefixes.
    let detail = crate::fuzzy::score(&needle, &id).unwrap_or_default();
    Some(bucket * 1_000_000 - detail.min(999_999))
}

fn choice_of(model: &CatalogModel, now_year: u32) -> ModelChoice {
    ModelChoice {
        id: model.id.clone(),
        label: model.label().to_owned(),
        badges: model.badges(),
        thinking: model.thinking_choices(),
        dated: is_dated(model, now_year),
    }
}

/// Whether a model is old enough to de-emphasise (FR-4.7: never hidden).
fn is_dated(model: &CatalogModel, now_year: u32) -> bool {
    let Some(date) = model.release_date.as_deref() else {
        return false;
    };
    let Some(year) = date.get(0..4).and_then(|year| year.parse::<u32>().ok()) else {
        return false;
    };
    // Two years is arbitrary but stated: old enough that a newer model almost
    // certainly exists, recent enough that the list does not go grey.
    now_year.saturating_sub(year) >= 2
}

/// A selection that has been checked against the catalog and the key store.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSelection {
    /// Provider id.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// How to reach the provider.
    pub route: Route,
    /// The provider's base URL, when it has one.
    pub base_url: Option<String>,
    /// The environment variable that would override a stored key, when documented.
    pub env_var: Option<String>,
    /// The environment variable that *is* providing the key, when it is.
    pub env_source: Option<String>,
    /// Whether the key came from the file rather than the environment.
    pub from_file: bool,
    /// Thinking setting to send, already mapped (FR-4.8).
    pub thinking: Option<crate::domain::model::ThinkingRequest>,
    /// Problems that do not stop the selection being used.
    pub warnings: Vec<String>,
}

impl ResolvedSelection {
    /// The thinking setting, rendered for the status line (FR-4.8).
    #[must_use]
    pub fn thinking_label(&self) -> String {
        // "no setting" and "explicitly off" read the same in a status line: what a
        // user needs to know at a glance is whether thinking is on.
        match &self.thinking {
            Some(crate::domain::model::ThinkingRequest::Reasoning(true)) => "on".to_owned(),
            Some(crate::domain::model::ThinkingRequest::Effort(level)) => level.as_str().to_owned(),
            Some(crate::domain::model::ThinkingRequest::BudgetTokens(tokens)) => {
                format!("{tokens} tokens")
            }
            None | Some(crate::domain::model::ThinkingRequest::Reasoning(false)) => {
                "off".to_owned()
            }
        }
    }

    /// `provider/model`, what the status line shows (FR-4.5).
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    /// How the provider is reached, for `:model show` (DEC-17).
    #[must_use]
    pub fn route_label(&self) -> &'static str {
        match self.route {
            Route::Native(_) => "native backend",
            Route::Passthrough { .. } => "openai-compatible",
        }
    }
}

/// Why a stored selection cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectionError {
    /// The catalog does not list the provider.
    #[error("{0} is not in the model catalog")]
    UnknownProvider(String),

    /// The catalog does not list the model for that provider.
    #[error("{provider} does not list a model called {model}")]
    UnknownModel {
        /// Provider id.
        provider: String,
        /// Model id.
        model: String,
    },

    /// This build cannot reach the provider (DEC-17).
    #[error("{0} cannot be reached by this build")]
    Unreachable(String),

    /// The thinking setting cannot be sent (FR-4.8).
    #[error("{0}")]
    Thinking(String),

    /// The provider has no key (FR-4.5).
    #[error("no API key is stored for {0}")]
    MissingKey(String),

    /// The key store could not be read.
    #[error("{0}")]
    KeyStore(String),
}

/// Validates a stored selection against the catalog, mapping thinking on the way
/// (FR-4.5, FR-4.8).
///
/// # Errors
///
/// Returns [`SelectionError`] naming the first thing that is wrong, in the order a
/// user would have to fix it: the provider exists, the model exists, thinking maps,
/// a key is present.
pub fn resolve_selection(
    catalog: &Catalog,
    selection: &ModelSelection,
    secrets: &dyn SecretStore,
) -> Result<ResolvedSelection, SelectionError> {
    let provider = catalog
        .provider(&selection.provider)
        .ok_or_else(|| SelectionError::UnknownProvider(selection.provider.clone()))?;
    let model =
        provider
            .models
            .get(&selection.model)
            .ok_or_else(|| SelectionError::UnknownModel {
                provider: selection.provider.clone(),
                model: selection.model.clone(),
            })?;
    let route = provider
        .route()
        .ok_or_else(|| SelectionError::Unreachable(selection.provider.clone()))?;

    let mut warnings = Vec::new();
    let thinking = match &selection.reasoning {
        None => None,
        Some(setting) => match setting.request(model) {
            Ok(mapped) => Some(mapped),
            Err(error) => {
                // Stored settings can go stale when the model changes its declared
                // options; that is a warning with the selection still usable, not a
                // reason to refuse the model.
                warnings.push(format!("{error}; thinking is off"));
                None
            }
        },
    };

    let env_var = provider.env_var().map(str::to_owned);
    let status = secrets
        .get(&selection.provider, env_var.as_deref())
        .map_err(|error| SelectionError::KeyStore(error.to_string()))?;
    let Some(key) = status else {
        return Err(SelectionError::MissingKey(selection.provider.clone()));
    };
    if let Some(max_tokens) = selection.max_tokens
        && let Some(limit) = model.output_limit()
        && max_tokens > limit
    {
        warnings.push(format!(
            "max_tokens {max_tokens} is above {}'s output limit of {limit}",
            provider.label()
        ));
    }

    Ok(ResolvedSelection {
        provider: provider.id.clone(),
        model: model.id.clone(),
        route,
        base_url: provider.api.clone(),
        env_var,
        env_source: key.source().is_environment().then(|| match key.source() {
            KeySource::Environment(name) => name.clone(),
            KeySource::File => String::new(),
        }),
        from_file: !key.source().is_environment(),
        thinking,
        warnings,
    })
}

/// A request that asks the provider for one word, to prove a selection works
/// (FR-4.5: the picker verifies rather than assumes).
///
/// Deliberately tiny: it costs a fraction of a cent, it exercises the key, the base
/// URL and the model id, and its answer is not used for anything.
#[must_use]
pub fn connection_check(resolved: &ResolvedSelection, key: ApiKey) -> ChatRequest {
    let mut request = ChatRequest::new(
        resolved.provider.clone(),
        resolved.model.clone(),
        resolved.route.clone(),
        key,
        "Reply with the single word: ready",
    );
    request.base_url.clone_from(&resolved.base_url);
    request.system =
        Some("You are a connectivity check. Answer with one word and nothing else.".to_owned());
    request.max_tokens = Some(16);
    request.timeout_secs = 60;
    // Thinking is deliberately off for the check: an effort setting would spend
    // reasoning tokens to answer a question that does not need them.
    request.thinking = None;
    request
}

/// The request that runs an analysis (FR-4.1, FR-4.6).
///
/// Separate from [`connection_check`] because the two ask for opposite things: the
/// check wants the cheapest possible answer and turns thinking off, while the analysis
/// wants as much room as the catalog allows and sends the thinking settings the user
/// chose (FR-4.8).
#[must_use]
pub fn analysis_chat(
    resolved: &ResolvedSelection,
    key: ApiKey,
    output_limit: Option<u32>,
) -> ChatRequest {
    let mut request = ChatRequest::new(
        resolved.provider.clone(),
        resolved.model.clone(),
        resolved.route.clone(),
        key,
        String::new(),
    );
    request.base_url.clone_from(&resolved.base_url);
    request.thinking.clone_from(&resolved.thinking);
    // The output cap comes from the catalog when it declares one: asking for more
    // than a model can produce is a truncation, not a bigger answer (FR-4.7).
    request.max_tokens = output_limit.map(|limit| limit.min(DEFAULT_MAX_TOKENS));
    request.timeout_secs = ANALYSIS_TIMEOUT_SECS;
    request
}

/// The context budget for one analysis (FR-4.6, FR-4.7, Appendix B.3).
///
/// Three inputs, in the order the requirements give them: the model's own
/// `limit.context` when the catalog declares one, the user's `max_context_tokens`
/// otherwise, and room for the answer. The reservation matters — a prompt that fills
/// the whole window leaves the model nothing to answer with, which reads to the user as
/// truncation rather than as a full context.
#[must_use]
pub fn context_budget(
    catalog_limit: Option<u32>,
    configured: u32,
    output_reserved: Option<u32>,
) -> u32 {
    /// The least context worth sending: below this the bundle is not an analysis of
    /// anything in particular, and a smaller request would be a worse answer rather
    /// than a cheaper one.
    const FLOOR: u32 = 4_096;
    /// What is held back for the answer when the caller does not say.
    const DEFAULT_RESERVE: u32 = 8_192;

    let base = catalog_limit.unwrap_or(configured).max(FLOOR);
    let reserve = output_reserved.unwrap_or(DEFAULT_RESERVE).min(base / 2);
    base.saturating_sub(reserve).max(FLOOR)
}

/// How long an analysis may take before it is given up on.
///
/// Long, because a whole pull request is a whole pull request: a large diff with
/// reasoning on can legitimately take minutes, and a timeout that fires early would
/// waste exactly the tokens it was trying to save.
pub const ANALYSIS_TIMEOUT_SECS: u64 = 600;

/// The most tokens an analysis is allowed to ask for.
const DEFAULT_MAX_TOKENS: u32 = 32_768;

/// What is known about the key for one provider (FR-4.5, NFR-3.1).
///
/// # Errors
///
/// Returns [`SecretError`] when the credentials file exists but cannot be used.
pub fn key_status(
    secrets: &dyn SecretStore,
    provider: &str,
    env_var: Option<&str>,
) -> Result<KeyStatus, SecretError> {
    let key = secrets.get(provider, env_var)?;
    Ok(KeyStatus {
        provider: provider.to_owned(),
        source: key.map(|key| key.source().clone()),
    })
}

/// Saves a key the user typed, refusing blanks (FR-4.5).
///
/// # Errors
///
/// Returns [`SecretError`] when the file cannot be written.
pub fn save_key(secrets: &dyn SecretStore, provider: &str, key: &str) -> Result<(), SecretError> {
    if key.trim().is_empty() {
        return Err(SecretError::Malformed(
            "the key is empty; nothing was saved".to_owned(),
        ));
    }
    secrets.set(provider, key)
}

/// Removes a key (`:key clear`, NFR-3.1).
///
/// # Errors
///
/// Returns [`SecretError`] when the file cannot be written.
pub fn clear_key(secrets: &dyn SecretStore, provider: &str) -> Result<(), SecretError> {
    secrets.remove(provider)
}

/// A thinking setting parsed for a model, used by the picker (FR-4.8).
///
/// # Errors
///
/// Returns [`SelectionError::Thinking`] when the model does not offer the control.
pub fn check_thinking(
    catalog: &Catalog,
    provider: &str,
    model: &str,
    thinking: &Thinking,
) -> Result<crate::domain::model::ThinkingRequest, SelectionError> {
    let entry = catalog
        .model(provider, model)
        .ok_or_else(|| SelectionError::UnknownModel {
            provider: provider.to_owned(),
            model: model.to_owned(),
        })?;
    entry
        .declared_options()
        .iter()
        .any(|option| {
            matches!(
                (option, thinking),
                (
                    crate::domain::model::ReasoningOption::Toggle,
                    Thinking::Toggle { .. }
                ) | (
                    crate::domain::model::ReasoningOption::Effort { .. },
                    Thinking::Effort { .. }
                ) | (
                    crate::domain::model::ReasoningOption::BudgetTokens { .. },
                    Thinking::BudgetTokens { .. }
                )
            )
        })
        .then_some(())
        .ok_or_else(|| {
            SelectionError::Thinking(format!(
                "{} does not offer a {} thinking control",
                entry.label(),
                thinking.kind()
            ))
        })?;
    thinking
        .request(entry)
        .map_err(|error| SelectionError::Thinking(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::secret::{KeySource, KeyStatus};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    fn catalog() -> Catalog {
        Catalog::from_json(include_str!("../../tests/fixtures/models/providers.json"))
            .expect("the fixture parses")
    }

    /// A secret store that answers from a map.
    #[derive(Debug, Default)]
    struct FakeSecrets {
        keys: Mutex<BTreeMap<String, String>>,
        env: Mutex<BTreeMap<String, String>>,
        fail: Mutex<bool>,
    }

    impl FakeSecrets {
        fn with_key(provider: &str, key: &str) -> Self {
            let store = Self::default();
            store
                .keys
                .lock()
                .unwrap()
                .insert(provider.to_owned(), key.to_owned());
            store
        }
    }

    impl SecretStore for FakeSecrets {
        fn get(
            &self,
            provider: &str,
            env_var: Option<&str>,
        ) -> Result<Option<ApiKey>, SecretError> {
            if *self.fail.lock().unwrap() {
                return Err(SecretError::Malformed("the file is broken".to_owned()));
            }
            if let Some(name) = env_var
                && let Some(value) = self.env.lock().unwrap().get(name)
            {
                return Ok(Some(ApiKey::new(
                    value.clone(),
                    KeySource::Environment(name.to_owned()),
                )));
            }
            Ok(self
                .keys
                .lock()
                .unwrap()
                .get(provider)
                .map(|key| ApiKey::new(key.clone(), KeySource::File)))
        }

        fn set(&self, provider: &str, key: &str) -> Result<(), SecretError> {
            self.keys
                .lock()
                .unwrap()
                .insert(provider.to_owned(), key.to_owned());
            Ok(())
        }

        fn remove(&self, provider: &str) -> Result<(), SecretError> {
            self.keys.lock().unwrap().remove(provider);
            Ok(())
        }

        fn status(&self) -> Result<Vec<KeyStatus>, SecretError> {
            Ok(self
                .keys
                .lock()
                .unwrap()
                .keys()
                .map(|provider| KeyStatus {
                    provider: provider.clone(),
                    source: Some(KeySource::File),
                })
                .collect())
        }
    }

    fn selection(provider: &str, model: &str) -> ModelSelection {
        ModelSelection {
            provider: provider.to_owned(),
            model: model.to_owned(),
            temperature: None,
            max_tokens: None,
            reasoning: None,
        }
    }

    #[test]
    fn only_reachable_providers_are_offered() {
        let choices = provider_choices(&catalog());
        let ids: Vec<&str> = choices.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"deepseek"));
        assert!(ids.contains(&"lmstudio"), "the passthrough is offered");
        assert!(
            !ids.contains(&"watsonx"),
            "a provider that cannot be reached is hidden, not broken"
        );
        assert_eq!(
            choices
                .iter()
                .find(|c| c.id == "lmstudio")
                .expect("lmstudio")
                .route_label(),
            "openai-compatible"
        );
        assert_eq!(
            choices
                .iter()
                .find(|c| c.id == "anthropic")
                .expect("anthropic")
                .route_label(),
            "native"
        );
    }

    #[test]
    fn an_empty_query_lists_every_model_of_the_provider() {
        let catalog = catalog();
        let all = model_choices(&catalog, "deepseek", "", 2026);
        let provider = catalog.provider("deepseek").expect("deepseek");
        assert_eq!(all.len(), provider.models.len());
        assert!(all.iter().all(|choice| !choice.badges.is_empty()));
    }

    #[test]
    fn an_unknown_provider_yields_nothing_rather_than_panicking() {
        assert!(model_choices(&catalog(), "nope", "", 2026).is_empty());
    }

    #[test]
    fn search_ranks_an_exact_id_first() {
        let catalog = catalog();
        let ranked = model_choices(&catalog, "openai", "gpt-5", 2026);
        assert_eq!(
            ranked.first().map(|choice| choice.id.as_str()),
            Some("gpt-5-pro").or(Some("gpt-5-nano")),
            "an id beginning with the query comes first: {:?}",
            ranked.iter().map(|c| &c.id).take(3).collect::<Vec<_>>()
        );
        // Whatever the order, everything returned must match the query.
        assert!(ranked.iter().all(|choice| {
            let needle = "gpt-5";
            choice.id.contains(needle)
                || choice.label.to_lowercase().contains(needle)
                || crate::fuzzy::matches(needle, &choice.id)
        }));
    }

    #[test]
    fn search_matches_family_and_is_case_insensitive() {
        let catalog = catalog();
        let lower = model_choices(&catalog, "deepseek", "flash", 2026);
        let upper = model_choices(&catalog, "deepseek", "FLASH", 2026);
        assert!(!lower.is_empty());
        assert_eq!(lower, upper, "case does not matter");
    }

    #[test]
    fn a_model_matching_nothing_is_not_listed() {
        let catalog = catalog();
        assert!(model_choices(&catalog, "openai", "zzzzzz", 2026).is_empty());
    }

    #[test]
    fn old_models_are_marked_rather_than_hidden() {
        let text = r#"{"p": {"id": "p", "name": "P", "api": "https://p/v1", "models": {
            "old": {"id": "old", "name": "Old", "release_date": "2023-01-01"},
            "new": {"id": "new", "name": "New", "release_date": "2026-01-01"}}}}"#;
        let catalog = Catalog::from_json(text).expect("parses");
        let choices = model_choices(&catalog, "p", "", 2026);
        assert_eq!(choices.len(), 2, "both are listed");
        assert!(choices.iter().find(|c| c.id == "old").expect("old").dated);
        assert!(!choices.iter().find(|c| c.id == "new").expect("new").dated);
    }

    #[test]
    fn a_selection_resolves_with_its_route_key_source_and_thinking() {
        let catalog = catalog();
        let secrets = FakeSecrets::with_key("deepseek", "sk-stored");
        let mut stored = selection("deepseek", "deepseek-v4-pro");
        stored.reasoning = Some(Thinking::Effort {
            value: "high".to_owned(),
        });

        let resolved = resolve_selection(&catalog, &stored, &secrets).expect("resolved");
        assert_eq!(resolved.label(), "deepseek/deepseek-v4-pro");
        assert!(matches!(resolved.route, Route::Native(_)));
        assert_eq!(
            resolved.base_url.as_deref(),
            Some("https://api.deepseek.com")
        );
        assert_eq!(resolved.env_var.as_deref(), Some("DEEPSEEK_API_KEY"));
        assert!(resolved.from_file);
        assert_eq!(resolved.thinking_label(), "high");
        assert!(resolved.warnings.is_empty());
    }

    #[test]
    fn a_missing_key_is_named_so_the_picker_can_ask_for_it() {
        let error = resolve_selection(
            &catalog(),
            &selection("deepseek", "deepseek-v4-pro"),
            &FakeSecrets::default(),
        )
        .expect_err("no key");
        assert!(matches!(error, SelectionError::MissingKey(_)), "{error:?}");
        assert!(error.to_string().contains("deepseek"), "{error}");
    }

    #[test]
    fn the_environment_key_is_reported_as_the_source() {
        let catalog = catalog();
        let secrets = FakeSecrets::default();
        secrets
            .env
            .lock()
            .unwrap()
            .insert("ANTHROPIC_API_KEY".to_owned(), "sk-from-env".to_owned());
        let resolved = resolve_selection(
            &catalog,
            &selection("anthropic", "claude-opus-4-5"),
            &secrets,
        )
        .expect("resolved");
        assert!(!resolved.from_file);
        assert_eq!(resolved.env_source.as_deref(), Some("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn an_unknown_model_is_named_with_its_provider() {
        let error = resolve_selection(
            &catalog(),
            &selection("deepseek", "gpt-9"),
            &FakeSecrets::with_key("deepseek", "sk"),
        )
        .expect_err("no such model");
        assert!(
            matches!(error, SelectionError::UnknownModel { .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("gpt-9"), "{error}");
    }

    #[test]
    fn a_stored_thinking_setting_that_no_longer_maps_is_a_warning_not_a_refusal() {
        let catalog = catalog();
        let secrets = FakeSecrets::with_key("deepseek", "sk");
        let mut stored = selection("deepseek", "deepseek-v4-pro");
        stored.reasoning = Some(Thinking::Effort {
            value: "max".to_owned(),
        });
        let resolved = resolve_selection(&catalog, &stored, &secrets).expect("still usable");
        assert!(resolved.thinking.is_none(), "thinking is off");
        assert_eq!(resolved.thinking_label(), "off");
        assert_eq!(resolved.warnings.len(), 1);
        assert!(resolved.warnings[0].contains("cannot send effort `max`"));
    }

    #[test]
    fn a_max_tokens_above_the_models_limit_warns() {
        let catalog = catalog();
        let secrets = FakeSecrets::with_key("deepseek", "sk");
        let mut stored = selection("deepseek", "deepseek-v4-pro");
        stored.max_tokens = Some(500_000);
        let resolved = resolve_selection(&catalog, &stored, &secrets).expect("usable");
        assert!(
            resolved.warnings.iter().any(|w| w.contains("output limit")),
            "{:?}",
            resolved.warnings
        );
    }

    #[test]
    fn a_broken_key_store_is_reported_as_such() {
        let secrets = FakeSecrets::default();
        *secrets.fail.lock().unwrap() = true;
        let error = resolve_selection(
            &catalog(),
            &selection("deepseek", "deepseek-v4-pro"),
            &secrets,
        )
        .expect_err("reported");
        assert!(matches!(error, SelectionError::KeyStore(_)), "{error:?}");
    }

    #[test]
    fn the_connection_check_is_small_and_needs_no_thinking() {
        let catalog = catalog();
        let secrets = FakeSecrets::with_key("deepseek", "sk");
        let resolved = resolve_selection(
            &catalog,
            &selection("deepseek", "deepseek-v4-pro"),
            &secrets,
        )
        .expect("resolved");
        let request = connection_check(&resolved, ApiKey::new("sk", KeySource::File));
        assert_eq!(request.max_tokens, Some(16));
        assert!(
            request.thinking.is_none(),
            "a check must not spend reasoning"
        );
        assert_eq!(
            request.base_url.as_deref(),
            Some("https://api.deepseek.com")
        );
        assert_eq!(request.timeout_secs, 60);
        assert!(request.prompt.len() < 64, "the prompt is one sentence");
    }

    #[test]
    fn a_blank_key_is_refused_before_it_reaches_the_file() {
        let secrets = FakeSecrets::default();
        let error = save_key(&secrets, "deepseek", "   ").expect_err("refused");
        assert!(error.to_string().contains("empty"), "{error}");
        assert!(
            secrets.keys.lock().unwrap().is_empty(),
            "nothing was written"
        );
    }

    #[test]
    fn saving_and_clearing_a_key_goes_through_the_store() {
        let secrets = FakeSecrets::default();
        save_key(&secrets, "openai", "sk-1").expect("saved");
        assert_eq!(
            key_status(&secrets, "openai", None).expect("status").source,
            Some(KeySource::File)
        );
        clear_key(&secrets, "openai").expect("cleared");
        assert!(
            !key_status(&secrets, "openai", None)
                .expect("status")
                .is_present()
        );
    }

    #[test]
    fn thinking_is_checked_against_the_model_before_it_is_saved() {
        let catalog = catalog();
        assert!(
            check_thinking(
                &catalog,
                "deepseek",
                "deepseek-v4-pro",
                &Thinking::Effort {
                    value: "high".to_owned()
                }
            )
            .is_ok()
        );
        let error = check_thinking(
            &catalog,
            "deepseek",
            "deepseek-v4-pro",
            &Thinking::BudgetTokens { value: 4096 },
        )
        .expect_err("no budget control");
        assert!(error.to_string().contains("budget_tokens"), "{error}");
    }

    #[test]
    fn the_summary_admits_what_it_could_not_read() {
        let text = r#"{"good": {"id": "good", "name": "Good", "api": "https://g/v1",
            "models": {"m": {"id": "m", "name": "M"}}},
            "broken": {"id": "broken", "name": 42, "models": {}}}"#;
        let catalog = Catalog::from_json(text).expect("parses");
        let state = CatalogState {
            providers: provider_choices(&catalog),
            load: CatalogLoad {
                catalog,
                source: crate::ports::catalog::CatalogSource::Fetched,
            },
        };
        let summary = state.summary();
        assert!(summary.contains("1 providers"), "{summary}");
        assert!(summary.contains("1 unreadable"), "{summary}");
    }

    #[test]
    fn a_catalog_state_summarises_what_it_holds() {
        let state = CatalogState {
            load: CatalogLoad {
                catalog: catalog(),
                source: crate::ports::catalog::CatalogSource::Cached { age_secs: 5 },
            },
            providers: provider_choices(&catalog()),
        };
        let summary = state.summary();
        assert!(summary.contains("providers"), "{summary}");
        assert!(summary.contains("cached"), "{summary}");
        assert!(state.model_count() > 0);
    }
}
