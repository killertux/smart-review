//! Persisted state and disposable caches (FR-8.5).
//!
//! `state.toml` holds only small, user-meaningful values. Everything under
//! `cache/` is disposable, so deleting it never loses a draft or a chat
//! (FR-8.5).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::adapters::fs::write_atomic;

/// Small values that survive a restart.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// Last theme chosen in the TUI.
    pub theme: Option<String>,
    /// Last repository reviewed, as `host/owner/name`.
    pub last_repo: Option<String>,
    /// Last pull request opened.
    pub last_pr: Option<u64>,
    /// Last focused pane, by name.
    pub focus: Option<String>,
}

/// Everything that can go wrong while reading or writing state.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("could not read state file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("state file {path} is not valid TOML: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("could not write state file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Reads `path`, returning defaults when the file does not exist yet.
///
/// # Errors
///
/// Returns [`StateError`] when the file exists but cannot be read or parsed.
pub fn load(path: &Path) -> Result<AppState, StateError> {
    if !path.exists() {
        return Ok(AppState::default());
    }
    let text = std::fs::read_to_string(path).map_err(|source| StateError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| StateError::Parse {
        path: path.to_path_buf(),
        message: source.to_string(),
    })
}

/// Writes `state` atomically, so a crash cannot leave a half-written file.
///
/// # Errors
///
/// Returns [`StateError`] when the state cannot be serialised or written.
pub fn save(path: &Path, state: &AppState) -> Result<(), StateError> {
    let text = toml::to_string_pretty(state).map_err(|source| StateError::Parse {
        path: path.to_path_buf(),
        message: source.to_string(),
    })?;
    write_atomic(path, &text).map_err(|source| StateError::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_home;

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = temp_home();
        let state = load(&dir.path().join("state.toml")).unwrap();
        assert_eq!(state, AppState::default());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = temp_home();
        let path = dir.path().join("state.toml");
        let state = AppState {
            theme: Some("light".to_owned()),
            last_repo: Some("github.com/acme/service".to_owned()),
            last_pr: Some(141),
            focus: Some("diff".to_owned()),
        };
        save(&path, &state).unwrap();
        assert_eq!(load(&path).unwrap(), state);
    }

    #[test]
    fn partial_files_keep_defaults_for_missing_keys() {
        let dir = temp_home();
        let path = dir.write("state.toml", "last_pr = 7\n");
        let state = load(&path).unwrap();
        assert_eq!(state.last_pr, Some(7));
        assert!(state.theme.is_none());
    }

    #[test]
    fn corrupt_state_is_an_error_not_a_panic() {
        let dir = temp_home();
        let path = dir.write("state.toml", "last_pr = \"not a number\"");
        assert!(load(&path).is_err());
    }

    #[test]
    fn saving_creates_parent_directories() {
        let dir = temp_home();
        let path = dir.path().join("nested").join("state.toml");
        save(&path, &AppState::default()).unwrap();
        assert!(path.exists());
    }
}
