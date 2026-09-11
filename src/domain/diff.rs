//! The diff model (FR-3.2, FR-3.3, ARCH-4).
//!
//! A patch parses into `FileDiff → Hunk → DiffLine`, with both line numbers
//! resolved by the parser so the renderer never has to replay the hunk to know
//! where it is. Every case that is not ordinary text — binary, mode-only, a
//! submodule pointer, a rename, an empty diff — is a state on the model rather
//! than a missing file, so the UI can always say something specific instead of
//! showing nothing.

pub mod parse;

pub use parse::parse_patch;

use serde::{Deserialize, Serialize};

/// A repository-relative path.
///
/// Validated on construction, because a path that escapes the repository would
/// only ever come from a malformed patch, and acting on it later (reading the
/// file, opening it) would be a bug.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelPath(String);

impl RelPath {
    /// Validates and wraps a path.
    pub fn parse(path: impl Into<String>) -> Option<Self> {
        let path = path.into();
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.contains('\0') {
            return None;
        }
        if trimmed
            .split('/')
            .any(|segment| segment == ".." || segment == ".")
        {
            return None;
        }
        Some(Self(trimmed.to_owned()))
    }

    /// The path as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The last segment.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// The leading directories, outermost first.
    #[must_use]
    pub fn directories(&self) -> Vec<&str> {
        let mut parts: Vec<&str> = self.0.split('/').collect();
        parts.pop();
        parts
    }

    /// The directories as one string, empty for a top-level file.
    #[must_use]
    pub fn parent(&self) -> String {
        self.directories().join("/")
    }

    /// The extension, if there is one.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        self.file_name().rsplit_once('.').map(|(_, ext)| ext)
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a file changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    /// New file.
    Added,
    /// Removed file.
    Deleted,
    /// Content changed.
    Modified,
    /// Moved, possibly with edits.
    Renamed,
    /// Duplicated from another path.
    Copied,
}

impl FileStatus {
    /// The single letter the file tree and the diff header show.
    #[must_use]
    pub fn marker(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Modified => "M",
            Self::Renamed => "R",
            Self::Copied => "C",
        }
    }

    /// A word for the status line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Modified => "modified",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
        }
    }
}

/// A file mode change, e.g. `100644 → 100755`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeChange {
    /// The mode before, `None` when the file is new.
    pub old: Option<String>,
    /// The mode after, `None` when the file was deleted.
    pub new: Option<String>,
}

/// The gitlink a submodule points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmoduleChange {
    /// The commit the submodule pointed at before.
    pub old: Option<String>,
    /// The commit it points at now.
    pub new: Option<String>,
}

/// What kind of content a file diff has, which decides what the pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Ordinary line-by-line changes.
    Text,
    /// Git refused to diff it.
    Binary,
    /// Permissions changed and nothing else.
    ModeOnly,
    /// A submodule pointer moved.
    Submodule,
    /// Git reports no changes for it.
    Empty,
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    /// The path before the change, absent for an addition.
    pub old_path: Option<RelPath>,
    /// The path after the change, absent for a deletion.
    pub new_path: Option<RelPath>,
    /// How it changed.
    pub status: FileStatus,
    /// Whether git declined to produce a textual diff.
    pub binary: bool,
    /// A permission change, if any.
    pub mode_change: Option<ModeChange>,
    /// A submodule pointer change, if any.
    pub submodule: Option<SubmoduleChange>,
    /// Lines added.
    pub additions: u32,
    /// Lines deleted.
    pub deletions: u32,
    /// The hunks, in file order.
    pub hunks: Vec<Hunk>,
    /// Anything the parser could not interpret, kept so nothing is dropped
    /// silently.
    pub notes: Vec<String>,
}

impl FileDiff {
    /// The path to show: the new one, falling back to the old one for a deletion.
    #[must_use]
    pub fn display_path(&self) -> String {
        match (&self.new_path, &self.old_path) {
            (Some(new), Some(old)) if new != old => format!("{old} → {new}"),
            (Some(new), _) => new.to_string(),
            (None, Some(old)) => old.to_string(),
            (None, None) => "(unknown path)".to_owned(),
        }
    }

    /// The path a reader would open, which is the new one where it exists.
    #[must_use]
    pub fn path(&self) -> Option<&RelPath> {
        self.new_path.as_ref().or(self.old_path.as_ref())
    }

    /// What kind of content this file has.
    #[must_use]
    pub fn kind(&self) -> FileKind {
        if self.binary {
            return FileKind::Binary;
        }
        if self.submodule.is_some() {
            return FileKind::Submodule;
        }
        if self.hunks.is_empty() {
            return if self.mode_change.is_some() {
                FileKind::ModeOnly
            } else {
                FileKind::Empty
            };
        }
        FileKind::Text
    }

