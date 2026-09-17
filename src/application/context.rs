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
use crate::ports::workspace::WorkspacePort;

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
    let ChangedContent {
        files,
        mut decisions,
        skipped,
    } = changed_content(workspace, source, &paths, eligibility, cancel);

    let (conventions, convention_label) = match source.checkout() {
        Some(checkout) => conventions(workspace, checkout, cancel),
        None => (Vec::new(), None),
    };

    let mut notes = ignores.notes();
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

    append_added_files(
        workspace,
        source,
        head_eligibility,
        cancel,
        &mut bundle,
        &mut notes,
    );

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
    workspace: &'a dyn WorkspacePort,
    checkout: &'a Checkout,
    cancel: &'a Cancel,
}

fn changed_content(
    workspace: &dyn WorkspacePort,
    source: &dyn ContextSource,
    paths: &[String],
    eligibility: RevisionEligibility<'_>,
    cancel: &Cancel,
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
    let classification = ClassificationContext {
        workspace,
        checkout,
        cancel,
    };
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
    workspace: &dyn WorkspacePort,
    source: &dyn ContextSource,
    eligibility: Eligibility<'_>,
    cancel: &Cancel,
    bundle: &mut Bundle,
    notes: &mut Vec<String>,
) {
    for path in source.added() {
        let Some(checkout) = source.checkout() else {
            notes.push(format!(
                "could not add {path}: there is no local workspace to read it from"
            ));
            continue;
        };
        match workspace.read_file(&checkout.path, &checkout.head_sha, path, cancel) {
            Ok(bytes) => bundle.push_user_file_with_decision(
                path,
                &bytes,
                source.policy(),
                Some(decide_path(path, Some(&bytes), eligibility)),
            ),
            Err(error) => notes.push(format!("could not add {path}: {error}")),
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
    match context
        .workspace
        .read_file(&context.checkout.path, revision, path, context.cancel)
    {
        Ok(bytes) => {
            decisions.push(decide_path(path, Some(&bytes), eligibility));
            Some(bytes)
        }
        Err(error) => {
            skipped.push(format!("{path} at {revision}: {error}"));
            decisions.push(PathDecision::omitted(
                path,
                "content could not be read to verify its size and type",
            ));
            None
        }
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
    workspace: &dyn WorkspacePort,
    checkout: &Checkout,
    cancel: &Cancel,
) -> (Vec<(String, Vec<u8>)>, Option<String>) {
    for name in CONVENTION_FILES {
        let Ok(bytes) = workspace.read_file(&checkout.path, &checkout.head_sha, name, cancel)
        else {
            continue;
        };
        if !bytes.is_empty() {
            return (vec![((*name).to_owned(), bytes)], Some((*name).to_owned()));
        }
    }
    (Vec::new(), None)
}
