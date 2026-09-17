//! Managed worktrees: materialising a pull request and diffing it locally
//! (FR-3.1, FR-3.2, DEC-1).
//!
//! This module is split out of `git.rs` because it is a different job: `git.rs`
//! answers "where am I and what is git", this answers "put this pull request on
//! disk and show me what changed". It is an inherent `impl` on the same
//! [`GitCli`], so the runner, the timeout and the error mapping are shared rather
//! than duplicated.
//!
//! # What is never touched
//!
//! The user's working tree, index, `HEAD`, branches and `.git`. Fetches, refs and
//! worktree bookkeeping live in an app-owned bare object store below
//! [`GitCli::worktrees_root`].
//!
//! # Why an explicit ref for the pull request head
//!
//! Both refs are fetched into explicit destinations rather than left in
//! `FETCH_HEAD`: with two refspecs `FETCH_HEAD` holds two lines and picking the
//! right one out of it depends on argument order, which is exactly the kind of
//! fragile parse that breaks silently. The head is fetched into
//! `refs/smart-review/<encoded-repository-id>/pr-<N>/head`, which is namespaced, never
//! appears in `git branch`, and is deleted by
//! [`GitCli::remove_workspace`].

use std::path::{Path, PathBuf};

use crate::adapters::git::GitCli;
use crate::adapters::process::{CommandSpec, Output, ProcessError};
use crate::domain::repo::RepoId;
use crate::logging::{self, Level};
use crate::ports::Cancel;
use crate::ports::workspace::{
    DiffOptions, DiffRequest, Workspace, WorkspaceEntry, WorkspaceError, WorkspaceRequest,
};

/// The timeout for fetch and worktree operations, which touch the network and the
/// disk and are slower than a `rev-parse` (FR-3.1).
const SLOW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

impl GitCli {
    /// Where managed worktrees live.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError::Failed`] when no root was configured, which is a
    /// wiring mistake rather than something a user can fix.
    pub(crate) fn worktrees_root(&self) -> Result<&Path, WorkspaceError> {
        self.worktrees_root
            .as_deref()
            .ok_or_else(|| WorkspaceError::Failed("no worktree directory is configured".to_owned()))
    }

    /// Runs git, requiring success.
    pub(crate) fn run_checked(
        &self,
        args: &[&str],
        cancel: &Cancel,
    ) -> Result<String, WorkspaceError> {
        let output = self.run_slow(args, cancel)?;
        if output.success() {
            Ok(output.stdout)
        } else {
            Err(Self::failure(args, &output.stderr))
        }
    }

    /// Runs git with the slower timeout that fetching and worktree creation need.
    fn run_slow(&self, args: &[&str], cancel: &Cancel) -> Result<Output, WorkspaceError> {
        let spec = self.spec_with_timeout(args, SLOW_TIMEOUT);
        self.runner
            .run(&spec, cancel)
            .map_err(ProcessError::into_workspace)
    }

    /// Runs git in another directory than the configured one.
    pub(crate) fn run_in(
        &self,
        dir: &Path,
        args: &[&str],
        cancel: &Cancel,
    ) -> Result<(bool, String, String), WorkspaceError> {
        let spec = CommandSpec::new(&self.program).args(args).current_dir(dir);
        self.run_in_spec(&spec, cancel)
    }

    /// Runs a prepared spec, so a caller can mark a destructive call as such.
    fn run_in_spec(
        &self,
        spec: &CommandSpec,
        cancel: &Cancel,
    ) -> Result<(bool, String, String), WorkspaceError> {
        match self.runner.run(spec, cancel) {
            Ok(output) => Ok((output.success(), output.stdout, output.stderr)),
            Err(ProcessError::NotFound { .. }) => Err(WorkspaceError::GitUnavailable(
                "install git and make sure it is on PATH".to_owned(),
            )),
            Err(ProcessError::Cancelled { .. }) => Err(WorkspaceError::Cancelled),
            Err(error) => Err(WorkspaceError::Failed(error.to_string())),
        }
    }

    fn failure(args: &[&str], stderr: &str) -> WorkspaceError {
        let detail = stderr.trim();
        logging::log(
            Level::Debug,
            format!("git {} failed: {detail}", args.join(" ")),
        );
        if detail.is_empty() {
            WorkspaceError::Failed(format!("git {} failed with no output", args.join(" ")))
        } else {
            WorkspaceError::Failed(detail.to_owned())
        }
    }

    /// The directory holding this repository's app-owned object store.
    #[must_use]
    pub(crate) fn store_path(&self, repo: &RepoId) -> Option<PathBuf> {
        self.worktrees_root
            .as_ref()
            .map(|root| root.join("git").join(repo.storage_key()).join("repo.git"))
    }

    /// The directory a pull request's worktree lives in (FR-3.1).
    #[must_use]
    pub(crate) fn worktree_path(&self, repo: &RepoId, number: u64) -> Option<PathBuf> {
        self.worktrees_root.as_ref().map(|root| {
            root.join("checkouts")
                .join(repo.storage_key())
                .join(format!("pr-{number}"))
        })
    }

