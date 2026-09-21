//! The `git` adapter (ARCH-3).
//!
//! This file answers "where am I running": the work tree root, the remotes, the
//! default branch and the git version, plus the trait implementation that ties
//! detection and the worktree operations together. The worktree operations
//! themselves live in `git/worktree.rs`.
//!
//! Nothing here can change the user's working tree, index, HEAD or branches
//! (DEC-1); see the module comment in `worktree.rs` for what a managed worktree
//! does touch.

mod worktree;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::adapters::process::{CommandSpec, ProcessError, ProcessRunner};
use crate::domain::diff::RelPath;
use crate::logging::{self, Level};
use crate::ports::Cancel;
use crate::ports::workspace::{
    DiffRequest, FileRead, FileReadOutcome, Remote, RepoInfo, Workspace, WorkspaceEntry,
    WorkspaceError, WorkspacePort, WorkspaceRequest,
};

/// Distinguishes concurrent revision ignore checks in this process.
static IGNORE_TREE_ID: AtomicU64 = AtomicU64::new(0);

/// A private, app-owned view containing only one revision's `.gitignore` files.
///
/// It is not a git worktree and has no repository metadata to clean up. Removing the
/// directory on drop also covers cancellation and command failures.
struct TemporaryIgnoreTree {
    path: PathBuf,
}

impl Drop for TemporaryIgnoreTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Runs `git` in a directory.
#[derive(Debug, Clone)]
pub struct GitCli {
    runner: ProcessRunner,
    program: PathBuf,
    cwd: Option<PathBuf>,
    /// Where managed worktrees are created. Absent until the composition root
    /// wires it in, which is what keeps the M1 detection path working unchanged.
    worktrees_root: Option<PathBuf>,
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
            worktrees_root: None,
        }
    }

    /// Uses a specific `git` and runs it in a specific directory.
    #[must_use]
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// Uses a prepared runner, which is how `--dry-run` reaches the destructive git
    /// calls (FR-6.5).
    #[must_use]
    pub fn with_runner(mut self, runner: ProcessRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Runs every command in this directory instead of the current one.
    #[must_use]
    pub fn in_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Creates managed worktrees under this directory (FR-3.1, DEC-1).
    #[must_use]
    pub fn with_worktrees(mut self, root: impl Into<PathBuf>) -> Self {
        self.worktrees_root = Some(root.into());
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

    /// The same, with an explicit timeout: `rev-parse` and a network fetch do not
    /// deserve the same patience (FR-3.1).
    pub(crate) fn spec_with_timeout(
        &self,
        args: &[&str],
        timeout: std::time::Duration,
    ) -> CommandSpec {
        self.spec(args).timeout(timeout)
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

    /// Materialises only the tracked `.gitignore` files from `rev` into a private
    /// temporary work tree. `git check-ignore` can then apply Git's own matching
    /// semantics without consulting the head checkout's rules for an old path.
    fn ignore_tree(
        &self,
        repo: &Path,
        rev: &str,
        cancel: &Cancel,
    ) -> Result<TemporaryIgnoreTree, WorkspaceError> {
        let root = self.worktrees_root()?.join(".ignore-rules");
        create_private_dir(&root)?;
        let id = IGNORE_TREE_ID.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("{}-{id}", std::process::id()));
        create_private_dir(&path)?;
        let tree = TemporaryIgnoreTree { path };

        let command = CommandSpec::new(&self.program)
            .args(["ls-tree", "-r", "-z", rev])
            .current_dir(repo);
        let output = self
            .runner
            .run(&command, cancel)
            .map_err(ProcessError::into_workspace)?;
        if !output.success() {
            return Err(WorkspaceError::Failed(format!(
                "could not list ignore rules at {rev}: {}",
                output.stderr_tail()
            )));
        }
        if output.stdout_truncated {
            return Err(WorkspaceError::Failed(format!(
                "could not list ignore rules at {rev}: the repository tree exceeded the output limit"
            )));
        }

        for rule_path in ignore_rule_paths(&output.stdout_bytes)? {
            let object = format!("{rev}:{rule_path}");
            let command = CommandSpec::new(&self.program)
                .args(["show", &object])
                .current_dir(repo);
            let output = self
                .runner
                .run(&command, cancel)
                .map_err(ProcessError::into_workspace)?;
            if !output.success() || output.stdout_truncated {
                return Err(WorkspaceError::Failed(format!(
                    "could not read ignore rule {rule_path} at {rev}: {}",
                    output.stderr_tail()
                )));
            }
            let destination = tree.path.join(&rule_path);
            if let Some(parent) = destination.parent() {
                create_private_dir(parent)?;
            }
            std::fs::write(&destination, output.stdout_bytes).map_err(|error| {
                WorkspaceError::Failed(format!(
                    "could not prepare ignore rule {rule_path} at {rev}: {error}"
                ))
            })?;
        }
        Ok(tree)
    }
}

/// Parses `git ls-tree -r -z`, returning regular files named `.gitignore`.
/// Symlinks are skipped because Git deliberately does not follow them for ignore rules.
fn ignore_rule_paths(bytes: &[u8]) -> Result<Vec<String>, WorkspaceError> {
    let mut paths = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            return Err(WorkspaceError::Failed(
                "could not parse the repository tree while checking ignore rules".to_owned(),
            ));
        };
        let metadata = std::str::from_utf8(&record[..tab]).map_err(|error| {
            WorkspaceError::Failed(format!(
                "could not parse repository tree metadata while checking ignore rules: {error}"
            ))
        })?;
        let mut fields = metadata.split_whitespace();
        let mode = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
        if kind != "blob" || !matches!(mode, "100644" | "100755") {
            continue;
        }
        let path = std::str::from_utf8(&record[tab + 1..]).map_err(|error| {
            WorkspaceError::Failed(format!(
                "a non-UTF-8 path prevents repository ignore rules from being checked: {error}"
            ))
        })?;
        if path == ".gitignore" || path.ends_with("/.gitignore") {
            let path = RelPath::parse(path).ok_or_else(|| {
                WorkspaceError::Failed(
                    "an invalid repository path prevents ignore rules from being checked"
                        .to_owned(),
                )
            })?;
            paths.push(path.to_string());
        }
    }
    Ok(paths)
}

