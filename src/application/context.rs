//! Gathering the context bundle (FR-4.6, FR-5.3).
//!
//! The bundle an analysis is asked with and the bundle a chat answer is grounded in
//! are the same bundle — FR-5.3 says chat "SHALL use the same deterministic context
//! bundle as analysis" — so this is one function with one caller-facing trait rather
//! than two similar-looking gathers that would drift.
//!
//! What it reads, and in what order, is the requirement's list: the pull request's
//! metadata, its commit messages, the convention file, the diff, the changed files at
//! head, and whatever the user added. It reads through [`WorkspacePort`], so nothing
//! here knows whether the code came from a worktree or somewhere else — the caller
//! says where to look.

use std::collections::BTreeSet;

use crate::domain::context::{
    Bundle, BundleInputs, BundlePolicy, Disposition, PathDecision, Segment, SegmentKind, build,
    disposition,
};
use crate::domain::diff::Patch;
use crate::domain::pr::PullRequestDetail;
use crate::ports::Cancel;
use crate::ports::workspace::{FileReadOutcome, WorkspacePort};

/// The immutable inputs that determine the context a provider can receive (IR-12).
///
/// This is deliberately smaller than a gathered [`Bundle`]: it is cheap to compare
/// before reading repository bytes, while still changing whenever the revision, change
/// set, user additions, or budget policy would produce a materially different bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextIdentity {
    /// The PR revision whose content is read.
    pub head_sha: String,
    /// The base/merge-base revision used for old-side diff content, when local context
    /// is available.
    pub base_sha: Option<String>,
    /// Canonical changed paths in patch order.
    pub changed_paths: Vec<String>,
    /// Extra head-revision files the user explicitly requested.
    pub added: Vec<String>,
    /// The policy controlling which content can fit.
    pub policy: BundlePolicy,
}

/// The complete immutable input to context gathering (IR-12).
///
/// Analysis, chat, inspection and their estimates carry this exact object rather than
/// independently rebuilding a near-identical collection of fields. `identity` is the
/// cheap cache/prepared-bundle comparison; the remaining fields are the source data
/// used only by the worker that resolves the bundle.
#[derive(Debug, Clone)]
pub struct ContextSpec {
    /// The pull request metadata and commits.
    pub detail: Box<PullRequestDetail>,
    /// The canonical parsed change set.
    pub patch: Option<Box<Patch>>,
    /// The revisions and checkout from which source bytes are read.
    pub checkout: Option<Checkout>,
    /// The source-budget policy.
    pub policy: BundlePolicy,
    /// Extra head-revision files selected by the user.
    pub added: Vec<String>,
    /// The complete non-secret identity of these inputs.
    pub identity: ContextIdentity,
}

impl ContextSpec {
    /// The paths the canonical patch changed, in patch order.
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

impl ContextIdentity {
    /// A stable, non-secret fingerprint for cache identity and prepared-bundle checks.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut parts = vec![
            "context-v1".to_owned(),
            self.head_sha.clone(),
            self.base_sha
                .clone()
                .unwrap_or_else(|| "unavailable".to_owned()),
            self.policy.max_context_tokens.to_string(),
            self.policy.max_file_bytes.to_string(),
            self.policy.reduced_context_lines.to_string(),
        ];
        parts.extend(
            self.changed_paths
                .iter()
                .map(|path| format!("changed:{path}")),
        );
        let mut added = self.added.clone();
        added.sort();
        added.dedup();
        parts.extend(added.into_iter().map(|path| format!("added:{path}")));
        fingerprint(&parts.join("\u{1}"))
    }
}

