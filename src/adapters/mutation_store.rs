//! Durable remote-mutation operation records (IR-07).
//!
//! Each confirmed mutation owns one document under `reviews/`, beside the durable
//! review-plan override but outside disposable cache. A record is created before its
//! process starts; later state changes use the IR-06 atomic replacement primitive.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::adapters::fs::{create_private_parents, write_atomic};
use crate::domain::mutation::{MUTATION_VERSION, MutationOperation};
use crate::domain::repo::RepoId;
use crate::ports::{MutationStoreError, MutationStorePort};

/// Operation records rooted at `<home>/reviews`.
#[derive(Debug, Clone)]
pub struct FileMutationStore {
    root: PathBuf,
}

impl FileMutationStore {
    /// Builds a store rooted at the application's durable review directory.
    #[must_use]
    pub fn new(home: impl AsRef<Path>) -> Self {
        Self {
            root: home.as_ref().join("reviews"),
        }
    }

    fn pr_dir(&self, repo: &RepoId, pr: u64) -> PathBuf {
        let mut path = self.root.clone();
        for part in repo.key().split('/') {
            path.push(part);
        }
        path.join(format!("pr-{pr}")).join("operations")
    }

    fn path(&self, operation: &MutationOperation) -> Result<PathBuf, MutationStoreError> {
        valid_id(&operation.id)
            .then(|| {
                self.pr_dir(&operation.repo, operation.pr)
                    .join(format!("{}.json", operation.id))
            })
            .ok_or_else(|| MutationStoreError::Malformed {
                path: self.pr_dir(&operation.repo, operation.pr),
                reason: "the mutation operation id contains unsafe filename characters".to_owned(),
            })
    }

    fn decode(path: PathBuf, text: &str) -> Result<MutationOperation, MutationStoreError> {
        let operation: MutationOperation =
            serde_json::from_str(text).map_err(|error| MutationStoreError::Malformed {
                path: path.clone(),
                reason: error.to_string(),
            })?;
        if operation.version > MUTATION_VERSION {
            return Err(MutationStoreError::Malformed {
                path,
                reason: format!(
                    "format {} is newer than this smart-review build supports",
                    operation.version
                ),
            });
        }
        Ok(operation)
    }

    fn unresolved_in(
        &self,
        repo: &RepoId,
        pr: u64,
    ) -> Result<Vec<MutationOperation>, MutationStoreError> {
        let dir = self.pr_dir(repo, pr);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(MutationStoreError::Io {
                    action: "list operation records",
                    path: dir,
                    source,
                });
            }
        };
        let mut operations = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| MutationStoreError::Io {
                action: "read an operation record entry",
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|source| MutationStoreError::Io {
                action: "read the operation record",
                path: path.clone(),
                source,
            })?;
            let operation = Self::decode(path, &text)?;
            if operation.repo == *repo && operation.pr == pr && operation.state.blocks_dispatch() {
                operations.push(operation);
            }
        }
        operations.sort_by_key(|operation| operation.created_at);
        Ok(operations)
    }

    fn create(&self, operation: &MutationOperation) -> Result<(), MutationStoreError> {
        let path = self.path(operation)?;
        let parent = path.parent().ok_or_else(|| MutationStoreError::Malformed {
            path: path.clone(),
            reason: "the mutation path has no parent directory".to_owned(),
        })?;
        create_private_parents(parent).map_err(|source| MutationStoreError::Io {
            action: "create the operation directory",
            path: parent.to_path_buf(),
            source,
        })?;
        let text = serde_json::to_vec_pretty(operation).map_err(|error| {
            MutationStoreError::Malformed {
                path: path.clone(),
                reason: error.to_string(),
            }
        })?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                MutationStoreError::Conflict
            } else {
                MutationStoreError::Io {
                    action: "create the operation record",
                    path: path.clone(),
                    source,
                }
            }
        })?;
        file.write_all(&text)
            .and_then(|()| file.sync_all())
            .map_err(|source| MutationStoreError::Io {
                action: "write the operation record",
                path,
                source,
            })
    }
}

