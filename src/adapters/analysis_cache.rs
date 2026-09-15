//! The on-disk analysis cache (FR-4.3, FR-4.2).
//!
//! Layout: `<home>/cache/analysis/<host>/<owner>/<name>/pr-<N>/<digest>.json` for an
//! analysis, and `plan.json` beside it for the manual review-plan overrides. One
//! directory per pull request is what makes DEC-15 implementable: the UI has to find
//! an analysis made for an *older* commit, and listing a directory is how it does.
//!
//! Two properties matter more than speed here:
//!
//! - **an entry can be checked.** The key is stored in the file and compared on read,
//!   so a digest that happens to collide is a miss, never somebody else's analysis.
//! - **a bad entry is a miss, not a failure.** The cache is disposable by
//!   definition (`state.toml`'s comment says so); a truncated file left by a full disk
//!   must not stop the user from analysing the pull request again.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::adapters::fs::write_atomic;
use crate::domain::analysis::Analysis;
use crate::domain::plan::Plan;
use crate::domain::repo::RepoId;
use crate::logging::{self, Level};
use crate::ports::analysis::{AnalysisCacheError, AnalysisCachePort, AnalysisKey, StoredAnalysis};
use crate::ports::cache::CacheKey;

/// The file an analysis is stored in.
const PLAN_FILE: &str = "plan.json";

/// Analyses under a directory.
#[derive(Debug, Clone)]
pub struct DiskAnalysisCache {
    root: PathBuf,
}

/// The envelope written to disk: the question, the answer, and the raw text.
#[derive(Debug, Serialize, Deserialize)]
struct Document {
    /// The question this answers, stored so a read can verify it.
    key: StoredKey,
    /// The document (§7.1).
    analysis: Analysis,
    /// The model's text, so a repaired answer can be read back (FR-4.1).
    #[serde(default)]
    raw: String,
    /// What normalization corrected (FR-4.1).
    #[serde(default)]
    warnings: Vec<String>,
    /// Whether a repair pass was needed (FR-4.1).
    #[serde(default)]
    repaired: bool,
    /// When it was stored, in seconds since the Unix epoch.
    stored_at: u64,
}

/// The key as stored. Separate from the port's type so the file format is explicit
/// rather than following `serde`'s idea of the Rust type.
#[derive(Debug, Serialize, Deserialize)]
struct StoredKey {
    repo: String,
    pr: u64,
    head_sha: String,
    provider: String,
    model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<crate::domain::model::Thinking>,
    prompt_version: u32,
}

impl StoredKey {
    fn of(key: &AnalysisKey) -> Self {
        Self {
            repo: key.repo.clone(),
            pr: key.pr,
            head_sha: key.head_sha.clone(),
            provider: key.provider.clone(),
            model: key.model.clone(),
            thinking: key.thinking.clone(),
            prompt_version: key.prompt_version,
        }
    }

    fn matches(&self, key: &AnalysisKey) -> bool {
        self.repo == key.repo
            && self.pr == key.pr
            && self.head_sha == key.head_sha
            && self.provider == key.provider
            && self.model == key.model
            && self.thinking == key.thinking
            && self.prompt_version == key.prompt_version
    }

    fn into_key(self) -> AnalysisKey {
        AnalysisKey {
            repo: self.repo,
            pr: self.pr,
            head_sha: self.head_sha,
            provider: self.provider,
            model: self.model,
            thinking: self.thinking,
            prompt_version: self.prompt_version,
        }
    }
}

impl DiskAnalysisCache {
    /// Binds the cache to a directory, created on demand.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where the cache lives, for `:doctor`.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory holding one pull request's analyses.
    fn pr_dir(&self, repo: &RepoId, pr: u64) -> PathBuf {
        self.root
            .join(repo.host())
            .join(repo.owner())
            .join(repo.name())
            .join(format!("pr-{pr}"))
    }

    /// The path a key is stored at.
    ///
    /// The repository and pull request decide the directory; the digest decides the
    /// file name within it.
    #[must_use]
    pub fn path_for(&self, key: &AnalysisKey) -> PathBuf {
        let dir = RepoId::parse(&key.repo).map_or_else(
            |_| self.root.join(key.pr.to_string()),
            |repo| self.pr_dir(&repo, key.pr),
        );
        dir.join(format!("{}.json", key.digest()))
    }

