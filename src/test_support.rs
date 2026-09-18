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

/// An in-memory [`DraftStorePort`](crate::ports::DraftStorePort) for tests.
///
/// In memory rather than on disk because most of these tests are about what the
/// interface does with a draft, not about where it is kept — and a test that writes to
/// a real directory has to be believed about cleaning up after itself.
#[derive(Debug, Default)]
pub(crate) struct FakeDraftStore {
    drafts: Mutex<std::collections::BTreeMap<(String, u64), crate::domain::draft::Draft>>,
    /// Set to make every write fail, which is how "the draft could not be saved" is
    /// exercised.
    pub(crate) fail: bool,
}

impl crate::ports::DraftStorePort for FakeDraftStore {
    fn load(
        &self,
        repo: &crate::domain::repo::RepoId,
        number: u64,
    ) -> Result<Option<crate::domain::draft::Draft>, crate::ports::DraftStoreError> {
        Ok(self
            .drafts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(repo.key(), number))
            .cloned())
    }

    fn save(
        &self,
        repo: &crate::domain::repo::RepoId,
        draft: &crate::domain::draft::Draft,
    ) -> Result<(), crate::ports::DraftStoreError> {
        if self.fail {
            return Err(crate::ports::DraftStoreError::Io {
                action: "write the draft",
                path: std::path::PathBuf::from("/dev/null"),
                source: std::io::Error::other("the fake was told to fail"),
            });
        }
        self.drafts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((repo.key(), draft.pr), draft.clone());
        Ok(())
    }

    fn remove(
        &self,
        repo: &crate::domain::repo::RepoId,
        number: u64,
    ) -> Result<(), crate::ports::DraftStoreError> {
        self.drafts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(repo.key(), number));
        Ok(())
    }

    fn remove_if_matches(
        &self,
        repo: &crate::domain::repo::RepoId,
        submitted: &crate::domain::draft::Draft,
    ) -> Result<bool, crate::ports::DraftStoreError> {
        let mut drafts = self
            .drafts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (repo.key(), submitted.pr);
        if drafts.get(&key) == Some(submitted) {
            drafts.remove(&key);
            return Ok(true);
        }
        Ok(false)
    }

    fn list(
        &self,
        repo: &crate::domain::repo::RepoId,
    ) -> Result<Vec<crate::domain::draft::Draft>, crate::ports::DraftStoreError> {
        Ok(self
            .drafts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|((key, _), _)| key == &repo.key())
            .map(|(_, draft)| draft.clone())
            .collect())
    }
}

/// An in-memory [`MutationStorePort`](crate::ports::MutationStorePort) for mutation
/// lifecycle tests (IR-07).
#[derive(Debug, Default)]
pub(crate) struct FakeMutationStore {
    operations:
        Mutex<std::collections::BTreeMap<String, crate::domain::mutation::MutationOperation>>,
}

impl crate::ports::MutationStorePort for FakeMutationStore {
    fn begin(
        &self,
        operation: &crate::domain::mutation::MutationOperation,
    ) -> Result<(), crate::ports::MutationStoreError> {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if operations.contains_key(&operation.id)
            || operations.values().any(|existing| {
                existing.repo == operation.repo
                    && existing.pr == operation.pr
                    && existing.state.needs_reconciliation()
            })
        {
            return Err(crate::ports::MutationStoreError::Conflict);
        }
        operations.insert(operation.id.clone(), operation.clone());
        Ok(())
    }

    fn save(
        &self,
        operation: &crate::domain::mutation::MutationOperation,
    ) -> Result<(), crate::ports::MutationStoreError> {
        self.operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(operation.id.clone(), operation.clone());
        Ok(())
    }

