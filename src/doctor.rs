//! Environment checks, used by `smart-review --check` and the `:doctor` popup
//! (FR-9.3).
//!
//! Exit codes: `0` ready, `1` degraded (usable, but some features are
//! unavailable), `2` unusable.
//!
//! Nothing here writes to the terminal directly; output goes to a writer so it
//! can be captured in tests and rendered in a popup.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use crate::config::Config;
use crate::error::Error;
use crate::paths::Home;
use crate::tui::keymap::Keymap;
use crate::tui::theme::Theme;

/// The pieces of the running application the checks need.
///
/// Taking this instead of the whole [`crate::Startup`] lets the same code serve
/// both `--check` and the `:doctor` popup once the startup has been consumed.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The application's private directory.
    pub home: &'a Home,
    /// Effective settings.
    pub config: &'a Config,
    /// Where the config file lives.
    pub config_path: &'a Path,
    /// Whether a config file was read.
    pub config_exists: bool,
    /// Warnings collected while loading configuration and keybindings.
    pub warnings: &'a [String],
    /// The keybinding engine.
    pub keymap: &'a Keymap,
    /// The active theme.
    pub theme: &'a Theme,
    /// Where the theme came from.
    pub theme_source: &'a str,
}

/// Overall outcome of the checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Everything needed is present.
    Ready,
    /// The app runs, but something is missing or misconfigured.
    Degraded,
    /// The app cannot run.
    Unusable,
}

/// Outcome of a single check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// As expected.
    Ok,
    /// Usable but incomplete.
    Warn,
    /// Broken.
    Fail,
}

impl Status {
    /// Short marker used in text output.
    #[must_use]
    pub const fn marker(self) -> &'static str {
        match self {
            Self::Ok => "[ ok ]",
            Self::Warn => "[warn]",
            Self::Fail => "[fail]",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Short name, also used as the column key.
    pub name: &'static str,
    /// Outcome.
    pub status: Status,
    /// What was found, and what to do about it when something is wrong.
    pub detail: String,
}

/// Runs every check and returns the report.
#[must_use]
pub fn collect(context: &Context<'_>) -> Vec<Check> {
    let mut checks = vec![
        directory_check("home", context.home.root()),
        config_check(context),
        Check {
            name: "theme",
            status: Status::Ok,
            detail: format!("{} (from {})", context.theme.name(), context.theme_source),
        },
        Check {
            name: "keybinds",
            status: Status::Ok,
            detail: format!(
                "{} bindings, leader `{}`, timeout {} ms",
                context.keymap.bindings().len(),
                context.keymap.leader().describe(),
                context.keymap.timeout().as_millis()
            ),
        },
        Check {
            name: "log",
            status: Status::Ok,
            detail: context.home.log_file().display().to_string(),
        },
        terminal_check(),
        tool_check("git", "git", &["--version"]),
        gh_check(&context.config.forge.gh_path),
        llm_check(context),
    ];

    checks.sort_by_key(|check| check.name);
    checks
}

/// Folds the checks into one verdict.
#[must_use]
pub fn health(checks: &[Check]) -> Health {
    if checks.iter().any(|check| check.status == Status::Fail) {
        Health::Unusable
    } else if checks.iter().any(|check| check.status == Status::Warn) {
        Health::Degraded
    } else {
        Health::Ready
    }
}

/// Renders the report as aligned lines.
#[must_use]
pub fn lines(checks: &[Check]) -> Vec<String> {
    let width = checks
        .iter()
        .map(|check| check.name.len())
        .max()
        .unwrap_or(0);
    checks
        .iter()
        .map(|check| {
            format!(
                "{} {:<width$}  {}",
                check.status.marker(),
                check.name,
                check.detail,
                width = width
            )
        })
        .collect()
}

/// Prints the report and returns the verdict.
///
/// # Errors
///
/// Returns an error when the report cannot be written to `out`.
pub fn run(out: &mut impl Write, context: &Context<'_>) -> Result<Health, Error> {
    let checks = collect(context);
    for line in lines(&checks) {
        writeln!(out, "{line}").map_err(Error::Terminal)?;
    }
    Ok(health(&checks))
}

fn directory_check(name: &'static str, path: &Path) -> Check {
    if !path.is_dir() {
        return Check {
            name,
            status: Status::Fail,
            detail: format!("{} does not exist", path.display()),
        };
    }
    // Writability matters more than existence: a read-only home silently breaks
    // caching, state and logs.
    let probe = path.join(".write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Check {
                name,
                status: Status::Ok,
                detail: format!("{} (writable)", path.display()),
            }
        }
        Err(error) => Check {
            name,
            status: Status::Fail,
            detail: format!("{} is not writable: {error}", path.display()),
        },
    }
}

fn config_check(context: &Context<'_>) -> Check {
    let origin = if context.config_exists {
        context.config_path.display().to_string()
    } else {
        format!("{} (not created yet)", context.config_path.display())
    };

    if context.warnings.is_empty() {
        Check {
            name: "config",
            status: Status::Ok,
            detail: format!("{origin}, no warnings"),
        }
    } else {
        Check {
            name: "config",
            status: Status::Warn,
            detail: format!(
                "{origin}, {} warning(s): {}",
                context.warnings.len(),
                context.warnings.join(" | ")
            ),
        }
    }
}

