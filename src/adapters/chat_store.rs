//! Chat sessions on disk (FR-5.1, DEC-9).
//!
//! Layout: `<home>/chats/<host>/<owner>/<name>/pr-<N>/<id>.json` for the
//! conversation, and `index.json` beside it for the list. The index is a convenience,
//! never a source of truth: it is rebuilt from the sessions whenever it is missing or
//! unreadable, because a cache that cannot be trusted to describe itself is a cache
//! that will eventually disagree with its own contents.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::adapters::fs::write_atomic;
use crate::domain::chat::{
    MAX_SESSION_BYTES, MAX_SESSIONS_PER_PR, Pruned, Session, SessionMeta, sessions_to_prune,
};
use crate::domain::repo::RepoId;
use crate::logging::{self, Level};
use crate::ports::chat::{ChatStoreError, ChatStorePort};

/// The file the list is kept in.
const INDEX_FILE: &str = "index.json";
/// The file whose OS advisory lock serializes chat/index updates for one PR.
const LOCK_FILE: &str = ".chat.lock";
/// Records that every validated legacy session has a durable copy.
const MIGRATION_FILE: &str = ".migrated-from-cache-v1";

/// Sessions under a directory.
#[derive(Debug, Clone)]
pub struct FileChatStore {
    root: PathBuf,
    legacy_root: PathBuf,
}

/// The index document.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    /// Document version, for migrations.
    #[serde(default)]
    version: u32,
    /// One row per session.
    #[serde(default)]
    sessions: Vec<SessionMeta>,
}

impl FileChatStore {
    /// A store rooted at `<home>/chats`.
    #[must_use]
    pub fn new(home: impl AsRef<Path>) -> Self {
        Self {
            root: home.as_ref().join("chats"),
            legacy_root: home.as_ref().join("cache/chat"),
        }
    }

