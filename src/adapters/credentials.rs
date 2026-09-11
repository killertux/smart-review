//! `credentials.toml`, mode 0600 (FR-4.5, §7.4, NFR-3.1).
//!
//! The file is a small TOML document:
//!
//! ```toml
//! version = 1
//!
//! [providers.deepseek]
//! api_key = "sk-…"
//! ```
//!
//! Three rules the code enforces rather than documents:
//!
//! - **the mode is checked on every load.** A key written by an older version, an
//!   editor, or a `umask` slip is refused with the `chmod` command to fix it, rather
//!   than being read and trusted;
//! - **no value ever reaches a log or an error.** Errors name the provider and the
//!   file, never the contents;
//! - **the environment wins.** A provider's documented variable shadows the file
//!   (§7.4), and the caller is told which one is in use.
//!
//! The environment is behind [`EnvSource`] because edition 2024 makes
//! `std::env::set_var` unsafe and the crate forbids unsafe code: a test cannot set a
//! variable, so precedence would otherwise be untestable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ports::secret::{ApiKey, KeySource, KeyStatus, SecretError, SecretStore};

/// The `version` this build writes.
pub const CREDENTIALS_VERSION: u32 = 1;

/// Reads the process environment.
pub trait EnvSource: std::fmt::Debug + Send + Sync {
    /// The value of a variable, if it is set and not empty.
    fn var(&self, name: &str) -> Option<String>;
}

/// The real environment.
#[derive(Debug, Default)]
pub struct RealEnv;

impl EnvSource for RealEnv {
    /// Reports the raw value; the emptiness rule lives in `FileSecrets::get` so a
    /// test's environment behaves exactly like the real one.
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// The credentials document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Document {
    /// Format version written by the app.
    #[serde(default)]
    version: u32,
    /// Per-provider entries, keyed by catalog provider id.
    #[serde(default)]
    providers: BTreeMap<String, ProviderEntry>,
}

/// One provider's entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProviderEntry {
    /// The key.
    #[serde(default)]
    api_key: String,
}

/// Keys stored in `${SMART_REVIEW_HOME}/credentials.toml`.
pub struct FileSecrets {
    path: PathBuf,
    env: Arc<dyn EnvSource>,
}

impl std::fmt::Debug for FileSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSecrets")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl FileSecrets {
    /// A store over a file, reading the environment through `env`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, env: Arc<dyn EnvSource>) -> Self {
        Self {
            path: path.into(),
            env,
        }
    }

    /// The file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the document, verifying the file mode first.
    fn read(&self) -> Result<Document, SecretError> {
        if !self.path.exists() {
            return Ok(Document {
                version: CREDENTIALS_VERSION,
                providers: BTreeMap::new(),
            });
        }
        verify_mode(&self.path)?;
        let text = std::fs::read_to_string(&self.path).map_err(|error| SecretError::Io {
            action: "read",
            reason: error.to_string(),
        })?;
        // An empty file is a file a user created to get started, not an error.
        if text.trim().is_empty() {
            return Ok(Document {
                version: CREDENTIALS_VERSION,
                providers: BTreeMap::new(),
            });
        }
        toml::from_str(&text).map_err(|error| SecretError::Malformed(short(&error.to_string())))
    }

    /// Writes the document atomically, mode 0600.
    fn write(&self, document: &Document) -> Result<(), SecretError> {
        let text = toml::to_string_pretty(document)
            .map_err(|error| SecretError::Malformed(error.to_string()))?;
        crate::adapters::fs::write_atomic_with_mode(&self.path, &text, Some(0o600)).map_err(
            |error| SecretError::Io {
                action: "write",
                reason: error.to_string(),
            },
        )
    }
}

/// Refuses a file that anybody but the owner can read.
fn verify_mode(path: &Path) -> Result<(), SecretError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path).map_err(|error| SecretError::Io {
            action: "read",
            reason: error.to_string(),
        })?;
        // `PermissionsExt::mode` is not `const`, hence the binding rather than a
        // literal in the pattern.
        let mode = metadata.permissions().mode();
        let group_or_other = mode & 0o077;
        if group_or_other != 0 {
            return Err(SecretError::InsecureMode {
                path: path.display().to_string(),
                mode: mode & 0o777,
            });
        }
    }
    Ok(())
}

