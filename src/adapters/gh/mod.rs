//! The GitHub adapter, built on the `gh` CLI (FR-1.1, FR-2.1, FR-2.4, FR-3.2).
//!
//! The rules it obeys, from ARCH-3 and FR-6.5:
//!
//! - every call passes `--repo <owner>/<name>` so the current directory can never
//!   change which repository is being read (`gh api` takes the repository in its
//!   endpoint path instead, which is equally unambiguous and is asserted by a
//!   test);
//! - arguments are always an argv array, never a shell string, so a PR title
//!   containing `; rm -rf` is just a title;
//! - stdout and stderr are captured separately with a size cap and a timeout, and
//!   a failure carries the exit code and the tail of stderr;
//! - the response is parsed tolerantly: a field this build does not know about,
//!   or a value it has never seen, degrades rather than failing the call.
//!
//! `gh --paginate` concatenates pages as a stream of JSON documents rather than
//! merging them (and `--slurp` is not available on the minimum supported version),
//! so responses are read as a *stream* of values and flattened. That is what makes
//! a PR with more than 100 inline comments work on gh 2.40 and on 2.45 alike.

mod comments;
pub mod json;
pub mod probe;
mod review;

use std::path::PathBuf;

use serde::de::DeserializeOwned;

use crate::adapters::process::{CommandSpec, Output, ProcessRunner};
use crate::domain::pr::{CheckRun, ConversationComment, PullRequestDetail, Review, ReviewComment};
use crate::domain::query::PrQuery;
use crate::domain::repo::RepoId;
use crate::error::{Error, Result};
use crate::logging::{self, Level};
use crate::ports::forge::{ForgeCapabilities, ForgeFactory, ForgePort, PullRequestPage};
use crate::ports::{Cancel, CommentPosted, ReviewPosted};

use json::{
    DETAIL_FIELDS, GhDetail, GhReview, GhReviewComment, GhSummary, GraphQlResponse, LIST_FIELDS,
};

/// How long a `gh` call may take before it is killed.
///
/// Generous on purpose: `gh pr diff` on a large PR over a slow link is legitimately
/// slow, and a timeout that fires on a working call is worse than a slow one.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The `gh` CLI as a [`ForgePort`].
#[derive(Clone)]
pub struct GhCliForge {
    runner: ProcessRunner,
    program: PathBuf,
    repo: RepoId,
}

/// Builds [`GhCliForge`] instances for whatever repository detection resolves.
#[derive(Debug, Clone)]
pub struct GhForgeFactory {
    program: PathBuf,
    runner: ProcessRunner,
}

impl GhForgeFactory {
    /// Uses the configured `gh` program.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            runner: ProcessRunner::new(),
        }
    }

    /// Uses a prepared runner, which is how `--dry-run` reaches every forge the
    /// application builds (FR-6.5).
    #[must_use]
    pub fn with_runner(mut self, runner: ProcessRunner) -> Self {
        self.runner = runner;
        self
    }
}

impl ForgeFactory for GhForgeFactory {
    fn forge(&self, repo: &RepoId) -> std::sync::Arc<dyn ForgePort> {
        std::sync::Arc::new(
            GhCliForge::new(&self.program, repo.clone()).with_runner(self.runner.clone()),
        )
    }
}

impl std::fmt::Debug for GhCliForge {
    /// Renders the repository the way a person writes it, so log lines and error
    /// messages do not have to be decoded. The runner is a pair of constants and
    /// is left out deliberately.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GhCliForge")
            .field("repo", &self.repo.slug())
            .field("program", &self.program)
            .finish_non_exhaustive()
    }
}

