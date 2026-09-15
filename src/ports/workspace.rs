//! The workspace port (FR-3.1, FR-3.2, FR-4.6, ARCH-2).
//!
//! M1 only detected where it was running. M2a adds the rest: materialising a pull
//! request as a managed worktree, diffing it locally with the toggles the review
//! screen offers, and reading a file *at the pull request's revision* rather than
//! from whatever the worktree happens to contain (FR-4.6).
//!
//! Two decisions are visible in the types:
//!
//! - a worktree is identified by the head SHA it holds, so "is this the code we
//!   analysed?" is answerable without asking git (DEC-1, FR-4.3);
//! - a diff is a *request* value, not a set of flags threaded through call sites,
//!   because the toggles are part of what is cached and of the cache key (FR-3.2).

use std::path::PathBuf;

use crate::domain::repo::RepoId;
use crate::ports::Cancel;

/// A git remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// The remote's name, e.g. `origin`.
    pub name: String,
    /// Its URL, as configured.
    pub url: String,
}

/// What `git` can tell us about the current directory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepoInfo {
    /// The root of the working tree, absent when the directory is not inside one.
    pub root: Option<PathBuf>,
    /// Every remote, in git's order.
    pub remotes: Vec<Remote>,
    /// The repository's default branch, when git can name it.
    pub default_branch: Option<String>,
    /// The git version, for `:doctor`.
    pub git_version: String,
}

impl RepoInfo {
    /// The named remote, if it exists.
    #[must_use]
    pub fn remote(&self, name: &str) -> Option<&Remote> {
        self.remotes.iter().find(|remote| remote.name == name)
    }
}

/// Why the workspace could not be inspected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceError {
    #[error("git could not be run: {0}")]
    GitUnavailable(String),

    #[error("git failed: {0}")]
    Failed(String),

    #[error("{path} does not exist at {rev}")]
    NotFound {
        /// The path that was asked for.
        path: String,
        /// The revision it was asked for at.
        rev: String,
    },

    #[error("the pull request could not be fetched: {0}")]
    Fetch(String),

    #[error("git refused the revision for this pull request: {0}")]
    NoMergeBase(String),

    #[error("the work was cancelled")]
    Cancelled,
}

/// A pull request to materialise locally (FR-3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRequest {
    /// The repository the pull request belongs to.
    pub repo: RepoId,
    /// The remote to fetch from, usually `origin`.
    pub remote: String,
    /// The pull request number.
    pub number: u64,
    /// The base branch the pull request targets.
    pub base: String,
    /// The head commit the pull request is at, as the forge reported it.
    pub head_sha: String,
}

/// Where a pull request's code lives locally, and at which commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The worktree's directory.
    pub path: PathBuf,
    /// The merge base the diff is taken from.
    pub base_sha: String,
    /// The head commit the worktree holds.
    pub head_sha: String,
    /// Whether an existing worktree was reused rather than created.
    pub reused: bool,
}

/// How to produce a diff (FR-3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffOptions {
    /// Context lines around each change, `--unified`.
    pub context: u32,
    /// Ignore whitespace-only changes, `-w`.
    pub ignore_whitespace: bool,
    /// Detect renames, `--find-renames`.
    pub find_renames: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            context: 3,
            ignore_whitespace: false,
            find_renames: true,
        }
    }
}

/// What to diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRequest {
    /// The worktree to run in.
    pub path: PathBuf,
    /// The commit to diff from.
    pub base_sha: String,
    /// The commit to diff to.
    pub head_sha: String,
    /// The flags.
    pub options: DiffOptions,
}

/// A managed worktree as the app sees it, for `:workspace` and `:doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEntry {
    /// The repository the worktree belongs to.
    pub repo: RepoId,
    /// The pull request number.
    pub number: u64,
    /// The directory.
    pub path: PathBuf,
    /// How many seconds since it was last used, when known.
    pub age_secs: Option<u64>,
}

/// Anything that can describe and materialise the checkout the app reviews.
pub trait WorkspacePort: std::fmt::Debug + Send + Sync {
    /// Inspects the directory the app was started in.
    ///
    /// A directory that is not a repository is `Ok` with `root: None`, not an
    /// error: "not a repository" is a state to explain, and the caller decides
    /// whether `--repo` makes it workable (FR-1.1).
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when `git` cannot be run at all.
    fn detect(&self) -> Result<RepoInfo, WorkspaceError>;

