//! The shapes `gh --json` produces, and how they become domain types.
//!
//! Kept apart from the command construction so that the mapping can be tested
//! against committed fixtures from a real `gh`, without running anything.
//!
//! Everything optional stays optional and every unknown value degrades to
//! something honest: a missing timestamp, an absent rollup, a conclusion this
//! build has never heard of. GitHub adds fields and values over time, and a PR
//! list that fails to render because one check reported a new conclusion would be
//! a worse outcome than one that shows "unknown" (FR-2.4).

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::domain::pr::{
    CheckRun, CheckState, CheckSummary, Commit, PrState, PullRequestDetail, PullRequestSummary,
    Review, ReviewComment, ReviewDecision, ReviewState,
};
use crate::domain::time::Timestamp;

/// The JSON field list for `gh pr list` (FR-2.1).
pub const LIST_FIELDS: &str = "number,title,author,createdAt,updatedAt,isDraft,baseRefName,headRefName,headRefOid,additions,deletions,changedFiles,labels,reviewDecision,statusCheckRollup,url,isCrossRepository";

/// The JSON field list for `gh pr view`.
///
/// `baseRefOid` is deliberately absent: `gh` 2.45 does not accept it, and the base
/// SHA has to come from `git merge-base` (FR-2.4, Appendix A).
pub const DETAIL_FIELDS: &str = "number,title,body,author,state,isDraft,baseRefName,headRefName,headRefOid,isCrossRepository,createdAt,updatedAt,mergeStateStatus,additions,deletions,changedFiles,labels,reviewDecision,url,reviewRequests,reviews,commits,statusCheckRollup";

/// `gh`'s author object.
#[derive(Debug, Clone, Deserialize)]
pub struct GhAuthor {
    /// The login, which is what the list shows.
    pub login: Option<String>,
    /// The display name, kept as a fallback for a deleted account.
    pub name: Option<String>,
}

impl GhAuthor {
    /// The best name available.
    fn display(&self) -> String {
        self.login
            .clone()
            .or_else(|| self.name.clone())
            .unwrap_or_else(|| "ghost".to_owned())
    }
}

/// `gh`'s label object.
#[derive(Debug, Clone, Deserialize)]
pub struct GhLabel {
    /// The label text.
    pub name: Option<String>,
}

/// One entry of `statusCheckRollup`, which mixes check runs and status contexts.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhRollupItem {
    /// `CheckRun` or `StatusContext`.
    #[serde(rename = "__typename")]
    pub typename: Option<String>,
    /// A check run's name.
    pub name: Option<String>,
    /// A status context's name.
    pub context: Option<String>,
    /// A check run's conclusion, once completed.
    pub conclusion: Option<String>,
    /// A check run's lifecycle state.
    pub status: Option<String>,
    /// A status context's state.
    pub state: Option<String>,
    /// Where to read the result.
    pub details_url: Option<String>,
    /// Where to read a status context's result.
    pub target_url: Option<String>,
    /// A one-line description, when GitHub sends one.
    pub description: Option<String>,
}

impl GhRollupItem {
    /// This entry as a domain check.
    #[must_use]
    pub fn into_check(self) -> CheckRun {
        let is_check_run = self.typename.as_deref() != Some("StatusContext");
        let state = if is_check_run {
            // A check that has not completed has no conclusion to read, so its
            // lifecycle state decides; a completed one is judged by its
            // conclusion.
            if self
                .status
                .as_deref()
                .is_some_and(|status| !status.eq_ignore_ascii_case("COMPLETED"))
            {
                CheckState::from_status_field(self.status.as_deref())
            } else {
                CheckState::from_conclusion(self.conclusion.as_deref())
            }
        } else {
            CheckState::from_status(self.state.as_deref())
        };

        CheckRun {
            name: self
                .name
                .or(self.context)
                .unwrap_or_else(|| "unnamed check".to_owned()),
            state,
            url: self.details_url.or(self.target_url),
            description: self.description.filter(|text| !text.is_empty()),
        }
    }
}

