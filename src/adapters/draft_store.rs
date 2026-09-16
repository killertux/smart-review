//! Review drafts on disk (FR-6.1, NFR-4.1).
//!
//! Layout: `<home>/drafts/<host>/<owner>/<name>/pr-<N>.json` — one document per pull
//! request, keyed exactly as the diff and the chat are, so the three agree about what
//! "this pull request" means.
//!
//! Not under `cache/`, deliberately: a draft is the one thing here that cannot be
//! fetched again, so it must not share a directory with things that are expected to be
//! evicted. It is written through the same atomic replace as the credentials file
//! (NFR-4.1), because a half-written draft is a review the user has to reconstruct by
//! hand.

use std::path::{Path, PathBuf};

use crate::adapters::fs::{create_private_parents, write_atomic};
use crate::domain::draft::Draft;
use crate::domain::repo::RepoId;
use crate::logging::{self, Level};
use crate::ports::draft::{DraftStoreError, DraftStorePort};

/// Drafts under a directory.
#[derive(Debug, Clone)]
pub struct FileDraftStore {
    root: PathBuf,
}

impl FileDraftStore {
    /// A store rooted at `<home>/drafts`.
    #[must_use]
    pub fn new(home: impl AsRef<Path>) -> Self {
        Self {
            root: home.as_ref().join("drafts"),
        }
    }

    /// The directory holding one repository's drafts.
    fn repo_dir(&self, repo: &RepoId) -> PathBuf {
        let mut path = self.root.clone();
        for part in repo.key().split('/') {
            path.push(part);
        }
        path
    }

    /// The file holding one pull request's draft.
    fn path(&self, repo: &RepoId, number: u64) -> PathBuf {
        self.repo_dir(repo).join(format!("pr-{number}.json"))
    }