    /// Migrates every readable legacy conversation before `cache/` can be evicted.
    ///
    /// Invalid legacy files are retained and logged. A valid session is migrated by its
    /// own declared repository and PR, never by guessing from an arbitrary path.
    ///
    /// # Errors
    ///
    /// Returns an error when a valid legacy document cannot be safely copied or
    /// reconciled with its durable counterpart.
    pub fn migrate_all(&self) -> Result<(), ChatStoreError> {
        let mut subjects = BTreeSet::new();
        for path in json_files_under(&self.legacy_root) {
            if path.file_name().and_then(std::ffi::OsStr::to_str) == Some(INDEX_FILE) {
                continue;
            }
            let session = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Session>(&text).ok());
            let Some(session) = session else {
                logging::log(
                    Level::Warn,
                    format!(
                        "chat: retaining unreadable legacy session {}",
                        path.display()
                    ),
                );
                continue;
            };
            let repo = match repo_of(&session) {
                Ok(repo) => repo,
                Err(error) => {
                    logging::log(
                        Level::Warn,
                        format!("chat: retaining legacy session {}: {error}", path.display()),
                    );
                    continue;
                }
            };
            subjects.insert((repo, session.pr));
        }
        for (repo, pr) in subjects {
            let _lock = self.lock(&repo, pr)?;
            self.migrate_legacy(&repo, pr)?;
        }
        Ok(())
    }

    /// The directory holding one pull request's sessions.
    fn pr_dir(&self, repo: &RepoId, pr: u64) -> PathBuf {
        let mut path = self.root.clone();
        for part in repo.key().split('/') {
            path.push(part);
        }
        path.join(format!("pr-{pr}"))
    }

    fn legacy_pr_dir(&self, repo: &RepoId, pr: u64) -> PathBuf {
        let mut path = self.legacy_root.clone();
        for part in repo.key().split('/') {
            path.push(part);
        }
        path.join(format!("pr-{pr}"))
    }

    fn migration_path(&self, repo: &RepoId, pr: u64) -> PathBuf {
        self.pr_dir(repo, pr).join(MIGRATION_FILE)
    }

    fn migrate_session(
        &self,
        repo: &RepoId,
        pr: u64,
        session: Session,
    ) -> Result<(), ChatStoreError> {
        let destination = self.session_path(repo, pr, &session.id);
        let destination_session = std::fs::read_to_string(&destination)
            .ok()
            .and_then(|body| serde_json::from_str::<Session>(&body).ok());
        let chosen = match destination_session {
            Some(existing) if existing == session => existing,
            Some(existing) if existing.updated_at > session.updated_at => existing,
            Some(existing) if existing.updated_at == session.updated_at => {
                logging::log(
                    Level::Warn,
                    format!(
                        "chat: migration conflict for {}; retaining both locations for recovery",
                        destination.display()
                    ),
                );
                return Err(ChatStoreError::Conflict);
            }
            _ => {
                let body =
                    serde_json::to_string_pretty(&session).map_err(|error| ChatStoreError::Io {
                        action: "serialise a migrated chat session".to_owned(),
                        path: destination.display().to_string(),
                        cause: error.to_string(),
                    })?;
                write_atomic(&destination, &body).map_err(|error| ChatStoreError::Io {
                    action: "migrate a chat session".to_owned(),
                    path: destination.display().to_string(),
                    cause: error.to_string(),
                })?;
                session
            }
        };
        let copied = std::fs::read_to_string(&destination)
            .ok()
            .and_then(|body| serde_json::from_str::<Session>(&body).ok());
        if copied.as_ref() != Some(&chosen) {
            return Err(ChatStoreError::Io {
                action: "verify a migrated chat session".to_owned(),
                path: destination.display().to_string(),
                cause: "the copied document did not validate".to_owned(),
            });
        }
        Ok(())
    }

    /// The file holding one session.
    fn session_path(&self, repo: &RepoId, pr: u64, id: &str) -> PathBuf {
        self.pr_dir(repo, pr).join(format!("{}.json", sanitise(id)))
    }

    /// The index file for one pull request.
    fn index_path(&self, repo: &RepoId, pr: u64) -> PathBuf {
        self.pr_dir(repo, pr).join(INDEX_FILE)
    }

    fn lock(&self, repo: &RepoId, pr: u64) -> Result<DocumentLock, ChatStoreError> {
        let path = self.pr_dir(repo, pr).join(LOCK_FILE);
        if let Some(parent) = path.parent() {
            crate::adapters::fs::create_private_parents(parent).map_err(|error| {
                ChatStoreError::Io {
                    action: "create the chat directory".to_owned(),
                    path: parent.display().to_string(),
                    cause: error.to_string(),
                }
            })?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => {
                file.try_lock_exclusive().map_err(|error| {
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        ChatStoreError::Conflict
                    } else {
                        ChatStoreError::Io {
                            action: "lock the chat document".to_owned(),
                            path: path.display().to_string(),
                            cause: error.to_string(),
                        }
                    }
                })?;
                Ok(DocumentLock { file })
            }
            Err(error) => Err(ChatStoreError::Io {
                action: "lock the chat document".to_owned(),
                path: path.display().to_string(),
                cause: error.to_string(),
            }),
        }
    }

    /// Copies validated legacy sessions before reading the durable location (IR-06).
    /// The legacy files remain in place so an interrupted migration can retry safely.
    fn migrate_legacy(&self, repo: &RepoId, pr: u64) -> Result<(), ChatStoreError> {
        if self.migration_path(repo, pr).exists() {
            return Ok(());
        }
        let legacy = self.legacy_pr_dir(repo, pr);
        let entries = match std::fs::read_dir(&legacy) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(ChatStoreError::Io {
                    action: "read legacy chat sessions".to_owned(),
                    path: legacy.display().to_string(),
                    cause: error.to_string(),
                });
            }
        };
        let mut complete = true;
        for entry in entries.flatten() {
            let source = entry.path();
            if source.extension().and_then(std::ffi::OsStr::to_str) != Some("json")
                || source.file_name().and_then(std::ffi::OsStr::to_str) == Some(INDEX_FILE)
            {
                continue;
            }
            let text = match std::fs::read_to_string(&source) {
                Ok(text) => text,
                Err(error) => {
                    logging::log(
                        Level::Warn,
                        format!("chat: cannot migrate {}: {error}", source.display()),
                    );
                    complete = false;
                    continue;
                }
            };
            let session = match serde_json::from_str::<Session>(&text) {
                Ok(session) => session,
                Err(error) => {
                    logging::log(
                        Level::Warn,
                        format!(
                            "chat: retaining unreadable legacy session {}: {error}",
                            source.display()
                        ),
                    );
                    complete = false;
                    continue;
                }
            };
            if session.repo != repo.key() || session.pr != pr {
                logging::log(
                    Level::Warn,
                    format!(
                        "chat: retaining misplaced legacy session {}",
                        source.display()
                    ),
                );
                complete = false;
                continue;
            }
            self.migrate_session(repo, pr, session)?;
        }
        if complete {
            let marker = self.migration_path(repo, pr);
            write_atomic(&marker, "migrated").map_err(|error| ChatStoreError::Io {
                action: "mark chat migration complete".to_owned(),
                path: marker.display().to_string(),
                cause: error.to_string(),
            })?;
        }
        Ok(())
    }

    /// Reconciles the index against every durable session document.
    ///
    /// The index is only a projection: a crash after publishing a session but before
    /// replacing this file must not hide the newly saved conversation (IR-06).
    fn index(&self, repo: &RepoId, pr: u64) -> Vec<SessionMeta> {
        let path = self.index_path(repo, pr);
        let existing = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Index>(&text).ok())
            .map(|index| index.sessions);
        let metas = self.rebuild_index(repo, pr);
        if existing.as_ref() == Some(&metas) {
            return metas;
        }
        // Writing it back is best-effort: a read that succeeded must not fail because
        // its disposable projection could not be refreshed.
        if let Ok(text) = serde_json::to_string_pretty(&Index {
            version: 1,
            sessions: metas.clone(),
        }) {
            let _ = write_atomic(&path, &text);
        }
        metas
    }

    /// Reads every session in the directory, which is how the index is rebuilt.
    ///
    /// Never fails: a directory that cannot be listed has no sessions in it as far as
    /// the caller is concerned, and an unreadable file is logged and skipped.
    fn rebuild_index(&self, repo: &RepoId, pr: u64) -> Vec<SessionMeta> {
        let dir = self.pr_dir(repo, pr);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut metas = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("json")
                || path.file_name().and_then(std::ffi::OsStr::to_str) == Some(INDEX_FILE)
            {
                continue;
            }
            match std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|text| {
                    serde_json::from_str::<Session>(&text).map_err(|error| error.to_string())
                }) {
                Ok(session) => metas.push(SessionMeta::of(&session)),
                Err(reason) => logging::log(
                    Level::Debug,
                    format!(
                        "chat: skipping unreadable session {}: {reason}",
                        path.display()
                    ),
                ),
            }
        }
        metas.sort_by_key(|meta| std::cmp::Reverse((meta.updated_at, meta.id.clone())));
        metas
    }

    /// Writes the index, replacing the row for `session` when there is one.
    fn update_index(&self, repo: &RepoId, session: &Session) -> Result<(), ChatStoreError> {
        let mut index = self.index(repo, session.pr);
        let meta = SessionMeta::of(session);
        index.retain(|row| row.id != meta.id);
        index.push(meta);
        index.sort_by_key(|row| std::cmp::Reverse((row.updated_at, row.id.clone())));
        let path = self.index_path(repo, session.pr);
        let text = serde_json::to_string_pretty(&Index {
            version: 1,
            sessions: index,
        })
        .map_err(|error| ChatStoreError::Io {
            action: "serialise the chat index".to_owned(),
            path: path.display().to_string(),
            cause: error.to_string(),
        })?;
        write_atomic(&path, &text).map_err(|error| ChatStoreError::Io {
            action: "write the chat index".to_owned(),
            path: path.display().to_string(),
            cause: error.to_string(),
        })
    }
}

