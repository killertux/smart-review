//! Turns command line arguments into everything the application needs.
//!
//! This is the only place that decides precedence between the CLI flags, the
//! configuration file, and the persisted state, so the rules are stated once
//! (FR-8.1, FR-8.2, FR-8.4).

use std::path::PathBuf;

use crate::adapters::clock::SystemClock;
use crate::adapters::fs::{TomlConfigStore, TomlStateStore};
use crate::cli::Cli;
use crate::config::{Config, ConfigDocument};
use crate::doctor::Context;
use crate::error::Result;
use crate::paths::Home;
use crate::ports::{ConfigStore, StateStore};
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
    /// The state store in use.
    pub state_store: TomlStateStore,
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
            repo: cli.repo.clone(),
            pr: cli.pr,
            path: cli.path.clone(),
            remote: cli.remote.clone(),
            clock: SystemClock,
            state_store,
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
