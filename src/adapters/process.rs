//! Running external programs without a shell (ARCH-3, NFR-3.3, FR-9.1).
//!
//! Every `git` and `gh` call in this application goes through here, so the safety
//! rules are stated once and enforced by tests:
//!
//! - the program is passed as a path and the arguments as an argv array; nothing
//!   is ever interpolated into a shell string, so a PR title containing `; rm -rf`
//!   is just a title;
//! - stdout and stderr are captured separately, each with a size cap, and the
//!   stream keeps being drained after the cap so the child never blocks on a full
//!   pipe;
//! - every call has a timeout, and the child is killed when it expires;
//! - a call is cancellable, and the child is killed within one poll interval;
//! - a non-zero exit becomes an error carrying the exit code and a tail of
//!   stderr, which is what the user needs to see.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::ports::Cancel;

/// How long a child may run before it is killed.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// How much of each stream is kept. Enough for a large diff, small enough that a
/// runaway process cannot exhaust memory (NFR-1.3).
pub const DEFAULT_OUTPUT_CAP: usize = 32 * 1024 * 1024;

/// How many times a spawn is retried when the kernel says the file is busy.
///
/// "Text file busy" is not about our program being wrong: the kernel refuses to
/// `exec` a file that any process holds open for writing, which happens when another
/// thread in this process was mid-`fork` while the file was being written (a test
/// harness building a fake `gh`, for instance) or when a package manager is replacing
/// the binary. The condition clears as soon as that other process execs, so a retry
/// over a few milliseconds is the right answer rather than a hard failure.
const SPAWN_ATTEMPTS: u32 = 8;
/// How long to wait between spawn attempts.
const SPAWN_BACKOFF: Duration = Duration::from_millis(10);

/// How often a running child is checked for exit, timeout and cancellation.
///
/// This is the latency of `Esc` on a running command, so it is well inside the
/// 200 ms budget of NFR-1.4.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How much stderr is quoted back to the user.
const STDERR_TAIL: usize = 800;

/// A program to run and the arguments to pass it.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    program: PathBuf,
    args: Vec<OsString>,
    timeout: Option<Duration>,
    cwd: Option<PathBuf>,
}

impl CommandSpec {
    /// Starts building a command for `program`.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            timeout: None,
            cwd: None,
        }
    }

    /// Appends one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Appends several arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Overrides the timeout for this call.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Runs the child in a different directory.
    ///
    /// Used by `--path` and by every `git` call that must run inside a checkout
    /// rather than wherever the user started the app.
    #[must_use]
    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// The program.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The arguments, in order.
    #[must_use]
    pub fn argv(&self) -> &[OsString] {
        &self.args
    }

    /// The command as a line a user could paste into a shell.
    ///
    /// Only ever used for messages and for `--dry-run` (FR-6.5): the real
    /// invocation never goes through a shell.
    #[must_use]
    pub fn render(&self) -> String {
        let mut line = String::new();
        if let Some(dir) = &self.cwd {
            line.push_str("cd ");
            line.push_str(&quote(&dir.display().to_string()));
            line.push_str(" && ");
        }
        line.push_str(&quote(&self.program.display().to_string()));
        for arg in &self.args {
            line.push(' ');
            line.push_str(&quote(&arg.to_string_lossy()));
        }
        line
    }
}