    /// The sentence the diff pane shows for a file with no lines to show
    /// (FR-3.2: a specific placeholder, never a blank pane).
    #[must_use]
    pub fn placeholder(&self) -> Option<String> {
        Some(match self.kind() {
            FileKind::Text => return None,
            FileKind::Binary => {
                let size = self
                    .notes
                    .iter()
                    .find_map(|note| note.strip_prefix("size:"))
                    .map_or(String::new(), |bytes| format!(" ({bytes} bytes)"));
                format!("binary file, not shown{size}")
            }
            FileKind::ModeOnly => {
                let change = self.mode_change.as_ref();
                let from = change.and_then(|c| c.old.as_deref()).unwrap_or("?");
                let to = change.and_then(|c| c.new.as_deref()).unwrap_or("?");
                format!("mode changed {from} → {to}, contents unchanged")
            }
            FileKind::Submodule => {
                let change = self.submodule.as_ref();
                let from = change
                    .and_then(|c| c.old.as_deref())
                    .map_or("?".to_owned(), short_sha);
                let to = change
                    .and_then(|c| c.new.as_deref())
                    .map_or("?".to_owned(), short_sha);
                format!("submodule pointer {from} → {to}")
            }
            FileKind::Empty => match self.status {
                FileStatus::Renamed => "renamed, contents unchanged".to_owned(),
                FileStatus::Copied => "copied, contents unchanged".to_owned(),
                _ => "no changes".to_owned(),
            },
        })
    }

    /// `+18 −4`, the per-file stats the tree shows.
    #[must_use]
    pub fn size_label(&self) -> String {
        match (self.additions, self.deletions) {
            (0, 0) => String::new(),
            (added, 0) => format!("+{added}"),
            (0, deleted) => format!("−{deleted}"),
            (added, deleted) => format!("+{added} −{deleted}"),
        }
    }

    /// How many rows the file occupies when drawn, excluding the header rows.
    #[must_use]
    pub fn body_rows(&self) -> usize {
        self.hunks.iter().map(|hunk| hunk.lines.len() + 1).sum()
    }
}

/// A short SHA, the way reviewers write them.
fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// A contiguous run of changed lines with its context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// First line of the hunk on the old side.
    pub old_start: u32,
    /// How many old lines the hunk covers.
    pub old_lines: u32,
    /// First line on the new side.
    pub new_start: u32,
    /// How many new lines the hunk covers.
    pub new_lines: u32,
    /// The function or section git found, shown after the `@@` marker.
    pub heading: Option<String>,
    /// The lines, in order.
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    /// The `@@ -a,b +c,d @@` text.
    #[must_use]
    pub fn header(&self) -> String {
        let mut header = format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_lines, self.new_start, self.new_lines
        );
        if let Some(heading) = &self.heading {
            header.push(' ');
            header.push_str(heading);
        }
        header
    }

    /// The line at `index`, if there is one.
    #[must_use]
    pub fn line(&self, index: usize) -> Option<&DiffLine> {
        self.lines.get(index)
    }
}

/// What a line does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    /// Unchanged, shown for context.
    Context,
    /// Present only on the new side.
    Add,
    /// Present only on the old side.
    Delete,
}

impl LineKind {
    /// The single character the unified view prints in the gutter.
    #[must_use]
    pub fn marker(self) -> char {
        match self {
            Self::Context => ' ',
            Self::Add => '+',
            Self::Delete => '-',
        }
    }
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    /// What it does.
    pub kind: LineKind,
    /// Its line number on the old side, `None` for an addition.
    pub old_line: Option<u32>,
    /// Its line number on the new side, `None` for a deletion.
    pub new_line: Option<u32>,
    /// The text, without the leading marker.
    pub content: String,
    /// Whether git reported that the file does not end with a newline here.
    pub no_newline: bool,
}

impl DiffLine {
    /// The line as a patch would write it, marker included.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{}{}", self.kind.marker(), self.content)
    }
}

/// Where a diff was read from (FR-3.2).
///
/// Part of the cache key, because the two sources are not interchangeable: GitHub
/// truncates very large diffs, and its patch is whatever its API decided to send,
/// while the local diff is produced by `git diff` with the flags the user chose.
/// Serving one for the other would look like a toggle that does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DiffSource {
    /// `gh pr diff` from the forge.
    Forge,
    /// `git diff` in the pull request's worktree.
    Worktree,
}

