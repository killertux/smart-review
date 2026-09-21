//! The conversation a pull request already has, and what may be added to it
//! (FR-6.4, DEC-16).
//!
//! Three different things live in this file because they are three different GitHub
//! APIs, and the shape of each is a fact worth keeping in one place:
//!
//! * **Reading threads** — GitHub reports review comments *and* their replies as one
//!   flat list on the REST side, with an `in_reply_to_id` linking them, and reports
//!   whether the thread is resolved only through GraphQL's `reviewThreads`. So the
//!   bodies come from REST and the state comes from GraphQL, joined by comment id
//!   (`databaseId` on the GraphQL side, `id` on the REST side).
//! * **Replying** — `POST …/pulls/{N}/comments/{id}/replies`, a REST route. A reply is
//!   its own call, not part of a review: GitHub's review endpoint accepts only *new*
//!   inline comments, so answering something already said cannot be batched with a
//!   verdict. That is why the interface sends it on its own rather than adding it to
//!   the draft.
//! * **Resolving** — `resolveReviewThread` / `unresolveReviewThread`, GraphQL
//!   mutations with no REST equivalent at all (verified on gh 2.45: neither route
//!   exists under `repos/…/pulls/…`). The input takes the thread's node id.
//!
//! Everything here is one call per action and every failure is reported rather than
//! swallowed: the only thing that changes is a thread's state, and a silently failed
//! resolve would look exactly like a successful one.

use crate::adapters::gh::GhCliForge;
use crate::adapters::process::CommandSpec;
use crate::domain::pr::ConversationComment;
use crate::ports::{Cancel, CommentPosted};

/// The graph, in one query: every thread, its state and the ids of its comments.
///
/// `first: 100` because the page size is the API's maximum and a pull request with more
/// than a hundred review threads is one where the *reading* is the problem. The result
/// is used to annotate comments that were already fetched, so a truncated answer loses
/// thread state rather than losing comments.
const THREADS_QUERY: &str = "\
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100) {
        nodes {
          id
          isResolved
          isOutdated
          comments(first: 100) { nodes { databaseId } }
        }
      }
    }
  }
}";

/// The mutation for either direction, built from the flag rather than duplicated.
fn resolve_query(resolved: bool) -> String {
    let field = if resolved {
        "resolveReviewThread"
    } else {
        "unresolveReviewThread"
    };
    format!(
        "mutation($id: ID!) {{ {field}(input: {{ threadId: $id }}) {{ thread {{ id isResolved }} }} }}"
    )
}

impl GhCliForge {
    /// Reads which threads exist, and which of them are resolved (FR-6.4).
    ///
    /// Named `read_*` rather than `list_*` so that it cannot be confused with the port
    /// method of the same shape: this is an inherent method the adapter uses to build a
    /// detail, not something the application may call.
    ///
    /// # Errors
    ///
    /// Returns an error when `gh` cannot be run or the answer cannot be understood. A
    /// caller that only wants the comments tolerates this: it is state *about* them.
    pub(super) fn read_review_threads(
        &self,
        number: u64,
        cancel: &Cancel,
    ) -> crate::Result<Vec<ThreadState>> {
        let spec = self
            .graphql_spec(THREADS_QUERY)
            .arg("-f")
            .arg(format!("owner={}", self.repo.owner()))
            .arg("-f")
            .arg(format!("name={}", self.repo.name()))
            .arg("-F")
            .arg(format!("number={number}"));
        let answer: GraphQlAnswer<ThreadsData> = self.json(&spec, cancel)?;
        let Some(data) = answer.data else {
            // A GraphQL error with no data is what a token without access looks like.
            // Reporting it as an empty list would draw every thread as unresolved,
            // which is a claim, not an absence.
            let reason = answer
                .errors
                .first()
                .map_or_else(|| "no data".to_owned(), |error| error.message.clone());
            return Err(crate::Error::forge(spec.diagnostic(), reason));
        };
        Ok(data
            .repository
            .pull_request
            .review_threads
            .nodes
            .into_iter()
            .map(|node| ThreadState {
                id: node.id,
                resolved: node.is_resolved,
                outdated: node.is_outdated,
                comment_ids: node
                    .comments
                    .nodes
                    .into_iter()
                    .filter_map(|comment| comment.database_id)
                    .collect(),
            })
            .collect())
    }