fn fingerprint(text: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    struct TestSource {
        detail: PullRequestDetail,
        patch: Patch,
        checkout: Checkout,
        policy: BundlePolicy,
    }

    impl ContextSource for TestSource {
        fn changed_paths(&self) -> Vec<String> {
            self.patch
                .files
                .iter()
                .filter_map(|file| file.path().map(ToString::to_string))
                .collect()
        }

        fn detail(&self) -> &PullRequestDetail {
            &self.detail
        }

        fn patch(&self) -> Option<&Patch> {
            Some(&self.patch)
        }

        fn checkout(&self) -> Option<&Checkout> {
            Some(&self.checkout)
        }

        fn policy(&self) -> &BundlePolicy {
            &self.policy
        }
    }

    #[test]
    fn ir_12_context_identity_changes_for_additions_revisions_and_budget() {
        let base = ContextIdentity {
            head_sha: "head".to_owned(),
            base_sha: Some("base".to_owned()),
            changed_paths: vec!["src/main.rs".to_owned()],
            added: vec!["docs/design.md".to_owned()],
            policy: BundlePolicy::default(),
        };
        let variants = [
            ContextIdentity {
                added: vec!["docs/other.md".to_owned()],
                ..base.clone()
            },
            ContextIdentity {
                base_sha: Some("other-base".to_owned()),
                ..base.clone()
            },
            ContextIdentity {
                policy: BundlePolicy {
                    max_context_tokens: 1,
                    ..BundlePolicy::default()
                },
                ..base.clone()
            },
        ];
        for variant in variants {
            assert_ne!(base.fingerprint(), variant.fingerprint());
        }
    }

    #[test]
    fn ir_17_context_reads_one_bounded_batch_per_revision() {
        let source = TestSource {
            detail: crate::test_support::sample_detail(),
            patch: crate::domain::diff::parse_patch(
                "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-a\n+b\n\
                 diff --git a/src/b.rs b/src/b.rs\n--- a/src/b.rs\n+++ b/src/b.rs\n@@ -1 +1 @@\n-c\n+d\n",
            ),
            checkout: Checkout {
                path: std::path::PathBuf::from("/fake"),
                head_sha: "head".to_owned(),
                base_sha: "base".to_owned(),
            },
            policy: BundlePolicy::default(),
        };
        let workspace = crate::test_support::FakeWorkspace::default();
        let ignores = RevisionIgnores::default();
        let _reads = batch_reads(
            &workspace,
            &source,
            ignores.eligibility(&source.policy),
            &Cancel::new(),
        );
        assert_eq!(
            workspace.calls(),
            vec!["read_files", "read_files"],
            "file count must not become process count"
        );
    }

    #[test]
    #[ignore = "opt-in IR-17 reference workload; run in release mode"]
    fn ir_17_reference_batched_context_workload() {
        let mut patch_text = String::new();
        let mut workspace = crate::test_support::FakeWorkspace::default();
        for index in 0..400 {
            let path = format!("src/file-{index:03}.rs");
            let _ = std::fmt::Write::write_fmt(
                &mut patch_text,
                format_args!(
                    "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old {index}\n+new {index}\n"
                ),
            );
            let bytes = if index % 50 == 0 {
                vec![b'x'; 48 * 1024]
            } else if index % 75 == 0 {
                vec![0, 1, 2, 3]
            } else {
                format!("pub fn item_{index}() {{}}\n").into_bytes()
            };
            workspace
                .files
                .insert(format!("base:{path}"), bytes.clone());
            workspace.files.insert(format!("head:{path}"), bytes);
        }
        let source = TestSource {
            detail: crate::test_support::sample_detail(),
            patch: crate::domain::diff::parse_patch(&patch_text),
            checkout: Checkout {
                path: std::path::PathBuf::from("/fake"),
                head_sha: "head".to_owned(),
                base_sha: "base".to_owned(),
            },
            policy: BundlePolicy {
                max_context_tokens: 10_000,
                max_file_bytes: 32 * 1024,
                reduced_context_lines: 1,
            },
        };
        let started = std::time::Instant::now();
        let gathered = gather(&workspace, &source, &Cancel::new());
        let elapsed = started.elapsed();
        let calls = workspace.calls();
        let batches = calls.iter().filter(|call| *call == "read_files").count();
        let expected_git_processes = batches.saturating_mul(2);
        let retained_source_bytes = gathered
            .files
            .iter()
            .map(|(_, bytes)| bytes.len())
            .sum::<usize>();
        eprintln!(
            "IR17_METRIC profile={} workload=batched_context files=400 batch_operations={batches} expected_git_processes={expected_git_processes} retained_source_bytes={retained_source_bytes} bundle_bytes={} segments={} duration_us={}",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            gathered.bundle.bytes(),
            gathered.bundle.segments.len(),
            elapsed.as_micros(),
        );
        assert_eq!(batches, 2, "one batch per represented revision");
        assert!(retained_source_bytes <= source.policy.max_bytes().saturating_mul(2));
    }
}

/// The convention files, in the priority the requirements give them (FR-4.6).
///
/// The first one that exists is *the* conventions file: a repository that has an
/// `AGENTS.md` has said what it wants reviewers told, and appending its README to that
/// would dilute the instruction it wrote.
pub const CONVENTION_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md", "README.md"];

