//! Pull requests: what the list shows, what the detail needs, how it is queried
//! (FR-2.1, FR-2.2, FR-2.4).
//!
//! The distinction between [`PullRequestSummary`] and [`PullRequestDetail`] is
//! deliberate: the list is deliberately cheap (one `gh pr list`), and everything
//! expensive — body, commits, checks, reviews, inline comments — arrives only when
//! a PR is opened.

use serde::{Deserialize, Serialize};

use crate::domain::repo::RepoId;
use crate::domain::time::{Timestamp, relative};

/// A pull request as the list shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullRequestSummary {
    /// The PR number, unique within the repository.
    pub number: u64,
    /// The title.
    pub title: String,
    /// The login of the author.
    pub author: String,
    /// Open, closed or merged.
    pub state: PrState,
    /// Whether the author marked it a draft (or titled it WIP: see
    /// [`Self::is_work_in_progress`]).
    pub is_draft: bool,
    /// The branch it merges into.
    pub base_ref: String,
    /// The branch it comes from.
    pub head_ref: String,
    /// The head commit SHA, which is what a cached analysis is keyed by (FR-4.3).
    pub head_sha: String,
    /// When it was opened.
    pub created_at: Timestamp,
    /// When it last changed, which is the default sort key.
    pub updated_at: Timestamp,
    /// Lines added.
    pub additions: u64,
    /// Lines deleted.
    pub deletions: u64,
    /// Files touched.
    pub changed_files: u64,
    /// Label names, in GitHub's order.
    pub labels: Vec<String>,
    /// The aggregate review decision, when GitHub has one.
    pub review_decision: Option<ReviewDecision>,
    /// The state of the PR's checks.
    pub checks: CheckSummary,
    /// The browser URL, used by `:open` and by copied links.
    pub url: String,
    /// Whether the head branch lives in a fork (FR-2.4).
    pub is_cross_repository: bool,
}

/// Whether a pull request is open, closed or merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrState {
    /// Still open for review.
    Open,
    /// Closed without merging.
    Closed,
    /// Merged.
    Merged,
}

impl PrState {
    /// Parses `gh`'s spelling.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.to_ascii_uppercase().as_str() {
            "MERGED" => Self::Merged,
            "CLOSED" => Self::Closed,
            _ => Self::Open,
        }
    }

    /// A short label for the list.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
        }
    }
}

/// The review decision GitHub computes from the reviews it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// Approved by the required reviewers.
    Approved,
    /// A reviewer asked for changes.
    ChangesRequested,
    /// Reviews are required and not yet given.
    ReviewRequired,
}

impl ReviewDecision {
    /// Parses `gh`'s spelling, mapping its `null` to `None`.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value?.to_ascii_uppercase().as_str() {
            "APPROVED" => Some(Self::Approved),
            "CHANGES_REQUESTED" => Some(Self::ChangesRequested),
            "REVIEW_REQUIRED" => Some(Self::ReviewRequired),
            _ => None,
        }
    }

    /// A short label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::ChangesRequested => "changes requested",
            Self::ReviewRequired => "review required",
        }
    }

    /// The one or two characters the list can afford.
    #[must_use]
    pub fn marker(self) -> &'static str {
        match self {
            Self::Approved => "ok",
            Self::ChangesRequested => "no",
            Self::ReviewRequired => "..",
        }
    }
}

/// The state of one check run or status context.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckState {
    /// Passed.
    Success,
    /// Failed, timed out, or was cancelled.
    Failure,
    /// Still running.
    Pending,
    /// Concluded without a verdict.
    Neutral,
    /// Deliberately not run.
    Skipped,
    /// A conclusion this build does not know, and the default: an unset or
    /// unrecognised state must never look green.
    #[default]
    Unknown,
}

/// The forge-reported execution lifecycle of a check run (IR-10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckLifecycle {
    /// The run has not started.
    Queued,
    /// The run is executing.
    Running,
    /// The run reached a conclusion.
    Completed,
    /// The forge did not provide a recognisable lifecycle.
    #[default]
    Unknown,
}

