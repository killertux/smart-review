//! The review plan and the order files are read in (FR-3.5, FR-4.2, DEC-10).
//!
//! Two orders exist for the same diff: the path order the patch arrives in, and the
//! order a reviewer would choose. The second one comes from the analysis when there is
//! one, and from path rules when there is not — so the ordered view is never simply
//! absent (FR-3.5, DEC-10).
//!
//! Three rules make this safe to build a view on:
//!
//! - **a plan is a view, never a filter.** Whatever the plan says, the ordered view
//!   contains every changed file exactly once; the analysis's own grouping is
//!   normalized (in [`crate::domain::analysis`]) before it gets here.
//! - **the user wins.** A group moved by hand stays moved: the override is kept with
//!   the analysis, `overridden` records that it happened, and re-deriving the plan from
//!   the same analysis never silently discards it (FR-4.2).
//! - **an override is bound to exact file changes.** A new head keeps it only when all
//!   file-change fingerprints still match; otherwise the derived plan wins and the UI
//!   explains the reset.
//!
//! Pure: no IO or terminal knowledge; the domain diff is its only change input
//! (NFR-5.2).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::domain::analysis::{Analysis, PlanGroup, UNCLASSIFIED};

/// Which order the review screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderMode {
    /// The order the analysis recommends, or the heuristic order when there is no
    /// analysis: what the rest depends on first (FR-4.2).
    #[default]
    Recommended,
    /// The order the patch lists the files in.
    Path,
}

impl OrderMode {
    /// The word shown in the header, so the toggle is never a mystery (FR-3.5).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Recommended => "recommended order",
            Self::Path => "path order",
        }
    }

    /// The other mode.
    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            Self::Recommended => Self::Path,
            Self::Path => Self::Recommended,
        }
    }
}

/// Where a plan came from, which the panel states rather than implying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanSource {
    /// The model's review plan.
    Analysis,
    /// Path rules, because there is no analysis (FR-3.5's fallback).
    Heuristic,
}

/// Human review progress for one changed file (IR-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    /// The reviewer has not explicitly completed this file.
    #[default]
    NotReviewed,
    /// The reviewer explicitly completed this exact file change.
    Reviewed,
    /// The reviewer wants to return, or the previously reviewed change moved.
    NeedsRevisit,
}

impl ReviewStatus {
    /// Compact marker shown beside a file.
    #[must_use]
    pub const fn marker(self) -> &'static str {
        match self {
            Self::NotReviewed => "□",
            Self::Reviewed => "✓",
            Self::NeedsRevisit => "!",
        }
    }

    /// Human-readable status.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotReviewed => "not reviewed",
            Self::Reviewed => "reviewed",
            Self::NeedsRevisit => "needs revisit",
        }
    }

    /// The next explicit state for the default `m` action.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::NotReviewed => Self::Reviewed,
            Self::Reviewed => Self::NeedsRevisit,
            Self::NeedsRevisit => Self::NotReviewed,
        }
    }
}

/// Durable human state bound to an exact file change (IR-16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReview {
    /// Canonical changed-file path.
    pub path: String,
    /// Stable digest of the file's changed content and coordinates.
    pub fingerprint: String,
    /// Explicit human progress; never inferred from AI or cursor movement.
    #[serde(default)]
    pub status: ReviewStatus,
}

impl PlanSource {
    /// The word shown next to the order label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Analysis => "from the analysis",
            Self::Heuristic => "by path rules (no analysis yet)",
        }
    }
}

/// The layers a heuristic plan sorts into, in the order they are read.
///
/// Derived from the requirements' example shape for a DDD-style repository
/// (`domain → application → infrastructure → interfaces/config → tests/docs`, FR-4.2)
/// and applied only as a fallback: an analysis that disagrees with these rules is
/// followed, not corrected.
const LAYERS: &[Layer] = &[
    Layer {
        name: "domain",
        rationale: "the rules this change is about, which everything else depends on",
        prefixes: &[
            "src/domain/",
            "src/model/",
            "src/models/",
            "src/core/",
            "src/entities/",
        ],
        contains: &[],
    },
    Layer {
        name: "application",
        rationale: "the use cases that put the rules to work",
        prefixes: &[
            "src/application/",
            "src/services/",
            "src/service/",
            "src/usecases/",
            "src/use_cases/",
            "src/app/",
        ],
        contains: &[],
    },
    Layer {
        name: "infrastructure",
        rationale: "the outside world: storage, network and other people's systems",
        prefixes: &[
            "src/adapters/",
            "src/infrastructure/",
            "src/infra/",
            "src/persistence/",
            "src/repositories/",
            "migrations/",
            "terraform/",
            "helm/",
            "k8s/",
            "deploy/",
            "docker/",
        ],
        contains: &[],
    },
    Layer {
        name: "interfaces",
        rationale: "how the change is reached: entry points and their wiring",
        prefixes: &[
            "src/api/",
            "src/routes/",
            "src/handlers/",
            "src/http/",
            "src/cli/",
            "src/bin/",
            "src/tui/",
            "src/ui/",
            "cmd/",
        ],
        contains: &["src/main.rs", "src/lib.rs"],
    },
    Layer {
        name: "config",
        rationale: "build and runtime configuration: read after the code it configures",
        prefixes: &[".github/", "ci/"],
        contains: &[
            "Cargo.toml",
            "package.json",
            "Makefile",
            "justfile",
            "Dockerfile",
            "go.mod",
            "pyproject.toml",
        ],
    },
    Layer {
        name: "tests",
        rationale: "the tests, read once the behaviour they pin down is understood",
        prefixes: &["tests/", "test/", "spec/", "testdata/", "benches/"],
        contains: &[],
    },
    Layer {
        name: "docs",
        rationale: "the prose, which states the intent and needs no code review",
        prefixes: &["docs/", "doc/", "documentation/"],
        contains: &[
            "README.md",
            "CHANGELOG.md",
            "CONTRIBUTING.md",
            "LICENSE",
            "AGENTS.md",
            "CLAUDE.md",
        ],
    },
];