impl GhCliForge {
    /// Binds the adapter to a `gh` executable and a repository.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>, repo: RepoId) -> Self {
        Self {
            runner: ProcessRunner::new().with_timeout(TIMEOUT),
            program: program.into(),
            repo,
        }
    }

    /// Overrides the process runner, which tests use to shorten timeouts.
    #[must_use]
    pub fn with_runner(mut self, runner: ProcessRunner) -> Self {
        self.runner = runner;
        self
    }

    /// The repository being read.
    #[must_use]
    pub fn repo(&self) -> &RepoId {
        &self.repo
    }

    /// A unique private file for prose/JSON passed to `gh` by path rather than argv.
    /// Dry-run payloads live under the configured app home so the recorded command
    /// remains replayable; ordinary one-shot payloads use the system temporary area.
    fn payload_path(&self, stem: &str, number: u64, extension: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if let Some(path) = self
            .runner
            .dry_run_artifact_path(&format!("{stem}-pr-{number}"), extension)
        {
            return path;
        }
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "smart-review-{stem}-{}-{number}-{unique}.{extension}",
            std::process::id()
        ))
    }

    /// A `gh pr` command for this repository.
    ///
    /// `--repo` is appended to every one of them, so the current directory can
    /// never change which repository is read (ARCH-3).
    fn spec(&self, args: &[&str]) -> CommandSpec {
        CommandSpec::new(&self.program)
            .args(args)
            .arg("--repo")
            .arg(self.repo.slug())
    }

    /// A `gh api` command.
    ///
    /// `gh api` has no `--repo` flag; the repository goes in the endpoint, built
    /// from the parsed [`RepoId`] rather than from anything the user typed.
    fn api_spec(&self, endpoint: &str) -> CommandSpec {
        CommandSpec::new(&self.program).arg("api").arg(format!(
            "repos/{}/{}/{}",
            self.repo.owner(),
            self.repo.name(),
            endpoint.trim_start_matches('/')
        ))
    }

    /// Runs a command and decodes one JSON value from its output.
    fn json<T: DeserializeOwned>(&self, spec: &CommandSpec, cancel: &Cancel) -> Result<T> {
        let text = self.text(spec, cancel)?;
        serde_json::from_str(&text).map_err(|error| {
            Error::forge(
                spec.diagnostic(),
                format!("could not read the response: {error}"),
            )
        })
    }

    /// Runs a command and decodes a *stream* of JSON values, which is what
    /// `--paginate` produces.
    fn json_stream<T: DeserializeOwned>(
        &self,
        spec: &CommandSpec,
        cancel: &Cancel,
    ) -> Result<Vec<T>> {
        let text = self.text(spec, cancel)?;
        let stream = serde_json::Deserializer::from_str(&text).into_iter::<Vec<T>>();
        let mut items = Vec::new();
        for page in stream {
            match page {
                Ok(page) => items.extend(page),
                Err(error) => {
                    return Err(Error::forge(
                        spec.diagnostic(),
                        format!("could not read the response: {error}"),
                    ));
                }
            }
        }
        Ok(items)
    }

    /// Runs a command and returns its stdout.
    fn text(&self, spec: &CommandSpec, cancel: &Cancel) -> Result<String> {
        logging::log(Level::Debug, format!("running {}", spec.diagnostic()));
        let output = self.runner.run(spec, cancel).map_err(|error| {
            Error::forge(
                spec.diagnostic(),
                match error {
                    crate::adapters::process::ProcessError::NotFound { .. } => {
                        "the GitHub CLI was not found; install it from https://cli.github.com"
                            .to_owned()
                    }
                    other => other.to_string(),
                },
            )
        })?;

        if !output.success() {
            return Err(Error::forge(
                spec.diagnostic(),
                format!(
                    "exit {}: {}",
                    output
                        .code()
                        .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
                    output.stderr_tail()
                ),
            ));
        }
        if output.stdout_truncated {
            logging::log(
                Level::Warn,
                format!(
                    "{} produced more output than was captured",
                    spec.diagnostic()
                ),
            );
        }
        Ok(output.stdout)
    }

    /// Runs a command that changes something outside this process (FR-6.5).
    ///
    /// Under `--dry-run` the call is recorded and `Ok(None)` comes back: the caller
    /// must then behave as if nothing happened, because nothing did. Every mutating
    /// call in this adapter goes through here, so there is one place where the
    /// dry-run promise is kept rather than one per feature.
    fn mutate(&self, spec: &CommandSpec, cancel: &Cancel) -> Result<Option<Output>> {
        logging::log(Level::Debug, format!("running {}", spec.diagnostic()));
        let output = self.runner.run(spec, cancel).map_err(|error| {
            Error::forge(
                spec.diagnostic(),
                match error {
                    crate::adapters::process::ProcessError::NotFound { .. } => {
                        "the GitHub CLI was not found; install it from https://cli.github.com"
                            .to_owned()
                    }
                    other => other.to_string(),
                },
            )
        })?;
        if output.dry_run {
            return Ok(None);
        }
        Ok(Some(output))
    }

    /// The search string for the count query.
    fn search_query(&self, query: &PrQuery) -> String {
        let mut parts = vec![format!("repo:{}", self.repo.slug()), "is:pr".to_owned()];
        if let Some(qualifier) = query.state.qualifier() {
            parts.push(qualifier.to_owned());
        }
        for filter in &query.filters {
            if let Some(qualifier) = filter.qualifier() {
                parts.push(qualifier);
            }
        }
        parts.join(" ")
    }
}

impl ForgePort for GhCliForge {
    fn capabilities(&self) -> ForgeCapabilities {
        ForgeCapabilities::default()
    }

    fn list_pull_requests(&self, query: &PrQuery, cancel: &Cancel) -> Result<PullRequestPage> {
        // A query naming one PR is resolved by fetching it, not by searching for a
        // number: `gh pr list --search 141` would return unrelated PRs whose text
        // happens to contain 141 (FR-2.2).
        if let Some(number) = query.number() {
            let detail = self.get_pull_request(number, cancel)?;
            return Ok(PullRequestPage::complete(vec![detail.summary], query.limit));
        }

        let limit = query.limit.to_string();
        let search = query.server_search();
        let spec = self.spec(&[
            "pr",
            "list",
            "--state",
            query.state.as_gh_state(),
            "--limit",
            &limit,
            "--search",
            &search,
            "--json",
            LIST_FIELDS,
        ]);

        let summaries: Vec<GhSummary> = self.json(&spec, cancel)?;
        let items: Vec<_> = summaries.into_iter().map(GhSummary::into_domain).collect();

        // A short page is the whole answer; a full one may have been cut off, and
        // saying so is what stops the list from lying about being complete.
        Ok(
            if u32::try_from(items.len()).unwrap_or(u32::MAX) < query.limit {
                PullRequestPage::complete(items, query.limit)
            } else {
                PullRequestPage::possibly_truncated(items, query.limit)
            },
        )
    }

    fn count_pull_requests(&self, query: &PrQuery, cancel: &Cancel) -> Result<u32> {
        // A GraphQL variable rather than an interpolated string: the search text
        // can contain quotes (`in:title "retry webhook"`), which would otherwise
        // end the GraphQL string literal early.
        // `gh -f key=value` is *one* argv element: passing the value as a separate
        // argument makes gh reject the call ("accepts 1 arg(s), received 2"), which
        // is a silent failure because a missing count is not fatal (FR-2.1 only
        // degrades to "showing 50 of ≥50").
        let spec = CommandSpec::new(&self.program)
            .args([
                "api",
                "graphql",
                "-f",
                "query=query($q:String!){search(query:$q,type:ISSUE,first:1){issueCount}}",
            ])
            .arg(format!("q={}", self.search_query(query)));

        let response: GraphQlResponse = self.json(&spec, cancel)?;
        if let Some(errors) = response.errors.as_ref().filter(|errors| !errors.is_empty()) {
            let message = errors
                .first()
                .and_then(|error| error.message.clone())
                .unwrap_or_else(|| "unknown GraphQL error".to_owned());
            return Err(Error::forge(spec.diagnostic(), message));
        }
        response
            .data
            .and_then(|data| data.search)
            .and_then(|search| search.issue_count)
            .ok_or_else(|| Error::forge(spec.diagnostic(), "the response carried no count"))
    }

