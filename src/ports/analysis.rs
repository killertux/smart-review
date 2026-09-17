//! The analysis cache port (FR-4.3, DEC-15).
//!
//! An analysis costs money, so it is stored, and everything about *how* it is stored
//! is decided by one requirement: a cached answer must never be presented as if it
//! described the current commits.
//!
//! - **the key is the whole question.** Repository, pull request, head commit,
//!   provider, model, thinking settings and prompt version. Changing any of them is a
//!   different question, so flipping thinking on never serves the answer computed with
//!   it off (FR-4.3, FR-4.8).
//! - **a stored entry can be checked.** The key is written into the file and compared
//!   on read, so a hash collision is a miss rather than somebody else's analysis.
//! - **listing is how staleness is noticed.** Entries are found per pull request
//!   rather than per key, because the one thing the UI has to say is "this analysis
//!   was made for an older commit" — which requires finding an entry the current key
//!   does not match (DEC-15).
//!
//! The port owns the pull request's analysis directory, the manual review-plan
//! overrides included: they describe the same analysis and belong next to it.

use std::fmt;

use crate::domain::analysis::Analysis;
use crate::domain::model::Thinking;
use crate::domain::plan::Plan;
use crate::domain::repo::RepoId;

/// What was asked, in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisKey {
    /// The repository, as `host/owner/name`.
    pub repo: String,
    /// The pull request number.
    pub pr: u64,
    /// The head commit the analysis describes.
    pub head_sha: String,
    /// The base/merge-base revision used for the evidence, when available (IR-12).
    pub base_sha: Option<String>,
    /// The resolved context policy and inventory, without source bytes or secrets.
    pub context_fingerprint: String,
    /// Version of the cache-identity contract. Entries from an earlier version are
    /// retained for diagnostics but never certified as current (IR-12).
    pub identity_version: u8,
    /// The provider.
    pub provider: String,
    /// The model.
    pub model: String,
    /// The provider endpoint selected from catalog metadata, never credentials.
    pub endpoint: Option<String>,
    /// The thinking settings, if any (FR-4.8).
    pub thinking: Option<Thinking>,
    /// Effective input allowance after model/window limits.
    pub input_tokens: u32,
    /// Effective completion allowance after model/window limits.
    pub max_tokens: Option<u32>,
    /// Effective temperature, rendered deterministically for cache identity.
    pub temperature: Option<String>,
    /// The prompt version (FR-4.3).
    pub prompt_version: u32,
}

impl AnalysisKey {
    /// Whether this entry predates the full IR-12 identity contract.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        self.identity_version < 1
    }

    /// The key as one line, used to hash it and to explain a cache miss.
    ///
    /// The parts are separated by a byte that cannot appear in a repository name, a
    /// SHA or a model id, so two different keys cannot produce the same line by
    /// concatenation.
    #[must_use]
    pub fn canonical(&self) -> String {
        let thinking = match &self.thinking {
            Some(thinking) => serde_json::to_string(thinking).unwrap_or_default(),
            None => "none".to_owned(),
        };
        format!(
            "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
            self.repo,
            self.pr,
            self.head_sha,
            self.base_sha.as_deref().unwrap_or("unavailable"),
            self.context_fingerprint,
            self.identity_version,
            self.provider,
            self.model,
            self.endpoint.as_deref().unwrap_or("provider-default"),
            thinking,
            self.input_tokens,
            self.max_tokens
                .map_or_else(|| "provider-default".to_owned(), |value| value.to_string()),
            self.temperature.as_deref().unwrap_or("provider-default"),
            self.prompt_version
        )
    }

    /// A file-name-safe digest of the key.
    ///
    /// FNV-1a, not a cryptographic hash: the key itself is stored in the file and
    /// compared on read, so a collision costs a cache miss rather than serving the
    /// wrong analysis, and adding a hash dependency for a file name is not worth it.
    #[must_use]
    pub fn digest(&self) -> String {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash = OFFSET;
        for byte in self.canonical().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        format!("{hash:016x}")
    }

    /// The model, with its thinking setting, for the staleness notice (FR-4.3).
    #[must_use]
    pub fn label(&self) -> String {
        match Thinking::label(&self.thinking) {
            Some(thinking) => format!("{} ({thinking})", self.model),
            None => self.model.clone(),
        }
    }

    /// The same key with a different head commit, for explaining what changed.
    #[must_use]
    pub fn with_head(&self, head_sha: &str) -> Self {
        let mut key = self.clone();
        head_sha.clone_into(&mut key.head_sha);
        key
    }
}