/// One heuristic layer.
struct Layer {
    name: &'static str,
    rationale: &'static str,
    prefixes: &'static [&'static str],
    contains: &'static [&'static str],
}

/// Extensions that make a file configuration, whatever directory it is in.
const CONFIG_SUFFIXES: &[&str] = &[
    ".toml",
    ".yaml",
    ".yml",
    ".json",
    ".ini",
    ".cfg",
    ".conf",
    ".properties",
    ".tf",
    ".tfvars",
];

/// Extensions that make a file documentation.
const DOC_SUFFIXES: &[&str] = &[".md", ".mdx", ".rst", ".adoc", ".txt"];

/// The review plan, effective and persisted per pull request (FR-4.2, FR-4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// Monotonic durable-document revision used to reject stale cross-process writes.
    #[serde(default)]
    pub document_revision: u64,
    /// The head this plan describes. A plan for another head is not applied.
    pub head_sha: String,
    /// Where the grouping came from.
    pub source: PlanSource,
    /// The groups, in reading order.
    pub groups: Vec<PlanGroup>,
    /// Whether the user has moved anything by hand.
    #[serde(default)]
    pub overridden: bool,
    /// Human review markers, including the fingerprint that makes carryover safe.
    #[serde(default)]
    pub file_reviews: Vec<FileReview>,
    /// Whether a manual order could not safely be carried to this revision.
    #[serde(default)]
    pub override_invalidated: bool,
}

impl Plan {
    /// The plan the analysis asks for.
    #[must_use]
    pub fn from_analysis(analysis: &Analysis) -> Self {
        Self {
            document_revision: 0,
            head_sha: analysis.head_sha.clone(),
            source: PlanSource::Analysis,
            groups: analysis.review_plan.clone(),
            overridden: false,
            file_reviews: Vec::new(),
            override_invalidated: false,
        }
    }

    /// A plan from path rules, used when there is no analysis (FR-3.5, DEC-10).
    ///
    /// Every path lands in exactly one layer, and a path that matches nothing is
    /// `unclassified` — a name the user can act on rather than an absence.
    #[must_use]
    pub fn heuristic(head_sha: &str, paths: &[String]) -> Self {
        let mut groups: Vec<PlanGroup> = LAYERS
            .iter()
            .map(|layer| PlanGroup {
                order: 0,
                group: layer.name.to_owned(),
                rationale: layer.rationale.to_owned(),
                files: Vec::new(),
            })
            .collect();
        let mut unclassified: Vec<String> = Vec::new();

        // Sorted first so a layer's files are in path order regardless of the order the
        // diff happened to list them in.
        let mut sorted: Vec<&String> = paths.iter().collect();
        sorted.sort();
        sorted.dedup();
        for path in sorted {
            match layer_of(path) {
                Some(name) => {
                    if let Some(group) = groups.iter_mut().find(|group| group.group == name) {
                        group.files.push(path.clone());
                    }
                }
                None => unclassified.push(path.clone()),
            }
        }

        groups.retain(|group| !group.files.is_empty());
        if !unclassified.is_empty() {
            groups.push(PlanGroup {
                order: 0,
                group: UNCLASSIFIED.to_owned(),
                rationale: "no path rule matches these; they are read last, in path order"
                    .to_owned(),
                files: unclassified,
            });
        }
        for (position, group) in groups.iter_mut().enumerate() {
            group.order = u32::try_from(position + 1).unwrap_or(u32::MAX);
        }
        Self {
            document_revision: 0,
            head_sha: head_sha.to_owned(),
            source: PlanSource::Heuristic,
            groups,
            overridden: false,
            file_reviews: Vec::new(),
            override_invalidated: false,
        }
    }

