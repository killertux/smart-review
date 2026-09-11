//! The `git` adapter (ARCH-3).
//!
//! M1 uses it to answer "where am I running": the work tree root, the remotes, the
//! default branch and the git version. M2 adds the worktree operations that
//! materialise a pull request.
//!
//! Every call is read-only. Nothing here can change the user's working tree, index,
//! HEAD or branches (DEC-1).

use std::path::{Path, PathBuf};

use crate::adapters::process::{CommandSpec, ProcessError, ProcessRunner};
use crate::logging::{self, Level};
use crate::ports::workspace::{Remote, RepoInfo, WorkspaceError, WorkspacePort};

/// Runs `git` in a directory.
#[derive(Debug, Clone)]
pub struct GitCli {
    runner: ProcessRunner,
    program: PathBuf,
    cwd: Option<PathBuf>,
}

impl Default for GitCli {
    fn default() -> Self {
        Self::new()
    }
}

impl GitCli {
    /// Uses `git` from `PATH`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: ProcessRunner::new().with_timeout(std::time::Duration::from_secs(30)),
            program: PathBuf::from("git"),
            cwd: None,
        }
    }

    /// Uses a specific `git` and runs it in a specific directory.
    #[must_use]
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// Runs every command in this directory instead of the current one.
    #[must_use]
    pub fn in_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Builds a command, applying the configured directory.
    fn spec(&self, args: &[&str]) -> CommandSpec {
        let spec = CommandSpec::new(&self.program).args(args);
        match &self.cwd {
            Some(dir) => spec.current_dir(dir),
            None => spec,
        }
    }

    /// Runs a command, returning `None` when git exits non-zero.
    ///
    /// A non-zero exit from `git rev-parse` is how git says "not a repository", so
    /// it is a normal answer rather than a failure.
    fn try_run(&self, args: &[&str]) -> std::result::Result<Option<String>, WorkspaceError> {
        let spec = self.spec(args);
        match self.runner.run(&spec, &crate::ports::Cancel::new()) {
            Ok(output) if output.success() => Ok(Some(output.stdout)),
            Ok(_) => Ok(None),
            Err(ProcessError::NotFound { .. }) => Err(WorkspaceError::GitUnavailable(
                "install git and make sure it is on PATH".to_owned(),
            )),
            Err(error) => Err(WorkspaceError::Failed(error.to_string())),
        }
    }
}

impl WorkspacePort for GitCli {
    fn detect(&self) -> std::result::Result<RepoInfo, WorkspaceError> {
        // `--show-toplevel` also fails outside a work tree, which is the answer we
        // want rather than an error.
        let root = self
            .try_run(&["rev-parse", "--show-toplevel"])?
            .map(|text| PathBuf::from(text.trim()))
            .filter(|path| path.is_dir());

        let git_version = match self.try_run(&["--version"])? {
            Some(text) => parse_git_version(&text),
            None => {
                return Err(WorkspaceError::GitUnavailable(
                    "`git --version` failed; the installation looks broken".to_owned(),
                ));
            }
        };

        // Without a work tree there are no remotes to read, and asking outside one
        // prints an error rather than failing cleanly.
        let Some(root) = root else {
            return Ok(RepoInfo {
                root: None,
                remotes: Vec::new(),
                default_branch: None,
                git_version,
            });
        };

        let remotes = self
            .try_run(&["remote", "-v"])?
            .map(|text| parse_remotes(&text))
            .unwrap_or_default();

        let default_branch = self
            .try_run(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])?
            .map(|text| {
                text.trim()
                    .trim_start_matches("origin/")
                    .trim_start_matches("refs/remotes/origin/")
                    .to_owned()
            })
            .filter(|branch| !branch.is_empty());

        logging::log(
            Level::Debug,
            format!(
                "detected a work tree at {} with {} remote(s)",
                root.display(),
                remotes.len()
            ),
        );

        Ok(RepoInfo {
            root: Some(root),
            remotes,
            default_branch,
            git_version,
        })
    }
}

