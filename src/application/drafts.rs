//! The drafting use case (FR-6.1–FR-6.3): stage, keep, and publish exactly once.
//!
//! Three rules shape this module, and all three exist because publishing is the one
//! action in this application that other people can see:
//!
//! - **the draft is saved before anything is sent.** Every edit writes through to
//!   disk, so the answer to "did I lose my comments?" is never yes, whatever happens
//!   next — including the app being killed mid-publish.
//! - **publishing is a single request through the forge port.** The port's contract is
//!   one review, so this module never loops, and there is no path here that posts N
//!   comments and hopes.
//! - **a failed publish keeps the draft.** The only thing that ever clears it is a
//!   success — or the user saying so. An error is the moment the draft matters most,
//!   and the moment it is the easiest to lose.

use crate::domain::draft::{Draft, DraftComment, DraftError, Side};
use crate::domain::repo::RepoId;
use crate::domain::time::Timestamp;
use crate::logging::{self, Level};
use crate::ports::forge::ForgePort;
use crate::ports::{Cancel, DraftStoreError, DraftStorePort, ReviewPosted};

/// Staged comments, and the rules for keeping them (FR-6.1).
///
/// Owns the draft and the store together, because a draft that was changed but not
/// saved is exactly the state this type exists to make impossible.
#[derive(Debug)]
pub struct Drafter {
    store: Box<dyn DraftStorePort>,
    repo: RepoId,
    draft: Draft,
    /// Whether the last save failed, for the status line to admit to.
    save_error: Option<String>,
}

impl Drafter {
    /// Loads the draft for a pull request, or starts an empty one (FR-6.1).
    ///
    /// A draft that cannot be read is *reported* and replaced with an empty one: the
    /// alternative is refusing to open the pull request at all because of a file the
    /// user may not know exists. The unreadable file is left where it is, and the
    /// error is returned alongside so `:draft` can say what happened.
    #[must_use]
    pub fn open(
        store: Box<dyn DraftStorePort>,
        repo: RepoId,
        pr: u64,
        now: Timestamp,
    ) -> (Self, Option<String>) {
        match store.load(&repo, pr) {
            Ok(Some(draft)) => (
                Self {
                    store,
                    repo,
                    draft,
                    save_error: None,
                },
                None,
            ),
            Ok(None) => (
                Self {
                    store,
                    repo,
                    draft: Draft::new(pr, now),
                    save_error: None,
                },
                None,
            ),
            Err(error) => (
                Self {
                    store,
                    repo,
                    draft: Draft::new(pr, now),
                    save_error: None,
                },
                Some(error.to_string()),
            ),
        }
    }

    /// The draft as it stands.
    #[must_use]
    pub fn draft(&self) -> &Draft {
        &self.draft
    }

    /// The last save failure, if there was one.
    #[must_use]
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// The pull request this drafter is for.
    #[must_use]
    pub fn pr(&self) -> u64 {
        self.draft.pr
    }

    /// Remembers the commit the comments are being written against (FR-6.3).
    ///
    /// Recorded rather than asked for: the diff knows it, and a comment that cannot
    /// say which revision it was written about cannot be checked later.
    pub fn anchor_to(&mut self, head_sha: &str) {
        if self.draft.head_sha.as_deref() == Some(head_sha) || head_sha.is_empty() {
            return;
        }
        // No save: the head reaches disk with the next real edit. Writing a file
        // because a pull request was opened would fill the drafts directory with empty
        // documents for every pull request anyone ever looked at — and the head is not
        // something the user typed, so it is not a change on its own.
        self.draft.head_sha = Some(head_sha.to_owned());
    }

    /// Stages a comment and saves the draft (FR-6.1).
    ///
    /// The comment is validated before it can get near the draft: a refusal is a
    /// sentence in the composer, not a 422 from GitHub after the modal is confirmed.
    ///
    /// # Errors
    ///
    /// As [`DraftComment::new`].
    pub fn stage(
        &mut self,
        path: impl Into<String>,
        side: Side,
        line: u32,
        start_line: Option<u32>,
        body: impl Into<String>,
        now: Timestamp,
    ) -> Result<(), DraftError> {
        let comment = DraftComment::new(path, side, line, start_line, body)?;
        self.draft.insert(comment, now);
        self.save();
        Ok(())
    }

    /// Removes the comment the panel numbers `number` (FR-6.1).
    pub fn remove(&mut self, number: usize, now: Timestamp) -> Option<DraftComment> {
        let removed = self.draft.remove(number, now);
        if removed.is_some() {
            self.save();
        }
        removed
    }