    /// The files, in the order the given mode reads them.
    ///
    /// This is what the ordered view iterates, and it is guaranteed to contain each
    /// file once: the groups come from a normalized plan, and the caller passes the
    /// diff's own path list so nothing can be lost between the two.
    #[must_use]
    pub fn files(&self, mode: OrderMode, path_order: &[String]) -> Vec<String> {
        self.effective_order(mode, path_order)
            .into_iter()
            .filter_map(|index| path_order.get(index).cloned())
            .collect()
    }

    /// Stable indexes into `canonical_paths`, in the order a reader should see them.
    ///
    /// A path is presentation data, not a file identity: a malformed model answer may
    /// repeat it, and a patch can contain more than one entry with the same displayed
    /// path. This projection consumes each canonical index at most once, then appends
    /// every unclaimed index in patch order. Callers can consequently keep cursors and
    /// folds keyed by the immutable patch index (IR-11).
    #[must_use]
    pub fn effective_order(&self, mode: OrderMode, canonical_paths: &[String]) -> Vec<usize> {
        if mode == OrderMode::Path {
            return (0..canonical_paths.len()).collect();
        }

        let mut claimed = vec![false; canonical_paths.len()];
        let mut order = Vec::with_capacity(canonical_paths.len());
        for path in self.groups.iter().flat_map(|group| &group.files) {
            if let Some(index) =
                canonical_paths
                    .iter()
                    .enumerate()
                    .find_map(|(index, candidate)| {
                        (!claimed[index] && candidate == path).then_some(index)
                    })
            {
                claimed[index] = true;
                order.push(index);
            }
        }
        order.extend(
            claimed
                .iter()
                .enumerate()
                .filter_map(|(index, claimed)| (!claimed).then_some(index)),
        );
        order
    }

    /// The 1-based position of a path in the given mode.
    #[must_use]
    pub fn position_of(&self, path: &str, mode: OrderMode, path_order: &[String]) -> Option<usize> {
        self.effective_order(mode, path_order)
            .iter()
            .position(|index| {
                path_order
                    .get(*index)
                    .is_some_and(|candidate| candidate == path)
            })
            .map(|index| index + 1)
    }

    /// How many files the plan covers, ignoring the mode.
    #[must_use]
    pub fn len(&self, path_order: &[String]) -> usize {
        self.effective_order(OrderMode::Recommended, path_order)
            .len()
    }