fn json_files_under(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(json_files_under(&path));
        } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("json") {
            files.push(path);
        }
    }
    files
}

/// The repository a session names, as a `RepoId`.
///
/// The session stores what the store needs to place it, so the two cannot disagree
/// about where a conversation belongs.
fn repo_of(session: &Session) -> Result<RepoId, ChatStoreError> {
    RepoId::parse(&session.repo).map_err(|error| ChatStoreError::Io {
        action: "place the session".to_owned(),
        path: session.repo.clone(),
        cause: error.to_string(),
    })
}

/// Keeps a session id safe to use as a file name.
///
/// The ids this build writes are hex, but a hand-edited file, an older version or a
/// future one may not be, and a `../` in a file name is a bug that writes outside the
/// directory it was given.
fn sanitise(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

impl ChatStorePort for FileChatStore {
    fn list(&self, repo: &RepoId, pr: u64) -> Result<Vec<SessionMeta>, ChatStoreError> {
        self.migrate_legacy(repo, pr)?;
        Ok(self.index(repo, pr))
    }

    fn load(&self, repo: &RepoId, pr: u64, id: &str) -> Result<Option<Session>, ChatStoreError> {
        self.migrate_legacy(repo, pr)?;
        let path = self.session_path(repo, pr, id);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| ChatStoreError::Malformed {
                path: path.display().to_string(),
                reason: error.to_string(),
            })
    }

    fn latest(&self, repo: &RepoId, pr: u64) -> Result<Option<Session>, ChatStoreError> {
        self.migrate_legacy(repo, pr)?;
        let Some(newest) = self.index(repo, pr).into_iter().next() else {
            return Ok(None);
        };
        self.load(repo, pr, &newest.id)
    }

    fn put(&self, session: &Session) -> Result<(), ChatStoreError> {
        // DEC-9's per-session cap is checked before the write, and the message names
        // the way out: starting another session, or exporting this one.
        let bytes = session.bytes();
        if bytes > MAX_SESSION_BYTES {
            return Err(ChatStoreError::Full {
                limit: crate::domain::context::human_bytes(MAX_SESSION_BYTES as u64),
            });
        }
        let repo = repo_of(session)?;
        let _lock = self.lock(&repo, session.pr)?;
        self.migrate_legacy(&repo, session.pr)?;
        let path = self.session_path(&repo, session.pr, &session.id);
        let text = serde_json::to_string_pretty(session).map_err(|error| ChatStoreError::Io {
            action: "serialise the session".to_owned(),
            path: path.display().to_string(),
            cause: error.to_string(),
        })?;
        write_atomic(&path, &text).map_err(|error| ChatStoreError::Io {
            action: "write the session".to_owned(),
            path: path.display().to_string(),
            cause: error.to_string(),
        })?;
        self.update_index(&repo, session)
    }

    fn remove(&self, repo: &RepoId, pr: u64, id: &str) -> Result<(), ChatStoreError> {
        let _lock = self.lock(repo, pr)?;
        self.migrate_legacy(repo, pr)?;
        let path = self.session_path(repo, pr, id);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| ChatStoreError::Io {
                action: "remove the session".to_owned(),
                path: path.display().to_string(),
                cause: error.to_string(),
            })?;
        }
        let index_path = self.index_path(repo, pr);
        let mut index = self.index(repo, pr);
        index.retain(|row| row.id != id);
        let text = serde_json::to_string_pretty(&Index {
            version: 1,
            sessions: index,
        })
        .unwrap_or_else(|_| "{}".to_owned());
        write_atomic(&index_path, &text).map_err(|error| ChatStoreError::Io {
            action: "write the chat index".to_owned(),
            path: index_path.display().to_string(),
            cause: error.to_string(),
        })
    }

    fn prune(&self, repo: &RepoId, pr: u64) -> Result<Pruned, ChatStoreError> {
        self.migrate_legacy(repo, pr)?;
        let metas = self.index(repo, pr);
        let doomed = sessions_to_prune(metas.clone(), MAX_SESSIONS_PER_PR);
        let mut removed = Vec::new();
        for id in doomed {
            // A session that is already gone is not a failure: the point is that it is
            // not there.
            match self.remove(repo, pr, &id) {
                Ok(()) => removed.push(id),
                Err(error) => {
                    logging::log(Level::Warn, format!("chat: could not prune {id}: {error}"));
                    return Err(error);
                }
            }
        }
        if !removed.is_empty() {
            logging::log(
                Level::Info,
                format!("chat: pruned {} sessions for PR {pr}", removed.len()),
            );
        }
        Ok(Pruned {
            removed,
            over_bytes: false,
        })
    }
}