impl DiffSource {
    /// A short label for the status line and notices.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Forge => "github",
            Self::Worktree => "worktree",
        }
    }

    /// The part of a cache key that distinguishes the sources.
    #[must_use]
    pub fn cache_tag(self) -> &'static str {
        match self {
            Self::Forge => "",
            Self::Worktree => "-wt",
        }
    }
}

/// A whole patch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Patch {
    /// The changed files, in the order git listed them.
    pub files: Vec<FileDiff>,
}

impl Patch {
    /// Totals for the header.
    #[must_use]
    pub fn stats(&self) -> PatchStats {
        PatchStats {
            files: u32::try_from(self.files.len()).unwrap_or(u32::MAX),
            additions: self.files.iter().map(|file| file.additions).sum(),
            deletions: self.files.iter().map(|file| file.deletions).sum(),
        }
    }

    /// Whether the patch is empty, which happens for a PR whose changes are all
    /// merges or which was diffed against itself.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The index of the file at `path`, comparing the new path then the old one.
    #[must_use]
    pub fn find(&self, path: &RelPath) -> Option<usize> {
        self.files.iter().position(|file| {
            file.new_path.as_ref() == Some(path) || file.old_path.as_ref() == Some(path)
        })
    }

    /// The total number of renderable rows, used to size the scroll bar without
    /// laying anything out (FR-3.3).
    #[must_use]
    pub fn body_rows(&self) -> usize {
        self.files
            .iter()
            .map(|file| file.body_rows() + 1)
            .sum::<usize>()
    }
}

/// Totals across a patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchStats {
    /// How many files changed.
    pub files: u32,
    /// Lines added.
    pub additions: u32,
    /// Lines deleted.
    pub deletions: u32,
}

