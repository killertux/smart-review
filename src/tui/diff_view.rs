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

use crate::domain::diff::{DiffLine, FileKind, FileStatus, LineKind, Patch, RelPath};

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
    /// A line of an existing discussion, drawn under the line it is about (FR-6.4).
    Discussion,
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

/// A row of the side-by-side view.
///
/// Pairs are computed when the view is rebuilt, not when it is drawn: pairing a run
/// of deletions with the additions that replaced them is the only part of the split
/// view with any logic in it, and it happens once per patch rather than once per
/// frame (NFR-1.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitRow {
    /// Set when the row spans the full width: file banners, hunk headers,
    /// placeholders and folded summaries.
    pub full: Option<DiffRow>,
    /// The old side, when this row has one.
    pub left: Option<DiffLine>,
    /// The new side, when this row has one.
    pub right: Option<DiffLine>,
    /// The file the row belongs to.
    pub file: usize,
    /// The first unified row this split row covers, so the cursor and the split view
    /// stay about the same thing.
    pub unified: usize,
}

impl SplitRow {
    /// The unified row to show as selected when `cursor` is on either half.
    #[must_use]
    pub fn covers(&self, cursor: usize, next_unified: usize) -> bool {
        cursor >= self.unified && cursor < next_unified
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
    /// A review-plan group (FR-4.2): a heading the ordered view is read through.
    Group {
        /// The group's name.
        name: String,
        /// Where it sits in the recommended order, from one.
        order: u32,
        /// How many files are in it.
        files: usize,
        /// Whether its files are folded.
        folded: bool,
    },
    /// The sentence explaining why a group is read where it is (FR-4.2).
    Rationale {
        /// The explanation.
        text: String,
    },
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
    /// The same content paired up for the side-by-side view.
    pub split_rows: Vec<SplitRow>,
    /// For each unified row, the split row that shows it.
    split_index: Vec<usize>,
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
    /// The first visible row of the tree.
    pub tree_scroll: usize,
    /// How many tree rows fit, learned from the last frame.
    pub tree_viewport: u16,
    /// Whether the split (side-by-side) view is asked for. Whether it can be
    /// *shown* is a width question, answered at draw time (DEC-4).
    pub split: bool,
    /// How many lines of context the diff was produced with (FR-3.2). Read-only in
    /// M1: a remote diff always carries three, and M2's local re-diff is what makes
    /// it adjustable.
    pub context: u32,
    /// Whether the diff was produced with whitespace ignored. Read-only in M1, for
    /// the same reason as [`Self::context`].
    pub ignore_whitespace: bool,
    /// The reviews and comments GitHub already has, grouped by the line they are on
    /// (FR-6.4). Read-only in v1: replying is M5, and drawing them where they belong
    /// is what makes the diff readable next to a discussion about it.
    pub comments: Vec<crate::domain::pr::ReviewComment>,
    /// Which order the tree is in (FR-3.5).
    pub order: crate::domain::plan::OrderMode,
    /// The review plan, when there is one. The heuristic plan is present as soon as a
    /// patch is, so the recommended order always works (FR-3.5, DEC-10).
    pub plan: Option<crate::domain::plan::Plan>,
}

impl DiffView {
    /// Builds a view over a patch with the configured diff options.
    #[must_use]
    pub fn with_options(patch: Patch, context: u32, ignore_whitespace: bool) -> Self {
        let mut view = Self::new(patch);
        view.context = context;
        view.ignore_whitespace = ignore_whitespace;
        view
    }

    /// Builds a view over a patch.
    #[must_use]
    pub fn new(patch: Patch) -> Self {
        let mut view = Self {
            patch,
            rows: Vec::new(),
            split_rows: Vec::new(),
            split_index: Vec::new(),
            tree: Vec::new(),
            cursor: 0,
            scroll: 0,
            viewport: 0,
            folded_hunks: BTreeSet::new(),
            folded_dirs: BTreeSet::new(),
            tree_focused: false,
            tree_cursor: 0,
            tree_scroll: 0,
            tree_viewport: 0,
            split: false,
            context: 3,
            ignore_whitespace: false,
            order: crate::domain::plan::OrderMode::Path,
            plan: None,
            comments: Vec::new(),
        };
        view.rebuild();
        view
    }