    /// Whether there is nothing to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(|group| group.files.is_empty())
    }

    /// The group a file is in, if any.
    #[must_use]
    pub fn group_of(&self, path: &str) -> Option<&str> {
        self.groups
            .iter()
            .find(|group| group.files.iter().any(|file| file == path))
            .map(|group| group.group.as_str())
    }

    /// Moves a group up or down, which is the manual override of FR-4.2.
    ///
    /// Returns whether anything moved, so the caller can say "already first" rather
    /// than reporting a change that did not happen.
    pub fn move_group(&mut self, group: &str, delta: i32) -> bool {
        let Some(index) = self
            .groups
            .iter()
            .position(|candidate| candidate.group == group)
        else {
            return false;
        };
        let Some(target) = index
            .checked_add_signed(delta as isize)
            .filter(|target| *target < self.groups.len())
        else {
            return false;
        };
        self.groups.swap(index, target);
        self.renumber();
        self.overridden = true;
        true
    }

    /// Puts a file into a group, which is the other half of FR-4.2's override.
    ///
    /// A group that does not exist is created at the end rather than refused: the user
    /// asked for a heading, and inventing one is friendlier than an error message
    /// listing the ones that do exist.
    pub fn pin_file(&mut self, path: &str, group: &str) -> bool {
        let name = group.trim();
        if name.is_empty() || path.trim().is_empty() {
            return false;
        }
        if self
            .group_of(path)
            .is_some_and(|current| current.eq_ignore_ascii_case(name))
        {
            return false;
        }
        let existing_name = self
            .groups
            .iter()
            .find(|existing| existing.group.eq_ignore_ascii_case(name))
            .map(|existing| existing.group.clone());
        let target_name = existing_name.unwrap_or_else(|| name.to_owned());
        for existing in &mut self.groups {
            existing.files.retain(|file| file != path);
        }
        if !self
            .groups
            .iter()
            .any(|existing| existing.group == target_name)
        {
            let order = u32::try_from(self.groups.len() + 1).unwrap_or(u32::MAX);
            self.groups.push(PlanGroup {
                order,
                group: target_name.clone(),
                rationale: "added by hand".to_owned(),
                files: Vec::new(),
            });
        }
        if let Some(target) = self
            .groups
            .iter_mut()
            .find(|group| group.group == target_name)
        {
            target.files.push(path.to_owned());
            target.files.sort();
            target.files.dedup();
        }
        self.groups.retain(|g| !g.files.is_empty());
        self.renumber();
        self.overridden = true;
        true
    }

    /// Whether this plan describes the given head.
    #[must_use]
    pub fn matches_head(&self, head_sha: &str) -> bool {
        !self.head_sha.is_empty() && self.head_sha == head_sha
    }

    /// Ensures every current file has a marker bound to its exact change.
    pub fn sync_file_reviews(&mut self, patch: &crate::domain::diff::Patch) {
        let previous: BTreeMap<&str, &FileReview> = self
            .file_reviews
            .iter()
            .map(|review| (review.path.as_str(), review))
            .collect();
        self.file_reviews = file_fingerprints(patch)
            .into_iter()
            .map(|(path, fingerprint)| FileReview {
                status: previous
                    .get(path.as_str())
                    .filter(|review| review.fingerprint == fingerprint)
                    .map_or(ReviewStatus::NotReviewed, |review| review.status),
                path,
                fingerprint,
            })
            .collect();
    }

    /// Reconciles durable human state with a freshly derived plan.
    ///
    /// On the same head, review markers and manual ordering are copied directly: no
    /// file has changed, including binary files whose patch cannot provide a useful
    /// fingerprint. Across heads, state is carried only where fingerprints prove the
    /// file change is identical (DEC-23).
    #[must_use]
    pub fn carry_forward(
        previous: &Self,
        mut current: Self,
        patch: &crate::domain::diff::Patch,
    ) -> Self {
        current.document_revision = previous.document_revision;
        if previous.head_sha == current.head_sha {
            current.file_reviews.clone_from(&previous.file_reviews);
            if previous.overridden {
                current.groups.clone_from(&previous.groups);
                current.overridden = true;
            }
            current.override_invalidated = previous.override_invalidated;
            return current;
        }

        current.sync_file_reviews(patch);
        let old: BTreeMap<&str, &FileReview> = previous
            .file_reviews
            .iter()
            .map(|review| (review.path.as_str(), review))
            .collect();
        for review in &mut current.file_reviews {
            let Some(previous) = old.get(review.path.as_str()) else {
                continue;
            };
            review.status = if carryable_match(&previous.fingerprint, &review.fingerprint) {
                previous.status
            } else if previous.status == ReviewStatus::NotReviewed {
                ReviewStatus::NotReviewed
            } else {
                ReviewStatus::NeedsRevisit
            };
        }

        if previous.overridden {
            let unchanged = current.file_reviews.len() == previous.file_reviews.len()
                && current.file_reviews.iter().all(|review| {
                    old.get(review.path.as_str())
                        .is_some_and(|old| carryable_match(&old.fingerprint, &review.fingerprint))
                });
            if unchanged {
                current.groups.clone_from(&previous.groups);
                current.overridden = true;
            } else {
                current.override_invalidated = true;
            }
        }
        current
    }

    /// Restores only the analysis-proposed ordering, preserving durable review state.
    pub fn reset_order_from_analysis(&mut self, analysis: &Analysis) {
        self.head_sha.clone_from(&analysis.head_sha);
        self.source = PlanSource::Analysis;
        self.groups.clone_from(&analysis.review_plan);
        self.overridden = false;
        self.override_invalidated = false;
    }

    /// The explicit status for a path.
    #[must_use]
    pub fn review_status(&self, path: &str) -> ReviewStatus {
        self.file_reviews
            .iter()
            .find(|review| review.path == path)
            .map_or(ReviewStatus::NotReviewed, |review| review.status)
    }

    /// Sets a human marker, returning whether it changed.
    pub fn set_review_status(&mut self, path: &str, status: ReviewStatus) -> bool {
        let Some(review) = self
            .file_reviews
            .iter_mut()
            .find(|review| review.path == path)
        else {
            return false;
        };
        if review.status == status {
            return false;
        }
        review.status = status;
        true
    }

    /// Reviewed files and total files.
    #[must_use]
    pub fn review_progress(&self) -> (usize, usize, usize) {
        let reviewed = self
            .file_reviews
            .iter()
            .filter(|review| review.status == ReviewStatus::Reviewed)
            .count();
        let revisit = self
            .file_reviews
            .iter()
            .filter(|review| review.status == ReviewStatus::NeedsRevisit)
            .count();
        (reviewed, revisit, self.file_reviews.len())
    }

    /// Renumbers the groups from one, so the panel never shows "2, 1".
    fn renumber(&mut self) {
        for (position, group) in self.groups.iter_mut().enumerate() {
            group.order = u32::try_from(position + 1).unwrap_or(u32::MAX);
        }
    }
}

/// Stable, non-secret fingerprints for every file change in patch order.
#[must_use]
pub fn file_fingerprints(parsed_patch: &crate::domain::diff::Patch) -> Vec<(String, String)> {
    parsed_patch
        .files
        .iter()
        .filter_map(|file| {
            let file_path = file.path()?.to_string();
            let mut canonical = String::new();
            let _ = write!(
                canonical,
                "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}",
                file.old_path
                    .as_ref()
                    .map_or("", crate::domain::diff::RelPath::as_str),
                file.new_path
                    .as_ref()
                    .map_or("", crate::domain::diff::RelPath::as_str),
                file.status.label(),
                file.binary,
                file.mode_change
                    .as_ref()
                    .map_or_else(String::new, |mode| format!(
                        "{}>{}",
                        mode.old.as_deref().unwrap_or(""),
                        mode.new.as_deref().unwrap_or("")
                    ))
            );
            for line in file
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind != crate::domain::diff::LineKind::Context)
            {
                let _ = write!(
                    canonical,
                    "{:?}:{}:{}:{}:{}\u{1}",
                    line.kind,
                    line.old_line.unwrap_or(0),
                    line.new_line.unwrap_or(0),
                    line.content,
                    line.no_newline
                );
            }
            let fingerprint =
                if file.binary || (file.hunks.is_empty() && file.mode_change.is_none()) {
                    // A binary/metadata-only patch does not carry enough content to prove
                    // equality across heads. It may still be marked on this head, but an
                    // empty fingerprint deliberately never carries forward (DEC-23).
                    String::new()
                } else {
                    fnv(&canonical)
                };
            Some((file_path, fingerprint))
        })
        .collect()
}

