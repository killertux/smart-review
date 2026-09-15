//! The analysis use case (FR-4.1, FR-4.4, FR-4.6): gather, ask, normalize, store.
//!
//! This is the one place that knows the whole sequence, and it is deliberately a
//! straight line:
//!
//! 1. gather the context bundle (reading changed files at head, and the repository's
//!    conventions, through [`WorkspacePort`]);
//! 2. ask the provider, streaming, so `Esc` and the progress line both work (FR-4.4);
//! 3. normalize the answer against the diff's real paths (FR-4.1);
//! 4. when that fails, ask once more with the reason — a model that answers with prose
//!    is a normal event, not an exception (FR-4.1);
//! 5. store the result, successful or not, so a failure can be read back rather than
//!    re-paid for.
//!
//! Everything IO-shaped lives in the caller's worker thread; this type holds ports and
//! never touches the terminal. The reducer sees only values.

pub use crate::application::context::Checkout;
use crate::application::context::ContextSource;
use crate::domain::analysis::{
    Analysis, AnalysisUsage, Normalized, ParseFailure, PathIndex, Severity, normalize,
    repair_prompt, system_prompt, user_prompt,
};
use crate::domain::context::{Bundle, BundlePolicy, human_bytes};
use crate::domain::diff::Patch;
use crate::domain::pr::PullRequestDetail;
use crate::domain::repo::RepoId;
use crate::ports::analysis::{AnalysisCachePort, AnalysisKey, StoredAnalysis};
use crate::ports::llm::{ChatRequest, DeltaHandler, LlmError, LlmPort, TokenUsage};
use crate::ports::workspace::WorkspacePort;
use crate::ports::{Cancel, Clock};

/// What the caller wants a gathered bundle for (FR-4.6).
///
/// The bundle is expensive to gather and is the thing a user is asked to approve, so
/// the gather is shared: the estimate that is shown, the inspector that explains it and
/// the request that is finally sent all use the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisIntent {
    /// Show how large the request would be, before anything is sent (FR-4.6).
    Estimate,
    /// Send it (FR-4.1).
    Run,
    /// Open the `:context` inspector (FR-4.6).
    Inspect,
}

/// Progress reported while an analysis runs (FR-4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// What the job is doing now, for the status line.
    Stage(String),
    /// Model text as it arrived.
    Delta(String),
}

/// Receives progress as it happens.
pub type ProgressHandler<'a> = dyn FnMut(Progress) + Send + 'a;

/// Everything an analysis needs that the caller already knows.
///
/// `Clone` because the same request is gathered with, shown as an estimate and then
/// sent: the three steps must describe the same thing (FR-4.6).
#[derive(Debug, Clone)]
pub struct AnalysisRequest {
    /// What is being asked, for the cache key (FR-4.3).
    pub key: AnalysisKey,
    /// How to reach the provider, with the key and the thinking settings.
    pub chat: ChatRequest,
    /// The pull request, for the metadata and commit blocks.
    pub detail: Box<PullRequestDetail>,
    /// The diff, already parsed. `None` when it could not be read.
    pub patch: Option<Box<Patch>>,
    /// Where the files can be read, when a worktree exists (FR-3.1).
    pub checkout: Option<Checkout>,
    /// The bundle budget (FR-4.6).
    pub policy: BundlePolicy,
}