impl CheckLifecycle {
    /// Maps GitHub's check-run status without losing queued versus running.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or_default().to_ascii_uppercase().as_str() {
            "QUEUED" | "REQUESTED" | "WAITING" | "PENDING" => Self::Queued,
            "IN_PROGRESS" | "RUNNING" => Self::Running,
            "COMPLETED" | "SUCCESS" | "FAILURE" | "ERROR" => Self::Completed,
            _ => Self::Unknown,
        }
    }

    /// User-facing label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Unknown => "unknown",
        }
    }
}

impl CheckState {
    /// Maps `gh`'s `conclusion` field for a check run.
    #[must_use]
    pub fn from_conclusion(value: Option<&str>) -> Self {
        match value.unwrap_or_default().to_ascii_uppercase().as_str() {
            "SUCCESS" => Self::Success,
            "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => {
                Self::Failure
            }
            "NEUTRAL" => Self::Neutral,
            "SKIPPED" | "STALE" => Self::Skipped,
            "" => Self::Pending,
            _ => Self::Unknown,
        }
    }

    /// Maps `gh`'s `state` field for a commit status context.
    #[must_use]
    pub fn from_status(value: Option<&str>) -> Self {
        match value.unwrap_or_default().to_ascii_uppercase().as_str() {
            "SUCCESS" => Self::Success,
            "FAILURE" | "ERROR" => Self::Failure,
            "PENDING" | "EXPECTED" => Self::Pending,
            _ => Self::Unknown,
        }
    }

    /// Maps `gh`'s `status` field for a check run that has not concluded.
    #[must_use]
    pub fn from_status_field(value: Option<&str>) -> Self {
        match value.unwrap_or_default().to_ascii_uppercase().as_str() {
            "QUEUED" | "IN_PROGRESS" | "WAITING" | "REQUESTED" | "PENDING" => Self::Pending,
            _ => Self::Unknown,
        }
    }

    /// Whether this counts as passed.
    #[must_use]
    pub fn is_success(self) -> bool {
        matches!(self, Self::Success | Self::Skipped | Self::Neutral)
    }

    /// Whether this is a failure worth drawing attention to.
    #[must_use]
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failure)
    }
}

/// One check run or status context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckRun {
    /// The check's name, e.g. `ci / test (ubuntu-latest)`.
    pub name: String,
    /// Its state.
    pub state: CheckState,
    /// Execution lifecycle, retained separately from its final state.
    #[serde(default)]
    pub lifecycle: CheckLifecycle,
    /// Forge conclusion such as `CANCELLED`, retained for the Checks tab.
    #[serde(default)]
    pub conclusion: Option<String>,
    /// Where to read it.
    pub url: Option<String>,
    /// A one-line description, when GitHub provides one.
    pub description: Option<String>,
}

/// The rolled-up state of every check on a PR, which is all the list has room for.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckSummary {
    /// The aggregate state.
    pub state: CheckState,
    /// How many passed.
    pub passed: u32,
    /// How many there are in total.
    pub total: u32,
}

impl CheckSummary {
    /// Rolls up individual runs.
    ///
    /// A failure anywhere dominates, then a pending run, so the summary is never
    /// greener than its worst check.
    #[must_use]
    pub fn from_runs(runs: &[CheckRun]) -> Self {
        if runs.is_empty() {
            return Self {
                state: CheckState::Unknown,
                passed: 0,
                total: 0,
            };
        }
        let passed = runs.iter().filter(|run| run.state.is_success()).count();
        let total = runs.len();
        let state = if runs.iter().any(|run| run.state.is_failure()) {
            CheckState::Failure
        } else if runs.iter().any(|run| run.state == CheckState::Pending) {
            CheckState::Pending
        } else if runs.iter().any(|run| run.state == CheckState::Unknown) {
            CheckState::Unknown
        } else if passed == total {
            CheckState::Success
        } else {
            CheckState::Neutral
        };
        Self {
            state,
            passed: u32::try_from(passed).unwrap_or(u32::MAX),
            total: u32::try_from(total).unwrap_or(u32::MAX),
        }
    }

