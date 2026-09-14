//! The drafting use case (FR-6.1–FR-6.3): stage, keep, and publish exactly once.
//!
//! Three rules shape this module, and all three exist because publishing is the one
//! action in this application that other people can see:
//!
//! - **the draft is saved before anything is sent.** The caller saves it as it is
//!   edited and again immediately before sending, so the answer to "did I lose my
//!   comments?" is never yes, whatever happens next — including the app being killed
//!   mid-publish. This type does not hold the draft: the UI owns the document, and
//!   puts it here to be read, written or sent, which is what keeps a reducer free of
//!   file system work.
//! - **publishing is a single request through the forge port.** The port's contract is
//!   one review, so this module never loops, and there is no path here that posts N
//!   comments and hopes.
//! - **a failed publish keeps the draft.** The only thing that ever clears it is a
//!   success — or the user saying so. An error is the moment the draft matters most,
//!   and the moment it is the easiest to lose.

use crate::domain::draft::{Draft, DraftError};
use crate::domain::repo::RepoId;
use crate::domain::time::Timestamp;
use crate::ports::forge::ForgePort;
use crate::ports::{Cancel, DraftStoreError, DraftStorePort, ReviewPosted};

/// The draft store, as the rest of the application uses it (FR-6.1, FR-6.3).
///
/// Stateless with respect to the draft on purpose: the document is the UI's, and is
/// handed in to be written or sent. A service that owned it would put a file write
/// inside every state transition, including the ones that run on a keypress.
#[derive(Debug)]
pub struct Drafts {
    store: std::sync::Arc<dyn DraftStorePort>,
    repo: RepoId,
}

impl Drafts {
    /// Binds the service to a store and a repository.
    #[must_use]
    pub fn new(store: std::sync::Arc<dyn DraftStorePort>, repo: RepoId) -> Self {
        Self { store, repo }
    }

    /// Loads the draft for a pull request, or starts an empty one (FR-6.1).
    ///
    /// A draft that cannot be read is *reported* and replaced with an empty one: the
    /// alternative is refusing to open the pull request at all because of a file the
    /// user may not know exists. The unreadable file is left where it is, so a build
    /// that can read it still can.
    #[must_use]
    pub fn load(&self, pr: u64, now: Timestamp) -> (Draft, Option<String>) {
        match self.store.load(&self.repo, pr) {
            Ok(Some(draft)) => (draft, None),
            Ok(None) => (Draft::new(pr, now), None),
            Err(error) => (Draft::new(pr, now), Some(error.to_string())),
        }
    }

    /// Writes a draft, replacing whatever was there (FR-6.1).
    ///
    /// # Errors
    ///
    /// Returns the store's error when the document cannot be written.
    pub fn save(&self, draft: &Draft) -> Result<(), DraftStoreError> {
        if draft.is_empty() {
            // Nothing staged, so there is nothing to keep: an empty document per
            // visited pull request is what `:draft list` would then have to filter.
            return self.store.remove(&self.repo, draft.pr);
        }
        self.store.save(&self.repo, draft)
    }

    /// Deletes the stored draft for a pull request (FR-6.1).
    ///
    /// # Errors
    ///
    /// Returns the store's error when a file exists and cannot be removed.
    pub fn remove(&self, pr: u64) -> Result<(), DraftStoreError> {
        self.store.remove(&self.repo, pr)
    }

    /// Every draft this repository has, for `:draft list` (FR-6.1).
    ///
    /// # Errors
    ///
    /// Returns the store's error when the directory cannot be read.
    pub fn list(&self) -> Result<Vec<Draft>, DraftStoreError> {
        self.store.list(&self.repo)
    }

    /// Sends a draft as one review (FR-6.3).
    ///
    /// Does **not** clear the draft on success: clearing is the caller's decision,
    /// and the caller is the one that knows whether the review it just sent is the
    /// review it is still showing.
    ///
    /// # Errors
    ///
    /// Returns [`PublishError::Refused`] when the draft cannot be sent, and
    /// [`PublishError::Forge`] when GitHub or the network refused.
    pub fn publish(
        &self,
        forge: &dyn ForgePort,
        draft: &Draft,
        cancel: &Cancel,
    ) -> Result<ReviewPosted, PublishError> {
        draft.publishable().map_err(PublishError::Refused)?;
        forge
            .submit_review(draft.pr, draft, cancel)
            .map_err(PublishError::Forge)
    }
}

