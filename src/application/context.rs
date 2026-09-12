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

use crate::domain::context::{Bundle, BundleInputs, BundlePolicy, Segment, SegmentKind, build};
use crate::domain::diff::Patch;
use crate::domain::pr::PullRequestDetail;
use crate::ports::Cancel;
use crate::ports::workspace::WorkspacePort;

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
    let paths = source.changed_paths();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    if let Some(checkout) = source.checkout() {
        for path in &paths {
            match workspace.read_file(&checkout.path, &checkout.head_sha, path, cancel) {
                Ok(bytes) => files.push((path.clone(), bytes)),
                // A file that cannot be read is not a reason to fail the whole
                // request: it is one elided file, and the bundle says so.
                Err(error) => skipped.push(format!("{path}: {error}")),
            }
        }
    }

    let (conventions, convention_label) = match source.checkout() {
        Some(checkout) => conventions(workspace, checkout, cancel),
        None => (Vec::new(), None),
    };

    let mut notes = Vec::new();
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
    };
    let mut bundle = build(&inputs, source.policy());

    // The user's own additions, read from the same checkout. A path that cannot be
    // read is reported exactly like a changed file that could not be read.
    for path in source.added() {
        let Some(checkout) = source.checkout() else {
            notes.push(format!(
                "could not add {path}: there is no local workspace to read it from"
            ));
            continue;
        };
        match workspace.read_file(&checkout.path, &checkout.head_sha, path, cancel) {
            Ok(bytes) => bundle.push_user_file(path, &bytes, source.policy()),
            Err(error) => notes.push(format!("could not add {path}: {error}")),
        }
    }

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
