//! Test-only helpers shared between unit tests inside the crate.
//!
//! Deliberately dependency-free: temporary directories and fake clocks are
//! implemented here rather than pulling in another crate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Hands out a distinct number per temporary directory in this process.
///
/// The clock alone is not enough: macOS reports time at microsecond resolution,
/// so two tests starting in the same microsecond would share a directory, and one
/// deleting it on drop would pull the ground out from under the other (which is
/// exactly what happened on the macOS runner).
static NEXT_HOME: AtomicU64 = AtomicU64::new(0);

/// A unique temporary directory that removes itself when dropped.
#[derive(Debug)]
pub(crate) struct TempHome {
    path: PathBuf,
}

impl TempHome {
    /// Creates a fresh temporary directory.
    pub(crate) fn new() -> Self {
        let unique = format!(
            "smart-review-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default(),
            NEXT_HOME.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        let _ = std::fs::create_dir_all(&path);
        Self { path }
    }

    /// The directory path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Writes `contents` to `name` inside the directory and returns the path.
    pub(crate) fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.path.join(name);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, contents);
        path
    }

    /// Writes an executable script and returns its path.
    ///
    /// The contents go to a temporary name and are renamed into place, because writing
    /// the script and then `exec`ing it is racy: another thread's `fork` inherits the
    /// open write descriptor, and the kernel refuses to execute a file that any
    /// process still holds open for writing — "Text file busy". The rename gives the
    /// exec target an inode that was never open for writing anywhere.
    #[cfg(unix)]
    pub(crate) fn write_executable(&self, name: &str, contents: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let final_path = self.path.join(name);
        let temporary = self.path.join(format!("{name}.pending"));
        let _ = std::fs::write(&temporary, contents);
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::rename(&temporary, &final_path);
        final_path
    }

    /// Writes an executable script; on platforms without modes this is [`Self::write`].
    #[cfg(not(unix))]
    pub(crate) fn write_executable(&self, name: &str, contents: &str) -> PathBuf {
        self.write(name, contents)
    }
}

impl Default for TempHome {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Shorthand for [`TempHome::new`].
pub(crate) fn temp_home() -> TempHome {
    TempHome::new()
}

/// An in-memory [`CacheStore`](crate::ports::CacheStore) for tests.
///
/// Exists so the cache-first and offline paths of the application layer can be
/// exercised without touching a disk (NFR-5.2).
#[derive(Debug, Default)]
pub(crate) struct InMemoryCache {
    entries: Mutex<std::collections::BTreeMap<String, crate::ports::Stored>>,
}

impl crate::ports::CacheStore for InMemoryCache {
    fn read(&self, key: &crate::ports::CacheKey) -> crate::Result<Option<crate::ports::Stored>> {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(guard.get(key.as_str()).cloned())
    }

    fn write(&self, key: &crate::ports::CacheKey, body: &str, now: u64) -> crate::Result<()> {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(
            key.as_str().to_owned(),
            crate::ports::Stored {
                body: body.to_owned(),
                fetched_at: now,
            },
        );
        Ok(())
    }

    fn remove(&self, key: &crate::ports::CacheKey) -> crate::Result<()> {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.remove(key.as_str());
        Ok(())
    }

    fn clear_prefix(&self, prefix: &str) -> crate::Result<u32> {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = guard.len();
        guard.retain(|key, _| !key.starts_with(prefix));
        Ok(u32::try_from(before - guard.len()).unwrap_or(u32::MAX))
    }
}

/// An in-memory [`StateStore`](crate::ports::StateStore) for tests.
///
/// Exists so the port is a real seam rather than a single implementation behind
/// a trait: application and loop tests can substitute it for the on-disk store.
#[derive(Debug, Default)]
pub(crate) struct InMemoryStateStore {
    state: Mutex<Option<crate::state::AppState>>,
}

impl crate::ports::StateStore for InMemoryStateStore {
    fn load(&self) -> crate::Result<crate::state::AppState> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(guard.clone().unwrap_or_default())
    }