impl PatchStats {
    /// `7 files +412 −96`.
    #[must_use]
    pub fn label(&self) -> String {
        let files = if self.files == 1 {
            "1 file".to_owned()
        } else {
            format!("{} files", self.files)
        };
        format!("{files} +{} −{}", self.additions, self.deletions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(status: FileStatus) -> FileDiff {
        FileDiff {
            old_path: RelPath::parse("src/a.rs"),
            new_path: RelPath::parse("src/a.rs"),
            status,
            binary: false,
            mode_change: None,
            submodule: None,
            additions: 0,
            deletions: 0,
            hunks: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn relative_paths_are_validated() {
        assert_eq!(
            RelPath::parse("src/domain/invoice.rs").unwrap().as_str(),
            "src/domain/invoice.rs"
        );
        assert!(RelPath::parse("").is_none());
        assert!(RelPath::parse("/etc/passwd").is_none(), "absolute");
        assert!(RelPath::parse("../outside").is_none(), "escaping");
        assert!(RelPath::parse("a/../../b").is_none(), "escaping");
        assert!(RelPath::parse("a/./b").is_none(), "not canonical");
        assert!(RelPath::parse("with\0nul").is_none());
    }

    #[test]
    fn path_parts_are_available_without_string_surgery_at_the_call_site() {
        let path = RelPath::parse("src/domain/invoice.rs").unwrap();
        assert_eq!(path.file_name(), "invoice.rs");
        assert_eq!(path.directories(), vec!["src", "domain"]);
        assert_eq!(path.parent(), "src/domain");
        assert_eq!(path.extension(), Some("rs"));

        let top = RelPath::parse("README.md").unwrap();
        assert!(top.directories().is_empty());
        assert_eq!(top.parent(), "");
        assert_eq!(top.extension(), Some("md"));

        let bare = RelPath::parse("Makefile").unwrap();
        assert_eq!(bare.extension(), None);
    }

    #[test]
    fn a_rename_shows_both_paths() {
        let mut diff = file(FileStatus::Renamed);
        diff.new_path = RelPath::parse("src/b.rs");
        assert_eq!(diff.display_path(), "src/a.rs → src/b.rs");
        assert_eq!(diff.path().unwrap().as_str(), "src/b.rs");
    }

    #[test]
    fn a_deletion_falls_back_to_the_old_path() {
        let diff = FileDiff {
            old_path: RelPath::parse("src/gone.rs"),
            new_path: None,
            ..file(FileStatus::Deleted)
        };
        assert_eq!(diff.display_path(), "src/gone.rs");
        assert_eq!(diff.path().unwrap().as_str(), "src/gone.rs");
    }

    #[test]
    fn a_file_with_neither_path_is_not_a_crash() {
        let diff = FileDiff {
            old_path: None,
            new_path: None,
            ..file(FileStatus::Modified)
        };
        assert_eq!(diff.display_path(), "(unknown path)");
        assert!(diff.path().is_none());
    }

    #[test]
    fn every_non_text_file_has_something_to_say() {
        let mut binary = file(FileStatus::Modified);
        binary.binary = true;
        binary.notes.push("size:1234".to_owned());
        assert_eq!(binary.kind(), FileKind::Binary);
        assert_eq!(
            binary.placeholder().unwrap(),
            "binary file, not shown (1234 bytes)"
        );

        let mode_only = FileDiff {
            mode_change: Some(ModeChange {
                old: Some("100644".to_owned()),
                new: Some("100755".to_owned()),
            }),
            ..file(FileStatus::Modified)
        };
        assert_eq!(mode_only.kind(), FileKind::ModeOnly);
        assert!(mode_only.placeholder().unwrap().contains("100755"));

        let submodule = FileDiff {
            submodule: Some(SubmoduleChange {
                old: Some("1234567890abcdef".to_owned()),
                new: Some("fedcba0987654321".to_owned()),
            }),
            ..file(FileStatus::Modified)
        };
        assert_eq!(submodule.kind(), FileKind::Submodule);
        assert_eq!(
            submodule.placeholder().unwrap(),
            "submodule pointer 1234567 → fedcba0"
        );

        let renamed = file(FileStatus::Renamed);
        assert_eq!(renamed.kind(), FileKind::Empty);
        assert_eq!(
            renamed.placeholder().unwrap(),
            "renamed, contents unchanged"
        );

        let empty = file(FileStatus::Modified);
        assert_eq!(empty.kind(), FileKind::Empty);
        assert_eq!(empty.placeholder().unwrap(), "no changes");

        // A text file has no placeholder: there are lines to show.
        let text = FileDiff {
            hunks: vec![Hunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                heading: None,
                lines: Vec::new(),
            }],
            ..file(FileStatus::Modified)
        };
        assert_eq!(text.kind(), FileKind::Text);
        assert!(text.placeholder().is_none());
    }

    #[test]
    fn a_hunk_header_round_trips() {
        let hunk = Hunk {
            old_start: 12,
            old_lines: 6,
            new_start: 14,
            new_lines: 9,
            heading: Some("impl Invoice {".to_owned()),
            lines: Vec::new(),
        };
        assert_eq!(hunk.header(), "@@ -12,6 +14,9 @@ impl Invoice {");

        let bare = Hunk {
            heading: None,
            ..hunk
        };
        assert_eq!(bare.header(), "@@ -12,6 +14,9 @@");
    }

    #[test]
    fn size_labels_omit_a_side_that_did_not_change() {
        let mut diff = file(FileStatus::Modified);
        assert_eq!(diff.size_label(), "");
        diff.additions = 5;
        assert_eq!(diff.size_label(), "+5");
        diff.deletions = 2;
        assert_eq!(diff.size_label(), "+5 −2");
        diff.additions = 0;
        assert_eq!(diff.size_label(), "−2");
    }

    #[test]
    fn patch_totals_add_up_over_files() {
        let mut one = file(FileStatus::Modified);
        one.additions = 10;
        one.deletions = 2;
        let mut two = file(FileStatus::Added);
        two.additions = 3;
        let patch = Patch {
            files: vec![one, two],
        };
        let stats = patch.stats();
        assert_eq!(stats.files, 2);
        assert_eq!(stats.additions, 13);
        assert_eq!(stats.deletions, 2);
        assert_eq!(stats.label(), "2 files +13 −2");
        assert!(!patch.is_empty());
        assert!(Patch::default().is_empty());
    }

    #[test]
    fn files_can_be_found_by_either_path() {
        let mut renamed = file(FileStatus::Renamed);
        renamed.new_path = RelPath::parse("src/b.rs");
        let patch = Patch {
            files: vec![renamed],
        };
        assert_eq!(patch.find(&RelPath::parse("src/b.rs").unwrap()), Some(0));
        assert_eq!(patch.find(&RelPath::parse("src/a.rs").unwrap()), Some(0));
        assert_eq!(patch.find(&RelPath::parse("nope.rs").unwrap()), None);
    }

    #[test]
    fn status_markers_are_the_letters_git_uses() {
        assert_eq!(FileStatus::Added.marker(), "A");
        assert_eq!(FileStatus::Deleted.marker(), "D");
        assert_eq!(FileStatus::Modified.marker(), "M");
        assert_eq!(FileStatus::Renamed.marker(), "R");
        assert_eq!(FileStatus::Copied.marker(), "C");
    }

    #[test]
    fn a_line_renders_the_way_a_patch_writes_it() {
        let line = DiffLine {
            kind: LineKind::Add,
            old_line: None,
            new_line: Some(14),
            content: "let x = 1;".to_owned(),
            no_newline: false,
        };
        assert_eq!(line.render(), "+let x = 1;");
    }
}