    fn unresolved(
        &self,
        repo: &crate::domain::repo::RepoId,
        pr: u64,
    ) -> Result<Vec<crate::domain::mutation::MutationOperation>, crate::ports::MutationStoreError>
    {
        Ok(self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|operation| {
                operation.repo == *repo
                    && operation.pr == pr
                    && operation.state.needs_reconciliation()
            })
            .cloned()
            .collect())
    }
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
    /// Repository-relative paths matched by ignore rules.
    pub(crate) ignored: Vec<String>,
    /// Revision-specific ignore matches, keyed by commit SHA.
    pub(crate) ignored_at: BTreeMap<String, Vec<String>>,
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

    fn ignored_paths(
        &self,
        _repo: &Path,
        rev: &str,
        paths: &[String],
        _cancel: &crate::ports::Cancel,
    ) -> Result<Vec<String>, crate::ports::workspace::WorkspaceError> {
        self.record("ignored_paths");
        Ok(paths
            .iter()
            .filter(|path| {
                self.ignored.contains(path)
                    || self
                        .ignored_at
                        .get(rev)
                        .is_some_and(|ignored| ignored.contains(path))
            })
            .cloned()
            .collect())
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
        _cancel: &crate::ports::Cancel,
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

/// A resolved environment, for tests that need a repository and a forge (FR-1.1).
pub(crate) fn environment() -> crate::domain::environment::Environment {
    crate::domain::environment::Environment {
        repo: crate::domain::repo::RepoId::new("github.com", "acme", "service"),
        mode: crate::domain::environment::RunMode::InRepo,
        remote: Some("origin".to_owned()),
        remote_url: Some("https://github.com/acme/service.git".to_owned()),
        root: Some(std::path::PathBuf::from("/src/service")),
        default_branch: Some("main".to_owned()),
        git_version: "2.43.0".to_owned(),
        gh: crate::domain::environment::GhInstall {
            path: std::path::PathBuf::from("/usr/bin/gh"),
            version: "2.45.0".to_owned(),
            account: Some("tester".to_owned()),
            scopes: vec!["repo".to_owned()],
        },
    }
}

/// A selection that is ready to use, for tests that need a model (FR-4.5).
pub(crate) fn resolved_model() -> crate::application::models::ResolvedSelection {
    crate::application::models::ResolvedSelection {
        provider: "deepseek".to_owned(),
        model: "deepseek-v4-pro".to_owned(),
        route: crate::domain::model::Route::Native(crate::domain::model::NativeBackend::DeepSeek),
        base_url: None,
        env_var: Some("DEEPSEEK_API_KEY".to_owned()),
        env_source: None,
        from_file: true,
        thinking: None,
        settings: crate::application::models::EffectiveRequestSettings {
            configured_temperature: None,
            temperature: None,
            max_tokens: None,
            configured_max_tokens: None,
            catalog_output_tokens: None,
            input_tokens: 100_000,
            configured_input_tokens: 100_000,
            model_window: None,
        },
        warnings: Vec::new(),
    }
}

/// A pull request detail for the analysis tests (FR-4.1).
pub(crate) fn analysis_detail() -> crate::domain::pr::PullRequestDetail {
    let summary = crate::domain::pr::PullRequestSummary {
        number: 141,
        title: "Round half up".to_owned(),
        author: "someone".to_owned(),
        state: crate::domain::pr::PrState::Open,
        is_draft: false,
        base_ref: "main".to_owned(),
        head_ref: "rounding".to_owned(),
        head_sha: "abc123".to_owned(),
        created_at: crate::domain::time::from_unix_secs(0),
        updated_at: crate::domain::time::from_unix_secs(0),
        additions: 4,
        deletions: 2,
        changed_files: 2,
        labels: Vec::new(),
        review_decision: None,
        checks: crate::domain::pr::CheckSummary::default(),
        url: "https://example.invalid/141".to_owned(),
        is_cross_repository: false,
    };
    crate::domain::pr::PullRequestDetail {
        summary,
        body: "Fixes a rounding bug.".to_owned(),
        merge_state_status: None,
        reviewers: Vec::new(),
        commits: vec![crate::domain::pr::Commit {
            sha: "abcdef1234567890".to_owned(),
            summary: "round half up".to_owned(),
            author: "someone".to_owned(),
            committed_at: crate::domain::time::from_unix_secs(0),
        }],
        checks: Vec::new(),
        reviews: Vec::new(),
        comments: Vec::new(),
        conversation: Vec::new(),
        base_sha: None,
    }
}

/// A patch with two files, so a plan has something to order (FR-4.2).
pub(crate) fn analysis_patch() -> crate::domain::diff::Patch {
    crate::domain::diff::parse_patch(
        "diff --git a/src/domain/money.rs b/src/domain/money.rs\n\
         --- a/src/domain/money.rs\n\
         +++ b/src/domain/money.rs\n\
         @@ -1 +1 @@\n\
         -old\n\
         +new\n\
         diff --git a/tests/money.rs b/tests/money.rs\n\
         new file mode 100644\n\
         --- /dev/null\n\
         +++ b/tests/money.rs\n\
         @@ -0,0 +1 @@\n\
         +fn it_rounds() {}\n",
    )
}

/// An analysis document for the panel tests (FR-4.1).
pub(crate) fn stored_analysis(head_sha: &str) -> crate::ports::StoredAnalysis {
    let index = crate::domain::analysis::PathIndex::from_paths(
        ["src/domain/money.rs", "tests/money.rs"].map(str::to_owned),
    );
    let normalized = crate::domain::analysis::normalize(
        r#"{"summary": "Billing rounds half up.", "intent": "fix a bug",
            "risk_areas": [{"title": "Rounding", "severity": "high",
                            "files": ["src/domain/money.rs"], "why": "money"}],
            "review_plan": [{"order": 1, "group": "domain", "rationale": "rules first",
                             "files": ["src/domain/money.rs"]},
                            {"order": 2, "group": "tests", "rationale": "then the tests",
                             "files": ["tests/money.rs"]}],
            "per_file_notes": [{"path": "src/domain/money.rs", "change": "rounding",
                                "notes": "check the sign", "review_focus": ["negatives"]}],
            "suggested_questions": ["Documented?"]}"#,
        &index,
        "deepseek/deepseek-v4-pro",
        head_sha,
        "2026-01-01T00:00:00Z",
        crate::domain::analysis::AnalysisUsage::default(),
    )
    .expect("the fixture normalizes");
    crate::ports::StoredAnalysis {
        key: crate::ports::AnalysisKey {
            repo: "github.com/acme/service".to_owned(),
            pr: 141,
            head_sha: head_sha.to_owned(),
            base_sha: Some("base123".to_owned()),
            context_fingerprint: "context".to_owned(),
            identity_version: 1,
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            endpoint: Some("https://api.deepseek.test/v1".to_owned()),
            thinking: None,
            input_tokens: 12_000,
            max_tokens: Some(4_000),
            temperature: Some("0.2".to_owned()),
            prompt_version: crate::domain::analysis::PROMPT_VERSION,
        },
        analysis: normalized.analysis,
        raw: "{}".to_owned(),
        warnings: normalized.warnings,
        repaired: false,
        stored_at: 1_767_225_600,
    }
}