/// A PR as `gh pr list` reports it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhSummary {
    /// The number.
    pub number: u64,
    /// The title.
    pub title: String,
    /// The author.
    pub author: Option<GhAuthor>,
    /// When it was opened.
    pub created_at: Option<String>,
    /// When it last changed.
    pub updated_at: Option<String>,
    /// Whether it is a draft.
    pub is_draft: Option<bool>,
    /// The target branch.
    pub base_ref_name: Option<String>,
    /// The source branch.
    pub head_ref_name: Option<String>,
    /// The head commit.
    pub head_ref_oid: Option<String>,
    /// Lines added.
    pub additions: Option<u64>,
    /// Lines deleted.
    pub deletions: Option<u64>,
    /// Files changed.
    pub changed_files: Option<u64>,
    /// Labels.
    pub labels: Option<Vec<GhLabel>>,
    /// The review decision.
    pub review_decision: Option<String>,
    /// The check rollup.
    pub status_check_rollup: Option<Vec<GhRollupItem>>,
    /// The browser URL.
    pub url: Option<String>,
    /// Whether the head branch is in a fork.
    pub is_cross_repository: Option<bool>,
    /// Open, closed or merged. Only `gh pr view` sends it.
    pub state: Option<String>,
}

impl GhSummary {
    /// Converts to the domain type the UI uses.
    #[must_use]
    pub fn into_domain(self) -> PullRequestSummary {
        let checks: Vec<CheckRun> = self
            .status_check_rollup
            .unwrap_or_default()
            .into_iter()
            .map(GhRollupItem::into_check)
            .collect();
        let checks = CheckSummary::from_runs(&checks);

        PullRequestSummary {
            number: self.number,
            title: self.title,
            author: self
                .author
                .as_ref()
                .map_or_else(|| "ghost".to_owned(), GhAuthor::display),
            state: self.state.as_deref().map_or(PrState::Open, PrState::parse),
            is_draft: self.is_draft.unwrap_or(false),
            base_ref: self.base_ref_name.unwrap_or_default(),
            head_ref: self.head_ref_name.unwrap_or_default(),
            head_sha: self.head_ref_oid.unwrap_or_default(),
            created_at: parse_timestamp(self.created_at.as_deref()),
            updated_at: parse_timestamp(self.updated_at.as_deref()),
            additions: self.additions.unwrap_or(0),
            deletions: self.deletions.unwrap_or(0),
            changed_files: self.changed_files.unwrap_or(0),
            labels: self
                .labels
                .unwrap_or_default()
                .into_iter()
                .filter_map(|label| label.name)
                .collect(),
            review_decision: ReviewDecision::parse(self.review_decision.as_deref()),
            checks,
            url: self.url.unwrap_or_default(),
            is_cross_repository: self.is_cross_repository.unwrap_or(false),
        }
    }
}

/// `gh`'s commit object.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhCommit {
    /// The SHA.
    pub oid: Option<String>,
    /// The first line of the message.
    pub message_headline: Option<String>,
    /// When it was committed.
    pub committed_date: Option<String>,
    /// Its authors.
    pub authors: Option<Vec<GhAuthor>>,
}

impl GhCommit {
    /// Converts to the domain type.
    #[must_use]
    pub fn into_domain(self) -> Commit {
        Commit {
            sha: self.oid.unwrap_or_default(),
            summary: self.message_headline.unwrap_or_default(),
            author: self
                .authors
                .unwrap_or_default()
                .first()
                .map_or_else(|| "unknown".to_owned(), GhAuthor::display),
            committed_at: parse_timestamp(self.committed_date.as_deref()),
        }
    }
}