    fn lock(&self, repo: &RepoId, number: u64) -> Result<std::fs::File, DraftStoreError> {
        use fs2::FileExt as _;

        let directory = self.repo_dir(repo);
        create_private_parents(&directory).map_err(|source| DraftStoreError::Io {
            action: "create the draft directory",
            path: directory.clone(),
            source,
        })?;
        let path = directory.join(format!(".pr-{number}.lock"));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| DraftStoreError::Io {
                action: "open the draft lock",
                path: path.clone(),
                source,
            })?;
        file.lock_exclusive()
            .map_err(|source| DraftStoreError::Io {
                action: "lock the draft",
                path,
                source,
            })?;
        Ok(file)
    }

    fn remove_path(path: &Path) -> Result<(), DraftStoreError> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(DraftStoreError::Io {
                action: "remove the draft",
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

impl DraftStorePort for FileDraftStore {
    fn load(&self, repo: &RepoId, number: u64) -> Result<Option<Draft>, DraftStoreError> {
        let path = self.path(repo, number);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(DraftStoreError::Io {
                    action: "read the draft",
                    path,
                    source,
                });
            }
        };
        Draft::from_json(&text)
            .map(Some)
            .map_err(|error| DraftStoreError::Malformed {
                path,
                reason: error.to_string(),
            })
    }

    fn save(&self, repo: &RepoId, draft: &Draft) -> Result<(), DraftStoreError> {
        let _lock = self.lock(repo, draft.pr)?;
        let path = self.path(repo, draft.pr);
        let text = draft
            .to_json()
            .map_err(|error| DraftStoreError::Malformed {
                path: path.clone(),
                reason: error.to_string(),
            })?;
        write_atomic(&path, &text).map_err(|source| DraftStoreError::Io {
            action: "write the draft",
            path,
            source,
        })
    }

    fn remove(&self, repo: &RepoId, number: u64) -> Result<(), DraftStoreError> {
        let path = self.path(repo, number);
        let _lock = self.lock(repo, number)?;
        Self::remove_path(&path)
    }

    fn remove_if_matches(&self, repo: &RepoId, submitted: &Draft) -> Result<bool, DraftStoreError> {
        let path = self.path(repo, submitted.pr);
        let _lock = self.lock(repo, submitted.pr)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => {
                return Err(DraftStoreError::Io {
                    action: "read the draft",
                    path,
                    source,
                });
            }
        };
        let current = Draft::from_json(&text).map_err(|error| DraftStoreError::Malformed {
            path: path.clone(),
            reason: error.to_string(),
        })?;
        if current != *submitted {
            return Ok(false);
        }
        Self::remove_path(&path)?;
        Ok(true)
    }

    fn list(&self, repo: &RepoId) -> Result<Vec<Draft>, DraftStoreError> {
        let dir = self.repo_dir(repo);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(DraftStoreError::Io {
                    action: "list drafts",
                    path: dir,
                    source,
                });
            }
        };
        let mut drafts = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match Draft::from_json(&text) {
                // An empty draft is not work in progress, so it is not listed: the
                // file may be left over from a build that kept one per pull request.
                Ok(draft) if draft.is_empty() => {}
                Ok(draft) => drafts.push(draft),
                // A draft this build cannot read is reported and skipped rather than
                // failing the whole list: the others are still the user's work, and
                // the file is left exactly as it is for whoever can read it.
                Err(error) => {
                    logging::log(Level::Warn, format!("skipping {}: {error}", path.display()));
                }
            }
        }
        drafts.sort_by_key(|draft| draft.pr);
        Ok(drafts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::draft::{Decision, DraftComment, Side};
    use crate::domain::time::from_unix_secs;
    use crate::test_support::temp_home;

    fn now() -> crate::domain::time::Timestamp {
        from_unix_secs(1_700_000_000)
    }

    fn repo() -> RepoId {
        RepoId::parse("github.com/acme/service").expect("a repository")
    }

    fn staged(number: u64) -> Draft {
        let mut draft = Draft::new(number, now());
        draft.set_decision(Some(Decision::RequestChanges), now());
        draft.set_body("One thing to fix.", now());
        draft.add(
            DraftComment::new("src/a.rs", Side::New, 31, Some(28), "this rounds up")
                .expect("valid"),
            now(),
        );
        draft
    }

    #[test]
    fn a_draft_that_was_never_written_is_not_an_error() {
        let home = temp_home();
        let store = FileDraftStore::new(home.path());
        assert_eq!(store.load(&repo(), 141).expect("readable"), None);
        assert!(store.list(&repo()).expect("listable").is_empty());
        // Removing nothing is nothing to worry about either: `:draft clear` on an
        // empty draft means what it says.
        store.remove(&repo(), 141).expect("removable");
    }

    #[test]
    fn a_draft_survives_the_round_trip() {
        let home = temp_home();
        let store = FileDraftStore::new(home.path());
        let draft = staged(141);
        store.save(&repo(), &draft).expect("writable");
        assert_eq!(store.load(&repo(), 141).expect("readable"), Some(draft));
    }

    #[test]
    fn drafts_are_per_pull_request_and_per_repository() {
        let home = temp_home();
        let store = FileDraftStore::new(home.path());
        let mut other = staged(142);
        other.set_body("A different review.", now());
        store.save(&repo(), &staged(141)).expect("writable");
        store.save(&repo(), &other).expect("writable");
        let elsewhere = RepoId::parse("github.com/other/thing").expect("a repository");
        store.save(&elsewhere, &staged(141)).expect("writable");

        assert_eq!(
            store.load(&repo(), 141).expect("readable").map(|d| d.body),
            Some(Some("One thing to fix.".to_owned()))
        );
        assert_eq!(
            store.load(&repo(), 142).expect("readable").map(|d| d.body),
            Some(Some("A different review.".to_owned()))
        );
        assert_eq!(
            store.list(&repo()).expect("listable").len(),
            2,
            "both pull requests of this repository, and none of the other"
        );
    }

    #[test]
    fn the_document_sits_where_the_readme_says() {
        let home = temp_home();
        let store = FileDraftStore::new(home.path());
        store.save(&repo(), &staged(141)).expect("writable");
        let expected = home
            .path()
            .join("drafts/github.com/acme/service/pr-141.json");
        assert!(expected.exists(), "{}", expected.display());
        assert!(
            !home.path().join("cache").exists(),
            "not under cache: a draft cannot be fetched again"
        );
    }

    #[test]
    fn a_malformed_draft_is_reported_with_its_path() {
        let home = temp_home();
        let store = FileDraftStore::new(home.path());
        home.write("drafts/github.com/acme/service/pr-141.json", "{ oops");
        let error = store.load(&repo(), 141).expect_err("reported");
        assert!(matches!(error, DraftStoreError::Malformed { .. }));
        assert!(error.to_string().contains("pr-141.json"), "{error}");
    }

    #[test]
    fn a_malformed_draft_does_not_hide_the_others() {
        let home = temp_home();
        let store = FileDraftStore::new(home.path());
        store.save(&repo(), &staged(142)).expect("writable");
        home.write("drafts/github.com/acme/service/pr-141.json", "{ oops");
        let drafts = store.list(&repo()).expect("listable");
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].pr, 142);
    }

    #[test]
    fn removing_a_draft_removes_it() {
        let home = temp_home();
        let store = FileDraftStore::new(home.path());
        store.save(&repo(), &staged(141)).expect("writable");
        store.remove(&repo(), 141).expect("removable");
        assert_eq!(store.load(&repo(), 141).expect("readable"), None);
    }

    #[test]
    fn ir_07_a_submitted_snapshot_never_removes_newer_draft_writing() {
        let home = temp_home();
        let store = FileDraftStore::new(home.path());
        let submitted = staged(141);
        let mut newer = submitted.clone();
        newer.set_body("newer words", now());
        store.save(&repo(), &newer).expect("writes newer draft");

        assert!(
            !store
                .remove_if_matches(&repo(), &submitted)
                .expect("compares")
        );
        assert_eq!(store.load(&repo(), 141).expect("loads"), Some(newer));
    }

    #[test]
    fn a_draft_from_a_newer_build_is_reported_rather_than_read() {
        let home = temp_home();
        let store = FileDraftStore::new(home.path());
        home.write(
            "drafts/github.com/acme/service/pr-141.json",
            r#"{"version": 99, "pr": 141, "updated_at": "2023-11-14T22:13:20Z"}"#,
        );
        let error = store.load(&repo(), 141).expect_err("refused");
        assert!(error.to_string().contains("newer version"), "{error}");
    }
}