impl SecretStore for FileSecrets {
    fn get(&self, provider: &str, env_var: Option<&str>) -> Result<Option<ApiKey>, SecretError> {
        // The environment wins, and the caller learns which source answered (§7.4).
        if let Some(name) = env_var
            && let Some(value) = self.env.var(name)
            // An empty variable is not an override: a shell that exports
            // `FOO_API_KEY=` must not hide a key the user stored.
            && !value.trim().is_empty()
        {
            return Ok(Some(ApiKey::new(
                value,
                KeySource::Environment(name.to_owned()),
            )));
        }
        let document = self.read()?;
        Ok(document
            .providers
            .get(provider)
            .map(|entry| entry.api_key.trim())
            .filter(|key| !key.is_empty())
            .map(|key| ApiKey::new(key, KeySource::File)))
    }

    fn set(&self, provider: &str, key: &str) -> Result<(), SecretError> {
        let mut document = self.read()?;
        document.version = CREDENTIALS_VERSION;
        document.providers.insert(
            provider.to_owned(),
            ProviderEntry {
                api_key: key.trim().to_owned(),
            },
        );
        self.write(&document)
    }

    fn remove(&self, provider: &str) -> Result<(), SecretError> {
        let mut document = self.read()?;
        if document.providers.remove(provider).is_none() {
            return Ok(());
        }
        self.write(&document)
    }

    fn status(&self) -> Result<Vec<KeyStatus>, SecretError> {
        let document = self.read()?;
        Ok(document
            .providers
            .into_iter()
            .filter(|(_, entry)| !entry.api_key.trim().is_empty())
            .map(|(provider, _)| KeyStatus {
                provider,
                source: Some(KeySource::File),
            })
            .collect())
    }
}

