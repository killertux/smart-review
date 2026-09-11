//! Configuration: the typed settings (FR-8.2) and the document behind them
//! (FR-8.6).
//!
//! Two guarantees matter here:
//!
//! 1. **The app always starts.** Every key is read individually, so one malformed
//!    value costs that key only — the rest of the file still applies (FR-8.6).
//! 2. **Nothing is silently lost.** Unknown keys are reported as warnings and are
//!    left in place when the document is written back. Comments are *not*
//!    preserved by this implementation; comment-preserving writes need
//!    `toml_edit` (see DEC-19 in `REQUIREMENTS.md`), which is why configuration
//!    is never written back in M0.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Applies one key, keeping the default and reporting the problem when the value
/// cannot be read as the expected type (FR-8.6).
///
/// Defined before its first use because `macro_rules!` macros are scoped
/// textually.
macro_rules! apply_key {
    ($warnings:expr, $section:literal, $table:expr, $key:literal, $target:expr, $ty:ty) => {
        if let Some(value) = $table.get($key) {
            match value.clone().try_into::<$ty>() {
                Ok(parsed) => $target = parsed,
                Err(error) => $warnings.push(format!(
                    "config: [{section}].{key} {error}; keeping the default",
                    section = $section,
                    key = $key
                )),
            }
        }
    };
}

/// The settings a user can change.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    /// Terminal presentation (FR-7.7, FR-7.8).
    pub ui: UiConfig,
    /// Pull request listing and diff defaults (FR-2.1, FR-3.2).
    pub review: ReviewConfig,
    /// How PR code is materialized on disk (FR-3.1).
    pub workspace: WorkspaceConfig,
    /// LLM selection and limits (FR-4.5, FR-4.6).
    pub llm: LlmConfig,
    /// models.dev catalog access (FR-4.7).
    pub catalog: CatalogConfig,
    /// GitHub access (FR-1.1).
    pub forge: ForgeConfig,
    /// Cache lifetimes (FR-2.3).
    pub cache: CacheConfig,
    /// Logging (FR-9.2).
    pub log: LogConfig,
}

impl Config {
    /// The built-in defaults, so callers do not have to spell out every field.
    #[must_use]
    pub fn built_in() -> Self {
        Self::default()
    }
}

/// `[ui]` (FR-7.7, FR-8.2).
#[derive(Debug, Clone, PartialEq)]
pub struct UiConfig {
    /// Built-in theme name or the stem of a file in `themes/`.
    pub theme: String,
    /// Whether mouse capture is enabled (FR-7.5).
    pub mouse: bool,
    /// Leader key for the action menu (FR-7.2).
    pub leader: String,
    /// Ambiguity timeout for multi-key sequences, in milliseconds (FR-7.2).
    pub timeoutlen: u64,
    /// Whether to render icons where the terminal supports them.
    pub icons: bool,
    /// `relative` or `absolute` timestamps in lists.
    pub date_format: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".to_owned(),
            mouse: true,
            leader: "<Space>".to_owned(),
            timeoutlen: 500,
            icons: true,
            date_format: "relative".to_owned(),
        }
    }
}

/// `[review]` (FR-2.1, FR-3.2).
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewConfig {
    /// Default PR state filter: `open`, `closed`, `merged` or `all`.
    pub state: String,
    /// PRs fetched per page.
    pub page_size: usize,
    /// Upper bound on pages loaded via `:load-more`.
    pub max_pages: usize,
    /// Context lines shown around each hunk.
    pub context_lines: u32,
    /// Whether whitespace-only changes are ignored.
    pub ignore_whitespace: bool,
    /// `recommended` (LLM review plan) or `path` order (FR-3.5).
    pub order: String,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            state: "open".to_owned(),
            page_size: 50,
            max_pages: 10,
            context_lines: 3,
            ignore_whitespace: false,
            order: "recommended".to_owned(),
        }
    }
}

/// `[workspace]` (FR-3.1).
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceConfig {
    /// `worktree` (managed, the only supported mode in v1) or `none`.
    pub mode: String,
    /// Whether worktrees survive the session.
    pub keep_on_exit: bool,
    /// Age after which unused worktrees may be cleaned.
    pub auto_clean_days: u32,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            mode: "worktree".to_owned(),
            keep_on_exit: true,
            auto_clean_days: 14,
        }
    }
}