    fn save(&self, value: &crate::state::AppState) -> crate::Result<()> {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(value.clone());
        Ok(())
    }
}

/// A workspace port that answers detection and refuses the rest.
///
/// The worktree operations belong to `git.rs` and are exercised there against a
/// real repository; the tests that need a `WorkspacePort` at all are about
/// detection, the job runner and the event loop, and they should not grow six
/// methods each. The refusal is loud rather than silent: a test that reaches one of
/// these by accident is told which call it was.
#[derive(Debug, Default)]
pub(crate) struct FakeWorkspace {
    /// What `detect` reports.
    pub(crate) info: crate::ports::workspace::RepoInfo,
    /// The diff a `diff` call returns, keyed by the head SHA it was asked for.
    pub(crate) diffs: BTreeMap<String, String>,
    /// Files a `read_file` call returns, keyed by `rev:path`.
    pub(crate) files: BTreeMap<String, Vec<u8>>,
    /// The files `list_files` reports.
    pub(crate) tracked: Vec<String>,
    /// Worktrees `list` reports.
    pub(crate) entries: Vec<crate::ports::workspace::WorkspaceEntry>,
    /// Calls that were made, for assertions about what the app asked for.
    calls: Mutex<Vec<String>>,
}

impl FakeWorkspace {
    /// A fake that reports the given repository information.
    pub(crate) fn new(info: crate::ports::workspace::RepoInfo) -> Self {
        Self {
            info,
            ..Self::default()
        }
    }

    /// Records a call and returns the refusal.
    fn unsupported(&self, call: &str) -> crate::ports::workspace::WorkspaceError {
        self.record(call);
        crate::ports::workspace::WorkspaceError::Failed(format!(
            "the test's fake workspace does not implement {call}"
        ))
    }

    /// Notes that a call happened, so a test can assert what the app asked for.
    pub(crate) fn record(&self, call: &str) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(call.to_owned());
    }

    /// The calls made so far, in order.
    #[allow(
        dead_code,
        reason = "used by the M2a tests that assert what the app asked the workspace for"
    )]
    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl crate::ports::workspace::WorkspacePort for FakeWorkspace {
    fn detect(
        &self,
    ) -> Result<crate::ports::workspace::RepoInfo, crate::ports::workspace::WorkspaceError> {
        Ok(self.info.clone())
    }

    fn ensure(
        &self,
        request: &crate::ports::workspace::WorkspaceRequest,
        _cancel: &crate::ports::Cancel,
    ) -> Result<crate::ports::workspace::Workspace, crate::ports::workspace::WorkspaceError> {
        self.record("ensure");
        Ok(crate::ports::workspace::Workspace {
            path: PathBuf::from(format!("/tmp/fake/pr-{}", request.number)),
            base_sha: "base".to_owned(),
            head_sha: request.head_sha.clone(),
            reused: false,
        })
    }

    fn diff(
        &self,
        request: &crate::ports::workspace::DiffRequest,
        _cancel: &crate::ports::Cancel,
    ) -> Result<String, crate::ports::workspace::WorkspaceError> {
        self.record("diff");
        Ok(self
            .diffs
            .get(&request.head_sha)
            .cloned()
            .unwrap_or_default())
    }

    fn read_file(
        &self,
        _repo: &Path,
        rev: &str,
        path: &str,
        _cancel: &crate::ports::Cancel,
    ) -> Result<Vec<u8>, crate::ports::workspace::WorkspaceError> {
        self.record("read_file");
        self.files
            .get(&format!("{rev}:{path}"))
            .cloned()
            .ok_or_else(|| crate::ports::workspace::WorkspaceError::NotFound {
                path: path.to_owned(),
                rev: rev.to_owned(),
            })
    }

    fn list_files(
        &self,
        _repo: &Path,
        _rev: &str,
        _cancel: &crate::ports::Cancel,
    ) -> Result<Vec<String>, crate::ports::workspace::WorkspaceError> {
        self.record("list_files");
        Ok(self.tracked.clone())
    }

    fn list(
        &self,
    ) -> Result<Vec<crate::ports::workspace::WorkspaceEntry>, crate::ports::workspace::WorkspaceError>
    {
        self.record("list");
        Ok(self.entries.clone())
    }

    fn remove(
        &self,
        _repo: &crate::domain::repo::RepoId,
        _number: u64,
        _cancel: &crate::ports::Cancel,
    ) -> Result<(), crate::ports::workspace::WorkspaceError> {
        Err(self.unsupported("remove"))
    }
}

