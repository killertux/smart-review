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

pub mod json;
pub mod probe;

use std::path::PathBuf;

use serde::de::DeserializeOwned;

use crate::adapters::process::{CommandSpec, ProcessRunner};
use crate::domain::pr::{CheckRun, PullRequestDetail, Review, ReviewComment};
use crate::domain::query::PrQuery;
use crate::domain::repo::RepoId;
use crate::error::{Error, Result};
use crate::logging::{self, Level};
use crate::ports::Cancel;
use crate::ports::forge::{ForgeCapabilities, ForgeFactory, ForgePort, PullRequestPage};

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
}

impl GhForgeFactory {
    /// Uses the configured `gh` program.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl ForgeFactory for GhForgeFactory {
    fn forge(&self, repo: &RepoId) -> std::sync::Arc<dyn ForgePort> {
        std::sync::Arc::new(GhCliForge::new(&self.program, repo.clone()))
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
                spec.render(),
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
                        spec.render(),
                        format!("could not read the response: {error}"),
                    ));
                }
            }
        }
        Ok(items)
    }

    /// Runs a command and returns its stdout.
    fn text(&self, spec: &CommandSpec, cancel: &Cancel) -> Result<String> {
        logging::log(Level::Debug, format!("running {}", spec.render()));
        let output = self.runner.run(spec, cancel).map_err(|error| {
            Error::forge(
                spec.render(),
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
                spec.render(),
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
                format!("{} produced more output than was captured", spec.render()),
            );
        }
        Ok(output.stdout)
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
            return Err(Error::forge(spec.render(), message));
        }
        response
            .data
            .and_then(|data| data.search)
            .and_then(|search| search.issue_count)
            .ok_or_else(|| Error::forge(spec.render(), "the response carried no count"))
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
        Ok(detail)
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
            dir.write("gh", &script);
            set_executable(&dir.path().join("gh"));

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
    }

    #[cfg(unix)]
    fn set_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }

    #[cfg(not(unix))]
    fn set_executable(_path: &std::path::Path) {}

    const LIST: &str = include_str!("../../../tests/fixtures/gh/pr-list.json");
    const VIEW: &str = include_str!("../../../tests/fixtures/gh/pr-view.json");
    const COMMENTS: &str = include_str!("../../../tests/fixtures/gh/review-comments.json");
    const COMMENTS_PAGE2: &str =
        include_str!("../../../tests/fixtures/gh/review-comments-page2.json");
    const COUNT: &str = include_str!("../../../tests/fixtures/gh/graphql-count.json");
    const DIFF: &str = include_str!("../../../tests/fixtures/gh/pr-diff.patch");

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

        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][1], "view");
        assert_eq!(calls[1][0], "api");
        assert_eq!(calls[1][1], "repos/acme/service/pulls/141/comments");
        assert!(
            !calls[1].contains(&"--repo".to_owned()),
            "gh api takes the repository in the path and has no --repo flag"
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
        let program = dir.path().join("gh");
        dir.write("gh", "#!/bin/sh\nsleep 30\n");
        set_executable(&program);

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