/// `[llm]` (FR-4.5, FR-4.6).
///
/// There is deliberately no default provider or model (DEC-6): the active
/// selection is created by the user in the TUI.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmConfig {
    /// The selection in use, or `None` when nothing has been configured yet.
    pub active: Option<ModelSelection>,
    /// Optional user-created presets.
    pub presets: BTreeMap<String, ModelSelection>,
    /// Upper bound on the prompt sent to the model (FR-4.6).
    pub max_context_tokens: u32,
    /// Files larger than this are replaced by a placeholder (FR-4.6).
    pub max_file_bytes: u64,
    /// Bound for agentic tool loops once they exist (FR-5.3).
    pub max_tool_calls: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            active: None,
            presets: BTreeMap::new(),
            max_context_tokens: 100_000,
            max_file_bytes: 262_144,
            max_tool_calls: 8,
        }
    }
}

/// A provider/model/thinking selection (§7.4 of the requirements).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelSelection {
    /// Provider id as published by the catalog, e.g. `deepseek`.
    pub provider: String,
    /// Model id as published by the provider.
    pub model: String,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Response token cap; falls back to the catalog's `limit.output`.
    pub max_tokens: Option<u32>,
    /// Thinking settings. The shape depends on the model's `reasoning_options`,
    /// so it is kept as an untyped value until the picker (M2) owns it.
    pub reasoning: Option<toml::Value>,
}

impl ModelSelection {
    /// Reports why a selection cannot be used, if it cannot.
    ///
    /// # Errors
    ///
    /// Returns the reason the selection is unusable.
    pub fn validate(&self) -> Result<(), String> {
        if self.provider.trim().is_empty() {
            return Err("`provider` is empty".to_owned());
        }
        if self.model.trim().is_empty() {
            return Err("`model` is empty".to_owned());
        }
        Ok(())
    }
}

/// `[catalog]` (FR-4.7).
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogConfig {
    /// Where the catalog is fetched from.
    pub url: String,
    /// Cache freshness window.
    pub ttl_hours: u32,
}

impl Default for CatalogConfig {
    fn default() -> Self {
        Self {
            url: "https://models.dev/api.json".to_owned(),
            ttl_hours: 24,
        }
    }
}

/// `[forge]` (FR-1.1).
#[derive(Debug, Clone, PartialEq)]
pub struct ForgeConfig {
    /// Remote to prefer; otherwise `origin`, then the first GitHub remote.
    pub remote: Option<String>,
    /// Path to the `gh` executable.
    pub gh_path: String,
    /// `gh` page size for list operations.
    pub page_size: usize,
}

impl Default for ForgeConfig {
    fn default() -> Self {
        Self {
            remote: None,
            gh_path: "gh".to_owned(),
            page_size: 50,
        }
    }
}

/// `[cache]` (FR-2.3).
#[derive(Debug, Clone, PartialEq)]
pub struct CacheConfig {
    /// Freshness window for PR lists.
    pub ttl_list_secs: u64,
    /// Freshness window for PR details.
    pub ttl_detail_secs: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            ttl_list_secs: 60,
            ttl_detail_secs: 300,
        }
    }
}

/// `[log]` (FR-9.2).
#[derive(Debug, Clone, PartialEq)]
pub struct LogConfig {
    /// `error`, `warn`, `info`, `debug` or `trace`.
    pub level: String,
    /// Overrides the default log file location when set.
    pub path: Option<String>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
            path: None,
        }
    }
}

/// Everything that can go wrong while reading configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read config file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("config file {path} is not valid TOML: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("could not write config file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A loaded configuration plus everything the caller needs to report on it.
#[derive(Debug)]
pub struct Loaded {
    /// The effective settings (defaults merged with the file).
    pub config: Config,
    /// The parsed file, kept so unknown keys survive a future write.
    pub document: ConfigDocument,
    /// Non-fatal problems: unknown keys, unreadable values.
    pub warnings: Vec<String>,
    /// Where the file lives (or would live).
    pub path: PathBuf,
    /// Whether a file was actually read.
    pub exists: bool,
}

