//! Saying something that is not part of a review (FR-6.4, DEC-16).
//!
//! Two of the three things here post words — a reply into a thread that already exists,
//! and a comment on the pull request's own conversation — and the third changes a
//! thread's state. None of them is a review, and none of them can be batched with one:
//! GitHub's review endpoint accepts only new inline comments, so answering something
//! already said is its own request, made when the user makes it.
//!
//! This exists as a service rather than as three calls in the job runner because of the
//! same rule that made [`crate::application::drafts`] a service: **the boundary that can
//! put words on the internet checks its own input**. The composer has already validated
//! the body, and this checks it again, because the composer is not the only caller and
//! because "an empty comment" reaching the API is a 422 with a worse sentence than
//! [`crate::domain::draft::DraftError::EmptyBody`].

use crate::domain::draft::Post;
use crate::domain::repo::RepoId;
use crate::ports::forge::ForgePort;
use crate::ports::{Cancel, CommentPosted};

/// Posting a message, or changing a thread's state (FR-6.4).
///
/// Stateless: there is nothing to keep between posts, and a service that held "the post
/// in flight" would be a second copy of the interface's own state.
#[derive(Debug)]
pub struct Posts<'a> {
    forge: &'a dyn ForgePort,
    repo: RepoId,
}

impl<'a> Posts<'a> {
    /// Binds the service to a forge and a repository.
    #[must_use]
    pub fn new(forge: &'a dyn ForgePort, repo: RepoId) -> Self {
        Self { forge, repo }
    }

    /// Replies into a review thread (FR-6.4).
    ///
    /// # Errors
    ///
    /// Returns [`ReplyError::Refused`] when the body cannot be sent, and
    /// [`ReplyError::Forge`] when GitHub or the network refused.
    pub fn reply(
        &self,
        number: u64,
        comment_id: u64,
        body: &str,
        cancel: &Cancel,
    ) -> Result<CommentPosted, ReplyError> {
        let post = Post::new(body).map_err(ReplyError::Refused)?;
        self.forge
            .reply_to_review_comment(number, comment_id, &post.body, cancel)
            .map_err(ReplyError::Forge)
    }

    /// Comments on the pull request's conversation (FR-6.4, DEC-16).
    ///
    /// # Errors
    ///
    /// As [`Self::reply`].
    pub fn comment(
        &self,
        number: u64,
        body: &str,
        cancel: &Cancel,
    ) -> Result<CommentPosted, ReplyError> {
        let post = Post::new(body).map_err(ReplyError::Refused)?;
        self.forge
            .comment_on_conversation(number, &post.body, cancel)
            .map_err(ReplyError::Forge)
    }

    /// Resolves or unresolves a thread (FR-6.4).
    ///
    /// No validation to do: the only input is an id that came from GitHub, and the
    /// direction. The check that matters — that this thread id is one GitHub knows — is
    /// one only GitHub can make.
    ///
    /// # Errors
    ///
    /// Returns [`ReplyError::Forge`] when GitHub refused. The caller is expected to
    /// report it rather than to retry: a thread that failed to resolve looks exactly
    /// like one that was resolved, so silence here is a lie about state.
    pub fn resolve(
        &self,
        thread_id: &str,
        resolved: bool,
        cancel: &Cancel,
    ) -> Result<(), ReplyError> {
        self.forge
            .set_thread_resolved(thread_id, resolved, cancel)
            .map_err(ReplyError::Forge)
    }

    /// The repository this service posts to, for error messages.
    #[must_use]
    pub fn repo(&self) -> &RepoId {
        &self.repo
    }
}

