//! The workspace port, as far as M1 needs it (ARCH-2).
//!
//! M1 only detects where it is running. M2 adds the parts that materialise a pull
//! request in a managed worktree (`ensure_workspace`, `diff`, `read_file`,
//! `list_files`), which is why this trait is deliberately small for now: an
//! abstraction that promises more than it does is worse than none.

use std::path::PathBuf;

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
}

/// Anything that can describe the checkout the app is running in.
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
    fn an_empty_detection_is_not_a_repository() {
        let info = RepoInfo::default();
        assert!(info.root.is_none());
        assert!(info.remotes.is_empty());
    }
}
