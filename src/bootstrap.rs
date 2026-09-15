//! Turns command line arguments into everything the application needs.
//!
//! This is the only place that decides precedence between the CLI flags, the
//! configuration file, and the persisted state, so the rules are stated once
//! (FR-8.1, FR-8.2, FR-8.4).

use std::path::PathBuf;

use std::sync::Arc;

use crate::adapters::cache::DiskCache;
use crate::adapters::catalog::ModelsDevCatalog;
use crate::adapters::clock::SystemClock;
use crate::adapters::credentials::{FileSecrets, RealEnv};
use crate::adapters::fs::{TomlConfigStore, TomlStateStore};
use crate::adapters::gh::GhForgeFactory;
use crate::adapters::gh::probe::GhCliProbe;
use crate::adapters::git::GitCli;
use crate::adapters::http::ReqwestFetcher;
use crate::adapters::llm::LlmCrate;
use crate::adapters::process::ProcessRunner;
use crate::cli::Cli;
use crate::config::{Config, ConfigDocument};
use crate::doctor::Context;
use crate::error::Result;
use crate::paths::Home;
use crate::ports::{
    CacheStore, Clock, ConfigStore, DraftStorePort, ForgeFactory, ForgeProbe, LlmPort,
    ModelCatalogPort, SecretStore, StateStore, WorkspacePort,
};
use crate::state::AppState;
use crate::tui::keymap::{self, Keymap};
use crate::tui::theme::{self, Theme};

/// Everything the UI needs to start.
#[derive(Debug)]
pub struct Startup {
    /// The application's private directory.
    pub home: Home,
    /// Effective settings.
    pub config: Config,
    /// The parsed config file, kept so unknown keys survive a later write.
    pub document: ConfigDocument,
    /// Where the config file lives.
    pub config_path: PathBuf,
    /// Whether a config file was actually read.
    pub config_exists: bool,
    /// `file` or `built-in defaults`, for display.
    pub config_source: &'static str,
    /// The keybinding engine.
    pub keymap: Keymap,
    /// The active theme.
    pub theme: Theme,
    /// Where the theme came from, for `:doctor`.
    pub theme_source: String,
    /// The name the theme was requested by (a built-in name or a file stem),
    /// which is what the picker marks. It can differ from `theme.name()`.
    pub theme_request: String,
    /// Persisted state.
    pub state: AppState,
    /// Non-fatal problems to report once the UI is up.
    pub warnings: Vec<String>,
    /// `--repo`, if given.
    pub repo: Option<String>,
    /// `--pr`, if given.
    pub pr: Option<u64>,
    /// `--path`, if given.
    pub path: Option<PathBuf>,
    /// `--remote`, if given.
    pub remote: Option<String>,
    /// Injected time source.
    pub clock: SystemClock,
    /// The checkout the app is running in (ARCH-2).
    pub workspace: Arc<dyn WorkspacePort>,
    /// The forge probe, which needs no repository (FR-1.1).
    pub probe: Arc<dyn ForgeProbe>,
    /// Builds the forge once detection has resolved a repository.
    pub forge_factory: Arc<dyn ForgeFactory>,
    /// Where answers are cached (FR-2.3).
    pub cache: Arc<dyn CacheStore>,
    /// The state store in use.
    pub state_store: TomlStateStore,
    /// Provider keys (FR-4.5).
    pub secret_store: Arc<dyn SecretStore>,
    /// The workspace port, which the interface also uses to list worktrees (FR-3.1).
    pub workspace_port: Arc<dyn WorkspacePort>,
    /// The model catalog (FR-4.7).
    pub catalog: Arc<dyn ModelCatalogPort>,
    /// The LLM client (FR-4.4).
    pub llm: Arc<dyn LlmPort>,
    /// Where analyses and their review-plan overrides are kept (FR-4.3).
    pub analysis: Arc<dyn crate::ports::AnalysisCachePort>,
    /// Where chat sessions are kept (FR-5.1). Disposable, like everything else under
    /// `cache/`, but written carefully: the conversation is the user's own words.
    pub chat: Arc<dyn crate::ports::ChatStorePort>,
    /// Where review drafts are kept (FR-6.1). Deliberately *not* under `cache/`: a
    /// draft cannot be fetched again.
    pub drafts: Arc<dyn DraftStorePort>,
    /// Whether mutating calls are recorded rather than run (FR-6.5).
    pub dry_run: bool,
    /// The calls a dry run recorded, which the loop writes out (FR-6.5).
    pub dry_run_ledger: crate::adapters::process::DryRunLedger,
}