    /// Reads the comments on the pull request's conversation (FR-6.4).
    ///
    /// # Errors
    ///
    /// As [`crate::ports::ForgePort::list_conversation`].
    pub(super) fn read_conversation(
        &self,
        number: u64,
        cancel: &Cancel,
    ) -> crate::Result<Vec<ConversationComment>> {
        let spec = self.api_spec(&format!("issues/{number}/comments")).args([
            "--paginate",
            "-X",
            "GET",
            "-f",
            "per_page=100",
        ]);
        let comments: Vec<crate::adapters::gh::json::GhIssueComment> =
            self.json_stream(&spec, cancel)?;
        Ok(comments
            .into_iter()
            .map(crate::adapters::gh::json::GhIssueComment::into_domain)
            .collect())
    }

    /// Replies into an existing review thread (FR-6.4).
    ///
    /// # Errors
    ///
    /// Returns an error when the comment is gone, when GitHub refuses, or when `gh`
    /// cannot be run.
    pub(super) fn post_reply(
        &self,
        number: u64,
        comment_id: u64,
        body: &str,
        cancel: &Cancel,
    ) -> crate::Result<CommentPosted> {
        self.post_body(
            &format!("pulls/{number}/comments/{comment_id}/replies"),
            "reply",
            number,
            body,
            cancel,
        )
    }

    /// Comments on the pull request's conversation (FR-6.4, DEC-16).
    ///
    /// `issues/{N}/comments` rather than a pull-request route: a pull request *is* an
    /// issue on GitHub, and the conversation is the issue's comment list.
    ///
    /// # Errors
    ///
    /// As [`Self::reply_to_review_comment`].
    pub(super) fn post_conversation_comment(
        &self,
        number: u64,
        body: &str,
        cancel: &Cancel,
    ) -> crate::Result<CommentPosted> {
        self.post_body(
            &format!("issues/{number}/comments"),
            "conversation-comment",
            number,
            body,
            cancel,
        )
    }

    /// Resolves or unresolves a thread (FR-6.4).
    ///
    /// # Errors
    ///
    /// Returns an error when GitHub refuses or when `gh` cannot be run.
    pub(super) fn set_thread_resolution(
        &self,
        thread_id: &str,
        resolved: bool,
        cancel: &Cancel,
    ) -> crate::Result<()> {
        let spec = self
            .graphql_spec(&resolve_query(resolved))
            .arg("-f")
            .arg(format!("id={thread_id}"))
            .mutating();
        let Some(output) = self.mutate(&spec, cancel)? else {
            return Ok(());
        };
        if !output.success() {
            return Err(crate::Error::forge(
                spec.diagnostic(),
                crate::adapters::gh::review::translate_refusal(&output.stderr, 0),
            ));
        }
        // `gh api graphql` exits zero on a query that failed at the GraphQL layer, with
        // the error in the body: for a mutation that is the difference between "the
        // thread is resolved" and "nothing happened".
        let answer: GraphQlAnswer<serde_json::Value> =
            serde_json::from_str(&output.stdout).unwrap_or_default();
        if let Some(error) = answer.errors.first() {
            return Err(crate::Error::forge(
                spec.diagnostic(),
                error.message.clone(),
            ));
        }
        Ok(())
    }

    /// A `gh api graphql` command with the query as one argument.
    ///
    /// The query is one argv element, never assembled from a shell string: it contains
    /// newlines and braces, following the same argv-only query rule (FR-1.3).
    fn graphql_spec(&self, query: &str) -> CommandSpec {
        CommandSpec::new(&self.program)
            .args(["api", "graphql", "-f"])
            .arg(format!("query={query}"))
    }

    /// Runs a posting command and reads what came back.
    ///
    /// Shared by both routes because they differ only in the endpoint: a reply and a
    /// conversation comment return the same object and are refused the same way.
    fn posted(
        &self,
        spec: &CommandSpec,
        number: u64,
        cancel: &Cancel,
    ) -> crate::Result<CommentPosted> {
        let Some(output) = self.mutate(spec, cancel)? else {
            return Ok(CommentPosted {
                dry_run: true,
                ..CommentPosted::default()
            });
        };
        if !output.success() {
            return Err(crate::Error::forge(
                spec.diagnostic(),
                crate::adapters::gh::review::translate_refusal(&output.stderr, number),
            ));
        }
        let response: serde_json::Value =
            serde_json::from_str(&output.stdout).unwrap_or(serde_json::Value::Null);
        Ok(CommentPosted {
            id: response.get("id").and_then(serde_json::Value::as_u64),
            url: response
                .get("html_url")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            dry_run: false,
        })
    }