/// `git version 2.43.0` → `2.43.0`; anything else is passed through.
#[must_use]
pub fn parse_git_version(output: &str) -> String {
    output
        .trim()
        .strip_prefix("git version ")
        .unwrap_or(output.trim())
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Parses `git remote -v` output, keeping one entry per remote.
///
/// The output has a line per URL, and a remote with separate fetch and push URLs
/// appears twice; the first (fetch) wins, because that is the one PRs are read
/// from.
#[must_use]
pub fn parse_remotes(output: &str) -> Vec<Remote> {
    let mut remotes: Vec<Remote> = Vec::new();
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let (Some(name), Some(url)) = (fields.next(), fields.next()) else {
            continue;
        };
        if remotes.iter().any(|remote| remote.name == name) {
            continue;
        }
        remotes.push(Remote {
            name: name.to_owned(),
            url: url.to_owned(),
        });
    }
    remotes
}

/// Whether the path looks like a git work tree.
#[must_use]
pub fn is_repository(path: &Path) -> bool {
    path.join(".git").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::repo::RepoId;
    use crate::test_support::temp_home;

    #[test]
    fn a_version_line_loses_its_words() {
        assert_eq!(parse_git_version("git version 2.43.0\n"), "2.43.0");
        assert_eq!(
            parse_git_version("git version 2.43.0.windows.1"),
            "2.43.0.windows.1"
        );
        assert_eq!(parse_git_version("2.40.1"), "2.40.1");
        assert_eq!(parse_git_version(""), "");
    }

    #[test]
    fn remotes_are_parsed_with_their_fetch_urls() {
        let remotes = parse_remotes(
            "origin\tgit@github.com:acme/service.git (fetch)\norigin\tgit@github.com:acme/service.git (push)\nupstream\thttps://github.com/acme/upstream.git (fetch)\n",
        );
        assert_eq!(remotes.len(), 2, "a remote with one URL is one remote");
        assert_eq!(remotes[0].name, "origin");
        assert_eq!(remotes[0].url, "git@github.com:acme/service.git");
        assert_eq!(remotes[1].name, "upstream");
    }

    #[test]
    fn malformed_remote_lines_are_skipped() {
        assert!(parse_remotes("").is_empty());
        assert!(parse_remotes("justaname\n").is_empty());
        assert_eq!(parse_remotes("origin\turl\t(fetch)\n").len(), 1);
    }

    #[test]
    fn a_directory_that_is_not_a_repository_is_reported_as_such() {
        let dir = temp_home();
        let git = GitCli::new().in_dir(dir.path());
        let info = git.detect().unwrap();
        assert!(info.root.is_none(), "a temp directory is not a repository");
        assert!(info.remotes.is_empty());
        assert!(
            !info.git_version.is_empty(),
            "the version is still reported, because git itself works"
        );
    }

    #[test]
    fn a_missing_git_is_reported_as_unavailable() {
        let git = GitCli::new().with_program("/nonexistent/git");
        let error = git.detect().unwrap_err();
        assert!(
            matches!(error, WorkspaceError::GitUnavailable(_)),
            "{error:?}"
        );
        assert!(error.to_string().contains("PATH"), "{error}");
    }

    #[test]
    fn a_real_repository_is_detected_end_to_end() {
        // A repository built by the test, not the checkout it happens to be compiled
        // in: an assertion that can silently skip itself is not an assertion, and a
        // source tarball has no `.git` at all.
        let dir = temp_home();
        let root = dir.path().join("checkout");
        std::fs::create_dir_all(&root).unwrap();
        let ok = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .is_ok_and(|output| output.status.success())
        };
        if !ok(&["init", "--quiet"]) {
            // No git on this machine: the adapter's own "git is unavailable" path is
            // covered by another test.
            return;
        }
        let _ = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ])
            .current_dir(&root)
            .output();
        assert!(ok(&[
            "remote",
            "add",
            "origin",
            "git@github.com:acme/service.git"
        ]));

        let info = GitCli::new().in_dir(&root).detect().unwrap();
        assert_eq!(info.root.as_deref(), Some(root.as_path()));
        assert!(!info.git_version.is_empty());
        assert_eq!(info.remotes.len(), 1);
        assert_eq!(info.remotes[0].name, "origin");
        assert_eq!(info.remotes[0].url, "git@github.com:acme/service.git");
        assert!(
            RepoId::from_remote_url(&info.remotes[0].url).is_some_and(|repo| repo.is_github()),
            "the URL must be usable as a repository identity"
        );
    }
}