    /// Whether GitHub has no checks configured for this repository.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// `3/3`, `1/3`, or `—` when there are none (FR-2.1).
    #[must_use]
    pub fn label(&self) -> String {
        if self.is_empty() {
            return "—".to_owned();
        }
        format!("{}/{}", self.passed, self.total)
    }
}

/// One commit on a PR.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Commit {
    /// The full SHA.
    pub sha: String,
    /// The first line of the message.
    pub summary: String,
    /// The author's login or name.
    pub author: String,
    /// When it was committed.
    pub committed_at: Timestamp,
}

/// A review that has been submitted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Review {
    /// Who wrote it.
    pub author: String,
    /// What they decided.
    pub state: ReviewState,
    /// The review body, which is often empty on an approval.
    pub body: String,
    /// When it was submitted.
    pub submitted_at: Option<Timestamp>,
}

/// What a reviewer decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    /// Approved.
    Approved,
    /// Asked for changes.
    ChangesRequested,
    /// Commented without a verdict.
    Commented,
    /// Dismissed by someone else.
    Dismissed,
    /// Submitted but not yet visible.
    Pending,
    /// Something this build does not know.
    Unknown,
}

impl ReviewState {
    /// Parses `gh`'s spelling.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.to_ascii_uppercase().as_str() {
            "APPROVED" => Self::Approved,
            "CHANGES_REQUESTED" => Self::ChangesRequested,
            "COMMENTED" => Self::Commented,
            "DISMISSED" => Self::Dismissed,
            "PENDING" => Self::Pending,
            _ => Self::Unknown,
        }
    }

    /// A short label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::ChangesRequested => "changes requested",
            Self::Commented => "commented",
            Self::Dismissed => "dismissed",
            Self::Pending => "pending",
            Self::Unknown => "unknown",
        }
    }
}

/// An inline review comment anchored to a line of the diff (FR-2.4, FR-6.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewComment {
    /// GitHub's comment id.
    pub id: u64,
    /// Who wrote it.
    pub author: String,
    /// The file it is anchored to.
    pub path: String,
    /// The line it is anchored to, when GitHub still knows it.
    pub line: Option<u64>,
    /// `LEFT` (deletions) or `RIGHT` (additions).
    pub side: Option<String>,
    /// The comment body.
    pub body: String,
    /// When it was written.
    pub created_at: Timestamp,
    /// The comment this one replies to, for threads.
    pub in_reply_to: Option<u64>,
    /// The diff hunk GitHub shows above the comment.
    pub diff_hunk: Option<String>,
    /// The browser URL.
    pub url: Option<String>,
    /// The thread this comment belongs to, when the forge can say (FR-6.4).
    ///
    /// GitHub's own thread id, which is what resolving and unresolving needs: the
    /// REST comments endpoint does not mention threads at all, so this arrives from a
    /// second read and stays `None` when that read failed. `None` means "cannot be
    /// resolved", never "not in a thread".
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Whether the comment's thread has been resolved (FR-6.4).
    ///
    /// A property of the *thread*, denormalised onto each of its comments because the
    /// thread is not a thing this application models: the diff draws comments, and
    /// asking each comment whether it is resolved is one lookup instead of two. Every
    /// comment of one thread carries the same value, which
    /// `threads_carry_their_state_on_every_comment` checks.
    #[serde(default)]
    pub resolved: bool,
    /// Whether GitHub considers the anchor out of date (FR-6.4).
    ///
    /// Also a thread property: the line the comment was made on has moved since, so
    /// the comment is drawn where GitHub reports it, not where it was written.
    #[serde(default)]
    pub outdated: bool,
}

/// A comment on the pull request itself, not on a line of it (FR-6.4, DEC-16).
///
/// The conversation is flat on GitHub: there are no threads here, no lines to anchor
/// to and nothing to resolve. It is a separate type from [`ReviewComment`] for that
/// reason — the two share an author and a body and nothing else, and giving them one
/// type would give every renderer a set of fields it must remember to ignore.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationComment {
    /// GitHub's comment id.
    pub id: u64,
    /// Who wrote it.
    pub author: String,
    /// The body, as markdown.
    pub body: String,
    /// When it was written.
    pub created_at: Timestamp,
    /// The browser URL.
    pub url: Option<String>,
}

