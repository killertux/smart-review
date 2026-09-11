//! The forge port: everything the application needs from GitHub (ARCH-2).
//!
//! One trait, implemented by the `gh` CLI adapter in M1 and by nothing else in
//! v1. Every method takes a [`Cancel`] because every one of them runs a process
//! that the user must be able to abandon (NFR-1.4).

use crate::domain::diff::Patch;
use crate::domain::pr::{CheckRun, PullRequestDetail, PullRequestSummary, Review, ReviewComment};
use crate::domain::query::PrQuery;
use crate::ports::Cancel;

/// What the forge can do, so the UI can hide what it cannot (ARCH-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForgeCapabilities {
    /// Whether a review can be submitted with inline comments in one call.
    pub batched_review: bool,
    /// Whether inline review comments can be read.
    pub inline_comments: bool,
}

impl Default for ForgeCapabilities {
    fn default() -> Self {
        Self {
            batched_review: true,
            inline_comments: true,
        }
    }
}

/// A page of pull requests, with what is known about the total.
///
/// The total is deliberately optional: GitHub does not report a count alongside
/// `gh pr list`, so it is only known when the page was not full (in which case the
/// count is the page size) or after a separate count query. An unknown total is
/// reported as such rather than guessed at (FR-2.1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PullRequestPage {
    /// The PRs, newest first.
    pub items: Vec<PullRequestSummary>,
    /// How many the request asked for.
    pub limit: u32,
    /// How many exist in total, when it is known.
    pub total: Option<u32>,
}

impl PullRequestPage {
    /// A page whose size is below the limit, so it is known to be complete.
    #[must_use]
    pub fn complete(items: Vec<PullRequestSummary>, limit: u32) -> Self {
        let total = u32::try_from(items.len()).unwrap_or(u32::MAX);
        Self {
            items,
            limit,
            total: Some(total),
        }
    }

    /// A page that filled the limit, so more may exist.
    #[must_use]
    pub fn possibly_truncated(items: Vec<PullRequestSummary>, limit: u32) -> Self {
        Self {
            items,
            limit,
            total: None,
        }
    }

    /// Whether there may be more PRs than were fetched.
    #[must_use]
    pub fn may_have_more(&self) -> bool {
        u32::try_from(self.items.len()).is_ok_and(|count| count >= self.limit)
    }

    /// The phrase the status line shows: "showing 50 of ≥137" and friends
    /// (FR-2.1).
    #[must_use]
    pub fn status_label(&self) -> String {
        let shown = self.items.len();
        match self.total {
            Some(total) if usize::try_from(total).unwrap_or(usize::MAX) > shown => {
                format!("showing {shown} of {total}")
            }
            Some(total) => format!("{total} pull requests"),
            None if self.may_have_more() => format!("showing {shown} of ≥{shown}"),
            None => format!("showing {shown}"),
        }
    }

    /// Adds another page, keeping the first page's total.
    #[must_use]
    pub fn extended_with(mut self, mut next: Self) -> Self {
        self.items.append(&mut next.items);
        self.limit = next.limit;
        if next.total.is_some() {
            self.total = next.total;
        }
        self
    }
}

/// Anything that can answer questions about a pull request.
pub trait ForgePort: std::fmt::Debug + Send + Sync {
    /// What this forge supports.
    fn capabilities(&self) -> ForgeCapabilities;

    /// Lists pull requests matching a query (FR-2.1).
    ///
    /// # Errors
    ///
    /// Returns an error when `gh` cannot be run or reports a failure.
    fn list_pull_requests(
        &self,
        query: &PrQuery,
        cancel: &Cancel,
    ) -> crate::Result<PullRequestPage>;

    /// Counts the pull requests matching a query (FR-2.1).
    ///
    /// # Errors
    ///
    /// Returns an error when `gh` cannot be run or reports a failure.
    fn count_pull_requests(&self, query: &PrQuery, cancel: &Cancel) -> crate::Result<u32>;

    /// Fetches one pull request in full (FR-2.4).
    ///
    /// # Errors
    ///
    /// Returns an error when `gh` cannot be run, the PR does not exist, or the
    /// response cannot be understood.
    fn get_pull_request(&self, number: u64, cancel: &Cancel) -> crate::Result<PullRequestDetail>;