/// An analysis cache in memory, for tests that run the analysis path (FR-4.3).
#[derive(Debug, Default)]
pub(crate) struct InMemoryAnalysis {
    entries: Mutex<BTreeMap<String, crate::ports::StoredAnalysis>>,
    plans: Mutex<BTreeMap<String, crate::domain::plan::Plan>>,
}

impl crate::ports::AnalysisCachePort for InMemoryAnalysis {
    fn get(
        &self,
        key: &crate::ports::AnalysisKey,
    ) -> Result<Option<crate::ports::StoredAnalysis>, crate::ports::AnalysisCacheError> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key.digest())
            .cloned())
    }

    fn list(
        &self,
        repo: &crate::domain::repo::RepoId,
        pr: u64,
    ) -> Result<Vec<crate::ports::StoredAnalysis>, crate::ports::AnalysisCacheError> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|stored| stored.key.repo == repo.key() && stored.key.pr == pr)
            .cloned()
            .collect())
    }

    fn put(
        &self,
        stored: &crate::ports::StoredAnalysis,
    ) -> Result<(), crate::ports::AnalysisCacheError> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(stored.key.digest(), stored.clone());
        Ok(())
    }

    fn plan(
        &self,
        repo: &crate::domain::repo::RepoId,
        pr: u64,
    ) -> Result<Option<crate::domain::plan::Plan>, crate::ports::AnalysisCacheError> {
        Ok(self
            .plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&format!("{}/pr-{pr}", repo.key()))
            .cloned())
    }

    fn put_plan(
        &self,
        repo: &crate::domain::repo::RepoId,
        pr: u64,
        plan: &crate::domain::plan::Plan,
    ) -> Result<crate::domain::plan::Plan, crate::ports::AnalysisCacheError> {
        self.plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(format!("{}/pr-{pr}", repo.key()), plan.clone());
        Ok(plan.clone())
    }
}

/// An in-memory [`ChatStorePort`](crate::ports::ChatStorePort).
///
/// A `BTreeMap` per pull request, with the same pruning rule the real store has — the
/// rule is DEC-9's, not the filesystem's, so a fake that skipped it would let a test
/// pass while the app filled a disk.
#[derive(Debug, Default)]
pub(crate) struct FakeChatStore {
    sessions:
        Mutex<std::collections::BTreeMap<(String, u64, String), crate::domain::chat::Session>>,
}