/// A pull request, with everything that is expensive to fetch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullRequestDetail {
    /// The cheap part, so the list and the header can reuse it.
    pub summary: PullRequestSummary,
    /// The description, as markdown.
    pub body: String,
    /// The status of the merge, e.g. `CLEAN`, `BEHIND`, `DIRTY`.
    pub merge_state_status: Option<String>,
    /// Who has been asked to review.
    pub reviewers: Vec<String>,
    /// The commits on the PR, oldest first.
    pub commits: Vec<Commit>,
    /// Every check run, for the checks tab.
    pub checks: Vec<CheckRun>,
    /// Submitted reviews.
    pub reviews: Vec<Review>,
    /// Inline comments.
    pub comments: Vec<ReviewComment>,
    /// Comments on the pull request's conversation (FR-6.4).
    ///
    /// Read with the rest of the detail, in the same job: "has anyone said anything
    /// about this pull request" is part of what a detail is, and a panel that fetched
    /// its own contents would need its own loading state, its own failure and its own
    /// cache. Refreshing it is refreshing the detail, which is what happens after a
    /// comment is posted.
    #[serde(default)]
    pub conversation: Vec<ConversationComment>,
    /// The merge base of base and head, resolved with `git` (Appendix A).
    ///
    /// `None` in remote-only mode: `gh` cannot report `baseRefOid` (verified on 2.45),
    /// so resolving it needs the local workspace.
    pub base_sha: Option<String>,
}

impl PullRequestDetail {
    /// The PR's identity.
    #[must_use]
    pub fn reference(&self, repo: &RepoId) -> super::PullRequestRef {
        super::PullRequestRef {
            repo: repo.clone(),
            number: self.summary.number,
        }
    }

    /// Counts of the reviews by decision, for the status line.
    #[must_use]
    pub fn review_counts(&self) -> (u32, u32) {
        let approvals = self
            .reviews
            .iter()
            .filter(|review| review.state == ReviewState::Approved)
            .count();
        let changes = self
            .reviews
            .iter()
            .filter(|review| review.state == ReviewState::ChangesRequested)
            .count();
        (
            u32::try_from(approvals).unwrap_or(u32::MAX),
            u32::try_from(changes).unwrap_or(u32::MAX),
        )
    }
}

/// Identifies a pull request unambiguously (ARCH-4).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PullRequestRef {
    /// Which repository.
    pub repo: RepoId,
    /// Which number.
    pub number: u64,
}

impl std::fmt::Display for PullRequestRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.repo.slug(), self.number)
    }
}

impl PullRequestSummary {
    /// The relative age of the last update, for the list (FR-2.1).
    #[must_use]
    pub fn updated_label(&self, now: Timestamp) -> String {
        relative(now, self.updated_at)
    }

    /// `+18 −4`, or `+18` / `−4` when one side is zero.
    #[must_use]
    pub fn size_label(&self) -> String {
        match (self.additions, self.deletions) {
            (0, 0) => "0".to_owned(),
            (added, 0) => format!("+{added}"),
            (0, deleted) => format!("−{deleted}"),
            (added, deleted) => format!("+{added} −{deleted}"),
        }
    }

    /// Whether the title marks work in progress as well as the draft flag.
    ///
    /// GitHub's draft flag is authoritative, but reviewers also use a `WIP:`
    /// prefix, and the list promises a "draft/WIP marker" (FR-2.1).
    #[must_use]
    pub fn is_work_in_progress(&self) -> bool {
        self.is_draft || has_wip_prefix(&self.title)
    }