    /// The ref the pull request's head is fetched into.
    fn head_ref(repo: &RepoId, number: u64) -> String {
        format!("refs/smart-review/{}/pr-{number}/head", repo.storage_key())
    }

    /// The fetched base reference for a PR. It is separate from the mutable local
    /// branch name and belongs to the same request snapshot as the head (IR-13).
    fn base_ref(repo: &RepoId, number: u64) -> String {
        format!("refs/smart-review/{}/pr-{number}/base", repo.storage_key())
    }

    /// Whether an existing worktree already holds `head_sha` (FR-3.1: reuse first).
    fn existing_head(&self, path: &Path) -> Option<String> {
        let (ok, stdout, _) = self
            .run_in(path, &["rev-parse", "HEAD"], &Cancel::new())
            .ok()?;
        ok.then(|| stdout.trim().to_owned())
    }

    /// Materialises a pull request, reusing a worktree that already holds its head.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] for the reason the workspace could not be built,
    /// naming the git failure rather than a generic message.
    pub(crate) fn ensure_workspace(
        &self,
        request: &WorkspaceRequest,
        cancel: &Cancel,
    ) -> Result<Workspace, WorkspaceError> {
        let path = self
            .worktree_path(&request.repo, request.number)
            .ok_or_else(|| {
                WorkspaceError::Failed("no worktree directory is configured".to_owned())
            })?;

        let store = self.store_path(&request.repo).ok_or_else(|| {
            WorkspaceError::Failed("no worktree directory is configured".to_owned())
        })?;
        self.ensure_store(&store, &request.repo, cancel)?;

        // Fetch both immutable request refs before considering reuse. A local branch
        // can be stale and the PR can keep its head while changing base; neither must
        // affect the resulting merge base (IR-13).
        let base_ref = Self::base_ref(&request.repo, request.number);
        let head_ref = Self::head_ref(&request.repo, request.number);
        let base_refspec = format!("+refs/heads/{}:{base_ref}", request.base);
        let head_refspec = format!("+refs/pull/{}/head:{head_ref}", request.number);
        let fallback_remote_url;
        let remote_url = if let Some(url) = request.remote_url.as_deref() {
            url
        } else {
            // Older in-memory callers and local-path fixture remotes cannot be
            // represented as forge repository URLs during detection. This read is
            // intentionally the only fallback: it reads config only, never updates
            // the source clone, and normal jobs carry the captured URL (IR-13).
            fallback_remote_url = self.remote_url(&request.remote, cancel)?;
            &fallback_remote_url
        };
        self.run_checked_in(
            &store,
            &[
                "fetch",
                "--no-tags",
                remote_url,
                &base_refspec,
                &head_refspec,
            ],
            cancel,
        )
        .map_err(|error| match error {
            WorkspaceError::Failed(detail) => WorkspaceError::Fetch(detail),
            other => other,
        })?;
        let head_sha = self.rev_parse_in(&store, &format!("{head_ref}^{{commit}}"), cancel)?;
        if head_sha != request.head_sha {
            return Err(WorkspaceError::Fetch(format!(
                "pull request #{} resolved to {head_sha}, expected {} — refresh the pull request and retry",
                request.number, request.head_sha
            )));
        }
        let base_sha = self
            .merge_base(&store, &base_ref, &head_ref, cancel)?
            .ok_or_else(|| {
                WorkspaceError::NoMergeBase(format!(
                    "{} and {} share no history",
                    request.base, head_sha
                ))
            })?;

        // Reuse only after the base/head request snapshot was resolved in the app's
        // object store.
        if path.is_dir()
            && let Some(head) = self.existing_head(&path)
            && head == request.head_sha
        {
            logging::log(
                Level::Debug,
                format!("reusing the worktree at {}", path.display()),
            );
            return Ok(Workspace {
                path,
                base_sha,
                head_sha: head,
                reused: true,
            });
        }

        // 2. A worktree that exists but holds something else is removed first:
        //    `git worktree add` refuses a directory that is already registered.
        if path.is_dir() {
            logging::log(
                Level::Debug,
                format!("replacing the stale worktree at {}", path.display()),
            );
            self.drop_worktree(&store, &path, cancel)?;
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                WorkspaceError::Failed(format!("could not create {}: {error}", parent.display()))
            })?;
        }

        // Detached at the fetched head: no branch is created or moved.
        let path_arg = path.to_string_lossy().into_owned();
        self.run_checked_in(
            &store,
            &["worktree", "add", "--detach", &path_arg, &head_sha],
            cancel,
        )?;

        logging::log(
            Level::Info,
            format!(
                "materialised {} pull request #{} at {} ({head_sha})",
                request.repo.slug(),
                request.number,
                path.display()
            ),
        );

        Ok(Workspace {
            path,
            base_sha,
            head_sha,
            reused: false,
        })
    }

    fn ensure_store(
        &self,
        store: &Path,
        repo: &RepoId,
        cancel: &Cancel,
    ) -> Result<(), WorkspaceError> {
        if store.is_dir() {
            return Ok(());
        }
        let parent = store.parent().ok_or_else(|| {
            WorkspaceError::Failed(format!(
                "could not determine parent for {}",
                store.display()
            ))
        })?;
        std::fs::create_dir_all(parent).map_err(|error| {
            WorkspaceError::Failed(format!("could not create {}: {error}", parent.display()))
        })?;
        let store_arg = store.to_string_lossy().into_owned();
        self.run_checked(&["init", "--bare", &store_arg], cancel)?;
        let metadata = parent.join("identity.json");
        let bytes = serde_json::to_vec(repo).map_err(|error| {
            WorkspaceError::Failed(format!("could not encode repository identity: {error}"))
        })?;
        std::fs::write(&metadata, bytes).map_err(|error| {
            WorkspaceError::Failed(format!("could not write {}: {error}", metadata.display()))
        })
    }

    fn remote_url(&self, remote: &str, cancel: &Cancel) -> Result<String, WorkspaceError> {
        let source = self.cwd.as_deref().ok_or_else(|| {
            WorkspaceError::Fetch(format!(
                "the URL for remote {remote} was unavailable; reopen the pull request from a detected clone"
            ))
        })?;
        Ok(self
            .run_checked_in(source, &["remote", "get-url", remote], cancel)?
            .trim()
            .to_owned())
    }

    /// `git merge-base`, as an option because unrelated histories are a state.
    fn merge_base(
        &self,
        dir: &Path,
        base: &str,
        head: &str,
        cancel: &Cancel,
    ) -> Result<Option<String>, WorkspaceError> {
        let (ok, stdout, _) = self.run_in(dir, &["merge-base", base, head], cancel)?;
        Ok(ok
            .then(|| stdout.trim().to_owned())
            .filter(|sha| !sha.is_empty()))
    }

    fn run_checked_in(
        &self,
        dir: &Path,
        args: &[&str],
        cancel: &Cancel,
    ) -> Result<String, WorkspaceError> {
        let spec = CommandSpec::new(&self.program)
            .args(args)
            .current_dir(dir)
            .timeout(SLOW_TIMEOUT);
        let (ok, stdout, stderr) = self.run_in_spec(&spec, cancel)?;
        if ok {
            Ok(stdout)
        } else {
            Err(Self::failure(args, &stderr))
        }
    }

    fn rev_parse_in(
        &self,
        dir: &Path,
        rev: &str,
        cancel: &Cancel,
    ) -> Result<String, WorkspaceError> {
        Ok(self
            .run_checked_in(dir, &["rev-parse", rev], cancel)?
            .trim()
            .to_owned())
    }

    /// Diffs a materialised pull request (FR-3.2).
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when git fails.
    pub(crate) fn diff_workspace(
        &self,
        request: &DiffRequest,
        cancel: &Cancel,
    ) -> Result<String, WorkspaceError> {
        let arguments = diff_arguments(&request.base_sha, &request.head_sha, request.options);
        let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
        self.run_in(&request.path, &borrowed, cancel)
            .and_then(|(ok, stdout, stderr)| {
                if ok {
                    Ok(stdout)
                } else {
                    Err(WorkspaceError::Failed(stderr.trim().to_owned()))
                }
            })
    }

    /// Removes a worktree from the app-owned object store (FR-3.1).
    pub(crate) fn drop_worktree(
        &self,
        store: &Path,
        path: &Path,
        cancel: &Cancel,
    ) -> Result<(), WorkspaceError> {
        if path.is_dir() {
            let path_arg = path.to_string_lossy().into_owned();
            // Destructive, so `--dry-run` records it instead of removing anything
            // (FR-6.5): the worktree is the user's only copy of nothing, but saying
            // what would be deleted is the point of a dry run.
            let spec = CommandSpec::new(&self.program)
                .args(["worktree", "remove", "--force", &path_arg])
                .current_dir(store)
                .mutating();
            let (_, _, stderr) = self.run_in_spec(&spec, cancel)?;
            if path.exists() {
                // `worktree remove` refuses a directory with local modifications or
                // an untracked file it would lose; the message says what to delete,
                // because that is the only way forward (FR-3.1).
                return Err(WorkspaceError::Failed(format!(
                    "git could not remove {}: {}{}",
                    path.display(),
                    stderr.trim(),
                    if stderr.trim().is_empty() {
                        "delete the directory by hand and run `git worktree prune`"
                    } else {
                        " — delete the directory by hand and run `git worktree prune`"
                    }
                )));
            }
        }
        let spec = CommandSpec::new(&self.program)
            .args(["worktree", "prune"])
            .current_dir(store)
            .mutating();
        let _ = self.run_in_spec(&spec, cancel);
        Ok(())
    }

    /// Deletes the fetched refs for a pull request, if they exist.
    ///
    /// Best effort by design: the worktree is already gone at this point, and a ref
    /// that could not be deleted is untidy rather than wrong.
    pub(crate) fn delete_head_ref(
        &self,
        store: &Path,
        repo: &RepoId,
        number: u64,
        cancel: &Cancel,
    ) {
        // Deleting a ref is destructive, so a dry run records it (FR-6.5).
        let spec = CommandSpec::new(&self.program)
            .args(["update-ref", "-d", &Self::head_ref(repo, number)])
            .current_dir(store)
            .mutating();
        let _ = self.run_in_spec(&spec, cancel);
        let spec = CommandSpec::new(&self.program)
            .args(["update-ref", "-d", &Self::base_ref(repo, number)])
            .current_dir(store)
            .mutating();
        let _ = self.run_in_spec(&spec, cancel);
    }

    /// Every managed worktree, newest names last (FR-3.1).
    pub(crate) fn list_workspaces(&self) -> Result<Vec<WorkspaceEntry>, WorkspaceError> {
        let root = self.worktrees_root()?.join("checkouts");
        let mut entries = Vec::new();
        let repos = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            // A root that does not exist yet means nothing has been materialised,
            // which is an empty answer rather than an error.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
            Err(error) => {
                return Err(WorkspaceError::Failed(format!(
                    "could not read {}: {error}",
                    root.display()
                )));
            }
        };
        for repo_dir in repos.flatten() {
            let metadata = self
                .worktrees_root()?
                .join("git")
                .join(repo_dir.file_name())
                .join("identity.json");
            let Ok(bytes) = std::fs::read(&metadata) else {
                continue;
            };
            let Ok(repo) = serde_json::from_slice::<RepoId>(&bytes) else {
                continue;
            };
            let Ok(inner) = std::fs::read_dir(repo_dir.path()) else {
                continue;
            };
            for entry in inner.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(number) = parse_pr_dir(&name) else {
                    continue;
                };
                let path = entry.path();
                entries.push(WorkspaceEntry {
                    repo: repo.clone(),
                    number,
                    age_secs: age_secs(&path),
                    path,
                });
            }
        }
        entries.sort_by(|a, b| {
            a.repo
                .key()
                .cmp(&b.repo.key())
                .then(a.number.cmp(&b.number))
        });
        Ok(entries)
    }
}