impl AnalysisRequest {
    /// The paths the diff changed, in the diff's own order.
    #[must_use]
    pub fn changed_paths(&self) -> Vec<String> {
        self.patch
            .as_ref()
            .map(|patch| {
                patch
                    .files
                    .iter()
                    .filter_map(|file| file.path().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// An analysis and a chat gather the same bundle (FR-5.3).
impl ContextSource for AnalysisRequest {
    fn changed_paths(&self) -> Vec<String> {
        AnalysisRequest::changed_paths(self)
    }

    fn detail(&self) -> &PullRequestDetail {
        &self.detail
    }

    fn patch(&self) -> Option<&Patch> {
        self.patch.as_deref()
    }

    fn checkout(&self) -> Option<&Checkout> {
        self.checkout.as_ref()
    }

    fn policy(&self) -> &BundlePolicy {
        &self.policy
    }
}

/// An analysis that succeeded.
#[derive(Debug, Clone)]
pub struct Analyzed {
    /// The document.
    pub analysis: Box<Analysis>,
    /// What was corrected on the way, shown to the user so a plan that lost a file is
    /// never a silent loss (FR-4.1).
    pub warnings: Vec<String>,
    /// Whether a repair pass was needed.
    pub repaired: bool,
    /// What the provider charged.
    pub usage: Option<TokenUsage>,
}

/// An answer that could not be used, with the text that produced it (FR-4.1).
#[derive(Debug, Clone)]
pub struct Unparsed {
    /// The model's text, for the failure view.
    pub raw: String,
    /// Why it was rejected.
    pub reason: String,
    /// Whether a repair pass was already tried. It is always true here; the field
    /// exists so the UI can say "after a retry" rather than "the model failed".
    pub repaired: bool,
    /// What the provider charged, across both attempts.
    pub usage: Option<TokenUsage>,
}

/// What an analysis produced.
#[derive(Debug, Clone)]
pub enum AnalysisRun {
    /// A usable document.
    Ready(Box<Analyzed>),
    /// Text that could not be normalized, kept so it can be read.
    Unparsed(Box<Unparsed>),
    /// The user cancelled it (FR-4.4). Partial text is in the progress stream.
    Cancelled,
}

/// The analysis use case.
pub struct Analyst<'a> {
    workspace: &'a dyn WorkspacePort,
    cache: &'a dyn AnalysisCachePort,
    llm: &'a dyn LlmPort,
    clock: &'a dyn Clock,
    repo: &'a RepoId,
}

impl std::fmt::Debug for Analyst<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Analyst")
            .field("repo", &self.repo.key())
            .finish_non_exhaustive()
    }
}

impl<'a> Analyst<'a> {
    /// Binds the use case to its ports.
    #[must_use]
    pub fn new(
        workspace: &'a dyn WorkspacePort,
        cache: &'a dyn AnalysisCachePort,
        llm: &'a dyn LlmPort,
        clock: &'a dyn Clock,
        repo: &'a RepoId,
    ) -> Self {
        Self {
            workspace,
            cache,
            llm,
            clock,
            repo,
        }
    }

    /// Gathers the context bundle, reading files at head (FR-4.6).
    ///
    /// Separated from [`Analyst::run`] because the estimate is shown *before* the
    /// first send for a repository (FR-4.6), and the bundle that was estimated is the
    /// one that is sent — gathering twice would be both slower and a chance for the
    /// two to differ.
    ///
    /// Delegates to [`crate::application::context::gather`], which chat shares (FR-5.3).
    #[must_use]
    pub fn gather(
        &self,
        request: &AnalysisRequest,
        cancel: &Cancel,
    ) -> (Bundle, Vec<(String, Vec<u8>)>) {
        let gathered = crate::application::context::gather(self.workspace, request, cancel);
        (gathered.bundle, gathered.files)
    }

    /// Gathers and asks (FR-4.1, FR-4.4).
    ///
    /// # Errors
    ///
    /// Returns the provider's own [`LlmError`] when the request failed. A *useful*
    /// failure — the provider answered something that cannot be parsed — is
    /// [`AnalysisRun::Unparsed`], not an error, because there is something to show.
    pub fn run(
        &self,
        request: &AnalysisRequest,
        bundle: &Bundle,
        cancel: &Cancel,
        progress: &mut ProgressHandler<'_>,
    ) -> Result<AnalysisRun, LlmError> {
        let conventions = bundle
            .segments
            .iter()
            .find(|segment| segment.kind == crate::domain::context::SegmentKind::Conventions)
            .map(|segment| segment.label.clone());
        let system = system_prompt(conventions.as_deref());
        let prompt = user_prompt(&bundle.text);
        let index = request
            .patch
            .as_deref()
            .map_or_else(PathIndex::default, PathIndex::from_patch);

        let mut usage = None;
        progress(Progress::Stage(format!("asking {}", request.chat.model)));
        let first = self.ask(request, &system, &prompt, cancel, progress)?;
        add_usage(&mut usage, first.usage);

        if cancel.is_cancelled() {
            return Ok(AnalysisRun::Cancelled);
        }

        let model_label = format!("{}/{}", request.key.provider, request.chat.model);
        // One timestamp for the whole run: a repair attempt must not look like a
        // different analysis because a second passed between them.
        let created_at = self.now_rfc3339();

        let attempt = self.normalize(
            &first.text,
            &index,
            &model_label,
            request,
            usage,
            &created_at,
        );
        let (normalized, repaired, raw) = match attempt {
            Ok(normalized) => (normalized, false, first.text),
            Err(failure) => {
                // One repair pass, with the reason and the previous answer (FR-4.1).
                progress(Progress::Stage(format!("repairing: {}", failure.reason)));
                let previous = first.text.clone();
                let repair = repair_prompt(&previous, &failure);
                // A failed repair attempt still leaves the first answer to show, with
                // the first failure's reason: the user has text to read either way,
                // which is what FR-4.1 asks for.
                let Ok(second) = self.ask(request, &system, &repair, cancel, progress) else {
                    return Ok(AnalysisRun::Unparsed(Box::new(Unparsed {
                        raw: previous,
                        reason: failure.reason,
                        repaired: true,
                        usage,
                    })));
                };
                add_usage(&mut usage, second.usage);
                match self.normalize(
                    &second.text,
                    &index,
                    &model_label,
                    request,
                    usage,
                    &created_at,
                ) {
                    Ok(normalized) => (normalized, true, second.text),
                    Err(second_failure) => {
                        return Ok(AnalysisRun::Unparsed(Box::new(Unparsed {
                            raw: second.text,
                            reason: format!(
                                "{} (the retry also failed: {})",
                                failure.reason, second_failure.reason
                            ),
                            repaired: true,
                            usage,
                        })));
                    }
                }
            }
        };

        // The corrections and the repair flag are part of the answer, so they are
        // stored with it: a reader who opens the analysis tomorrow is told what was
        // fixed just as much as the reader who watched it arrive (FR-4.1). They go in
        // *before* the write, because the stored copy is the one that is read back.
        let mut warnings = normalized.warnings;
        let stored = StoredAnalysis {
            key: request.key.clone(),
            analysis: normalized.analysis.clone(),
            raw: cap_raw(&raw),
            warnings: warnings.clone(),
            repaired,
            stored_at: self.clock.now_unix_secs(),
        };
        // A cache that cannot be written must not lose the analysis the user just paid
        // for: it is reported and the run continues. This warning is about the storage
        // rather than about the answer, so it is deliberately not part of the entry —
        // there is no entry.
        if let Err(error) = self.cache.put(&stored) {
            warnings.push(format!("the analysis could not be cached: {error}"));
        }

        Ok(AnalysisRun::Ready(Box::new(Analyzed {
            analysis: Box::new(normalized.analysis),
            warnings,
            repaired,
            usage,
        })))
    }

    /// The current instant, as the RFC 3339 string the document stores.
    fn now_rfc3339(&self) -> String {
        crate::domain::time::from_unix_secs(
            i64::try_from(self.clock.now_unix_secs()).unwrap_or(i64::MAX),
        )
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// Normalizes one answer, stamping the fields this build owns.
    #[allow(
        clippy::unused_self,
        reason = "kept as a method so the call sites read as one sequence on the use case"
    )]
    fn normalize(
        &self,
        text: &str,
        index: &PathIndex,
        model_label: &str,
        request: &AnalysisRequest,
        usage: Option<TokenUsage>,
        created_at: &str,
    ) -> Result<Normalized, ParseFailure> {
        normalize(
            text,
            index,
            model_label,
            &request.key.head_sha,
            created_at,
            analysis_usage(usage),
        )
    }

    /// One streamed request.
    fn ask(
        &self,
        request: &AnalysisRequest,
        system: &str,
        prompt: &str,
        cancel: &Cancel,
        progress: &mut ProgressHandler<'_>,
    ) -> Result<Answer, LlmError> {
        let mut chat = request.chat.clone();
        chat.system = Some(system.to_owned());
        prompt.clone_into(&mut chat.prompt);
        let mut on_delta: Box<DeltaHandler<'_>> =
            Box::new(|delta: &str| progress(Progress::Delta(delta.to_owned())));
        self.llm
            .stream(&chat, cancel, &mut on_delta)
            .map(|outcome| Answer {
                text: outcome.text,
                usage: outcome.usage,
            })
    }

    /// The stored analysis for a key, if there is one (FR-4.3).
    ///
    /// # Errors
    ///
    /// Returns the cache's error, which the caller shows rather than ignores: a cache
    /// that cannot be read is worth knowing about, and it is not fatal.
    pub fn cached(
        &self,
        key: &AnalysisKey,
    ) -> Result<Option<StoredAnalysis>, crate::ports::analysis::AnalysisCacheError> {
        self.cache.get(key)
    }

    /// An analysis for the same pull request made against an older commit (DEC-15).
    ///
    /// This is what makes "the head moved" sayable: the current key cannot match, so
    /// the entry has to be found by looking at the pull request rather than the key.
    ///
    /// # Errors
    ///
    /// As [`Analyst::cached`].
    pub fn stale(
        &self,
        key: &AnalysisKey,
    ) -> Result<Option<StoredAnalysis>, crate::ports::analysis::AnalysisCacheError> {
        let entries = self.cache.list(self.repo, key.pr)?;
        Ok(entries.into_iter().find(|entry| {
            entry.key.head_sha != key.head_sha
                && entry.key.provider == key.provider
                && entry.key.model == key.model
                && entry.key.thinking == key.thinking
                && entry.key.prompt_version == key.prompt_version
        }))
    }
}

/// One provider answer.
struct Answer {
    text: String,
    usage: Option<TokenUsage>,
}

/// Adds the second attempt's usage to the first, so the numbers are the whole cost.
fn add_usage(total: &mut Option<TokenUsage>, extra: Option<TokenUsage>) {
    match (total.as_mut(), extra) {
        (Some(total), Some(extra)) => {
            total.prompt = total.prompt.saturating_add(extra.prompt);
            total.completion = total.completion.saturating_add(extra.completion);
            total.total = total.total.saturating_add(extra.total);
            total.reasoning = match (total.reasoning, extra.reasoning) {
                (Some(left), Some(right)) => Some(left.saturating_add(right)),
                (left, right) => left.or(right),
            };
        }
        (None, Some(extra)) => *total = Some(extra),
        _ => {}
    }
}

/// Copies the port's usage into the document's shape.
fn analysis_usage(usage: Option<TokenUsage>) -> AnalysisUsage {
    usage.map_or_else(AnalysisUsage::default, |usage| AnalysisUsage {
        prompt: usage.prompt,
        completion: usage.completion,
        reasoning: usage.reasoning,
    })
}

/// Keeps the raw text bounded, saying so when it was cut (FR-4.1).
fn cap_raw(raw: &str) -> String {
    if raw.len() <= crate::domain::analysis::MAX_RAW_BYTES {
        return raw.to_owned();
    }
    let mut end = crate::domain::analysis::MAX_RAW_BYTES;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[the raw answer was truncated at {} for storage]",
        &raw[..end],
        human_bytes(crate::domain::analysis::MAX_RAW_BYTES as u64)
    )
}