    /// Posts user prose from a private JSON file, never from argv. The exact file is
    /// retained only for a dry run, where the recorded command is explicitly meant to
    /// be replayable; ordinary success and failure remove it.
    fn post_body(
        &self,
        endpoint: &str,
        kind: &str,
        number: u64,
        body: &str,
        cancel: &Cancel,
    ) -> crate::Result<CommentPosted> {
        let text = serde_json::to_string_pretty(&serde_json::json!({ "body": body }))
            .map_err(|error| crate::Error::forge(kind, error.to_string()))?;
        let path = self.payload_path(kind, number, "json");
        crate::adapters::fs::write_atomic_with_mode(&path, &text, Some(0o600))
            .map_err(|error| crate::Error::io("write the comment payload", path.clone(), error))?;
        let spec = self
            .api_spec(endpoint)
            .args(["-X", "POST"])
            .arg("--input")
            .arg(&path)
            .mutating();
        let result = self.posted(&spec, number, cancel);
        if result.as_ref().is_ok_and(|posted| !posted.dry_run) || result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        result
    }
}

/// One thread as GitHub reports it (FR-6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThreadState {
    /// The GraphQL node id, which is what resolving takes.
    pub id: String,
    /// Whether it has been resolved.
    pub resolved: bool,
    /// Whether the diff has moved under it.
    pub outdated: bool,
    /// The REST ids of its comments, the root first.
    pub comment_ids: Vec<u64>,
}

/// Applies thread state to the comments it belongs to (FR-6.4).
///
/// A comment that no thread mentions keeps `thread_id: None` and `resolved: false`. The
/// two cases are worth telling apart at the call site — "no thread state was read" and
/// "this thread is open" — which is why the caller skips this entirely when the read
/// failed rather than calling it with an empty list.
pub(super) fn apply_threads(
    comments: &mut [crate::domain::pr::ReviewComment],
    threads: &[ThreadState],
) {
    use std::collections::HashMap;
    let mut state: HashMap<u64, (&str, bool, bool)> = HashMap::new();
    for thread in threads {
        for id in &thread.comment_ids {
            state.insert(*id, (thread.id.as_str(), thread.resolved, thread.outdated));
        }
    }
    for comment in comments {
        if let Some((id, resolved, outdated)) = state.get(&comment.id) {
            comment.thread_id = Some((*id).to_owned());
            comment.resolved = *resolved;
            comment.outdated = *outdated;
        } else {
            comment.thread_id = None;
            comment.resolved = false;
            comment.outdated = false;
        }
    }
}

