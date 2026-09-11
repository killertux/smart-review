//! The diff view: the flattened row model, the file tree, and the cursor
//! (FR-3.3, FR-3.4).
//!
//! Rendering a 10 000-line patch by walking the model every frame is what makes a
//! diff viewer feel slow, so the patch is flattened **once** into a list of rows
//! that carry everything the renderer needs: the gutter numbers, the line kind, the
//! text, and which file and hunk they came from. Drawing then slices out the rows
//! that fit on screen and touches nothing else (NFR-1.3).
//!
//! The file tree is built from the same patch, once, with directory folding held
//! separately so folding is a rebuild rather than a re-parse.

use std::collections::BTreeSet;

use crate::domain::diff::{FileKind, FileStatus, LineKind, Patch, RelPath};

/// What a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// The banner that starts a file.
    FileHeader,
    /// A `@@` line.
    HunkHeader,
    /// A line of the hunk.
    Line,
    /// The sentence shown for a binary, mode-only or submodule file.
    Placeholder,
    /// The sentence shown for a folded hunk.
    Folded,
}

/// One drawable row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    /// What this row is.
    pub kind: RowKind,
    /// Which file it belongs to.
    pub file: usize,
    /// Which hunk, for a line or a hunk header.
    pub hunk: Option<usize>,
    /// The old-side line number, when there is one.
    pub old_line: Option<u32>,
    /// The new-side line number, when there is one.
    pub new_line: Option<u32>,
    /// The text to draw after the gutters.
    pub text: String,
    /// The line kind, for styling an addition or a deletion.
    pub line_kind: Option<LineKind>,
    /// Whether the file has no newline at the end here.
    pub no_newline: bool,
}

impl DiffRow {
    /// Whether moving to this row is moving to a different file.
    #[must_use]
    pub fn is_file_start(&self) -> bool {
        self.kind == RowKind::FileHeader
    }

    /// Whether this row begins a hunk.
    #[must_use]
    pub fn is_hunk_start(&self) -> bool {
        self.kind == RowKind::HunkHeader
    }
}

/// A row of the file tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// Nesting depth, for indentation.
    pub depth: u16,
    /// What to draw.
    pub label: String,
    /// What it stands for.
    pub kind: TreeKind,
}

/// What a tree row stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeKind {
    /// A directory, with the files underneath it.
    Directory {
        /// Its path, used as the fold key.
        path: String,
        /// How many files are under it.
        files: usize,
        /// Whether it is folded.
        folded: bool,
    },
    /// A file, by index into [`DiffView::patch`].
    File {
        /// The index.
        index: usize,
    },
}

/// A directory while the tree is being built.
#[derive(Debug, Default)]
struct DirBuilder {
    dirs: std::collections::BTreeMap<String, DirBuilder>,
    files: Vec<usize>,
}

impl DirBuilder {
    /// Adds a file at `segments`, which are the directories leading to it.
    fn insert(&mut self, segments: &[&str], index: usize) {
        match segments.split_first() {
            None => self.files.push(index),
            Some((head, rest)) => {
                self.dirs
                    .entry((*head).to_owned())
                    .or_default()
                    .insert(rest, index);
            }
        }
    }

    /// Flattens into drawable rows, honouring the folded directories.
    ///
    /// Directories come before files at each level, so a tree reads the way a file
    /// manager draws one rather than interleaving `src/` between two root files.
    fn flatten(&self, prefix: &str, depth: u16, folded: &BTreeSet<String>, out: &mut Vec<TreeRow>) {
        for (name, child) in &self.dirs {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let is_folded = folded.contains(&path);
            out.push(TreeRow {
                depth,
                label: name.clone(),
                kind: TreeKind::Directory {
                    path: path.clone(),
                    files: child.count(),
                    folded: is_folded,
                },
            });
            if !is_folded {
                child.flatten(&path, depth + 1, folded, out);
            }
        }
        for index in &self.files {
            out.push(TreeRow {
                depth,
                label: String::new(),
                kind: TreeKind::File { index: *index },
            });
        }
    }