/// A bare "origin" plus a clone of it, with a pull request head at
/// `refs/pull/<N>/head` — the ref GitHub publishes for every pull request,
/// including from forks (FR-3.1).
///
/// Built with the real `git` on purpose: the workspace adapter's whole job is to
/// drive git's refspecs and worktree machinery, and a fake would only test the fake.
/// It panics rather than skipping when git is missing: M2a is *about* driving git, so
/// a test that quietly returns when the fixture cannot be built is a test that cannot
/// fail, and this one did exactly that once already.
#[derive(Debug)]
pub(crate) struct GitFixture {
    dir: TempHome,
    /// The bare repository that plays the role of GitHub.
    pub(crate) origin: PathBuf,
    /// A clone of it, playing the role of the user's checkout.
    pub(crate) clone: PathBuf,
}

impl GitFixture {
    /// Builds the fixture.
    ///
    /// Panics when git cannot build it. That is deliberate: an optional fixture read
    /// as `if let Some(..)` turns every test that uses it into a silent skip, and the
    /// workspace tests are the acceptance criteria for FR-3.1 rather than a nicety.
    pub(crate) fn new() -> Self {
        let dir = TempHome::new();
        let origin = dir.path().join("origin.git");
        let clone = dir.path().join("clone");
        let ok = |cwd: &Path, args: &[&str]| -> Option<String> {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
        };

        std::fs::create_dir_all(&origin).expect("the fixture directory is created");
        ok(&origin, &["init", "--bare", "--quiet", "-b", "main"])
            .expect("git init --bare works: is git installed?");
        ok(
            dir.path(),
            &[
                "clone",
                "--quiet",
                origin.to_str().expect("a UTF-8 path"),
                clone.to_str().expect("a UTF-8 path"),
            ],
        )
        .expect("git clone works");
        // `git config` takes the key and the value as separate arguments; the
        // `key=value` form is rejected ("invalid key"), which is how this fixture
        // first came to fail every test that used it.
        for (key, value) in [
            ("user.email", "t@example.com"),
            ("user.name", "Test"),
            ("commit.gpgsign", "false"),
        ] {
            ok(&clone, &["config", key, value]).expect("git config works");
        }
        ok(&clone, &["checkout", "--quiet", "-b", "main"]).expect("git checkout works");
        Self { dir, origin, clone }
    }

    /// The fixture's private directory, for extra files (a `git` shim, say).
    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Runs git in the clone, returning stdout, and panics on failure.
    pub(crate) fn git(&self, args: &[&str]) -> String {
        Self::git_in(&self.clone, args)
    }