/// The metadata block of the bundle (FR-4.6).
#[must_use]
pub fn render_metadata(detail: &PullRequestDetail) -> String {
    use std::fmt::Write;
    let summary = &detail.summary;
    let mut out = String::new();
    let _ = writeln!(out, "# Pull request {}", summary.number);
    let _ = writeln!(out, "title: {}", summary.title);
    let _ = writeln!(out, "author: {}", summary.author);
    let _ = writeln!(out, "state: {}", summary.state.label());
    let _ = writeln!(out, "branches: {} ← {}", summary.base_ref, summary.head_ref);
    let _ = writeln!(
        out,
        "size: +{} -{} across {} file(s)",
        summary.additions, summary.deletions, summary.changed_files
    );
    if !summary.labels.is_empty() {
        let _ = writeln!(out, "labels: {}", summary.labels.join(", "));
    }
    if let Some(decision) = summary.review_decision {
        let _ = writeln!(out, "review decision: {}", decision.label());
    }
    if !detail.reviewers.is_empty() {
        let _ = writeln!(out, "requested reviewers: {}", detail.reviewers.join(", "));
    }
    if summary.is_draft {
        let _ = writeln!(out, "draft: yes");
    }
    if !summary.checks.is_empty() {
        let _ = writeln!(out, "checks: {}", summary.checks.label());
    }
    let body = detail.body.trim();
    if body.is_empty() {
        let _ = writeln!(out, "\ndescription: (the author wrote none)");
    } else {
        let _ = writeln!(out, "\ndescription:\n{body}");
    }
    out
}