    /// How many files are under this directory.
    fn count(&self) -> usize {
        self.files.len() + self.dirs.values().map(DirBuilder::count).sum::<usize>()
    }
}

/// The diff pane: a patch, the rows it flattens to, the tree, and the cursor.
#[derive(Debug, Clone)]
pub struct DiffView {
    /// The patch being read.
    pub patch: Patch,
    /// The flattened rows, rebuilt only when the patch or the folds change.
    pub rows: Vec<DiffRow>,
    /// The file tree, rebuilt with [`Self::rebuild`].
    pub tree: Vec<TreeRow>,
    /// The cursor, as an index into [`Self::rows`].
    pub cursor: usize,
    /// The first visible row.
    pub scroll: usize,
    /// The height of the diff pane, learned from the last frame so paging can use
    /// it.
    pub viewport: u16,
    /// Which hunks are folded, as `(file, hunk)`.
    pub folded_hunks: BTreeSet<(usize, usize)>,
    /// Which directories are folded, by path.
    pub folded_dirs: BTreeSet<String>,
    /// Whether the tree has the cursor rather than the diff.
    pub tree_focused: bool,
    /// The cursor inside the tree.
    pub tree_cursor: usize,
    /// Whether the split (side-by-side) view is asked for. Whether it can be
    /// *shown* is a width question, answered at draw time (DEC-4).
    pub split: bool,
    /// How many lines of context the diff was produced with (FR-3.2).
    pub context: u32,
    /// Whether the diff was produced with whitespace ignored.
    pub ignore_whitespace: bool,
    /// The number of files whose hunks are all folded, for the header.
    pub files: usize,
}

impl DiffView {
    /// Builds a view over a patch.
    #[must_use]
    pub fn new(patch: Patch) -> Self {
        let mut view = Self {
            files: patch.files.len(),
            patch,
            rows: Vec::new(),
            tree: Vec::new(),
            cursor: 0,
            scroll: 0,
            viewport: 0,
            folded_hunks: BTreeSet::new(),
            folded_dirs: BTreeSet::new(),
            tree_focused: false,
            tree_cursor: 0,
            split: false,
            context: 3,
            ignore_whitespace: false,
        };
        view.rebuild();
        view
    }

    /// Rebuilds the rows and the tree from the current patch and fold state.
    ///
    /// This is the only place that walks the patch, and it runs when the patch or a
    /// fold changes rather than per frame.
    pub fn rebuild(&mut self) {
        self.rows = flatten(&self.patch, &self.folded_hunks);
        self.tree = build_tree(&self.patch, &self.folded_dirs);
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        if self.tree_cursor >= self.tree.len() {
            self.tree_cursor = self.tree.len().saturating_sub(1);
        }
    }

    /// The row under the cursor.
    #[must_use]
    pub fn current(&self) -> Option<&DiffRow> {
        self.rows.get(self.cursor)
    }

    /// The file the cursor is in.
    #[must_use]
    pub fn current_file(&self) -> Option<usize> {
        self.current().map(|row| row.file)
    }

    /// The path of the file the cursor is in, which is what `:copy-path` copies.
    #[must_use]
    pub fn current_path(&self) -> Option<&RelPath> {
        self.patch.files.get(self.current_file()?)?.path()
    }

    /// Moves by whole rows, stopping at the ends.
    pub fn move_by(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        self.cursor = self
            .cursor
            .saturating_add_signed(delta as isize)
            .min(self.rows.len() - 1);
    }

    /// Moves to the first or last row.
    pub fn move_to(&mut self, last: bool) {
        if self.rows.is_empty() {
            return;
        }
        self.cursor = if last { self.rows.len() - 1 } else { 0 };
    }

