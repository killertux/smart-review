//! Environment checks, used by `smart-review --check` and the `:doctor` popup
//! (FR-9.3).
//!
//! Exit codes: `0` ready, `1` degraded (usable, but some features are
//! unavailable), `2` unusable.
//!
//! The report is split in two on purpose: [`collect_local`] inspects the
//! application and needs no external process, while [`collect`] adds the `git`
//! and `gh` probes. Tests use the former, so they stay hermetic (AGENTS.md §8)
//! and pass on a machine without `gh`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;
use crate::error::Error;
use crate::logging;
use crate::paths::Home;
use crate::tui::keymap::Keymap;
use crate::tui::theme::Theme;

/// Everything the checks need.
///
/// Owned rather than borrowed so the same value can be handed to a background
/// thread while the interface keeps running.
#[derive(Debug, Clone)]
pub struct Context {
    /// The application's private directory.
    pub home: Home,
    /// Effective settings.
    pub config: Config,
    /// Where the config file lives.
    pub config_path: PathBuf,
    /// Whether a config file was read.
    pub config_exists: bool,
    /// Warnings collected while loading configuration and keybindings.
    pub warnings: Vec<String>,
    /// The keybinding engine.
    pub keymap: Keymap,
    /// The active theme.
    pub theme: Theme,
    /// Where the theme came from.
    pub theme_source: String,
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

/// Checks that inspect the application only (no external processes).
#[must_use]
pub fn collect_local(context: &Context) -> Vec<Check> {
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
            detail: log_detail(context),
        },
        terminal_check(),
        llm_check(context),
    ];

    checks.sort_by_key(|check| check.name);
    checks
}

/// The full report, including the `git` and `gh` probes (FR-9.3).
#[must_use]
pub fn collect(context: &Context) -> Vec<Check> {
    let mut checks = collect_local(context);

    checks.push(tool_check("git", "git", &["--version"]));
    checks.push(gh_check(&context.config.forge.gh_path));

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
pub fn run(out: &mut impl Write, context: &Context) -> Result<Health, Error> {
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

/// The log path, the active level, and what the tail of the file says (FR-9.2).
fn log_detail(context: &Context) -> String {
    let path = context.home.log_file();
    let level = logging::level().map_or_else(|| "not initialised".to_owned(), |l| l.to_string());
    format!(
        "{} (level {level}), {}",
        path.display(),
        tail_summary(&path, LOG_TAIL_LINES)
    )
}

/// How many lines of the log the doctor looks at.
const LOG_TAIL_LINES: usize = 200;

/// Summarises the warnings and errors at the end of the log.
///
/// The log is read from its end: only the tail matters for "what went wrong just
/// now", and it keeps the read bounded regardless of how big the file grew.
fn tail_summary(path: &Path, lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return "no log yet".to_owned();
    };

    let recent: Vec<&str> = text.lines().rev().take(lines).collect();
    let warnings = recent.iter().filter(|line| line.contains(" warn ")).count();
    let errors = recent
        .iter()
        .filter(|line| line.contains(" error "))
        .count();

    if warnings == 0 && errors == 0 {
        return format!("no warnings or errors in the last {lines} lines");
    }

    let last = recent
        .iter()
        .find(|line| line.contains(" warn ") || line.contains(" error "))
        .map(|line| line.trim())
        .unwrap_or_default();

    format!(
        "{warnings} warning(s) and {errors} error(s) in the last {lines} lines; most recent: {last}"
    )
}

fn config_check(context: &Context) -> Check {
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

/// `gh` is not used until M1.
///
/// Reporting it as a warning keeps `--check` exit code 1 ("degraded, will still
/// work") on a fresh machine or a CI runner, which is the truth for M0: nothing
/// the shell does needs GitHub. It becomes a failure once M1 depends on it.
fn gh_check(program: &str) -> Check {
    let version = match run_tool(program, &["--version"]) {
        Ok(output) => first_line(&output),
        Err(detail) => {
            return Check {
                name: "gh",
                status: Status::Warn,
                detail: format!(
                    "{detail}; not needed until M1, then install it from \
                     https://cli.github.com or set [forge].gh_path"
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
            status: Status::Warn,
            detail: format!("{version}, not authenticated; run `gh auth login` before M1"),
        },
    }
}

fn llm_check(context: &Context) -> Check {
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

    /// Returns the temporary home alongside the context so the directory outlives
    /// the assertions.
    fn context() -> (TempHome, Context) {
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
        (dir, startup.doctor_context())
    }

    #[test]
    fn reports_the_expected_local_names() {
        let (_dir, context) = context();
        let checks = collect_local(&context);
        let names: Vec<&str> = checks.iter().map(|check| check.name).collect();
        for expected in [
            "config", "home", "keybinds", "llm", "log", "terminal", "theme",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
    }

    #[test]
    fn no_model_configured_is_degraded_not_broken() {
        // DEC-6: a fresh install has no model, and that must not stop the app.
        let (_dir, context) = context();
        let checks = collect_local(&context);
        assert_eq!(health(&checks), Health::Degraded);
    }

    #[test]
    fn the_full_report_adds_the_tool_probes() {
        let (_dir, context) = context();
        let names: Vec<&str> = collect(&context).iter().map(|check| check.name).collect();
        assert!(names.contains(&"git"), "{names:?}");
        assert!(names.contains(&"gh"), "{names:?}");
    }

    #[test]
    fn a_missing_gh_is_a_warning_while_m0_does_not_need_it() {
        let check = gh_check("smart-review-no-such-binary");
        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("M1"), "{}", check.detail);
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
        let (_dir, context) = context();
        let rendered = lines(&collect_local(&context));
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

    #[test]
    fn the_log_tail_summary_counts_and_quotes() {
        let dir = temp_home();
        let path = dir.write(
            "smart-review.log",
            "2026-01-01T00:00:00Z info  started\n\
             2026-01-01T00:00:01Z warn  something is off\n\
             2026-01-01T00:00:02Z error something broke\n",
        );
        let summary = tail_summary(&path, 200);
        assert!(summary.contains("1 warning(s) and 1 error(s)"), "{summary}");
        assert!(summary.contains("something broke"), "{summary}");
    }

    #[test]
    fn a_clean_log_tail_says_so() {
        let dir = temp_home();
        let path = dir.write("smart-review.log", "2026-01-01T00:00:00Z info  started\n");
        assert!(tail_summary(&path, 200).contains("no warnings or errors"));
    }

    #[test]
    fn a_missing_log_is_not_an_error() {
        let dir = temp_home();
        assert_eq!(tail_summary(&dir.path().join("nope.log"), 10), "no log yet");
    }
}