impl Loaded {
    /// Human-readable description of where the settings came from.
    #[must_use]
    pub const fn source(&self) -> &'static str {
        if self.exists {
            "file"
        } else {
            "built-in defaults"
        }
    }
}

/// Reads `path`, falling back to built-in defaults for anything missing or
/// unusable.
///
/// # Errors
///
/// Returns [`ConfigError`] when the file exists but cannot be read or parsed.
pub fn load(path: &Path) -> Result<Loaded, ConfigError> {
    if !path.exists() {
        return Ok(Loaded {
            config: Config::default(),
            document: ConfigDocument::default(),
            warnings: Vec::new(),
            path: path.to_path_buf(),
            exists: false,
        });
    }

    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let document = ConfigDocument::parse(&text, path)?;
    let mut warnings = Vec::new();
    let config = document.to_config(&mut warnings);
    // Name the file: "which config am I actually loading?" is the first question
    // a warning should answer (FR-8.6).
    let warnings = warnings
        .into_iter()
        .map(|warning| format!("{}: {warning}", path.display()))
        .collect();

    Ok(Loaded {
        config,
        document,
        warnings,
        path: path.to_path_buf(),
        exists: true,
    })
}

/// The parsed TOML document, kept alongside the typed view so unknown keys are
/// not destroyed when settings are written back (FR-8.6).
#[derive(Debug, Clone, Default)]
pub struct ConfigDocument {
    value: toml::Table,
}

impl ConfigDocument {
    /// How many keys the file declares, counting nested tables as one each.
    ///
    /// `:doctor` reports this so that "the configuration parsed" is a statement with
    /// something behind it rather than the absence of an error (FR-9.3).
    #[must_use]
    pub fn key_count(&self) -> usize {
        fn walk(table: &toml::Table) -> usize {
            table
                .values()
                .map(|value| match value {
                    toml::Value::Table(nested) => 1 + walk(nested),
                    _ => 1,
                })
                .sum()
        }
        walk(&self.value)
    }

