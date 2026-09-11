//! Probing the `gh` installation (FR-1.1, FR-9.3).
//!
//! This runs *before* a repository is known — that is the whole point, since the
//! repository may be resolved from `--repo` and the user still has to be told that
//! `gh` is missing or logged out. So it cannot be a method on the repository-scoped
//! forge.
//!
//! Two details are worth stating because they are not obvious:
//!
//! - `gh auth status` writes to **stderr**, including when it succeeds;
//! - it exits non-zero when not logged in, so a failure here is a finding to
//!   report, not an error to propagate.

use std::path::PathBuf;

use crate::adapters::process::{CommandSpec, ProcessRunner};
use crate::domain::environment::GhInstall;
use crate::ports::Cancel;
use crate::ports::forge::{ForgeProbe, ForgeStatus};

/// How long a probe may take. `gh auth status` talks to GitHub.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Probes the `gh` CLI.
#[derive(Debug, Clone)]
pub struct GhCliProbe {
    runner: ProcessRunner,
    program: PathBuf,
}

impl GhCliProbe {
    /// Binds the probe to a `gh` executable.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            runner: ProcessRunner::new().with_timeout(TIMEOUT),
            program: program.into(),
        }
    }

    /// Runs a probe command, returning its stdout and stderr together.
    ///
    /// Returns `None` when the command could not be run at all, which is how a
    /// missing `gh` is distinguished from a `gh` that answered "not logged in".
    fn run(&self, args: &[&str], cancel: &Cancel) -> Option<(bool, String)> {
        let spec = CommandSpec::new(&self.program).args(args);
        let output = self.runner.run(&spec, cancel).ok()?;
        Some((
            output.success(),
            format!("{}{}", output.stdout, output.stderr),
        ))
    }
}

impl ForgeProbe for GhCliProbe {
    fn probe(&self, cancel: &Cancel) -> Result<ForgeStatus, String> {
        let Some((success, version_text)) = self.run(&["--version"], cancel) else {
            return Ok(ForgeStatus::Missing);
        };
        if !success {
            return Ok(ForgeStatus::Missing);
        }
        let version = parse_version(&version_text);

        let (authenticated, auth_text) = self
            .run(&["auth", "status"], cancel)
            .unwrap_or((false, String::new()));
        let (account, scopes) = parse_auth_status(&auth_text);

        let install = GhInstall {
            path: self.program.clone(),
            version,
            account: account.clone(),
            scopes,
        };

        if !authenticated {
            return Ok(ForgeStatus::Unauthenticated {
                install,
                detail: first_meaningful_line(&auth_text),
            });
        }

        // Authenticated, but the login could not be read: the account is unknown
        // rather than absent, and that is not a reason to refuse to start.
        Ok(ForgeStatus::Ready(install))
    }
}

/// The most useful line of `gh auth status` output, for an error message.
fn first_meaningful_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("gh reported no detail")
        .to_owned()
}

/// `gh version 2.45.0 (2025-07-18 Ubuntu 2.45.0-1ubuntu0.3)` → `2.45.0`.
#[must_use]
pub fn parse_version(output: &str) -> String {
    output
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .strip_prefix("gh version ")
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Pulls the account and the token scopes out of `gh auth status`.
///
/// The output differs between gh versions and between hosts, so both are best
/// effort: an account that cannot be found is reported as unknown rather than
/// guessed, and scopes that are not printed are reported as "not reported".
#[must_use]
pub fn parse_auth_status(output: &str) -> (Option<String>, Vec<String>) {
    let mut account = None;
    let mut scopes: Vec<String> = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim().trim_start_matches(['✓', '*', '-', ' ']).trim();
        if account.is_none() && trimmed.starts_with("Logged in to ") {
            // Both spellings appear: "Logged in to github.com account alice (...)"
            // and "Logged in to github.com as alice (...)".
            let rest = trimmed.trim_start_matches("Logged in to ");
            let after_host = rest
                .split_once(" account ")
                .map(|(_, tail)| tail)
                .or_else(|| rest.split_once(" as ").map(|(_, tail)| tail));
            if let Some(tail) = after_host {
                let login: String = tail
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '(')
                    .collect();
                if !login.is_empty() {
                    account = Some(login);
                }
            }
        }

        if let Some(rest) = trimmed.strip_prefix("Token scopes:") {
            scopes = rest
                .split(',')
                .map(|scope| scope.trim().trim_matches(['\'', '"']).to_owned())
                .filter(|scope| !scope.is_empty())
                .collect();
        }
    }

    (account, scopes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_line_is_reduced_to_its_number() {
        assert_eq!(
            parse_version("gh version 2.45.0 (2025-07-18 Ubuntu 2.45.0-1ubuntu0.3)\nhttps://...\n"),
            "2.45.0"
        );
        assert_eq!(parse_version(""), "");
    }

    #[test]
    fn the_account_is_read_from_either_spelling() {
        let modern = "github.com\n  ✓ Logged in to github.com account alice (keyring)\n  - Active account: true\n";
        assert_eq!(parse_auth_status(modern).0.as_deref(), Some("alice"));

        let older =
            "github.com\n  ✓ Logged in to github.com as bruno (/home/x/.config/gh/hosts.yml)\n";
        assert_eq!(parse_auth_status(older).0.as_deref(), Some("bruno"));
    }

    #[test]
    fn scopes_are_read_when_gh_prints_them() {
        let output = "  ✓ Logged in to github.com account alice (keyring)\n  - Token scopes: 'gist', 'read:org', 'repo', 'workflow'\n";
        let (account, scopes) = parse_auth_status(output);
        assert_eq!(account.as_deref(), Some("alice"));
        assert_eq!(scopes, vec!["gist", "read:org", "repo", "workflow"]);
    }

    #[test]
    fn a_logged_out_status_yields_no_account_and_no_crash() {
        let output =
            "You are not logged into any GitHub hosts. Run gh auth login to authenticate.\n";
        let (account, scopes) = parse_auth_status(output);
        assert!(account.is_none());
        assert!(scopes.is_empty());
    }

    #[test]
    fn nonsense_output_is_survived() {
        let (account, scopes) = parse_auth_status("\u{1f600}\nLogged in to \n");
        assert!(account.is_none());
        assert!(scopes.is_empty());
    }

    #[test]
    fn a_missing_gh_is_reported_as_missing_rather_than_an_error() {
        let probe = GhCliProbe::new("/nonexistent/gh");
        assert_eq!(probe.probe(&Cancel::new()).unwrap(), ForgeStatus::Missing);
    }

    #[test]
    fn a_logged_out_gh_is_reported_with_what_it_said() {
        // The fake answers both probes: a version, then a failed auth status.
        let dir = crate::test_support::temp_home();
        let program = dir.path().join("gh");
        let script = "#!/bin/sh\ncase \"$1\" in\n  --version) echo \"gh version 2.45.0 (2025-07-18)\" ;;\n  auth) echo \"You are not logged into any GitHub hosts. Run gh auth login to authenticate.\" >&2; exit 1 ;;\nesac\nexit 0\n";
        dir.write("gh", script);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755));
        }

        let probe = GhCliProbe::new(&program);
        match probe.probe(&Cancel::new()).unwrap() {
            ForgeStatus::Unauthenticated { install, detail } => {
                assert_eq!(install.version, "2.45.0");
                assert!(install.account.is_none());
                assert!(detail.contains("not logged"), "{detail}");
            }
            other => panic!("expected unauthenticated, got {other:?}"),
        }
    }
}