/// The adapters a startup builds.
///
/// One struct rather than a dozen locals, because the composition root is the only place
/// that names an adapter (ARCH-1) and a list of them is easier to audit for that than a
/// stretch of `let`s in the middle of a long function.
struct Ports {
    /// Git, for detection and for the worktrees (FR-3.1).
    workspace: Arc<dyn WorkspacePort>,
    /// The same git, as the interface's port for listing worktrees (FR-3.1).
    workspace_port: Arc<dyn WorkspacePort>,
    /// `gh`, for detection (FR-1.1).
    probe: Arc<dyn ForgeProbe>,
    /// `gh`, for one repository's requests (FR-2.1).
    forge_factory: Arc<dyn ForgeFactory>,
    /// The answer cache (FR-2.3).
    cache: Arc<dyn CacheStore>,
    /// Provider keys (FR-4.5).
    secret_store: Arc<dyn SecretStore>,
    /// The model catalog (FR-4.7).
    catalog: Arc<dyn ModelCatalogPort>,
    /// The provider (FR-4.4).
    llm: Arc<dyn LlmPort>,
    /// Where analyses live (FR-4.3).
    analysis: Arc<dyn crate::ports::AnalysisCachePort>,
    /// Where conversations live (FR-5.1).
    chat: Arc<dyn crate::ports::ChatStorePort>,
    drafts: Arc<dyn DraftStorePort>,
    dry_run_ledger: crate::adapters::process::DryRunLedger,
}

impl Ports {
    /// Builds every adapter, given the one directory the caller resolved.
    ///
    /// The worktree root is wired in here and only here: everything that creates a
    /// worktree writes inside the app's own directory (FR-3.1, DEC-1).
    fn build(
        home: &Home,
        config: &crate::config::Config,
        path: Option<std::path::PathBuf>,
        dry_run: bool,
    ) -> Self {
        let workspace_root = home.worktrees();
        // The dry-run gate is built once and cloned into every adapter: one gate, one
        // promise, and one list of calls to hand to the user (FR-6.5).
        let ledger =
            crate::adapters::process::DryRunLedger::in_directory(home.exports().join("dry-run"));
        let runner = || {
            let runner = ProcessRunner::new();
            if dry_run {
                runner.with_dry_run(ledger.clone())
            } else {
                runner
            }
        };
        let workspace: Arc<dyn WorkspacePort> = Arc::new(match path {
            Some(path) => GitCli::new()
                .with_runner(runner())
                .in_dir(path)
                .with_worktrees(workspace_root),
            None => GitCli::new()
                .with_runner(runner())
                .with_worktrees(workspace_root),
        });
        let workspace_port = Arc::clone(&workspace);
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let cache: Arc<dyn CacheStore> = Arc::new(DiskCache::new(home.cache()));
        Self {
            probe: Arc::new(GhCliProbe::new(config.forge.gh_path.clone())),
            forge_factory: Arc::new(
                GhForgeFactory::new(config.forge.gh_path.clone()).with_runner(runner()),
            ),
            secret_store: Arc::new(FileSecrets::new(home.credentials(), Arc::new(RealEnv))),
            catalog: Arc::new(ModelsDevCatalog::new(
                config.catalog.url.clone(),
                home.catalog_cache(),
                u64::from(config.catalog.ttl_hours) * 3600,
                Arc::new(ReqwestFetcher::new()),
                Arc::clone(&clock),
            )),
            llm: Arc::new(LlmCrate::new()),
            analysis: Arc::new(crate::adapters::analysis_cache::DiskAnalysisCache::new(
                home.analysis_cache(),
            )),
            chat: Arc::new(crate::adapters::chat_store::FileChatStore::new(
                home.cache(),
            )),
            drafts: Arc::new(crate::adapters::draft_store::FileDraftStore::new(
                home.root(),
            )),
            dry_run_ledger: ledger,
            workspace,
            workspace_port,
            cache,
        }
    }
}

/// Resolves which repository to read: the flag wins, then the environment.
///
/// A blank value counts as unset, because `SMART_REVIEW_REPO=` in a shell script is
/// almost always a variable that was never given a value.
#[must_use]
pub fn resolve_repo(flag: Option<String>, environment: Option<String>) -> Option<String> {
    flag.or_else(|| environment.filter(|value| !value.trim().is_empty()))
}