    /// Parses `text` as a TOML document.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Parse`] when the text is not valid TOML.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let value: toml::Table = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
        Ok(Self { value })
    }

    /// The raw document.
    #[must_use]
    pub const fn value(&self) -> &toml::Table {
        &self.value
    }

    /// Reads one key inside a section.
    #[must_use]
    pub fn get(&self, section: &str, key: &str) -> Option<&toml::Value> {
        self.section(section)?.get(key)
    }

    /// Sets one key inside a section, creating the section if needed. Existing
    /// keys elsewhere in the document are untouched.
    pub fn set(&mut self, section: &str, key: &str, value: toml::Value) {
        let entry = self
            .value
            .entry(section.to_owned())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(table) = entry.as_table_mut() {
            table.insert(key.to_owned(), value);
        }
    }

    /// Serialises the document, preserving keys this build does not understand.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the document cannot be serialised.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(&self.value).map_err(|source| ConfigError::Parse {
            path: PathBuf::from("<memory>"),
            message: source.to_string(),
        })
    }

    fn section(&self, name: &str) -> Option<&toml::Table> {
        self.value.get(name)?.as_table()
    }

    /// Builds the typed configuration, collecting a warning per unusable key.
    ///
    /// Long on purpose: one explicit block per section keeps every key's default
    /// and warning message in a single readable place (FR-8.6).
    #[allow(clippy::too_many_lines)]
    pub fn to_config(&self, warnings: &mut Vec<String>) -> Config {
        let mut config = Config::default();

        for key in self.value.keys() {
            if !KNOWN_SECTIONS.contains(&key.as_str()) {
                warnings.push(format!(
                    "config: unknown section [{key}] is ignored (left untouched in the file)"
                ));
            }
        }

        if let Some(table) = self.section("ui") {
            apply_key!(warnings, "ui", table, "theme", config.ui.theme, String);
            apply_key!(warnings, "ui", table, "mouse", config.ui.mouse, bool);
            apply_key!(warnings, "ui", table, "leader", config.ui.leader, String);
            apply_key!(
                warnings,
                "ui",
                table,
                "timeoutlen",
                config.ui.timeoutlen,
                u64
            );
            apply_key!(warnings, "ui", table, "icons", config.ui.icons, bool);
            apply_key!(
                warnings,
                "ui",
                table,
                "date_format",
                config.ui.date_format,
                String
            );
            warn_unknown(table, "ui", UI_KEYS, warnings);
        }

        if let Some(table) = self.section("review") {
            apply_key!(
                warnings,
                "review",
                table,
                "state",
                config.review.state,
                String
            );
            apply_key!(
                warnings,
                "review",
                table,
                "page_size",
                config.review.page_size,
                usize
            );
            apply_key!(
                warnings,
                "review",
                table,
                "max_pages",
                config.review.max_pages,
                usize
            );
            apply_key!(
                warnings,
                "review",
                table,
                "context_lines",
                config.review.context_lines,
                u32
            );
            apply_key!(
                warnings,
                "review",
                table,
                "ignore_whitespace",
                config.review.ignore_whitespace,
                bool
            );
            apply_key!(
                warnings,
                "review",
                table,
                "order",
                config.review.order,
                String
            );
            warn_unknown(table, "review", REVIEW_KEYS, warnings);
        }

        if let Some(table) = self.section("workspace") {
            apply_key!(
                warnings,
                "workspace",
                table,
                "mode",
                config.workspace.mode,
                String
            );
            apply_key!(
                warnings,
                "workspace",
                table,
                "keep_on_exit",
                config.workspace.keep_on_exit,
                bool
            );
            apply_key!(
                warnings,
                "workspace",
                table,
                "auto_clean_days",
                config.workspace.auto_clean_days,
                u32
            );
            warn_unknown(table, "workspace", WORKSPACE_KEYS, warnings);
        }

        if let Some(table) = self.section("llm") {
            apply_key!(
                warnings,
                "llm",
                table,
                "max_context_tokens",
                config.llm.max_context_tokens,
                u32
            );
            apply_key!(
                warnings,
                "llm",
                table,
                "max_file_bytes",
                config.llm.max_file_bytes,
                u64
            );
            apply_key!(
                warnings,
                "llm",
                table,
                "max_tool_calls",
                config.llm.max_tool_calls,
                u32
            );
            read_selection(table, "llm.active", warnings, &mut config.llm.active);
            if let Some(presets) = table.get("presets").and_then(toml::Value::as_table) {
                for (name, value) in presets {
                    let mut selection = None;
                    read_selection_value(
                        value,
                        &format!("llm.presets.{name}"),
                        warnings,
                        &mut selection,
                    );
                    if let Some(selection) = selection {
                        config.llm.presets.insert(name.clone(), selection);
                    }
                }
            }
            warn_unknown(table, "llm", LLM_KEYS, warnings);
        }

        if let Some(table) = self.section("catalog") {
            apply_key!(
                warnings,
                "catalog",
                table,
                "url",
                config.catalog.url,
                String
            );
            apply_key!(
                warnings,
                "catalog",
                table,
                "ttl_hours",
                config.catalog.ttl_hours,
                u32
            );
            warn_unknown(table, "catalog", CATALOG_KEYS, warnings);
        }

        if let Some(table) = self.section("forge") {
            apply_key!(
                warnings,
                "forge",
                table,
                "gh_path",
                config.forge.gh_path,
                String
            );
            apply_key!(
                warnings,
                "forge",
                table,
                "page_size",
                config.forge.page_size,
                usize
            );
            if let Some(value) = table.get("remote") {
                match value.clone().try_into::<String>() {
                    Ok(remote) => config.forge.remote = Some(remote),
                    Err(error) => warnings.push(format!(
                        "config: [forge].remote {error}; using automatic detection"
                    )),
                }
            }
            warn_unknown(table, "forge", FORGE_KEYS, warnings);
        }

        if let Some(table) = self.section("cache") {
            apply_key!(
                warnings,
                "cache",
                table,
                "ttl_list_secs",
                config.cache.ttl_list_secs,
                u64
            );
            apply_key!(
                warnings,
                "cache",
                table,
                "ttl_detail_secs",
                config.cache.ttl_detail_secs,
                u64
            );
            warn_unknown(table, "cache", CACHE_KEYS, warnings);
        }

        if let Some(table) = self.section("log") {
            apply_key!(warnings, "log", table, "level", config.log.level, String);
            if let Some(value) = table.get("path") {
                match value.clone().try_into::<String>() {
                    Ok(path) => config.log.path = Some(path),
                    Err(error) => {
                        warnings.push(format!("[log].path {error}; using the default"));
                    }
                }
            }
            warn_unknown(table, "log", LOG_KEYS, warnings);
        }

        // Reject impossible values that would fail silently much later.
        if !matches!(config.workspace.mode.as_str(), "worktree" | "none") {
            warnings.push(format!(
                "config: [workspace].mode = {:?} is not supported; using \"worktree\"",
                config.workspace.mode
            ));
            "worktree".clone_into(&mut config.workspace.mode);
        }
        if crate::logging::Level::parse(&config.log.level).is_none() {
            warnings.push(format!(
                "config: [log].level = {:?} is not a level; using \"info\"",
                config.log.level
            ));
            "info".clone_into(&mut config.log.level);
        }

        config
    }
}

