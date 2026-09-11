//! The unified diff parser (FR-3.2).
//!
//! Pure and total: it takes the text of a patch and returns a [`Patch`], never
//! panics, and never drops a line silently. Malformed input degrades into a note
//! on the file it appeared in, because a patch is produced by an external tool and
//! a parser that trusts it will eventually meet a patch it does not understand at
//! the worst possible moment.
//!
//! The cases that decide whether this is pleasant to use are all covered by
//! tests: renames, copies, new and deleted files, binary files in both spellings,
//! mode-only changes, submodule pointers, CRLF line endings, a missing newline at
//! the end of a file, a missing newline at the end of the patch, quoted paths with
//! spaces, and hunks with omitted line counts.
//!
//! Two traps worth naming, because they are the reason the dispatch order below is
//! what it is:
//!
//! 1. inside a hunk, `--- x` is a *deleted line* whose content begins with `--`,
//!    not a file header, so hunk content is consulted before header markers;
//! 2. `\ No newline at end of file` annotates the line *before* it, so it must be
//!    recorded against the previous line rather than emitted as a row.

use super::{
    DiffLine, FileDiff, FileStatus, Hunk, LineKind, ModeChange, Patch, RelPath, SubmoduleChange,
};

/// How many unparsed lines are quoted before the rest are only counted.
const MAX_NOTES: usize = 3;

/// Parses the output of `git diff` or `gh pr diff`.
#[must_use]
pub fn parse_patch(text: &str) -> Patch {
    let mut parser = Parser::default();
    let mut lines = text.split('\n').peekable();
    while let Some(raw) = lines.next() {
        // A patch that ends with a newline yields one empty trailing element,
        // which is an artefact of splitting rather than a line.
        if raw.is_empty() && lines.peek().is_none() {
            break;
        }
        parser.consume(strip_carriage_return(raw));
    }
    parser.finish()
}