    /// Rebuilds the rows and the tree from the current patch and fold state.
    ///
    /// This is the only place that walks the patch, and it runs when the patch or a
    /// fold changes rather than per frame.
    pub fn rebuild(&mut self) {
        self.rows = flatten(&self.patch, &self.folded_hunks, &self.comments);
        let (split, index) = build_split(&self.rows);
        self.split_rows = split;
        self.split_index = index;
        self.tree = self.build_tree();
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        if self.tree_cursor >= self.tree.len() {
            self.tree_cursor = self.tree.len().saturating_sub(1);
        }
    }

    /// The paths the patch changed, in the patch's own order.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        self.patch
            .files
            .iter()
            .filter_map(|file| file.path().map(ToString::to_string))
            .collect()
    }

    /// Sets the existing discussion, drawn inline (FR-6.4).
    pub fn set_comments(&mut self, comments: &[crate::domain::pr::ReviewComment]) {
        if self.comments == comments {
            return;
        }
        // The cursor is kept on the same *line* rather than the same row index: adding
        // rows above it would otherwise move what the user is reading.
        let anchor = self
            .current()
            .map(|row| (row.file, row.new_line, row.old_line));
        self.comments = comments.to_vec();
        self.rebuild();
        if let Some((file, new_line, old_line)) = anchor
            && let Some(index) = self.rows.iter().position(|row| {
                row.kind == RowKind::Line
                    && row.file == file
                    && row.new_line == new_line
                    && row.old_line == old_line
            })
        {
            self.cursor = index;
        }
    }

    /// Sets the review plan, keeping the order mode.
    pub fn set_plan(&mut self, plan: Option<crate::domain::plan::Plan>) {
        self.plan = plan;
        // With a plan available the recommended order is the useful default, which is
        // the whole point of analysing a pull request (FR-3.5). Without one the tree
        // stays in path order rather than in a heuristic order the user did not ask
        // for.
        if self.plan.is_some() && self.order == crate::domain::plan::OrderMode::Path {
            self.order = crate::domain::plan::OrderMode::Recommended;
        }
        self.rebuild_tree();
    }

    /// Switches between the recommended and path orders, keeping the current file.
    ///
    /// "Preserves the current file when possible" is a requirement (FR-3.5), and it is
    /// also the only way the toggle does not feel like it moved the ground.
    pub fn set_order(&mut self, order: crate::domain::plan::OrderMode) {
        if self.order == order {
            return;
        }
        let file = self.current_file();
        self.order = order;
        self.rebuild_tree();
        if let Some(file) = file {
            self.focus_file(file);
        }
    }

    /// Toggles between the two orders (FR-3.5, `o`).
    pub fn toggle_order(&mut self) {
        self.set_order(self.order.toggled());
    }

    /// Rebuilds the tree alone, which is all an order change needs.
    pub fn rebuild_tree(&mut self) {
        self.tree = self.build_tree();
        if self.tree_cursor >= self.tree.len() {
            self.tree_cursor = self.tree.len().saturating_sub(1);
        }
    }

    /// Moves the tree cursor to a file, wherever it is in the current tree.
    pub fn focus_file(&mut self, file: usize) {
        if let Some(index) = self
            .tree
            .iter()
            .position(|row| matches!(row.kind, TreeKind::File { index } if index == file))
        {
            self.tree_cursor = index;
        }
    }

    /// The tree for the current order.
    fn build_tree(&self) -> Vec<TreeRow> {
        if self.order == crate::domain::plan::OrderMode::Recommended
            && let Some(plan) = &self.plan
        {
            return build_plan_tree(plan, &self.patch, &self.folded_dirs);
        }
        build_tree(&self.patch, &self.folded_dirs)
    }

    /// How the current file sits in both orders, for the tree's header (FR-3.5).
    ///
    /// The two numbers are what make the toggle honest: the user can see that the file
    /// they are reading is seventh by path and second by plan before pressing anything.
    #[must_use]
    pub fn order_positions(&self) -> Option<String> {
        let plan = self.plan.as_ref()?;
        let path = self.current_path()?.to_string();
        let paths = self.paths();
        let recommended =
            plan.position_of(&path, crate::domain::plan::OrderMode::Recommended, &paths)?;
        let by_path = plan.position_of(&path, crate::domain::plan::OrderMode::Path, &paths)?;
        // Short, because the tree pane's title is 34 columns wide and a position that
        // is cut off is a position nobody can read (FR-3.5).
        let total = plan.len(&paths);
        Some(format!(
            "{recommended}/{total} plan · {by_path}/{total} path"
        ))
    }