    /// Runs git in a directory, returning stdout, and panics on failure.
    ///
    /// Without `&self` because the directory is what identifies the repository: the
    /// fixture is just a convenient place to hang it.
    pub(crate) fn git_in(cwd: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Writes a file, commits it on the current branch and returns the new SHA.
    pub(crate) fn commit(&self, file: &str, contents: &str, message: &str) -> String {
        let path = self.clone.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("creates the directory");
        }
        std::fs::write(&path, contents).expect("writes the file");
        self.git(&["add", "--", file]);
        self.git(&["commit", "--quiet", "-m", message]);
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// Pushes the clone's current commit to `main` on the origin.
    pub(crate) fn push_main(&self) {
        self.git(&["push", "--quiet", "origin", "main"]);
    }

    /// Creates a commit on a throwaway branch and publishes it as the head of pull
    /// request `number`, then restores the clone to `main`.
    ///
    /// The clone is left exactly as it was: no extra local branch, `HEAD` on `main`,
    /// clean tree — which is what lets a test assert that materialising a pull
    /// request changed nothing about the user's checkout.
    pub(crate) fn publish_pull_request(
        &self,
        number: u64,
        change: impl FnOnce(&Self) -> String,
    ) -> String {
        self.git(&["checkout", "--quiet", "-b", "pr-work"]);
        let sha = change(self);
        // Forced on purpose: calling this twice models an author who amended or
        // rebased their branch, which is what makes a materialised workspace stale.
        self.git(&[
            "push",
            "--quiet",
            "--force",
            "origin",
            &format!("HEAD:refs/pull/{number}/head"),
        ]);
        self.git(&["checkout", "--quiet", "main"]);
        self.git(&["branch", "--quiet", "-D", "pr-work"]);
        sha
    }

    /// The commits a branch points at, for "nothing moved" assertions.
    pub(crate) fn head_sha(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// The tracked files git reports, for "the checkout is clean" assertions.
    pub(crate) fn status(&self) -> String {
        self.git(&["status", "--porcelain"])
    }
}

/// A clock the test drives (NFR-5.2).
#[derive(Debug)]
pub(crate) struct FakeClock(AtomicU64);

impl FakeClock {
    /// A clock fixed at `now` seconds since the epoch.
    pub(crate) fn new(now: u64) -> Self {
        Self(AtomicU64::new(now))
    }

    /// Moves the clock forward.
    pub(crate) fn advance(&self, secs: u64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl crate::ports::Clock for FakeClock {
    fn now_unix_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A model catalog that never answers, for tests that never open the picker.
#[derive(Debug)]
pub(crate) struct NoCatalog;

impl crate::ports::ModelCatalogPort for NoCatalog {
    fn load(
        &self,
        _policy: crate::ports::CatalogPolicy,
    ) -> Result<crate::ports::CatalogLoad, crate::ports::CatalogFetchError> {
        Err(crate::ports::CatalogFetchError::Unavailable(
            "this test has no model catalog".to_owned(),
        ))
    }
}

/// An LLM client that never answers, for the same reason.
#[derive(Debug)]
pub(crate) struct NoLlm;

impl crate::ports::LlmPort for NoLlm {
    fn complete(
        &self,
        _request: &crate::ports::ChatRequest,
        _cancel: &crate::ports::Cancel,
    ) -> Result<crate::ports::ChatOutcome, crate::ports::LlmError> {
        Err(crate::ports::LlmError::Transport {
            provider: "test".to_owned(),
            reason: "this test has no provider".to_owned(),
        })
    }

    fn stream(
        &self,
        _request: &crate::ports::ChatRequest,
        _cancel: &crate::ports::Cancel,
        _on_delta: &mut crate::ports::DeltaHandler<'_>,
    ) -> Result<crate::ports::ChatOutcome, crate::ports::LlmError> {
        Err(crate::ports::LlmError::Transport {
            provider: "test".to_owned(),
            reason: "this test has no provider".to_owned(),
        })
    }
}

/// A job runner wired for tests: the real ports detection needs, and the two the
/// picker uses replaced by ones that refuse.
pub(crate) fn test_job_runner(
    workspace: std::sync::Arc<dyn crate::ports::WorkspacePort>,
    probe: std::sync::Arc<dyn crate::ports::ForgeProbe>,
    request: crate::application::DetectRequest,
) -> crate::tui::jobs::JobRunner {
    crate::tui::jobs::JobRunner::new(
        workspace,
        probe,
        std::sync::Arc::new(NoCatalog),
        std::sync::Arc::new(NoLlm),
        request,
    )
}