/// The envelope every `gh api graphql` answer has.
#[derive(Debug, serde::Deserialize)]
struct GraphQlAnswer<T> {
    // No `#[serde(default)]` here on purpose: serde's derive adds a `T: Default` bound
    // for a generic field that has it, and `Option` is already optional in serde.
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

impl<T> Default for GraphQlAnswer<T> {
    fn default() -> Self {
        Self {
            data: None,
            errors: Vec::new(),
        }
    }
}

/// One GraphQL error.
#[derive(Debug, serde::Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsData {
    repository: ThreadsRepository,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsRepository {
    pull_request: ThreadsPullRequest,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsPullRequest {
    review_threads: ThreadsConnection,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsConnection {
    #[serde(default)]
    nodes: Vec<ThreadNode>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadNode {
    id: String,
    #[serde(default)]
    is_resolved: bool,
    #[serde(default)]
    is_outdated: bool,
    #[serde(default)]
    comments: ThreadComments,
}

#[derive(Debug, serde::Deserialize, Default)]
struct ThreadComments {
    #[serde(default)]
    nodes: Vec<ThreadCommentNode>,
}

/// `databaseId` is the REST id. GitHub's GraphQL ids (`PRRC_…`) and its REST ids are
/// different namespaces, and this field is the only bridge between them.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadCommentNode {
    #[serde(default)]
    database_id: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pr::ReviewComment;
    use crate::domain::time::from_unix_secs;

    fn comment(id: u64, reply_to: Option<u64>) -> ReviewComment {
        ReviewComment {
            id,
            author: "alice".to_owned(),
            path: "src/a.rs".to_owned(),
            line: Some(31),
            side: Some("RIGHT".to_owned()),
            body: "why?".to_owned(),
            created_at: from_unix_secs(1_700_000_000),
            in_reply_to: reply_to,
            diff_hunk: None,
            url: None,
            thread_id: None,
            resolved: false,
            outdated: false,
        }
    }

    fn thread(id: &str, resolved: bool, ids: &[u64]) -> ThreadState {
        ThreadState {
            id: id.to_owned(),
            resolved,
            outdated: false,
            comment_ids: ids.to_vec(),
        }
    }

    #[test]
    fn threads_carry_their_state_on_every_comment() {
        // The root and its reply are one thread, so the reply is resolved too: the UI
        // draws a thread as a block, and a block with two different states in it would
        // be a lie about one of them.
        let mut comments = vec![comment(1, None), comment(2, Some(1))];
        apply_threads(&mut comments, &[thread("PRRT_1", true, &[1, 2])]);
        assert_eq!(comments[0].thread_id.as_deref(), Some("PRRT_1"));
        assert_eq!(comments[1].thread_id.as_deref(), Some("PRRT_1"));
        assert!(comments[0].resolved && comments[1].resolved);
    }

    #[test]
    fn a_comment_no_thread_mentions_is_left_open() {
        let mut comments = vec![comment(1, None)];
        apply_threads(&mut comments, &[]);
        assert!(comments[0].thread_id.is_none());
        assert!(!comments[0].resolved);
    }

    #[test]
    fn applying_thread_state_twice_does_not_leave_the_first_answer_behind() {
        // A thread resolved, then resolved again after someone reopened it: the second
        // read must be able to say "open" about a comment that said "resolved".
        let mut comments = vec![comment(1, None)];
        apply_threads(&mut comments, &[thread("PRRT_1", true, &[1])]);
        assert!(comments[0].resolved);
        apply_threads(&mut comments, &[]);
        assert!(!comments[0].resolved, "the newer answer wins");
        assert!(comments[0].thread_id.is_none());
    }

    #[test]
    fn the_resolve_mutation_names_the_field_that_matches_the_direction() {
        assert!(resolve_query(true).contains("resolveReviewThread("));
        assert!(!resolve_query(true).contains("unresolveReviewThread"));
        assert!(resolve_query(false).contains("unresolveReviewThread("));
        assert!(
            resolve_query(true).contains("threadId: $id"),
            "the thread id is a variable, never pasted in"
        );
    }

    #[test]
    fn the_threads_query_asks_for_both_halves_of_the_join() {
        assert!(THREADS_QUERY.contains("reviewThreads"));
        assert!(THREADS_QUERY.contains("isResolved"));
        assert!(
            THREADS_QUERY.contains("databaseId"),
            "the REST id is what lets the comments be joined to their thread"
        );
    }

    #[test]
    fn a_graphql_answer_with_errors_and_no_data_is_an_error_not_an_empty_list() {
        // The distinction that matters: "this pull request has no threads" and "the
        // threads could not be read" draw differently — the first draws nothing, the
        // second must not claim every thread is open.
        let answer: GraphQlAnswer<ThreadsData> = serde_json::from_str(
            r#"{"errors":[{"message":"Could not resolve to a PullRequest"}]}"#,
        )
        .expect("parses");
        assert!(answer.data.is_none());
        assert_eq!(
            answer.errors.first().map(|error| error.message.as_str()),
            Some("Could not resolve to a PullRequest")
        );
    }

    #[test]
    fn a_thread_with_no_database_ids_keeps_no_comment_it_could_not_match() {
        // A comment whose id GitHub did not report is not silently attached to the
        // thread by position: it simply does not get a thread id, and the interface
        // says it cannot be resolved.
        let mut comments = vec![comment(1, None)];
        apply_threads(
            &mut comments,
            &[ThreadState {
                id: "PRRT_1".to_owned(),
                resolved: true,
                outdated: false,
                comment_ids: vec![],
            }],
        );
        assert!(comments[0].thread_id.is_none());
        assert!(!comments[0].resolved);
    }
}