fn create_private_dir(path: &Path) -> Result<(), WorkspaceError> {
    std::fs::create_dir_all(path).map_err(|error| {
        WorkspaceError::Failed(format!("could not create {}: {error}", path.display()))
    })?;
    set_private_dir_mode(path)
}

#[cfg(unix)]
fn set_private_dir_mode(path: &Path) -> Result<(), WorkspaceError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        WorkspaceError::Failed(format!(
            "could not restrict permissions on {}: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_private_dir_mode(_path: &Path) -> Result<(), WorkspaceError> {
    Ok(())
}

impl GitCli {
    /// Inspects the directory this adapter runs in (FR-1.1).
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when `git` cannot be run at all.
    pub fn detect(&self) -> std::result::Result<RepoInfo, WorkspaceError> {
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

impl WorkspacePort for GitCli {
    fn detect(&self) -> std::result::Result<RepoInfo, WorkspaceError> {
        Self::detect(self)
    }

    fn ensure(
        &self,
        request: &WorkspaceRequest,
        cancel: &Cancel,
    ) -> std::result::Result<Workspace, WorkspaceError> {
        self.ensure_workspace(request, cancel)
    }

    fn diff(
        &self,
        request: &DiffRequest,
        cancel: &Cancel,
    ) -> std::result::Result<String, WorkspaceError> {
        self.diff_workspace(request, cancel)
    }

    fn read_file(
        &self,
        repo: &Path,
        rev: &str,
        path: &str,
        cancel: &Cancel,
    ) -> std::result::Result<Vec<u8>, WorkspaceError> {
        // `git show <rev>:<path>` reads from the object database, so the answer does
        // not depend on the worktree's state or on the file still being there
        // (FR-4.6). A path that is not in the revision is a named error rather than
        // an empty file, so callers can skip it deliberately.
        let spec = format!("{rev}:{path}");
        let command = CommandSpec::new(&self.program)
            .args(["show", &spec])
            .current_dir(repo)
            .timeout(std::time::Duration::from_secs(60));
        let output = self
            .runner
            .run(&command, cancel)
            .map_err(ProcessError::into_workspace);
        match output {
            Ok(output) if output.success() => Ok(output.stdout_bytes),
            Ok(_) => Err(WorkspaceError::NotFound {
                path: path.to_owned(),
                rev: rev.to_owned(),
            }),
            Err(WorkspaceError::Failed(detail)) => {
                if detail.contains("does not exist")
                    || detail.contains("exists on disk, but not in")
                    || detail.contains("unknown revision")
                    || detail.contains("invalid object name")
                {
                    Err(WorkspaceError::NotFound {
                        path: path.to_owned(),
                        rev: rev.to_owned(),
                    })
                } else {
                    Err(WorkspaceError::Failed(detail))
                }
            }
            Err(other) => Err(other),
        }
    }

    fn read_files(
        &self,
        repo: &Path,
        rev: &str,
        paths: &[String],
        max_file_bytes: u64,
        max_total_bytes: usize,
        cancel: &Cancel,
    ) -> std::result::Result<Vec<FileRead>, WorkspaceError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }

        let (mut outcomes, candidates) = batch_candidates(rev, paths);
        if candidates.is_empty() {
            return Ok(paths
                .iter()
                .cloned()
                .zip(outcomes)
                .map(|(path, outcome)| FileRead { path, outcome })
                .collect());
        }

        let input = batch_input(candidates.iter().map(|(_, spec)| spec.as_str()));
        let check = CommandSpec::new(&self.program)
            .args([
                "cat-file",
                "--batch-check=%(objectname) %(objecttype) %(objectsize)",
            ])
            .current_dir(repo)
            .stdin_bytes(input);
        let checked = self
            .runner
            .run_checked(&check, cancel)
            .map_err(ProcessError::into_workspace)?;
        if checked.stdout_truncated {
            return Err(WorkspaceError::Failed(
                "Git's object metadata exceeded the bounded output limit; reduce the context file set and retry"
                    .to_owned(),
            ));
        }
        let (content, content_cap) = inspect_batch_metadata(
            &candidates,
            &checked.stdout,
            max_file_bytes,
            max_total_bytes,
            &mut outcomes,
        )?;

        if !content.is_empty() {
            let input = batch_input(content.iter().map(|(_, spec, _)| *spec));
            let command = CommandSpec::new(&self.program)
                .args(["cat-file", "--batch"])
                .current_dir(repo)
                .stdin_bytes(input)
                .output_cap(content_cap.max(1024));
            let output = self
                .runner
                .run_checked(&command, cancel)
                .map_err(ProcessError::into_workspace)?;
            if output.stdout_truncated {
                return Err(WorkspaceError::Failed(
                    "Git's bounded object batch was truncated; lower `llm.max_file_bytes` and retry"
                        .to_owned(),
                ));
            }
            parse_batch_content(&output.stdout_bytes, &content, &mut outcomes)?;
        }

        Ok(paths
            .iter()
            .cloned()
            .zip(outcomes)
            .map(|(path, outcome)| FileRead { path, outcome })
            .collect())
    }

    fn list_files(
        &self,
        repo: &Path,
        rev: &str,
        cancel: &Cancel,
    ) -> std::result::Result<Vec<String>, WorkspaceError> {
        let (ok, stdout, stderr) =
            self.run_in(repo, &["ls-tree", "-r", "--name-only", rev], cancel)?;
        if !ok {
            return Err(WorkspaceError::Failed(stderr.trim().to_owned()));
        }
        Ok(stdout
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn ignored_paths(
        &self,
        repo: &Path,
        rev: &str,
        paths: &[String],
        cancel: &Cancel,
    ) -> std::result::Result<Vec<String>, WorkspaceError> {
        // `--no-index` is essential: without it git suppresses tracked files from
        // `check-ignore`, which would make committing a secret disable the guardrail.
        // One quiet call per path avoids parsing git's quoted pathname output. IR-17
        // owns batching this IO after measuring it; correctness comes first here.
        let tree = self.ignore_tree(repo, rev, cancel)?;
        let work_tree = format!("--work-tree={}", tree.path.to_string_lossy());
        let mut ignored = Vec::new();
        for path in paths {
            let command = CommandSpec::new(&self.program)
                .args([&work_tree, "check-ignore", "--quiet", "--no-index", "--"])
                .arg(path)
                .current_dir(repo);
            let output = self
                .runner
                .run(&command, cancel)
                .map_err(ProcessError::into_workspace)?;
            if output.success() {
                ignored.push(path.clone());
            } else if output.code() != Some(1) {
                return Err(WorkspaceError::Failed(format!(
                    "could not evaluate repository ignore rules: {}",
                    output.stderr_tail()
                )));
            }
        }
        Ok(ignored)
    }

    fn list(&self) -> std::result::Result<Vec<WorkspaceEntry>, WorkspaceError> {
        self.list_workspaces()
    }

    fn remove(
        &self,
        repo: &crate::domain::repo::RepoId,
        number: u64,
        cancel: &Cancel,
    ) -> std::result::Result<(), WorkspaceError> {
        let path = self.worktree_path(repo, number).ok_or_else(|| {
            WorkspaceError::Failed("no worktree directory is configured".to_owned())
        })?;
        let store = self.store_path(repo).ok_or_else(|| {
            WorkspaceError::Failed("no worktree directory is configured".to_owned())
        })?;
        let _lock = self.lock_workspace(&repo.storage_key(), cancel, true)?;
        if !path.exists() {
            let legacy = self
                .worktrees_root()?
                .join(repo.dir_name())
                .join(format!("pr-{number}"));
            if legacy.exists() {
                return match worktree::legacy_owner_path(&legacy) {
                    Some(owner_path) => Err(WorkspaceError::LegacyWorkspace {
                        path: legacy,
                        owner_path,
                    }),
                    None => Err(WorkspaceError::Failed(format!(
                        "legacy workspace at {} has unreadable Git ownership metadata; inspect {} and remove it with the repository that owns it",
                        legacy.display(),
                        legacy.join(".git").display()
                    ))),
                };
            }
            return Ok(());
        }
        self.drop_worktree(&store, &path, cancel)?;
        self.delete_head_ref(&store, repo, number, cancel);
        Ok(())
    }
}

fn batch_input<'a>(specs: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    let mut input = Vec::new();
    for spec in specs {
        input.extend_from_slice(spec.as_bytes());
        input.push(b'\n');
    }
    input
}

fn batch_candidates(rev: &str, paths: &[String]) -> (Vec<FileReadOutcome>, Vec<(usize, String)>) {
    // The line-oriented protocol cannot represent a literal newline in an object
    // expression. Fail only that path closed rather than changing what Git reads.
    let outcomes = paths
        .iter()
        .map(|path| {
            if path.contains(['\n', '\r']) {
                FileReadOutcome::Unreadable {
                    reason: "the path contains a newline unsupported by Git's batch protocol"
                        .to_owned(),
                }
            } else {
                FileReadOutcome::Missing
            }
        })
        .collect();
    let candidates = paths
        .iter()
        .enumerate()
        .filter(|(_, path)| !path.contains(['\n', '\r']))
        .map(|(index, path)| (index, format!("{rev}:{path}")))
        .collect();
    (outcomes, candidates)
}

type BatchObject<'a> = (usize, &'a str, usize);
type BatchPlan<'a> = (Vec<BatchObject<'a>>, usize);