    /// Reads every readable document in a directory.
    fn read_dir(dir: &Path) -> Result<Vec<StoredAnalysis>, AnalysisCacheError> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(AnalysisCacheError::Io {
                    action: "read the analysis cache".to_owned(),
                    path: dir.display().to_string(),
                    cause: error.to_string(),
                });
            }
        };
        let mut stored = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            match read_document(&path) {
                Ok(Some(analysis)) => stored.push(analysis),
                // A file that cannot be read is skipped and said out loud in the log:
                // one corrupt entry must not hide the others, and it must not be
                // silent either.
                Ok(None) => {}
                Err(error) => logging::log(
                    Level::Warn,
                    format!("ignoring {error}; it can be deleted and recomputed"),
                ),
            }
        }
        stored.sort_by_key(|entry| std::cmp::Reverse(entry.stored_at));
        Ok(stored)
    }

    fn write(path: &Path, body: &str) -> Result<(), AnalysisCacheError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| AnalysisCacheError::Io {
                action: "create".to_owned(),
                path: parent.display().to_string(),
                cause: error.to_string(),
            })?;
        }
        write_atomic(path, body).map_err(|error| AnalysisCacheError::Io {
            action: "write".to_owned(),
            path: path.display().to_string(),
            cause: error.to_string(),
        })
    }
}

/// Reads one document, verifying nothing but its shape.
///
/// Returns `Ok(None)` when the file is readable JSON that is not an analysis this build
/// understands, which a caller treats as a miss.
fn read_document(path: &Path) -> Result<Option<StoredAnalysis>, AnalysisCacheError> {
    let text = std::fs::read_to_string(path).map_err(|error| AnalysisCacheError::Io {
        action: "read".to_owned(),
        path: path.display().to_string(),
        cause: error.to_string(),
    })?;
    match serde_json::from_str::<Document>(&text) {
        Ok(document) => Ok(Some(StoredAnalysis {
            key: document.key.into_key(),
            analysis: document.analysis,
            raw: document.raw,
            warnings: document.warnings,
            repaired: document.repaired,
            stored_at: document.stored_at,
        })),
        Err(error) => Err(AnalysisCacheError::Malformed {
            path: path.display().to_string(),
            reason: error.to_string(),
        }),
    }
}

impl AnalysisCachePort for DiskAnalysisCache {
    fn get(&self, key: &AnalysisKey) -> Result<Option<StoredAnalysis>, AnalysisCacheError> {
        let path = self.path_for(key);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AnalysisCacheError::Io {
                    action: "read".to_owned(),
                    path: path.display().to_string(),
                    cause: error.to_string(),
                });
            }
        };
        let document: Document =
            serde_json::from_str(&text).map_err(|error| AnalysisCacheError::Malformed {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        // The digest is a file name, not a guarantee: the key is what decides whether
        // this answer is the one that was asked for.
        if !document.key.matches(key) {
            logging::log(
                Level::Warn,
                format!(
                    "{} holds an analysis for a different question; ignoring it",
                    path.display()
                ),
            );
            return Ok(None);
        }
        Ok(Some(StoredAnalysis {
            key: document.key.into_key(),
            analysis: document.analysis,
            raw: document.raw,
            warnings: document.warnings,
            repaired: document.repaired,
            stored_at: document.stored_at,
        }))
    }

    fn list(&self, repo: &RepoId, pr: u64) -> Result<Vec<StoredAnalysis>, AnalysisCacheError> {
        Self::read_dir(&self.pr_dir(repo, pr))
    }

    fn put(&self, stored: &StoredAnalysis) -> Result<(), AnalysisCacheError> {
        let path = self.path_for(&stored.key);
        let document = Document {
            key: StoredKey::of(&stored.key),
            analysis: stored.analysis.clone(),
            raw: stored.raw.clone(),
            warnings: stored.warnings.clone(),
            repaired: stored.repaired,
            stored_at: stored.stored_at,
        };
        let body = serde_json::to_string(&document).map_err(|error| AnalysisCacheError::Io {
            action: "serialise an analysis for".to_owned(),
            path: path.display().to_string(),
            cause: error.to_string(),
        })?;
        Self::write(&path, &body)
    }

    fn plan(&self, repo: &RepoId, pr: u64) -> Result<Option<Plan>, AnalysisCacheError> {
        let path = self.pr_dir(repo, pr).join(PLAN_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AnalysisCacheError::Io {
                    action: "read".to_owned(),
                    path: path.display().to_string(),
                    cause: error.to_string(),
                });
            }
        };
        match serde_json::from_str(&text) {
            Ok(plan) => Ok(Some(plan)),
            // A plan that cannot be read is a plan that was not made: the analysis is
            // still there, and the derived plan is as good as the overrides were not.
            Err(error) => {
                logging::log(
                    Level::Warn,
                    format!(
                        "the review plan in {} could not be read ({error}); the analysis's own \
                         order will be used",
                        path.display()
                    ),
                );
                Ok(None)
            }
        }
    }

    fn put_plan(&self, repo: &RepoId, pr: u64, plan: &Plan) -> Result<(), AnalysisCacheError> {
        let path = self.pr_dir(repo, pr).join(PLAN_FILE);
        let body = serde_json::to_string(plan).map_err(|error| AnalysisCacheError::Io {
            action: "serialise the review plan for".to_owned(),
            path: path.display().to_string(),
            cause: error.to_string(),
        })?;
        Self::write(&path, &body)
    }
}