fn terminal_check() -> Check {
    let term = std::env::var("TERM").unwrap_or_else(|_| "unset".to_owned());
    let colorterm = std::env::var("COLORTERM").unwrap_or_else(|_| "unset".to_owned());
    match crossterm::terminal::size() {
        Ok((width, height)) => {
            let (status, size) = if width >= 80 && height >= 24 {
                (Status::Ok, format!("{width}x{height}"))
            } else {
                (
                    Status::Warn,
                    format!("{width}x{height} (below the 80x24 minimum)"),
                )
            };
            Check {
                name: "terminal",
                status,
                detail: format!("TERM={term} COLORTERM={colorterm} size {size}"),
            }
        }
        Err(error) => Check {
            name: "terminal",
            status: Status::Warn,
            detail: format!("TERM={term} COLORTERM={colorterm}, size unknown: {error}"),
        },
    }
}

fn tool_check(name: &'static str, program: &str, args: &[&str]) -> Check {
    match run_tool(program, args) {
        Ok(output) => Check {
            name,
            status: Status::Ok,
            detail: first_line(&output),
        },
        Err(detail) => Check {
            name,
            status: Status::Fail,
            detail,
        },
    }
}

fn gh_check(program: &str) -> Check {
    let version = match run_tool(program, &["--version"]) {
        Ok(output) => first_line(&output),
        Err(detail) => {
            return Check {
                name: "gh",
                status: Status::Fail,
                detail: format!(
                    "{detail}; install the GitHub CLI (https://cli.github.com) or set \
                     [forge].gh_path"
                ),
            };
        }
    };

    match run_tool(program, &["auth", "status"]) {
        Ok(_) => Check {
            name: "gh",
            status: Status::Ok,
            detail: format!("{version} (authenticated)"),
        },
        Err(_) => Check {
            name: "gh",
            status: Status::Fail,
            detail: format!("{version}, not authenticated; run `gh auth login`"),
        },
    }
}

fn llm_check(context: &Context<'_>) -> Check {
    let Some(active) = context.config.llm.active.as_ref() else {
        return Check {
            name: "llm",
            status: Status::Warn,
            detail: "no model configured ([llm.active] is absent); the model picker arrives in M2"
                .to_owned(),
        };
    };
    Check {
        name: "llm",
        status: Status::Ok,
        detail: format!(
            "{}/{} (key presence is checked when the model picker lands in M2)",
            active.provider, active.model
        ),
    }
}

/// Runs an external command and returns its combined output.
fn run_tool(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("could not run `{program}`: {error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    if output.status.success() {
        Ok(combined.trim().to_owned())
    } else {
        let reason = first_line(&combined);
        Err(format!("`{program} {}` failed: {reason}", args.join(" ")))
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("(no output)")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::Startup;
    use crate::cli::Cli;
    use crate::test_support::{TempHome, temp_home};

    /// Returns the temporary home alongside the startup so the directory outlives
    /// the assertions.
    fn startup() -> (TempHome, Startup) {
        let dir = temp_home();
        let cli = Cli {
            repo: None,
            pr: None,
            path: None,
            remote: None,
            config: None,
            theme: None,
            home: Some(dir.path().to_path_buf()),
            log_level: None,
            check: false,
        };
        let startup = Startup::load(&cli).unwrap();
        (dir, startup)
    }

    #[test]
    fn reports_the_expected_names() {
        let (_dir, startup) = startup();
        let checks = collect(&startup.doctor_context());
        let names: Vec<&str> = checks.iter().map(|check| check.name).collect();
        for expected in [
            "config", "home", "keybinds", "llm", "log", "terminal", "theme",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
    }

    #[test]
    fn a_fresh_home_reports_degraded() {
        let (_dir, startup) = startup();
        let checks = collect(&startup.doctor_context());
        // No model configured is a warning, not a failure.
        assert_eq!(health(&checks), Health::Degraded);
    }

    #[test]
    fn a_failure_makes_it_unusable() {
        let checks = vec![
            Check {
                name: "home",
                status: Status::Ok,
                detail: String::new(),
            },
            Check {
                name: "gh",
                status: Status::Fail,
                detail: String::new(),
            },
        ];
        assert_eq!(health(&checks), Health::Unusable);
    }

    #[test]
    fn all_ok_is_ready() {
        let checks = vec![Check {
            name: "home",
            status: Status::Ok,
            detail: String::new(),
        }];
        assert_eq!(health(&checks), Health::Ready);
    }

    #[test]
    fn lines_are_aligned_and_use_markers() {
        let (_dir, startup) = startup();
        let rendered = lines(&collect(&startup.doctor_context()));
        assert!(!rendered.is_empty());
        assert!(
            rendered.iter().all(|line| line.starts_with('[')),
            "{rendered:?}"
        );
    }

    #[test]
    fn a_missing_directory_fails() {
        let check = directory_check("home", Path::new("/nonexistent/smart-review"));
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("does not exist"));
    }
}
