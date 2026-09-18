//! What the app knows about where it is running (FR-1.1, FR-9.3).
//!
//! Detection has four distinct ways to fail and each produces its own sentence
//! and its own next step, because "something went wrong" is not actionable:
//! not a git repository, a repository with no GitHub remote, no `gh` installed,
//! and `gh` installed but not authenticated.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::repo::RepoId;

/// The oldest `gh` this application supports (FR-1.1).
pub const MINIMUM_GH: (u32, u32) = (2, 40);

/// Whether the app has a local checkout to work with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// Running inside the repository: workspace, file context and analysis are
    /// all available.
    InRepo,
    /// `--repo OWNER/NAME` outside a clone: everything that needs the working
    /// tree is disabled, and the UI says so (FR-1.1).
    RemoteOnly,
}

impl RunMode {
    /// Whether the local working tree can be used.
    #[must_use]
    pub fn has_workspace(self) -> bool {
        matches!(self, Self::InRepo)
    }

    /// A label for the status line and `:doctor`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::InRepo => "repository",
            Self::RemoteOnly => "remote only",
        }
    }
}

/// The `gh` installation that was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhInstall {
    /// The program that was used, which may come from the configuration.
    pub path: PathBuf,
    /// The version, e.g. `2.45.0`.
    pub version: String,
    /// The account `gh auth status` reports, when it is authenticated.
    pub account: Option<String>,
    /// OAuth scopes the token carries, when gh reports them.
    pub scopes: Vec<String>,
}

impl GhInstall {
    /// The version as comparable numbers.
    #[must_use]
    pub fn version_parts(&self) -> (u32, u32) {
        parse_version(&self.version)
    }

    /// Whether the version is new enough for the field sets this app uses.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        self.version_parts() >= MINIMUM_GH
    }

    /// Whether the token can read and write repository data (FR-1.1).
    ///
    /// Unknown scopes are assumed to be sufficient rather than blocking the user:
    /// `gh auth status` does not always report them, and refusing to start because
    /// a string was missing would be worse than letting the first real call fail
    /// with GitHub's own message.
    #[must_use]
    pub fn has_repo_scope(&self) -> bool {
        self.scopes.is_empty() || self.scopes.iter().any(|scope| scope == "repo")
    }

    /// The scopes as one string.
    #[must_use]
    pub fn scope_label(&self) -> String {
        if self.scopes.is_empty() {
            "not reported".to_owned()
        } else {
            self.scopes.join(", ")
        }
    }
}

/// Parses `2.45.0` (or a longer suffix) into major and minor.
#[must_use]
pub fn parse_version(version: &str) -> (u32, u32) {
    let mut parts = version
        .trim()
        .trim_start_matches('v')
        .split(['.', '-'])
        .map(|part| part.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// Everything detection resolved, which is what the rest of the app runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    /// Which repository.
    pub repo: RepoId,
    /// How the repository was found.
    pub mode: RunMode,
    /// The git remote the repository was identified from, if any.
    pub remote: Option<String>,
    /// The selected remote's URL, captured during detection so workspace jobs do not
    /// reread mutable source-clone configuration (IR-13).
    pub remote_url: Option<String>,
    /// The root of the working tree, when there is one.
    pub root: Option<PathBuf>,
    /// The repository's default branch, when it could be determined.
    pub default_branch: Option<String>,
    /// The git version, for `:doctor`.
    pub git_version: String,
    /// The `gh` installation.
    pub gh: GhInstall,
}

impl Environment {
    /// A one-line summary for the status line.
    #[must_use]
    pub fn status_label(&self) -> String {
        let account = self
            .gh
            .account
            .as_deref()
            .map_or(String::new(), |account| format!(" as {account}"));
        format!("{}{account}", self.mode.label())
    }

    /// Whether the environment can do everything v1 promises.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        !self.mode.has_workspace() || !self.gh.is_supported() || !self.gh.has_repo_scope()
    }
}

/// Why detection could not produce an [`Environment`].
///
/// Each variant is one of the four failures FR-1.1 requires to be distinct and
/// actionable; the `advice` is the next step, already written as a command.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvironmentError {
    /// Not inside a git work tree.
    #[error("this directory is not a git repository")]
    NotARepository {
        /// The command that fixes it.
        advice: String,
    },

    /// Inside a repository, but no remote points at GitHub.
    #[error("{path} has no GitHub remote")]
    NoGitHubRemote {
        /// The directory that was inspected.
        path: PathBuf,
        /// The remotes that do exist, so the user can pick one.
        remotes: Vec<String>,
        /// The next step.
        advice: String,
    },

    /// `gh` is missing or not executable.
    #[error("the GitHub CLI was not found (looked for {tried})")]
    GhMissing {
        /// The path that was tried.
        tried: PathBuf,
        /// The next step.
        advice: String,
    },

    /// `gh` is installed but too old.
    #[error("gh {found} is too old; {required_major}.{required_minor} or newer is required")]
    GhOutdated {
        /// The version that was found.
        found: String,
        /// The minimum major version.
        required_major: u32,
        /// The minimum minor version.
        required_minor: u32,
        /// The next step.
        advice: String,
    },

    /// `gh` is installed but not logged in.
    #[error("gh is not authenticated")]
    GhUnauthenticated {
        /// What `gh` reported, so the user can tell "not logged in" from
        /// "logged in to the wrong host".
        detail: String,
        /// The next step.
        advice: String,
    },

    /// `git` is missing or not executable.
    #[error("git was not found or could not be run")]
    GitMissing {
        /// What git reported.
        detail: String,
        /// The next step.
        advice: String,
    },

    /// A command could not be run at all.
    #[error("{0}")]
    Failed(String),
}