/// The OS releases this advisory lock when the file closes, including after a process
/// crash. The lock file itself is merely a stable lock target, not an ownership marker.
struct DocumentLock {
    file: std::fs::File,
}

impl Drop for DocumentLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::chat::{Message, Role};
    use crate::test_support::temp_home;

    fn repo() -> RepoId {
        RepoId::parse("github.com/acme/service").expect("valid")
    }

    fn session(id: &str, at: u64) -> Session {
        let mut session = Session::new(
            id,
            "github.com/acme/service",
            141,
            "ba6c89f0a1b2c3d4",
            "deepseek/deepseek-v4-pro",
            None,
            at,
        );
        session.messages = vec![
            Message::user(format!("question from {id}"), at),
            Message::assistant("an answer", at, None, Vec::new()),
        ];
        session
    }

    fn store() -> (FileChatStore, crate::test_support::TempHome) {
        let home = temp_home();
        let store = FileChatStore::new(home.path());
        (store, home)
    }

    #[test]
    fn a_session_round_trips_and_appears_in_the_list() {
        let (store, _home) = store();
        let session = session("100-0", 100);
        store.put(&session).expect("writes");

        let found = store
            .load(&repo(), 141, "100-0")
            .expect("reads")
            .expect("there");
        assert_eq!(found, session);
        let listed = store.list(&repo(), 141).expect("lists");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "100-0");
        assert_eq!(listed[0].turns, 1);
        assert_eq!(listed[0].messages, 2);
        assert_eq!(listed[0].model, "deepseek/deepseek-v4-pro");
    }

    #[test]
    fn sessions_are_scoped_to_their_pull_request_and_repository() {
        let (store, _home) = store();
        store.put(&session("100-0", 100)).expect("writes");
        let other_pr = RepoId::parse("github.com/acme/other").expect("valid");
        // Both a different pull request number and a different repository are
        // different conversations.
        assert!(store.list(&repo(), 142).unwrap().is_empty());
        assert!(store.list(&other_pr, 141).unwrap().is_empty());
        assert!(store.load(&repo(), 141, "100-0").unwrap().is_some());
    }

    #[test]
    fn the_list_is_newest_first() {
        let (store, _home) = store();
        for (id, at) in [("1-0", 10), ("2-0", 20), ("3-0", 15)] {
            store.put(&session(id, at)).expect("writes");
        }
        let listed: Vec<String> = store
            .list(&repo(), 141)
            .unwrap()
            .into_iter()
            .map(|meta| meta.id)
            .collect();
        assert_eq!(listed, vec!["2-0", "3-0", "1-0"]);
    }

    #[test]
    fn a_lost_index_is_rebuilt_from_the_sessions() {
        let (store, home) = store();
        store.put(&session("1-0", 10)).expect("writes");
        store.put(&session("2-0", 20)).expect("writes");
        // The index is disposable; the sessions are the truth.
        std::fs::remove_file(store.index_path(&repo(), 141)).expect("removes");
        let listed = store.list(&repo(), 141).expect("rebuilds");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "2-0");
        // …and it was written back, because the rebuild is what a cache is for.
        assert!(
            home.path()
                .join("chats/github.com/acme/service/pr-141/index.json")
                .exists()
        );
    }

    #[test]
    fn an_unreadable_session_is_reported_by_load_and_skipped_by_list() {
        let (store, _home) = store();
        let dir = store.pr_dir(&repo(), 141);
        std::fs::create_dir_all(&dir).expect("creates");
        std::fs::write(dir.join("broken.json"), "{ not json").expect("writes");
        // `list` skips it: one bad file must not hide the good ones.
        let listed = store.list(&repo(), 141).expect("lists");
        assert!(listed.is_empty());
        // `load` says so: the user can see the file, so pretending it is not there
        // would be the wrong kind of quiet.
        let error = store.load(&repo(), 141, "broken").expect_err("reports");
        assert!(
            matches!(error, ChatStoreError::Malformed { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_missing_session_is_none_rather_than_an_error() {
        let (store, _home) = store();
        assert!(
            store
                .load(&repo(), 141, "nothing")
                .expect("reads")
                .is_none()
        );
        assert!(store.latest(&repo(), 141).expect("reads").is_none());
    }

    #[test]
    fn the_latest_session_is_the_newest_one() {
        let (store, _home) = store();
        store.put(&session("1-0", 10)).unwrap();
        store.put(&session("2-0", 20)).unwrap();
        let latest = store.latest(&repo(), 141).unwrap().expect("there");
        assert_eq!(latest.id, "2-0");
    }

    #[test]
    fn an_id_that_is_not_a_file_name_cannot_escape_the_directory() {
        let (store, _home) = store();
        let mut session = session("../../escape", 10);
        session.id = "../../escape".to_owned();
        store.put(&session).expect("writes");
        // The file is inside the pull request's directory — every separator replaced —
        // and the session still reads back under its own id.
        let entries: Vec<String> = std::fs::read_dir(store.pr_dir(&repo(), 141))
            .expect("lists")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != INDEX_FILE && name != LOCK_FILE)
            .collect();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert!(!entries[0].contains('/'), "{entries:?}");
        assert!(!entries[0].contains(".."), "{entries:?}");
        assert!(store.load(&repo(), 141, "../../escape").unwrap().is_some());
    }

    #[test]
    fn pruning_removes_the_oldest_and_leaves_the_newest() {
        let (store, _home) = store();
        for index in 0..(MAX_SESSIONS_PER_PR + 3) {
            let at = u64::try_from(index).expect("small");
            store
                .put(&session(&format!("{at:x}-0"), at))
                .expect("writes");
        }
        assert_eq!(
            store.list(&repo(), 141).unwrap().len(),
            MAX_SESSIONS_PER_PR + 3
        );
        let pruned = store.prune(&repo(), 141).expect("prunes");
        assert_eq!(pruned.removed.len(), 3);
        let left = store.list(&repo(), 141).unwrap();
        assert_eq!(left.len(), MAX_SESSIONS_PER_PR);
        // The newest is still there, the oldest three are not.
        assert_eq!(left[0].id, format!("{:x}-0", MAX_SESSIONS_PER_PR + 2));
        assert!(left.iter().all(|meta| meta.id != "0-0"));
        // And a second prune has nothing to do.
        assert!(
            store
                .prune(&repo(), 141)
                .expect("prunes")
                .removed
                .is_empty()
        );
    }

    #[test]
    fn a_session_over_the_cap_is_refused_with_the_way_out_named() {
        let (store, _home) = store();
        let mut big = session("1-0", 10);
        big.messages = vec![Message::assistant(
            "x".repeat(MAX_SESSION_BYTES + 1),
            10,
            None,
            Vec::new(),
        )];
        let error = store.put(&big).expect_err("refuses");
        assert!(matches!(error, ChatStoreError::Full { .. }), "{error:?}");
        assert!(error.to_string().contains(":chat new"), "{error}");
        // And nothing half-written was left behind.
        assert!(store.load(&repo(), 141, "1-0").unwrap().is_none());
    }

    #[test]
    fn removing_a_session_takes_it_out_of_the_list() {
        let (store, _home) = store();
        store.put(&session("1-0", 10)).unwrap();
        store.remove(&repo(), 141, "1-0").expect("removes");
        assert!(store.list(&repo(), 141).unwrap().is_empty());
        // Removing something that is not there is not a failure.
        store.remove(&repo(), 141, "1-0").expect("idempotent");
    }

    #[test]
    fn the_file_is_json_a_reader_can_open() {
        let (store, _home) = store();
        store.put(&session("1-0", 10)).expect("writes");
        let text = std::fs::read_to_string(store.session_path(&repo(), 141, "1-0")).expect("reads");
        assert!(text.contains("\"role\": \"user\""), "{text}");
        assert!(
            text.contains("\"repo\": \"github.com/acme/service\""),
            "{text}"
        );
        // Pretty-printed, because the user is allowed to read it.
        assert!(text.contains('\n'), "{text}");
    }

    #[test]
    fn an_append_replaces_the_document_and_keeps_the_conversation() {
        let (store, _home) = store();
        let mut session = session("1-0", 10);
        store.put(&session).expect("writes");
        session
            .messages
            .push(Message::user("a second question", 11));
        session
            .messages
            .push(Message::assistant("a second answer", 12, None, Vec::new()));
        session.updated_at = 12;
        store.put(&session).expect("writes");

        let found = store.load(&repo(), 141, "1-0").unwrap().expect("there");
        assert_eq!(found.messages.len(), 4);
        assert_eq!(found.turns(), 2);
        // One row, not two: the id is the identity.
        assert_eq!(store.list(&repo(), 141).unwrap().len(), 1);
    }

    #[test]
    fn a_session_whose_repository_is_not_a_repository_id_is_an_error() {
        let (store, _home) = store();
        let mut broken = session("1-0", 10);
        broken.repo = "not a repo".to_owned();
        let error = store.put(&broken).expect_err("refuses");
        assert!(matches!(error, ChatStoreError::Io { .. }), "{error:?}");
    }

    #[test]
    fn a_role_is_written_the_way_the_reader_expects_it() {
        let message = Message::user("hello", 1);
        let json = serde_json::to_string(&message).expect("serialises");
        assert!(json.contains("\"user\""), "{json}");
        assert_eq!(message.role, Role::User);
    }

    #[test]
    fn ir_06_migrates_legacy_sessions_before_cache_is_removed() {
        let (store, home) = store();
        let legacy = home
            .path()
            .join("cache/chat/github.com/acme/service/pr-141/1-0.json");
        std::fs::create_dir_all(legacy.parent().expect("parent")).expect("creates");
        std::fs::write(
            &legacy,
            serde_json::to_string(&session("1-0", 10)).expect("serialises"),
        )
        .expect("writes");

        assert_eq!(store.list(&repo(), 141).expect("migrates").len(), 1);
        std::fs::remove_dir_all(home.path().join("cache")).expect("removes cache");
        assert!(store.latest(&repo(), 141).expect("reads").is_some());
    }

    #[test]
    fn ir_06_reports_another_instances_chat_write_conflict() {
        let (store, _home) = store();
        let lock = store.lock(&repo(), 141).expect("acquires first lock");
        let error = store
            .put(&session("1-0", 10))
            .expect_err("reports conflict");
        assert_eq!(error, ChatStoreError::Conflict);
        drop(lock);
        store
            .put(&session("1-0", 10))
            .expect("retries after lock release");
    }

    #[test]
    fn ir_06_legacy_sessions_are_not_reimported_after_a_removal() {
        let (store, home) = store();
        let legacy = home
            .path()
            .join("cache/chat/github.com/acme/service/pr-141/1-0.json");
        std::fs::create_dir_all(legacy.parent().expect("parent")).expect("creates");
        std::fs::write(
            &legacy,
            serde_json::to_string(&session("1-0", 10)).expect("serialises"),
        )
        .expect("writes");

        store
            .remove(&repo(), 141, "1-0")
            .expect("migrates then removes");
        assert!(store.list(&repo(), 141).expect("lists").is_empty());
        assert!(store.migration_path(&repo(), 141).exists());
    }

    #[test]
    fn ir_06_a_valid_old_index_is_reconciled_with_an_unindexed_session() {
        let (store, _home) = store();
        store
            .put(&session("1-0", 10))
            .expect("writes indexed session");
        let unindexed = session("2-0", 20);
        crate::adapters::fs::write_atomic(
            &store.session_path(&repo(), 141, "2-0"),
            &serde_json::to_string(&unindexed).expect("serialises"),
        )
        .expect("publishes session before index update");

        let listed = store.list(&repo(), 141).expect("reconciles index");
        assert_eq!(
            listed
                .iter()
                .map(|meta| meta.id.as_str())
                .collect::<Vec<_>>(),
            ["2-0", "1-0"]
        );
        assert_eq!(
            store.latest(&repo(), 141).expect("reads latest"),
            Some(unindexed)
        );
    }

    #[test]
    fn ir_06_migration_keeps_the_newer_legacy_session() {
        let (store, home) = store();
        let legacy = home
            .path()
            .join("cache/chat/github.com/acme/service/pr-141/1-0.json");
        std::fs::create_dir_all(legacy.parent().expect("parent")).expect("creates");
        std::fs::write(
            &legacy,
            serde_json::to_string(&session("1-0", 20)).expect("serialises"),
        )
        .expect("writes newer legacy session");
        store
            .put(&session("1-0", 10))
            .expect("writes older durable session");
        std::fs::remove_file(store.migration_path(&repo(), 141)).expect("retries migration");

        store.migrate_legacy(&repo(), 141).expect("reconciles");
        assert_eq!(
            store
                .load(&repo(), 141, "1-0")
                .expect("reads")
                .expect("exists")
                .updated_at,
            20
        );
    }

    #[test]
    fn ir_06_startup_migration_covers_unopened_legacy_chats() {
        let (store, home) = store();
        let legacy = home
            .path()
            .join("cache/chat/github.com/acme/service/pr-999/1-0.json");
        std::fs::create_dir_all(legacy.parent().expect("parent")).expect("creates");
        let mut unopened = session("1-0", 10);
        unopened.pr = 999;
        std::fs::write(
            &legacy,
            serde_json::to_string(&unopened).expect("serialises"),
        )
        .expect("writes");

        store.migrate_all().expect("migrates all");
        std::fs::remove_dir_all(home.path().join("cache")).expect("removes cache");
        assert!(
            store
                .latest(&repo(), 999)
                .expect("reads durable chat")
                .is_some()
        );
    }
}
