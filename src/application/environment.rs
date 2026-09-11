//! Environment detection (FR-1.1).
//!
//! This is the first thing the app does, and the four ways it can fail each need
//! their own sentence and their own next step. The order matters: the check that
//! comes first is the one the user has to fix first, so a missing remote is never
//! reported before "this is not a git repository".
//!
//! Nothing here draws anything or spawns anything itself: it asks the ports, and
//! the loop runs it on a background thread so the first frame does not wait for
//! `gh auth status` to reach GitHub (NFR-1.1, FR-1.1).

use crate::domain::environment::{Environment, EnvironmentError, GhInstall, MINIMUM_GH, RunMode};
use crate::domain::repo::{RepoId, RepoIdError};
use crate::ports::Cancel;
use crate::ports::forge::{ForgeProbe, ForgeStatus};
use crate::ports::workspace::{RepoInfo, WorkspacePort};

/// What the command line and the configuration asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectRequest {
    /// `--repo OWNER/NAME` or `SMART_REVIEW_REPO`.
    pub repo: Option<String>,
    /// The configured `gh` program, when it is not just `gh`.
    pub gh_program: Option<String>,
}

impl DetectRequest {
    /// A request with a repository override.
    #[must_use]
    pub fn with_repo(repo: impl Into<String>) -> Self {
        Self {
            repo: Some(repo.into()),
            gh_program: None,
        }
    }
}

/// Resolves where the app is running and whether it can work.
///
/// # Errors
///
/// Returns the specific [`EnvironmentError`] for the first thing that is wrong,
/// in the order the user has to fix them.
pub fn detect(
    workspace: &dyn WorkspacePort,
    probe: &dyn ForgeProbe,
    request: &DetectRequest,
    cancel: &Cancel,
) -> Result<Environment, EnvironmentError> {
    let info = workspace.detect().map_err(|error| match error {
        crate::ports::workspace::WorkspaceError::GitUnavailable(detail) => {
            EnvironmentError::GhMissing {
                tried: std::path::PathBuf::from("git"),
                advice: detail,
            }
        }
        other @ crate::ports::workspace::WorkspaceError::Failed(_) => {
            EnvironmentError::Failed(other.to_string())
        }
    })?;

    let (repo, remote, mode) = resolve_repository(&info, request)?;

    // Only now is `gh` worth probing: a missing remote would send the user down the
    // wrong path if `gh` were checked first.
    let status = probe.probe(cancel).map_err(EnvironmentError::Failed)?;
    let gh = match status {
        ForgeStatus::Missing => {
            return Err(EnvironmentError::GhMissing {
                tried: request
                    .gh_program
                    .as_deref()
                    .map_or_else(|| std::path::PathBuf::from("gh"), std::path::PathBuf::from),
                advice:
                    "install the GitHub CLI from https://cli.github.com, then run `gh auth login`"
                        .to_owned(),
            });
        }
        ForgeStatus::Unauthenticated { install, detail } => {
            return Err(EnvironmentError::GhUnauthenticated {
                detail: format!(
                    "{detail} (gh {} at {})",
                    install.version,
                    install.path.display()
                ),
                advice: "run `gh auth login` (add `--scopes repo` if the token is read-only)"
                    .to_owned(),
            });
        }
        ForgeStatus::Ready(install) => install,
    };

    if !gh.is_supported() {
        let (major, minor) = gh.version_parts();
        return Err(EnvironmentError::GhOutdated {
            found: gh.version.clone(),
            required_major: MINIMUM_GH.0,
            required_minor: MINIMUM_GH.1,
            advice: format!(
                "upgrade gh: this build needs {} or newer, found {major}.{minor}",
                format_args!("{}.{}", MINIMUM_GH.0, MINIMUM_GH.1)
            ),
        });
    }

    Ok(Environment {
        repo,
        mode,
        remote,
        root: info.root.clone(),
        default_branch: info.default_branch.clone(),
        git_version: info.git_version.clone(),
        gh,
    })
}