/// Why a publish did not happen (FR-6.3).
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// The draft is not something that can be sent.
    #[error("{0}")]
    Refused(#[from] DraftError),
    /// The forge refused, in its own words (translated by the adapter).
    #[error("{0}")]
    Forge(#[source] crate::Error),
}

impl PublishError {
    /// Whether the failure was GitHub's rather than this application's.
    #[must_use]
    pub fn is_forge(&self) -> bool {
        matches!(self, Self::Forge(_))
    }

    /// The command that failed, when there was one, for FR-9.1's copyable command.
    #[must_use]
    pub fn command(&self) -> Option<&str> {
        match self {
            Self::Forge(error) => error.command(),
            Self::Refused(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::draft::{Decision, DraftComment, Side};
    use crate::domain::time::from_unix_secs;
    use crate::ports::CommentPosted;
    use crate::ports::forge::ReviewPosted;
    use crate::test_support::temp_home;
    use std::sync::Mutex;

    fn now() -> Timestamp {
        from_unix_secs(1_700_000_000)
    }

    fn repo() -> RepoId {
        RepoId::parse("github.com/acme/service").expect("a repository")
    }

    fn drafts(home: &crate::test_support::TempHome) -> Drafts {
        Drafts::new(
            std::sync::Arc::new(crate::adapters::draft_store::FileDraftStore::new(
                home.path(),
            )),
            repo(),
        )
    }

    fn staged(number: u64) -> Draft {
        let mut draft = Draft::new(number, now());
        draft.set_decision(Some(Decision::RequestChanges), now());
        draft.set_body("One thing to fix.", now());
        draft.add(
            DraftComment::new("src/a.rs", Side::New, 31, Some(28), "this rounds up")
                .expect("valid"),
            now(),
        );
        draft
    }

    /// A forge that records what it was asked to publish.
    #[derive(Debug, Default)]
    struct RecordingForge {
        submitted: Mutex<Vec<Draft>>,
        refuse: Mutex<Option<String>>,
        dry_run: bool,
    }

    impl ForgePort for RecordingForge {
        fn capabilities(&self) -> crate::ports::ForgeCapabilities {
            crate::ports::ForgeCapabilities::default()
        }

        fn list_pull_requests(
            &self,
            _query: &crate::domain::query::PrQuery,
            _cancel: &Cancel,
        ) -> crate::Result<crate::ports::PullRequestPage> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn count_pull_requests(
            &self,
            _query: &crate::domain::query::PrQuery,
            _cancel: &Cancel,
        ) -> crate::Result<u32> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn get_pull_request(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> crate::Result<crate::domain::pr::PullRequestDetail> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn list_reviews(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> crate::Result<Vec<crate::domain::pr::Review>> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn list_review_comments(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> crate::Result<Vec<crate::domain::pr::ReviewComment>> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn list_checks(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> crate::Result<Vec<crate::domain::pr::CheckRun>> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn pull_request_diff(&self, _number: u64, _cancel: &Cancel) -> crate::Result<String> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn reply_to_review_comment(
            &self,
            _number: u64,
            _comment_id: u64,
            _body: &str,
            _cancel: &Cancel,
        ) -> crate::Result<CommentPosted> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn comment_on_conversation(
            &self,
            _number: u64,
            _body: &str,
            _cancel: &Cancel,
        ) -> crate::Result<CommentPosted> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn set_thread_resolved(
            &self,
            _thread_id: &str,
            _resolved: bool,
            _cancel: &Cancel,
        ) -> crate::Result<()> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn list_conversation(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> crate::Result<Vec<crate::domain::pr::ConversationComment>> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn submit_review(
            &self,
            _number: u64,
            draft: &Draft,
            _cancel: &Cancel,
        ) -> crate::Result<ReviewPosted> {
            if let Some(reason) = self.refuse.lock().unwrap().clone() {
                return Err(crate::Error::forge("gh api", reason));
            }
            self.submitted.lock().unwrap().push(draft.clone());
            Ok(ReviewPosted {
                id: Some(7),
                url: Some("https://example.test/review/7".to_owned()),
                dry_run: self.dry_run,
            })
        }
    }

    #[test]
    fn a_pull_request_with_no_draft_gets_an_empty_one() {
        let home = temp_home();
        let (draft, warning) = drafts(&home).load(141, now());
        assert_eq!(draft.pr, 141);
        assert!(draft.is_empty());
        assert!(warning.is_none(), "nothing to warn about");
        assert!(drafts(&home).list().expect("listable").is_empty());
    }

    #[test]
    fn a_saved_draft_comes_back() {
        let home = temp_home();
        let service = drafts(&home);
        service.save(&staged(141)).expect("writable");
        let (draft, warning) = service.load(141, now());
        assert!(warning.is_none());
        assert_eq!(draft, staged(141));
        assert_eq!(service.list().expect("listable").len(), 1);
    }

    #[test]
    fn saving_nothing_removes_rather_than_keeps_an_empty_document() {
        // `:draft clear` and "I removed the last comment" both end here, and neither
        // should leave a file that `:draft list` has to filter out.
        let home = temp_home();
        let service = drafts(&home);
        service.save(&staged(141)).expect("writable");
        service.save(&Draft::new(141, now())).expect("removable");
        assert!(service.list().expect("listable").is_empty());
        assert_eq!(service.load(141, now()).0, Draft::new(141, now()));
    }

    #[test]
    fn a_draft_that_cannot_be_read_is_reported_without_losing_the_file() {
        let home = temp_home();
        home.write("drafts/github.com/acme/service/pr-141.json", "{ oops");
        let (draft, warning) = drafts(&home).load(141, now());
        let warning = warning.expect("reported");
        assert!(warning.contains("not a usable draft"), "{warning}");
        assert!(draft.is_empty(), "and the app still opens");
        assert!(
            home.path()
                .join("drafts/github.com/acme/service/pr-141.json")
                .exists(),
            "the file is left for a build that can read it"
        );
    }

    #[test]
    fn publishing_sends_exactly_one_review() {
        let home = temp_home();
        let forge = RecordingForge::default();
        let posted = drafts(&home)
            .publish(&forge, &staged(141), &Cancel::new())
            .expect("sent");
        assert_eq!(posted.id, Some(7));
        assert!(!posted.dry_run);
        assert_eq!(forge.submitted.lock().unwrap().len(), 1, "one review");
    }

    #[test]
    fn a_refused_publish_leaves_the_draft_alone() {
        let home = temp_home();
        let service = drafts(&home);
        let draft = staged(141);
        service.save(&draft).expect("writable");
        let forge = RecordingForge {
            refuse: Mutex::new(Some("your token is not allowed".to_owned())),
            ..RecordingForge::default()
        };

        let error = service
            .publish(&forge, &draft, &Cancel::new())
            .expect_err("refused");
        assert!(error.is_forge());
        assert_eq!(error.command(), Some("gh api"));
        assert_eq!(service.load(141, now()).0, draft, "still stageable");
    }

    #[test]
    fn an_empty_draft_never_reaches_the_forge() {
        let home = temp_home();
        let forge = RecordingForge::default();
        let error = drafts(&home)
            .publish(&forge, &Draft::new(141, now()), &Cancel::new())
            .expect_err("refused");
        assert!(matches!(error, PublishError::Refused(_)));
        assert!(!error.is_forge());
        assert!(forge.submitted.lock().unwrap().is_empty());
    }

    #[test]
    fn a_dry_run_is_reported_as_one() {
        let home = temp_home();
        let forge = RecordingForge {
            dry_run: true,
            ..RecordingForge::default()
        };
        let posted = drafts(&home)
            .publish(&forge, &staged(141), &Cancel::new())
            .expect("recorded");
        assert!(posted.dry_run, "the caller must not clear the draft");
    }

    #[test]
    fn a_draft_from_a_newer_build_is_reported_rather_than_read() {
        let home = temp_home();
        home.write(
            "drafts/github.com/acme/service/pr-141.json",
            r#"{"version": 99, "pr": 141, "updated_at": "2023-11-14T22:13:20Z"}"#,
        );
        let (_, warning) = drafts(&home).load(141, now());
        assert!(warning.expect("reported").contains("newer version"));
    }
}