/// Reads `[llm.active]`, which may be absent.
fn read_selection(
    table: &toml::Table,
    label: &str,
    warnings: &mut Vec<String>,
    target: &mut Option<ModelSelection>,
) {
    let Some(value) = table.get("active") else {
        return;
    };
    read_selection_value(value, label, warnings, target);
}

fn read_selection_value(
    value: &toml::Value,
    label: &str,
    warnings: &mut Vec<String>,
    target: &mut Option<ModelSelection>,
) {
    match value.clone().try_into::<ModelSelection>() {
        Ok(selection) => match selection.validate() {
            Ok(()) => *target = Some(selection),
            Err(reason) => warnings.push(format!(
                "config: [{label}] is incomplete ({reason}); no model is configured"
            )),
        },
        Err(error) => warnings.push(format!("[{label}] {error}; no model is configured")),
    }
}

fn warn_unknown(table: &toml::Table, section: &str, known: &[&str], warnings: &mut Vec<String>) {
    for key in table.keys() {
        if !known.contains(&key.as_str()) {
            warnings.push(format!(
                "config: unknown key [{section}].{key} is ignored (left untouched in the file)"
            ));
        }
    }
}

const KNOWN_SECTIONS: &[&str] = &[
    "ui",
    "review",
    "workspace",
    "llm",
    "catalog",
    "forge",
    "cache",
    "log",
];