fn inspect_batch_metadata<'a>(
    candidates: &'a [(usize, String)],
    metadata: &str,
    max_file_bytes: u64,
    max_total_bytes: usize,
    outcomes: &mut [FileReadOutcome],
) -> Result<BatchPlan<'a>, WorkspaceError> {
    let metadata = metadata.lines().collect::<Vec<_>>();
    if metadata.len() != candidates.len() {
        return Err(WorkspaceError::Failed(format!(
            "Git returned metadata for {} of {} requested objects; retry the context gather",
            metadata.len(),
            candidates.len()
        )));
    }
    let mut content = Vec::new();
    let mut cap = 0usize;
    let mut retained = 0usize;
    for ((index, spec), line) in candidates.iter().zip(metadata) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.last() == Some(&"missing") {
            continue;
        }
        let (Some(kind), Some(size)) = (
            fields.get(fields.len().saturating_sub(2)),
            fields.last().and_then(|size| size.parse::<u64>().ok()),
        ) else {
            outcomes[*index] = FileReadOutcome::Unreadable {
                reason: "Git returned malformed object metadata".to_owned(),
            };
            continue;
        };
        if *kind != "blob" {
            outcomes[*index] = FileReadOutcome::NotBlob {
                kind: (*kind).to_owned(),
            };
        } else if size > max_file_bytes {
            outcomes[*index] = FileReadOutcome::Oversize { bytes: size };
        } else {
            let size = usize::try_from(size).unwrap_or(usize::MAX);
            if retained.saturating_add(size) > max_total_bytes {
                outcomes[*index] = FileReadOutcome::BudgetExceeded {
                    bytes: u64::try_from(size).unwrap_or(u64::MAX),
                };
            } else {
                retained = retained.saturating_add(size);
                cap = cap.saturating_add(size).saturating_add(128);
                content.push((*index, spec.as_str(), size));
            }
        }
    }
    Ok((content, cap))
}

