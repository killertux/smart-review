//! The forge port: everything the application needs from GitHub (ARCH-2).
//!
//! One trait, implemented by the `gh` CLI adapter in M1 and by nothing else in
//! v1. Every method takes a [`Cancel`] because every one of them runs a process
//! that the user must be able to abandon (NFR-1.4).

use crate::domain::diff::Patch;
use crate::domain::pr::{CheckRun, PullRequestDetail, PullRequestSummary, Review, ReviewComment};
use crate::domain::query::PrQuery;
use std::sync::Arc;

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
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
}

/// What probing the forge installation found (FR-1.1).
///
/// Three states rather than "installed or an error", because "not installed" and
/// "installed but not logged in" need different sentences and different next steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeStatus {
    /// No `gh` (or not executable).
    Missing,
    /// `gh` is there but not logged in; the detail is what it said.
    Unauthenticated {
        /// What is installed, so `:doctor` can still report the version.
        install: crate::domain::environment::GhInstall,
        /// `gh`'s own message, which distinguishes "logged out" from
        /// "logged in to another host".
        detail: String,
    },
    /// `gh` is installed and logged in.
    Ready(crate::domain::environment::GhInstall),
}

impl ForgeStatus {
    /// The installation, whichever state it is in.
    #[must_use]
    pub fn install(&self) -> Option<&crate::domain::environment::GhInstall> {
        match self {
            Self::Missing => None,
            Self::Unauthenticated { install, .. } | Self::Ready(install) => Some(install),
        }
    }

    /// Whether the forge is usable.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }
}

/// Builds a forge for a repository.
///
/// The forge cannot be constructed until detection has resolved *which* repository,
/// so the composition root supplies a factory instead of an instance. That is what
/// keeps `tui` from having to name `GhCliForge` (ARCH-1).
pub trait ForgeFactory: std::fmt::Debug + Send + Sync {
    /// A forge bound to `repo`.
    fn forge(&self, repo: &crate::domain::repo::RepoId) -> Arc<dyn ForgePort>;
}

/// Probes the forge client without needing a repository.
///
/// Separate from [`ForgePort`] because detection runs *before* the repository is
/// known: it may come from `--repo`, and the user still has to be told that `gh` is
/// missing.
pub trait ForgeProbe: std::fmt::Debug + Send + Sync {
    /// Checks what is installed and whether it is logged in.
    ///
    /// # Errors
    ///
    /// Returns a message when the probe itself could not be run, which is rarer
    /// than a missing or logged-out client and is reported separately.
    fn probe(&self, cancel: &Cancel) -> Result<ForgeStatus, String>;
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
    fn a_page_below_the_limit_is_complete_and_known_to_be_so() {
        let page = PullRequestPage::complete(vec![summary(1), summary(2)], 50);
        assert_eq!(page.total, Some(2));
        assert!(!page.may_have_more());
    }

    #[test]
    fn a_full_page_is_honest_about_not_knowing_the_total() {
        // The requirement is explicit: never silently truncate. Until the count
        // arrives, the most that can be said is that there are at least this many.
        let items: Vec<PullRequestSummary> = (1..=50).map(summary).collect();
        let page = PullRequestPage::possibly_truncated(items, 50);
        assert_eq!(page.total, None);
        assert!(page.may_have_more());
    }

    #[test]
    fn an_empty_page_is_complete_rather_than_unknown() {
        let page = PullRequestPage::complete(Vec::new(), 50);
        assert_eq!(page.total, Some(0));
        assert!(!page.may_have_more());
    }
}