/// The cache key prefix the analysis cache uses, for the diagnostics report.
#[must_use]
pub fn cache_key_for(repo: &RepoId, pr: u64) -> Option<CacheKey> {
    CacheKey::new(format!("{}/pr-{pr}", repo.key())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::analysis::{Analysis, AnalysisUsage};
    use crate::domain::model::Thinking;
    use crate::domain::plan::{OrderMode, Plan, PlanSource};
    use crate::ports::analysis::AnalysisCachePort;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Each test gets its own directory without a temp-file dependency.
    fn cache() -> (DiskAnalysisCache, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "smart-review-analysis-cache-{}-{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        (DiskAnalysisCache::new(root.clone()), root)
    }

    fn repo() -> RepoId {
        RepoId::new("github.com", "acme", "service")
    }

    fn key() -> AnalysisKey {
        AnalysisKey {
            repo: "github.com/acme/service".to_owned(),
            pr: 141,
            head_sha: "abc123".to_owned(),
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            thinking: None,
            prompt_version: 1,
        }
    }

    fn analysis(head_sha: &str) -> Analysis {
        Analysis {
            version: 1,
            prompt_version: 1,
            model: "deepseek/deepseek-v4-pro".to_owned(),
            head_sha: head_sha.to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            token_usage: AnalysisUsage {
                complete: true,
                prompt: 100,
                completion: 200,
                reasoning: Some(50),
            },
            summary: "what changed".to_owned(),
            intent: "why".to_owned(),
            risk_areas: Vec::new(),
            review_plan: Vec::new(),
            per_file_notes: Vec::new(),
            suggested_questions: Vec::new(),
        }
    }

    fn stored(key: &AnalysisKey, stored_at: u64) -> StoredAnalysis {
        StoredAnalysis {
            key: key.clone(),
            analysis: analysis(&key.head_sha),
            raw: "{\"summary\": \"what changed\"}".to_owned(),
            warnings: vec!["one invented path was dropped".to_owned()],
            repaired: false,
            stored_at,
        }
    }

    #[test]
    fn an_entry_round_trips_with_its_raw_text() {
        let (cache, root) = cache();
        let key = key();
        cache.put(&stored(&key, 100)).expect("stores");
        let found = cache.get(&key).expect("reads").expect("is there");
        assert_eq!(found.analysis.summary, "what changed");
        assert_eq!(found.analysis.token_usage.reasoning, Some(50));
        assert_eq!(found.raw, "{\"summary\": \"what changed\"}");
        assert_eq!(found.stored_at, 100);
        // The corrections travel with the document: they describe the answer, not
        // the run.
        assert_eq!(found.warnings, ["one invented path was dropped"]);
        assert!(!found.repaired);
        assert_eq!(found.key, key);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_different_question_is_a_miss_even_in_the_same_directory() {
        let (cache, root) = cache();
        let key = key();
        cache.put(&stored(&key, 100)).expect("stores");
        let other = AnalysisKey {
            thinking: Some(Thinking::Toggle { value: true }),
            ..key.clone()
        };
        assert!(cache.get(&other).expect("reads").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_digest_collision_is_a_miss_rather_than_a_wrong_answer() {
        let (cache, root) = cache();
        let key = key();
        // Write the entry under the *wrong* file name, which is what a collision would
        // look like from the reader's side.
        let other = AnalysisKey {
            model: "some-other-model".to_owned(),
            ..key.clone()
        };
        let path = cache.path_for(&other);
        let document = Document {
            key: StoredKey::of(&key),
            analysis: analysis(&key.head_sha),
            raw: String::new(),
            warnings: Vec::new(),
            repaired: false,
            stored_at: 1,
        };
        DiskAnalysisCache::write(
            &path,
            &serde_json::to_string(&document).expect("serialises"),
        )
        .expect("writes");
        assert!(cache.get(&other).expect("reads").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn listing_finds_every_analysis_for_a_pull_request_newest_first() {
        let (cache, root) = cache();
        let first = key();
        let second = first.with_head("def456");
        cache.put(&stored(&first, 100)).expect("stores");
        cache.put(&stored(&second, 200)).expect("stores");
        let found = cache.list(&repo(), 141).expect("lists");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].stored_at, 200, "newest first");
        assert_eq!(found[1].key.head_sha, "abc123");
        // Another pull request has its own directory.
        assert!(cache.list(&repo(), 142).expect("lists").is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_corrupt_entry_is_skipped_and_the_valid_ones_survive() {
        let (cache, root) = cache();
        let key = key();
        cache.put(&stored(&key, 100)).expect("stores");
        let broken = cache.pr_dir(&repo(), 141).join("broken.json");
        std::fs::write(&broken, "{ this is not json").expect("writes");
        let found = cache.list(&repo(), 141).expect("lists");
        assert_eq!(found.len(), 1, "the readable entry is still there");
        assert_eq!(found[0].key, key);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_read_of_a_corrupt_named_entry_reports_it() {
        let (cache, root) = cache();
        let key = key();
        let path = cache.path_for(&key);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("creates");
        std::fs::write(&path, "{ not json").expect("writes");
        let error = cache.get(&key).expect_err("reports it");
        assert!(
            matches!(error, AnalysisCacheError::Malformed { .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("not a readable analysis"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn storing_the_same_key_twice_replaces_the_entry() {
        let (cache, root) = cache();
        let key = key();
        cache.put(&stored(&key, 100)).expect("stores");
        let mut replacement = stored(&key, 300);
        replacement.analysis.summary = "a newer answer".to_owned();
        cache.put(&replacement).expect("stores");
        let found = cache.get(&key).expect("reads").expect("is there");
        assert_eq!(found.analysis.summary, "a newer answer");
        assert_eq!(cache.list(&repo(), 141).expect("lists").len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_plan_overrides_round_trip_beside_the_analysis() {
        let (cache, root) = cache();
        assert!(cache.plan(&repo(), 141).expect("reads").is_none());
        let plan = Plan {
            head_sha: "abc123".to_owned(),
            source: PlanSource::Analysis,
            groups: Vec::new(),
            overridden: true,
        };
        cache.put_plan(&repo(), 141, &plan).expect("stores");
        let found = cache.plan(&repo(), 141).expect("reads").expect("is there");
        assert!(found.overridden);
        assert_eq!(found.head_sha, "abc123");
        // Storing a plan does not disturb the analyses next to it.
        cache.put(&stored(&key(), 100)).expect("stores");
        assert!(cache.plan(&repo(), 141).expect("reads").is_some());
        assert!(cache.get(&key()).expect("reads").is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_plan_that_cannot_be_read_is_a_miss_not_an_error() {
        let (cache, root) = cache();
        let path = cache.pr_dir(&repo(), 141).join(PLAN_FILE);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("creates");
        std::fs::write(&path, "not json at all").expect("writes");
        assert!(
            cache.plan(&repo(), 141).expect("reads").is_none(),
            "the analysis is still usable without its overrides"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_digest_is_reproducible_across_processes() {
        // An analysis written by an earlier run has to be found by a later one, which is
        // only true if the digest depends on nothing but the key. The literal is the
        // guarantee: change the hashing and this test fails on purpose, because every
        // existing cache entry would stop being found.
        assert_eq!(key().digest(), "b36e57269e6f7234");
    }

    #[test]
    fn the_path_is_inside_the_cache_root() {
        let cache = DiskAnalysisCache::new("/home/someone/.smart-review/cache/analysis");
        let path = cache.path_for(&key());
        assert_eq!(
            path,
            PathBuf::from(
                "/home/someone/.smart-review/cache/analysis/github.com/acme/service/pr-141/\
                 b36e57269e6f7234.json"
            )
        );
    }

    #[test]
    fn the_cache_key_for_diagnostics_is_well_formed() {
        let key = cache_key_for(&repo(), 141).expect("valid");
        assert_eq!(key.to_string(), "github.com/acme/service/pr-141");
    }

    #[test]
    fn ordering_the_plan_by_mode_still_works_after_a_round_trip() {
        let (cache, root) = cache();
        let files = vec!["src/b.rs".to_owned(), "src/a.rs".to_owned()];
        let plan = Plan::heuristic("abc123", &files);
        cache.put_plan(&repo(), 141, &plan).expect("stores");
        let found = cache.plan(&repo(), 141).expect("reads").expect("is there");
        assert_eq!(
            found.files(OrderMode::Recommended, &files),
            ["src/a.rs", "src/b.rs"],
            "the plan comes back usable, not just parseable"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
