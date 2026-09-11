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
//! - **an override is bound to the head it was made against.** New commits mean a new
//!   set of files, so a stale override is dropped rather than applied to a plan whose
//!   files it no longer describes.
//!
//! Pure: no IO, no terminal, no diff model (NFR-5.2).

use std::collections::BTreeSet;

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
    /// The head this plan describes. A plan for another head is not applied.
    pub head_sha: String,
    /// Where the grouping came from.
    pub source: PlanSource,
    /// The groups, in reading order.
    pub groups: Vec<PlanGroup>,
    /// Whether the user has moved anything by hand.
    #[serde(default)]
    pub overridden: bool,
}

impl Plan {
    /// The plan the analysis asks for.
    #[must_use]
    pub fn from_analysis(analysis: &Analysis) -> Self {
        Self {
            head_sha: analysis.head_sha.clone(),
            source: PlanSource::Analysis,
            groups: analysis.review_plan.clone(),
            overridden: false,
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
            head_sha: head_sha.to_owned(),
            source: PlanSource::Heuristic,
            groups,
            overridden: false,
        }
    }

    /// The files, in the order the given mode reads them.
    ///
    /// This is what the ordered view iterates, and it is guaranteed to contain each
    /// file once: the groups come from a normalized plan, and the caller passes the
    /// diff's own path list so nothing can be lost between the two.
    #[must_use]
    pub fn files(&self, mode: OrderMode, path_order: &[String]) -> Vec<String> {
        match mode {
            OrderMode::Path => path_order.to_vec(),
            OrderMode::Recommended => {
                let placed: BTreeSet<&str> = self
                    .groups
                    .iter()
                    .flat_map(|group| group.files.iter().map(String::as_str))
                    .collect();
                // Files the plan does not know about (a plan derived from an older
                // analysis, say) are appended rather than dropped: the ordered view is
                // a view of the diff, not a subset of it.
                let mut files: Vec<String> = self
                    .groups
                    .iter()
                    .flat_map(|group| group.files.iter().cloned())
                    .collect();
                files.extend(
                    path_order
                        .iter()
                        .filter(|path| !placed.contains(path.as_str()))
                        .cloned(),
                );
                files
            }
        }
    }

    /// The 1-based position of a path in the given mode.
    #[must_use]
    pub fn position_of(&self, path: &str, mode: OrderMode, path_order: &[String]) -> Option<usize> {
        self.files(mode, path_order)
            .iter()
            .position(|candidate| candidate == path)
            .map(|index| index + 1)
    }

    /// How many files the plan covers, ignoring the mode.
    #[must_use]
    pub fn len(&self, path_order: &[String]) -> usize {
        self.files(OrderMode::Recommended, path_order).len()
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
        let name = group.trim().to_lowercase().replace(' ', "-");
        if name.is_empty() || path.trim().is_empty() {
            return false;
        }
        if self.group_of(path) == Some(name.as_str()) {
            return false;
        }
        for existing in &mut self.groups {
            existing.files.retain(|file| file != path);
        }
        if !self.groups.iter().any(|existing| existing.group == name) {
            let order = u32::try_from(self.groups.len() + 1).unwrap_or(u32::MAX);
            self.groups.push(PlanGroup {
                order,
                group: name.clone(),
                rationale: "added by hand".to_owned(),
                files: Vec::new(),
            });
        }
        if let Some(target) = self.groups.iter_mut().find(|g| g.group == name) {
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

    /// Renumbers the groups from one, so the panel never shows "2, 1".
    fn renumber(&mut self) {
        for (position, group) in self.groups.iter_mut().enumerate() {
            group.order = u32::try_from(position + 1).unwrap_or(u32::MAX);
        }
    }
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
        // And it is not still in `domain`.
        assert!(
            plan.files(OrderMode::Recommended, &path_order())
                .iter()
                .filter(|file| *file == "src/domain/money.rs")
                .count()
                == 1
        );
        assert!(plan.overridden);
    }

    #[test]
    fn pinning_to_a_new_group_creates_it() {
        let mut plan = plan();
        assert!(plan.pin_file("tests/billing.rs", "my own group"));
        assert_eq!(plan.group_of("tests/billing.rs"), Some("my-own-group"));
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
        assert!(plan().source == PlanSource::Analysis);
    }
}