    /// Moves by screens or half screens (FR-3.4).
    pub fn move_page(&mut self, direction: i32, half: bool) {
        let height = i32::from(self.viewport.max(1));
        let step = if half { (height / 2).max(1) } else { height };
        self.move_by(direction * step);
    }

    /// Moves to the next or previous hunk, wrapping into the neighbouring file
    /// (FR-3.4).
    pub fn move_hunk(&mut self, forward: bool) {
        if self.rows.is_empty() {
            return;
        }
        let range: Vec<usize> = if forward {
            (self.cursor + 1..self.rows.len()).collect()
        } else {
            (0..self.cursor).rev().collect()
        };
        // Land on the hunk *start*, which is where a reader wants to be: the line
        // after it is the first line of the hunk, not its header.
        if let Some(found) = range
            .into_iter()
            .find(|index| self.rows[*index].is_hunk_start())
        {
            self.cursor = found;
        }
    }

    /// Moves to the next or previous file (FR-3.4).
    pub fn move_file(&mut self, forward: bool) {
        if self.rows.is_empty() {
            return;
        }
        let range: Vec<usize> = if forward {
            (self.cursor + 1..self.rows.len()).collect()
        } else {
            (0..self.cursor).rev().collect()
        };
        if let Some(found) = range
            .into_iter()
            .find(|index| self.rows[*index].is_file_start())
        {
            self.cursor = found;
        }
    }

    /// Jumps to the first row of a file, which is what `Enter` in the tree does.
    pub fn goto_file(&mut self, index: usize) {
        if let Some(position) = self
            .rows
            .iter()
            .position(|row| row.kind == RowKind::FileHeader && row.file == index)
        {
            self.cursor = position;
            self.tree_cursor = self
                .tree
                .iter()
                .position(|row| matches!(row.kind, TreeKind::File { index: i } if i == index))
                .unwrap_or(self.tree_cursor);
        }
    }

    /// Fold or unfold the hunk under the cursor (FR-3.3).
    pub fn toggle_hunk(&mut self) {
        let Some(row) = self.current() else {
            return;
        };
        let (file, hunk) = match (row.file, row.hunk) {
            (file, Some(hunk)) => (file, hunk),
            // On a file banner, fold every hunk in the file.
            (file, None) => {
                let count = self.patch.files.get(file).map_or(0, |f| f.hunks.len());
                let fold = (0..count).any(|hunk| !self.folded_hunks.contains(&(file, hunk)));
                for hunk in 0..count {
                    if fold {
                        self.folded_hunks.insert((file, hunk));
                    } else {
                        self.folded_hunks.remove(&(file, hunk));
                    }
                }
                self.rebuild();
                return;
            }
        };
        if !self.folded_hunks.remove(&(file, hunk)) {
            self.folded_hunks.insert((file, hunk));
        }
        self.rebuild();
    }

    /// Whether the hunk under the cursor is folded.
    #[must_use]
    pub fn current_hunk_is_folded(&self) -> bool {
        self.current()
            .and_then(|row| row.hunk.map(|hunk| (row.file, hunk)))
            .is_some_and(|key| self.folded_hunks.contains(&key))
    }

    /// The tree row under the tree cursor.
    #[must_use]
    pub fn current_tree_row(&self) -> Option<&TreeRow> {
        self.tree.get(self.tree_cursor)
    }

    /// Moves the tree cursor.
    pub fn move_tree(&mut self, delta: i32) {
        if self.tree.is_empty() {
            return;
        }
        self.tree_cursor = self
            .tree_cursor
            .saturating_add_signed(delta as isize)
            .min(self.tree.len() - 1);
    }

    /// Activates the tree row under the cursor: folds a directory, opens a file.
    pub fn activate_tree(&mut self) {
        let Some(row) = self.tree.get(self.tree_cursor).cloned() else {
            return;
        };
        match row.kind {
            TreeKind::Directory { path, folded, .. } => {
                if folded {
                    self.folded_dirs.remove(&path);
                } else {
                    self.folded_dirs.insert(path);
                }
                self.rebuild();
            }
            TreeKind::File { index } => {
                self.tree_focused = false;
                self.goto_file(index);
            }
        }
    }