/// Works out which repository to use, from the override or the remotes.
fn resolve_repository(
    info: &RepoInfo,
    request: &DetectRequest,
) -> Result<(RepoId, Option<String>, RunMode), EnvironmentError> {
    if let Some(slug) = &request.repo {
        let repo = RepoId::parse(slug).map_err(|error| match error {
            RepoIdError::Malformed(value) | RepoIdError::MissingParts(value) => {
                EnvironmentError::NotARepository {
                    advice: format!(
                        "'{value}' is not OWNER/NAME; pass --repo OWNER/NAME (e.g. --repo acme/service)"
                    ),
                }
            }
        })?;

        // Running outside a clone is allowed: the workspace-dependent features are
        // switched off and the UI says so (FR-1.1). Inside a clone of the same
        // repository, the working tree is usable.
        let matching_remote = info
            .remotes
            .iter()
            .find(|remote| RepoId::from_remote_url(&remote.url).as_ref() == Some(&repo));
        let mode = if info.root.is_some() && matching_remote.is_some() {
            RunMode::InRepo
        } else {
            RunMode::RemoteOnly
        };
        return Ok((
            repo,
            matching_remote.map(|remote| remote.name.clone()),
            mode,
        ));
    }

    if info.root.is_none() {
        return Err(EnvironmentError::NotARepository {
            advice: "cd into a clone, or pass `--repo OWNER/NAME` to work without one".to_owned(),
        });
    }

    let github_remotes: Vec<(&String, RepoId)> = info
        .remotes
        .iter()
        .filter_map(|remote| {
            RepoId::from_remote_url(&remote.url)
                .filter(RepoId::is_github)
                .map(|repo| (&remote.name, repo))
        })
        .collect();

    let Some((name, repo)) = github_remotes
        .iter()
        .find(|(name, _)| name.as_str() == "origin")
        .or_else(|| github_remotes.first())
    else {
        let remotes: Vec<String> = info
            .remotes
            .iter()
            .map(|remote| format!("{} ({})", remote.name, remote.url))
            .collect();
        return Err(EnvironmentError::NoGitHubRemote {
            path: info
                .root
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
            remotes,
            advice: if info.remotes.is_empty() {
                "add one with `git remote add origin git@github.com:OWNER/NAME.git`".to_owned()
            } else {
                "pass --remote NAME to choose one, or --repo OWNER/NAME".to_owned()
            },
        });
    };

    Ok((repo.clone(), Some((*name).clone()), RunMode::InRepo))
}

/// The `gh` program to use: the configured path, or `gh` from `PATH`.
#[must_use]
pub fn gh_program(request: &DetectRequest) -> std::path::PathBuf {
    request
        .gh_program
        .as_deref()
        .map_or_else(|| std::path::PathBuf::from("gh"), std::path::PathBuf::from)
}