/// Why a post did not happen (FR-6.4).
#[derive(Debug, thiserror::Error)]
pub enum ReplyError {
    /// The words cannot be sent as they are.
    #[error("{0}")]
    Refused(#[from] crate::domain::draft::DraftError),
    /// The forge refused, in its own words (translated by the adapter).
    #[error("{0}")]
    Forge(#[source] crate::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::ReviewPosted;
    use crate::ports::forge::{CommentPosted, ForgeCapabilities, ForgePort, PullRequestPage};
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct RecordingForge {
        replies: Mutex<Vec<(u64, u64, String)>>,
        comments: Mutex<Vec<(u64, String)>>,
        resolved: Mutex<Vec<(String, bool)>>,
        refuse: Mutex<Option<String>>,
    }

    impl ForgePort for RecordingForge {
        fn capabilities(&self) -> ForgeCapabilities {
            ForgeCapabilities::default()
        }

        fn list_pull_requests(
            &self,
            _query: &crate::domain::query::PrQuery,
            _cancel: &Cancel,
        ) -> crate::Result<PullRequestPage> {
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
            _draft: &crate::domain::draft::Draft,
            _cancel: &Cancel,
        ) -> crate::Result<ReviewPosted> {
            Err(crate::Error::forge("gh", "not used"))
        }

        fn reply_to_review_comment(
            &self,
            number: u64,
            comment_id: u64,
            body: &str,
            _cancel: &Cancel,
        ) -> crate::Result<CommentPosted> {
            if let Some(reason) = self.refuse.lock().unwrap().clone() {
                return Err(crate::Error::forge("gh", reason));
            }
            self.replies
                .lock()
                .unwrap()
                .push((number, comment_id, body.to_owned()));
            Ok(CommentPosted {
                id: Some(7),
                url: Some("https://example.test/c/7".to_owned()),
                dry_run: false,
            })
        }

        fn comment_on_conversation(
            &self,
            number: u64,
            body: &str,
            _cancel: &Cancel,
        ) -> crate::Result<CommentPosted> {
            self.comments
                .lock()
                .unwrap()
                .push((number, body.to_owned()));
            Ok(CommentPosted::default())
        }

        fn set_thread_resolved(
            &self,
            thread_id: &str,
            resolved: bool,
            _cancel: &Cancel,
        ) -> crate::Result<()> {
            if let Some(reason) = self.refuse.lock().unwrap().clone() {
                return Err(crate::Error::forge("gh", reason));
            }
            self.resolved
                .lock()
                .unwrap()
                .push((thread_id.to_owned(), resolved));
            Ok(())
        }

        fn list_conversation(
            &self,
            _number: u64,
            _cancel: &Cancel,
        ) -> crate::Result<Vec<crate::domain::pr::ConversationComment>> {
            Ok(Vec::new())
        }
    }

    fn repo() -> RepoId {
        RepoId::parse("acme/service").expect("a repository")
    }

    #[test]
    fn a_reply_reaches_the_forge_with_the_comment_it_answers() {
        let forge = RecordingForge::default();
        let posts = Posts::new(&forge, repo());
        let posted = posts
            .reply(141, 1001, "agreed, fixed", &Cancel::new())
            .expect("posted");

        assert_eq!(posted.id, Some(7));
        assert_eq!(
            forge.replies.lock().unwrap().as_slice(),
            [(141, 1001, "agreed, fixed".to_owned())]
        );
    }

    #[test]
    fn an_empty_reply_never_reaches_the_forge() {
        // The check that makes this a service rather than a delegate: a 422 from GitHub
        // is a worse sentence than the one the composer already knows how to say.
        let forge = RecordingForge::default();
        let posts = Posts::new(&forge, repo());
        let error = posts
            .reply(141, 1001, "   \n ", &Cancel::new())
            .expect_err("refused");
        assert_eq!(
            error.to_string(),
            crate::domain::draft::DraftError::EmptyBody.to_string()
        );
        assert!(forge.replies.lock().unwrap().is_empty());
    }

    #[test]
    fn a_conversation_comment_is_not_a_reply_to_anything() {
        let forge = RecordingForge::default();
        let posts = Posts::new(&forge, repo());
        posts
            .comment(141, "thanks, looking now", &Cancel::new())
            .expect("posted");
        assert_eq!(
            forge.comments.lock().unwrap().as_slice(),
            [(141, "thanks, looking now".to_owned())]
        );
        assert!(forge.replies.lock().unwrap().is_empty());
    }

    #[test]
    fn resolving_carries_the_direction_and_the_thread() {
        let forge = RecordingForge::default();
        let posts = Posts::new(&forge, repo());
        posts
            .resolve("PRRT_1", true, &Cancel::new())
            .expect("resolved");
        posts
            .resolve("PRRT_1", false, &Cancel::new())
            .expect("reopened");
        assert_eq!(
            forge.resolved.lock().unwrap().as_slice(),
            [("PRRT_1".to_owned(), true), ("PRRT_1".to_owned(), false)]
        );
    }

    #[test]
    fn a_refusal_from_github_is_reported_rather_than_swallowed() {
        // A resolve that failed silently would look exactly like one that worked, and
        // the interface would draw a thread as resolved that is not.
        let forge = RecordingForge::default();
        *forge.refuse.lock().unwrap() = Some("Resource not accessible".to_owned());
        let posts = Posts::new(&forge, repo());
        let error = posts
            .resolve("PRRT_1", true, &Cancel::new())
            .expect_err("refused");
        assert!(error.to_string().contains("Resource not accessible"));
        assert!(forge.resolved.lock().unwrap().is_empty());
    }
}