impl EnvironmentError {
    /// The next step to show under the message, already phrased as a command.
    #[must_use]
    pub fn advice(&self) -> &str {
        match self {
            Self::NotARepository { advice }
            | Self::NoGitHubRemote { advice, .. }
            | Self::GhMissing { advice, .. }
            | Self::GhOutdated { advice, .. }
            | Self::GitMissing { advice, .. }
            | Self::GhUnauthenticated { advice, .. } => advice,
            Self::Failed(_) => "run `gh auth status` to see what gh reports",
        }
    }

    /// Whether the user can continue in a reduced mode.
    ///
    /// A missing workspace is recoverable (remote-only mode reads the diff from
    /// `gh`), but nothing works without an authenticated `gh`.
    #[must_use]
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::NotARepository { .. } | Self::NoGitHubRemote { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_of_every_shape_parse() {
        assert_eq!(parse_version("2.45.0"), (2, 45));
        assert_eq!(parse_version("2.40"), (2, 40));
        assert_eq!(parse_version("v2.45.0"), (2, 45));
        assert_eq!(parse_version("2.45.0 (2024-01-01)"), (2, 45));
        assert_eq!(parse_version("garbage"), (0, 0));
        assert_eq!(parse_version(""), (0, 0));
    }

    #[test]
    fn the_minimum_version_is_enforced_with_a_clear_message() {
        let install = |version: &str| GhInstall {
            path: PathBuf::from("/usr/bin/gh"),
            version: version.to_owned(),
            account: Some("bruno".to_owned()),
            scopes: vec!["repo".to_owned()],
        };
        assert!(
            install("2.40.0").is_supported(),
            "the minimum itself passes"
        );
        assert!(install("2.45.0").is_supported());
        assert!(!install("2.39.9").is_supported());
        assert!(!install("1.9.0").is_supported());

        let error = EnvironmentError::GhOutdated {
            found: "2.39.9".to_owned(),
            required_major: MINIMUM_GH.0,
            required_minor: MINIMUM_GH.1,
            advice: "upgrade gh".to_owned(),
        };
        assert!(error.to_string().contains("2.39.9"), "{error}");
        assert!(error.to_string().contains("2.40"), "{error}");
    }

    #[test]
    fn unknown_scopes_do_not_block_the_user() {
        let mut install = GhInstall {
            path: PathBuf::from("/usr/bin/gh"),
            version: "2.45.0".to_owned(),
            account: None,
            scopes: Vec::new(),
        };
        assert!(install.has_repo_scope());
        assert_eq!(install.scope_label(), "not reported");

        install.scopes = vec!["gist".to_owned()];
        assert!(!install.has_repo_scope());

        install.scopes = vec!["gist".to_owned(), "repo".to_owned()];
        assert!(install.has_repo_scope());
        assert_eq!(install.scope_label(), "gist, repo");
    }

    #[test]
    fn every_failure_has_advice_and_a_reason() {
        let failures = [
            EnvironmentError::NotARepository {
                advice: "cd into a clone, or pass --repo OWNER/NAME".to_owned(),
            },
            EnvironmentError::NoGitHubRemote {
                path: PathBuf::from("/tmp/x"),
                remotes: vec!["upstream".to_owned()],
                advice: "pass --remote upstream".to_owned(),
            },
            EnvironmentError::GhMissing {
                tried: PathBuf::from("gh"),
                advice: "install it from https://cli.github.com".to_owned(),
            },
            EnvironmentError::GhOutdated {
                found: "2.1.0".to_owned(),
                required_major: MINIMUM_GH.0,
                required_minor: MINIMUM_GH.1,
                advice: "run `brew upgrade gh`".to_owned(),
            },
            EnvironmentError::GhUnauthenticated {
                detail: "no oauth token".to_owned(),
                advice: "run `gh auth login`".to_owned(),
            },
        ];
        for failure in &failures {
            assert!(!failure.to_string().is_empty());
            assert!(!failure.advice().is_empty());
        }

        // The two that the user can work around are marked as such.
        assert!(failures[0].is_recoverable());
        assert!(failures[1].is_recoverable());
        assert!(!failures[2].is_recoverable());
        assert!(!failures[4].is_recoverable());
    }

    #[test]
    fn remote_only_mode_says_what_is_missing() {
        let environment = Environment {
            repo: RepoId::parse("acme/service").unwrap(),
            mode: RunMode::RemoteOnly,
            remote: None,
            remote_url: None,
            root: None,
            default_branch: None,
            git_version: "2.43.0".to_owned(),
            gh: GhInstall {
                path: PathBuf::from("/usr/bin/gh"),
                version: "2.45.0".to_owned(),
                account: Some("bruno".to_owned()),
                scopes: vec!["repo".to_owned()],
            },
        };
        assert!(environment.is_degraded());
        assert_eq!(environment.status_label(), "remote only as bruno");
        assert!(!environment.mode.has_workspace());
    }
}
