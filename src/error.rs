//! Top level error type and its exit-code mapping (ARCH-7, FR-9.1, FR-9.3).

use std::path::{Path, PathBuf};

use crate::config::ConfigError;
use crate::state::StateError;
use crate::tui::keymap::KeymapError;
use crate::tui::theme::ThemeError;

/// Convenience alias used across the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Anything that can stop the application.
///
/// Each variant carries enough context for the user to act on it (DEV-7): the
/// path, the command, or the setting that caused the failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not determine the smart-review home directory: {0}")]
    Home(String),

    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    State(#[from] StateError),

    #[error(transparent)]
    Theme(#[from] ThemeError),

    #[error(transparent)]
    Keymap(#[from] KeymapError),

    #[error("could not {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),

    #[error("{message}")]
    Forge { command: String, message: String },
}

impl Error {
    /// Builds an [`Error::Forge`] naming the command that failed.
    ///
    /// The command is kept separately from the message because FR-9.1 asks for a
    /// copyable command, and the message for something a person can read.
    pub fn forge(command: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Forge {
            command: command.into(),
            message: message.into(),
        }
    }

    /// The command that produced this error, when there was one.
    #[must_use]
    pub fn command(&self) -> Option<&str> {
        match self {
            Self::Forge { command, .. } => Some(command),
            _ => None,
        }
    }

    /// Builds an [`Error::Io`] with a human-readable action verb.
    pub fn io(action: &'static str, path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            action,
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}