    /// The split row that shows the cursor, and the rows after it.
    ///
    /// The renderer walks this instead of the unified rows when the side-by-side
    /// view is on, so a paired deletion and addition occupy one drawn row.
    #[must_use]
    pub fn split_window(&self, height: u16) -> &[SplitRow] {
        if self.split_rows.is_empty() {
            return &[];
        }
        let first = self
            .split_index
            .get(self.cursor)
            .copied()
            .unwrap_or_default();
        let height = usize::from(height.max(1));
        let end = (first + height).min(self.split_rows.len());
        &self.split_rows[first..end]
    }

    /// Which unified row the split view should show as selected.
    #[must_use]
    pub fn selected_unified(&self) -> usize {
        self.cursor
    }

    /// Updates everything that depends on the size of the panes.
    ///
    /// Called by the renderer, which is the only thing that knows how tall the panes
    /// are; it is arithmetic, not IO, so the render path stays free of side effects
    /// on the world.
    pub fn prepare(&mut self, diff_height: u16, tree_height: u16) {
        self.ensure_visible(diff_height);
        self.tree_viewport = tree_height.max(1);
        self.tree_scroll = crate::tui::components::ensure_visible(
            self.tree_cursor,
            self.tree_scroll,
            usize::from(self.tree_viewport),
            self.tree.len(),
        );
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

    /// Scrolls the diff view by `delta` rows, dragging the cursor if it would be left
    /// outside the window.
    pub fn scroll_by(&mut self, delta: i32) {
        crate::tui::components::scroll_view(
            &mut self.cursor,
            &mut self.scroll,
            delta,
            usize::from(self.viewport.max(1)),
            self.rows.len(),
        );
    }

    /// Scrolls the file tree by `delta` rows.
    pub fn scroll_tree_by(&mut self, delta: i32) {
        self.tree_focused = true;
        crate::tui::components::scroll_view(
            &mut self.tree_cursor,
            &mut self.tree_scroll,
            delta,
            usize::from(self.tree_viewport.max(1)),
            self.tree.len(),
        );
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

    /// Puts the cursor on a row of the *window*, clamped to the rows that exist.
    ///
    /// What a click means: the pointer names a visible row, and the caller has already
    /// added the scroll offset, so this is an absolute index into [`Self::rows`].
    pub fn select_row(&mut self, index: usize) {
        if self.rows.is_empty() {
            return;
        }
        self.cursor = index.min(self.rows.len() - 1);
    }

    /// Puts the tree cursor on a row, and gives the tree the focus.
    pub fn select_tree_row(&mut self, index: usize) {
        if self.tree.is_empty() {
            return;
        }
        self.tree_focused = true;
        self.tree_cursor = index.min(self.tree.len() - 1);
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

    /// Whether every hunk of `file` is folded, which is what the tree shows so a
    /// collapsed file is visible without opening it (FR-3.3).
    #[must_use]
    pub fn file_is_folded(&self, file: usize) -> bool {
        let hunks = self.patch.files.get(file).map_or(0, |f| f.hunks.len());
        hunks > 0 && (0..hunks).all(|hunk| self.folded_hunks.contains(&(file, hunk)))
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
            // A group folds like a directory: the rationale and its files are the
            // detail, and the reader may want only the shape (FR-4.2).
            TreeKind::Group { name, folded, .. } => {
                if folded {
                    self.folded_dirs.remove(&name);
                } else {
                    self.folded_dirs.insert(name);
                }
                self.rebuild_tree();
            }
            // The rationale is not a thing to act on; pressing Enter on it does
            // nothing rather than something surprising.
            TreeKind::Rationale { .. } => {}
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
        self.viewport = height.max(1);
        let height = usize::from(self.viewport);
        // The shared rule, so the diff and the list agree on what "visible" means and a
        // view the user scrolled by hand is not dragged back.
        self.scroll = crate::tui::components::ensure_visible(
            self.cursor,
            self.scroll,
            height,
            self.rows.len(),
        );
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    /// The one-line summary the diff header shows.
    #[must_use]
    pub fn status_label(&self) -> String {
        let stats = self.patch.stats();
        let mode = if self.split { "split" } else { "unified" };
        crate::tui::text::truncate(&format!("{} · {mode}", stats.label()), 60)
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

/// Builds the tree the ordered view shows: one group per plan step, with its
/// rationale above the files (FR-3.5, FR-4.2).
///
/// The files are the *same* files the path tree shows, referenced by index into the
/// patch, so switching orders never touches the diff model and never re-runs git
/// (FR-3.5).
fn build_plan_tree(
    plan: &crate::domain::plan::Plan,
    patch: &Patch,
    folded: &BTreeSet<String>,
) -> Vec<TreeRow> {
    let by_path: std::collections::BTreeMap<&str, usize> = patch
        .files
        .iter()
        .enumerate()
        .filter_map(|(index, file)| Some((file.path()?.as_str(), index)))
        .collect();
    let mut rows = Vec::new();
    for group in &plan.groups {
        let present: Vec<usize> = group
            .files
            .iter()
            .filter_map(|path| by_path.get(path.as_str()).copied())
            .collect();
        if present.is_empty() {
            continue;
        }
        let is_folded = folded.contains(&group.group);
        rows.push(TreeRow {
            depth: 0,
            // The renderer adds the number, so the label is only the name.
            label: group.group.clone(),
            kind: TreeKind::Group {
                name: group.group.clone(),
                order: group.order,
                files: present.len(),
                folded: is_folded,
            },
        });
        if !group.rationale.trim().is_empty() && !is_folded {
            rows.push(TreeRow {
                depth: 0,
                label: group.rationale.clone(),
                kind: TreeKind::Rationale {
                    text: group.rationale.clone(),
                },
            });
        }
        if is_folded {
            continue;
        }
        for index in present {
            // The label is the file's own name, exactly as the path tree sets it, so
            // the two orders name the same file the same way.
            let label = patch.files.get(index).map_or_else(String::new, |file| {
                let full = file.path().map_or_else(String::new, RelPath::to_string);
                full.rsplit('/').next().unwrap_or(full.as_str()).to_owned()
            });
            rows.push(TreeRow {
                depth: 1,
                label,
                kind: TreeKind::File { index },
            });
        }
    }
    rows
}

/// Flattens a patch into drawable rows.
fn flatten(
    patch: &Patch,
    folded: &BTreeSet<(usize, usize)>,
    comments: &[crate::domain::pr::ReviewComment],
) -> Vec<DiffRow> {
    let threads = threads_by_line(patch, comments);
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
                // FR-6.4: the discussion about this line, under it. The *side* is part
                // of the lookup: a context line is on both sides at once, and a comment
                // anchored to one of them would otherwise be drawn twice — which is how
                // this first read "one short comment" as two rows.
                let lookups = [(false, line.new_line), (true, line.old_line)];
                for (old_side, side_line) in lookups {
                    let Some(line_number) = side_line else {
                        continue;
                    };
                    let Some(thread) = threads.get(&(file_index, old_side, line_number)) else {
                        continue;
                    };
                    for text in thread {
                        rows.push(DiffRow {
                            kind: RowKind::Discussion,
                            file: file_index,
                            hunk: Some(hunk_index),
                            old_line: line.old_line,
                            new_line: line.new_line,
                            text: text.clone(),
                            line_kind: None,
                            no_newline: false,
                        });
                    }
                }
            }
        }
    }
    rows
}

/// Pairs the flattened rows for the side-by-side view.
///
/// A run of deletions is matched against the run of additions that follows it, index
/// by index, which is how a replacement reads as one row with an old side and a new
/// side. Context lines appear on both sides. Unbalanced runs leave one side empty
/// rather than shifting everything after them.
///
/// Returns the rows and, for each unified row, the index of the split row that shows
/// it.
/// How wide a discussion line is wrapped to, before the pane truncates it.
///
/// A fixed number rather than the pane's width because a row is built once and drawn at
/// whatever width the terminal has: wrapping at draw time would mean rebuilding rows on
/// every resize, and this is the compromise that keeps scrolling cheap.
const DISCUSSION_WRAP: usize = 100;

/// The existing discussion, keyed by the file and line it is about (FR-6.4).
///
/// Grouped by *thread*, not by comment: a reply belongs under the comment it answers,
/// and GitHub reports the two as one flat list with an `in_reply_to` link. A reply whose
/// parent is not in the list (deleted since, or on another page) starts a thread of its
/// own rather than disappearing.
///
/// Each thread becomes a list of ready-to-draw lines, because a row is one terminal row:
/// a body of three lines takes three rows, and the wrapping happens here so that what a
/// row holds is what the renderer truncates.
fn threads_by_line(
    patch: &Patch,
    comments: &[crate::domain::pr::ReviewComment],
) -> std::collections::HashMap<(usize, bool, u32), Vec<String>> {
    use std::collections::{HashMap, HashSet};

    let by_id: HashMap<u64, usize> = comments
        .iter()
        .enumerate()
        .map(|(index, comment)| (comment.id, index))
        .collect();
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    let mut is_root: HashSet<usize> = HashSet::new();
    let mut claimed: HashSet<usize> = HashSet::new();
    for (index, comment) in comments.iter().enumerate() {
        match comment
            .in_reply_to
            .and_then(|parent| by_id.get(&parent).copied())
        {
            Some(parent) if parent != index => {
                children.entry(parent).or_default().push(index);
                claimed.insert(index);
            }
            _ => {
                roots.push(index);
                is_root.insert(index);
            }
        }
    }

    let mut threads: HashMap<(usize, bool, u32), Vec<String>> = HashMap::new();
    for root in roots {
        let Some((anchored_path, old_side, line)) = anchor_of(&comments[root]) else {
            continue;
        };
        // A file index rather than a path: the caller looks rows up by file, and the
        // same path can appear twice in a patch (a rename).
        let Some(file) = patch.files.iter().position(|candidate| {
            candidate
                .path()
                .is_some_and(|own| own.as_str() == anchored_path)
        }) else {
            continue;
        };
        let mut lines = Vec::new();
        write_thread(&mut lines, comments, root, &children, 0);
        threads
            .entry((file, old_side, line))
            .or_default()
            .append(&mut lines);
    }

    // A reply whose root is gone is still a comment: it is drawn as its own thread
    // rather than dropped, which is what keeps the count in the tab honest.
    for (index, comment) in comments.iter().enumerate() {
        if claimed.contains(&index) || is_root.contains(&index) {
            continue;
        }
        let Some((anchored_path, old_side, line)) = anchor_of(comment) else {
            continue;
        };
        let Some(file) = patch.files.iter().position(|candidate| {
            candidate
                .path()
                .is_some_and(|own| own.as_str() == anchored_path)
        }) else {
            continue;
        };
        let mut lines = Vec::new();
        write_thread(&mut lines, comments, index, &children, 0);
        threads
            .entry((file, old_side, line))
            .or_default()
            .append(&mut lines);
    }

    threads
}

/// One comment and its replies, as lines.
fn write_thread(
    lines: &mut Vec<String>,
    comments: &[crate::domain::pr::ReviewComment],
    index: usize,
    children: &std::collections::HashMap<usize, Vec<usize>>,
    depth: usize,
) {
    let comment = &comments[index];
    let indent = "  ".repeat(depth);
    let author = if comment.author.trim().is_empty() {
        "someone"
    } else {
        comment.author.trim()
    };
    // The side is worth naming: line 31 is two different lines in a hunk, and a comment
    // on the old one is usually about what was removed.
    let side = match comment.side.as_deref() {
        Some("LEFT") => " (old side)",
        _ => "",
    };
    let body = comment
        .body
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let opening = format!("{indent}{} {author}{side}: ", thread_marker(depth));
    for (position, line) in wrap_text(&format!("{opening}{body}"), DISCUSSION_WRAP)
        .into_iter()
        .enumerate()
    {
        if position == 0 {
            lines.push(line);
        } else {
            lines.push(format!("{indent}   {line}"));
        }
    }
    for child in children.get(&index).into_iter().flatten() {
        write_thread(lines, comments, *child, children, depth + 1);
    }
}

/// The character in front of a thread's first line.
fn thread_marker(depth: usize) -> &'static str {
    if depth == 0 { "▸" } else { "↳" }
}

/// Wraps text at `width` columns, keeping the paragraph as one block.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split(' ') {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push("(empty)".to_owned());
    }
    lines
}