impl MutationStorePort for FileMutationStore {
    fn begin(&self, operation: &MutationOperation) -> Result<(), MutationStoreError> {
        use fs2::FileExt as _;
        let dir = self.pr_dir(&operation.repo, operation.pr);
        create_private_parents(&dir).map_err(|source| MutationStoreError::Io {
            action: "create the operation directory",
            path: dir.clone(),
            source,
        })?;
        let lock_path = dir.join(".mutation.lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| MutationStoreError::Io {
                action: "open the operation lock",
                path: lock_path,
                source,
            })?;
        lock.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                MutationStoreError::Conflict
            } else {
                MutationStoreError::Io {
                    action: "lock the operation journal",
                    path: dir.clone(),
                    source: error,
                }
            }
        })?;
        let unresolved = self.unresolved_in(&operation.repo, operation.pr)?;
        if !unresolved.is_empty() {
            return Err(MutationStoreError::Conflict);
        }
        self.create(operation)
    }

    fn save(&self, operation: &MutationOperation) -> Result<(), MutationStoreError> {
        let path = self.path(operation)?;
        if !path.exists() {
            return Err(MutationStoreError::Io {
                action: "update the operation record",
                path,
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "operation not found"),
            });
        }
        let text = serde_json::to_string_pretty(operation).map_err(|error| {
            MutationStoreError::Malformed {
                path: path.clone(),
                reason: error.to_string(),
            }
        })?;
        write_atomic(&path, &text).map_err(|source| MutationStoreError::Io {
            action: "update the operation record",
            path,
            source,
        })
    }

    fn unresolved(
        &self,
        repo: &RepoId,
        pr: u64,
    ) -> Result<Vec<MutationOperation>, MutationStoreError> {
        self.unresolved_in(repo, pr)
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::mutation::{MutationKind, MutationState};
    use crate::domain::time::from_unix_secs;
    use crate::test_support::temp_home;

    fn operation() -> MutationOperation {
        MutationOperation::queued(
            "op-1".to_owned(),
            RepoId::parse("acme/service").expect("repository"),
            141,
            Some("head".to_owned()),
            MutationKind::Conversation {
                body: "please add a test".to_owned(),
            },
            from_unix_secs(1_700_000_000),
        )
    }

    #[test]
    fn ir_07_a_created_operation_survives_until_reconciliation() {
        let home = temp_home();
        let store = FileMutationStore::new(home.path());
        let mut operation = operation();
        store.begin(&operation).expect("created");
        operation.mark_dispatching(from_unix_secs(1_700_000_001));
        store.save(&operation).expect("updated");

        assert_eq!(
            store
                .unresolved(&operation.repo, operation.pr)
                .expect("listed"),
            vec![operation]
        );
    }

    #[test]
    fn ir_07_a_known_success_is_not_replayed_on_restart() {
        let home = temp_home();
        let store = FileMutationStore::new(home.path());
        let mut operation = operation();
        store.begin(&operation).expect("created");
        operation.mark_succeeded(Some(12), None, from_unix_secs(1_700_000_001));
        store.save(&operation).expect("updated");

        assert!(
            store
                .unresolved(&operation.repo, operation.pr)
                .expect("listed")
                .is_empty()
        );
        assert!(matches!(operation.state, MutationState::Succeeded { .. }));
    }

    #[test]
    fn ir_07_duplicate_operation_ids_are_refused() {
        let home = temp_home();
        let store = FileMutationStore::new(home.path());
        let operation = operation();
        store.begin(&operation).expect("created");
        assert!(matches!(
            store.begin(&operation),
            Err(MutationStoreError::Conflict)
        ));
    }

    #[test]
    fn ir_07_an_unresolved_operation_blocks_a_different_dispatch_id() {
        let home = temp_home();
        let store = FileMutationStore::new(home.path());
        let first = operation();
        store.begin(&first).expect("created");
        let mut second = operation();
        second.id = "op-2".to_owned();

        assert!(matches!(
            store.begin(&second),
            Err(MutationStoreError::Conflict)
        ));
    }

    #[test]
    fn ir_07_an_unreadable_journal_record_blocks_new_dispatches() {
        let home = temp_home();
        let store = FileMutationStore::new(home.path());
        let operation = operation();
        let directory = store.pr_dir(&operation.repo, operation.pr);
        std::fs::create_dir_all(&directory).expect("creates operation directory");
        std::fs::write(directory.join("broken.json"), "not json").expect("writes broken record");

        assert!(matches!(
            store.begin(&operation),
            Err(MutationStoreError::Malformed { .. })
        ));
    }
}
