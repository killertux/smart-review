//! Where chat sessions live (FR-5.1, FR-8.5, DEC-9).
//!
//! A session is one JSON document per conversation, replaced atomically on every
//! append, rather than a log appended to in place. Two reasons, and both are
//! requirements:
//!
//! - **NFR-4.1: a crash must not lose the conversation.** An append to the end of a
//!   file is atomic enough for a log *if* the process is the only writer and the
//!   record is one line; a document rewritten through a temporary file and a rename is
//!   atomic whatever happens, and a session is small enough to rewrite.
//! - **`cache/` is disposable, but the conversation is the user's own words**
//!   (FR-8.5). It lives under `cache/` because that is where per-pull-request data
//!   goes, and it is written carefully because losing it is still a failure.
//!
//! The port is per `(repo, pull request)`: that scope is what `:chat list` lists, what
//! DEC-9's cap applies to, and what the UI can name.

use std::fmt;

use crate::domain::chat::{Pruned, Session, SessionMeta};
use crate::domain::repo::RepoId;

/// What can go wrong with the chat store.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatStoreError {
    /// The directory could not be read or written.
    #[error("could not {action} {path}: {cause}")]
    Io {
        /// What was being attempted.
        action: String,
        /// The path.
        path: String,
        /// The cause.
        cause: String,
    },

    /// The session could not be parsed.
    #[error("{path} is not a readable chat session: {reason}")]
    Malformed {
        /// The path.
        path: String,
        /// What was wrong.
        reason: String,
    },

    /// The session cannot grow any further (DEC-9).
    #[error(
        "this conversation has reached its {limit} limit; `:chat new` starts another, \
         or `:chat export` keeps a copy"
    )]
    Full {
        /// The limit, as a readable size.
        limit: String,
    },
}

/// The sessions belonging to one pull request.
pub trait ChatStorePort: fmt::Debug + Send + Sync {
    /// The sessions for a pull request, newest first (FR-5.1).
    ///
    /// # Errors
    ///
    /// Returns [`ChatStoreError::Io`] when the directory cannot be listed. A session
    /// that cannot be read is skipped rather than raised: one unreadable file must not
    /// hide the conversations that are fine.
    fn list(&self, repo: &RepoId, pr: u64) -> Result<Vec<SessionMeta>, ChatStoreError>;

    /// One session, if it is there.
    ///
    /// # Errors
    ///
    /// Returns [`ChatStoreError::Malformed`] when the file exists and cannot be read
    /// as a session, because silently starting an empty conversation over a file the
    /// user can still see would be the wrong kind of quiet.
    fn load(&self, repo: &RepoId, pr: u64, id: &str) -> Result<Option<Session>, ChatStoreError>;

    /// The newest session for a pull request, if there is one.
    ///
    /// # Errors
    ///
    /// As [`ChatStorePort::list`] and [`ChatStorePort::load`].
    fn latest(&self, repo: &RepoId, pr: u64) -> Result<Option<Session>, ChatStoreError>;

    /// Stores a session, creating or replacing it (FR-5.1).
    ///
    /// # Errors
    ///
    /// Returns [`ChatStoreError::Full`] when the session would pass DEC-9's per-session
    /// cap, and [`ChatStoreError::Io`] when it cannot be written.
    fn put(&self, session: &Session) -> Result<(), ChatStoreError>;

    /// Removes a session.
    ///
    /// # Errors
    ///
    /// Returns [`ChatStoreError::Io`] when it cannot be removed.
    fn remove(&self, repo: &RepoId, pr: u64, id: &str) -> Result<(), ChatStoreError>;

    /// Enforces DEC-9's cap, removing the oldest sessions first.
    ///
    /// # Errors
    ///
    /// Returns [`ChatStoreError::Io`] when the directory cannot be listed or a removal
    /// fails. Sessions that were removed successfully are reported even when a later
    /// removal fails, because the caller has to announce what went.
    fn prune(&self, repo: &RepoId, pr: u64) -> Result<Pruned, ChatStoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_session_says_what_to_do_about_it() {
        let error = ChatStoreError::Full {
            limit: "2.0 MB".to_owned(),
        };
        let message = error.to_string();
        assert!(message.contains("2.0 MB"), "{message}");
        assert!(message.contains(":chat new"), "{message}");
        assert!(message.contains(":chat export"), "{message}");
    }

    #[test]
    fn an_unreadable_session_is_reported_rather_than_ignored() {
        let error = ChatStoreError::Malformed {
            path: "/tmp/x.json".to_owned(),
            reason: "expected value".to_owned(),
        };
        assert!(error.to_string().contains("expected value"));
    }
}