/// The file, side and line a comment is anchored to, when GitHub still knows it.
fn anchor_of(comment: &crate::domain::pr::ReviewComment) -> Option<(&str, bool, u32)> {
    let line = u32::try_from(comment.line?).ok()?;
    let old_side = comment.side.as_deref() == Some("LEFT");
    Some((comment.path.as_str(), old_side, line))
}

fn build_split(rows: &[DiffRow]) -> (Vec<SplitRow>, Vec<usize>) {
    let mut split: Vec<SplitRow> = Vec::with_capacity(rows.len());
    let mut index = Vec::with_capacity(rows.len());

    let mut position = 0;
    while position < rows.len() {
        let row = &rows[position];

        if row.kind != RowKind::Line {
            index.push(split.len());
            split.push(SplitRow {
                full: Some(row.clone()),
                left: None,
                right: None,
                file: row.file,
                unified: position,
            });
            position += 1;
            continue;
        }

        // Collect the deletions, then the additions that follow.
        let mut deletions: Vec<DiffLine> = Vec::new();
        let mut additions: Vec<DiffLine> = Vec::new();
        let start = position;
        while position < rows.len() && rows[position].line_kind == Some(LineKind::Delete) {
            index.push(split.len());
            deletions.push(as_line(&rows[position]));
            position += 1;
        }
        while position < rows.len() && rows[position].line_kind == Some(LineKind::Add) {
            index.push(split.len());
            additions.push(as_line(&rows[position]));
            position += 1;
        }

        if deletions.is_empty() && additions.is_empty() {
            // A context line keeps both sides.
            let line = as_line(row);
            index.push(split.len());
            split.push(SplitRow {
                full: None,
                left: Some(line.clone()),
                right: Some(line),
                file: row.file,
                unified: position,
            });
            position += 1;
            continue;
        }

        // The pairing for a run: deletion i against addition i.
        for slot in 0..deletions.len().max(additions.len()) {
            split.push(SplitRow {
                full: None,
                left: deletions.get(slot).cloned(),
                right: additions.get(slot).cloned(),
                file: row.file,
                unified: start,
            });
        }
    }

    (split, index)
}