/// An analysis that was found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAnalysis {
    /// The question it answers.
    pub key: AnalysisKey,
    /// The document.
    pub analysis: Analysis,
    /// The model's text, kept so a repaired or failed answer can be read back
    /// (FR-4.1).
    pub raw: String,
    /// What normalization corrected, kept with the document (FR-4.1).
    ///
    /// Persisted rather than recomputed because it is a property of the *answer*, not
    /// of the run that fetched it: an analysis read back from the cache dropped three
    /// invented paths just as much as the run that wrote it did, and the user who
    /// opens it tomorrow should be told.
    pub warnings: Vec<String>,
    /// Whether a repair pass was needed to get this answer (FR-4.1).
    pub repaired: bool,
    /// When it was stored, in seconds since the Unix epoch.
    pub stored_at: u64,
}

/// What can go wrong with the analysis cache.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnalysisCacheError {
    /// The directory could not be read or written.
    #[error("could not {action} {path}: {cause}")]
    Io {
        /// What was being attempted.
        action: String,
        /// The path.
        path: String,
        /// The cause.
        cause: String,
    },

    /// The entry could not be parsed.
    #[error("{path} is not a readable analysis: {reason}")]
    Malformed {
        /// The path.
        path: String,
        /// What was wrong.
        reason: String,
    },

    /// Another application instance has changed this durable review plan.
    #[error(
        "another smart-review instance changed this review order; reopen it before editing again"
    )]
    Conflict,
}

/// Where analyses and their review-plan overrides are kept.
pub trait AnalysisCachePort: fmt::Debug + Send + Sync {
    /// The entry for this exact key, if there is one.
    ///
    /// # Errors
    ///
    /// Returns [`AnalysisCacheError`] when the directory cannot be read or an entry
    /// that matches the key is unreadable. Entries that do not match the key are not
    /// this call's business.
    fn get(&self, key: &AnalysisKey) -> Result<Option<StoredAnalysis>, AnalysisCacheError>;

    /// Every analysis stored for a pull request, newest first.
    ///
    /// Entries that cannot be read are skipped rather than returned as errors: the
    /// cache is disposable, and one corrupt file must not hide the valid ones.
    ///
    /// # Errors
    ///
    /// Returns [`AnalysisCacheError::Io`] when the directory itself cannot be listed.
    fn list(&self, repo: &RepoId, pr: u64) -> Result<Vec<StoredAnalysis>, AnalysisCacheError>;

    /// Stores an analysis, replacing any entry with the same key.
    ///
    /// # Errors
    ///
    /// Returns [`AnalysisCacheError::Io`] when it cannot be written.
    fn put(&self, stored: &StoredAnalysis) -> Result<(), AnalysisCacheError>;

    /// The review-plan overrides for a pull request, if any (FR-4.2).
    ///
    /// # Errors
    ///
    /// As [`AnalysisCachePort::get`].
    fn plan(&self, repo: &RepoId, pr: u64) -> Result<Option<Plan>, AnalysisCacheError>;