/// Where the pull request's files can be read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// A directory in which the head commit exists, usually the managed worktree.
    pub path: std::path::PathBuf,
    /// The commit to read from.
    pub head_sha: String,
    /// The merge-base commit used by the review diff.
    pub base_sha: String,
}

/// Everything a bundle is gathered from.
///
/// Implemented by the analysis request and the chat request, which are otherwise
/// different things: what this trait asks for is exactly what gathering needs, and
/// nothing about the question being asked.
pub trait ContextSource {
    /// The paths the diff changed, in the diff's own order.
    fn changed_paths(&self) -> Vec<String>;
    /// The pull request, for the metadata and commit blocks.
    fn detail(&self) -> &PullRequestDetail;
    /// The diff, already parsed. `None` when it could not be read.
    fn patch(&self) -> Option<&Patch>;
    /// Where the files can be read, when a worktree exists (FR-3.1).
    fn checkout(&self) -> Option<&Checkout>;
    /// The bundle budget (FR-4.6).
    fn policy(&self) -> &BundlePolicy;
    /// Files the user added with `:context add` (FR-5.3).
    fn added(&self) -> &[String] {
        &[]
    }
}

impl ContextSource for ContextSpec {
    fn changed_paths(&self) -> Vec<String> {
        self.changed_paths()
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

    fn added(&self) -> &[String] {
        &self.added
    }
}

/// A gathered bundle, and the files that went into it.
#[derive(Debug, Clone)]
pub struct Gathered {
    /// The bundle.
    pub bundle: Bundle,
    /// Each changed file's bytes, as read, in diff order.
    pub files: Vec<(String, Vec<u8>)>,
}

/// Reads everything the bundle needs (FR-4.6).
///
/// Never fails: a file that cannot be read is one elided file, recorded as a segment
/// and reported by `:context`, which is more useful than refusing to ask anything.
#[must_use]
pub fn gather(
    workspace: &dyn WorkspacePort,
    source: &dyn ContextSource,
    cancel: &Cancel,
) -> Gathered {
    let paths = candidate_paths(source);
    let ignores = revision_ignores(workspace, source.checkout(), &paths, cancel);
    let eligibility = ignores.eligibility(source.policy());
    let head_eligibility = eligibility.head;
    let reads = batch_reads(workspace, source, eligibility, cancel);
    let ChangedContent {
        files,
        mut decisions,
        skipped,
    } = changed_content(source, &paths, eligibility, &reads);

    let (conventions, convention_label) = match source.checkout() {
        Some(checkout) => conventions(checkout, &reads, head_eligibility),
        None => (Vec::new(), None),
    };

    let mut notes = ignores.notes();
    notes.extend(reads.errors.iter().cloned());
    if source.checkout().is_none() {
        notes.push(
            "no local workspace: the changed files' contents are not in the bundle. \
             `:workspace` or the review screen's local diff creates one"
                .to_owned(),
        );
    }
    for missing in &skipped {
        notes.push(format!("could not read {missing}"));
    }
    if let Some(label) = &convention_label {
        let others: Vec<&&str> = CONVENTION_FILES
            .iter()
            .filter(|name| **name != label.as_str())
            .collect();
        notes.push(format!(
            "{label} was used as the repository's conventions; {} did not override it",
            others
                .iter()
                .map(|name| **name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let metadata = crate::application::analysis::render_metadata(source.detail());
    let commits = crate::application::analysis::render_commits(source.detail());
    for (label, bytes) in &conventions {
        decisions.push(decide_path(label, Some(bytes), head_eligibility));
    }
    let inputs = BundleInputs {
        metadata: &metadata,
        commits: &commits,
        conventions: conventions
            .iter()
            .map(|(label, bytes)| (label.as_str(), bytes.as_slice()))
            .collect(),
        diff: source.patch(),
        files: files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
            .collect(),
        decisions,
    };
    let mut bundle = build(&inputs, source.policy());

    append_added_files(source, head_eligibility, &reads, &mut bundle, &mut notes);

    for note in notes {
        bundle.segments.push(Segment {
            kind: SegmentKind::Metadata,
            label: "note".to_owned(),
            bytes: 0,
            included: false,
            truncated: false,
            detail: Some(note),
        });
    }
    Gathered { bundle, files }
}

#[derive(Clone, Copy)]
struct Eligibility<'a> {
    ignored: &'a BTreeSet<String>,
    error: Option<&'a str>,
    policy: &'a BundlePolicy,
}

#[derive(Clone, Copy)]
struct RevisionEligibility<'a> {
    base: Eligibility<'a>,
    head: Eligibility<'a>,
}

#[derive(Default)]
struct RevisionIgnores {
    base: BTreeSet<String>,
    head: BTreeSet<String>,
    base_error: Option<String>,
    head_error: Option<String>,
}

impl RevisionIgnores {
    fn eligibility<'a>(&'a self, policy: &'a BundlePolicy) -> RevisionEligibility<'a> {
        RevisionEligibility {
            base: Eligibility {
                ignored: &self.base,
                error: self.base_error.as_deref(),
                policy,
            },
            head: Eligibility {
                ignored: &self.head,
                error: self.head_error.as_deref(),
                policy,
            },
        }
    }

    fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if let Some(error) = &self.base_error {
            notes.push(format!(
                "repository ignore rules at the base revision could not be checked ({error}); affected content was not sent"
            ));
        }
        if let Some(error) = &self.head_error {
            notes.push(format!(
                "repository ignore rules at the head revision could not be checked ({error}); affected content was not sent"
            ));
        }
        notes
    }
}

struct ChangedContent {
    files: Vec<(String, Vec<u8>)>,
    decisions: Vec<PathDecision>,
    skipped: Vec<String>,
}

#[derive(Clone, Copy)]
struct ClassificationContext<'a> {
    reads: &'a BatchReads,
}

#[derive(Default)]
struct BatchReads {
    values: std::collections::BTreeMap<(String, String), FileReadOutcome>,
    errors: Vec<String>,
}

impl BatchReads {
    fn get(&self, revision: &str, path: &str) -> Option<&FileReadOutcome> {
        self.values.get(&(revision.to_owned(), path.to_owned()))
    }
}

fn batch_reads(
    workspace: &dyn WorkspacePort,
    source: &dyn ContextSource,
    eligibility: RevisionEligibility<'_>,
    cancel: &Cancel,
) -> BatchReads {
    let Some(checkout) = source.checkout() else {
        return BatchReads::default();
    };
    let mut base = Vec::new();
    let mut head = Vec::new();
    let mut base_seen = BTreeSet::new();
    let mut head_seen = BTreeSet::new();
    for path in CONVENTION_FILES {
        push_unique(&mut head, &mut head_seen, (*path).to_owned());
    }
    if let Some(patch) = source.patch() {
        for file in &patch.files {
            if let Some(path) = &file.old_path {
                push_unique(&mut base, &mut base_seen, path.to_string());
            }
            if let Some(path) = &file.new_path {
                push_unique(&mut head, &mut head_seen, path.to_string());
            }
        }
    } else {
        for path in source.changed_paths() {
            push_unique(&mut head, &mut head_seen, path);
        }
    }
    for path in source.added() {
        push_unique(&mut head, &mut head_seen, path.clone());
    }
    base.retain(|path| representation_is_readable(path, eligibility.base));
    head.retain(|path| representation_is_readable(path, eligibility.head));

    let mut reads = BatchReads::default();
    read_revision_batch(
        workspace,
        checkout,
        &checkout.base_sha,
        &base,
        source.policy(),
        cancel,
        &mut reads,
    );
    if !cancel.is_cancelled() {
        read_revision_batch(
            workspace,
            checkout,
            &checkout.head_sha,
            &head,
            source.policy(),
            cancel,
            &mut reads,
        );
    }
    reads
}

fn push_unique(paths: &mut Vec<String>, seen: &mut BTreeSet<String>, path: String) {
    if seen.insert(path.clone()) {
        paths.push(path);
    }
}

fn representation_is_readable(path: &str, eligibility: Eligibility<'_>) -> bool {
    eligibility.error.is_none()
        && !eligibility.ignored.contains(path)
        && !crate::domain::context::is_secret_path(path)
}

fn read_revision_batch(
    workspace: &dyn WorkspacePort,
    checkout: &Checkout,
    revision: &str,
    paths: &[String],
    policy: &BundlePolicy,
    cancel: &Cancel,
    reads: &mut BatchReads,
) {
    match workspace.read_files(
        &checkout.path,
        revision,
        paths,
        policy.max_file_bytes,
        policy.max_bytes(),
        cancel,
    ) {
        Ok(results) => {
            for result in results {
                reads
                    .values
                    .insert((revision.to_owned(), result.path), result.outcome);
            }
        }
        Err(error) => reads.errors.push(format!(
            "could not batch-read source objects at {revision}: {error}"
        )),
    }
}

fn changed_content(
    source: &dyn ContextSource,
    paths: &[String],
    eligibility: RevisionEligibility<'_>,
    reads: &BatchReads,
) -> ChangedContent {
    let mut content = ChangedContent {
        files: Vec::new(),
        decisions: Vec::new(),
        skipped: Vec::new(),
    };
    let Some(checkout) = source.checkout() else {
        content.decisions.extend(paths.iter().map(|path| {
            PathDecision::omitted(
                path,
                "content eligibility cannot be verified without a local workspace",
            )
        }));
        return content;
    };
    let classification = ClassificationContext { reads };
    if let Some(patch) = source.patch() {
        for file in &patch.files {
            if let Some(path) = &file.old_path {
                classify_representation(
                    classification,
                    eligibility.base,
                    &path.to_string(),
                    &checkout.base_sha,
                    &mut content.decisions,
                    &mut content.skipped,
                );
            }
            if let Some(path) = &file.new_path {
                let path = path.to_string();
                let bytes = classify_representation(
                    classification,
                    eligibility.head,
                    &path,
                    &checkout.head_sha,
                    &mut content.decisions,
                    &mut content.skipped,
                );
                if let Some(bytes) = bytes {
                    content.files.push((path, bytes));
                }
            }
        }
    } else {
        for path in source.changed_paths() {
            let bytes = classify_representation(
                classification,
                eligibility.head,
                &path,
                &checkout.head_sha,
                &mut content.decisions,
                &mut content.skipped,
            );
            if let Some(bytes) = bytes {
                content.files.push((path, bytes));
            }
        }
    }
    content
}

fn append_added_files(
    source: &dyn ContextSource,
    eligibility: Eligibility<'_>,
    reads: &BatchReads,
    bundle: &mut Bundle,
    notes: &mut Vec<String>,
) {
    for path in source.added() {
        if eligibility.error.is_some()
            || eligibility.ignored.contains(path)
            || crate::domain::context::is_secret_path(path)
        {
            bundle.push_user_file_with_decision(
                path,
                &[],
                source.policy(),
                Some(decide_path(path, None, eligibility)),
            );
            continue;
        }
        let Some(checkout) = source.checkout() else {
            notes.push(format!(
                "could not add {path}: there is no local workspace to read it from"
            ));
            continue;
        };
        match reads.get(&checkout.head_sha, path) {
            Some(FileReadOutcome::Content(bytes)) => bundle.push_user_file_with_decision(
                path,
                bytes,
                source.policy(),
                Some(decide_path(path, Some(bytes), eligibility)),
            ),
            Some(other) => notes.push(format!(
                "could not add {path}: {}",
                read_failure(other, source.policy())
            )),
            None => notes.push(format!("could not add {path}: source object was not read")),
        }
    }
}

/// Every path whose content could enter the bundle, deduplicated for one ignore check.
fn candidate_paths(source: &dyn ContextSource) -> Vec<String> {
    let mut paths = BTreeSet::new();
    if let Some(patch) = source.patch() {
        for file in &patch.files {
            paths.extend(file.old_path.iter().map(ToString::to_string));
            paths.extend(file.new_path.iter().map(ToString::to_string));
        }
    } else {
        paths.extend(source.changed_paths());
    }
    paths.extend(CONVENTION_FILES.iter().map(|path| (*path).to_owned()));
    paths.extend(source.added().iter().cloned());
    paths.into_iter().collect()
}

fn ignored_paths(
    workspace: &dyn WorkspacePort,
    checkout: Option<&Checkout>,
    revision: Option<&str>,
    paths: &[String],
    cancel: &Cancel,
) -> (BTreeSet<String>, Option<String>) {
    let (Some(checkout), Some(revision)) = (checkout, revision) else {
        return (BTreeSet::new(), None);
    };
    match workspace.ignored_paths(&checkout.path, revision, paths, cancel) {
        Ok(paths) => (paths.into_iter().collect(), None),
        Err(error) => (BTreeSet::new(), Some(error.to_string())),
    }
}

fn revision_ignores(
    workspace: &dyn WorkspacePort,
    checkout: Option<&Checkout>,
    paths: &[String],
    cancel: &Cancel,
) -> RevisionIgnores {
    let Some(checkout) = checkout else {
        return RevisionIgnores::default();
    };
    let (base, base_error) = ignored_paths(
        workspace,
        Some(checkout),
        Some(&checkout.base_sha),
        paths,
        cancel,
    );
    let (head, head_error) = ignored_paths(
        workspace,
        Some(checkout),
        Some(&checkout.head_sha),
        paths,
        cancel,
    );
    RevisionIgnores {
        base,
        head,
        base_error,
        head_error,
    }
}

/// Reads and classifies exactly one old/new representation. A read failure is a
/// fail-closed content decision: the diff still exists locally, but unknown bytes are
/// not copied to a provider merely because their size/type could not be checked.
fn classify_representation(
    context: ClassificationContext<'_>,
    eligibility: Eligibility<'_>,
    path: &str,
    revision: &str,
    decisions: &mut Vec<PathDecision>,
    skipped: &mut Vec<String>,
) -> Option<Vec<u8>> {
    if eligibility.error.is_some()
        || eligibility.ignored.contains(path)
        || crate::domain::context::is_secret_path(path)
    {
        decisions.push(decide_path(path, None, eligibility));
        return None;
    }
    match context.reads.get(revision, path) {
        Some(FileReadOutcome::Content(bytes)) => {
            decisions.push(decide_path(path, Some(bytes), eligibility));
            Some(bytes.clone())
        }
        Some(FileReadOutcome::Oversize { bytes }) => {
            decisions.push(PathDecision {
                path: path.to_owned(),
                disposition: Disposition::Placeholder {
                    reason: format!(
                        "{} over the {} KiB per-file limit",
                        crate::domain::context::human_bytes(*bytes),
                        eligibility.policy.max_file_bytes / 1024
                    ),
                },
            });
            None
        }
        Some(other) => {
            skipped.push(format!(
                "{path} at {revision}: {}",
                read_failure(other, eligibility.policy)
            ));
            decisions.push(PathDecision::omitted(
                path,
                "content could not be read to verify its size and type",
            ));
            None
        }
        None => {
            skipped.push(format!("{path} at {revision}: source object was not read"));
            decisions.push(PathDecision::omitted(
                path,
                "content could not be read to verify its size and type",
            ));
            None
        }
    }
}

fn read_failure(outcome: &FileReadOutcome, policy: &BundlePolicy) -> String {
    match outcome {
        FileReadOutcome::Missing => "the path does not exist at this revision".to_owned(),
        FileReadOutcome::Oversize { bytes } => format!(
            "{} exceeds the {} KiB per-file limit",
            crate::domain::context::human_bytes(*bytes),
            policy.max_file_bytes / 1024
        ),
        FileReadOutcome::BudgetExceeded { bytes } => format!(
            "{} was not retained because the bounded source-read budget was full",
            crate::domain::context::human_bytes(*bytes)
        ),
        FileReadOutcome::NotBlob { kind } => format!("Git object is {kind}, not a file blob"),
        FileReadOutcome::Unreadable { reason } => reason.clone(),
        FileReadOutcome::Content(_) => "source object is available".to_owned(),
    }
}

fn decide_path(path: &str, bytes: Option<&[u8]>, eligibility: Eligibility<'_>) -> PathDecision {
    let disposition = if crate::domain::context::is_secret_path(path) {
        disposition(path, &[], eligibility.policy)
    } else if eligibility.error.is_some() {
        Disposition::Omit {
            reason: "repository ignore eligibility could not be verified".to_owned(),
        }
    } else if eligibility.ignored.contains(path) {
        Disposition::Omit {
            reason: "this path matches repository ignore rules, so it is never sent".to_owned(),
        }
    } else if let Some(bytes) = bytes {
        disposition(path, bytes, eligibility.policy)
    } else {
        Disposition::Include
    };
    PathDecision {
        path: path.to_owned(),
        disposition,
    }
}

/// Reads the first convention file that exists (FR-4.6).
fn conventions(
    checkout: &Checkout,
    reads: &BatchReads,
    eligibility: Eligibility<'_>,
) -> (Vec<(String, Vec<u8>)>, Option<String>) {
    for name in CONVENTION_FILES {
        if eligibility.error.is_some() || eligibility.ignored.contains(*name) {
            return (
                vec![((*name).to_owned(), Vec::new())],
                Some((*name).to_owned()),
            );
        }
        let Some(FileReadOutcome::Content(bytes)) = reads.get(&checkout.head_sha, name) else {
            continue;
        };
        if !bytes.is_empty() {
            return (
                vec![((*name).to_owned(), bytes.clone())],
                Some((*name).to_owned()),
            );
        }
    }
    (Vec::new(), None)
}