/// The commit block of the bundle (FR-4.6).
#[must_use]
pub fn render_commits(detail: &PullRequestDetail) -> String {
    use std::fmt::Write;
    if detail.commits.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "# Commits ({})", detail.commits.len());
    for commit in &detail.commits {
        let short = commit.sha.get(..8).unwrap_or(&commit.sha);
        let _ = writeln!(out, "{short} {} — {}", commit.summary, commit.author);
    }
    out
}

/// What the review screen's analysis panel shows (FR-4.1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PanelModel {
    /// The summary paragraph.
    pub summary: String,
    /// The inferred intent.
    pub intent: String,
    /// The risk areas, most serious first.
    pub risks: Vec<(Severity, String, String)>,
    /// The questions worth asking the author.
    pub questions: Vec<String>,
}

impl PanelModel {
    /// Builds the panel from a document.
    #[must_use]
    pub fn of(analysis: &Analysis) -> Self {
        Self {
            summary: analysis.summary.clone(),
            intent: analysis.intent.clone(),
            risks: analysis
                .risk_areas
                .iter()
                .map(|risk| (risk.severity, risk.title.clone(), risk.why.clone()))
                .collect(),
            questions: analysis.suggested_questions.clone(),
        }
    }

    /// Whether there is anything to draw.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.summary.is_empty()
            && self.intent.is_empty()
            && self.risks.is_empty()
            && self.questions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::analysis::AnalysisUsage;
    use crate::ports::analysis::{AnalysisCacheError, StoredAnalysis};
    use crate::test_support::{FakeClock, FakeLlm, sample_detail};
    use std::collections::BTreeMap;
    use std::fmt::Write as _;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct FakeCache {
        entries: Mutex<BTreeMap<String, StoredAnalysis>>,
    }

    impl AnalysisCachePort for FakeCache {
        fn get(&self, key: &AnalysisKey) -> Result<Option<StoredAnalysis>, AnalysisCacheError> {
            Ok(self
                .entries
                .lock()
                .expect("lock")
                .get(&key.digest())
                .cloned())
        }
        fn list(
            &self,
            _repo: &RepoId,
            _pr: u64,
        ) -> Result<Vec<StoredAnalysis>, AnalysisCacheError> {
            Ok(self
                .entries
                .lock()
                .expect("lock")
                .values()
                .cloned()
                .collect())
        }
        fn put(&self, stored: &StoredAnalysis) -> Result<(), AnalysisCacheError> {
            self.entries
                .lock()
                .expect("lock")
                .insert(stored.key.digest(), stored.clone());
            Ok(())
        }
        fn plan(
            &self,
            _repo: &RepoId,
            _pr: u64,
        ) -> Result<Option<crate::domain::plan::Plan>, AnalysisCacheError> {
            Ok(None)
        }
        fn put_plan(
            &self,
            _repo: &RepoId,
            _pr: u64,
            _plan: &crate::domain::plan::Plan,
        ) -> Result<(), AnalysisCacheError> {
            Ok(())
        }
    }

    fn repo() -> RepoId {
        RepoId::new("github.com", "acme", "service")
    }

    fn key() -> AnalysisKey {
        AnalysisKey {
            repo: repo().key(),
            pr: 141,
            head_sha: "abc123".to_owned(),
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            thinking: None,
            prompt_version: crate::domain::analysis::PROMPT_VERSION,
        }
    }

    fn patch() -> Patch {
        crate::domain::diff::parse_patch(
            "diff --git a/src/money.rs b/src/money.rs\n\
             --- a/src/money.rs\n\
             +++ b/src/money.rs\n\
             @@ -1 +1 @@\n\
             -old\n\
             +new\n",
        )
    }