/// A review request, which may be for a person or a team.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhReviewRequest {
    /// `User` or `Team`.
    #[serde(rename = "__typename")]
    pub typename: Option<String>,
    /// A person's login.
    pub login: Option<String>,
    /// A team's slug.
    pub slug: Option<String>,
    /// A team's display name.
    pub name: Option<String>,
}

impl GhReviewRequest {
    /// The name to show.
    fn display(self) -> Option<String> {
        if self.typename.as_deref() == Some("Team") {
            return self.name.or(self.slug);
        }
        self.login.or(self.name)
    }
}

/// A submitted review.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhReview {
    /// Who wrote it.
    pub author: Option<GhAuthor>,
    /// The decision.
    pub state: Option<String>,
    /// The body.
    pub body: Option<String>,
    /// When it was submitted.
    pub submitted_at: Option<String>,
}

impl GhReview {
    /// Converts to the domain type.
    #[must_use]
    pub fn into_domain(self) -> Review {
        Review {
            author: self
                .author
                .as_ref()
                .map_or_else(|| "ghost".to_owned(), GhAuthor::display),
            state: self
                .state
                .as_deref()
                .map_or(ReviewState::Unknown, ReviewState::parse),
            body: self.body.unwrap_or_default(),
            submitted_at: self
                .submitted_at
                .as_deref()
                .map(|at| parse_timestamp(Some(at))),
        }
    }
}

/// The shape of `gh pr view --json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhDetail {
    /// Everything the list also needs.
    #[serde(flatten)]
    pub summary: GhSummary,
    /// The description.
    pub body: Option<String>,
    /// The merge state, e.g. `CLEAN` or `BLOCKED`.
    pub merge_state_status: Option<String>,
    /// Who has been asked to review.
    pub review_requests: Option<Vec<GhReviewRequest>>,
    /// Submitted reviews.
    pub reviews: Option<Vec<GhReview>>,
    /// The commits.
    pub commits: Option<Vec<GhCommit>>,
}

impl GhDetail {
    /// Converts to the domain type.
    ///
    /// `comments` and `base_sha` stay empty here: inline comments are a separate
    /// call, and the base SHA needs `git merge-base` (FR-2.4, Appendix A).
    #[must_use]
    pub fn into_domain(self) -> PullRequestDetail {
        let checks: Vec<CheckRun> = self
            .summary
            .status_check_rollup
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(GhRollupItem::into_check)
            .collect();
        let reviewers = self
            .review_requests
            .unwrap_or_default()
            .into_iter()
            .filter_map(GhReviewRequest::display)
            .collect();

        PullRequestDetail {
            summary: self.summary.into_domain(),
            body: self.body.unwrap_or_default(),
            merge_state_status: self.merge_state_status,
            reviewers,
            commits: self
                .commits
                .unwrap_or_default()
                .into_iter()
                .map(GhCommit::into_domain)
                .collect(),
            checks,
            reviews: self
                .reviews
                .unwrap_or_default()
                .into_iter()
                .map(GhReview::into_domain)
                .collect(),
            comments: Vec::new(),
            base_sha: None,
        }
    }
}

/// An inline review comment from `gh api .../pulls/N/comments`.
#[derive(Debug, Clone, Deserialize)]
pub struct GhReviewComment {
    /// The comment id.
    pub id: u64,
    /// Who wrote it.
    pub user: Option<GhAuthor>,
    /// The file it is anchored to.
    pub path: Option<String>,
    /// The line, when GitHub still knows it.
    pub line: Option<u64>,
    /// `LEFT` or `RIGHT`.
    pub side: Option<String>,
    /// The body.
    pub body: Option<String>,
    /// When it was written.
    pub created_at: Option<String>,
    /// The comment it replies to.
    pub in_reply_to_id: Option<u64>,
    /// The hunk GitHub shows above it.
    pub diff_hunk: Option<String>,
    /// The browser URL.
    pub html_url: Option<String>,
}