/// Quotes one word for display, leaving harmless words untouched.
fn quote(word: &str) -> String {
    let safe = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@+,^~".contains(c));
    if safe {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// What a finished command produced.
#[derive(Debug, Clone)]
pub struct Output {
    /// The exit status.
    pub status: ExitStatus,
    /// Standard output, decoded lossily as UTF-8.
    pub stdout: String,
    /// Standard error, decoded lossily as UTF-8.
    pub stderr: String,
    /// Whether stdout was cut off at the cap.
    pub stdout_truncated: bool,
    /// Whether stderr was cut off at the cap.
    pub stderr_truncated: bool,
    /// How long the child ran.
    pub duration: Duration,
}

impl Output {
    /// Whether the child exited zero.
    #[must_use]
    pub fn success(&self) -> bool {
        self.status.success()
    }

    /// The exit code, if the child exited normally.
    #[must_use]
    pub fn code(&self) -> Option<i32> {
        self.status.code()
    }

    /// A one-line description of a failure, for a notification or an error.
    #[must_use]
    pub fn stderr_tail(&self) -> String {
        let trimmed = self.stderr.trim();
        if trimmed.is_empty() {
            return "no error output".to_owned();
        }
        if trimmed.len() <= STDERR_TAIL {
            return trimmed.to_owned();
        }
        // Keep the end: tools put the actual complaint last.
        let from = trimmed.len() - STDERR_TAIL;
        let tail = trimmed.get(from..).unwrap_or(trimmed);
        format!("…{}", tail.trim_start())
    }
}

/// Why a command could not be run.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("{program} was not found on PATH")]
    NotFound { program: String },

    #[error("could not start {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{command} did not finish within {timeout:?}")]
    Timeout { command: String, timeout: Duration },

    #[error("{command} was cancelled")]
    Cancelled { command: String },

    #[error("{command} failed (exit {code}): {stderr}")]
    Failed {
        command: String,
        code: String,
        stderr: String,
    },
}

/// Runs child processes with a timeout, an output cap and cancellation.
#[derive(Debug, Clone)]
pub struct ProcessRunner {
    timeout: Duration,
    output_cap: usize,
}

impl Default for ProcessRunner {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            output_cap: DEFAULT_OUTPUT_CAP,
        }
    }
}

impl ProcessRunner {
    /// A runner with the default timeout and output cap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the default timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Overrides the default output cap.
    #[must_use]
    pub fn with_output_cap(mut self, cap: usize) -> Self {
        self.output_cap = cap;
        self
    }

    /// Runs a command to completion.
    ///
    /// A non-zero exit is *not* an error here: the caller decides whether it
    /// matters, and can turn it into [`ProcessError::Failed`] with
    /// [`Self::require_success`].
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when the program cannot be started, when it
    /// exceeds its timeout, or when it is cancelled.
    pub fn run(&self, spec: &CommandSpec, cancel: &Cancel) -> Result<Output, ProcessError> {
        let program = spec.program.display().to_string();
        // Only a program given *as a path* is pre-checked. A bare name is meant to
        // be found on PATH, and the operating system is the only thing that can do
        // that resolution correctly (NFR-3.3 validates configured paths, not
        // names). Skipping the check keeps the nicer "not found" error for both.
        if is_explicit_path(&spec.program) && !is_executable_file(&spec.program) {
            // Distinguish "not there" from "there but not runnable": the fix is
            // different for each.
            if spec.program.exists() {
                return Err(ProcessError::Spawn {
                    program: program.clone(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "the file exists but is not executable",
                    ),
                });
            }
            return Err(ProcessError::NotFound { program });
        }

        let started = Instant::now();
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        if let Some(dir) = &spec.cwd {
            command.current_dir(dir);
        }
        let mut child = {
            let mut attempts = 0;
            loop {
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                match command.spawn() {
                    Ok(child) => break child,
                    Err(source) => {
                        attempts += 1;
                        if is_text_file_busy(&source) && attempts < SPAWN_ATTEMPTS {
                            thread::sleep(SPAWN_BACKOFF);
                            continue;
                        }
                        return Err(match source.kind() {
                            std::io::ErrorKind::NotFound => ProcessError::NotFound {
                                program: program.clone(),
                            },
                            _ => ProcessError::Spawn {
                                program: program.clone(),
                                source,
                            },
                        });
                    }
                }
            }
        };

        // Read both pipes on their own threads: a child that fills one pipe while
        // we block on the other would otherwise deadlock.
        let cap = self.output_cap;
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_reader = stdout_pipe.map(|pipe| thread::spawn(move || read_capped(pipe, cap)));
        let stderr_reader = stderr_pipe.map(|pipe| thread::spawn(move || read_capped(pipe, cap)));

        let timeout = spec.timeout.unwrap_or(self.timeout);
        let deadline = started + timeout;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|source| ProcessError::Spawn {
                program: program.clone(),
                source,
            })? {
                break status;
            }
            if cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProcessError::Cancelled {
                    command: spec.render(),
                });
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProcessError::Timeout {
                    command: spec.render(),
                    timeout,
                });
            }
            thread::sleep(POLL_INTERVAL);
        };

        let (stdout, stdout_truncated) = join_reader(stdout_reader, GRACE);
        let (stderr, stderr_truncated) = join_reader(stderr_reader, GRACE);

        Ok(Output {
            status,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            stdout_truncated,
            stderr_truncated,
            duration: started.elapsed(),
        })
    }

    /// Runs a command and turns a non-zero exit into [`ProcessError::Failed`].
    ///
    /// # Errors
    ///
    /// As [`Self::run`], plus [`ProcessError::Failed`] for a non-zero exit.
    pub fn run_checked(&self, spec: &CommandSpec, cancel: &Cancel) -> Result<Output, ProcessError> {
        let output = self.run(spec, cancel)?;
        Self::require_success(spec, output)
    }

    /// Turns a non-zero exit into an error carrying the code and a stderr tail.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Failed`] when the command did not exit zero.
    pub fn require_success(spec: &CommandSpec, output: Output) -> Result<Output, ProcessError> {
        if output.success() {
            return Ok(output);
        }
        Err(ProcessError::Failed {
            command: spec.render(),
            code: output
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stderr: output.stderr_tail(),
        })
    }
}