    /// How many rows can be drawn in the given height.
    #[must_use]
    pub fn visible_rows(&self, height: u16) -> std::ops::Range<usize> {
        let height = usize::from(height.max(1));
        let start = self.scroll.min(self.rows.len().saturating_sub(1));
        let end = (start + height).min(self.rows.len());
        start..end
    }

    /// Keeps the cursor inside the viewport, moving the scroll if it is not.
    ///
    /// Called by the renderer with the height it was given, which is how the view
    /// learns the size of its own window without the reducer needing to know it.
    pub fn ensure_visible(&mut self, height: u16) {
        let height = height.max(1);
        self.viewport = height;
        let height = usize::from(height);

        let last = self.rows.len().saturating_sub(1);
        self.cursor = self.cursor.min(last);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
    }

    /// The one-line summary the diff header shows.
    #[must_use]
    pub fn status_label(&self) -> String {
        let stats = self.patch.stats();
        let mode = if self.split { "split" } else { "unified" };
        let mut label = format!("{} · {mode}", stats.label());
        if self.ignore_whitespace {
            label.push_str(" · -w");
        }
        if label.len() > 60 {
            label.truncate(60);
        }
        label
    }

    /// What the file under the cursor is, for the status line.
    #[must_use]
    pub fn current_file_label(&self) -> Option<String> {
        let file = self.patch.files.get(self.current_file()?)?;
        Some(format!(
            "{} {} {}",
            file.status.marker(),
            file.display_path(),
            file.size_label()
        ))
    }
}

/// Flattens a patch into drawable rows.
fn flatten(patch: &Patch, folded: &BTreeSet<(usize, usize)>) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for (file_index, file) in patch.files.iter().enumerate() {
        rows.push(DiffRow {
            kind: RowKind::FileHeader,
            file: file_index,
            hunk: None,
            old_line: None,
            new_line: None,
            text: format!("{} {}", file.status.marker(), file.display_path()),
            line_kind: None,
            no_newline: false,
        });

        if let Some(placeholder) = file.placeholder() {
            rows.push(DiffRow {
                kind: RowKind::Placeholder,
                file: file_index,
                hunk: None,
                old_line: None,
                new_line: None,
                text: placeholder,
                line_kind: None,
                no_newline: false,
            });
            continue;
        }

        for (hunk_index, hunk) in file.hunks.iter().enumerate() {
            rows.push(DiffRow {
                kind: RowKind::HunkHeader,
                file: file_index,
                hunk: Some(hunk_index),
                old_line: None,
                new_line: None,
                text: hunk.header(),
                line_kind: None,
                no_newline: false,
            });

            if folded.contains(&(file_index, hunk_index)) {
                rows.push(DiffRow {
                    kind: RowKind::Folded,
                    file: file_index,
                    hunk: Some(hunk_index),
                    old_line: None,
                    new_line: None,
                    text: format!("… {} lines folded (za to open)", hunk.lines.len()),
                    line_kind: None,
                    no_newline: false,
                });
                continue;
            }

            for line in &hunk.lines {
                rows.push(DiffRow {
                    kind: RowKind::Line,
                    file: file_index,
                    hunk: Some(hunk_index),
                    old_line: line.old_line,
                    new_line: line.new_line,
                    text: line.content.clone(),
                    line_kind: Some(line.kind),
                    no_newline: line.no_newline,
                });
            }
        }
    }
    rows
}