    const GOOD: &str = r#"{"summary": "Billing rounds half up now.", "intent": "fix a bug",
        "review_plan": [{"order": 1, "group": "domain", "rationale": "rules",
                         "files": ["src/money.rs"]}]}"#;

    fn request() -> AnalysisRequest {
        AnalysisRequest {
            key: key(),
            chat: ChatRequest::new(
                "deepseek",
                "deepseek-v4-pro",
                crate::domain::model::Route::Native(crate::domain::model::NativeBackend::DeepSeek),
                crate::ports::secret::ApiKey::new("sk-test", crate::ports::secret::KeySource::File),
                "placeholder",
            ),
            detail: Box::new(sample_detail()),
            patch: Some(Box::new(patch())),
            checkout: Some(Checkout {
                path: std::path::PathBuf::from("/tmp/ws"),
                head_sha: "abc123".to_owned(),
                base_sha: "base123".to_owned(),
            }),
            policy: BundlePolicy::default(),
        }
    }

    /// The shared fake, with the files this test wants at head.
    fn workspace(files: &[(&str, &str)]) -> crate::test_support::FakeWorkspace {
        let mut workspace =
            crate::test_support::FakeWorkspace::new(crate::ports::workspace::RepoInfo {
                root: Some(std::path::PathBuf::from("/tmp/ws")),
                remotes: Vec::new(),
                default_branch: Some("main".to_owned()),
                git_version: "2.43.0".to_owned(),
            });
        for (path, body) in files {
            workspace
                .files
                .insert(format!("abc123:{path}"), body.as_bytes().to_vec());
            workspace
                .files
                .insert(format!("base123:{path}"), body.as_bytes().to_vec());
        }
        workspace
    }

    fn run(
        answers: &[&str],
        files: &[(&str, &str)],
        request: &AnalysisRequest,
    ) -> (AnalysisRun, FakeCache, Mutex<Vec<Progress>>) {
        let workspace = workspace(files);
        let cache = FakeCache::default();
        let llm = FakeLlm::answering(answers);
        let clock = FakeClock::new(1_767_225_600);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let (bundle, _) = use_case.gather(request, &cancel);
        let progress = Mutex::new(Vec::new());
        let mut handler = |update: Progress| progress.lock().expect("lock").push(update);
        let outcome = use_case
            .run(request, &bundle, &cancel, &mut handler)
            .expect("the fake never fails");
        (outcome, cache, progress)
    }

    #[test]
    fn a_good_answer_is_normalized_stored_and_reported() {
        let request = request();
        let (outcome, cache, progress) =
            run(&[GOOD], &[("src/money.rs", "fn money() {}")], &request);
        let AnalysisRun::Ready(ready) = outcome else {
            panic!("expected a ready analysis, got {outcome:?}");
        };
        assert_eq!(ready.analysis.summary, "Billing rounds half up now.");
        // FR-4.3: the document records the model, the commit and what it cost.
        assert_eq!(ready.analysis.head_sha, "abc123");
        assert_eq!(ready.analysis.model, "deepseek/deepseek-v4-pro");
        assert_eq!(ready.analysis.token_usage.prompt, 10);
        assert_eq!(ready.analysis.token_usage.reasoning, Some(5));
        assert!(!ready.repaired);
        // It was stored under the key the caller asked with.
        assert!(
            cache
                .entries
                .lock()
                .expect("lock")
                .contains_key(&key().digest())
        );
        // And progress reported both the stage and the text.
        let progress = progress.into_inner().expect("lock");
        assert!(
            progress
                .iter()
                .any(|update| matches!(update, Progress::Stage(stage) if stage.contains("asking"))),
            "{progress:?}"
        );
        assert!(
            progress
                .iter()
                .any(|update| matches!(update, Progress::Delta(_))),
            "{progress:?}"
        );
    }

    #[test]
    fn prose_instead_of_json_is_repaired_once_and_then_used() {
        let request = request();
        let (outcome, _, progress) = run(
            &["I could not produce JSON, sorry.", GOOD],
            &[("src/money.rs", "fn money() {}")],
            &request,
        );
        let AnalysisRun::Ready(ready) = outcome else {
            panic!("expected a repaired analysis, got {outcome:?}");
        };
        assert!(ready.repaired, "the panel says a retry happened");
        assert_eq!(ready.analysis.summary, "Billing rounds half up now.");
        // The second attempt's usage is added, not lost.
        assert_eq!(ready.analysis.token_usage.prompt, 20);
        let progress = progress.into_inner().expect("lock");
        assert!(
            progress.iter().any(
                |update| matches!(update, Progress::Stage(stage) if stage.starts_with("repairing"))
            ),
            "{progress:?}"
        );
    }

    #[test]
    fn prose_twice_is_reported_with_the_raw_text_and_both_reasons() {
        let request = request();
        let (outcome, cache, _) = run(
            &["Still prose.", "Also prose."],
            &[("src/money.rs", "fn money() {}")],
            &request,
        );
        let AnalysisRun::Unparsed(unparsed) = outcome else {
            panic!("expected an unparsed answer, got {outcome:?}");
        };
        assert_eq!(unparsed.raw, "Also prose.");
        assert!(
            unparsed.reason.contains("the retry also failed"),
            "{}",
            unparsed.reason
        );
        assert!(unparsed.repaired);
        // Nothing was stored: a failure is not an analysis.
        assert!(cache.entries.lock().expect("lock").is_empty());
    }

    #[test]
    fn the_repair_prompt_carries_the_first_answers_failure() {
        let request = request();
        let workspace = workspace(&[("src/money.rs", "fn money() {}")]);
        let cache = FakeCache::default();
        let llm = FakeLlm::answering(&["not json at all", GOOD]);
        let clock = FakeClock::new(1);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let (bundle, _) = use_case.gather(&request, &cancel);
        let mut handler = |_: Progress| {};
        use_case
            .run(&request, &bundle, &cancel, &mut handler)
            .expect("runs");
        let prompts = llm.prompts.lock().expect("lock");
        assert_eq!(prompts.len(), 2);
        assert!(prompts[1].1.contains("no JSON object"), "{}", prompts[1].1);
        assert!(prompts[1].1.contains("not json at all"), "{}", prompts[1].1);
        // The system prompt is the same one both times.
        assert_eq!(prompts[0].0, prompts[1].0);
    }

    #[test]
    fn the_bundle_carries_the_metadata_the_commits_the_diff_the_files_and_the_conventions() {
        let request = request();
        let workspace = workspace(&[
            ("AGENTS.md", "Use thiserror for errors."),
            ("README.md", "A service."),
            ("src/money.rs", "fn money() {}"),
        ]);
        let cache = FakeCache::default();
        let llm = FakeLlm::answering(&[GOOD]);
        let clock = FakeClock::new(1);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let (bundle, files) = use_case.gather(&request, &cancel);
        assert!(bundle.text.contains("Pull request 141"), "{}", bundle.text);
        assert!(bundle.text.contains("Round half up"), "{}", bundle.text);
        assert!(bundle.text.contains("round half up"), "{}", bundle.text);
        assert!(bundle.text.contains("-old"), "{}", bundle.text);
        assert!(bundle.text.contains("fn money()"), "{}", bundle.text);
        // FR-4.6: the first convention file wins, and only it is sent.
        assert!(bundle.text.contains("Use thiserror"), "{}", bundle.text);
        assert!(!bundle.text.contains("A service."), "{}", bundle.text);
        assert!(
            bundle.segments.iter().any(|segment| segment
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("AGENTS.md was used"))),
            "{:?}",
            bundle.segments
        );
        assert_eq!(
            files.len(),
            1,
            "only the diff's file is read, not AGENTS.md"
        );
    }

    #[test]
    fn fr_4_6_the_final_fake_provider_request_excludes_secret_diff_content() {
        let mut request = request();
        request.patch = Some(Box::new(crate::domain::diff::parse_patch(
            "diff --git a/.env b/.env\n\
             --- a/.env\n\
             +++ b/.env\n\
             @@ -1 +1 @@\n\
             -PROTECTED_OLD_SENTINEL=one\n\
             +PROTECTED_NEW_SENTINEL=two\n\
             diff --git a/src/money.rs b/src/money.rs\n\
             --- a/src/money.rs\n\
             +++ b/src/money.rs\n\
             @@ -1 +1 @@\n\
             -fn old() {}\n\
             +fn allowed_source_sentinel() {}\n",
        )));
        let workspace = workspace(&[
            (".env", "PROTECTED_NEW_SENTINEL=two"),
            ("src/money.rs", "fn allowed_source_sentinel() {}"),
        ]);
        let cache = FakeCache::default();
        let llm = FakeLlm::answering(&[GOOD]);
        let clock = FakeClock::new(1);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let (bundle, _) = use_case.gather(&request, &cancel);
        let mut handler = |_: Progress| {};
        use_case
            .run(&request, &bundle, &cancel, &mut handler)
            .expect("the fake provider answers");

        let prompts = llm.prompts.lock().expect("lock");
        let sent = &prompts[0].1;
        assert!(!sent.contains("PROTECTED_OLD_SENTINEL"), "{sent}");
        assert!(!sent.contains("PROTECTED_NEW_SENTINEL"), "{sent}");
        assert!(sent.contains("allowed_source_sentinel"), "{sent}");
        assert!(
            bundle
                .inspection()
                .iter()
                .any(|line| line.contains("✗ .env"))
        );
    }

    #[test]
    fn fr_4_6_the_reduced_final_provider_request_preserves_exclusions() {
        let mut patch_text = String::from(
            "diff --git a/.env b/.env\n--- a/.env\n+++ b/.env\n@@ -1 +1 @@\n\
             -REDUCED_PROVIDER_OLD_SECRET\n+REDUCED_PROVIDER_NEW_SECRET\n\
             diff --git a/src/money.rs b/src/money.rs\n--- a/src/money.rs\n+++ b/src/money.rs\n\
             @@ -1,300 +1,300 @@\n",
        );
        for index in 0..149 {
            let _ = writeln!(patch_text, " context before {index}");
        }
        patch_text.push_str("-fn old() {}\n+fn reduced_provider_allowed() {}\n");
        for index in 0..149 {
            let _ = writeln!(patch_text, " context after {index}");
        }
        let mut request = request();
        request.patch = Some(Box::new(crate::domain::diff::parse_patch(&patch_text)));
        request.policy.max_context_tokens = 1_000;
        let workspace = workspace(&[
            (".env", "REDUCED_PROVIDER_NEW_SECRET"),
            ("src/money.rs", "fn reduced_provider_allowed() {}"),
        ]);
        let cache = FakeCache::default();
        let llm = FakeLlm::answering(&[GOOD]);
        let clock = FakeClock::new(1);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let (bundle, _) = use_case.gather(&request, &cancel);
        let mut handler = |_: Progress| {};
        use_case
            .run(&request, &bundle, &cancel, &mut handler)
            .expect("the fake provider answers");

        assert!(bundle.segments.iter().any(|segment| {
            segment.kind == crate::domain::context::SegmentKind::Diff && segment.truncated
        }));
        let prompts = llm.prompts.lock().expect("lock");
        let sent = &prompts[0].1;
        assert!(!sent.contains("REDUCED_PROVIDER_OLD_SECRET"), "{sent}");
        assert!(!sent.contains("REDUCED_PROVIDER_NEW_SECRET"), "{sent}");
        assert!(sent.contains("reduced_provider_allowed"), "{sent}");
    }

    #[test]
    fn fr_4_6_a_tracked_ignored_path_is_absent_from_bundle_and_inventory() {
        let request = request();
        let mut workspace = workspace(&[("src/money.rs", "IGNORED_TRACKED_SENTINEL")]);
        workspace.ignored.push("src/money.rs".to_owned());
        let cache = FakeCache::default();
        let llm = FakeLlm::answering(&[GOOD]);
        let clock = FakeClock::new(1);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);

        let (bundle, _) = use_case.gather(&request, &Cancel::new());

        assert!(!bundle.text.contains("IGNORED_TRACKED_SENTINEL"));
        assert!(!bundle.text.contains("-old"), "the ignored diff is absent");
        assert!(
            bundle
                .inspection()
                .iter()
                .any(|line| { line.contains("src/money.rs") && line.contains("ignore rules") })
        );
    }

    #[test]
    fn fr_4_6_a_base_only_ignore_rule_excludes_deleted_content() {
        let mut request = request();
        request.patch = Some(Box::new(crate::domain::diff::parse_patch(
            "diff --git a/tracked.log b/tracked.log\n\
             deleted file mode 100644\n\
             --- a/tracked.log\n\
             +++ /dev/null\n\
             @@ -1 +0,0 @@\n\
             -BASE_ONLY_IGNORED_SENTINEL\n",
        )));
        let mut workspace = workspace(&[("tracked.log", "BASE_ONLY_IGNORED_SENTINEL")]);
        workspace
            .ignored_at
            .insert("base123".to_owned(), vec!["tracked.log".to_owned()]);
        let cache = FakeCache::default();
        let llm = FakeLlm::answering(&[GOOD]);
        let clock = FakeClock::new(1);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);

        let (bundle, _) = use_case.gather(&request, &Cancel::new());

        assert!(!bundle.text.contains("BASE_ONLY_IGNORED_SENTINEL"));
        assert!(
            bundle
                .inspection()
                .iter()
                .any(|line| { line.contains("tracked.log") && line.contains("ignore rules") })
        );
    }

    #[test]
    fn without_a_workspace_the_bundle_says_so_instead_of_pretending() {
        let mut request = request();
        request.checkout = None;
        let (outcome, _, _) = run(&[GOOD], &[("src/money.rs", "fn money() {}")], &request);
        assert!(matches!(outcome, AnalysisRun::Ready(_)));
        let request = request;
        let workspace = workspace(&[("src/money.rs", "fn money() {}")]);
        let cache = FakeCache::default();
        let llm = FakeLlm::answering(&[GOOD]);
        let clock = FakeClock::new(1);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);
        let (bundle, files) = use_case.gather(&request, &Cancel::new());
        assert!(files.is_empty());
        assert!(
            bundle.segments.iter().any(|segment| segment
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("no local workspace"))),
            "{:?}",
            bundle.segments
        );
        assert!(!bundle.text.contains("fn money()"));
    }

    #[test]
    fn a_file_that_cannot_be_read_is_reported_and_the_rest_still_goes() {
        let request = request();
        let (outcome, _, _) = run(&[GOOD], &[], &request);
        assert!(matches!(outcome, AnalysisRun::Ready(_)));
        let workspace = workspace(&[]);
        let cache = FakeCache::default();
        let llm = FakeLlm::answering(&[GOOD]);
        let clock = FakeClock::new(1);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);
        let (bundle, _) = use_case.gather(&request, &Cancel::new());
        assert!(
            bundle.segments.iter().any(|segment| segment
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("could not read src/money.rs"))),
            "{:?}",
            bundle.segments
        );
        // Unknown bytes are fail-closed: without reading them the app cannot verify
        // the size/binary policy that also governs their diff representation.
        assert!(!bundle.text.contains("-old"));
        assert!(bundle.inspection().iter().any(|line| {
            line.contains("src/money.rs") && line.contains("diff content excluded")
        }));
    }

    #[test]
    fn a_cancelled_run_reports_cancellation_rather_than_a_failure() {
        let request = request();
        let workspace = workspace(&[("src/money.rs", "fn money() {}")]);
        let cache = FakeCache::default();
        let llm = FakeLlm::answering(&[GOOD]);
        let clock = FakeClock::new(1);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);
        let cancel = Cancel::new();
        cancel.cancel();
        let (bundle, _) = use_case.gather(&request, &cancel);
        let mut handler = |_: Progress| {};
        let outcome = use_case
            .run(&request, &bundle, &cancel, &mut handler)
            .expect("runs");
        assert!(matches!(outcome, AnalysisRun::Cancelled));
        assert!(cache.entries.lock().expect("lock").is_empty());
    }

    #[test]
    fn the_corrections_are_stored_with_the_document_they_describe() {
        // FR-4.1: the warnings are a property of the answer, not of the run. An
        // analysis read back tomorrow dropped the same invented path, so a cache hit
        // has to report it too — which means the entry that is written must carry
        // them, not just the value the run returns.
        let request = request();
        let (outcome, cache, _) = run(
            &[
                r#"{"summary": "ok", "review_plan": [{"order": 1, "group": "domain",
                 "rationale": "rules", "files": ["src/money.rs", "src/invented.rs"]}]}"#,
            ],
            &[("src/money.rs", "fn money() {}")],
            &request,
        );
        let AnalysisRun::Ready(ready) = outcome else {
            panic!("expected a usable answer, got {outcome:?}");
        };
        assert!(
            ready.warnings.iter().any(|w| w.contains("src/invented.rs")),
            "the run reports it: {:?}",
            ready.warnings
        );

        let entries = cache.entries.lock().expect("lock");
        let stored = entries.values().next().expect("the analysis was stored");
        assert_eq!(
            stored.warnings, ready.warnings,
            "the stored copy must carry the same corrections the run reported"
        );
    }

    #[test]
    fn a_cache_that_cannot_be_written_does_not_lose_the_analysis() {
        #[derive(Debug)]
        struct Unwritable;
        impl AnalysisCachePort for Unwritable {
            fn get(&self, _: &AnalysisKey) -> Result<Option<StoredAnalysis>, AnalysisCacheError> {
                Ok(None)
            }
            fn list(&self, _: &RepoId, _: u64) -> Result<Vec<StoredAnalysis>, AnalysisCacheError> {
                Ok(Vec::new())
            }
            fn put(&self, _: &StoredAnalysis) -> Result<(), AnalysisCacheError> {
                Err(AnalysisCacheError::Io {
                    action: "write".to_owned(),
                    path: "/nowhere".to_owned(),
                    cause: "the disk is full".to_owned(),
                })
            }
            fn plan(
                &self,
                _: &RepoId,
                _: u64,
            ) -> Result<Option<crate::domain::plan::Plan>, AnalysisCacheError> {
                Ok(None)
            }
            fn put_plan(
                &self,
                _: &RepoId,
                _: u64,
                _: &crate::domain::plan::Plan,
            ) -> Result<(), AnalysisCacheError> {
                Ok(())
            }
        }

        let request = request();
        let workspace = workspace(&[("src/money.rs", "fn money() {}")]);
        let cache = Unwritable;
        let llm = FakeLlm::answering(&[GOOD]);
        let clock = FakeClock::new(1);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let (bundle, _) = use_case.gather(&request, &cancel);
        let mut handler = |_: Progress| {};
        let outcome = use_case
            .run(&request, &bundle, &cancel, &mut handler)
            .expect("runs");
        let AnalysisRun::Ready(ready) = outcome else {
            panic!("the analysis survives a broken cache");
        };
        assert!(
            ready
                .warnings
                .iter()
                .any(|warning| warning.contains("could not be cached")),
            "{:?}",
            ready.warnings
        );
    }

    #[test]
    fn the_document_records_what_the_provider_charged_across_both_attempts() {
        let request = request();
        let (outcome, _, _) = run(
            &["prose first", GOOD],
            &[("src/money.rs", "fn money() {}")],
            &request,
        );
        let AnalysisRun::Ready(ready) = outcome else {
            panic!("expected an analysis");
        };
        let usage = ready.usage.expect("usage is reported");
        assert_eq!(usage.prompt, 20);
        assert_eq!(usage.completion, 40);
        assert_eq!(usage.reasoning, Some(10));
    }

    #[test]
    fn the_raw_text_is_capped_before_it_is_stored() {
        let long = format!(
            "{}{}",
            "x".repeat(crate::domain::analysis::MAX_RAW_BYTES + 100),
            ""
        );
        let capped = cap_raw(&long);
        assert!(capped.len() < long.len());
        assert!(capped.contains("was truncated at"), "{capped}");
        // Short text is stored exactly.
        assert_eq!(cap_raw("short"), "short");
    }

    #[test]
    fn the_metadata_block_states_the_facts_the_model_needs() {
        let rendered = render_metadata(&sample_detail());
        for needle in [
            "Pull request 141",
            "title: Round half up",
            "author: someone",
            "branches: main ← rounding",
            "size: +10 -2 across 1 file(s)",
            "labels: bug",
            "Fixes a rounding bug.",
        ] {
            assert!(
                rendered.contains(needle),
                "missing {needle:?} in {rendered}"
            );
        }
        // A description-less pull request says so rather than showing an empty block.
        let mut bare = sample_detail();
        bare.body = "   ".to_owned();
        assert!(render_metadata(&bare).contains("(the author wrote none)"));
    }

    #[test]
    fn the_commit_block_lists_every_commit_oldest_first() {
        let rendered = render_commits(&sample_detail());
        assert!(rendered.contains("# Commits (1)"), "{rendered}");
        assert!(
            rendered.contains("abcdef12 round half up — someone"),
            "{rendered}"
        );
        // No commits means no block at all rather than an empty heading.
        let mut bare = sample_detail();
        bare.commits.clear();
        assert!(render_commits(&bare).is_empty());
    }

    #[test]
    fn the_panel_model_mirrors_the_document() {
        let request = request();
        let (outcome, _, _) = run(
            &[r#"{"summary": "s", "intent": "i",
                 "risk_areas": [{"title": "Rounding", "severity": "high",
                                 "files": ["src/money.rs"], "why": "money"}],
                 "suggested_questions": ["Documented?"],
                 "review_plan": [{"order": 1, "group": "domain", "rationale": "r",
                                  "files": ["src/money.rs"]}]}"#],
            &[("src/money.rs", "fn money() {}")],
            &request,
        );
        let AnalysisRun::Ready(ready) = outcome else {
            panic!("expected an analysis");
        };
        let panel = PanelModel::of(&ready.analysis);
        assert_eq!(panel.summary, "s");
        assert_eq!(panel.risks.len(), 1);
        assert_eq!(panel.risks[0].0, Severity::High);
        assert_eq!(panel.questions, ["Documented?"]);
        assert!(!panel.is_empty());
        assert!(PanelModel::default().is_empty());
    }

    #[test]
    fn a_stale_entry_is_found_by_pull_request_not_by_key() {
        let request = request();
        let workspace = workspace(&[("src/money.rs", "fn money() {}")]);
        let cache = FakeCache::default();
        let llm = FakeLlm::answering(&[GOOD]);
        let clock = FakeClock::new(1);
        let repo = repo();
        let use_case = Analyst::new(&workspace, &cache, &llm, &clock, &repo);
        let cancel = Cancel::new();
        let (bundle, _) = use_case.gather(&request, &cancel);
        let mut handler = |_: Progress| {};
        use_case
            .run(&request, &bundle, &cancel, &mut handler)
            .expect("runs");

        // The same question at a newer commit: the exact key misses...
        let newer = request.key.with_head("def456");
        assert!(use_case.cached(&newer).expect("reads").is_none());
        // ...but the older entry is found and named, which is what DEC-15 needs.
        let stale = use_case
            .stale(&newer)
            .expect("reads")
            .expect("the older analysis is found");
        assert_eq!(stale.key.head_sha, "abc123");
        assert_eq!(stale.analysis.summary, "Billing rounds half up now.");

        // A different model is not a stale version of this one: it is another question.
        let other = AnalysisKey {
            model: "another-model".to_owned(),
            ..newer.clone()
        };
        assert!(use_case.stale(&other).expect("reads").is_none());
    }

    #[test]
    fn a_reasoning_token_count_survives_into_the_document() {
        let request = request();
        let (outcome, _, _) = run(&[GOOD], &[("src/money.rs", "fn money() {}")], &request);
        let AnalysisRun::Ready(ready) = outcome else {
            panic!("expected an analysis");
        };
        assert_eq!(ready.analysis.token_usage.reasoning, Some(5));
        assert_eq!(
            ready.analysis.token_usage,
            AnalysisUsage {
                prompt: 10,
                completion: 20,
                reasoning: Some(5)
            }
        );
    }
}
