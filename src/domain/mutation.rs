//! Durable remote-mutation records (IR-07, FR-6.3–6.5).
//!
//! A local process cannot infer GitHub's result from a lost response. This document
//! therefore records the exact confirmed payload and distinguishes a refusal from an
//! outcome that must be reconciled before the user deliberately submits again.

use serde::{Deserialize, Serialize};

use crate::domain::draft::Draft;
use crate::domain::repo::RepoId;
use crate::domain::time::Timestamp;

/// The current durable mutation-document format.
pub const MUTATION_VERSION: u32 = 1;

/// A confirmed request that may change remote GitHub state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MutationKind {
    /// Submit one complete review snapshot.
    Review { draft: Draft },
    /// Reply to a root inline review comment.
    Reply { comment_id: u64, body: String },
    /// Post to the pull request conversation.
    Conversation { body: String },
    /// Resolve or reopen one review thread.
    ThreadResolution { thread_id: String, resolved: bool },
}

/// What is known about a mutation after it was confirmed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MutationState {
    /// The snapshot is durable but no request has started.
    Queued,
    /// The request may have reached GitHub; cancellation is no longer proof it did not.
    Dispatching,
    /// GitHub confirmed the mutation.
    Succeeded {
        /// GitHub's object id, when that route reports one.
        id: Option<u64>,
        /// GitHub's URL, when that route reports one.
        url: Option<String>,
    },
    /// GitHub definitely rejected the request before accepting it.
    Rejected {
        /// The actionable refusal returned by the forge.
        reason: String,
    },
    /// The process lost the result after dispatch and must reconcile before retrying.
    OutcomeUnknown {
        /// The local observation that made the result uncertain.
        reason: String,
    },
    /// A dry run recorded commands but did not mutate GitHub.
    Simulated,
}

impl MutationState {
    /// Whether this operation prevents an automatic repeat dispatch.
    #[must_use]
    pub const fn blocks_dispatch(&self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Dispatching | Self::OutcomeUnknown { .. }
        )
    }

    /// Whether this record needs recovery on the next application start.
    #[must_use]
    pub const fn needs_reconciliation(&self) -> bool {
        matches!(self, Self::Dispatching | Self::OutcomeUnknown { .. })
    }
}

/// An immutable confirmed payload with its durable remote-mutation lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationOperation {
    /// The document format version.
    #[serde(default = "default_version")]
    pub version: u32,
    /// A locally unique record id. It is not presented as a GitHub idempotency key.
    pub id: String,
    /// The repository the operation belongs to.
    pub repo: RepoId,
    /// The pull request it targets.
    pub pr: u64,
    /// The head revision visible when the user confirmed, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    /// The confirmed immutable payload.
    pub kind: MutationKind,
    /// The operation's current local/remote truth.
    pub state: MutationState,
    /// When confirmation created this record.
    pub created_at: Timestamp,
    /// When its state last changed.
    pub updated_at: Timestamp,
}

const fn default_version() -> u32 {
    MUTATION_VERSION
}

impl MutationOperation {
    /// Builds a queued operation from a confirmed snapshot.
    #[must_use]
    pub fn queued(
        id: String,
        repo: RepoId,
        pr: u64,
        head_sha: Option<String>,
        kind: MutationKind,
        now: Timestamp,
    ) -> Self {
        Self {
            version: MUTATION_VERSION,
            id,
            repo,
            pr,
            head_sha,
            kind,
            state: MutationState::Queued,
            created_at: now,
            updated_at: now,
        }
    }

    /// Records that dispatch has begun.
    pub fn mark_dispatching(&mut self, now: Timestamp) {
        self.state = MutationState::Dispatching;
        self.updated_at = now;
    }

    /// Records a confirmed remote success.
    pub fn mark_succeeded(&mut self, id: Option<u64>, url: Option<String>, now: Timestamp) {
        self.state = MutationState::Succeeded { id, url };
        self.updated_at = now;
    }

    /// Records a known rejection, keeping the snapshot available for an explicit retry.
    pub fn mark_rejected(&mut self, reason: String, now: Timestamp) {
        self.state = MutationState::Rejected { reason };
        self.updated_at = now;
    }

    /// Records an uncertain result that must not be retried automatically.
    pub fn mark_outcome_unknown(&mut self, reason: String, now: Timestamp) {
        self.state = MutationState::OutcomeUnknown { reason };
        self.updated_at = now;
    }

    /// Records that only a dry-run artifact was written.
    pub fn mark_simulated(&mut self, now: Timestamp) {
        self.state = MutationState::Simulated;
        self.updated_at = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::time::from_unix_secs;

    #[test]
    fn ir_07_a_lost_response_blocks_an_automatic_repeat_dispatch() {
        let now = from_unix_secs(1_700_000_000);
        let mut operation = MutationOperation::queued(
            "op-1".to_owned(),
            RepoId::parse("acme/service").expect("repository"),
            141,
            Some("head".to_owned()),
            MutationKind::Conversation {
                body: "please add a test".to_owned(),
            },
            now,
        );
        operation.mark_dispatching(now);
        operation.mark_outcome_unknown("connection closed after dispatch".to_owned(), now);

        assert!(operation.state.blocks_dispatch());
        assert!(operation.state.needs_reconciliation());
    }
}
