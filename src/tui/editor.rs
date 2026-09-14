//! The one place a comment composer can hand control to `$EDITOR`.
//!
//! The reducer asks for an edit through an effect; this module only prepares a private
//! scratch file and runs the user's editor after the terminal guard has been suspended.
//! Keeping the file until the caller has successfully resumed the UI makes a terminal
//! resume failure recoverable: the log names the file rather than losing the prose.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::paths::Home;

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

/// A private, app-owned file handed to `$EDITOR`.
#[derive(Debug)]
pub(crate) struct EditorFile {
    path: PathBuf,
}

impl EditorFile {
    /// Creates a unique scratch file below the application home with the composer text.
    pub(crate) fn create(home: &Home, body: &str) -> io::Result<Self> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..32 {
            let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let path = home.root().join(format!(
                ".smart-review-editor-{}-{now}-{serial}.md",
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(body.as_bytes()) {
                        let _ = fs::remove_file(&path);
                        return Err(error);
                    }
                    return Ok(Self { path });
                }
                // The loop advances to the next counter value by itself.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique editor file",
        ))
    }

    /// Starts `$EDITOR`, passing the scratch file as its final positional argument.
    ///
    /// The editor value is an executable name or path, not a shell snippet. This keeps
    /// user-controlled environment data out of a shell command; configure wrapper
    /// scripts through their executable path when editor flags are needed.
    pub(crate) fn run(&self) -> io::Result<EditorExit> {
        let editor = std::env::var_os("EDITOR")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "$EDITOR is not set"))?;
        let status = Command::new(editor).arg(&self.path).status()?;
        Ok(EditorExit::from_status(status))
    }

    /// Reads the edited text and removes the scratch file after the terminal resumed.
    pub(crate) fn read_and_remove(self) -> io::Result<String> {
        let body = fs::read_to_string(&self.path)?;
        fs::remove_file(&self.path)?;
        Ok(body)
    }

    /// The recovery location if smart-review could not resume its terminal afterwards.
    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Whether the editor returned success; its file is read in either case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EditorExit {
    /// The editor completed normally.
    Success,
    /// The editor stopped with a non-zero code. The text remains usable.
    Failed(Option<i32>),
}

impl EditorExit {
    fn from_status(status: std::process::ExitStatus) -> Self {
        if status.success() {
            Self::Success
        } else {
            Self::Failed(status.code())
        }
    }

    /// A user-facing warning that does not discard the file's contents.
    #[must_use]
    pub(crate) fn warning(&self) -> Option<String> {
        match self {
            Self::Success => None,
            Self::Failed(Some(code)) => {
                Some(format!("$EDITOR exited with status {code}; kept its text"))
            }
            Self::Failed(None) => Some("$EDITOR was interrupted; kept its text".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_editor_is_a_warning_not_a_reason_to_discard_text() {
        assert_eq!(EditorExit::Success.warning(), None);
        assert_eq!(
            EditorExit::Failed(Some(23)).warning().as_deref(),
            Some("$EDITOR exited with status 23; kept its text")
        );
    }

    #[test]
    fn a_scratch_file_round_trips_utf8_and_is_removed() {
        let home_dir = crate::test_support::temp_home();
        let home = Home::resolve(Some(home_dir.path())).unwrap();
        home.ensure().unwrap();
        let file = EditorFile::create(&home, "a comment\n").unwrap();
        let path = file.path().to_path_buf();
        assert_eq!(file.read_and_remove().unwrap(), "a comment\n");
        assert!(!path.exists());
    }
}