/// Groups the patch's files into a tree, with the files in name order.
fn build_tree(patch: &Patch, folded: &BTreeSet<String>) -> Vec<TreeRow> {
    // Sorted by path so the tree reads like a directory listing rather than like
    // git's output order, which puts renames wherever they happened.
    let mut entries: Vec<(String, usize)> = patch
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            (
                file.path()
                    .map_or_else(|| format!("(unknown {index})"), RelPath::to_string),
                index,
            )
        })
        .collect();
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut root = DirBuilder::default();
    for (path, index) in &entries {
        let segments: Vec<&str> = path.split('/').collect();
        // `split` always yields at least one element, so the last is the file name
        // and everything before it is the directory path.
        let (_, dirs) = segments.split_last().unwrap_or((&"", &[]));
        root.insert(dirs, *index);
    }

    let mut rows = Vec::new();
    root.flatten("", 0, folded, &mut rows);
    // The `flatten` walk puts files under their directory, so the file rows carry
    // the name through the label lookup below.
    for row in &mut rows {
        if let TreeKind::File { index } = row.kind
            && let Some((path, _)) = entries.iter().find(|(_, other)| *other == index)
        {
            path.rsplit('/')
                .next()
                .unwrap_or(path.as_str())
                .clone_into(&mut row.label);
        }
    }
    rows
}