fn carryable_match(previous: &str, current: &str) -> bool {
    !current.is_empty() && previous == current
}

fn fnv(text: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Which heuristic layer a path belongs to, if any.
#[must_use]
pub fn layer_of(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    // Tests and documentation are recognised everywhere, not only in their own
    // directories: a Rust unit test lives next to the code it tests.
    if is_test(&lower) {
        return Some("tests");
    }
    if is_documentation(&lower) {
        return Some("docs");
    }
    for layer in LAYERS {
        if layer
            .prefixes
            .iter()
            .any(|prefix| lower.starts_with(prefix))
            || layer.contains.iter().any(|name| lower == *name)
        {
            return Some(layer.name);
        }
    }
    if is_config(&lower) {
        return Some("config");
    }
    None
}

/// Whether a path is a test.
fn is_test(lower: &str) -> bool {
    let name = lower.rsplit('/').next().unwrap_or(lower);
    lower.starts_with("tests/")
        || lower.starts_with("test/")
        || lower.starts_with("spec/")
        || lower.starts_with("testdata/")
        || lower.starts_with("benches/")
        || name.starts_with("test_")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_test.go")
        || name.ends_with("_spec.rb")
        || lower.contains("/__tests__/")
}

/// Whether a path is documentation.
fn is_documentation(lower: &str) -> bool {
    let name = lower.rsplit('/').next().unwrap_or(lower);
    let doc_name = name.starts_with("readme")
        || name.starts_with("changelog")
        || name.starts_with("contributing")
        || name.starts_with("license")
        || name == "agents.md"
        || name == "claude.md";
    doc_name || DOC_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
}

/// Whether a path is configuration.
fn is_config(lower: &str) -> bool {
    let name = lower.rsplit('/').next().unwrap_or(lower);
    name == "dockerfile"
        || name == "makefile"
        || name == "justfile"
        || name.starts_with(".env.example")
        || lower.starts_with(".github/")
        || CONFIG_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::domain::analysis::{Normalized, PathIndex};

    fn analysis(text: &str) -> Analysis {
        let index = PathIndex::from_paths(
            [
                "src/domain/money.rs",
                "src/application/billing.rs",
                "tests/billing.rs",
                "docs/rounding.md",
            ]
            .map(str::to_owned),
        );
        let Normalized { analysis, .. } = crate::domain::analysis::normalize(
            text,
            &index,
            "m",
            "head1",
            "2026-01-01T00:00:00Z",
            crate::domain::analysis::AnalysisUsage::default(),
        )
        .expect("the fixture parses");
        analysis
    }

    fn plan() -> Plan {
        Plan::from_analysis(&analysis(
            r#"{"summary": "s", "intent": "i",
                "review_plan": [
                    {"order": 1, "group": "domain", "rationale": "rules",
                     "files": ["src/domain/money.rs"]},
                    {"order": 2, "group": "application", "rationale": "callers",
                     "files": ["src/application/billing.rs"]},
                    {"order": 3, "group": "docs", "rationale": "prose",
                     "files": ["docs/rounding.md"]}]}"#,
        ))
    }

    fn path_order() -> Vec<String> {
        [
            "src/application/billing.rs",
            "src/domain/money.rs",
            "tests/billing.rs",
            "docs/rounding.md",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    fn review_patch(second_line: &str) -> crate::domain::diff::Patch {
        crate::domain::diff::parse_patch(&format!(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n\
             diff --git a/tests/a.rs b/tests/a.rs\n--- a/tests/a.rs\n+++ b/tests/a.rs\n@@ -1 +1 @@\n-old test\n+{second_line}\n"
        ))
    }

    fn patch_paths(patch: &crate::domain::diff::Patch) -> Vec<String> {
        patch
            .files
            .iter()
            .filter_map(crate::domain::diff::FileDiff::path)
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn the_recommended_order_follows_the_plan_and_the_path_order_does_not() {
        let plan = plan();
        let order = path_order();
        assert_eq!(
            plan.files(OrderMode::Recommended, &order),
            [
                "src/domain/money.rs",
                "src/application/billing.rs",
                "docs/rounding.md",
                // The plan never mentioned the test file; it is appended, not lost.
                "tests/billing.rs"
            ]
        );
        assert_eq!(plan.files(OrderMode::Path, &order), order);
    }

    #[test]
    fn both_orders_report_a_position_for_the_same_file() {
        let plan = plan();
        let order = path_order();
        assert_eq!(
            plan.position_of("src/domain/money.rs", OrderMode::Recommended, &order),
            Some(1)
        );
        assert_eq!(
            plan.position_of("src/domain/money.rs", OrderMode::Path, &order),
            Some(2)
        );
        assert_eq!(
            plan.position_of("nowhere.rs", OrderMode::Recommended, &order),
            None
        );
    }

    #[test]
    fn toggling_the_mode_is_reversible_and_labelled() {
        assert_eq!(OrderMode::default(), OrderMode::Recommended);
        assert_eq!(OrderMode::Recommended.toggled(), OrderMode::Path);
        assert_eq!(OrderMode::Path.toggled(), OrderMode::Recommended);
        assert_eq!(OrderMode::Recommended.label(), "recommended order");
        assert_eq!(OrderMode::Path.label(), "path order");
    }

    #[test]
    fn every_file_is_in_exactly_one_position_whatever_the_plan_says() {
        // A plan that knows about one file only: the rest are appended.
        let plan = Plan::from_analysis(&analysis(
            r#"{"summary": "s", "intent": "i",
                "review_plan": [{"order": 1, "group": "domain", "rationale": "r",
                                 "files": ["src/domain/money.rs"]}]}"#,
        ));
        let order = path_order();
        let files = plan.files(OrderMode::Recommended, &order);
        assert_eq!(files.len(), order.len(), "{files:?}");
        let unique: BTreeSet<&String> = files.iter().collect();
        assert_eq!(unique.len(), order.len(), "no file twice: {files:?}");
    }

    #[test]
    fn effective_order_uses_canonical_indexes_for_duplicate_and_unknown_entries() {
        let mut plan = plan();
        plan.groups[0].files = vec![
            "src/domain/money.rs".to_owned(),
            "src/domain/money.rs".to_owned(),
            "missing.rs".to_owned(),
        ];
        let paths = vec![
            "src/application/billing.rs".to_owned(),
            "src/domain/money.rs".to_owned(),
            "src/domain/money.rs".to_owned(),
        ];

        assert_eq!(
            plan.effective_order(OrderMode::Recommended, &paths),
            vec![1, 2, 0],
            "each immutable patch entry is read once, even when paths repeat"
        );
    }

    #[test]
    fn effective_order_appends_omitted_files_in_their_original_patch_sequence() {
        let mut plan = plan();
        plan.groups[0].files = vec!["m.rs".to_owned()];
        let paths = vec!["z.rs".to_owned(), "a.rs".to_owned(), "m.rs".to_owned()];

        assert_eq!(
            plan.effective_order(OrderMode::Recommended, &paths),
            vec![2, 0, 1],
            "unplanned entries remain in patch order, not lexical path order"
        );
    }

    #[test]
    fn a_heuristic_plan_places_paths_by_their_layer() {
        let paths: Vec<String> = [
            "src/domain/money.rs",
            "src/adapters/gh.rs",
            "src/tui/app.rs",
            "src/application/prs.rs",
            "Cargo.toml",
            "tests/shell_snapshots.rs",
            "README.md",
            "scripts/validate/m1.sh",
            "assets/logo.png",
        ]
        .map(str::to_owned)
        .to_vec();
        let plan = Plan::heuristic("head1", &paths);
        let names: Vec<&str> = plan.groups.iter().map(|g| g.group.as_str()).collect();
        assert_eq!(
            names,
            [
                "domain",
                "application",
                "infrastructure",
                "interfaces",
                "config",
                "tests",
                "docs",
                "unclassified"
            ]
        );
        let placement: Vec<(String, String)> = plan
            .groups
            .iter()
            .flat_map(|group| {
                group
                    .files
                    .iter()
                    .map(|file| (file.clone(), group.group.clone()))
            })
            .collect();
        assert!(placement.contains(&("src/domain/money.rs".to_owned(), "domain".to_owned())));
        assert!(
            placement.contains(&("src/adapters/gh.rs".to_owned(), "infrastructure".to_owned()))
        );
        assert!(placement.contains(&("src/tui/app.rs".to_owned(), "interfaces".to_owned())));
        assert!(placement.contains(&("Cargo.toml".to_owned(), "config".to_owned())));
        assert!(placement.contains(&("README.md".to_owned(), "docs".to_owned())));
        assert!(placement.contains(&("assets/logo.png".to_owned(), "unclassified".to_owned())));
        // Every file is placed, and the source says this is a fallback.
        assert_eq!(
            plan.files(OrderMode::Recommended, &paths).len(),
            paths.len()
        );
        assert_eq!(plan.source, PlanSource::Heuristic);
    }

    #[test]
    fn every_heuristic_group_explains_its_position() {
        let plan = Plan::heuristic("head1", &["src/domain/a.rs".to_owned()]);
        for group in &plan.groups {
            assert!(!group.rationale.is_empty(), "{group:?}");
        }
    }

    #[test]
    fn a_path_is_classified_by_its_own_shape_not_only_its_directory() {
        assert_eq!(layer_of("src/domain/money_test.rs"), Some("tests"));
        assert_eq!(layer_of("internal/billing_test.go"), Some("tests"));
        assert_eq!(layer_of("CHANGELOG.md"), Some("docs"));
        assert_eq!(layer_of("config/app.yaml"), Some("config"));
        assert_eq!(layer_of("docker-compose.yml"), Some("config"));
        assert_eq!(layer_of("src/lib.rs"), Some("interfaces"));
        assert_eq!(layer_of("assets/logo.png"), None);
    }

    #[test]
    fn moving_a_group_changes_the_order_and_marks_the_plan_overridden() {
        let mut plan = plan();
        let order = path_order();
        assert_eq!(
            plan.files(OrderMode::Recommended, &order)[0],
            "src/domain/money.rs"
        );
        assert!(plan.move_group("docs", -2));
        // `docs` has no test file behind it, so the appended file stays last.
        assert_eq!(
            plan.files(OrderMode::Recommended, &order)[0],
            "docs/rounding.md"
        );
        assert!(plan.overridden);
        // The numbers are renumbered, so the panel reads 1, 2, 3.
        let numbers: Vec<u32> = plan.groups.iter().map(|group| group.order).collect();
        assert_eq!(numbers, [1, 2, 3, 4]);
    }

    #[test]
    fn moving_the_first_group_up_says_nothing_happened() {
        let mut plan = plan();
        assert!(!plan.move_group("domain", -1));
        assert!(!plan.overridden, "a no-op is not an override");
        assert!(!plan.move_group("nothing-like-this", 1));
    }

    #[test]
    fn pinning_a_file_moves_it_and_removes_it_from_where_it_was() {
        let mut plan = plan();
        assert!(plan.pin_file("src/domain/money.rs", "application"));
        let application = plan
            .groups
            .iter()
            .find(|group| group.group == "application")
            .expect("the group");
        assert!(
            application
                .files
                .contains(&"src/domain/money.rs".to_owned())
        );
        assert_eq!(plan.group_of("src/domain/money.rs"), Some("application"));
        // And it is not still in `domain`: exactly one position for the file.
        assert_eq!(
            plan.files(OrderMode::Recommended, &path_order())
                .iter()
                .filter(|file| *file == "src/domain/money.rs")
                .count(),
            1
        );
        assert!(plan.overridden);
    }

    #[test]
    fn pinning_to_a_new_group_creates_it() {
        let mut plan = plan();
        assert!(plan.pin_file("tests/billing.rs", "my own group"));
        assert_eq!(plan.group_of("tests/billing.rs"), Some("my own group"));
        // And an empty group left behind by the move is dropped.
        assert!(plan.groups.iter().all(|group| !group.files.is_empty()));
    }

    #[test]
    fn pinning_a_file_where_it_already_is_reports_no_change() {
        let mut plan = plan();
        assert!(!plan.pin_file("src/domain/money.rs", "domain"));
        assert!(!plan.overridden);
        assert!(!plan.pin_file("", "domain"));
        assert!(!plan.pin_file("src/domain/money.rs", "   "));
    }

    #[test]
    fn a_plan_is_bound_to_the_head_it_was_made_for() {
        let plan = plan();
        assert!(plan.matches_head("head1"));
        assert!(!plan.matches_head("head2"));
        // An empty head never matches: a plan from before the head was known is not
        // advertised as current.
        let mut blank = plan.clone();
        blank.head_sha = String::new();
        assert!(!blank.matches_head(""));
    }

    #[test]
    fn a_plan_survives_a_round_trip_through_json() {
        let mut plan = plan();
        plan.move_group("docs", -1);
        let text = serde_json::to_string(&plan).expect("serialises");
        let back: Plan = serde_json::from_str(&text).expect("deserialises");
        assert_eq!(back, plan);
        assert!(back.overridden);
    }

    #[test]
    fn without_an_analysis_the_heuristic_order_is_the_recommended_one() {
        // `normalize` guarantees a plan, so this is the belt to that braces: the
        // heuristic is what the app uses when there is no analysis at all, and the
        // ordered view must still be a complete, readable order.
        let paths = path_order();
        let plan = Plan::heuristic("head1", &paths);
        assert_eq!(
            plan.files(OrderMode::Recommended, &paths),
            [
                "src/domain/money.rs",
                "src/application/billing.rs",
                "tests/billing.rs",
                "docs/rounding.md"
            ]
        );
        assert_eq!(plan.files(OrderMode::Path, &paths), paths);
        assert_eq!(plan.source, PlanSource::Heuristic);
    }

    #[test]
    fn the_plan_says_where_it_came_from() {
        assert_eq!(PlanSource::Analysis.label(), "from the analysis");
        assert!(PlanSource::Heuristic.label().contains("path rules"));
        assert_eq!(plan().source, PlanSource::Analysis);
    }

    #[test]
    fn ir_16_human_progress_is_explicit_and_bound_to_the_file_change() {
        let patch = review_patch("new test");
        let paths = patch_paths(&patch);
        let mut plan = Plan::heuristic("head1", &paths);
        plan.sync_file_reviews(&patch);
        assert_eq!(plan.review_progress(), (0, 0, 2));
        assert!(plan.set_review_status("src/a.rs", ReviewStatus::Reviewed));
        assert_eq!(plan.review_status("src/a.rs"), ReviewStatus::Reviewed);
        assert_eq!(plan.review_progress(), (1, 0, 2));
    }

    #[test]
    fn ir_16_a_new_head_keeps_only_provably_unchanged_review_progress() {
        let old_patch = review_patch("new test");
        let old_paths = patch_paths(&old_patch);
        let mut previous = Plan::heuristic("head1", &old_paths);
        previous.sync_file_reviews(&old_patch);
        assert!(previous.set_review_status("src/a.rs", ReviewStatus::Reviewed));
        assert!(previous.set_review_status("tests/a.rs", ReviewStatus::Reviewed));

        let new_patch = review_patch("different test");
        let new_paths = patch_paths(&new_patch);
        let current = Plan::heuristic("head2", &new_paths);
        let carried = Plan::carry_forward(&previous, current, &new_patch);

        assert_eq!(carried.review_status("src/a.rs"), ReviewStatus::Reviewed);
        assert_eq!(
            carried.review_status("tests/a.rs"),
            ReviewStatus::NeedsRevisit
        );
    }

    #[test]
    fn ir_16_ambiguous_binary_changes_need_revisit_on_a_new_head() {
        let patch = crate::domain::diff::parse_patch(
            "diff --git a/logo.png b/logo.png\n--- a/logo.png\n+++ b/logo.png\nBinary files a/logo.png and b/logo.png differ\n",
        );
        let paths = patch_paths(&patch);
        let mut previous = Plan::heuristic("head1", &paths);
        previous.sync_file_reviews(&patch);
        assert!(previous.set_review_status("logo.png", ReviewStatus::Reviewed));

        let same_head = Plan::carry_forward(&previous, Plan::heuristic("head1", &paths), &patch);
        assert_eq!(same_head.review_status("logo.png"), ReviewStatus::Reviewed);

        let current = Plan::heuristic("head2", &paths);
        let carried = Plan::carry_forward(&previous, current, &patch);
        assert_eq!(
            carried.review_status("logo.png"),
            ReviewStatus::NeedsRevisit
        );
    }

    #[test]
    fn ir_16_same_head_reanalysis_keeps_manual_order_and_progress() {
        let patch = review_patch("new test");
        let paths = patch_paths(&patch);
        let mut previous = Plan::heuristic("head1", &paths);
        previous.sync_file_reviews(&patch);
        previous.document_revision = 7;
        assert!(previous.set_review_status("src/a.rs", ReviewStatus::Reviewed));
        let first_group = previous.groups[0].group.clone();
        assert!(previous.move_group(&first_group, 1));
        let expected_groups = previous.groups.clone();

        let carried = Plan::carry_forward(&previous, Plan::heuristic("head1", &paths), &patch);

        assert_eq!(carried.document_revision, 7);
        assert_eq!(carried.review_status("src/a.rs"), ReviewStatus::Reviewed);
        assert!(carried.overridden);
        assert_eq!(carried.groups, expected_groups);
    }

    #[test]
    fn ir_16_revision_changes_explain_and_reset_only_incompatible_manual_order() {
        let old_patch = review_patch("new test");
        let old_paths = patch_paths(&old_patch);
        let mut previous = Plan::heuristic("head1", &old_paths);
        previous.sync_file_reviews(&old_patch);
        let first_group = previous.groups[0].group.clone();
        assert!(previous.move_group(&first_group, 1));
        let overridden_groups = previous.groups.clone();

        let unchanged =
            Plan::carry_forward(&previous, Plan::heuristic("head2", &old_paths), &old_patch);
        assert!(unchanged.overridden);
        assert!(!unchanged.override_invalidated);
        assert_eq!(unchanged.groups, overridden_groups);

        let changed_patch = review_patch("different test");
        let changed_paths = patch_paths(&changed_patch);
        let changed = Plan::carry_forward(
            &previous,
            Plan::heuristic("head3", &changed_paths),
            &changed_patch,
        );
        assert!(!changed.overridden);
        assert!(changed.override_invalidated);
    }

    #[test]
    fn ir_16_human_step_names_are_not_forced_into_machine_slugs() {
        let mut plan = plan();
        assert!(plan.pin_file("tests/billing.rs", "Verify behavior"));
        assert_eq!(plan.group_of("tests/billing.rs"), Some("Verify behavior"));
        assert!(!plan.pin_file("tests/billing.rs", "verify BEHAVIOR"));
    }
}