impl FakeChatStore {
    /// Every session, for an assertion about what was written.
    #[cfg(test)]
    pub(crate) fn all(&self) -> Vec<crate::domain::chat::Session> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }
}

impl crate::ports::ChatStorePort for FakeChatStore {
    fn list(
        &self,
        repo: &crate::domain::repo::RepoId,
        pr: u64,
    ) -> Result<Vec<crate::domain::chat::SessionMeta>, crate::ports::ChatStoreError> {
        let mut metas: Vec<crate::domain::chat::SessionMeta> = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|session| session.repo == repo.key() && session.pr == pr)
            .map(crate::domain::chat::SessionMeta::of)
            .collect();
        metas.sort_by_key(|meta| std::cmp::Reverse((meta.updated_at, meta.id.clone())));
        Ok(metas)
    }

    fn load(
        &self,
        repo: &crate::domain::repo::RepoId,
        pr: u64,
        id: &str,
    ) -> Result<Option<crate::domain::chat::Session>, crate::ports::ChatStoreError> {
        Ok(self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(repo.key(), pr, id.to_owned()))
            .cloned())
    }

    fn latest(
        &self,
        repo: &crate::domain::repo::RepoId,
        pr: u64,
    ) -> Result<Option<crate::domain::chat::Session>, crate::ports::ChatStoreError> {
        let Some(meta) = self.list(repo, pr)?.into_iter().next() else {
            return Ok(None);
        };
        self.load(repo, pr, &meta.id)
    }

    fn put(
        &self,
        session: &crate::domain::chat::Session,
    ) -> Result<(), crate::ports::ChatStoreError> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                (session.repo.clone(), session.pr, session.id.clone()),
                session.clone(),
            );
        Ok(())
    }

    fn remove(
        &self,
        repo: &crate::domain::repo::RepoId,
        pr: u64,
        id: &str,
    ) -> Result<(), crate::ports::ChatStoreError> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(repo.key(), pr, id.to_owned()));
        Ok(())
    }

    fn prune(
        &self,
        _repo: &crate::domain::repo::RepoId,
        _pr: u64,
    ) -> Result<crate::domain::chat::Pruned, crate::ports::ChatStoreError> {
        // The fake stores nothing it should not, so there is nothing to prune; the
        // pruning rule itself is tested against the real store and in `domain::chat`.
        Ok(crate::domain::chat::Pruned::default())
    }
}

/// A [`SecretStore`](crate::ports::SecretStore) holding one key for every provider.
///
/// Simpler than the file-backed one, and it exists because a test that has to write a
/// `credentials.toml` to test anything that talks to a provider is a test that spends
/// its lines on the filesystem rather than on the behaviour.
#[derive(Debug)]
pub(crate) struct FixedSecrets {
    key: String,
    store: Mutex<std::collections::BTreeMap<String, String>>,
}

impl FixedSecrets {
    /// A store where every provider has this key.
    pub(crate) fn new(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            store: Mutex::new(std::collections::BTreeMap::new()),
        }
    }
}

impl crate::ports::SecretStore for FixedSecrets {
    fn get(
        &self,
        provider: &str,
        env_var: Option<&str>,
    ) -> Result<Option<crate::ports::ApiKey>, crate::ports::SecretError> {
        let _ = env_var;
        let stored = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(provider)
            .cloned()
            .unwrap_or_else(|| self.key.clone());
        Ok(Some(crate::ports::ApiKey::new(
            stored,
            crate::ports::KeySource::File,
        )))
    }

    fn set(&self, provider: &str, key: &str) -> Result<(), crate::ports::SecretError> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(provider.to_owned(), key.to_owned());
        Ok(())
    }

    fn remove(&self, provider: &str) -> Result<(), crate::ports::SecretError> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(provider);
        Ok(())
    }

    fn status(&self) -> Result<Vec<crate::ports::KeyStatus>, crate::ports::SecretError> {
        Ok(self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .map(|provider| crate::ports::KeyStatus {
                provider: provider.clone(),
                source: Some(crate::ports::KeySource::File),
            })
            .collect())
    }
}