/// The `za` description of a file's state, used by the tree and the header.
#[must_use]
pub fn file_marker(status: FileStatus, kind: FileKind) -> &'static str {
    match kind {
        FileKind::Binary => "B",
        FileKind::ModeOnly => "M",
        FileKind::Submodule => "S",
        FileKind::Empty => match status {
            FileStatus::Renamed => "R",
            FileStatus::Copied => "C",
            _ => " ",
        },
        FileKind::Text => status.marker(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::diff::parse_patch;

    const PATCH: &str = "\
diff --git a/src/domain/invoice.rs b/src/domain/invoice.rs
index 1a2b3c4..5d6e7f8 100644
--- a/src/domain/invoice.rs
+++ b/src/domain/invoice.rs
@@ -12,3 +14,4 @@ impl Invoice {
     pub fn total(&self) -> Money {
-        self.lines.sum()
+        let gross = self.lines.sum();
+        gross - self.discount
     }
diff --git a/README.md b/README.md
index 1111111..2222222 100644
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # Smart Review
+New line.
diff --git a/docs/logo.png b/docs/logo.png
new file mode 100644
index 0000000..2222222
Binary files /dev/null and b/docs/logo.png differ
";

    fn view() -> DiffView {
        DiffView::new(parse_patch(PATCH))
    }

    #[test]
    fn a_patch_flattens_into_file_hunks_and_lines() {
        let view = view();
        let kinds: Vec<RowKind> = view.rows.iter().map(|row| row.kind).collect();
        assert_eq!(
            kinds,
            vec![
                RowKind::FileHeader,
                RowKind::HunkHeader,
                RowKind::Line, // context
                RowKind::Line, // deletion
                RowKind::Line, // addition
                RowKind::Line, // addition
                RowKind::Line, // context
                RowKind::FileHeader,
                RowKind::HunkHeader,
                RowKind::Line,
                RowKind::Line,
                RowKind::FileHeader,
                RowKind::Placeholder, // the binary file
            ]
        );
    }

    #[test]
    fn rows_carry_the_gutter_numbers_and_the_text() {
        let view = view();
        let deletion = view
            .rows
            .iter()
            .find(|row| row.line_kind == Some(LineKind::Delete))
            .unwrap();
        assert_eq!(deletion.old_line, Some(13));
        assert_eq!(deletion.new_line, None);
        assert_eq!(deletion.text, "        self.lines.sum()");

        let addition = view
            .rows
            .iter()
            .find(|row| row.text.contains("let gross"))
            .unwrap();
        assert_eq!(addition.old_line, None);
        assert_eq!(addition.new_line, Some(15));
    }

    #[test]
    fn a_binary_file_shows_its_placeholder_instead_of_lines() {
        let view = view();
        let last = view.rows.last().unwrap();
        assert_eq!(last.kind, RowKind::Placeholder);
        assert_eq!(last.text, "binary file, not shown");
    }

    #[test]
    fn the_tree_groups_files_by_directory() {
        let view = view();
        let labels: Vec<(u16, String)> = view
            .tree
            .iter()
            .map(|row| (row.depth, row.label.clone()))
            .collect();

        // Directories first at each level, then files, each alphabetically.
        assert_eq!(labels[0], (0, "docs".to_owned()));
        assert_eq!(labels[1], (1, "logo.png".to_owned()));
        assert_eq!(labels[2], (0, "src".to_owned()));
        assert_eq!(labels[3], (1, "domain".to_owned()));
        assert_eq!(labels[4], (2, "invoice.rs".to_owned()));
        assert_eq!(labels[5], (0, "README.md".to_owned()));
    }

    #[test]
    fn a_directory_reports_how_many_files_are_under_it() {
        let view = view();
        let src = view.tree.iter().find(|row| row.label == "src").unwrap();
        match &src.kind {
            TreeKind::Directory { files, folded, .. } => {
                assert_eq!(*files, 1);
                assert!(!folded);
            }
            other @ TreeKind::File { .. } => panic!("expected a directory, got {other:?}"),
        }
    }

    #[test]
    fn folding_a_directory_hides_its_files_and_keeps_them_counted() {
        let mut view = view();
        let before = view.tree.len();
        view.tree_cursor = view.tree.iter().position(|row| row.label == "src").unwrap();
        view.activate_tree();

        assert!(view.tree.len() < before);
        assert!(!view.tree.iter().any(|row| row.label == "invoice.rs"));
        let src = view.tree.iter().find(|row| row.label == "src").unwrap();
        assert!(matches!(
            src.kind,
            TreeKind::Directory {
                folded: true,
                files: 1,
                ..
            }
        ));

        // Unfolding brings them back.
        view.activate_tree();
        assert!(view.tree.iter().any(|row| row.label == "invoice.rs"));
    }

    #[test]
    fn opening_a_file_from_the_tree_moves_the_diff_cursor_to_it() {
        let mut view = view();
        let index = view
            .tree
            .iter()
            .position(|row| row.label == "README.md")
            .unwrap();
        view.tree_cursor = index;
        view.tree_focused = true;
        view.activate_tree();

        assert!(!view.tree_focused, "focus moves to the diff");
        assert_eq!(view.current().unwrap().kind, RowKind::FileHeader);
        assert_eq!(view.current_path().unwrap().as_str(), "README.md");
    }

    #[test]
    fn the_cursor_moves_by_rows_and_stops_at_the_ends() {
        let mut view = view();
        assert_eq!(view.cursor, 0);
        view.move_by(-1);
        assert_eq!(view.cursor, 0);
        view.move_to(true);
        assert_eq!(view.cursor, view.rows.len() - 1);
        view.move_by(1);
        assert_eq!(view.cursor, view.rows.len() - 1);
        view.move_to(false);
        assert_eq!(view.cursor, 0);
    }

    #[test]
    fn hunk_navigation_lands_on_hunk_headers_and_crosses_files() {
        let mut view = view();
        view.move_hunk(true);
        assert!(view.current().unwrap().is_hunk_start());
        assert_eq!(view.current().unwrap().file, 0);

        view.move_hunk(true);
        assert_eq!(view.current().unwrap().file, 1, "the next file's hunk");
        let last_hunk = view.cursor;
        view.move_hunk(true);
        assert_eq!(
            view.cursor, last_hunk,
            "there is nothing after the last hunk"
        );

        view.move_hunk(false);
        assert_eq!(view.current().unwrap().file, 0);
        view.move_hunk(false);
        assert_eq!(view.cursor, 1, "and nothing before the first");
    }

    #[test]
    fn file_navigation_lands_on_file_banners() {
        let mut view = view();
        view.move_file(true);
        assert_eq!(view.current().unwrap().file, 1);
        assert!(view.current().unwrap().is_file_start());
        view.move_file(true);
        assert_eq!(view.current().unwrap().file, 2);
        let last_file = view.cursor;
        view.move_file(true);
        assert_eq!(
            view.cursor, last_file,
            "there is nothing after the last file"
        );

        view.move_file(false);
        assert_eq!(view.current().unwrap().file, 1);
        view.move_file(false);
        assert_eq!(view.current().unwrap().file, 0);
        view.move_file(false);
        assert_eq!(view.cursor, 0, "and nothing before the first");
    }

    #[test]
    fn folding_a_hunk_replaces_its_lines_with_one_row() {
        let mut view = view();
        let before = view.rows.len();
        view.move_hunk(true);
        assert!(!view.current_hunk_is_folded());

        view.toggle_hunk();
        assert!(view.folded_hunks.contains(&(0, 0)));
        assert!(
            view.current_hunk_is_folded(),
            "the cursor stays on the header"
        );
        assert_eq!(view.rows[view.cursor + 1].kind, RowKind::Folded);
        assert!(view.rows.len() < before);

        // The folded row says how much is hidden.
        let folded = &view.rows[view.cursor + 1];
        assert!(folded.text.contains("5 lines folded"), "{}", folded.text);

        view.toggle_hunk();
        assert_eq!(view.rows.len(), before, "unfolding restores the lines");
    }

    #[test]
    fn folding_a_file_banner_folds_every_hunk_in_it() {
        let mut view = view();
        // The cursor starts on the first file banner.
        view.toggle_hunk();
        assert!(view.folded_hunks.contains(&(0, 0)));
        assert_eq!(view.rows[1].kind, RowKind::HunkHeader);
        assert_eq!(view.rows[2].kind, RowKind::Folded);

        view.toggle_hunk();
        assert!(view.folded_hunks.is_empty(), "and unfolds them again");
    }

    #[test]
    fn the_viewport_scrolls_to_keep_the_cursor_visible() {
        let mut view = view();
        view.ensure_visible(5);
        assert_eq!(view.viewport, 5);
        assert_eq!(view.scroll, 0);

        view.cursor = 8;
        view.ensure_visible(5);
        assert_eq!(view.scroll, 4, "the cursor is at the bottom of the window");

        view.cursor = 1;
        view.ensure_visible(5);
        assert_eq!(view.scroll, 1, "and at the top when it moves up");

        // The visible range is what the renderer draws.
        let range = view.visible_rows(5);
        assert_eq!(range, 1..6);
    }

    #[test]
    fn a_height_of_zero_does_not_divide_by_zero_or_hide_the_cursor() {
        let mut view = view();
        view.ensure_visible(0);
        assert_eq!(view.viewport, 1);
        assert_eq!(view.scroll, view.cursor);
        assert_eq!(view.visible_rows(0).len(), 1);
    }

    #[test]
    fn paging_uses_the_height_of_the_last_frame() {
        let mut view = view();
        view.ensure_visible(4);
        view.move_page(1, false);
        assert_eq!(view.cursor, 4);
        view.move_page(1, true);
        assert_eq!(view.cursor, 6);
        view.move_page(-1, false);
        assert_eq!(view.cursor, 2);
        view.move_page(-1, true);
        assert_eq!(view.cursor, 0);
    }

    #[test]
    fn an_empty_patch_is_a_view_with_nothing_to_draw() {
        let mut view = DiffView::new(Patch::default());
        assert!(view.rows.is_empty());
        assert!(view.tree.is_empty());
        assert!(view.current().is_none());
        assert!(view.current_path().is_none());
        assert!(view.current_file_label().is_none());
        view.move_by(1);
        view.move_hunk(true);
        view.move_file(true);
        view.toggle_hunk();
        view.move_to(true);
        assert_eq!(view.cursor, 0);
        assert_eq!(view.status_label(), "0 files +0 −0 · unified");
    }

    #[test]
    fn the_status_line_names_the_mode_and_the_whitespace_setting() {
        let mut view = view();
        assert!(
            view.status_label().ends_with("· unified"),
            "{}",
            view.status_label()
        );
        view.split = true;
        assert!(view.status_label().contains("· split"));
        view.ignore_whitespace = true;
        assert!(view.status_label().contains("· -w"));
    }

    #[test]
    fn the_current_file_label_says_what_changed() {
        let mut view = view();
        assert_eq!(
            view.current_file_label().unwrap(),
            "M src/domain/invoice.rs +2 −1"
        );
        view.move_file(true);
        assert_eq!(view.current_file_label().unwrap(), "M README.md +1");
        view.move_file(true);
        assert!(view.current_file_label().unwrap().starts_with('A'));
    }

    #[test]
    fn the_current_path_is_the_path_a_file_would_be_opened_at() {
        let mut view = view();
        assert_eq!(
            view.current_path().unwrap().as_str(),
            "src/domain/invoice.rs"
        );
        view.move_file(true);
        assert_eq!(view.current_path().unwrap().as_str(), "README.md");
    }

    #[test]
    fn every_file_marker_is_distinguishable() {
        assert_eq!(file_marker(FileStatus::Added, FileKind::Text), "A");
        assert_eq!(file_marker(FileStatus::Deleted, FileKind::Text), "D");
        assert_eq!(file_marker(FileStatus::Renamed, FileKind::Empty), "R");
        assert_eq!(file_marker(FileStatus::Copied, FileKind::Empty), "C");
        assert_eq!(file_marker(FileStatus::Modified, FileKind::Binary), "B");
        assert_eq!(file_marker(FileStatus::Modified, FileKind::ModeOnly), "M");
        assert_eq!(file_marker(FileStatus::Modified, FileKind::Submodule), "S");
    }

    #[test]
    fn a_large_patch_flattens_once_and_slices_cheaply() {
        // 200 files x 100 lines: the size NFR-1.3 cares about.
        let mut text = String::new();
        for file in 0..200 {
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!(
                    "diff --git a/dir{file}/f.rs b/dir{file}/f.rs\n--- a/dir{file}/f.rs\n+++ b/dir{file}/f.rs\n@@ -1,50 +1,50 @@\n"
                ),
            );
            for line in 0..50 {
                let _ = std::fmt::Write::write_fmt(
                    &mut text,
                    format_args!("-old {file} {line}\n+new {file} {line}\n"),
                );
            }
        }
        let patch = parse_patch(&text);
        let started = std::time::Instant::now();
        let mut view = DiffView::new(patch);
        let built = started.elapsed();

        assert_eq!(view.rows.len(), 200 * (2 + 100));
        assert!(
            built < std::time::Duration::from_secs(2),
            "building took {built:?}"
        );

        // Slicing the visible window allocates nothing and touches 40 rows.
        view.cursor = 10_000;
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            let range = view.visible_rows(40);
            let visible = &view.rows[range];
            assert_eq!(visible.len(), 40);
        }
        let scrolling = started.elapsed();
        assert!(
            scrolling < std::time::Duration::from_millis(500),
            "a thousand frames of scrolling took {scrolling:?}"
        );
    }

    #[test]
    fn a_rename_is_shown_with_both_paths_and_grouped_under_the_new_one() {
        let patch = parse_patch(
            "diff --git a/old/name.rs b/new/name.rs\nsimilarity index 90%\nrename from old/name.rs\nrename to new/name.rs\n",
        );
        let view = DiffView::new(patch);
        assert_eq!(view.rows[0].text, "R old/name.rs → new/name.rs");
        assert!(view.tree.iter().any(|row| row.label == "new"));
        assert_eq!(view.current_path().unwrap().as_str(), "new/name.rs");
    }
}