impl GhReviewComment {
    /// Converts to the domain type.
    #[must_use]
    pub fn into_domain(self) -> ReviewComment {
        ReviewComment {
            id: self.id,
            author: self
                .user
                .as_ref()
                .map_or_else(|| "ghost".to_owned(), GhAuthor::display),
            path: self.path.unwrap_or_default(),
            line: self.line,
            side: self.side,
            body: self.body.unwrap_or_default(),
            created_at: parse_timestamp(self.created_at.as_deref()),
            in_reply_to: self.in_reply_to_id,
            diff_hunk: self.diff_hunk,
            url: self.html_url,
        }
    }
}

/// The envelope `gh api graphql` returns.
#[derive(Debug, Clone, Deserialize)]
pub struct GraphQlResponse {
    /// The `data` payload.
    pub data: Option<GraphQlData>,
    /// GraphQL-level errors, which arrive with HTTP 200.
    pub errors: Option<Vec<GraphQlError>>,
}

/// The part of the payload we ask for.
#[derive(Debug, Clone, Deserialize)]
pub struct GraphQlData {
    /// The search result, when the query was a search.
    pub search: Option<GraphQlSearch>,
}

/// A search result count.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQlSearch {
    /// How many results exist in total.
    pub issue_count: Option<u32>,
}

/// One GraphQL error.
#[derive(Debug, Clone, Deserialize)]
pub struct GraphQlError {
    /// The message.
    pub message: Option<String>,
}