/// Removes one trailing carriage return so a CRLF patch parses like an LF one.
///
/// The carriage return is part of the file's content, not of the patch format; a
/// terminal would treat it as a cursor movement, so it is dropped for display and
/// for parsing alike.
fn strip_carriage_return(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

#[derive(Default)]
struct Parser {
    files: Vec<FileDiff>,
    current: Option<FileBuilder>,
    hunk: Option<HunkBuilder>,
}

impl Parser {
    fn consume(&mut self, line: &str) {
        // A new file can begin at any point, including straight after a hunk.
        if let Some(rest) = line.strip_prefix("diff --git ") {
            self.finish_file();
            let (old, new) = split_git_header(rest).unzip();
            self.current = Some(FileBuilder {
                old_path: old.as_deref().and_then(clean_path),
                new_path: new.as_deref().and_then(clean_path),
                ..FileBuilder::default()
            });
            return;
        }

        // Anything before the first `diff --git` is not ours: git prepends commit
        // metadata when a caller used `git log -p`.
        if self.current.is_none() {
            return;
        }

        // Inside a hunk, `+`, `-` and ` ` are content. This is decided before the
        // header markers so a deleted line whose text begins with `--` is not
        // mistaken for a `--- a/path` header.
        if self.hunk.is_some() {
            if let Some(rest) = line.strip_prefix("@@") {
                let header = format!("@@{rest}");
                self.finish_hunk();
                self.open_hunk(&header);
                return;
            }
            if line.starts_with('\\') {
                self.mark_no_newline();
                return;
            }
            match line.chars().next() {
                Some('+') => self.push_line(LineKind::Add, &line[1..]),
                Some('-') => self.push_line(LineKind::Delete, &line[1..]),
                Some(' ') => self.push_line(LineKind::Context, &line[1..]),
                // Some tools emit an empty context line without its leading space.
                None => self.push_line(LineKind::Context, ""),
                Some(_) => {
                    // Not a content line, so the hunk ended without a marker of
                    // its own. Keep the text rather than dropping it.
                    self.finish_hunk();
                    self.note(format!("unparsed line inside a hunk: {}", preview(line)));
                }
            }
            return;
        }

        if let Some(rest) = line.strip_prefix("@@") {
            let header = format!("@@{rest}");
            self.open_hunk(&header);
            return;
        }

        // Everything below is file metadata: paths, modes, renames, binary.
        if self.consume_paths(line) || self.consume_metadata(line) {
            return;
        }
        if looks_like_content(line) {
            // A content line with no hunk header before it: say the patch was odd
            // instead of dropping the text.
            self.note(format!("line without a hunk: {}", preview(line)));
        }
    }

    /// Handles the markers that give a file its paths and modes.
    ///
    /// Returns whether the line was one of them.
    fn consume_paths(&mut self, line: &str) -> bool {
        let Some(file) = self.current.as_mut() else {
            return false;
        };
        if let Some(rest) = line.strip_prefix("--- ") {
            file.old_path = clean_path(rest);
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            file.new_path = clean_path(rest);
        } else if let Some(rest) = line.strip_prefix("new file mode ") {
            file.status = Some(FileStatus::Added);
            file.mode_new = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("deleted file mode ") {
            file.status = Some(FileStatus::Deleted);
            file.mode_old = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("old mode ") {
            file.mode_old = Some(rest.trim().to_owned());
            file.mode_changed = true;
        } else if let Some(rest) = line.strip_prefix("new mode ") {
            file.mode_new = Some(rest.trim().to_owned());
            file.mode_changed = true;
        } else {
            return false;
        }
        true
    }

    /// Handles the remaining metadata markers, including the ones that mark a
    /// file as binary.
    ///
    /// Returns whether the line was one of them.
    fn consume_metadata(&mut self, line: &str) -> bool {
        if let Some(rest) = line.strip_prefix("rename from ") {
            self.mark_rename(rest, true, FileStatus::Renamed);
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            self.mark_rename(rest, false, FileStatus::Renamed);
        } else if let Some(rest) = line.strip_prefix("copy from ") {
            self.mark_rename(rest, true, FileStatus::Copied);
        } else if let Some(rest) = line.strip_prefix("copy to ") {
            self.mark_rename(rest, false, FileStatus::Copied);
        } else if let Some(rest) = line.strip_prefix("index ") {
            if rest.split_whitespace().last() == Some("160000")
                && let Some(file) = self.current.as_mut()
            {
                file.submodule_mode = true;
            }
        } else if (line.starts_with("Binary files ") && line.ends_with(" differ"))
            || line.starts_with("GIT binary patch")
        {
            if let Some(file) = self.current.as_mut() {
                file.binary = true;
            }
        } else if let Some(rest) = line.strip_prefix("literal ") {
            // The size of a binary blob, which the placeholder can report.
            if let Ok(bytes) = rest.trim().parse::<u64>() {
                self.note(format!("size:{bytes}"));
            }
        } else if line.starts_with("similarity index ") || line.starts_with("dissimilarity index ")
        {
            // The rename threshold git used; the rename itself is what matters.
        } else {
            return false;
        }
        true
    }

    /// Records one half of a rename or copy marker.
    fn mark_rename(&mut self, path: &str, is_source: bool, status: FileStatus) {
        if let Some(file) = self.current.as_mut() {
            if is_source {
                file.rename_from = Some(unquote(path.trim()));
            } else {
                file.rename_to = Some(unquote(path.trim()));
            }
            file.status = Some(status);
        }
    }

    /// Records a problem against the file being parsed.
    fn note(&mut self, note: String) {
        if let Some(file) = self.current.as_mut() {
            file.note(note);
        }
    }

    fn open_hunk(&mut self, header: &str) {
        if let Some(hunk) = parse_hunk_header(header) {
            self.hunk = Some(hunk);
        } else {
            self.hunk = None;
            self.note(format!("unparsable hunk header: {}", preview(header)));
        }
    }

    fn push_line(&mut self, kind: LineKind, content: &str) {
        let (Some(hunk), Some(file)) = (self.hunk.as_mut(), self.current.as_mut()) else {
            return;
        };
        let (old_line, new_line) = match kind {
            LineKind::Add => {
                let line = hunk.new_line;
                hunk.new_line += 1;
                file.additions += 1;
                (None, Some(line))
            }
            LineKind::Delete => {
                let line = hunk.old_line;
                hunk.old_line += 1;
                file.deletions += 1;
                (Some(line), None)
            }
            LineKind::Context => {
                let (old, new) = (hunk.old_line, hunk.new_line);
                hunk.old_line += 1;
                hunk.new_line += 1;
                (Some(old), Some(new))
            }
        };

        // A submodule change arrives as ordinary-looking lines; recognising the
        // text is what lets the pane explain the pointer instead of showing two
        // opaque SHAs.
        if let Some(sha) = content.strip_prefix("Subproject commit ") {
            if kind == LineKind::Add {
                file.submodule_new = Some(sha.trim().to_owned());
            } else if kind == LineKind::Delete {
                file.submodule_old = Some(sha.trim().to_owned());
            }
        }

        hunk.lines.push(DiffLine {
            kind,
            old_line,
            new_line,
            content: content.to_owned(),
            no_newline: false,
        });
    }

    fn mark_no_newline(&mut self) {
        match self.hunk.as_mut().and_then(|hunk| hunk.lines.last_mut()) {
            Some(line) => line.no_newline = true,
            None => self.note("a no-newline marker with nothing to attach it to".to_owned()),
        }
    }

    fn finish_hunk(&mut self) {
        if let (Some(hunk), Some(file)) = (self.hunk.take(), self.current.as_mut()) {
            file.hunks.push(hunk.finish());
        }
    }

    fn finish_file(&mut self) {
        self.finish_hunk();
        if let Some(file) = self.current.take() {
            self.files.push(file.finish());
        }
    }

    fn finish(mut self) -> Patch {
        self.finish_file();
        Patch { files: self.files }
    }
}

#[derive(Default)]
struct FileBuilder {
    old_path: Option<RelPath>,
    new_path: Option<RelPath>,
    status: Option<FileStatus>,
    mode_old: Option<String>,
    mode_new: Option<String>,
    mode_changed: bool,
    rename_from: Option<String>,
    rename_to: Option<String>,
    binary: bool,
    submodule_mode: bool,
    submodule_old: Option<String>,
    submodule_new: Option<String>,
    additions: u32,
    deletions: u32,
    hunks: Vec<Hunk>,
    notes: Vec<String>,
    extra_notes: usize,
}

impl FileBuilder {
    fn note(&mut self, note: String) {
        if self.notes.len() < MAX_NOTES {
            self.notes.push(note);
        } else {
            self.extra_notes += 1;
        }
    }

    fn finish(mut self) -> FileDiff {
        if self.extra_notes > 0 {
            self.notes.push(format!("and {} more", self.extra_notes));
        }

        // Explicit rename markers win over the header paths, which git leaves at
        // the *old* names for a pure rename.
        if let Some(from) = self.rename_from.as_deref().and_then(RelPath::parse) {
            self.old_path = Some(from);
        }
        if let Some(to) = self.rename_to.as_deref().and_then(RelPath::parse) {
            self.new_path = Some(to);
        }

        let status = self
            .status
            .unwrap_or_else(|| match (&self.old_path, &self.new_path) {
                (None, Some(_)) => FileStatus::Added,
                (Some(_), None) => FileStatus::Deleted,
                (Some(old), Some(new)) if old != new => FileStatus::Renamed,
                _ => FileStatus::Modified,
            });

        let mode_change = if self.mode_changed
            || (status == FileStatus::Added && self.mode_new.is_some())
            || (status == FileStatus::Deleted && self.mode_old.is_some())
        {
            Some(ModeChange {
                old: self.mode_old,
                new: self.mode_new,
            })
        } else {
            None
        };

        let submodule = if self.submodule_mode
            || self.submodule_old.is_some()
            || self.submodule_new.is_some()
        {
            Some(SubmoduleChange {
                old: self.submodule_old,
                new: self.submodule_new,
            })
        } else {
            None
        };

        FileDiff {
            old_path: self.old_path,
            new_path: self.new_path,
            status,
            binary: self.binary,
            mode_change,
            submodule,
            additions: self.additions,
            deletions: self.deletions,
            hunks: self.hunks,
            notes: self.notes,
        }
    }
}

struct HunkBuilder {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
    heading: Option<String>,
    old_line: u32,
    new_line: u32,
    lines: Vec<DiffLine>,
}

impl HunkBuilder {
    fn finish(self) -> Hunk {
        Hunk {
            old_start: self.old_start,
            old_lines: self.old_lines,
            new_start: self.new_start,
            new_lines: self.new_lines,
            heading: self.heading,
            lines: self.lines,
        }
    }
}

/// Parses `@@ -12,6 +14,9 @@ heading`, including the `@@ -1 +1 @@` short form
/// where the counts are omitted and mean one line.
fn parse_hunk_header(header: &str) -> Option<HunkBuilder> {
    let rest = header.strip_prefix("@@")?;
    let (ranges, heading) = rest.split_once("@@")?;
    let mut parts = ranges.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;

    let (old_start, old_lines) = parse_range(old)?;
    let (new_start, new_lines) = parse_range(new)?;
    let heading = heading.trim();

    Some(HunkBuilder {
        old_start,
        old_lines,
        new_start,
        new_lines,
        heading: (!heading.is_empty()).then(|| heading.to_owned()),
        old_line: old_start,
        new_line: new_start,
        lines: Vec::new(),
    })
}

/// Parses `12,6` or `12`.
fn parse_range(range: &str) -> Option<(u32, u32)> {
    let (start, count) = range
        .split_once(',')
        .map_or((range, "1"), |(start, count)| (start, count));
    Some((start.parse().ok()?, count.parse().ok()?))
}

/// Splits `a/one b/two` into its two path tokens, honouring quotes.
fn split_git_header(rest: &str) -> Option<(String, String)> {
    let rest = rest.trim();
    let (first, remaining) = take_path_token(rest)?;
    let (second, _) = take_path_token(remaining.trim_start())?;
    if first.is_empty() || second.is_empty() {
        return None;
    }
    Some((first, second))
}

/// Takes one path token from the front of `text`.
fn take_path_token(text: &str) -> Option<(String, &str)> {
    if let Some(after_quote) = text.strip_prefix('"') {
        // A quoted path may contain spaces; find the closing quote, skipping
        // backslash escapes.
        let mut escaped = false;
        for (index, character) in after_quote.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match character {
                '\\' => escaped = true,
                '"' => return Some((unquote(&text[..index + 2]), &after_quote[index + 1..])),
                _ => {}
            }
        }
        return None;
    }
    // The last token on a line ends at the end of the line, not at a space.
    let end = text.find(' ').unwrap_or(text.len());
    Some((text[..end].to_owned(), &text[end..]))
}

/// Removes `a/` or `b/` and the surrounding quotes, and rejects `/dev/null`.
fn clean_path(raw: &str) -> Option<RelPath> {
    let unquoted = unquote(raw.trim());
    if unquoted == "/dev/null" {
        return None;
    }
    let stripped = unquoted
        .strip_prefix("a/")
        .or_else(|| unquoted.strip_prefix("b/"))
        .unwrap_or(&unquoted);
    RelPath::parse(stripped)
}

/// Removes one layer of double quotes and the escapes git applies inside them.
fn unquote(raw: &str) -> String {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(trimmed);
    if !trimmed.starts_with('"') {
        return inner.to_owned();
    }
    let mut out = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            // A backslash that ends the string, and octal escapes (a path git
            // could not keep as UTF-8) are left as written rather than guessed at.
            Some('\\') | None => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

/// Whether a line looks like diff content rather than metadata.
fn looks_like_content(line: &str) -> bool {
    line.starts_with('+') || line.starts_with('-') || line.starts_with(' ')
}

/// A short, single-line rendering of a line for a note.
fn preview(line: &str) -> String {
    let mut text: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use crate::domain::diff::FileKind;

    /// A typical one-hunk modification, with two files.
    const SIMPLE: &str = "\
diff --git a/src/domain/invoice.rs b/src/domain/invoice.rs
index 1a2b3c4..5d6e7f8 100644
--- a/src/domain/invoice.rs
+++ b/src/domain/invoice.rs
@@ -12,6 +14,9 @@ impl Invoice {
     pub fn total(&self) -> Money {
-        self.lines.sum()
+        let gross = self.lines.sum();
+        gross - self.discount
     }
 }
diff --git a/README.md b/README.md
index 1111111..2222222 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # Smart Review
+New line.
";

    #[test]
    fn a_simple_patch_parses_into_files_hunks_and_numbered_lines() {
        let patch = parse_patch(SIMPLE);
        assert_eq!(patch.files.len(), 2);

        let file = &patch.files[0];
        assert_eq!(file.status, FileStatus::Modified);
        assert_eq!(
            file.new_path.as_ref().unwrap().as_str(),
            "src/domain/invoice.rs"
        );
        assert_eq!(file.hunks.len(), 1);

        let hunk = &file.hunks[0];
        assert_eq!((hunk.old_start, hunk.old_lines), (12, 6));
        assert_eq!((hunk.new_start, hunk.new_lines), (14, 9));
        assert_eq!(hunk.heading.as_deref(), Some("impl Invoice {"));
        assert_eq!(hunk.header(), "@@ -12,6 +14,9 @@ impl Invoice {");

        // Context takes a number on both sides, an addition only on the new one.
        let context = &hunk.lines[0];
        assert_eq!(context.kind, LineKind::Context);
        assert_eq!((context.old_line, context.new_line), (Some(12), Some(14)));
        assert_eq!(context.content, "    pub fn total(&self) -> Money {");

        let deleted = &hunk.lines[1];
        assert_eq!(deleted.kind, LineKind::Delete);
        assert_eq!((deleted.old_line, deleted.new_line), (Some(13), None));
        assert_eq!(deleted.content, "        self.lines.sum()");

        let added = &hunk.lines[2];
        assert_eq!(added.kind, LineKind::Add);
        assert_eq!((added.old_line, added.new_line), (None, Some(15)));
        assert_eq!(added.render(), "+        let gross = self.lines.sum();");

        assert_eq!(file.additions, 2);
        assert_eq!(file.deletions, 1);
        assert_eq!(patch.stats().label(), "2 files +3 −1");
    }

    #[test]
    fn line_numbers_follow_the_hunk_after_the_header_offsets() {
        let patch = parse_patch(SIMPLE);
        let second = &patch.files[1];
        let hunk = &second.hunks[0];
        assert_eq!((hunk.old_start, hunk.old_lines), (1, 1));
        assert_eq!((hunk.new_start, hunk.new_lines), (1, 2));
        assert_eq!(hunk.lines[0].old_line, Some(1));
        assert_eq!(hunk.lines[0].new_line, Some(1));
        assert_eq!(hunk.lines[1].new_line, Some(2));
        assert_eq!(hunk.lines[1].old_line, None);
    }

    #[test]
    fn an_empty_patch_is_an_empty_patch_and_not_an_error() {
        assert!(parse_patch("").is_empty());
        assert!(parse_patch("\n").is_empty());
        assert!(parse_patch("not a patch at all\nreally not\n").is_empty());
    }

    #[test]
    fn a_new_file_is_added_and_keeps_its_mode_change() {
        let patch = parse_patch(
            "\
diff --git a/src/new.rs b/src/new.rs
new file mode 100644
index 0000000..1234567
--- /dev/null
+++ b/src/new.rs
@@ -0,0 +1,2 @@
+one
+two
",
        );
        let file = &patch.files[0];
        assert_eq!(file.status, FileStatus::Added);
        assert!(file.old_path.is_none(), "/dev/null is not a path");
        assert_eq!(file.new_path.as_ref().unwrap().as_str(), "src/new.rs");
        assert_eq!(file.additions, 2);
        assert_eq!(file.deletions, 0);
        assert_eq!(file.kind(), FileKind::Text);

        let mode = file.mode_change.as_ref().unwrap();
        assert_eq!(mode.old, None);
        assert_eq!(mode.new.as_deref(), Some("100644"));
    }

    #[test]
    fn a_deleted_file_keeps_its_old_path_and_drops_the_new_one() {
        let patch = parse_patch(
            "\
diff --git a/src/gone.rs b/src/gone.rs
deleted file mode 100644
index 1234567..0000000
--- a/src/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-one
-two
",
        );
        let file = &patch.files[0];
        assert_eq!(file.status, FileStatus::Deleted);
        assert_eq!(file.display_path(), "src/gone.rs");
        assert_eq!(file.deletions, 2);
        assert_eq!(file.additions, 0);
    }

    #[test]
    fn a_pure_rename_has_no_hunks_and_says_so() {
        let patch = parse_patch(
            "\
diff --git a/old/name.rs b/new/name.rs
similarity index 100%
rename from old/name.rs
rename to new/name.rs
",
        );
        let file = &patch.files[0];
        assert_eq!(file.status, FileStatus::Renamed);
        assert_eq!(file.old_path.as_ref().unwrap().as_str(), "old/name.rs");
        assert_eq!(file.new_path.as_ref().unwrap().as_str(), "new/name.rs");
        assert_eq!(file.display_path(), "old/name.rs → new/name.rs");
        assert_eq!(file.kind(), FileKind::Empty);
        assert_eq!(file.placeholder().unwrap(), "renamed, contents unchanged");
    }

    #[test]
    fn a_rename_with_edits_keeps_both_paths_and_the_hunks() {
        let patch = parse_patch(
            "\
diff --git a/old.rs b/new.rs
similarity index 87%
rename from old.rs
rename to new.rs
index 1234567..89abcde 100644
--- a/old.rs
+++ b/new.rs
@@ -1,2 +1,2 @@
 keep
-old
+new
",
        );
        let file = &patch.files[0];
        assert_eq!(file.status, FileStatus::Renamed);
        assert_eq!(file.hunks.len(), 1);
        assert_eq!(file.additions, 1);
        assert_eq!(file.deletions, 1);
        assert_eq!(file.kind(), FileKind::Text);
    }

    #[test]
    fn a_copy_is_reported_as_a_copy() {
        let patch = parse_patch(
            "\
diff --git a/original.rs b/copy.rs
similarity index 100%
copy from original.rs
copy to copy.rs
",
        );
        assert_eq!(patch.files[0].status, FileStatus::Copied);
        assert_eq!(patch.files[0].kind(), FileKind::Empty);
        assert_eq!(
            patch.files[0].placeholder().unwrap(),
            "copied, contents unchanged"
        );
    }

    #[test]
    fn a_binary_file_is_marked_binary_with_no_lines() {
        let patch = parse_patch(
            "\
diff --git a/logo.png b/logo.png
index 1111111..2222222 100644
Binary files a/logo.png and b/logo.png differ
",
        );
        let file = &patch.files[0];
        assert!(file.binary);
        assert_eq!(file.kind(), FileKind::Binary);
        assert!(file.hunks.is_empty());
        assert_eq!(file.placeholder().unwrap(), "binary file, not shown");
        assert_eq!(file.new_path.as_ref().unwrap().as_str(), "logo.png");
    }

    #[test]
    fn a_git_binary_patch_is_skipped_whole() {
        let patch = parse_patch(
            "\
diff --git a/logo.png b/logo.png
new file mode 100644
index 0000000..2222222
GIT binary patch
literal 1234
zcmZQzU|?VUU|?VUU|?VUU|?VUU|?VUU|?VUU|?VUU|?VUU|?VUU|?VUU|?VUU
literal 0
HcmV?d00001

diff --git a/after.txt b/after.txt
index 1111111..2222222 100644
--- a/after.txt
+++ b/after.txt
@@ -1 +1 @@
-a
+b
",
        );
        assert_eq!(patch.files.len(), 2, "the base64 blob is not a file");
        assert!(patch.files[0].binary);
        assert_eq!(
            patch.files[0].placeholder().unwrap(),
            "binary file, not shown (1234 bytes)"
        );
        assert_eq!(
            patch.files[1].new_path.as_ref().unwrap().as_str(),
            "after.txt"
        );
    }

    #[test]
    fn a_mode_only_change_has_no_hunks_and_a_specific_message() {
        let patch = parse_patch(
            "\
diff --git a/script.sh b/script.sh
old mode 100644
new mode 100755
",
        );
        let file = &patch.files[0];
        assert_eq!(file.status, FileStatus::Modified);
        assert_eq!(file.kind(), FileKind::ModeOnly);
        assert_eq!(
            file.placeholder().unwrap(),
            "mode changed 100644 → 100755, contents unchanged"
        );
    }

    #[test]
    fn a_submodule_pointer_move_is_named_rather_than_shown_as_two_shas() {
        let patch = parse_patch(
            "\
diff --git a/vendor/lib b/vendor/lib
index 1111111..2222222 160000
--- a/vendor/lib
+++ b/vendor/lib
@@ -1 +1 @@
-Subproject commit 1111111111111111111111111111111111111111
+Subproject commit 2222222222222222222222222222222222222222
",
        );
        let file = &patch.files[0];
        assert_eq!(file.kind(), FileKind::Submodule);
        assert_eq!(
            file.placeholder().unwrap(),
            "submodule pointer 1111111 → 2222222"
        );
    }

    #[test]
    fn a_submodule_is_recognised_even_without_the_gitlink_mode() {
        // Some servers (and `gh pr diff`) omit the mode on the index line.
        let patch = parse_patch(
            "\
diff --git a/vendor/lib b/vendor/lib
--- a/vendor/lib
+++ b/vendor/lib
@@ -1 +1 @@
-Subproject commit aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
+Subproject commit bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
",
        );
        assert_eq!(patch.files[0].kind(), FileKind::Submodule);
    }

    #[test]
    fn a_missing_newline_marker_annotates_the_line_before_it() {
        let patch = parse_patch(
            "\
diff --git a/one.txt b/one.txt
index 1111111..2222222 100644
--- a/one.txt
+++ b/one.txt
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
",
        );
        let hunk = &patch.files[0].hunks[0];
        assert_eq!(hunk.lines.len(), 2, "the marker is not a line of its own");
        assert!(hunk.lines[0].no_newline, "the deleted line lacks a newline");
        assert!(hunk.lines[1].no_newline, "so does the added line");
        assert_eq!(hunk.lines[0].content, "old");
        assert_eq!(hunk.lines[1].content, "new");
    }

    #[test]
    fn a_marker_with_nothing_to_annotate_becomes_a_note() {
        let patch = parse_patch(
            "\
diff --git a/one.txt b/one.txt
--- a/one.txt
+++ b/one.txt
@@ -1 +1 @@
\\ No newline at end of file
",
        );
        let file = &patch.files[0];
        assert!(
            file.notes
                .iter()
                .any(|note| note.contains("no-newline marker")),
            "{:?}",
            file.notes
        );
    }

    #[test]
    fn crlf_content_parses_and_keeps_its_text_without_the_carriage_return() {
        let patch = parse_patch(
            "diff --git a/win.txt b/win.txt\r\nindex 1111111..2222222 100644\r\n--- a/win.txt\r\n+++ b/win.txt\r\n@@ -1,2 +1,2 @@\r\n keep\r\n-old\r\n+new\r\n",
        );
        let file = &patch.files[0];
        assert_eq!(file.new_path.as_ref().unwrap().as_str(), "win.txt");
        let hunk = &file.hunks[0];
        assert_eq!(hunk.lines.len(), 3, "the headers still parsed");
        assert_eq!(hunk.lines[0].content, "keep", "no stray carriage return");
        assert_eq!(hunk.lines[2].content, "new");
        assert_eq!(file.additions, 1);
        assert_eq!(file.deletions, 1);
    }

    #[test]
    fn a_patch_without_a_final_newline_still_parses_its_last_line() {
        let patch = parse_patch(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new",
        );
        assert_eq!(patch.files[0].hunks[0].lines.len(), 2);
        assert_eq!(patch.files[0].hunks[0].lines[1].content, "new");
    }

    #[test]
    fn hunk_headers_may_omit_their_line_counts() {
        let patch = parse_patch(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let hunk = &patch.files[0].hunks[0];
        assert_eq!((hunk.old_lines, hunk.new_lines), (1, 1));
        assert_eq!(hunk.lines[0].old_line, Some(1));
        assert_eq!(hunk.lines[1].new_line, Some(1));
    }

    #[test]
    fn a_deleted_line_whose_text_starts_with_dashes_is_content_not_a_header() {
        // The trap: `--- foo` inside a hunk is a deletion of `-- foo`.
        let patch = parse_patch(
            "\
diff --git a/notes.md b/notes.md
index 1111111..2222222 100644
--- a/notes.md
+++ b/notes.md
@@ -1,3 +1,2 @@
 keep
--- an old separator
--- another one
",
        );
        let file = &patch.files[0];
        assert_eq!(file.deletions, 2);
        assert_eq!(file.hunks.len(), 1);
        let deleted: Vec<&str> = file.hunks[0]
            .lines
            .iter()
            .filter(|line| line.kind == LineKind::Delete)
            .map(|line| line.content.as_str())
            .collect();
        assert_eq!(deleted, vec!["-- an old separator", "-- another one"]);
    }

    #[test]
    fn quoted_paths_with_spaces_survive() {
        let patch = parse_patch(
            "\
diff --git \"a/docs/my notes.md\" \"b/docs/my notes.md\"
index 1111111..2222222 100644
--- \"a/docs/my notes.md\"
+++ \"b/docs/my notes.md\"
@@ -1 +1 @@
-a
+b
",
        );
        let file = &patch.files[0];
        assert_eq!(file.new_path.as_ref().unwrap().as_str(), "docs/my notes.md");
    }

    #[test]
    fn all_three_kinds_of_line_are_recognised_in_one_hunk() {
        let patch = parse_patch(
            "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,5 +1,5 @@
 one
-two
+2
 three
-four
+4
 five
",
        );
        let hunk = &patch.files[0].hunks[0];
        let kinds: Vec<LineKind> = hunk.lines.iter().map(|line| line.kind).collect();
        assert_eq!(
            kinds,
            vec![
                LineKind::Context,
                LineKind::Delete,
                LineKind::Add,
                LineKind::Context,
                LineKind::Delete,
                LineKind::Add,
                LineKind::Context
            ]
        );
        assert_eq!(patch.files[0].additions, 2);
        assert_eq!(patch.files[0].deletions, 2);
    }

    #[test]
    fn a_malformed_hunk_header_is_reported_and_does_not_lose_the_file() {
        let patch = parse_patch(
            "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -x,y +z,w @@
-a
+b
",
        );
        let file = &patch.files[0];
        assert_eq!(file.new_path.as_ref().unwrap().as_str(), "a.txt");
        assert!(file.hunks.is_empty());
        assert_eq!(file.kind(), FileKind::Empty);
        assert!(
            file.notes
                .iter()
                .any(|note| note.contains("unparsable hunk header")),
            "{:?}",
            file.notes
        );
    }

    #[test]
    fn content_before_a_hunk_header_is_reported_rather_than_dropped() {
        let patch = parse_patch(
            "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
+an orphan line
",
        );
        let file = &patch.files[0];
        assert!(
            file.notes
                .iter()
                .any(|note| note.contains("without a hunk")),
            "{:?}",
            file.notes
        );
    }

    #[test]
    fn unparsed_lines_are_quoted_a_few_times_then_counted() {
        let mut text = String::from("diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n");
        for index in 0..6 {
            let _ = writeln!(text, "+orphan {index}");
        }
        let file = &parse_patch(&text).files[0];
        assert_eq!(file.notes.len(), MAX_NOTES + 1);
        assert_eq!(file.notes.last().unwrap(), "and 3 more");
    }

    #[test]
    fn a_long_unparsed_line_is_shortened_in_the_note() {
        let long = "x".repeat(200);
        let patch = parse_patch(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n+{long}\n"
        ));
        let note = patch.files[0].notes.first().unwrap();
        assert!(note.ends_with('…'), "{note}");
        assert!(note.chars().count() < 90, "{} chars", note.chars().count());
    }

    #[test]
    fn a_hunk_ends_when_a_new_file_starts() {
        let patch = parse_patch(
            "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-a
+b
diff --git a/b.txt b/b.txt
--- a/b.txt
+++ b/b.txt
@@ -1 +1 @@
-c
+d
",
        );
        assert_eq!(patch.files.len(), 2);
        assert_eq!(patch.files[0].hunks.len(), 1);
        assert_eq!(patch.files[1].hunks.len(), 1);
        assert_eq!(patch.files[0].hunks[0].lines.len(), 2);
    }

    #[test]
    fn several_hunks_in_one_file_keep_their_own_numbers() {
        let patch = parse_patch(
            "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@ first
 a
-b
+B
 c
@@ -20,3 +22,4 @@ second
 x
-y
+Y
+Z
 z
",
        );
        let file = &patch.files[0];
        assert_eq!(file.hunks.len(), 2);
        assert_eq!(file.hunks[0].heading.as_deref(), Some("first"));
        assert_eq!(file.hunks[0].lines[1].old_line, Some(2));
        assert_eq!(file.hunks[1].heading.as_deref(), Some("second"));
        assert_eq!(file.hunks[1].old_start, 20);
        assert_eq!(file.hunks[1].new_start, 22);
        assert_eq!(file.hunks[1].lines[0].new_line, Some(22));
        assert_eq!(
            file.hunks[1].lines[3].new_line,
            Some(24),
            "the second addition"
        );
        assert_eq!(file.hunks[1].lines[4].new_line, Some(25));
        assert_eq!(file.additions, 3);
        assert_eq!(file.deletions, 2);
    }

    #[test]
    fn utf8_content_is_kept_whole() {
        let patch = parse_patch(
            "diff --git a/a.md b/a.md\n--- a/a.md\n+++ b/a.md\n@@ -1 +1 @@\n-olá\n+你好 🎉\n",
        );
        assert_eq!(patch.files[0].hunks[0].lines[1].content, "你好 🎉");
        assert_eq!(patch.files[0].additions, 1);
    }

    #[test]
    fn a_path_that_escapes_the_repository_is_refused_but_kept_visible() {
        let patch = parse_patch(
            "\
diff --git a/../../etc/passwd b/../../etc/passwd
--- a/../../etc/passwd
+++ b/../../etc/passwd
@@ -1 +1 @@
-a
+b
",
        );
        let file = &patch.files[0];
        assert!(file.new_path.is_none(), "an escaping path is not usable");
        assert_eq!(file.display_path(), "(unknown path)");
        assert_eq!(file.additions, 1, "the content is still accounted for");
    }

    #[test]
    fn an_empty_context_line_without_its_leading_space_is_tolerated() {
        let patch = parse_patch(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n a\n\n-b\n+B\n",
        );
        let hunk = &patch.files[0].hunks[0];
        assert_eq!(hunk.lines[1].kind, LineKind::Context);
        assert_eq!(hunk.lines[1].content, "");
        assert_eq!(hunk.lines[1].old_line, Some(2));
        assert_eq!(hunk.lines[1].new_line, Some(2));
    }

    #[test]
    fn parsing_is_linear_enough_for_a_large_patch() {
        // 10 000 changed lines across 200 files: the size NFR-1.3 cares about.
        let mut text = String::new();
        for file in 0..200 {
            let _ = writeln!(text, "diff --git a/f{file}.txt b/f{file}.txt");
            let _ = writeln!(text, "--- a/f{file}.txt\n+++ b/f{file}.txt");
            text.push_str("@@ -1,50 +1,50 @@\n");
            for line in 0..50 {
                let _ = writeln!(text, "-old {file} {line}");
                let _ = writeln!(text, "+new {file} {line}");
            }
        }
        let started = std::time::Instant::now();
        let patch = parse_patch(&text);
        let elapsed = started.elapsed();

        assert_eq!(patch.files.len(), 200);
        assert_eq!(patch.stats().additions, 10_000);
        assert_eq!(patch.stats().deletions, 10_000);
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "parsing took {elapsed:?}"
        );
    }

    #[test]
    fn a_patch_that_is_only_metadata_yields_a_file_with_notes() {
        let patch = parse_patch("diff --git a/a.txt b/a.txt\nindex 1111111..2222222 100644\n");
        assert_eq!(patch.files.len(), 1);
        assert_eq!(patch.files[0].kind(), FileKind::Empty);
        assert_eq!(patch.files[0].placeholder().unwrap(), "no changes");
    }

    #[test]
    fn unquote_handles_the_escapes_git_uses() {
        assert_eq!(unquote("\"with\\\"quote\""), "with\"quote");
        assert_eq!(unquote("\"back\\\\slash\""), "back\\slash");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(unquote("\"tab\\there\""), "tab\there");
        // An unknown escape is left as written rather than guessed at.
        assert_eq!(unquote("\"\\303\\251\""), "\\303\\251");
    }

    #[test]
    fn the_header_splitter_handles_quoted_and_bare_paths() {
        assert_eq!(
            split_git_header("a/one.rs b/one.rs"),
            Some(("a/one.rs".to_owned(), "b/one.rs".to_owned()))
        );
        assert_eq!(
            split_git_header("\"a/pa th\" \"b/pa th\""),
            Some(("a/pa th".to_owned(), "b/pa th".to_owned()))
        );
        assert_eq!(split_git_header("only-one-path"), None);
    }
}