const UI_KEYS: &[&str] = &[
    "theme",
    "mouse",
    "leader",
    "timeoutlen",
    "icons",
    "date_format",
];
const REVIEW_KEYS: &[&str] = &[
    "state",
    "page_size",
    "max_pages",
    "context_lines",
    "ignore_whitespace",
    "order",
];
const WORKSPACE_KEYS: &[&str] = &["mode", "keep_on_exit", "auto_clean_days"];
const LLM_KEYS: &[&str] = &[
    "active",
    "presets",
    "max_context_tokens",
    "max_file_bytes",
    "max_tool_calls",
];
const CATALOG_KEYS: &[&str] = &["url", "ttl_hours"];
const FORGE_KEYS: &[&str] = &["remote", "gh_path", "page_size"];
const CACHE_KEYS: &[&str] = &["ttl_list_secs", "ttl_detail_secs"];
const LOG_KEYS: &[&str] = &["level", "path"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_home;

    fn parse(text: &str) -> (Config, Vec<String>, ConfigDocument) {
        let dir = temp_home();
        let path = dir.write("config.toml", text);
        let loaded = load(&path).unwrap();
        (loaded.config, loaded.warnings, loaded.document)
    }

    #[test]
    fn empty_document_yields_defaults() {
        let (config, warnings, _) = parse("");
        assert_eq!(config, Config::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = temp_home();
        let loaded = load(&dir.path().join("nope.toml")).unwrap();
        assert!(!loaded.exists);
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.source(), "built-in defaults");
    }

    #[test]
    fn values_override_defaults() {
        let (config, warnings, _) = parse(
            r#"
            [ui]
            theme = "light"
            timeoutlen = 250
            mouse = false

            [review]
            page_size = 100
            "#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(config.ui.theme, "light");
        assert_eq!(config.ui.timeoutlen, 250);
        assert!(!config.ui.mouse);
        assert_eq!(config.review.page_size, 100);
        // Untouched keys keep their defaults.
        assert_eq!(config.review.context_lines, 3);
    }

    #[test]
    fn a_bad_value_costs_only_that_key() {
        let (config, warnings, _) = parse(
            r#"
            [ui]
            timeoutlen = "soon"
            theme = "light"
            "#,
        );
        assert_eq!(config.ui.timeoutlen, UiConfig::default().timeoutlen);
        assert_eq!(config.ui.theme, "light");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("[ui].timeoutlen"), "{warnings:?}");
    }

    #[test]
    fn unknown_keys_are_reported_and_preserved() {
        let (_, warnings, document) = parse(
            r#"
            [ui]
            theme = "light"
            future_option = 7

            [not_a_section]
            hello = "world"
            "#,
        );
        assert!(warnings.iter().any(|w| w.contains("future_option")));
        assert!(warnings.iter().any(|w| w.contains("not_a_section")));

        // Writing the document back keeps the keys this build does not know.
        let text = document.to_toml().unwrap();
        assert!(text.contains("future_option"), "{text}");
        assert!(text.contains("not_a_section"), "{text}");
        assert!(text.contains("hello"), "{text}");
    }

    #[test]
    fn setting_a_key_preserves_the_rest_of_the_document() {
        let (_, _, mut document) = parse(
            r#"
            # a comment the user wrote
            [ui]
            theme = "light"
            future_option = 7
            "#,
        );
        document.set("ui", "theme", toml::Value::String("dark".to_owned()));
        let text = document.to_toml().unwrap();
        assert!(text.contains("future_option"));
        assert!(text.contains("dark"));
        assert!(!text.contains("light"));
    }

    #[test]
    fn syntax_errors_are_fatal_and_precise() {
        let dir = temp_home();
        let path = dir.write("config.toml", "[ui\ntheme = ");
        let error = load(&path).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("not valid TOML"), "{message}");
        assert!(message.contains("config.toml"), "{message}");
    }

    #[test]
    fn a_configured_model_is_read() {
        let (config, warnings, _) = parse(
            r#"
            [llm.active]
            provider = "deepseek"
            model = "deepseek-chat"
            temperature = 0.2
            reasoning = { enabled = true, effort = "medium" }
            "#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let active = config.llm.active.expect("a selection should be read");
        assert_eq!(active.provider, "deepseek");
        assert_eq!(active.model, "deepseek-chat");
        assert!(active.reasoning.is_some());
    }

    #[test]
    fn an_incomplete_model_is_rejected_with_a_warning() {
        let (config, warnings, _) = parse(
            r#"
            [llm.active]
            provider = ""
            model = "deepseek-chat"
            "#,
        );
        assert!(config.llm.active.is_none());
        assert!(
            warnings.iter().any(|w| w.contains("incomplete")),
            "{warnings:?}"
        );
    }

    #[test]
    fn no_model_is_configured_by_default() {
        // DEC-6: there is no default provider or model.
        let (config, _, _) = parse("");
        assert!(config.llm.active.is_none());
        assert!(config.llm.presets.is_empty());
    }

    #[test]
    fn presets_are_read() {
        let (config, warnings, _) = parse(
            r#"
            [llm.presets.cheap]
            provider = "deepseek"
            model = "deepseek-chat"
            "#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(config.llm.presets.contains_key("cheap"));
    }

    #[test]
    fn unsupported_workspace_mode_falls_back() {
        let (config, warnings, _) = parse(
            r#"
            [workspace]
            mode = "checkout"
            "#,
        );
        assert_eq!(config.workspace.mode, "worktree");
        assert!(
            warnings.iter().any(|w| w.contains("not supported")),
            "{warnings:?}"
        );
    }

    #[test]
    fn invalid_log_level_falls_back() {
        let (config, warnings, _) = parse(
            r#"
            [log]
            level = "chatty"
            "#,
        );
        assert_eq!(config.log.level, "info");
        assert!(
            warnings.iter().any(|w| w.contains("not a level")),
            "{warnings:?}"
        );
    }
}