/// A sample pull request, for tests that need a detail but are not about parsing one.
pub(crate) fn sample_detail() -> crate::domain::pr::PullRequestDetail {
    let summary = crate::domain::pr::PullRequestSummary {
        number: 141,
        title: "Round half up".to_owned(),
        author: "someone".to_owned(),
        created_at: crate::domain::time::from_unix_secs(0),
        updated_at: crate::domain::time::from_unix_secs(0),
        is_draft: false,
        base_ref: "main".to_owned(),
        head_ref: "rounding".to_owned(),
        head_sha: "abc123".to_owned(),
        additions: 10,
        deletions: 2,
        changed_files: 1,
        labels: vec!["bug".to_owned()],
        review_decision: None,
        checks: crate::domain::pr::CheckSummary::default(),
        state: crate::domain::pr::PrState::Open,
        url: "https://example.invalid/141".to_owned(),
        is_cross_repository: false,
    };
    crate::domain::pr::PullRequestDetail {
        summary,
        body: "Fixes a rounding bug.".to_owned(),
        merge_state_status: None,
        reviewers: Vec::new(),
        commits: vec![crate::domain::pr::Commit {
            sha: "abcdef1234567890".to_owned(),
            summary: "round half up".to_owned(),
            author: "someone".to_owned(),
            committed_at: crate::domain::time::from_unix_secs(0),
        }],
        checks: Vec::new(),
        reviews: Vec::new(),
        comments: Vec::new(),
        conversation: Vec::new(),
        base_sha: None,
    }
}

/// An [`LlmPort`](crate::ports::LlmPort) that answers from a list and records what it
/// was asked.
///
/// It records the *history* as well as the prompt, because "what the provider saw" is
/// the thing a chat test is about (FR-5.3): a fake that only kept the prompt would pass
/// while sending the whole conversation nowhere.
#[derive(Debug, Default)]
pub(crate) struct FakeLlm {
    /// The answers, in the order they will be given.
    answers: Mutex<std::collections::VecDeque<String>>,
    /// `(system, prompt)` per request.
    pub(crate) prompts: Mutex<Vec<(Option<String>, String)>>,
    /// The conversation that came with each request.
    pub(crate) histories: Mutex<Vec<Vec<(crate::domain::chat::Role, String)>>>,
    /// A failure to return instead of an answer.
    failure: Option<crate::ports::LlmError>,
}

impl FakeLlm {
    /// A fake that answers with these texts, in order.
    pub(crate) fn answering(answers: &[&str]) -> Self {
        Self {
            answers: Mutex::new(answers.iter().map(|a| (*a).to_owned()).collect()),
            ..Self::default()
        }
    }

    /// A fake that always fails this way.
    pub(crate) fn failing(error: crate::ports::LlmError) -> Self {
        Self {
            failure: Some(error),
            ..Self::default()
        }
    }
}

impl crate::ports::LlmPort for FakeLlm {
    fn complete(
        &self,
        _request: &crate::ports::ChatRequest,
        _cancel: &crate::ports::Cancel,
    ) -> Result<crate::ports::ChatOutcome, crate::ports::LlmError> {
        Err(crate::ports::LlmError::Request {
            provider: "test".to_owned(),
            reason: "this fake implements `stream` only".to_owned(),
        })
    }

    fn stream(
        &self,
        request: &crate::ports::ChatRequest,
        cancel: &crate::ports::Cancel,
        on_delta: &mut crate::ports::DeltaHandler<'_>,
    ) -> Result<crate::ports::ChatOutcome, crate::ports::LlmError> {
        self.prompts
            .lock()
            .expect("lock")
            .push((request.system.clone(), request.prompt.clone()));
        self.histories
            .lock()
            .expect("lock")
            .push(request.history.clone());
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let answer = self
            .answers
            .lock()
            .expect("lock")
            .pop_front()
            .unwrap_or_else(|| "{}".to_owned());
        on_delta(&answer);
        if cancel.is_cancelled() {
            // A cancelled call reports what arrived, which is what the chat path reads
            // to keep partial text (FR-5.2).
            return Ok(crate::ports::ChatOutcome {
                text: answer,
                usage: None,
                thinking: None,
            });
        }
        Ok(crate::ports::ChatOutcome {
            text: answer,
            usage: Some(crate::ports::TokenUsage {
                prompt: 10,
                completion: 20,
                total: 30,
                reasoning: Some(5),
            }),
            thinking: None,
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
        std::sync::Arc::new(InMemoryStateStore::default()),
        request,
    )
}
