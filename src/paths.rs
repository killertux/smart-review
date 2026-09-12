//! Where smart-review keeps everything it owns (FR-8.1).
//!
//! Nothing is ever written inside the user's repository: the application owns a
//! single root directory, `$SMART_REVIEW_HOME` or `~/.smart-review`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::Error;

/// The application's private directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// Environment variable that relocates the whole application state (FR-8.1).
    pub const ENV_VAR: &'static str = "SMART_REVIEW_HOME";

    /// Resolves the home directory: explicit flag, then `$SMART_REVIEW_HOME`,
    /// then `~/.smart-review`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Home`] when neither the flag, the environment variable,
    /// nor `$HOME` identifies a directory.
    pub fn resolve(explicit: Option<&Path>) -> Result<Self, Error> {
        Self::resolve_from(
            explicit,
            std::env::var_os(Self::ENV_VAR),
            std::env::var_os("HOME"),
        )
    }

    /// Resolution with the environment passed in, so precedence is testable
    /// without mutating process-global state.
    fn resolve_from(
        explicit: Option<&Path>,
        env_var: Option<OsString>,
        os_home: Option<OsString>,
    ) -> Result<Self, Error> {
        if let Some(path) = explicit {
            return Ok(Self {
                root: path.to_path_buf(),
            });
        }

        if let Some(value) = env_var
            && !value.is_empty()
        {
            return Ok(Self {
                root: PathBuf::from(value),
            });
        }

        let os_home = os_home.filter(|value| !value.is_empty()).ok_or_else(|| {
            Error::Home(format!(
                "neither ${} nor $HOME is set; pass --home or set ${}",
                Self::ENV_VAR,
                Self::ENV_VAR
            ))
        })?;

        Ok(Self {
            root: PathBuf::from(os_home).join(".smart-review"),
        })
    }

    /// The root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `config.toml` (FR-8.2).
    #[must_use]
    pub fn config(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// `keybinds.toml` (FR-8.3).
    #[must_use]
    pub fn keybinds(&self) -> PathBuf {
        self.root.join("keybinds.toml")
    }

    /// `credentials.toml` (NFR-3.1, §7.4). Created by the TUI key-entry flow.
    #[must_use]
    pub fn credentials(&self) -> PathBuf {
        self.root.join("credentials.toml")
    }

    /// `state.toml` (FR-8.5).
    #[must_use]
    pub fn state(&self) -> PathBuf {
        self.root.join("state.toml")
    }

    /// Directory holding user themes (FR-8.4).
    #[must_use]
    pub fn themes(&self) -> PathBuf {
        self.root.join("themes")
    }

    /// Directory holding disposable caches (FR-8.5).
    #[must_use]
    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// Cached models.dev catalog (FR-4.7).
    #[must_use]
    pub fn catalog_cache(&self) -> PathBuf {
        self.cache().join("models.json")
    }

    /// Cached analyses, one directory per pull request (FR-4.3).
    ///
    /// Beside the forge cache rather than inside it: an analysis is keyed by the
    /// provider and the thinking settings as much as by the commit, so it does not
    /// belong under the same layout as a pull request's metadata.
    #[must_use]
    pub fn analysis_cache(&self) -> PathBuf {
        self.cache().join("analysis")
    }

    /// Directory holding managed PR worktrees (FR-3.1).
    #[must_use]
    pub fn worktrees(&self) -> PathBuf {
        self.root.join("worktrees")
    }

    /// Directory holding logs (FR-9.2).
    #[must_use]
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// The active log file.
    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.logs().join("smart-review.log")
    }

    /// Creates the directory layout if needed and returns the paths it created.
    ///
    /// Directories are created with mode `0700` so cached PR data and API keys are
    /// not readable by other users on the machine (NFR-3.1).
    ///
    /// # Errors
    ///
    /// Returns an error when a directory cannot be created or its permissions
    /// cannot be restricted.
    pub fn ensure(&self) -> Result<Vec<PathBuf>, Error> {
        let mut created = Vec::new();
        for directory in [
            self.root.clone(),
            self.themes(),
            self.cache(),
            self.worktrees(),
            self.logs(),
        ] {
            if directory.exists() {
                continue;
            }
            std::fs::create_dir_all(&directory)
                .map_err(|source| Error::io("create directory", &directory, source))?;
            set_private_mode(&directory)?;
            created.push(directory);
        }
        Ok(created)
    }

    /// Writes the explanatory `README.md` on first run (FR-8.1). Never overwrites.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be written.
    pub fn write_readme(&self) -> Result<bool, Error> {
        let path = self.root.join("README.md");
        if path.exists() {
            return Ok(false);
        }
        let body = format!(
            "# smart-review state directory\n\n\
             Everything smart-review owns lives here. Nothing is written into your\n\
             repositories.\n\n\
             - `config.toml` — application, review, workspace and LLM settings.\n\
             - `keybinds.toml` — keybinding overrides; the defaults are built in.\n\
             - `credentials.toml` — API keys entered in the TUI (mode 0600).\n\
             - `state.toml` — window, theme and recently opened state.\n\
             - `themes/` — your own themes; each file inherits from `base`.\n\
             - `cache/` — disposable: PR lists, details, analyses and chats.\n\
             - `worktrees/` — per-pull-request checkouts owned by the app.\n\
             - `logs/` — rotated logs; never contains secrets.\n\n\
             Root: {}\n",
            self.root.display()
        );
        std::fs::write(&path, body).map_err(|source| Error::io("write", &path, source))?;
        Ok(true)
    }
}