    fn get_pull_request(&self, number: u64, cancel: &Cancel) -> Result<PullRequestDetail> {
        let number_arg = number.to_string();
        let spec = self.spec(&["pr", "view", &number_arg, "--json", DETAIL_FIELDS]);

        let detail: GhDetail = self.json(&spec, cancel)?;
        let mut detail = detail.into_domain();

        // The inline comments are not part of `gh pr view`, and a failure to read
        // them must not lose the PR itself.
        match self.list_review_comments(number, cancel) {
            Ok(comments) => detail.comments = comments,
            Err(error) => logging::log(
                Level::Warn,
                format!("could not read the review comments of #{number}: {error}"),
            ),
        }
        // FR-6.4: whether those comments' threads are resolved, and what GitHub calls
        // them, is GraphQL-only. Fetched only when there is something to annotate — a
        // pull request with no inline comments has no threads — and skipped silently
        // free of charge when there are none.
        if !detail.comments.is_empty() {
            match self.read_review_threads(number, cancel) {
                Ok(threads) => {
                    comments::apply_threads(&mut detail.comments, &threads);
                }
                Err(error) => logging::log(
                    Level::Warn,
                    format!(
                        "could not read the review threads of #{number}, so no thread can be \
                         resolved: {error}"
                    ),
                ),
            }
        }
        // The conversation is the issue's comment list: a different endpoint from the
        // review comments, and separate for the same reason the domain types are.
        match self.read_conversation(number, cancel) {
            Ok(conversation) => detail.conversation = conversation,
            Err(error) => logging::log(
                Level::Warn,
                format!("could not read the conversation of #{number}: {error}"),
            ),
        }
        Ok(detail)
    }

    fn reply_to_review_comment(
        &self,
        number: u64,
        comment_id: u64,
        body: &str,
        cancel: &Cancel,
    ) -> Result<CommentPosted> {
        self.post_reply(number, comment_id, body, cancel)
    }

    fn comment_on_conversation(
        &self,
        number: u64,
        body: &str,
        cancel: &Cancel,
    ) -> Result<CommentPosted> {
        self.post_conversation_comment(number, body, cancel)
    }

    fn set_thread_resolved(&self, thread_id: &str, resolved: bool, cancel: &Cancel) -> Result<()> {
        self.set_thread_resolution(thread_id, resolved, cancel)
    }

    fn list_conversation(&self, number: u64, cancel: &Cancel) -> Result<Vec<ConversationComment>> {
        self.read_conversation(number, cancel)
    }

    fn list_reviews(&self, number: u64, cancel: &Cancel) -> Result<Vec<Review>> {
        #[derive(serde::Deserialize)]
        struct Envelope {
            #[serde(default)]
            reviews: Option<Vec<GhReview>>,
        }
        let number = number.to_string();
        let spec = self.spec(&["pr", "view", &number, "--json", "reviews"]);
        let envelope: Envelope = self.json(&spec, cancel)?;
        Ok(envelope
            .reviews
            .unwrap_or_default()
            .into_iter()
            .map(GhReview::into_domain)
            .collect())
    }

    fn list_review_comments(&self, number: u64, cancel: &Cancel) -> Result<Vec<ReviewComment>> {
        let spec = self.api_spec(&format!("pulls/{number}/comments")).args([
            "--paginate",
            "-X",
            "GET",
            "-f",
            "per_page=100",
        ]);
        let comments: Vec<GhReviewComment> = self.json_stream(&spec, cancel)?;
        Ok(comments
            .into_iter()
            .map(GhReviewComment::into_domain)
            .collect())
    }