/// How long the reader threads are given to finish after the child is gone.
///
/// The child has already exited, so its pipes close immediately *unless* something it
/// spawned inherited them — `git fetch` spawns helpers, and one of those holding the
/// pipe open would otherwise block this thread, and the job slot it occupies,
/// indefinitely.
const GRACE: Duration = Duration::from_millis(500);

/// Collects a reader thread's result, giving up after `grace`.
///
/// A thread that is still reading when the grace expires is left to finish on its
/// own; its output is treated as truncated, which is honest: the child is gone and
/// what it wrote is incomplete.
fn join_reader(
    handle: Option<thread::JoinHandle<(Vec<u8>, bool)>>,
    grace: Duration,
) -> (Vec<u8>, bool) {
    let Some(handle) = handle else {
        return (Vec::new(), false);
    };

    // A channel rather than `join`, because `join` has no timeout.
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let result = handle.join().unwrap_or_default();
        let _ = sender.send(result);
    });

    receiver.recv_timeout(grace).unwrap_or((Vec::new(), true))
}

/// Reads a stream, keeping at most `cap` bytes but always draining it.
///
/// Draining matters: a child blocked writing to a full pipe never exits, so
/// stopping the read at the cap would turn a large diff into a timeout.
fn read_capped(mut reader: impl Read, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut chunk = [0_u8; 16 * 1024];

    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                let room = cap.saturating_sub(kept.len());
                if room == 0 {
                    truncated = true;
                } else {
                    let take = read.min(room);
                    kept.extend_from_slice(&chunk[..take]);
                    if take < read {
                        truncated = true;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }

    (kept, truncated)
}

/// Whether an error is the kernel's "text file busy".
///
/// Matched on the raw code as well as the kind: `ExecutableFileBusy` wraps ETXTBSY,
/// but a platform that reports it as `Other` would otherwise lose the retry.
fn is_text_file_busy(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::ExecutableFileBusy {
        return true;
    }
    #[cfg(unix)]
    {
        // ETXTBSY is 26 on Linux and 26 on macOS.
        error.raw_os_error() == Some(26)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Whether a program was given as a path rather than as a name to look up.
fn is_explicit_path(program: &Path) -> bool {
    program.components().count() > 1
        || program
            .to_string_lossy()
            .contains(std::path::MAIN_SEPARATOR)
}

/// Whether `path` is a file the current user may execute (NFR-3.3).
///
/// A configured `gh` path is validated with this before it is used, so a typo in
/// the configuration is an explained error rather than a spawn failure.
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> ProcessRunner {
        ProcessRunner::new()
    }

    fn shell(script: &str) -> CommandSpec {
        CommandSpec::new("/bin/sh").args(["-c", script])
    }

    #[test]
    fn captures_stdout_and_stderr_separately() {
        let output = runner()
            .run(&shell("echo out; echo err >&2"), &Cancel::new())
            .unwrap();
        assert!(output.success());
        assert_eq!(output.stdout.trim(), "out");
        assert_eq!(output.stderr.trim(), "err");
        assert_eq!(output.code(), Some(0));
        assert!(!output.stdout_truncated);
    }

    #[test]
    fn a_non_zero_exit_is_reported_with_its_code_and_stderr() {
        let spec = shell("echo boom >&2; exit 3");
        let output = runner().run(&spec, &Cancel::new()).unwrap();
        assert!(!output.success());

        let error = ProcessRunner::require_success(&spec, output).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("exit 3"), "{message}");
        assert!(message.contains("boom"), "{message}");
    }

    #[test]
    fn a_missing_program_is_reported_as_missing_not_as_a_spawn_failure() {
        let spec = CommandSpec::new("definitely-not-a-program-9f8e7d");
        let error = runner().run(&spec, &Cancel::new()).unwrap_err();
        assert!(matches!(error, ProcessError::NotFound { .. }), "{error:?}");
        assert!(error.to_string().contains("was not found"), "{error}");
    }

    #[test]
    fn a_non_executable_file_says_so() {
        let dir = crate::test_support::temp_home();
        let path = dir.write("not-executable", "#!/bin/sh\n");
        let error = runner()
            .run(&CommandSpec::new(path), &Cancel::new())
            .unwrap_err();
        assert!(error.to_string().contains("not executable"), "{error}");
    }

    #[test]
    fn a_child_that_outlives_its_timeout_is_killed() {
        let spec = shell("sleep 30").timeout(Duration::from_millis(120));
        let started = Instant::now();
        let error = runner().run(&spec, &Cancel::new()).unwrap_err();
        assert!(matches!(error, ProcessError::Timeout { .. }), "{error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the timeout must be honoured, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_running_child_is_killed_when_cancelled() {
        let cancel = Cancel::new();
        let flag = cancel.clone();
        let started = Instant::now();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            flag.cancel();
        });

        let error = runner()
            .run(&shell("sleep 30").timeout(Duration::from_secs(30)), &cancel)
            .unwrap_err();
        handle.join().unwrap();
        assert!(matches!(error, ProcessError::Cancelled { .. }), "{error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancellation must be prompt, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn output_past_the_cap_is_dropped_but_the_stream_is_drained() {
        // A cap far below what the child writes: without draining, the child would
        // block on the full pipe and this test would time out instead.
        let spec = shell("yes abcdefgh | head -c 200000").timeout(Duration::from_secs(20));
        let output = ProcessRunner::new()
            .with_output_cap(1000)
            .run(&spec, &Cancel::new())
            .unwrap();
        assert!(output.success());
        assert_eq!(output.stdout.len(), 1000);
        assert!(output.stdout_truncated);
    }

    #[test]
    fn a_reader_that_never_finishes_is_abandoned_rather_than_waited_for() {
        // A grandchild holding the pipe open looks exactly like this: the reader
        // never sees EOF. Waiting for it would hang the job thread, so the wait is
        // bounded and the output is reported as incomplete.
        let stuck = thread::spawn(|| {
            thread::sleep(Duration::from_secs(30));
            (Vec::new(), false)
        });
        let started = Instant::now();
        let (kept, truncated) = join_reader(Some(stuck), Duration::from_millis(50));
        assert!(kept.is_empty());
        assert!(truncated, "the caller is told the output is incomplete");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn the_cap_keeps_exactly_the_cap_and_not_one_byte_more() {
        let (kept, truncated) = read_capped(std::io::Cursor::new(vec![b'x'; 5000]), 100);
        assert_eq!(kept.len(), 100);
        assert!(truncated);

        let (kept, truncated) = read_capped(std::io::Cursor::new(vec![b'x'; 10]), 100);
        assert_eq!(kept.len(), 10);
        assert!(!truncated, "nothing was dropped");
    }

    #[test]
    fn arguments_are_rendered_as_a_copyable_command() {
        let spec = CommandSpec::new("/usr/bin/gh")
            .args(["pr", "view", "141"])
            .arg("--repo")
            .arg("acme/service");
        assert_eq!(spec.render(), "/usr/bin/gh pr view 141 --repo acme/service");

        // A title full of shell metacharacters stays one quoted word.
        let hostile = CommandSpec::new("gh").arg("--title").arg("a; rm -rf ~ #");
        assert_eq!(hostile.render(), "gh --title 'a; rm -rf ~ #'");
    }

    #[test]
    fn a_single_quote_in_an_argument_is_escaped() {
        let spec = CommandSpec::new("gh").arg("it's");
        assert_eq!(spec.render(), r"gh 'it'\''s'");
    }

    #[test]
    fn the_argv_is_exactly_what_was_added() {
        let spec = CommandSpec::new("gh")
            .args(["pr", "list"])
            .arg("--limit")
            .arg("50");
        assert_eq!(
            spec.argv(),
            [
                OsString::from("pr"),
                OsString::from("list"),
                OsString::from("--limit"),
                OsString::from("50"),
            ]
        );
    }

    #[test]
    fn a_stderr_tail_is_capped_from_the_front() {
        let long = "x".repeat(2000);
        let output = runner()
            .run(&shell(&format!("printf '{long}' >&2")), &Cancel::new())
            .unwrap();
        let tail = output.stderr_tail();
        assert!(tail.len() < 900, "{} bytes", tail.len());
        assert!(tail.starts_with('…'));
    }

    #[test]
    fn an_empty_stderr_says_so_rather_than_showing_nothing() {
        let output = runner().run(&shell("exit 1"), &Cancel::new()).unwrap();
        assert_eq!(output.stderr_tail(), "no error output");
    }

    #[test]
    fn a_busy_executable_is_recognised() {
        let busy = std::io::Error::from_raw_os_error(26);
        assert!(is_text_file_busy(&busy) || !cfg!(unix));
        assert!(!is_text_file_busy(&std::io::Error::from_raw_os_error(2)));
        assert!(!is_text_file_busy(&std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "nope"
        )));
    }

    #[test]
    fn executability_is_checked_before_spawning() {
        assert!(is_executable_file(Path::new("/bin/sh")));
        assert!(!is_executable_file(Path::new("/definitely/not/here")));
        assert!(!is_executable_file(Path::new("/tmp")));
    }

    #[test]
    fn a_bare_program_name_is_looked_up_on_path() {
        // `sh` is on PATH everywhere this runs; the point is that the runner does
        // not demand it be an existing file path before spawning it.
        assert!(!is_explicit_path(Path::new("sh")));
        assert!(is_explicit_path(Path::new("/bin/sh")));
        assert!(is_explicit_path(Path::new("./sh")));

        let output = runner()
            .run(
                &CommandSpec::new("sh").args(["-c", "echo path-ok"]),
                &Cancel::new(),
            )
            .unwrap();
        assert_eq!(output.stdout.trim(), "path-ok");
    }

    #[test]
    fn a_child_runs_where_it_is_told_to() {
        let dir = crate::test_support::temp_home();
        let spec = CommandSpec::new("/bin/sh")
            .args(["-c", "pwd"])
            .current_dir(dir.path());
        let output = runner().run(&spec, &Cancel::new()).unwrap();
        // macOS reports /private/tmp for /tmp, so compare the tail.
        assert!(
            output
                .stdout
                .trim()
                .ends_with(dir.path().file_name().unwrap().to_string_lossy().as_ref()),
            "{} should be inside {}",
            output.stdout.trim(),
            dir.path().display()
        );
    }

    #[test]
    fn a_command_with_a_directory_renders_a_reproducible_line() {
        let spec = CommandSpec::new("/usr/bin/gh")
            .args(["pr", "list"])
            .current_dir("/srv/service");
        assert_eq!(
            spec.render(),
            "cd /srv/service && /usr/bin/gh pr list",
            "a copyable command has to say where it runs"
        );
    }
}