/// Turns a flattened row back into the line it came from.
fn as_line(row: &DiffRow) -> DiffLine {
    DiffLine {
        kind: row.line_kind.unwrap_or(LineKind::Context),
        old_line: row.old_line,
        new_line: row.new_line,
        content: row.text.clone(),
        no_newline: row.no_newline,
    }
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
mod discussion_tests {
    use super::*;
    use crate::domain::pr::ReviewComment;
    use crate::domain::time::from_unix_secs;

    const PATCH: &str = concat!(
        "diff --git a/src/domain/money.rs b/src/domain/money.rs\n",
        "--- a/src/domain/money.rs\n",
        "+++ b/src/domain/money.rs\n",
        "@@ -1,3 +1,3 @@\n",
        " pub fn round(cents: i64) -> i64 {\n",
        "-    cents\n",
        "+    (cents + 5) / 10 * 10\n",
        " }\n",
    );

    fn comment(
        id: u64,
        author: &str,
        line: u64,
        body: &str,
        reply_to: Option<u64>,
    ) -> ReviewComment {
        ReviewComment {
            id,
            author: author.to_owned(),
            path: "src/domain/money.rs".to_owned(),
            line: Some(line),
            side: Some("RIGHT".to_owned()),
            body: body.to_owned(),
            created_at: from_unix_secs(0),
            in_reply_to: reply_to,
            diff_hunk: None,
            url: None,
            thread_id: None,
            resolved: false,
            outdated: false,
        }
    }

    fn view_with(comments: &[ReviewComment]) -> DiffView {
        let mut view = DiffView::new(crate::domain::diff::parse_patch(PATCH));
        view.set_comments(comments);
        view
    }

    #[test]
    fn a_comment_is_drawn_under_the_line_it_is_about() {
        let view = view_with(&[comment(1, "alice", 2, "this rounds up", None)]);
        let rows: Vec<&DiffRow> = view
            .rows
            .iter()
            .filter(|row| row.kind == RowKind::Discussion)
            .collect();
        assert_eq!(rows.len(), 1, "one row for one short comment");
        assert!(rows[0].text.contains("alice"), "{}", rows[0].text);
        assert!(rows[0].text.contains("this rounds up"), "{}", rows[0].text);
        // And it is *after* the line it belongs to, which is what "under" means.
        let annotated = view
            .rows
            .iter()
            .position(|row| row.kind == RowKind::Line && row.new_line == Some(2))
            .expect("the added line");
        let drawn = view
            .rows
            .iter()
            .position(|row| row.kind == RowKind::Discussion)
            .expect("the comment");
        assert_eq!(drawn, annotated + 1);
    }

    #[test]
    fn a_reply_is_drawn_under_the_comment_it_answers() {
        let view = view_with(&[
            comment(1, "alice", 2, "this rounds up", None),
            comment(2, "bruno", 2, "fixed, thank you", Some(1)),
        ]);
        let texts: Vec<String> = view
            .rows
            .iter()
            .filter(|row| row.kind == RowKind::Discussion)
            .map(|row| row.text.clone())
            .collect();
        assert_eq!(texts.len(), 2);
        assert!(texts[0].contains("alice"), "{texts:?}");
        assert!(texts[1].contains("bruno"), "{texts:?}");
        assert!(texts[1].starts_with("↳"), "marked as a reply: {texts:?}");
    }

    #[test]
    fn a_reply_whose_parent_is_gone_is_still_drawn() {
        // GitHub reports a flat list and deletions happen: a reply with no visible
        // parent is a comment, not a reason to draw nothing.
        let view = view_with(&[comment(2, "bruno", 2, "fixed, thank you", Some(999))]);
        assert_eq!(
            view.rows
                .iter()
                .filter(|row| row.kind == RowKind::Discussion)
                .count(),
            1
        );
    }

    #[test]
    fn a_long_comment_takes_several_rows_and_stays_readable() {
        let long = "word ".repeat(80);
        let view = view_with(&[comment(1, "alice", 2, long.trim(), None)]);
        let rows: Vec<&DiffRow> = view
            .rows
            .iter()
            .filter(|row| row.kind == RowKind::Discussion)
            .collect();
        assert!(rows.len() > 1, "a long body wraps");
        assert!(
            rows.iter()
                .all(|row| row.text.chars().count() <= DISCUSSION_WRAP + 8),
            "no row is wider than the wrap plus its indent"
        );
        // The first row carries the author; the rest are continuation lines.
        assert!(rows[0].text.contains("alice"));
        assert!(rows[1].text.starts_with("   "), "{:?}", rows[1].text);
    }

    #[test]
    fn a_comment_on_a_line_the_diff_does_not_show_is_skipped() {
        // An outdated comment (the pull request moved) has a line number that is no
        // longer in the patch. Drawing it at the end of the file would be worse than
        // not drawing it: it would look like a comment about that line.
        let view = view_with(&[comment(1, "alice", 400, "old news", None)]);
        assert!(view.rows.iter().all(|row| row.kind != RowKind::Discussion));
    }

    #[test]
    fn setting_the_same_comments_twice_does_not_move_the_cursor() {
        let mut view = view_with(&[comment(1, "alice", 2, "this rounds up", None)]);
        view.cursor = view.rows.len() - 1;
        let before = view.cursor;
        view.set_comments(&[comment(1, "alice", 2, "this rounds up", None)]);
        assert_eq!(view.cursor, before, "nothing changed, so nothing moved");
    }

    #[test]
    fn the_cursor_stays_on_its_line_when_the_discussion_arrives() {
        // The comments arrive with the detail, which can be a frame after the diff: the
        // user is already reading a line, and inserting rows above it must not move it.
        let mut view = DiffView::new(crate::domain::diff::parse_patch(PATCH));
        view.cursor = 3;
        let before = (
            view.rows[3].file,
            view.rows[3].new_line,
            view.rows[3].old_line,
        );
        view.set_comments(&[comment(1, "alice", 1, "the first line", None)]);
        let now = &view.rows[view.cursor];
        assert_eq!((now.file, now.new_line, now.old_line), before);
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
    fn the_recommended_order_groups_the_files_and_explains_each_group() {
        let mut view = view();
        let paths = view.paths();
        assert!(paths.len() >= 2, "{paths:?}");
        let plan = crate::domain::plan::Plan::heuristic("head1", &paths);
        view.set_plan(Some(plan));

        // A plan switches the view to the recommended order, because that is what
        // analysing a pull request is for (FR-3.5).
        assert_eq!(view.order, crate::domain::plan::OrderMode::Recommended);
        let groups = view
            .tree
            .iter()
            .filter(|row| matches!(row.kind, TreeKind::Group { .. }))
            .count();
        assert!(groups >= 1, "{:?}", view.tree);
        // Every group that is shown explains its position (FR-4.2).
        for (index, row) in view.tree.iter().enumerate() {
            if matches!(row.kind, TreeKind::Group { .. }) {
                assert!(
                    matches!(
                        view.tree.get(index + 1).map(|next| &next.kind),
                        Some(TreeKind::Rationale { .. })
                    ),
                    "a group without a rationale: {row:?}"
                );
            }
        }
        // And every file is still there, exactly once.
        let files: Vec<usize> = view
            .tree
            .iter()
            .filter_map(|row| match row.kind {
                TreeKind::File { index } => Some(index),
                _ => None,
            })
            .collect();
        assert_eq!(files.len(), paths.len(), "{files:?}");
    }

    #[test]
    fn switching_orders_keeps_the_file_the_cursor_is_in() {
        let mut view = view();
        let paths = view.paths();
        view.set_plan(Some(crate::domain::plan::Plan::heuristic("head1", &paths)));
        // Put the cursor in the second file of the patch.
        view.goto_file(1);
        let file = view.current_file();
        assert_eq!(file, Some(1));
        view.set_order(crate::domain::plan::OrderMode::Path);
        assert_eq!(view.order, crate::domain::plan::OrderMode::Path);
        // The diff cursor did not move, and the tree highlights the same file.
        assert_eq!(view.current_file(), Some(1));
        assert_eq!(
            view.tree_cursor,
            view.tree
                .iter()
                .position(|row| matches!(row.kind, TreeKind::File { index } if index == 1))
                .unwrap()
        );
        assert_eq!(
            view.tree.get(view.tree_cursor).map(|row| &row.kind),
            Some(&TreeKind::File { index: 1 })
        );
        view.toggle_order();
        assert_eq!(view.order, crate::domain::plan::OrderMode::Recommended);
        assert_eq!(view.current_file(), Some(1));
        assert_eq!(
            view.tree.get(view.tree_cursor).map(|row| &row.kind),
            Some(&TreeKind::File { index: 1 }),
            "the file is still selected after toggling back"
        );
    }

    #[test]
    fn a_group_folds_like_a_directory() {
        let mut view = view();
        let paths = view.paths();
        view.set_plan(Some(crate::domain::plan::Plan::heuristic("head1", &paths)));
        let before = view.tree.len();
        let group = view
            .tree
            .iter()
            .position(|row| matches!(row.kind, TreeKind::Group { .. }))
            .expect("a group");
        view.tree_cursor = group;
        view.activate_tree();
        assert!(view.tree.len() < before, "the group folded");
        view.activate_tree();
        assert_eq!(view.tree.len(), before, "and it opens again");
    }

    #[test]
    fn the_header_says_where_the_file_is_in_each_order() {
        let mut view = view();
        let paths = view.paths();
        view.set_plan(Some(crate::domain::plan::Plan::heuristic("head1", &paths)));
        view.goto_file(0);
        let positions = view.order_positions().expect("both orders know the file");
        assert!(positions.contains("plan"), "{positions}");
        assert!(positions.contains("path"), "{positions}");
        assert!(
            positions.contains(&format!("/{} plan", paths.len())),
            "{positions}"
        );
        // The shape is "position/total plan · position/total path", which is what the
        // status line has room for (FR-3.5).
        assert_eq!(
            positions,
            format!("1/{} plan · 1/{} path", paths.len(), paths.len())
        );
    }

    #[test]
    fn without_a_plan_the_view_stays_in_path_order() {
        let view = view();
        assert_eq!(view.order, crate::domain::plan::OrderMode::Path);
        assert!(view.plan.is_none());
        assert!(view.order_positions().is_none());
        assert!(
            !view
                .tree
                .iter()
                .any(|row| matches!(row.kind, TreeKind::Group { .. }))
        );
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
            other => panic!("expected a directory, got {other:?}"),
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
    fn a_click_selects_an_absolute_row() {
        let mut view = view();
        view.select_row(6);
        assert_eq!(view.cursor, 6);
        view.select_row(usize::MAX);
        assert_eq!(view.cursor, view.rows.len() - 1, "clamped, not wrapped");

        view.select_tree_row(3);
        assert_eq!(view.tree_cursor, 3);
        assert!(view.tree_focused);
        view.select_tree_row(usize::MAX);
        assert_eq!(view.tree_cursor, view.tree.len() - 1);

        // An empty view stays at zero rather than panicking.
        let mut empty = DiffView::new(Patch::default());
        empty.select_row(4);
        empty.select_tree_row(4);
        assert_eq!(empty.cursor, 0);
        assert_eq!(empty.tree_cursor, 0);
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
    fn a_folded_file_is_visible_as_folded() {
        let mut view = view();
        assert!(!view.file_is_folded(0));
        // The cursor starts on the first file banner, so `za` folds the whole file.
        view.toggle_hunk();
        assert!(view.file_is_folded(0), "the tree can show it as collapsed");
        assert!(!view.file_is_folded(1), "the other file is not folded");

        // A file with no hunks is not "folded": there is nothing hidden.
        let empty = DiffView::new(crate::domain::diff::parse_patch(
            "diff --git a/a.txt b/a.txt
new file mode 100644
index 0000000..2222222
Binary files /dev/null and b/a.txt differ
",
        ));
        assert!(!empty.file_is_folded(0));
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
    fn the_status_line_names_the_mode_it_is_actually_in() {
        let mut view = view();
        assert!(
            view.status_label().ends_with("· unified"),
            "{}",
            view.status_label()
        );
        view.split = true;
        assert!(view.status_label().contains("· split"));
        // The whitespace setting is *not* named: a remote diff cannot honour it, and
        // printing "-w" would claim the pane is showing something it is not (FR-3.2).
        view.ignore_whitespace = true;
        assert!(
            !view.status_label().contains("-w"),
            "{}",
            view.status_label()
        );
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