    /// Lists the reviews submitted on a PR.
    ///
    /// # Errors
    ///
    /// As [`Self::get_pull_request`].
    fn list_reviews(&self, number: u64, cancel: &Cancel) -> crate::Result<Vec<Review>>;

    /// Lists the inline review comments on a PR.
    ///
    /// # Errors
    ///
    /// As [`Self::get_pull_request`].
    fn list_review_comments(
        &self,
        number: u64,
        cancel: &Cancel,
    ) -> crate::Result<Vec<ReviewComment>>;

    /// Lists the check runs on a PR.
    ///
    /// # Errors
    ///
    /// As [`Self::get_pull_request`].
    fn list_checks(&self, number: u64, cancel: &Cancel) -> crate::Result<Vec<CheckRun>>;

    /// The unified diff of a PR, as text (FR-3.2).
    ///
    /// Remote mode, used until the local workspace exists: the caller parses the
    /// text with [`crate::domain::diff::parse_patch`].
    ///
    /// # Errors
    ///
    /// As [`Self::get_pull_request`].
    fn pull_request_diff(&self, number: u64, cancel: &Cancel) -> crate::Result<String>;

    /// The unified diff of a PR, already parsed.
    ///
    /// # Errors
    ///
    /// As [`Self::pull_request_diff`].
    fn pull_request_patch(&self, number: u64, cancel: &Cancel) -> crate::Result<Patch> {
        Ok(crate::domain::diff::parse_patch(
            &self.pull_request_diff(number, cancel)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pr::{CheckSummary, PrState};

    fn summary(number: u64) -> PullRequestSummary {
        PullRequestSummary {
            number,
            title: format!("PR {number}"),
            author: "bruno".to_owned(),
            state: PrState::Open,
            is_draft: false,
            base_ref: "main".to_owned(),
            head_ref: "topic".to_owned(),
            head_sha: "abc".to_owned(),
            created_at: crate::domain::time::Timestamp::default(),
            updated_at: crate::domain::time::Timestamp::default(),
            additions: 1,
            deletions: 0,
            changed_files: 1,
            labels: Vec::new(),
            review_decision: None,
            checks: CheckSummary::default(),
            url: String::new(),
            is_cross_repository: false,
        }
    }

    #[test]
    fn a_page_below_the_limit_is_complete_and_says_how_many() {
        let page = PullRequestPage::complete(vec![summary(1), summary(2)], 50);
        assert_eq!(page.total, Some(2));
        assert!(!page.may_have_more());
        assert_eq!(page.status_label(), "2 pull requests");
    }

    #[test]
    fn a_full_page_is_honest_about_not_knowing_the_total() {
        // The requirement is explicit: never silently truncate. Until the count
        // arrives, the most that can be said is that there are at least this many.
        let items: Vec<PullRequestSummary> = (1..=50).map(summary).collect();
        let page = PullRequestPage::possibly_truncated(items, 50);
        assert_eq!(page.total, None);
        assert!(page.may_have_more());
        assert_eq!(page.status_label(), "showing 50 of ≥50");
    }

    #[test]
    fn a_known_total_renders_the_sentence_the_requirement_asks_for() {
        let items: Vec<PullRequestSummary> = (1..=50).map(summary).collect();
        let page = PullRequestPage {
            items,
            limit: 50,
            total: Some(137),
        };
        assert_eq!(page.status_label(), "showing 50 of 137");
    }

    #[test]
    fn loading_more_keeps_the_known_total_and_appends() {
        let first = PullRequestPage::possibly_truncated((1..=50).map(summary).collect(), 50);
        let second = PullRequestPage::complete((51..=80).map(summary).collect(), 100);
        let combined = first.extended_with(second);
        assert_eq!(combined.items.len(), 80);
        assert_eq!(combined.total, Some(30), "the second page was not full");
        assert_eq!(combined.limit, 100);
    }

    #[test]
    fn an_empty_page_is_reported_as_zero_rather_than_as_unknown() {
        let page = PullRequestPage::complete(Vec::new(), 50);
        assert_eq!(page.status_label(), "0 pull requests");
        assert!(!page.may_have_more());
    }
}
