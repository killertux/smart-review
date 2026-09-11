//! The credential port (FR-4.5, FR-4.7, §7.4, NFR-3.1).
//!
//! Two things this module is careful about, because both are easy to get wrong and
//! expensive to get wrong:
//!
//! - a key never renders itself. [`ApiKey`] has a hand-written `Debug`, no
//!   `Display`, and no `Serialize`, so the only way to read it is [`ApiKey::expose`]
//!   — which is the one place to look when auditing what can see a key;
//! - the *source* of a key is always known. A provider's documented environment
//!   variable shadows the stored file (§7.4), and the UI must be able to say which
//!   one is in use rather than leaving precedence a mystery.

use std::fmt;

/// Where a key came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// The provider's documented environment variable, which wins (§7.4).
    Environment(String),
    /// `credentials.toml`, mode 0600.
    File,
}

impl KeySource {
    /// A short label for the status line and `:model show`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Environment(name) => format!("env {name}"),
            Self::File => "credentials.toml".to_owned(),
        }
    }

    /// Whether this source shadows the stored file.
    #[must_use]
    pub fn is_environment(&self) -> bool {
        matches!(self, Self::Environment(_))
    }
}

/// A provider's key, carried with the place it came from.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey {
    secret: String,
    source: KeySource,
}

impl ApiKey {
    /// Wraps a key.
    #[must_use]
    pub fn new(secret: impl Into<String>, source: KeySource) -> Self {
        Self {
            secret: secret.into(),
            source,
        }
    }

    /// The key itself.
    ///
    /// Named `expose` rather than `as_str` on purpose: every call site is a place
    /// a key could reach a log line, and they should be countable.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.secret
    }

    /// Where it came from.
    #[must_use]
    pub fn source(&self) -> &KeySource {
        &self.source
    }

    /// Whether the key is empty, which is never usable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.secret.trim().is_empty()
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Deliberately not the secret: an `ApiKey` must be safe to print in a
        // trace, a snapshot or a panic message (NFR-3.1).
        f.debug_struct("ApiKey")
            .field("secret", &"<redacted>")
            .field("len", &self.secret.len())
            .field("source", &self.source)
            .finish()
    }
}

/// What is known about one provider's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStatus {
    /// The catalog provider id.
    pub provider: String,
    /// Where the key in use comes from, or `None` when there is none.
    pub source: Option<KeySource>,
}

impl KeyStatus {
    /// Whether a key is available at all.
    #[must_use]
    pub fn is_present(&self) -> bool {
        self.source.is_some()
    }
}

/// Why a key operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// The credentials file exists with the wrong permissions.
    #[error("{path} is mode {mode:o}, not 0600: run `chmod 600 {path}` and try again")]
    InsecureMode {
        /// The file's path.
        path: String,
        /// The mode it actually has.
        mode: u32,
    },

    /// The file could not be read or written.
    #[error("the credentials file could not be {action}: {reason}")]
    Io {
        /// What was being attempted, e.g. `read` or `write`.
        action: &'static str,
        /// The underlying message.
        reason: String,
    },

    /// The file is not valid TOML, or not shaped like credentials (§7.4).
    #[error("the credentials file is not usable: {0}")]
    Malformed(String),
}

/// Reads and writes the user's provider keys.
pub trait SecretStore: fmt::Debug + Send + Sync {
    /// The key to use for a provider, preferring the environment (§7.4).
    ///
    /// # Errors
    ///
    /// Returns [`SecretError`] when the file exists but cannot be used. An absent
    /// key is `Ok(None)`, not an error: it is a state the picker asks about.
    fn get(&self, provider: &str, env_var: Option<&str>) -> Result<Option<ApiKey>, SecretError>;

    /// Stores a key, creating the file mode 0600 and replacing atomically.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError`] when the file cannot be written.
    fn set(&self, provider: &str, key: &str) -> Result<(), SecretError>;

    /// Removes a provider's stored key (`:key clear`).
    ///
    /// # Errors
    ///
    /// Returns [`SecretError`] when the file cannot be written.
    fn remove(&self, provider: &str) -> Result<(), SecretError>;

    /// What is stored, for `:key` and `:model show`.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError`] when the file exists but cannot be used.
    fn status(&self) -> Result<Vec<KeyStatus>, SecretError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_never_prints_itself() {
        let key = ApiKey::new("sk-secret-value", KeySource::File);
        let debug = format!("{key:?}");
        assert!(!debug.contains("sk-secret-value"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        // The source is part of the trace on purpose: which file or variable a key
        // came from is exactly what has to be knowable when something is wrong.
        assert!(debug.contains("File"), "{debug}");
        // The only way to read it is by name.
        assert_eq!(key.expose(), "sk-secret-value");
    }

    #[test]
    fn sources_report_themselves() {
        assert_eq!(
            KeySource::Environment("DEEPSEEK_API_KEY".to_owned()).label(),
            "env DEEPSEEK_API_KEY"
        );
        assert!(KeySource::Environment("X".to_owned()).is_environment());
        assert_eq!(KeySource::File.label(), "credentials.toml");
        assert!(!KeySource::File.is_environment());
    }

    #[test]
    fn a_blank_key_is_not_a_key() {
        assert!(ApiKey::new("   ", KeySource::File).is_empty());
        assert!(!ApiKey::new("sk-1", KeySource::File).is_empty());
    }

    #[test]
    fn status_says_whether_a_key_exists() {
        let missing = KeyStatus {
            provider: "openai".to_owned(),
            source: None,
        };
        assert!(!missing.is_present());
        let from_env = KeyStatus {
            provider: "openai".to_owned(),
            source: Some(KeySource::Environment("OPENAI_API_KEY".to_owned())),
        };
        assert!(from_env.is_present());
    }

    #[test]
    fn an_insecure_file_says_how_to_fix_it() {
        let error = SecretError::InsecureMode {
            path: "/home/u/.smart-review/credentials.toml".to_owned(),
            mode: 0o644,
        };
        let message = error.to_string();
        assert!(message.contains("chmod 600"), "{message}");
        assert!(message.contains("644"), "{message}");
    }
}