    fn list_checks(&self, number: u64, cancel: &Cancel) -> Result<Vec<CheckRun>> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Envelope {
            #[serde(default)]
            status_check_rollup: Option<Vec<json::GhRollupItem>>,
        }
        let number = number.to_string();
        let spec = self.spec(&["pr", "view", &number, "--json", "statusCheckRollup"]);
        let envelope: Envelope = self.json(&spec, cancel)?;
        Ok(envelope
            .status_check_rollup
            .unwrap_or_default()
            .into_iter()
            .map(json::GhRollupItem::into_check)
            .collect())
    }

    fn pull_request_diff(&self, number: u64, cancel: &Cancel) -> Result<String> {
        let number = number.to_string();
        let spec = self.spec(&["pr", "diff", &number, "--patch"]);
        self.text(&spec, cancel)
    }

    fn submit_review(
        &self,
        number: u64,
        draft: &crate::domain::draft::Draft,
        cancel: &Cancel,
    ) -> Result<ReviewPosted> {
        self.submit_review(number, draft, cancel)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use crate::test_support::temp_home;

    /// Writes a fake `gh` that records its arguments and answers from files.
    ///
    /// The fake is invoked by absolute path rather than by putting it on `PATH`:
    /// mutating the process environment from a test would leak into every other
    /// test running in parallel, and `set_var` is unsafe in edition 2024 for
    /// exactly that reason.
    struct FakeGh {
        dir: crate::test_support::TempHome,
    }

    impl FakeGh {
        /// A fake that answers per subcommand, matched against `$*`.
        ///
        /// A key of `""` answers anything; anything else must appear in the
        /// argument list. A call with no match exits 1, which is how a failing
        /// command is simulated.
        fn scripted(responses: &[(&str, &str)]) -> Self {
            let dir = temp_home();
            let argv = dir.path().join("argv.txt");

            // Records one argument per line and ends the call with a unit
            // separator, so arguments containing `--` (such as `--json`) survive
            // being read back.
            let mut script = format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"{path}\"\nprintf '\\037\\n' >> \"{path}\"\n",
                path = argv.display()
            );
            // Keep a copy of anything passed with `--input`: the adapter deletes the
            // payload file once the call succeeds, and a test still has to be able to
            // see what was sent — asserting on what the fake *received* is the only
            // way to check the wire rather than the code that built it.
            let _ = writeln!(
                script,
                "prev=''\nfor arg in \"$@\"; do\n  if [ \"$prev\" = \"--input\" ] || [ \"$prev\" = \"--body-file\" ]; then\n    if [ -f \"$arg\" ]; then cp \"$arg\" \"{captured}\"; fi\n  fi\n  prev=\"$arg\"\ndone",
                captured = dir.path().join("input.json").display()
            );
            // The keys are matched against `$1:$2` rather than `$*`: a `case`
            // pattern may not contain whitespace (dash treats it as a separator
            // even inside `[ ]`), and the subcommand pair is what identifies the
            // call anyway.
            script.push_str("case \"$1:$2\" in\n");
            for (index, (key, body)) in responses.iter().enumerate() {
                let body_path = dir.write(&format!("body{index}.json"), body);
                let pattern = if key.is_empty() {
                    "*".to_owned()
                } else {
                    format!("*{key}*")
                };
                let _ = writeln!(script, "  {pattern}) cat \"{}\" ;;", body_path.display());
            }
            script.push_str(
                "  *) echo \"fake gh: unexpected call: $*\" >&2; exit 1 ;;\nesac\nexit 0\n",
            );
            dir.write_executable("gh", &script);

            Self { dir }
        }

        fn program(&self) -> PathBuf {
            self.dir.path().join("gh")
        }

        fn forge(&self, slug: &str) -> GhCliForge {
            GhCliForge::new(self.program(), RepoId::parse(slug).unwrap())
        }

        /// Every argument list the fake was called with, in order.
        fn calls(&self) -> Vec<Vec<String>> {
            std::fs::read_to_string(self.dir.path().join("argv.txt"))
                .unwrap_or_default()
                .split('\u{1f}')
                .filter(|call| !call.trim().is_empty())
                .map(|call| {
                    call.lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .collect()
        }

        fn last_call(&self) -> Vec<String> {
            self.calls().pop().unwrap_or_default()
        }

        /// The JSON body passed with `--input`, as the fake received it.
        fn input(&self) -> serde_json::Value {
            let text = std::fs::read_to_string(self.dir.path().join("input.json"))
                .expect("the fake kept a copy of --input");
            serde_json::from_str(&text).expect("valid JSON")
        }

        fn input_text(&self) -> String {
            std::fs::read_to_string(self.dir.path().join("input.json"))
                .expect("the fake kept a copy of the payload file")
        }
    }

    const LIST: &str = include_str!("../../../tests/fixtures/gh/pr-list.json");
    const VIEW: &str = include_str!("../../../tests/fixtures/gh/pr-view.json");
    const COMMENTS: &str = include_str!("../../../tests/fixtures/gh/review-comments.json");
    const COMMENTS_PAGE2: &str =
        include_str!("../../../tests/fixtures/gh/review-comments-page2.json");
    const COUNT: &str = include_str!("../../../tests/fixtures/gh/graphql-count.json");
    const DIFF: &str = include_str!("../../../tests/fixtures/gh/pr-diff.patch");

    fn a_draft_with(comments: usize) -> crate::domain::draft::Draft {
        use crate::domain::draft::{Decision, Draft, DraftComment, Side};
        let now = crate::domain::time::from_unix_secs(1_700_000_000);
        let mut draft = Draft::new(141, now);
        draft.set_decision(Some(Decision::RequestChanges), now);
        draft.set_body("One thing to fix.", now);
        draft.head_sha = Some("abc123".to_owned());
        for index in 0..comments {
            draft.add(
                DraftComment::new(
                    format!("src/domain/file{index}.rs"),
                    Side::New,
                    31 + u32::try_from(index).unwrap_or(0),
                    Some(28),
                    "this rounds up",
                )
                .expect("valid"),
                now,
            );
        }
        draft
    }

    #[test]
    fn a_review_without_comments_goes_through_gh_pr_review() {
        let fake = FakeGh::scripted(&[("pr:review", "Approved pull request #141")]);
        let forge = fake.forge("acme/service");

        let posted = forge
            .submit_review(141, &a_draft_with(0), &Cancel::new())
            .expect("published");

        assert!(!posted.dry_run);
        let call = fake.last_call();
        assert_eq!(&call[..4], ["pr", "review", "141", "--request-changes"]);
        assert!(
            call.windows(2)
                .any(|pair| pair[0] == "--repo" && pair[1] == "acme/service")
        );
        assert!(call.windows(2).any(|pair| pair[0] == "--body-file"));
        assert!(!call.iter().any(|arg| arg.contains("One thing to fix")));
        assert_eq!(fake.input_text(), "One thing to fix.");
    }

    #[test]
    fn an_approval_carries_no_body_when_there_is_none() {
        use crate::domain::draft::{Decision, Draft};
        let fake = FakeGh::scripted(&[("pr:review", "ok")]);
        let forge = fake.forge("acme/service");
        let now = crate::domain::time::from_unix_secs(1_700_000_000);
        let mut draft = Draft::new(141, now);
        draft.set_decision(Some(Decision::Approve), now);

        forge
            .submit_review(141, &draft, &Cancel::new())
            .expect("published");
        assert_eq!(
            fake.last_call(),
            vec!["pr", "review", "141", "--approve", "--repo", "acme/service"]
        );
    }

    #[test]
    fn a_review_with_comments_is_one_batched_call() {
        // The requirement is one review rather than N comments (FR-6.3), so the test
        // asserts the *count* of calls as well as their shape: one `gh api` call, and
        // the comments inside the payload rather than in arguments of their own.
        let fake = FakeGh::scripted(&[(
            "api:-X",
            r#"{"id": 4242, "html_url": "https://example.test/review/4242"}"#,
        )]);
        let forge = fake.forge("acme/service");

        let posted = forge
            .submit_review(141, &a_draft_with(3), &Cancel::new())
            .expect("published");

        assert_eq!(posted.id, Some(4242));
        assert_eq!(
            posted.url.as_deref(),
            Some("https://example.test/review/4242")
        );
        assert_eq!(fake.calls().len(), 1, "one review, one request");
        let call = fake.last_call();
        assert_eq!(call[0], "api");
        assert_eq!(call[1], "-X");
        assert_eq!(call[2], "POST");
        assert_eq!(call[3], "repos/acme/service/pulls/141/reviews");
        assert_eq!(call[4], "--input");

        // The payload is a file, and it held every comment verbatim — read back from
        // the copy the fake kept, because the adapter deletes the original.
        let payload = fake.input();
        assert_eq!(payload["event"], "REQUEST_CHANGES");
        assert_eq!(payload["body"], "One thing to fix.");
        assert_eq!(payload["commit_id"], "abc123");
        let comments = payload["comments"].as_array().expect("an array");
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0]["path"], "src/domain/file0.rs");
        assert_eq!(comments[0]["line"], 31);
        assert_eq!(comments[0]["start_line"], 28);
        assert_eq!(comments[0]["side"], "RIGHT");
        assert_eq!(comments[2]["line"], 33);
    }

    #[test]
    fn the_payload_file_is_cleaned_up_after_a_review() {
        let fake = FakeGh::scripted(&[("api:-X", r#"{"id": 1}"#)]);
        let forge = fake.forge("acme/service");
        forge
            .submit_review(141, &a_draft_with(1), &Cancel::new())
            .expect("published");
        let path = fake.last_call()[5].clone();
        assert!(
            !std::path::Path::new(&path).exists(),
            "a payload file left behind is prose in the temporary directory forever: {path}"
        );
        assert_eq!(
            fake.input()["comments"].as_array().map(Vec::len),
            Some(1),
            "and it was the review that was sent"
        );
    }

    #[test]
    fn a_refused_review_is_reported_with_the_draft_still_in_hand() {
        // The draft is the caller's, so "preserved" here means the adapter did not
        // consume it — and the error is a sentence rather than an HTTP body.
        let fake = FakeGh::scripted(&[]);
        let forge = fake.forge("acme/service");
        let draft = a_draft_with(1);
        let error = forge
            .submit_review(141, &draft, &Cancel::new())
            .expect_err("refused");
        assert!(
            error.to_string().contains("this rounds up") || error.to_string().contains("fake gh"),
            "what gh said: {error}"
        );
        assert_eq!(draft.comments.len(), 1, "the draft is untouched");
    }

    #[test]
    fn a_draft_with_nothing_to_say_is_refused_before_any_call() {
        use crate::domain::draft::Draft;
        let fake = FakeGh::scripted(&[("pr:review", "ok")]);
        let forge = fake.forge("acme/service");
        let draft = Draft::new(141, crate::domain::time::from_unix_secs(1_700_000_000));
        let error = forge
            .submit_review(141, &draft, &Cancel::new())
            .expect_err("refused");
        assert!(error.to_string().contains("nothing to send"), "{error}");
        assert!(fake.calls().is_empty(), "nothing was sent to GitHub");
    }

    #[test]
    fn a_reply_goes_to_the_reply_route_with_a_private_json_payload() {
        let fake =
            FakeGh::scripted(&[("api", r#"{"id":7,"html_url":"https://example.test/c/7"}"#)]);
        let forge = fake.forge("acme/service");
        let body = "agreed \"fixed\" in 9f2c1ab, and C:\\path too";
        let posted = forge
            .reply_to_review_comment(141, 1001, body, &Cancel::new())
            .expect("posted");

        assert_eq!(posted.id, Some(7));
        assert_eq!(posted.url.as_deref(), Some("https://example.test/c/7"));
        assert!(!posted.dry_run);

        let call = &fake.calls()[0];
        assert_eq!(call[0], "api");
        assert!(
            call.contains(&"repos/acme/service/pulls/141/comments/1001/replies".to_owned()),
            "{call:?}"
        );
        assert!(!call.iter().any(|arg| arg.contains(body)), "{call:?}");
        assert_eq!(fake.input()["body"], body);
    }

    #[test]
    fn a_conversation_comment_goes_to_the_issues_comment_list() {
        let fake = FakeGh::scripted(&[("issues/141/comments", r#"{"id":8}"#)]);
        let forge = fake.forge("acme/service");
        forge
            .comment_on_conversation(141, "thanks, looking now", &Cancel::new())
            .expect("posted");

        let call = &fake.calls()[0];
        assert!(
            call.contains(&"repos/acme/service/issues/141/comments".to_owned()),
            "a pull request is an issue, and its conversation is the issue's comments: {call:?}"
        );
        assert!(!call.iter().any(|arg| arg.contains("thanks, looking now")));
        assert_eq!(fake.input()["body"], "thanks, looking now");
    }

    #[test]
    fn resolving_a_thread_is_a_graphql_mutation_naming_that_thread() {
        let answer =
            r#"{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":true}}}}"#;
        let fake = FakeGh::scripted(&[("graphql", answer)]);
        let forge = fake.forge("acme/service");
        forge
            .set_thread_resolved("PRRT_1", true, &Cancel::new())
            .expect("resolved");

        let call = &fake.calls()[0];
        assert_eq!(call[0], "api");
        assert_eq!(call[1], "graphql");
        assert!(
            call.iter().any(|arg| arg.contains("resolveReviewThread")),
            "{call:?}"
        );
        assert!(
            call.iter().any(|arg| arg == "id=PRRT_1"),
            "the thread id is a variable: {call:?}"
        );

        // And the other direction is the other field, not the same one with a flag.
        let fake = FakeGh::scripted(&[("graphql", answer)]);
        let forge = fake.forge("acme/service");
        forge
            .set_thread_resolved("PRRT_1", false, &Cancel::new())
            .expect("resolved");
        let call = &fake.calls()[0];
        assert!(
            call.iter().any(|arg| arg.contains("unresolveReviewThread")),
            "{call:?}"
        );
        assert!(
            !call
                .iter()
                .any(|arg| arg.contains("mutation($id: ID!) {resolve")),
            "and not the resolving one: {call:?}"
        );
    }

    #[test]
    fn a_graphql_error_in_a_zero_exit_answer_is_a_failure() {
        // `gh api graphql` exits zero when the *query* failed and puts the reason in
        // the body. Reporting success there would say a thread was resolved when
        // nothing had happened.
        let answer = r#"{"data":null,"errors":[{"message":"Could not resolve to a node with the global id of 'PRRT_nope'."}]}"#;
        let fake = FakeGh::scripted(&[("graphql", answer)]);
        let forge = fake.forge("acme/service");
        let error = forge
            .set_thread_resolved("PRRT_nope", true, &Cancel::new())
            .expect_err("refused");
        assert!(
            error.to_string().contains("Could not resolve to a node"),
            "{error}"
        );
    }

    #[test]
    fn a_refused_reply_is_translated_rather_than_quoted() {
        // The fake exits 1 for a call it has no answer for, and prints what a failing
        // gh prints: the adapter must turn that into a sentence.
        let dir = temp_home();
        let script =
            "#!/bin/sh\necho 'gh: Resource not accessible by integration (HTTP 403)' >&2\nexit 1\n";
        dir.write_executable("gh", script);
        let forge = GhCliForge::new(
            dir.path().join("gh"),
            RepoId::parse("acme/service").unwrap(),
        );
        let error = forge
            .reply_to_review_comment(141, 1001, "hello", &Cancel::new())
            .expect_err("refused");
        assert!(
            error.to_string().contains("token is not allowed"),
            "{error}"
        );
    }

    #[test]
    fn posting_anything_during_a_dry_run_reaches_nothing() {
        let fake = FakeGh::scripted(&[("-X", "{}"), ("graphql", "{}")]);
        let artifacts = fake.dir.path().join("exports/dry-run");
        let runner = ProcessRunner::new().with_dry_run(
            crate::adapters::process::DryRunLedger::in_directory(&artifacts),
        );
        let ledger = runner.dry_run_ledger().expect("a ledger").clone();
        let forge = fake.forge("acme/service").with_runner(runner);

        let reply = forge
            .reply_to_review_comment(141, 1001, "hello", &Cancel::new())
            .expect("recorded");
        let conversation = forge
            .comment_on_conversation(141, "hello", &Cancel::new())
            .expect("recorded");
        forge
            .set_thread_resolved("PRRT_1", true, &Cancel::new())
            .expect("recorded");

        assert!(reply.dry_run && conversation.dry_run);
        assert!(
            fake.calls().is_empty(),
            "nothing reached gh: {:?}",
            fake.calls()
        );
        let commands = ledger.commands();
        assert_eq!(commands.len(), 3, "{commands:#?}");
        assert!(
            commands[0].contains("comments/1001/replies"),
            "{}",
            commands[0]
        );
        assert!(
            commands[1].contains("issues/141/comments"),
            "{}",
            commands[1]
        );
        assert!(
            commands[2].contains("resolveReviewThread"),
            "{}",
            commands[2]
        );
        assert!(
            commands[..2]
                .iter()
                .all(|command| !command.contains("hello")),
            "ordinary command records contain paths, not bodies: {commands:#?}"
        );
        let payloads = std::fs::read_dir(&artifacts)
            .expect("the app-owned artifact directory exists")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("payload entries are readable");
        assert_eq!(payloads.len(), 2);
        for payload in payloads {
            let text = std::fs::read_to_string(payload.path()).expect("payload is readable");
            assert!(text.contains("hello"), "the explicit artifact is exact");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = payload.metadata().expect("metadata").permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{} is private", payload.path().display());
            }
        }
    }

    #[test]
    fn a_dry_run_records_the_review_instead_of_posting_it() {
        let fake = FakeGh::scripted(&[("pr:review", "ok"), ("api:-X", "{}")]);
        let runner =
            ProcessRunner::new().with_dry_run(crate::adapters::process::DryRunLedger::new());
        let ledger = runner.dry_run_ledger().expect("a ledger").clone();
        let forge = fake.forge("acme/service").with_runner(runner);

        let posted = forge
            .submit_review(141, &a_draft_with(2), &Cancel::new())
            .expect("recorded");

        assert!(posted.dry_run);
        assert!(fake.calls().is_empty(), "nothing reached gh");
        let commands = ledger.commands();
        assert_eq!(commands.len(), 1);
        assert!(
            commands[0].contains("api -X POST repos/acme/service/pulls/141/reviews --input"),
            "{}",
            commands[0]
        );
        // The command is one a person can run: the payload it names exists.
        let path = commands[0]
            .rsplit("--input ")
            .next()
            .expect("a path")
            .trim()
            .to_owned();
        assert!(
            std::path::Path::new(&path).exists(),
            "a dry run writes the payload so the recorded command works: {path}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_list_call_passes_the_repository_state_limit_search_and_fields() {
        let fake = FakeGh::scripted(&[("pr:list", LIST)]);
        let forge = fake.forge("acme/service");
        let query = PrQuery::with_limit(50);

        let page = forge.list_pull_requests(&query, &Cancel::new()).unwrap();
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.total, Some(3));

        let call = fake.last_call();
        assert_eq!(
            call,
            vec![
                "pr",
                "list",
                "--state",
                "open",
                "--limit",
                "50",
                "--search",
                "is:open sort:created-desc",
                "--json",
                LIST_FIELDS,
                "--repo",
                "acme/service",
            ],
            "the exact argv matters: gh reads it positionally"
        );
    }

    #[test]
    fn a_full_page_is_flagged_as_possibly_truncated() {
        let fake = FakeGh::scripted(&[("pr:list", LIST)]);
        let forge = fake.forge("acme/service");
        // Asking for three means the three-item fixture fills the page exactly.
        let page = forge
            .list_pull_requests(&PrQuery::with_limit(3), &Cancel::new())
            .unwrap();
        assert!(page.may_have_more());
        assert_eq!(page.total, None, "the total is unknown until it is counted");
    }

    #[test]
    fn every_search_call_carries_the_repository_flag() {
        let fake = FakeGh::scripted(&[("pr:list", LIST)]);
        let forge = fake.forge("acme/service");
        let mut query = PrQuery::with_limit(10);
        query.push(crate::domain::query::Filter::Author("alice".to_owned()));
        query.set_state(crate::domain::query::PrStateFilter::Merged);

        forge.list_pull_requests(&query, &Cancel::new()).unwrap();
        let call = fake.last_call();
        assert!(call.ends_with(&["--repo".to_owned(), "acme/service".to_owned()]));
        let search = call.join(" ");
        assert!(search.contains("is:merged"), "{search}");
        assert!(search.contains("author:alice"), "{search}");
        assert!(search.contains("--state merged"), "{search}");
    }

    #[test]
    fn a_number_filter_fetches_that_pull_request_instead_of_searching() {
        let fake = FakeGh::scripted(&[("pr:view", VIEW)]);
        let forge = fake.forge("acme/service");
        let mut query = PrQuery::with_limit(50);
        query.push(crate::domain::query::Filter::Number(141));

        let page = forge.list_pull_requests(&query, &Cancel::new()).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].number, 141);

        // The first call is the view; the second reads the inline comments.
        let calls = fake.calls();
        let call = &calls[0];
        assert_eq!(call[0], "pr");
        assert_eq!(call[1], "view");
        assert_eq!(call[2], "141");
        assert!(
            !call.contains(&"list".to_owned()),
            "a number is resolved directly, not searched for: {call:?}"
        );
    }

    #[test]
    fn a_view_call_also_reads_the_inline_comments() {
        let fake = FakeGh::scripted(&[("pr:view", VIEW), ("comments", COMMENTS)]);
        let forge = fake.forge("acme/service");
        let detail = forge.get_pull_request(141, &Cancel::new()).unwrap();

        assert_eq!(detail.summary.number, 141);
        assert_eq!(detail.comments.len(), 2, "the second call filled these in");

        // Four calls: the view, the inline comments, the thread state that annotates
        // them, and the conversation. FR-6.4 needs all four to draw a pull request as
        // it is rather than as a list of bodies.
        let calls = fake.calls();
        assert_eq!(calls.len(), 4, "{calls:#?}");
        assert_eq!(calls[0][1], "view");
        assert_eq!(calls[1][0], "api");
        assert_eq!(calls[1][1], "repos/acme/service/pulls/141/comments");
        assert_eq!(calls[2][0], "api");
        assert_eq!(calls[2][1], "graphql");
        assert!(
            calls[2].iter().any(|arg| arg.contains("reviewThreads")),
            "the thread state is a GraphQL field: {calls:?}"
        );
        assert_eq!(
            calls[3][1], "repos/acme/service/issues/141/comments",
            "the conversation is the issue's comment list"
        );
        assert!(
            !calls[1].contains(&"--repo".to_owned()),
            "gh api takes the repository in the path and has no --repo flag"
        );
    }

    #[test]
    fn thread_state_is_joined_onto_the_comments_it_belongs_to() {
        let threads = r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
            {"id":"PRRT_1","isResolved":true,"isOutdated":false,
             "comments":{"nodes":[{"databaseId":1001},{"databaseId":1002}]}},
            {"id":"PRRT_2","isResolved":false,"isOutdated":true,
             "comments":{"nodes":[{"databaseId":1003}]}}
        ]}}}}}"#;
        let fake = FakeGh::scripted(&[
            ("pr:view", VIEW),
            ("comments", COMMENTS),
            ("graphql", threads),
        ]);
        let forge = fake.forge("acme/service");
        let detail = forge.get_pull_request(141, &Cancel::new()).unwrap();

        let resolved = detail
            .comments
            .iter()
            .find(|comment| comment.id == 1001)
            .expect("the root comment");
        assert_eq!(resolved.thread_id.as_deref(), Some("PRRT_1"));
        assert!(resolved.resolved);
        assert!(
            !resolved.outdated,
            "resolved and out of date are different things"
        );
        let reply = detail
            .comments
            .iter()
            .find(|comment| comment.id == 1002)
            .expect("the reply");
        assert!(
            reply.resolved,
            "a reply is in the same thread as what it answers"
        );
    }

    #[test]
    fn a_failure_to_read_thread_state_leaves_every_thread_open_and_unresolvable() {
        // The distinction the whole design turns on: no thread state means no thread
        // can be resolved, which is *not* the same as every thread being open.
        let fake = FakeGh::scripted(&[("pr:view", VIEW), ("comments", COMMENTS)]);
        let forge = fake.forge("acme/service");
        let detail = forge.get_pull_request(141, &Cancel::new()).unwrap();

        assert_eq!(detail.comments.len(), 2, "the comments still arrived");
        assert!(
            detail
                .comments
                .iter()
                .all(|comment| comment.thread_id.is_none() && !comment.resolved),
            "and none of them claims to know its thread"
        );
    }

    #[test]
    fn a_failure_to_read_comments_does_not_lose_the_pull_request() {
        // The fake exits 1 for any call it was not given a response for, which is
        // what a comment-endpoint failure looks like.
        let fake = FakeGh::scripted(&[("pr:view", VIEW)]);
        let forge = fake.forge("acme/service");
        let detail = forge.get_pull_request(141, &Cancel::new()).unwrap();
        assert_eq!(detail.summary.number, 141);
        assert!(detail.comments.is_empty());
    }

    #[test]
    fn paginated_comments_are_read_as_a_stream_of_pages() {
        // `gh --paginate` concatenates pages as separate JSON documents; `--slurp`
        // does not exist on the minimum supported gh, so both pages must be read.
        let two_pages = format!("{COMMENTS}{COMMENTS_PAGE2}");
        let fake = FakeGh::scripted(&[("comments", &two_pages)]);
        let forge = fake.forge("acme/service");
        let comments = forge.list_review_comments(141, &Cancel::new()).unwrap();
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[2].path, "src/infra/pg.rs");
        assert_eq!(comments[2].author, "carol");
    }

    #[test]
    fn an_empty_comment_page_is_an_empty_list_not_an_error() {
        let fake = FakeGh::scripted(&[("comments", "[]")]);
        let forge = fake.forge("acme/service");
        assert!(
            forge
                .list_review_comments(1, &Cancel::new())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_count_query_keeps_the_search_text_out_of_the_graphql_literal() {
        let fake = FakeGh::scripted(&[("graphql", COUNT)]);
        let forge = fake.forge("acme/service");
        let mut query = PrQuery::with_limit(50);
        query.push(crate::domain::query::Filter::Title(
            "retry \"webhook\"".to_owned(),
        ));

        assert_eq!(
            forge.count_pull_requests(&query, &Cancel::new()).unwrap(),
            137
        );

        let call = fake.last_call();
        let query_index = call.iter().position(|arg| arg == "-f").unwrap();
        assert!(
            call[query_index + 1].contains("($q:String!)"),
            "the search text must go through a variable: {call:?}"
        );
        // `gh -f key=value` is one argument; passing the value separately is
        // rejected by gh ("accepts 1 arg(s), received 2"). These assertions pin the
        // shape — a test that only inspected the last element would have passed with
        // the broken argv and the count would silently never have worked.
        let search = call.last().unwrap();
        assert_eq!(
            search.split('=').next(),
            Some("q"),
            "the search text must be the value of `q`: {search}"
        );
        assert!(
            search.starts_with("q=repo:acme/service is:pr is:open"),
            "{search}"
        );
        assert!(search.contains("in:title"), "{search}");
        assert!(
            !call.iter().any(|arg| arg == "q="),
            "an empty value plus a separate argument is the shape gh rejects: {call:?}"
        );
    }

    #[test]
    fn a_graphql_error_is_reported_with_its_message() {
        let fake = FakeGh::scripted(&[(
            "graphql",
            r#"{"data":null,"errors":[{"message":"Could not resolve to a Repository"}]}"#,
        )]);
        let forge = fake.forge("acme/service");
        let error = forge
            .count_pull_requests(&PrQuery::with_limit(1), &Cancel::new())
            .unwrap_err();
        assert!(error.to_string().contains("Could not resolve"), "{error}");
    }

    #[test]
    fn a_diff_comes_back_as_text() {
        let fake = FakeGh::scripted(&[("pr:diff", DIFF)]);
        let forge = fake.forge("acme/service");
        let text = forge.pull_request_diff(141, &Cancel::new()).unwrap();
        assert!(text.starts_with("diff --git"));

        let parsed = forge.pull_request_patch(141, &Cancel::new()).unwrap();
        assert_eq!(parsed.files.len(), 4, "the fixture has four files");
        assert_eq!(
            parsed.find(&crate::domain::diff::RelPath::parse("new/name.rs").unwrap()),
            Some(3)
        );

        let call = fake.last_call();
        assert_eq!(
            call,
            vec!["pr", "diff", "141", "--patch", "--repo", "acme/service"]
        );
    }

    #[test]
    fn reviews_and_checks_are_fetched_by_their_own_field_lists() {
        let fake = FakeGh::scripted(&[("pr:view", VIEW)]);
        let forge = fake.forge("acme/service");

        let reviews = forge.list_reviews(141, &Cancel::new()).unwrap();
        assert_eq!(reviews.len(), 3);
        assert!(fake.last_call().contains(&"reviews".to_owned()));

        let checks = forge.list_checks(141, &Cancel::new()).unwrap();
        assert_eq!(checks.len(), 3);
        assert!(fake.last_call().contains(&"statusCheckRollup".to_owned()));
    }

    #[test]
    fn a_failing_command_reports_the_exit_code_and_the_stderr_tail() {
        // No scripted answer means the fake exits 1 with a message on stderr.
        let fake = FakeGh::scripted(&[("pr:list", LIST)]);
        let forge = fake.forge("acme/service");
        let error = forge.pull_request_diff(141, &Cancel::new()).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("exit 1"), "{text}");
        assert!(text.contains("unexpected call"), "{text}");
        // FR-9.1 wants a copyable command alongside the message, which is why the
        // error keeps the two apart.
        let command = error.command().unwrap();
        assert!(command.contains("gh pr diff 141 --patch"), "{command}");
    }

    #[test]
    fn a_missing_gh_says_to_install_it() {
        let forge = GhCliForge::new("/nonexistent/gh", RepoId::parse("acme/service").unwrap());
        let error = forge
            .list_pull_requests(&PrQuery::default(), &Cancel::new())
            .unwrap_err();
        assert!(error.to_string().contains("cli.github.com"), "{error}");
    }

    #[test]
    fn output_that_is_not_json_is_reported_with_the_command() {
        let fake = FakeGh::scripted(&[("pr:list", "this is not json")]);
        let forge = fake.forge("acme/service");
        let error = forge
            .list_pull_requests(&PrQuery::default(), &Cancel::new())
            .unwrap_err();
        assert!(
            error.to_string().contains("could not read the response"),
            "{error}"
        );
        assert!(
            error.command().unwrap().contains("gh pr list"),
            "{:?}",
            error.command()
        );
    }

    #[test]
    fn a_cancelled_call_does_not_wait_for_the_timeout() {
        let dir = temp_home();
        let program = dir.write_executable("gh", "#!/bin/sh\nsleep 30\n");

        let forge = GhCliForge::new(&program, RepoId::parse("acme/service").unwrap());
        let cancel = Cancel::new();
        let flag = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            flag.cancel();
        });

        let started = std::time::Instant::now();
        let error = forge
            .list_pull_requests(&PrQuery::default(), &cancel)
            .unwrap_err();
        assert!(error.to_string().contains("cancel"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn the_repository_is_exposed_for_the_rest_of_the_app() {
        let forge = GhCliForge::new("gh", RepoId::parse("acme/service").unwrap());
        assert_eq!(forge.repo().slug(), "acme/service");
        assert!(forge.capabilities().batched_review);
        assert!(forge.capabilities().inline_comments);
        assert!(format!("{forge:?}").contains("acme/service"));
    }
}