/// The `git diff` arguments for a set of options (FR-3.2).
///
/// Pure and separate so the flags the review screen claims to apply can be asserted
/// without running git: a toggle that lies is worse than one that is missing.
#[must_use]
pub(crate) fn diff_arguments(base: &str, head: &str, options: DiffOptions) -> Vec<String> {
    let mut args = vec![
        "diff".to_owned(),
        "--no-color".to_owned(),
        "--no-ext-diff".to_owned(),
        format!("--unified={}", options.context),
    ];
    if options.find_renames {
        args.push("--find-renames".to_owned());
    }
    if options.ignore_whitespace {
        args.push("-w".to_owned());
    }
    // Three-dot: what the pull request changes relative to the merge base, which is
    // what GitHub shows, and not the same as diffing the branch tips.
    args.push(format!("{base}...{head}"));
    args
}

/// `pr-7` → `7`.
#[must_use]
pub(crate) fn parse_pr_dir(name: &str) -> Option<u64> {
    name.strip_prefix("pr-")?.parse().ok()
}

/// How long ago a worktree was last used, from its modification time.
fn age_secs(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let age = std::time::SystemTime::now().duration_since(modified).ok()?;
    Some(age.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::workspace::WorkspacePort;
    use crate::test_support::{GitFixture, temp_home};

    /// The repository a fixture build uses.
    fn repo_id() -> RepoId {
        RepoId::new("github.com", "acme", "service")
    }

    /// A git adapter pointed at a fixture's clone, creating worktrees under `root`.
    fn adapter(fixture: &GitFixture, root: &Path) -> GitCli {
        GitCli::new()
            .with_program("git")
            .in_dir(&fixture.clone)
            .with_worktrees(root)
    }

    /// A fixture with one commit on `main` and one pull request.
    ///
    /// Not optional: see [`GitFixture::new`] for why a fixture that can be missing
    /// turns every test that uses it into one that cannot fail.
    fn fixture() -> (GitFixture, String, String) {
        let fixture = GitFixture::new();
        let base = fixture.commit("src/lib.rs", "pub fn one() {}\n", "add one");
        fixture.push_main();
        let head = fixture.publish_pull_request(7, |f| {
            f.commit(
                "src/lib.rs",
                "pub fn one() {}\npub fn two() {}\n",
                "add two",
            )
        });
        (fixture, base, head)
    }

    /// A request for the fixture's pull request.
    ///
    /// `base` is the *branch name*, because that is what FR-3.1 fetches
    /// (`baseRefName`); the merge base is git's answer to be worked out, not
    /// something the caller supplies.
    fn request(fixture: &GitFixture, head: &str) -> WorkspaceRequest {
        WorkspaceRequest {
            repo: repo_id(),
            remote: "origin".to_owned(),
            remote_url: Some(fixture.origin.to_string_lossy().into_owned()),
            number: 7,
            base: "main".to_owned(),
            head_sha: head.to_owned(),
        }
    }

    #[test]
    fn a_pull_request_is_materialised_without_touching_the_checkout() {
        let (fixture, base, head) = fixture();
        let root = fixture.path().join("worktrees");
        let git = adapter(&fixture, &root);
        let before_head = fixture.head_sha();
        let before_refs = fixture.git(&["show-ref"]);

        let workspace = git
            .ensure(&request(&fixture, &head), &Cancel::new())
            .expect("the workspace is created");

        assert_eq!(
            workspace.path,
            root.join("checkouts")
                .join(repo_id().storage_key())
                .join("pr-7")
        );
        assert_eq!(workspace.head_sha, head, "detached at the fetched head");
        assert_eq!(
            workspace.base_sha, base,
            "the base commit is the merge base of main and the head"
        );
        assert!(!workspace.reused);
        assert_eq!(
            GitFixture::git_in(&workspace.path, &["rev-parse", "HEAD"]).trim(),
            head
        );
        // The pull request's code, not the clone's.
        let content = std::fs::read_to_string(workspace.path.join("src/lib.rs")).expect("the file");
        assert!(content.contains("pub fn two()"), "{content}");

        // FR-3.1: the user's working tree, index, HEAD and branches are untouched.
        assert_eq!(fixture.head_sha(), before_head, "HEAD did not move");
        assert_eq!(fixture.status(), "", "the working tree is still clean");
        assert_eq!(fixture.git(&["show-ref"]), before_refs, "no refs moved");
        assert!(
            !fixture.clone.join(".git/worktrees").exists(),
            "worktree bookkeeping belongs to the app-owned object store"
        );
        let branches = fixture.git(&["branch", "--format=%(refname:short)"]);
        assert_eq!(branches.trim(), "main", "no branch was created: {branches}");
    }

    #[test]
    fn a_workspace_holding_the_same_head_is_reused() {
        let (fixture, _base, head) = fixture();
        let root = fixture.path().join("worktrees");
        let git = adapter(&fixture, &root);
        let cancel = Cancel::new();

        let first = git
            .ensure(&request(&fixture, &head), &cancel)
            .expect("created");
        assert!(!first.reused);
        let second = git
            .ensure(&request(&fixture, &head), &cancel)
            .expect("reused");
        assert!(second.reused, "the same head SHA must not be fetched again");
        assert_eq!(second.path, first.path);
        assert_eq!(second.head_sha, head);
    }

    #[test]
    fn a_new_commit_on_the_pull_request_replaces_the_workspace() {
        let (fixture, _base, head) = fixture();
        let root = fixture.path().join("worktrees");
        let git = adapter(&fixture, &root);
        let cancel = Cancel::new();
        let first = git
            .ensure(&request(&fixture, &head), &cancel)
            .expect("created");

        let newer = fixture.publish_pull_request(7, |f| {
            f.commit("src/lib.rs", "pub fn three() {}\n", "add three")
        });
        assert_ne!(newer, head);

        let second = git
            .ensure(&request(&fixture, &newer), &cancel)
            .expect("replaced");
        assert!(!second.reused, "a moved head is not a reuse");
        assert_eq!(second.head_sha, newer, "the newest head is what is on disk");
        assert_eq!(second.path, first.path, "and it lives in the same place");
        let content = std::fs::read_to_string(second.path.join("src/lib.rs")).expect("the file");
        assert!(content.contains("pub fn three()"), "{content}");
    }

    #[test]
    fn the_stale_workspace_is_removed_before_the_new_one_is_added() {
        let (fixture, _base, head) = fixture();
        let root = fixture.path().join("worktrees");
        let git = adapter(&fixture, &root);
        let cancel = Cancel::new();
        let workspace = git
            .ensure(&request(&fixture, &head), &cancel)
            .expect("created");

        // Put an untracked file in the worktree: `git worktree add` would refuse a
        // directory it cannot claim, and the requirement says the user never has to
        // clean up by hand for the app to keep working.
        std::fs::write(workspace.path.join("scratch.txt"), "junk").expect("writes");
        let newer = fixture.publish_pull_request(7, |f| {
            f.commit("src/lib.rs", "pub fn four() {}\n", "add four")
        });
        let replaced = git
            .ensure(&request(&fixture, &newer), &cancel)
            .expect("replaces the stale worktree");
        assert_eq!(replaced.head_sha, newer);
        assert!(
            !replaced.path.join("scratch.txt").exists(),
            "the old checkout is gone, not merged into the new one"
        );
    }

    #[test]
    fn the_local_diff_is_the_pull_requests_change() {
        let (fixture, _base, head) = fixture();
        let root = fixture.path().join("worktrees");
        let git = adapter(&fixture, &root);
        let cancel = Cancel::new();
        let workspace = git
            .ensure(&request(&fixture, &head), &cancel)
            .expect("created");

        let diff = git
            .diff(
                &DiffRequest {
                    path: workspace.path.clone(),
                    base_sha: workspace.base_sha.clone(),
                    head_sha: workspace.head_sha.clone(),
                    options: DiffOptions::default(),
                },
                &cancel,
            )
            .expect("the diff is produced");

        assert!(
            diff.contains("diff --git a/src/lib.rs b/src/lib.rs"),
            "{diff}"
        );
        assert!(diff.contains("+pub fn two()"), "{diff}");
        assert!(diff.contains("@@"), "context lines are present: {diff}");
    }

    #[test]
    fn the_context_and_whitespace_toggles_change_what_git_returns() {
        let (fixture, _base, head) = fixture();
        let root = fixture.path().join("worktrees");
        let git = adapter(&fixture, &root);
        let cancel = Cancel::new();
        let workspace = git
            .ensure(&request(&fixture, &head), &cancel)
            .expect("created");
        let diff_with = |options: DiffOptions| {
            git.diff(
                &DiffRequest {
                    path: workspace.path.clone(),
                    base_sha: workspace.base_sha.clone(),
                    head_sha: workspace.head_sha.clone(),
                    options,
                },
                &cancel,
            )
            .expect("a diff")
        };

        let wide = diff_with(DiffOptions {
            context: 10,
            ..DiffOptions::default()
        });
        let narrow = diff_with(DiffOptions {
            context: 0,
            ..DiffOptions::default()
        });
        assert!(
            wide.lines().count() > narrow.lines().count(),
            "context 10 must show more than context 0"
        );

        // A whitespace-only change plus a real one: `-w` must drop the first hunk
        // and keep the second. Asserting on the *hunks* rather than on the text,
        // because a context line would still print the reindented line verbatim.
        let noisy = fixture.publish_pull_request(7, |f| {
            f.commit(
                "src/lib.rs",
                "pub  fn one() {}\npub fn two() {}\npub fn three() {}\n",
                "reindent one and add three",
            )
        });
        let workspace = git
            .ensure(&request(&fixture, &noisy), &cancel)
            .expect("created");
        let no_context = |ignore_whitespace| DiffOptions {
            context: 0,
            ignore_whitespace,
            ..DiffOptions::default()
        };
        let diff_at = |options| {
            git.diff(
                &DiffRequest {
                    path: workspace.path.clone(),
                    base_sha: workspace.base_sha.clone(),
                    head_sha: workspace.head_sha.clone(),
                    options,
                },
                &cancel,
            )
            .expect("a diff")
        };

        let shown = diff_at(no_context(false));
        let hidden = diff_at(no_context(true));
        // The marker is the `+` line itself: the surrounding hunk header also names
        // the function, so a bare substring test would match the header.
        assert!(
            shown.contains("+pub  fn one() {}"),
            "the whitespace change is shown without -w: {shown}"
        );
        assert!(
            !hidden.contains("+pub  fn one() {}"),
            "and is gone with -w: {hidden}"
        );
        assert!(
            hidden.contains("+pub fn three() {}"),
            "the real change survives -w: {hidden}"
        );
        assert!(
            hidden.lines().count() < shown.lines().count(),
            "-w must produce a smaller diff"
        );
    }

    #[test]
    fn a_file_is_read_at_the_pull_requests_revision() {
        let (fixture, _base, head) = fixture();
        let root = fixture.path().join("worktrees");
        let git = adapter(&fixture, &root);
        let cancel = Cancel::new();
        let workspace = git
            .ensure(&request(&fixture, &head), &cancel)
            .expect("created");

        // The fixture published the head where GitHub would.
        let refs = GitFixture::git_in(&fixture.origin, &["for-each-ref", "--format=%(refname)"]);
        assert!(refs.contains("refs/pull/7/head"), "{refs}");
        assert!(
            workspace.path.join("src/lib.rs").exists(),
            "the worktree holds the file"
        );

        // Read from the clone, not the worktree: the answer comes from the object
        // database, so it does not depend on where it is asked (FR-4.6).
        let bytes = git
            .read_file(&fixture.clone, &head, "src/lib.rs", &cancel)
            .expect("the file exists at the head");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("pub fn two()"), "{text}");

        let missing = git
            .read_file(&fixture.clone, &head, "src/gone.rs", &cancel)
            .expect_err("a file that is not there");
        assert!(
            matches!(missing, WorkspaceError::NotFound { .. }),
            "{missing:?}"
        );
    }

    #[test]
    fn binary_files_survive_the_round_trip_through_git_show() {
        let (fixture, _base, _) = fixture();
        // A PNG header: invalid UTF-8, which a lossy decode would mangle.
        let bytes: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0xff, 0xfe];
        let head = fixture.publish_pull_request(9, |f| {
            std::fs::write(f.clone.join("logo.png"), &bytes).expect("writes");
            f.git(&["add", "--", "logo.png"]);
            f.git(&["commit", "--quiet", "-m", "add a logo"]);
            f.git(&["rev-parse", "HEAD"]).trim().to_owned()
        });
        let root = fixture.path().join("worktrees");
        let git = adapter(&fixture, &root);
        let cancel = Cancel::new();
        let mut request = request(&fixture, &head);
        request.number = 9;
        git.ensure(&request, &cancel).expect("created");

        let read = git
            .read_file(&fixture.clone, &head, "logo.png", &cancel)
            .expect("the binary file is readable");
        assert_eq!(read, bytes, "bytes in, bytes out");
    }

    #[test]
    fn tracked_files_at_a_revision_are_listed() {
        let (fixture, _base, head) = fixture();
        let root = fixture.path().join("worktrees");
        let git = adapter(&fixture, &root);
        let cancel = Cancel::new();
        git.ensure(&request(&fixture, &head), &cancel)
            .expect("created");

        let files = git
            .list_files(&fixture.clone, &head, &cancel)
            .expect("the tree is listed");
        assert_eq!(files, vec!["src/lib.rs".to_owned()]);
    }

    #[test]
    fn fr_4_6_ignore_rules_include_tracked_files_and_respect_nested_negation() {
        let fixture = GitFixture::new();
        fixture.commit(
            ".gitignore",
            "tracked.log\nsecrets/**\n!secrets/public.txt\n",
            "add ignore rules",
        );
        std::fs::create_dir_all(fixture.clone.join("secrets")).expect("creates nested directory");
        std::fs::write(fixture.clone.join("tracked.log"), "tracked but ignored")
            .expect("writes ignored file");
        std::fs::write(fixture.clone.join("secrets/private.txt"), "private")
            .expect("writes private file");
        std::fs::write(fixture.clone.join("secrets/public.txt"), "public")
            .expect("writes negated file");
        fixture.git(&["add", "-f", "--", "tracked.log", "secrets/private.txt"]);
        fixture.git(&["add", "--", "secrets/public.txt"]);
        fixture.git(&["commit", "--quiet", "-m", "track fixture files"]);
        let git = adapter(&fixture, &fixture.path().join("worktrees"));
        let candidates = vec![
            "tracked.log".to_owned(),
            "secrets/private.txt".to_owned(),
            "secrets/public.txt".to_owned(),
            "src/lib.rs".to_owned(),
        ];

        let ignored = git
            .ignored_paths(
                &fixture.clone,
                &fixture.head_sha(),
                &candidates,
                &Cancel::new(),
            )
            .expect("ignore rules are evaluated");

        assert!(ignored.contains(&"tracked.log".to_owned()), "{ignored:?}");
        assert!(
            ignored.contains(&"secrets/private.txt".to_owned()),
            "{ignored:?}"
        );
        assert!(
            !ignored.contains(&"secrets/public.txt".to_owned()),
            "{ignored:?}"
        );
        assert!(!ignored.contains(&"src/lib.rs".to_owned()), "{ignored:?}");
    }

    #[test]
    fn fr_4_6_ignore_rules_are_evaluated_at_the_represented_revision() {
        let fixture = GitFixture::new();
        fixture.commit(".gitignore", "tracked.log\n", "ignore the tracked fixture");
        std::fs::write(
            fixture.clone.join("tracked.log"),
            "BASE_ONLY_IGNORE_SENTINEL",
        )
        .expect("writes tracked fixture");
        fixture.git(&["add", "-f", "--", "tracked.log"]);
        fixture.git(&["commit", "--quiet", "-m", "track ignored fixture"]);
        let base = fixture.head_sha();
        fixture.git(&["rm", "--quiet", ".gitignore"]);
        fixture.git(&["commit", "--quiet", "-m", "remove ignore rule"]);
        let head = fixture.head_sha();
        let git = adapter(&fixture, &fixture.path().join("worktrees"));
        let candidates = vec!["tracked.log".to_owned()];

        let ignored_at_base = git
            .ignored_paths(&fixture.clone, &base, &candidates, &Cancel::new())
            .expect("base ignore rules are evaluated");
        let ignored_at_head = git
            .ignored_paths(&fixture.clone, &head, &candidates, &Cancel::new())
            .expect("head ignore rules are evaluated");

        assert_eq!(ignored_at_base, candidates);
        assert!(ignored_at_head.is_empty(), "{ignored_at_head:?}");
    }

    #[test]
    fn managed_worktrees_are_listed_and_removed() {
        let (fixture, _base, head) = fixture();
        let root = fixture.path().join("worktrees");
        let git = adapter(&fixture, &root);
        let cancel = Cancel::new();
        let workspace = git
            .ensure(&request(&fixture, &head), &cancel)
            .expect("created");

        let entries = git.list().expect("worktrees are listed");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, workspace.path);
        assert_eq!(entries[0].number, 7);
        assert_eq!(entries[0].repo.dir_name(), "acme-service");
        assert!(entries[0].age_secs.is_some());

        git.remove(&repo_id(), 7, &cancel).expect("removed");
        assert!(!workspace.path.exists(), "the directory is gone");
        assert!(git.list().expect("listed again").is_empty());

        // The user's checkout is still intact. Its Git directory never knew about
        // this worktree in the first place.
        assert!(fixture.clone.join("src/lib.rs").exists());
        assert_eq!(fixture.status(), "");
        assert!(
            !fixture.clone.join(".git/worktrees").exists(),
            "the source clone has no app worktree bookkeeping"
        );
    }

    #[test]
    fn ir_13_reuse_resolves_the_merge_base_from_the_app_store_not_stale_source_refs() {
        let (fixture, base, head) = fixture();
        let root = fixture.path().join("worktrees");
        let git = adapter(&fixture, &root);
        let source_refs = fixture.git(&["show-ref"]);
        let cancel = Cancel::new();

        let fresh = git
            .ensure(&request(&fixture, &head), &cancel)
            .expect("fresh workspace");
        assert_eq!(fresh.base_sha, base);

        // Advance the remote base from another clone. The source clone is deliberately
        // not fetched, so an implementation using `origin/main` there would be stale.
        let writer = fixture.path().join("writer");
        let origin = fixture.origin.to_string_lossy().into_owned();
        let writer_arg = writer.to_string_lossy().into_owned();
        let _ = GitFixture::git_in(fixture.path(), &["clone", "--quiet", &origin, &writer_arg]);
        GitFixture::git_in(&writer, &["config", "user.email", "t@example.com"]);
        GitFixture::git_in(&writer, &["config", "user.name", "Test"]);
        std::fs::write(writer.join("base-only.txt"), "new base").expect("writes base");
        GitFixture::git_in(&writer, &["add", "--", "base-only.txt"]);
        GitFixture::git_in(&writer, &["commit", "--quiet", "-m", "advance main"]);
        GitFixture::git_in(&writer, &["push", "--quiet", "origin", "main"]);

        let reused = git
            .ensure(&request(&fixture, &head), &cancel)
            .expect("reused workspace");
        assert!(reused.reused);
        assert_eq!(reused.base_sha, fresh.base_sha);
        assert_eq!(
            fixture.git(&["show-ref"]),
            source_refs,
            "source refs are unchanged"
        );
    }

    #[test]
    fn ir_13_legacy_worktrees_are_not_removed_through_the_source_clone() {
        let (fixture, _base, _head) = fixture();
        let root = fixture.path().join("worktrees");
        let legacy = root.join(repo_id().dir_name()).join("pr-7");
        std::fs::create_dir_all(&legacy).expect("creates legacy path");
        let git = adapter(&fixture, &root);

        let error = git
            .remove(&repo_id(), 7, &Cancel::new())
            .expect_err("legacy is refused");
        assert!(
            matches!(error, WorkspaceError::LegacyWorkspace { .. }),
            "{error:?}"
        );
        assert!(
            legacy.exists(),
            "legacy data is left for explicit owner cleanup"
        );
    }

    #[test]
    fn a_worktree_root_that_was_never_used_lists_nothing() {
        let root = temp_home();
        let git = GitCli::new().with_worktrees(root.path().join("worktrees"));
        assert!(git.list().expect("an empty list").is_empty());
    }

    #[test]
    fn without_a_worktree_root_the_adapter_says_so_rather_than_inventing_a_path() {
        let git = GitCli::new();
        let error = git
            .ensure(&request(&GitFixture::new(), "deadbeef"), &Cancel::new())
            .expect_err("no root is configured");
        assert!(matches!(error, WorkspaceError::Failed(_)), "{error:?}");
        assert!(error.to_string().contains("worktree directory"), "{error}");
    }

    #[test]
    fn the_diff_arguments_match_what_the_toggles_promise() {
        let defaults = diff_arguments("base", "head", DiffOptions::default());
        assert_eq!(
            defaults,
            [
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--unified=3",
                "--find-renames",
                "base...head",
            ]
        );

        let ignored = diff_arguments(
            "base",
            "head",
            DiffOptions {
                context: 10,
                ignore_whitespace: true,
                find_renames: false,
            },
        );
        assert_eq!(
            ignored,
            [
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--unified=10",
                "-w",
                "base...head",
            ]
        );
    }

    #[test]
    fn a_diff_is_three_dot_so_the_merge_base_is_used() {
        let args = diff_arguments("a", "b", DiffOptions::default());
        assert_eq!(args.last().unwrap(), "a...b");
    }

    #[test]
    fn worktree_directory_names_are_not_used_as_repository_identity() {
        assert_eq!(parse_pr_dir("pr-7"), Some(7));
        assert_eq!(parse_pr_dir("pr-"), None);
        assert_eq!(parse_pr_dir("7"), None);
        assert_eq!(parse_pr_dir("pr-abc"), None);

        let a = RepoId::new("github.com", "a-b", "c");
        let b = RepoId::new("github.com", "a", "b-c");
        assert_ne!(a.storage_key(), b.storage_key());
    }

    #[test]
    fn the_head_ref_is_namespaced_and_per_pull_request() {
        let repo = RepoId::new("github.com", "acme", "service");
        let seven = GitCli::head_ref(&repo, 7);
        assert_eq!(
            seven,
            "refs/smart-review/h-6769746875622e636f6d--o-61636d65--r-73657276696365/pr-7/head"
        );
        assert_ne!(seven, GitCli::head_ref(&repo, 8));
    }
}