impl Startup {
    /// Everything the doctor needs, owned so it can be handed to a background
    /// thread while the interface keeps running (FR-9.3).
    #[must_use]
    pub fn doctor_context(&self) -> Context {
        Context {
            home: self.home.clone(),
            config: self.config.clone(),
            config_path: self.config_path.clone(),
            config_exists: self.config_exists,
            warnings: self.warnings.clone(),
            keymap: self.keymap.clone(),
            theme: self.theme.clone(),
            theme_source: self.theme_source.clone(),
            config_keys: self.document.key_count(),
            // Detection has not run yet when the startup is built.
            environment: None,
            environment_error: None,
        }
    }

    /// Resolves the home, reads configuration, and builds the theme and keymap.
    ///
    /// A missing or broken *theme* never stops startup: it falls back to the
    /// built-in dark theme with a warning (FR-8.6).
    ///
    /// # Errors
    ///
    /// Returns an error when the home directory cannot be created, the
    /// configuration cannot be read, or the keybinds file is malformed.
    pub fn load(cli: &Cli) -> Result<Self> {
        let home = Home::resolve(cli.home.as_deref())?;
        home.ensure()?;
        home.write_readme()?;

        // `SMART_REVIEW_REPO` is the environment spelling of `--repo` (FR-1.1):
        // useful in a shell that is not in a clone, and in a CI job.
        let repo = resolve_repo(cli.repo.clone(), std::env::var("SMART_REVIEW_REPO").ok());

        let config_path = cli.config.clone().unwrap_or_else(|| home.config());
        let config_store = TomlConfigStore::new(config_path.clone());
        let loaded = config_store.load()?;

        // Read the explicit theme before moving anything out of `loaded`.
        let theme_from_config = loaded
            .document
            .get("ui", "theme")
            .and_then(toml::Value::as_str)
            .map(str::to_owned);
        let default_theme = loaded.config.ui.theme.clone();
        let config_source = loaded.source();
        let mut warnings = loaded.warnings;

        // State is read before the theme because `:theme` remembers the last
        // choice there (FR-8.5).
        let state_store = TomlStateStore::new(home.state());
        let state = match state_store.load() {
            Ok(state) => state,
            Err(error) => {
                warnings.push(format!("state: {error}; starting from defaults"));
                AppState::default()
            }
        };

        // Theme precedence: --theme, then an explicit [ui].theme in the file,
        // then the last theme used, then the built-in default.
        let requested = cli
            .theme
            .clone()
            .or(theme_from_config)
            .or_else(|| state.theme.clone())
            .unwrap_or(default_theme);

        // Remember what was asked for: a theme file may declare its own display
        // name, and the picker has to mark the entry the user actually chose.
        let theme_request = if requested.is_empty() {
            "dark".to_owned()
        } else {
            requested.clone()
        };
        let (theme, theme_source) = match theme::load(&home, &requested, &mut warnings) {
            Ok(loaded_theme) => loaded_theme,
            Err(error) => {
                warnings.push(format!("theme: {error}; using the built-in dark theme"));
                (Theme::default(), "built-in fallback".to_owned())
            }
        };

        let keymap = keymap::load(&home, &loaded.config.ui, &mut warnings)?;

        let dry_run = cli.dry_run || loaded.config.forge.dry_run;
        let ports = Ports::build(&home, &loaded.config, cli.path.clone(), dry_run);

        Ok(Self {
            home,
            config: loaded.config,
            document: loaded.document,
            config_path: loaded.path,
            config_exists: loaded.exists,
            config_source,
            keymap,
            theme,
            theme_source,
            theme_request,
            state,
            warnings,
            repo,
            pr: cli.pr,
            path: cli.path.clone(),
            remote: cli.remote.clone(),
            clock: SystemClock,
            workspace: ports.workspace,
            probe: ports.probe,
            forge_factory: ports.forge_factory,
            cache: ports.cache,
            state_store,
            secret_store: ports.secret_store,
            workspace_port: ports.workspace_port,
            catalog: ports.catalog,
            llm: ports.llm,
            analysis: ports.analysis,
            chat: ports.chat,
            drafts: ports.drafts,
            dry_run,
            dry_run_ledger: ports.dry_run_ledger,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_home;

    fn cli_for(home: &std::path::Path) -> Cli {
        Cli {
            repo: None,
            pr: None,
            path: None,
            remote: None,
            config: None,
            theme: None,
            home: Some(home.to_path_buf()),
            log_level: None,
            check: false,
            dry_run: false,
        }
    }

    #[test]
    fn a_fresh_home_starts_with_defaults() {
        let dir = temp_home();
        let startup = Startup::load(&cli_for(dir.path())).unwrap();
        assert!(!startup.config_exists);
        assert_eq!(startup.config_source, "built-in defaults");
        assert_eq!(startup.theme.name(), "dark");
        assert!(startup.warnings.is_empty(), "{:?}", startup.warnings);
    }

    #[test]
    fn the_home_layout_is_created_on_startup() {
        let dir = temp_home();
        let startup = Startup::load(&cli_for(dir.path())).unwrap();
        assert!(startup.home.cache().is_dir());
        assert!(startup.home.logs().is_dir());
        assert!(startup.home.root().join("README.md").is_file());
    }

    #[test]
    fn an_explicit_file_theme_wins_over_the_state() {
        let dir = temp_home();
        dir.write("config.toml", "[ui]\ntheme = \"light\"\n");
        dir.write("state.toml", "theme = \"dark\"\n");

        let startup = Startup::load(&cli_for(dir.path())).unwrap();
        assert_eq!(startup.theme.name(), "light");
    }

    #[test]
    fn the_last_theme_wins_when_the_config_does_not_say() {
        let dir = temp_home();
        dir.write("state.toml", "theme = \"light\"\n");
        let startup = Startup::load(&cli_for(dir.path())).unwrap();
        assert_eq!(startup.theme.name(), "light");
    }

    #[test]
    fn the_cli_flag_beats_everything() {
        let dir = temp_home();
        dir.write("config.toml", "[ui]\ntheme = \"light\"\n");
        let mut cli = cli_for(dir.path());
        cli.theme = Some("dark".to_owned());
        let startup = Startup::load(&cli).unwrap();
        assert_eq!(startup.theme.name(), "dark");
    }

    #[test]
    fn a_missing_theme_falls_back_instead_of_failing() {
        let dir = temp_home();
        let mut cli = cli_for(dir.path());
        cli.theme = Some("dracula".to_owned());
        let startup = Startup::load(&cli).unwrap();
        assert_eq!(startup.theme.name(), "dark");
        assert_eq!(startup.theme_source, "built-in fallback");
        assert!(startup.warnings.iter().any(|w| w.contains("dracula")));
    }

    #[test]
    fn corrupt_state_does_not_stop_startup() {
        let dir = temp_home();
        dir.write("state.toml", "last_pr = \"nope\"\n");
        let startup = Startup::load(&cli_for(dir.path())).unwrap();
        assert!(startup.warnings.iter().any(|w| w.contains("state")));
    }

    #[test]
    fn config_warnings_are_carried_through() {
        let dir = temp_home();
        dir.write("config.toml", "[ui]\ntimeoutlen = \"soon\"\n");
        let startup = Startup::load(&cli_for(dir.path())).unwrap();
        assert!(startup.warnings.iter().any(|w| w.contains("timeoutlen")));
        assert_eq!(startup.config.ui.timeoutlen, 500);
    }

    #[test]
    fn the_flag_wins_over_the_environment_and_a_blank_value_is_unset() {
        // Tested as a function rather than by writing the variable: setting an
        // environment variable is `unsafe` in edition 2024 and would leak into every
        // test running in parallel, so the precedence is separated from the read.
        assert_eq!(
            resolve_repo(Some("acme/flag".to_owned()), Some("acme/env".to_owned())).as_deref(),
            Some("acme/flag")
        );
        assert_eq!(
            resolve_repo(None, Some("acme/env".to_owned())).as_deref(),
            Some("acme/env")
        );
        assert_eq!(resolve_repo(None, Some(String::new())), None);
        assert_eq!(resolve_repo(None, Some("   ".to_owned())), None);
        assert_eq!(resolve_repo(None, None), None);
    }

    #[test]
    fn cli_values_are_recorded() {
        let dir = temp_home();
        let mut cli = cli_for(dir.path());
        cli.repo = Some("acme/service".to_owned());
        cli.pr = Some(141);
        cli.remote = Some("upstream".to_owned());
        let startup = Startup::load(&cli).unwrap();
        assert_eq!(startup.repo.as_deref(), Some("acme/service"));
        assert_eq!(startup.pr, Some(141));
        assert_eq!(startup.remote.as_deref(), Some("upstream"));
    }
}