    /// Materialises a pull request locally, reusing a worktree that already holds
    /// the same head commit (FR-3.1, DEC-1).
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the fetch, the worktree creation or the
    /// merge-base lookup fails.
    fn ensure(
        &self,
        request: &WorkspaceRequest,
        cancel: &Cancel,
    ) -> Result<Workspace, WorkspaceError>;

    /// Diffs a materialised pull request (FR-3.2).
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when git fails.
    fn diff(&self, request: &DiffRequest, cancel: &Cancel) -> Result<String, WorkspaceError>;

    /// Reads one file at a revision, independent of what the worktree holds
    /// (FR-4.6, Appendix A).
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError::NotFound`] when the path does not exist at that
    /// revision, which callers treat as "skip this file", not as a failure.
    fn read_file(
        &self,
        repo: &std::path::Path,
        rev: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Vec<u8>, WorkspaceError>;

    /// Lists every file tracked at a revision, for the file tree (FR-4.6).
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when git fails.
    fn list_files(
        &self,
        repo: &std::path::Path,
        rev: &str,
        cancel: &Cancel,
    ) -> Result<Vec<String>, WorkspaceError>;

    /// Returns the supplied paths matched by repository ignore rules at `rev`.
    ///
    /// This deliberately includes tracked paths: a committed credential remains
    /// excluded when `.gitignore` also names it (FR-4.6).
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when git cannot evaluate the repository rules.
    fn ignored_paths(
        &self,
        repo: &std::path::Path,
        rev: &str,
        paths: &[String],
        cancel: &Cancel,
    ) -> Result<Vec<String>, WorkspaceError>;

    /// Every worktree the app manages (FR-3.1).
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the directory cannot be read.
    fn list(&self) -> Result<Vec<WorkspaceEntry>, WorkspaceError>;

    /// Removes a managed worktree and prunes git's bookkeeping.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when git fails.
    fn remove(&self, repo: &RepoId, number: u64, cancel: &Cancel) -> Result<(), WorkspaceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_named_remote_is_found_among_several() {
        let info = RepoInfo {
            root: Some(PathBuf::from("/src/service")),
            remotes: vec![
                Remote {
                    name: "origin".to_owned(),
                    url: "git@github.com:acme/service.git".to_owned(),
                },
                Remote {
                    name: "upstream".to_owned(),
                    url: "git@github.com:acme/fork.git".to_owned(),
                },
            ],
            default_branch: Some("main".to_owned()),
            git_version: "2.43.0".to_owned(),
        };
        assert_eq!(
            info.remote("upstream").unwrap().url,
            "git@github.com:acme/fork.git"
        );
        assert!(info.remote("nope").is_none());
    }

    #[test]
    fn diff_options_default_to_what_a_reviewer_expects() {
        let options = DiffOptions::default();
        assert_eq!(options.context, 3, "three lines, as git and GitHub do");
        assert!(!options.ignore_whitespace);
        assert!(options.find_renames, "a rename is not a delete plus an add");
    }

    #[test]
    fn a_workspace_remembers_the_commits_it_holds() {
        let workspace = Workspace {
            path: PathBuf::from("/home/u/.smart-review/worktrees/acme-service/pr-7"),
            base_sha: "aaaa".to_owned(),
            head_sha: "bbbb".to_owned(),
            reused: true,
        };
        assert!(workspace.reused);
        assert_eq!(workspace.head_sha, "bbbb");
    }

    #[test]
    fn a_request_names_the_repository_it_belongs_to() {
        let request = WorkspaceRequest {
            repo: RepoId::new("github.com", "acme", "service"),
            remote: "origin".to_owned(),
            number: 7,
            base: "main".to_owned(),
            head_sha: "bbbb".to_owned(),
        };
        assert_eq!(request.repo.dir_name(), "acme-service");
    }

    #[test]
    fn a_missing_file_is_a_named_error_not_a_panic() {
        let error = WorkspaceError::NotFound {
            path: "src/gone.rs".to_owned(),
            rev: "abc123".to_owned(),
        };
        let message = error.to_string();
        assert!(message.contains("src/gone.rs"), "{message}");
        assert!(message.contains("abc123"), "{message}");
    }

    #[test]
    fn an_empty_detection_is_not_a_repository() {
        let info = RepoInfo::default();
        assert!(info.root.is_none());
        assert!(info.remotes.is_empty());
    }
}