/// Shortens a path so it fits in `max_chars` columns without wrapping.
///
/// The user's home directory becomes `~`, and when the result is still too long
/// only the trailing components are kept, prefixed with `…`. Keeping the tail is
/// deliberate: the end of a path is what identifies it, and a pane that wraps a
/// path destroys its own layout (FR-7.8).
pub fn shorten_for_display(path: &Path, max_chars: usize) -> String {
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    shorten_with(path, user_home.as_deref(), max_chars)
}

/// [`shorten_for_display`] with the user's home passed in, so it is testable
/// without touching the environment.
fn shorten_with(path: &Path, user_home: Option<&Path>, max_chars: usize) -> String {
    let display = match user_home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    };

    if display.chars().count() <= max_chars {
        return display;
    }

    let parts: Vec<&str> = display.split('/').filter(|part| !part.is_empty()).collect();
    let mut kept: Vec<&str> = Vec::new();
    for part in parts.iter().rev() {
        kept.insert(0, part);
        if format!("…/{}", kept.join("/")).chars().count() > max_chars {
            kept.remove(0);
            break;
        }
    }

    if kept.is_empty() {
        // Not even one component fits: keep the end of the string, which is the
        // most identifying part.
        let characters: Vec<char> = display.chars().collect();
        return characters[characters.len().saturating_sub(max_chars)..]
            .iter()
            .collect();
    }

    format!("…/{}", kept.join("/"))
}

#[cfg(unix)]
fn set_private_mode(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|source| Error::io("restrict permissions on", path, source))
}

#[cfg(not(unix))]
fn set_private_mode(_path: &Path) -> Result<(), Error> {
    // Windows has no POSIX mode bits; ACL handling is out of scope until Windows
    // becomes a supported target (DEC-12).
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_home;

    #[test]
    fn explicit_path_wins_over_environment() {
        let home = Home::resolve_from(
            Some(Path::new("/tmp/explicit")),
            Some(OsString::from("/tmp/from-env")),
            Some(OsString::from("/home/user")),
        )
        .unwrap();
        assert_eq!(home.root(), Path::new("/tmp/explicit"));
    }

    #[test]
    fn environment_variable_is_honoured() {
        let home = Home::resolve_from(
            None,
            Some(OsString::from("/tmp/from-env")),
            Some(OsString::from("/home/user")),
        )
        .unwrap();
        assert_eq!(home.root(), Path::new("/tmp/from-env"));
    }

    #[test]
    fn falls_back_to_dot_smart_review_in_home() {
        let home = Home::resolve_from(None, None, Some(OsString::from("/home/user"))).unwrap();
        assert_eq!(home.root(), Path::new("/home/user/.smart-review"));
    }

    #[test]
    fn empty_environment_variable_is_ignored() {
        let home = Home::resolve_from(
            None,
            Some(OsString::new()),
            Some(OsString::from("/home/user")),
        )
        .unwrap();
        assert_eq!(home.root(), Path::new("/home/user/.smart-review"));
    }

    #[test]
    fn missing_home_is_an_actionable_error() {
        let error = Home::resolve_from(None, None, None).unwrap_err();
        assert!(error.to_string().contains(Home::ENV_VAR));
        assert!(error.to_string().contains("--home"));
    }

    #[test]
    fn ensure_creates_the_documented_layout() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        home.ensure().unwrap();

        for path in [home.themes(), home.cache(), home.worktrees(), home.logs()] {
            assert!(path.is_dir(), "{} should exist", path.display());
        }

        // The second call is a no-op and reports nothing created.
        assert!(home.ensure().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_restricts_permissions_to_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        home.ensure().unwrap();

        // Directories the app creates are private. An existing root that the
        // user pointed us at is only reported on, never chmodded behind their
        // back (it could be a shared directory).
        for created in [home.themes(), home.cache(), home.worktrees(), home.logs()] {
            let mode = std::fs::metadata(&created).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o700,
                "{} should be private",
                created.display()
            );
        }
    }

    #[test]
    fn readme_is_written_once() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        home.ensure().unwrap();
        assert!(home.write_readme().unwrap());
        assert!(!home.write_readme().unwrap());
    }

    #[test]
    fn short_paths_are_left_alone() {
        let home = Path::new("/home/dev");
        assert_eq!(
            shorten_with(Path::new("/home/dev/.smart-review"), Some(home), 40),
            "~/.smart-review"
        );
    }

    #[test]
    fn long_paths_keep_their_tail() {
        let home = Path::new("/home/dev");
        let path = Path::new("/home/dev/work/acme/service/target/deep/nested/config.toml");
        let shortened = shorten_with(path, Some(home), 30);
        assert!(shortened.starts_with("…/"), "{shortened}");
        assert!(shortened.ends_with("config.toml"), "{shortened}");
        assert!(shortened.chars().count() <= 30, "{shortened}");
    }

    #[test]
    fn shortening_is_stable_regardless_of_where_the_repo_lives() {
        // Two different machines, same trailing components: the pane must render
        // the same text, otherwise snapshots depend on the checkout location.
        let local = shorten_with(
            Path::new("/home/dev/Projects/smart-review/target/snapshot-home"),
            Some(Path::new("/home/dev")),
            32,
        );
        let runner = shorten_with(
            Path::new("/home/runner/work/smart-review/smart-review/target/snapshot-home"),
            Some(Path::new("/home/runner")),
            32,
        );
        assert_eq!(local, "…/target/snapshot-home");
        assert_eq!(local, runner);
    }

    #[test]
    fn a_single_very_long_component_is_truncated() {
        let shortened = shorten_with(Path::new("/a/bbbbbbbbbbbbbbbbbbbbbbbbbb"), None, 10);
        assert_eq!(shortened.chars().count(), 10);
        assert_eq!(shortened, "bbbbbbbbbb");
    }
}