    /// Clears every staged comment, the decision and the body (FR-6.1).
    pub fn clear(&mut self, now: Timestamp) {
        self.draft.clear(now);
        self.save();
    }

    /// Records the decision (FR-6.1).
    pub fn set_decision(
        &mut self,
        decision: Option<crate::domain::draft::Decision>,
        now: Timestamp,
    ) {
        self.draft.set_decision(decision, now);
        self.save();
    }

    /// Records the review body (FR-6.1).
    pub fn set_body(&mut self, body: impl Into<String>, now: Timestamp) {
        self.draft.set_body(body, now);
        self.save();
    }

    /// Writes the draft to the store, remembering a failure rather than raising it.
    ///
    /// A save failure must not abort the edit — the text is still in memory and the
    /// user can retry — but it must never be silent either, which is what
    /// [`Self::save_error`] is for.
    fn save(&mut self) {
        match self.store.save(&self.repo, &self.draft) {
            Ok(()) => self.save_error = None,
            Err(error) => {
                self.save_error = Some(error.to_string());
            }
        }
    }

    /// Forgets the draft once it has been sent (FR-6.3).
    ///
    /// The file is *removed* rather than rewritten empty: a draft that has been sent
    /// is finished with, and leaving an empty document behind would fill the directory
    /// with one file per reviewed pull request — and make `:draft list` claim work
    /// that does not exist.
    ///
    /// A failure here is recorded, not raised: the review is already on GitHub, and no
    /// failure to tidy up afterwards may be reported as a failure to publish.
    fn forget(&mut self, now: Timestamp) {
        self.draft.clear(now);
        match self.store.remove(&self.repo, self.draft.pr) {
            Ok(()) => self.save_error = None,
            Err(error) => {
                logging::log(
                    Level::Warn,
                    format!("the sent draft could not be removed: {error}"),
                );
                self.save_error = Some(error.to_string());
            }
        }
    }

    /// Whether there is anything to publish (FR-6.3).
    ///
    /// # Errors
    ///
    /// As [`Draft::publishable`].
    pub fn publishable(&self) -> Result<(), DraftError> {
        self.draft.publishable()
    }

    /// Sends the draft as one review, and clears it if it landed (FR-6.3).
    ///
    /// # Errors
    ///
    /// Returns [`PublishError::Refused`] when the draft cannot be sent, and
    /// [`PublishError::Forge`] when GitHub or the network refused. In both cases the
    /// draft is untouched: it is the caller's to keep, and it is kept here.
    pub fn publish(
        &mut self,
        forge: &dyn ForgePort,
        now: Timestamp,
        cancel: &Cancel,
    ) -> Result<ReviewPosted, PublishError> {
        self.publishable().map_err(PublishError::Refused)?;

        // Saved immediately before sending, so the record on disk is the review that
        // was sent even if the app dies while the request is in flight.
        self.save();
        if let Some(error) = self.save_error.clone() {
            return Err(PublishError::NotSaved(error));
        }

        let posted = forge
            .submit_review(self.draft.pr, &self.draft, cancel)
            .map_err(PublishError::Forge)?;
        if posted.dry_run {
            // Nothing was sent, so there is nothing to clear: the draft is still the
            // thing the user was about to send (FR-6.5).
            return Ok(posted);
        }
        self.forget(now);
        Ok(posted)
    }

    /// Deletes the stored draft as well as the in-memory one (FR-6.1).
    ///
    /// # Errors
    ///
    /// Returns the store's error when the file cannot be removed.
    pub fn discard(&mut self, now: Timestamp) -> Result<(), DraftStoreError> {
        self.draft.clear(now);
        self.store.remove(&self.repo, self.draft.pr)
    }

    /// The drafts this repository has, for `:draft list` (FR-6.1).
    ///
    /// # Errors
    ///
    /// Returns the store's error when the directory cannot be read.
    pub fn others(&self) -> Result<Vec<Draft>, DraftStoreError> {
        self.store.list(&self.repo)
    }
}