fn parse_batch_content(
    output: &[u8],
    requested: &[(usize, &str, usize)],
    outcomes: &mut [FileReadOutcome],
) -> Result<(), WorkspaceError> {
    let mut cursor = 0usize;
    for (index, _, expected_size) in requested {
        let Some(end) = output[cursor..].iter().position(|byte| *byte == b'\n') else {
            return Err(WorkspaceError::Failed(
                "Git ended an object batch before its header; retry the context gather".to_owned(),
            ));
        };
        let header_end = cursor + end;
        let header = std::str::from_utf8(&output[cursor..header_end]).map_err(|_| {
            WorkspaceError::Failed("Git returned a non-UTF-8 object header".to_owned())
        })?;
        let actual_size = header
            .split_whitespace()
            .last()
            .and_then(|size| size.parse::<usize>().ok())
            .ok_or_else(|| {
                WorkspaceError::Failed("Git returned a malformed object header".to_owned())
            })?;
        if actual_size != *expected_size {
            return Err(WorkspaceError::Failed(
                "Git changed an object between metadata and content reads; retry the context gather"
                    .to_owned(),
            ));
        }
        cursor = header_end.saturating_add(1);
        let content_end = cursor.saturating_add(actual_size);
        let Some(bytes) = output.get(cursor..content_end) else {
            return Err(WorkspaceError::Failed(
                "Git ended an object batch before its content; retry the context gather".to_owned(),
            ));
        };
        outcomes[*index] = FileReadOutcome::Content(bytes.to_vec());
        cursor = content_end;
        if output.get(cursor) != Some(&b'\n') {
            return Err(WorkspaceError::Failed(
                "Git returned malformed object framing; retry the context gather".to_owned(),
            ));
        }
        cursor += 1;
    }
    Ok(())
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
        // `git rev-parse --show-toplevel` reports the *resolved* path, and on macOS
        // the temporary directory is reached through a symlink (`/var` →
        // `/private/var`), so the comparison is on the canonical form. The adapter is
        // right to report what git says.
        let reported = info.root.clone().unwrap_or_default();
        let expected = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let reported_canonical =
            std::fs::canonicalize(&reported).unwrap_or_else(|_| reported.clone());
        assert_eq!(reported_canonical, expected);
        assert!(reported.join(".git").exists(), "and it is the fixture");
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