/// Parses a timestamp, degrading to the Unix epoch rather than failing.
///
/// GitHub always sends RFC 3339 UTC. If that ever changes, an admittedly odd
/// "1970" in one column is a better outcome than a PR list that will not open.
#[must_use]
pub fn parse_timestamp(value: Option<&str>) -> Timestamp {
    value
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map_or_else(DateTime::<Utc>::default, |parsed| {
            parsed.with_timezone(&Utc)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = include_str!("../../../tests/fixtures/gh/pr-list.json");
    const VIEW: &str = include_str!("../../../tests/fixtures/gh/pr-view.json");
    const COMMENTS: &str = include_str!("../../../tests/fixtures/gh/review-comments.json");
    const COUNT: &str = include_str!("../../../tests/fixtures/gh/graphql-count.json");

    fn list() -> Vec<GhSummary> {
        serde_json::from_str(LIST).unwrap()
    }

    #[test]
    fn a_list_response_maps_every_field_the_ui_shows() {
        let prs: Vec<PullRequestSummary> = list().into_iter().map(GhSummary::into_domain).collect();
        assert_eq!(prs.len(), 3);

        let first = &prs[0];
        assert_eq!(first.number, 142);
        assert_eq!(first.title, "Add retry to the webhook dispatcher");
        assert_eq!(first.author, "alice");
        assert_eq!(first.state, PrState::Open);
        assert!(!first.is_draft);
        assert_eq!(first.base_ref, "main");
        assert_eq!(first.head_ref, "feat/webhook-retry");
        assert_eq!(first.head_sha, "5e44fd9d2e1d4b6f9f0a1c7d3b2a4e5f60718293");
        assert_eq!(first.additions, 218);
        assert_eq!(first.deletions, 43);
        assert_eq!(first.changed_files, 7);
        assert_eq!(first.labels, vec!["backend", "needs review"]);
        assert_eq!(first.review_decision, Some(ReviewDecision::Approved));
        assert_eq!(first.checks.passed, 3);
        assert_eq!(first.checks.total, 3);
        assert_eq!(first.checks.state, CheckState::Success);
        assert_eq!(first.updated_label(first.updated_at), "just now");
        assert!(!first.is_cross_repository);
    }

    #[test]
    fn a_draft_with_a_failed_check_is_reported_as_such() {
        let prs: Vec<PullRequestSummary> = list().into_iter().map(GhSummary::into_domain).collect();
        let draft = &prs[1];
        assert!(draft.is_draft);
        assert!(draft.is_work_in_progress());
        assert_eq!(
            draft.review_decision,
            Some(ReviewDecision::ChangesRequested)
        );
        assert_eq!(draft.checks.state, CheckState::Failure);
        assert_eq!(draft.checks.passed, 1);
        assert_eq!(draft.checks.total, 3);
        assert_eq!(draft.checks.label(), "1/3");
    }

    #[test]
    fn a_pr_with_no_checks_is_not_reported_as_green() {
        let prs: Vec<PullRequestSummary> = list().into_iter().map(GhSummary::into_domain).collect();
        let dependabot = &prs[2];
        assert!(dependabot.checks.is_empty());
        assert_eq!(dependabot.checks.label(), "—");
        assert_eq!(dependabot.checks.state, CheckState::Unknown);
        assert_eq!(dependabot.review_decision, None);
        assert!(dependabot.is_cross_repository, "the head branch is a fork");
        assert_eq!(dependabot.author, "dependabot");
    }

    #[test]
    fn both_rollup_spellings_become_checks() {
        let prs: Vec<PullRequestSummary> = list().into_iter().map(GhSummary::into_domain).collect();
        // The fixture's first PR has two CheckRuns and one StatusContext.
        assert_eq!(prs[0].checks.total, 3);
    }

    #[test]
    fn an_in_progress_check_is_pending_rather_than_unknown() {
        let items: Vec<GhRollupItem> = list()[1].status_check_rollup.clone().unwrap();
        let pending = items[1].clone().into_check();
        assert_eq!(pending.name, "ci / test (macos-latest)");
        assert_eq!(
            pending.state,
            CheckState::Pending,
            "IN_PROGRESS is not a verdict"
        );
        assert!(pending.url.is_some());
    }

    #[test]
    fn a_status_context_is_read_from_its_state_field() {
        let items: Vec<GhRollupItem> = list()[0].status_check_rollup.clone().unwrap();
        let context = items[2].clone().into_check();
        assert_eq!(context.name, "ci/circleci: build");
        assert_eq!(context.state, CheckState::Success);
    }

    #[test]
    fn an_unknown_conclusion_is_not_silently_treated_as_success() {
        let item = GhRollupItem {
            typename: Some("CheckRun".to_owned()),
            name: Some("future check".to_owned()),
            context: None,
            conclusion: Some("SOMETHING_ELSE".to_owned()),
            status: Some("COMPLETED".to_owned()),
            state: None,
            details_url: None,
            target_url: None,
            description: None,
        };
        assert_eq!(item.into_check().state, CheckState::Unknown);
    }

    #[test]
    fn a_check_that_is_still_running_ignores_its_empty_conclusion() {
        let item = GhRollupItem {
            typename: Some("CheckRun".to_owned()),
            name: Some("running".to_owned()),
            context: None,
            conclusion: Some(String::new()),
            status: Some("IN_PROGRESS".to_owned()),
            state: None,
            details_url: None,
            target_url: None,
            description: None,
        };
        assert_eq!(item.into_check().state, CheckState::Pending);
    }

    #[test]
    fn a_detail_response_maps_commits_reviews_reviewers_and_checks() {
        let detail: PullRequestDetail = serde_json::from_str::<GhDetail>(VIEW)
            .unwrap()
            .into_domain();

        assert_eq!(detail.summary.number, 141);
        assert!(detail.summary.is_draft);
        assert!(detail.body.contains("Splits `Invoice`"));
        assert_eq!(detail.merge_state_status.as_deref(), Some("BLOCKED"));
        assert_eq!(
            detail.reviewers,
            vec!["alice", "platform"],
            "team names too"
        );
        assert_eq!(detail.commits.len(), 2);
        assert_eq!(
            detail.commits[0].summary,
            "Split Invoice into domain and projection"
        );
        assert_eq!(detail.commits[0].author, "bruno");
        assert_eq!(detail.reviews.len(), 3);
        assert_eq!(detail.reviews[0].state, ReviewState::ChangesRequested);
        assert!(detail.reviews[0].body.contains("needs a test"));
        assert_eq!(detail.reviews[2].state, ReviewState::Commented);
        assert!(detail.reviews[2].submitted_at.is_none());
        assert_eq!(detail.checks.len(), 3);
        assert_eq!(detail.review_counts(), (1, 1));

        // Filled in by the caller, not by this mapping.
        assert!(detail.comments.is_empty());
        assert!(detail.base_sha.is_none(), "gh cannot report baseRefOid");
    }

    #[test]
    fn inline_comments_map_including_threads() {
        let comments: Vec<ReviewComment> = serde_json::from_str::<Vec<GhReviewComment>>(COMMENTS)
            .unwrap()
            .into_iter()
            .map(GhReviewComment::into_domain)
            .collect();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].author, "alice");
        assert_eq!(comments[0].path, "src/domain/invoice.rs");
        assert_eq!(comments[0].line, Some(31));
        assert_eq!(comments[0].side.as_deref(), Some("RIGHT"));
        assert!(comments[0].in_reply_to.is_none());
        assert!(comments[0].diff_hunk.is_some());
        assert_eq!(
            comments[1].in_reply_to,
            Some(1001),
            "a reply keeps its parent"
        );
    }

    #[test]
    fn a_graphql_count_is_read_from_its_envelope() {
        let response: GraphQlResponse = serde_json::from_str(COUNT).unwrap();
        assert_eq!(
            response
                .data
                .and_then(|data| data.search)
                .and_then(|search| search.issue_count),
            Some(137)
        );
    }

    #[test]
    fn a_graphql_error_envelope_is_not_mistaken_for_a_count() {
        let response: GraphQlResponse = serde_json::from_str(
            "{\"data\":null,\"errors\":[{\"message\":\"Could not resolve to a Repository\"}]}",
        )
        .unwrap();
        assert!(response.data.is_none());
        assert_eq!(
            response.errors.unwrap()[0].message.as_deref(),
            Some("Could not resolve to a Repository")
        );
    }

    #[test]
    fn a_missing_field_degrades_instead_of_failing() {
        let minimal: GhSummary = serde_json::from_str(r#"{"number":1,"title":"Bare"}"#).unwrap();
        let pr = minimal.into_domain();
        assert_eq!(pr.number, 1);
        assert_eq!(pr.author, "ghost");
        assert_eq!(pr.additions, 0);
        assert!(pr.labels.is_empty());
        assert_eq!(pr.review_decision, None);
        assert!(pr.checks.is_empty());
        assert_eq!(pr.state, PrState::Open, "an absent state means open");
    }

    #[test]
    fn a_malformed_timestamp_does_not_lose_the_pull_request() {
        let broken: GhSummary = serde_json::from_str(
            r#"{"number":2,"title":"Odd","createdAt":"yesterday","updatedAt":""}"#,
        )
        .unwrap();
        let pr = broken.into_domain();
        assert_eq!(pr.number, 2);
        assert_eq!(pr.created_at, DateTime::<Utc>::default());
    }

    #[test]
    fn a_deleted_account_still_has_a_name() {
        let author: GhAuthor =
            serde_json::from_str(r#"{"login":null,"name":"Ghost User"}"#).unwrap();
        assert_eq!(author.display(), "Ghost User");

        let nameless: GhAuthor = serde_json::from_str("{}").unwrap();
        assert_eq!(nameless.display(), "ghost");
    }

    #[test]
    fn the_field_lists_do_not_ask_for_base_ref_oid() {
        // Verified on gh 2.45: asking for it makes the whole command fail.
        assert!(!LIST_FIELDS.contains("baseRefOid"));
        assert!(!DETAIL_FIELDS.contains("baseRefOid"));
        assert!(DETAIL_FIELDS.contains("mergeStateStatus"));
        assert!(DETAIL_FIELDS.contains("reviewRequests"));
    }
}