/// Why a publish did not happen (FR-6.3).
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// The draft is not something that can be sent.
    #[error("{0}")]
    Refused(#[from] DraftError),
    /// The draft could not be written, so sending it would lose it.
    #[error("the draft could not be saved, so nothing was sent: {0}")]
    NotSaved(String),
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
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::draft::Decision;
    use crate::domain::time::from_unix_secs;
    use crate::ports::forge::ReviewPosted;
    use crate::test_support::temp_home;
    use std::sync::Mutex;

    fn now() -> Timestamp {
        from_unix_secs(1_700_000_000)
    }

    fn repo() -> RepoId {
        RepoId::parse("github.com/acme/service").expect("a repository")
    }

    /// A drafter over an existing home, as a restart would build it.
    fn open_at(home: &crate::test_support::TempHome, pr: u64) -> Drafter {
        let store: Box<dyn DraftStorePort> = Box::new(
            crate::adapters::draft_store::FileDraftStore::new(home.path()),
        );
        Drafter::open(store, repo(), pr, now()).0
    }

    fn store() -> (Box<dyn DraftStorePort>, crate::test_support::TempHome) {
        let home = temp_home();
        let store: Box<dyn DraftStorePort> = Box::new(
            crate::adapters::draft_store::FileDraftStore::new(home.path()),
        );
        (store, home)
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
    fn an_edit_is_saved_as_it_is_made() {
        let (store, home) = store();
        let (mut drafter, warning) = Drafter::open(store, repo(), 141, now());
        assert!(warning.is_none());
        assert!(drafter.draft().is_empty());

        drafter
            .stage("src/a.rs", Side::New, 31, None, "why?", now())
            .expect("staged");
        let path = home
            .path()
            .join("drafts/github.com/acme/service/pr-141.json");
        assert!(
            path.exists(),
            "the draft is on disk before anything is sent"
        );
        let written = crate::domain::draft::Draft::from_json(
            &std::fs::read_to_string(&path).expect("readable"),
        )
        .expect("a draft");
        assert_eq!(written.comments.len(), 1);

        // Removing one saves again.
        drafter.remove(1, now()).expect("removed");
        let written = crate::domain::draft::Draft::from_json(
            &std::fs::read_to_string(&path).expect("readable"),
        )
        .expect("a draft");
        assert!(written.comments.is_empty());
    }

    #[test]
    fn a_refused_comment_never_reaches_the_draft() {
        let (store, home) = store();
        let (mut drafter, _) = Drafter::open(store, repo(), 141, now());
        assert_eq!(
            drafter
                .stage("src/a.rs", Side::New, 1, None, "   ", now())
                .unwrap_err(),
            DraftError::EmptyBody
        );
        assert_eq!(
            drafter
                .stage("src/a.rs", Side::New, 10, Some(20), "why?", now())
                .unwrap_err(),
            DraftError::BadRange { start: 20, end: 10 }
        );
        assert!(drafter.draft().is_empty());
        assert!(
            !home
                .path()
                .join("drafts/github.com/acme/service/pr-141.json")
                .exists(),
            "a refused comment does not even create the file"
        );
    }

    #[test]
    fn a_reopened_draft_is_the_one_that_was_staged() {
        let home = temp_home();
        let mut drafter = open_at(&home, 141);
        drafter
            .stage("src/a.rs", Side::New, 31, Some(28), "this rounds up", now())
            .expect("staged");
        drafter.set_decision(Some(Decision::RequestChanges), now());
        drafter.set_body("One thing to fix.", now());
        let staged = drafter.draft().clone();
        drop(drafter);

        let reopened = open_at(&home, 141);
        assert_eq!(reopened.draft(), &staged, "a restart is not a lost review");
        assert!(
            open_at(&home, 142).draft().is_empty(),
            "and a different pull request has a draft of its own"
        );
    }

    #[test]
    fn publishing_sends_one_review_and_clears_the_draft() {
        let (store, home) = store();
        let (mut drafter, _) = Drafter::open(store, repo(), 141, now());
        drafter
            .stage("src/a.rs", Side::New, 31, None, "why?", now())
            .expect("staged");
        drafter.set_decision(Some(Decision::RequestChanges), now());
        let forge = RecordingForge::default();

        let posted = drafter
            .publish(&forge, now(), &Cancel::new())
            .expect("sent");
        assert_eq!(posted.id, Some(7));
        assert!(!posted.dry_run);
        assert_eq!(forge.submitted.lock().unwrap().len(), 1, "one review");
        assert!(drafter.draft().is_empty(), "the draft is cleared");
        assert!(
            !home
                .path()
                .join("drafts/github.com/acme/service/pr-141.json")
                .exists(),
            "and so is the file"
        );
    }

    #[test]
    fn a_failed_publish_keeps_everything() {
        let (store, home) = store();
        let (mut drafter, _) = Drafter::open(store, repo(), 141, now());
        drafter
            .stage("src/a.rs", Side::New, 31, None, "why?", now())
            .expect("staged");
        drafter.set_decision(Some(Decision::RequestChanges), now());
        let forge = RecordingForge {
            refuse: Mutex::new(Some("your token is not allowed".to_owned())),
            ..RecordingForge::default()
        };

        let error = drafter
            .publish(&forge, now(), &Cancel::new())
            .expect_err("refused");
        assert!(error.is_forge());
        assert_eq!(error.command(), Some("gh api"));
        assert_eq!(
            drafter.draft().comments.len(),
            1,
            "the comment is still here"
        );
        let path = home
            .path()
            .join("drafts/github.com/acme/service/pr-141.json");
        let written = crate::domain::draft::Draft::from_json(
            &std::fs::read_to_string(&path).expect("still on disk"),
        )
        .expect("a draft");
        assert_eq!(written.comments.len(), 1);
        assert_eq!(written.decision, Some(Decision::RequestChanges));
    }

    #[test]
    fn a_dry_run_sends_nothing_and_keeps_the_draft() {
        let (store, home) = store();
        let (mut drafter, _) = Drafter::open(store, repo(), 141, now());
        drafter
            .stage("src/a.rs", Side::New, 31, None, "why?", now())
            .expect("staged");
        let forge = RecordingForge {
            dry_run: true,
            ..RecordingForge::default()
        };

        let posted = drafter
            .publish(&forge, now(), &Cancel::new())
            .expect("recorded");
        assert!(posted.dry_run);
        assert_eq!(
            drafter.draft().comments.len(),
            1,
            "nothing was sent, so nothing is cleared"
        );
        assert!(
            home.path()
                .join("drafts/github.com/acme/service/pr-141.json")
                .exists()
        );
    }

    #[test]
    fn an_empty_draft_is_refused_before_the_forge_is_asked() {
        let (store, _home) = store();
        let (mut drafter, _) = Drafter::open(store, repo(), 141, now());
        let forge = RecordingForge::default();
        let error = drafter
            .publish(&forge, now(), &Cancel::new())
            .expect_err("refused");
        assert!(matches!(error, PublishError::Refused(_)));
        assert!(!error.is_forge());
        assert!(
            forge.submitted.lock().unwrap().is_empty(),
            "nothing was sent"
        );
    }

    #[test]
    fn a_draft_that_cannot_be_read_is_reported_without_losing_the_file() {
        let home = temp_home();
        home.write("drafts/github.com/acme/service/pr-141.json", "{ oops");
        let store: Box<dyn DraftStorePort> = Box::new(
            crate::adapters::draft_store::FileDraftStore::new(home.path()),
        );
        let (drafter, warning) = Drafter::open(store, repo(), 141, now());
        let warning = warning.expect("the unreadable draft is reported");
        assert!(warning.contains("not a usable draft"), "{warning}");
        assert!(drafter.draft().is_empty(), "and the app still opens");
        assert!(
            home.path()
                .join("drafts/github.com/acme/service/pr-141.json")
                .exists(),
            "the file is left for a build that can read it"
        );
    }

    #[test]
    fn discarding_removes_the_file_as_well_as_the_draft() {
        let (store, home) = store();
        let (mut drafter, _) = Drafter::open(store, repo(), 141, now());
        drafter
            .stage("src/a.rs", Side::New, 31, None, "why?", now())
            .expect("staged");
        drafter.discard(now()).expect("discarded");
        assert!(drafter.draft().is_empty());
        assert!(
            !home
                .path()
                .join("drafts/github.com/acme/service/pr-141.json")
                .exists()
        );
    }

    #[test]
    fn the_head_is_remembered_but_does_not_write_a_file_by_itself() {
        let (store, home) = store();
        let (mut drafter, _) = Drafter::open(store, repo(), 141, now());
        drafter.anchor_to("abc123");
        assert_eq!(drafter.draft().head_sha.as_deref(), Some("abc123"));
        assert!(
            !home
                .path()
                .join("drafts/github.com/acme/service/pr-141.json")
                .exists(),
            "opening a pull request must not fill the drafts directory"
        );
        // The head reaches disk with the next real edit.
        drafter
            .stage("src/a.rs", Side::New, 1, None, "why?", now())
            .expect("staged");
        let written = crate::domain::draft::Draft::from_json(
            &std::fs::read_to_string(
                home.path()
                    .join("drafts/github.com/acme/service/pr-141.json"),
            )
            .expect("readable"),
        )
        .expect("a draft");
        assert_eq!(written.head_sha.as_deref(), Some("abc123"));
    }
}