    /// Stores the review-plan overrides for a pull request.
    ///
    /// # Errors
    ///
    /// Returns the saved plan, including its next durable revision.
    ///
    /// As [`AnalysisCachePort::put`]. Returns [`AnalysisCacheError::Conflict`] when
    /// another app instance has changed the plan since the caller read it.
    fn put_plan(&self, repo: &RepoId, pr: u64, plan: &Plan) -> Result<Plan, AnalysisCacheError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> AnalysisKey {
        AnalysisKey {
            repo: "github.com/acme/service".to_owned(),
            pr: 141,
            head_sha: "abc123".to_owned(),
            base_sha: Some("base123".to_owned()),
            context_fingerprint: "context".to_owned(),
            identity_version: 1,
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            endpoint: Some("https://api.deepseek.test/v1".to_owned()),
            thinking: None,
            input_tokens: 12_000,
            max_tokens: Some(4_000),
            temperature: Some("0.2".to_owned()),
            prompt_version: 1,
        }
    }

    #[test]
    fn every_part_of_the_question_changes_the_digest() {
        let base = key();
        let variants = [
            AnalysisKey {
                head_sha: "def456".to_owned(),
                ..base.clone()
            },
            // FR-4.3/FR-4.8: flipping thinking must never hit the other answer.
            AnalysisKey {
                thinking: Some(Thinking::Toggle { value: true }),
                ..base.clone()
            },
            AnalysisKey {
                thinking: Some(Thinking::Effort {
                    value: "high".to_owned(),
                }),
                ..base.clone()
            },
            AnalysisKey {
                provider: "openrouter".to_owned(),
                ..base.clone()
            },
            AnalysisKey {
                model: "deepseek-v4-lite".to_owned(),
                ..base.clone()
            },
            AnalysisKey {
                endpoint: Some("https://other.test/v1".to_owned()),
                ..base.clone()
            },
            AnalysisKey {
                max_tokens: Some(2_000),
                ..base.clone()
            },
            AnalysisKey {
                pr: 142,
                ..base.clone()
            },
            AnalysisKey {
                prompt_version: 2,
                ..base.clone()
            },
            AnalysisKey {
                repo: "github.com/acme/other".to_owned(),
                ..base.clone()
            },
        ];
        for variant in &variants {
            assert_ne!(
                variant.digest(),
                base.digest(),
                "{} and {} must not share a cache entry",
                variant.canonical(),
                base.canonical()
            );
        }
        // And the same question is stable.
        assert_eq!(key().digest(), key().digest());
        assert_eq!(key().digest().len(), 16);
        assert!(key().digest().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn two_thinking_settings_differ_from_each_other() {
        let on = AnalysisKey {
            thinking: Some(Thinking::Toggle { value: true }),
            ..key()
        };
        let off = AnalysisKey {
            thinking: Some(Thinking::Toggle { value: false }),
            ..key()
        };
        assert_ne!(on.digest(), off.digest());
        // And "no thinking setting" is its own question.
        assert_ne!(on.digest(), key().digest());
    }

    #[test]
    fn the_canonical_form_cannot_be_confused_by_concatenation() {
        // "acme/service1" + "41" and "acme/service" + "141" must not collide, which a
        // separator that cannot appear in either part guarantees.
        let left = AnalysisKey {
            repo: "github.com/acme/service1".to_owned(),
            pr: 41,
            ..key()
        };
        let right = AnalysisKey {
            repo: "github.com/acme/service".to_owned(),
            pr: 141,
            ..key()
        };
        assert_ne!(left.canonical(), right.canonical());
        assert_ne!(left.digest(), right.digest());
    }

    #[test]
    fn the_label_names_the_model_and_its_thinking_setting() {
        assert_eq!(key().label(), "deepseek-v4-pro");
        let thinking = AnalysisKey {
            thinking: Some(Thinking::Effort {
                value: "high".to_owned(),
            }),
            ..key()
        };
        assert_eq!(thinking.label(), "deepseek-v4-pro (thinking:high)");
    }

    #[test]
    fn with_head_replaces_only_the_commit() {
        let other = key().with_head("def456");
        assert_eq!(other.head_sha, "def456");
        assert_eq!(other.repo, key().repo);
        assert_eq!(other.pr, key().pr);
        assert_ne!(other.digest(), key().digest());
    }
}