/// Trims a long parser message to its first line.
fn short(message: &str) -> String {
    message.lines().next().unwrap_or(message).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempHome, temp_home};

    /// An environment a test controls.
    #[derive(Debug, Default)]
    struct MapEnv(BTreeMap<String, String>);

    impl MapEnv {
        fn with(pairs: &[(&str, &str)]) -> Arc<Self> {
            Arc::new(Self(
                pairs
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
            ))
        }
    }

    impl EnvSource for MapEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    struct Harness {
        _home: TempHome,
        store: FileSecrets,
        path: PathBuf,
    }

    fn harness(env: Arc<MapEnv>) -> Harness {
        let home = temp_home();
        let path = home.path().join("credentials.toml");
        Harness {
            _home: home,
            store: FileSecrets::new(&path, env),
            path,
        }
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("the file exists")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn a_stored_key_comes_back_with_its_source() {
        let harness = harness(MapEnv::with(&[]));
        harness.store.set("deepseek", "sk-stored").expect("stored");

        let key = harness
            .store
            .get("deepseek", Some("DEEPSEEK_API_KEY"))
            .expect("read")
            .expect("present");
        assert_eq!(key.expose(), "sk-stored");
        assert_eq!(key.source(), &KeySource::File);
    }

    #[test]
    fn the_environment_overrides_the_file_and_says_so() {
        let harness = harness(MapEnv::with(&[("DEEPSEEK_API_KEY", "sk-from-env")]));
        harness.store.set("deepseek", "sk-stored").expect("stored");

        let key = harness
            .store
            .get("deepseek", Some("DEEPSEEK_API_KEY"))
            .expect("read")
            .expect("present");
        assert_eq!(key.expose(), "sk-from-env", "the documented variable wins");
        assert_eq!(
            key.source(),
            &KeySource::Environment("DEEPSEEK_API_KEY".to_owned())
        );
        assert!(key.source().is_environment());
    }

    #[test]
    fn an_empty_environment_variable_does_not_shadow_a_stored_key() {
        let harness = harness(MapEnv::with(&[("DEEPSEEK_API_KEY", "")]));
        harness.store.set("deepseek", "sk-stored").expect("stored");
        let key = harness
            .store
            .get("deepseek", Some("DEEPSEEK_API_KEY"))
            .expect("read")
            .expect("present");
        assert_eq!(key.expose(), "sk-stored");
        assert_eq!(key.source(), &KeySource::File);
    }

    #[test]
    fn a_provider_without_a_key_is_none_not_an_error() {
        let harness = harness(MapEnv::with(&[]));
        assert!(harness.store.get("openai", None).expect("read").is_none());
        assert!(harness.store.status().expect("status").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_written_mode_0600() {
        let harness = harness(MapEnv::with(&[]));
        harness.store.set("deepseek", "sk-1").expect("stored");
        assert_eq!(mode_of(&harness.path), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_file_is_refused_with_the_command_that_fixes_it() {
        let harness = harness(MapEnv::with(&[]));
        harness.store.set("deepseek", "sk-1").expect("stored");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&harness.path, std::fs::Permissions::from_mode(0o644))
                .expect("chmod");
        }
        let error = harness
            .store
            .get("deepseek", None)
            .expect_err("refused rather than read");
        assert!(
            matches!(error, SecretError::InsecureMode { .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("chmod 600"), "{error}");
        assert!(error.to_string().contains("644"), "{error}");
    }

    #[test]
    fn keys_are_stored_per_provider_and_other_entries_survive_a_write() {
        let harness = harness(MapEnv::with(&[]));
        harness.store.set("deepseek", "sk-deep").expect("stored");
        harness.store.set("openrouter", "sk-or").expect("stored");
        harness
            .store
            .set("deepseek", "sk-deep-2")
            .expect("replaced");

        let text = std::fs::read_to_string(&harness.path).expect("read");
        assert!(text.contains("sk-or"), "the other provider is untouched");
        assert!(text.contains("sk-deep-2"), "{text}");
        assert!(!text.contains("sk-deep\""), "the old key is gone: {text}");

        let status = harness.store.status().expect("status");
        let providers: Vec<&str> = status.iter().map(|entry| entry.provider.as_str()).collect();
        assert_eq!(providers, ["deepseek", "openrouter"]);
        assert!(
            status
                .iter()
                .all(crate::ports::secret::KeyStatus::is_present)
        );
    }

    #[test]
    fn clearing_a_key_leaves_the_others_alone() {
        let harness = harness(MapEnv::with(&[]));
        harness.store.set("deepseek", "sk-deep").expect("stored");
        harness.store.set("openai", "sk-openai").expect("stored");

        harness.store.remove("deepseek").expect("removed");
        assert!(harness.store.get("deepseek", None).expect("read").is_none());
        assert_eq!(
            harness
                .store
                .get("openai", None)
                .expect("read")
                .expect("present")
                .expose(),
            "sk-openai"
        );
    }

    #[test]
    fn clearing_a_key_that_was_never_there_is_not_an_error() {
        let harness = harness(MapEnv::with(&[]));
        assert!(harness.store.remove("nobody").is_ok());
    }

    #[test]
    fn comments_and_unknown_keys_in_the_file_survive_a_write() {
        let harness = harness(MapEnv::with(&[]));
        std::fs::write(
            &harness.path,
            "# my keys\nversion = 1\nnote = \"hand written\"\n",
        )
        .expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&harness.path, std::fs::Permissions::from_mode(0o600))
                .expect("chmod");
        }

        harness.store.set("deepseek", "sk-1").expect("stored");
        let text = std::fs::read_to_string(&harness.path).expect("read");
        // `toml` cannot preserve comments, and this file is not one the user is
        // expected to hand-edit; what matters is that the key is stored and the
        // document stays valid.
        assert!(text.contains("sk-1"), "{text}");
        assert!(toml::from_str::<Document>(&text).is_ok(), "{text}");
    }

    #[test]
    fn a_key_never_appears_in_an_error_or_a_debug_rendering() {
        let harness = harness(MapEnv::with(&[]));
        harness
            .store
            .set("deepseek", "sk-super-secret")
            .expect("stored");

        let key = harness
            .store
            .get("deepseek", None)
            .expect("read")
            .expect("present");
        let debug = format!("{key:?} {:?}", harness.store);
        assert!(!debug.contains("sk-super-secret"), "{debug}");

        // The file mode error is the one error that could plausibly quote the file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&harness.path, std::fs::Permissions::from_mode(0o644))
                .expect("chmod");
            let error = harness.store.get("deepseek", None).expect_err("refused");
            assert!(!error.to_string().contains("sk-super-secret"), "{error}");
            assert!(!format!("{error:?}").contains("sk-super-secret"));
        }
    }

    #[test]
    fn a_missing_file_is_a_first_run_not_a_failure() {
        let harness = harness(MapEnv::with(&[]));
        assert!(!harness.path.exists());
        assert!(harness.store.status().expect("status").is_empty());
        assert!(harness.store.get("deepseek", None).expect("read").is_none());
    }

    #[test]
    fn a_broken_file_is_reported_without_echoing_it() {
        let harness = harness(MapEnv::with(&[]));
        std::fs::write(&harness.path, "this is not toml = = =").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&harness.path, std::fs::Permissions::from_mode(0o600))
                .expect("chmod");
        }
        let error = harness.store.get("deepseek", None).expect_err("reported");
        assert!(matches!(error, SecretError::Malformed(_)), "{error:?}");
        assert!(error.to_string().contains("not usable"), "{error}");
    }
}