    /// Whether this PR matches a client-side search string (FR-2.2).
    ///
    /// The searchable text is the number, the title, the author, the branch names
    /// and the labels, because those are what someone types when looking for a PR.
    #[must_use]
    pub fn matches(&self, query: &str) -> bool {
        let haystack = format!(
            "{} {} {} {} {} {}",
            self.number,
            self.title,
            self.author,
            self.base_ref,
            self.head_ref,
            self.labels.join(" ")
        );
        crate::fuzzy::matches_all_words(query, &haystack)
    }

    /// The text a fuzzy score should be computed against, exposed so tests can
    /// assert on ranking without duplicating the haystack.
    #[must_use]
    pub fn searchable(&self) -> String {
        format!("{} {}", self.number, self.title)
    }
}

/// Whether a title starts with a work-in-progress marker.
fn has_wip_prefix(title: &str) -> bool {
    let upper = title.trim_start().to_ascii_uppercase();
    ["WIP:", "WIP ", "[WIP]", "DRAFT:", "[DRAFT]"]
        .iter()
        .any(|prefix| upper.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn at(seconds: i64) -> Timestamp {
        Utc.timestamp_opt(1_700_000_000 + seconds, 0).unwrap()
    }

    fn summary() -> PullRequestSummary {
        PullRequestSummary {
            number: 141,
            title: "Refactor the billing domain".to_owned(),
            author: "bruno".to_owned(),
            state: PrState::Open,
            is_draft: false,
            base_ref: "main".to_owned(),
            head_ref: "feat/billing".to_owned(),
            head_sha: "abc123".to_owned(),
            created_at: at(-7200),
            updated_at: at(-1800),
            additions: 18,
            deletions: 4,
            changed_files: 3,
            labels: vec!["refactor".to_owned(), "billing".to_owned()],
            review_decision: Some(ReviewDecision::Approved),
            checks: CheckSummary::default(),
            url: "https://github.com/acme/service/pull/141".to_owned(),
            is_cross_repository: false,
        }
    }

    #[test]
    fn states_and_decisions_parse_the_way_gh_spells_them() {
        assert_eq!(PrState::parse("MERGED"), PrState::Merged);
        assert_eq!(PrState::parse("CLOSED"), PrState::Closed);
        assert_eq!(PrState::parse("OPEN"), PrState::Open);

        assert_eq!(
            ReviewDecision::parse(Some("APPROVED")),
            Some(ReviewDecision::Approved)
        );
        assert_eq!(
            ReviewDecision::parse(Some("CHANGES_REQUESTED")),
            Some(ReviewDecision::ChangesRequested)
        );
        assert_eq!(ReviewDecision::parse(None), None);
        assert_eq!(ReviewDecision::parse(Some("")), None);
    }

    #[test]
    fn check_conclusions_map_to_states_that_never_flatter() {
        assert_eq!(
            CheckState::from_conclusion(Some("SUCCESS")),
            CheckState::Success
        );
        assert_eq!(
            CheckState::from_conclusion(Some("TIMED_OUT")),
            CheckState::Failure
        );
        assert_eq!(
            CheckState::from_conclusion(Some("STARTUP_FAILURE")),
            CheckState::Failure
        );
        assert_eq!(CheckState::from_conclusion(Some("")), CheckState::Pending);
        assert_eq!(
            CheckState::from_conclusion(Some("SOMETHING_NEW")),
            CheckState::Unknown
        );
        assert_eq!(CheckState::from_status(Some("ERROR")), CheckState::Failure);
        assert_eq!(
            CheckState::from_status_field(Some("IN_PROGRESS")),
            CheckState::Pending
        );
    }

    #[test]
    fn a_single_failure_dominates_the_summary() {
        let runs = vec![
            CheckRun {
                name: "a".to_owned(),
                state: CheckState::Success,
                lifecycle: CheckLifecycle::Completed,
                conclusion: None,
                url: None,
                description: None,
            },
            CheckRun {
                name: "b".to_owned(),
                state: CheckState::Pending,
                lifecycle: CheckLifecycle::Queued,
                conclusion: None,
                url: None,
                description: None,
            },
            CheckRun {
                name: "c".to_owned(),
                state: CheckState::Failure,
                lifecycle: CheckLifecycle::Completed,
                conclusion: None,
                url: None,
                description: None,
            },
        ];
        let summary = CheckSummary::from_runs(&runs);
        assert_eq!(summary.state, CheckState::Failure);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.total, 3);
        assert_eq!(summary.label(), "1/3");
    }

    #[test]
    fn a_pending_check_dominates_a_successful_one() {
        let runs = vec![
            CheckRun {
                name: "a".to_owned(),
                state: CheckState::Success,
                lifecycle: CheckLifecycle::Completed,
                conclusion: None,
                url: None,
                description: None,
            },
            CheckRun {
                name: "b".to_owned(),
                state: CheckState::Pending,
                lifecycle: CheckLifecycle::Queued,
                conclusion: None,
                url: None,
                description: None,
            },
        ];
        assert_eq!(CheckSummary::from_runs(&runs).state, CheckState::Pending);
    }

    #[test]
    fn no_checks_is_not_green() {
        let summary = CheckSummary::from_runs(&[]);
        assert!(summary.is_empty());
        assert_eq!(summary.label(), "—");
        assert_eq!(summary.state, CheckState::Unknown);
        assert!(!summary.state.is_success());
    }

    #[test]
    fn an_unknown_conclusion_does_not_read_as_success() {
        let runs = vec![CheckRun {
            name: "a".to_owned(),
            state: CheckState::Unknown,
            lifecycle: CheckLifecycle::Unknown,
            conclusion: None,
            url: None,
            description: None,
        }];
        let summary = CheckSummary::from_runs(&runs);
        assert_eq!(summary.state, CheckState::Unknown);
        assert_eq!(summary.label(), "0/1");
    }

    #[test]
    fn size_and_age_labels_read_like_the_list() {
        let mut pr = summary();
        assert_eq!(pr.size_label(), "+18 −4");
        assert_eq!(pr.updated_label(at(0)), "30m");

        pr.additions = 0;
        assert_eq!(pr.size_label(), "−4");
        pr.deletions = 0;
        assert_eq!(pr.size_label(), "0");
    }

    #[test]
    fn drafts_are_recognised_however_the_author_spells_it() {
        let mut pr = summary();
        assert!(!pr.is_work_in_progress());

        pr.is_draft = true;
        assert!(pr.is_work_in_progress());

        pr.is_draft = false;
        for title in [
            "WIP: something",
            "wip something",
            "[WIP] something",
            "Draft: x",
        ] {
            pr.title = title.to_owned();
            assert!(pr.is_work_in_progress(), "{title} should be a draft");
        }
        pr.title = "Wiping the slate".to_owned();
        assert!(
            !pr.is_work_in_progress(),
            "a word starting with WIP is not WIP"
        );
    }

    #[test]
    fn client_side_search_covers_number_title_author_branches_and_labels() {
        let pr = summary();
        for query in [
            "141",
            "billing",
            "bruno",
            "feat/billing",
            "refactor",
            "billing refactor",
        ] {
            assert!(pr.matches(query), "{query} should match");
        }
        assert!(!pr.matches("postgres"), "an unrelated word must not match");
    }

    #[test]
    fn review_counts_separate_approvals_from_objections() {
        let detail = PullRequestDetail {
            summary: summary(),
            body: String::new(),
            merge_state_status: None,
            reviewers: Vec::new(),
            commits: Vec::new(),
            checks: Vec::new(),
            reviews: vec![
                Review {
                    author: "a".to_owned(),
                    state: ReviewState::Approved,
                    body: String::new(),
                    submitted_at: None,
                },
                Review {
                    author: "b".to_owned(),
                    state: ReviewState::ChangesRequested,
                    body: String::new(),
                    submitted_at: None,
                },
                Review {
                    author: "c".to_owned(),
                    state: ReviewState::Commented,
                    body: String::new(),
                    submitted_at: None,
                },
            ],
            comments: Vec::new(),
            conversation: Vec::new(),
            base_sha: None,
        };
        assert_eq!(detail.review_counts(), (1, 1));
    }
}
