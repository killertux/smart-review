//! Top level error type and its exit-code mapping (ARCH-7, FR-9.1, FR-9.3).

use std::path::{Path, PathBuf};

use crate::config::ConfigError;
use crate::state::StateError;
use crate::tui::keymap::KeymapError;
use crate::tui::theme::ThemeError;

/// Convenience alias used across the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Whether a forge mutation error proves that GitHub did not apply the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeDelivery {
    /// GitHub returned a definite refusal, so the snapshot can be edited and retried.
    Refused,
    /// The request may have reached GitHub, so retrying blindly is unsafe.
    Unknown,
}

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
    Forge {
        command: String,
        message: String,
        delivery: ForgeDelivery,
    },

    #[error("{0}")]
    Cache(String),
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
            delivery: ForgeDelivery::Refused,
        }
    }

    /// Builds a forge error after a mutation's delivery could not be established.
    pub fn forge_outcome_unknown(command: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Forge {
            command: command.into(),
            message: message.into(),
            delivery: ForgeDelivery::Unknown,
        }
    }

    /// Whether this forge failure may have applied a remote mutation.
    #[must_use]
    pub const fn forge_delivery(&self) -> Option<ForgeDelivery> {
        match self {
            Self::Forge { delivery, .. } => Some(*delivery),
            _ => None,
        }
    }

    /// Builds an [`Error::Cache`] for a payload that cannot be trusted.
    pub fn cache(message: impl Into<String>) -> Self {
        Self::Cache(message.into())
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