/// Whether an installation satisfies the minimum version, for `:doctor`.
#[must_use]
pub fn version_is_supported(install: &GhInstall) -> bool {
    install.is_supported()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::environment::EnvironmentError;
    use crate::ports::workspace::{Remote, RepoInfo};

    /// A workspace that reports whatever a test needs.
    #[derive(Debug)]
    struct FakeWorkspace(RepoInfo);

    impl WorkspacePort for FakeWorkspace {
        fn detect(&self) -> Result<RepoInfo, crate::ports::workspace::WorkspaceError> {
            Ok(self.0.clone())
        }
    }

    /// A probe with a fixed answer.
    #[derive(Debug)]
    struct FakeProbe(ForgeStatus);

    impl ForgeProbe for FakeProbe {
        fn probe(&self, _cancel: &Cancel) -> Result<ForgeStatus, String> {
            Ok(self.0.clone())
        }
    }

    fn install(version: &str, account: Option<&str>) -> GhInstall {
        GhInstall {
            path: std::path::PathBuf::from("/usr/bin/gh"),
            version: version.to_owned(),
            account: account.map(str::to_owned),
            scopes: vec!["repo".to_owned()],
        }
    }

    fn ready(version: &str) -> FakeProbe {
        FakeProbe(ForgeStatus::Ready(install(version, Some("bruno"))))
    }

    fn github_remote(name: &str, url: &str) -> Remote {
        Remote {
            name: name.to_owned(),
            url: url.to_owned(),
        }
    }

    fn repo_with(remotes: Vec<Remote>) -> FakeWorkspace {
        FakeWorkspace(RepoInfo {
            root: Some(std::path::PathBuf::from("/src/service")),
            remotes,
            default_branch: Some("main".to_owned()),
            git_version: "2.43.0".to_owned(),
        })
    }

    fn detect_with(
        workspace: &FakeWorkspace,
        probe: &FakeProbe,
        request: &DetectRequest,
    ) -> Result<Environment, EnvironmentError> {
        detect(workspace, probe, request, &Cancel::new())
    }

    #[test]
    fn origin_is_used_when_it_is_github() {
        let workspace = repo_with(vec![
            github_remote("upstream", "git@github.com:other/fork.git"),
            github_remote("origin", "git@github.com:acme/service.git"),
        ]);
        let environment =
            detect_with(&workspace, &ready("2.45.0"), &DetectRequest::default()).unwrap();
        assert_eq!(environment.repo.slug(), "acme/service");
        assert_eq!(environment.remote.as_deref(), Some("origin"));
        assert_eq!(environment.mode, RunMode::InRepo);
        assert_eq!(environment.default_branch.as_deref(), Some("main"));
        assert_eq!(environment.gh.account.as_deref(), Some("bruno"));
        assert!(!environment.is_degraded());
        assert_eq!(environment.status_label(), "repository as bruno");
    }

    #[test]
    fn the_first_github_remote_is_used_when_origin_is_not_github() {
        let workspace = repo_with(vec![
            github_remote("origin", "git@gitlab.com:acme/service.git"),
            github_remote("upstream", "git@github.com:acme/service.git"),
        ]);
        let environment =
            detect_with(&workspace, &ready("2.45.0"), &DetectRequest::default()).unwrap();
        assert_eq!(environment.repo.slug(), "acme/service");
        assert_eq!(environment.remote.as_deref(), Some("upstream"));
    }

    #[test]
    fn no_github_remote_lists_the_ones_that_exist_and_says_what_to_do() {
        let workspace = repo_with(vec![github_remote(
            "origin",
            "git@gitlab.com:acme/service.git",
        )]);
        let error =
            detect_with(&workspace, &ready("2.45.0"), &DetectRequest::default()).unwrap_err();
        match &error {
            EnvironmentError::NoGitHubRemote {
                remotes, advice, ..
            } => {
                assert_eq!(remotes.len(), 1);
                assert!(remotes[0].contains("gitlab.com"), "{remotes:?}");
                assert!(advice.contains("--remote"), "{advice}");
            }
            other => panic!("expected no-github-remote, got {other:?}"),
        }
        assert!(error.is_recoverable());
    }

    #[test]
    fn not_being_in_a_repository_is_the_first_thing_reported() {
        let workspace = FakeWorkspace(RepoInfo {
            git_version: "2.43.0".to_owned(),
            ..RepoInfo::default()
        });
        // The probe is ready, so a wrong order would report something else.
        let error =
            detect_with(&workspace, &ready("2.45.0"), &DetectRequest::default()).unwrap_err();
        assert!(
            matches!(error, EnvironmentError::NotARepository { .. }),
            "{error:?}"
        );
        assert!(error.advice().contains("--repo"), "{error}");
    }

    #[test]
    fn a_repo_override_works_outside_a_clone_and_says_it_is_degraded() {
        let workspace = FakeWorkspace(RepoInfo::default());
        let request = DetectRequest::with_repo("acme/service");
        let environment = detect_with(&workspace, &ready("2.45.0"), &request).unwrap();

        assert_eq!(environment.repo.slug(), "acme/service");
        assert_eq!(environment.mode, RunMode::RemoteOnly);
        assert!(environment.remote.is_none());
        assert!(environment.root.is_none());
        assert!(environment.is_degraded());
        assert_eq!(environment.status_label(), "remote only as bruno");
    }

    #[test]
    fn a_repo_override_inside_the_matching_clone_keeps_the_workspace() {
        let workspace = repo_with(vec![github_remote(
            "origin",
            "git@github.com:acme/service.git",
        )]);
        let request = DetectRequest::with_repo("acme/service");
        let environment = detect_with(&workspace, &ready("2.45.0"), &request).unwrap();
        assert_eq!(environment.mode, RunMode::InRepo);
        assert_eq!(environment.remote.as_deref(), Some("origin"));
    }

    #[test]
    fn a_repo_override_for_another_repository_is_remote_only() {
        let workspace = repo_with(vec![github_remote(
            "origin",
            "git@github.com:acme/other.git",
        )]);
        let request = DetectRequest::with_repo("acme/service");
        let environment = detect_with(&workspace, &ready("2.45.0"), &request).unwrap();
        assert_eq!(environment.repo.slug(), "acme/service");
        assert_eq!(
            environment.mode,
            RunMode::RemoteOnly,
            "the clone on disk is a different repository, so it cannot be used"
        );
        assert!(environment.remote.is_none());
    }

    #[test]
    fn a_nonsense_repo_override_is_refused_with_a_shape_to_copy() {
        let workspace = FakeWorkspace(RepoInfo::default());
        let error = detect_with(
            &workspace,
            &ready("2.45.0"),
            &DetectRequest::with_repo("justaname"),
        )
        .unwrap_err();
        assert!(error.advice().contains("OWNER/NAME"), "{error}");
    }

    #[test]
    fn a_missing_gh_is_reported_with_where_it_was_looked_for() {
        let workspace = repo_with(vec![github_remote(
            "origin",
            "git@github.com:acme/service.git",
        )]);
        let error = detect_with(
            &workspace,
            &FakeProbe(ForgeStatus::Missing),
            &DetectRequest::default(),
        )
        .unwrap_err();
        match &error {
            EnvironmentError::GhMissing { tried, advice } => {
                assert_eq!(tried.to_string_lossy(), "gh");
                assert!(advice.contains("cli.github.com"), "{advice}");
            }
            other => panic!("expected gh-missing, got {other:?}"),
        }
        assert!(!error.is_recoverable());
    }

    #[test]
    fn a_configured_gh_path_is_named_in_the_missing_message() {
        let workspace = repo_with(vec![github_remote(
            "origin",
            "git@github.com:acme/service.git",
        )]);
        let request = DetectRequest {
            repo: None,
            gh_program: Some("/opt/gh/bin/gh".to_owned()),
        };
        let error =
            detect_with(&workspace, &FakeProbe(ForgeStatus::Missing), &request).unwrap_err();
        assert!(error.to_string().contains("/opt/gh/bin/gh"), "{error}");
        assert_eq!(gh_program(&request).to_string_lossy(), "/opt/gh/bin/gh");
    }

    #[test]
    fn a_logged_out_gh_says_to_log_in_and_keeps_what_gh_said() {
        let workspace = repo_with(vec![github_remote(
            "origin",
            "git@github.com:acme/service.git",
        )]);
        let probe = FakeProbe(ForgeStatus::Unauthenticated {
            install: install("2.45.0", None),
            detail: "You are not logged into any GitHub hosts.".to_owned(),
        });
        let error = detect_with(&workspace, &probe, &DetectRequest::default()).unwrap_err();
        match &error {
            EnvironmentError::GhUnauthenticated { detail, advice } => {
                assert!(detail.contains("not logged into"), "{detail}");
                assert!(detail.contains("2.45.0"), "the version is still reported");
                assert!(advice.contains("gh auth login"), "{advice}");
            }
            other => panic!("expected unauthenticated, got {other:?}"),
        }
    }

    #[test]
    fn an_old_gh_is_refused_with_both_versions() {
        let workspace = repo_with(vec![github_remote(
            "origin",
            "git@github.com:acme/service.git",
        )]);
        let error =
            detect_with(&workspace, &ready("2.39.9"), &DetectRequest::default()).unwrap_err();
        match &error {
            EnvironmentError::GhOutdated {
                found,
                required_major,
                required_minor,
                ..
            } => {
                assert_eq!(found, "2.39.9");
                assert_eq!((*required_major, *required_minor), MINIMUM_GH);
            }
            other => panic!("expected outdated, got {other:?}"),
        }
    }

    #[test]
    fn the_minimum_version_is_accepted() {
        let workspace = repo_with(vec![github_remote(
            "origin",
            "git@github.com:acme/service.git",
        )]);
        let environment =
            detect_with(&workspace, &ready("2.40.0"), &DetectRequest::default()).unwrap();
        assert!(version_is_supported(&environment.gh));
        assert!(!environment.is_degraded());
    }

    #[test]
    fn a_read_only_token_is_noticed_but_does_not_stop_startup() {
        // Scopes are copied, not enforced: the first real call will fail with
        // GitHub's own message if the token is too narrow, which is better than
        // refusing to start on a guess.
        let workspace = repo_with(vec![github_remote(
            "origin",
            "git@github.com:acme/service.git",
        )]);
        let mut narrow = install("2.45.0", Some("bruno"));
        narrow.scopes = vec!["gist".to_owned()];
        let environment = detect_with(
            &workspace,
            &FakeProbe(ForgeStatus::Ready(narrow)),
            &DetectRequest::default(),
        )
        .unwrap();
        assert!(!environment.gh.has_repo_scope());
        assert!(environment.is_degraded(), "the status line can say so");
    }
}
