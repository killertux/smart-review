//! The review screen: tabs, file tree, and the diff (FR-3.3, FR-3.4).
//!
//! The diff pane is drawn from [`DiffView`](crate::tui::diff_view::DiffView)'s
//! flattened rows, and only the rows inside the viewport are turned into spans, so
//! the cost of a frame does not depend on the size of the patch (NFR-1.3).
//!
//! Below 140 columns the split view is not offered at all — the toggle says why
//! instead of drawing two unusable half-panes (DEC-4).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::domain::diff::{FileStatus, LineKind};
use crate::tui::app::{App, Pane};
use crate::tui::components::border_style;
use crate::tui::diff_view::{DiffRow, DiffView, RowKind, SplitRow, TreeKind, TreeRow};
use crate::tui::text;
use crate::tui::theme::{Theme, element};

/// The width below which the split view is not offered (DEC-4).
pub const SPLIT_MIN_WIDTH: u16 = 140;

/// The width of the file tree when both panes are shown.
pub const TREE_WIDTH: u16 = 34;

/// Renders the review screen (FR-3.3).
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(view) = app.review.as_ref() else {
        return;
    };

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).split(area);
    render_tabs(frame, rows[0], app);

    // The chat pane takes the bottom of the body when it is open, through the same
    // split the mouse handler uses (FR-7.5).
    let (body, chat) = super::chat::chat_split(rows[1], app.chat_state().is_some());
    let columns =
        Layout::horizontal([Constraint::Length(TREE_WIDTH), Constraint::Min(20)]).split(body);

    // The composer sits at the bottom of the diff pane, through the same split the
    // mouse handler uses, so a click lands where the drawing is (FR-6.2, FR-7.5).
    let (diff, composer) = super::drafts::composer_split(columns[1], app.drafts().is_composing());
    render_tree(frame, columns[0], app, view);
    render_diff(frame, diff, app, view);
    if let Some(composer_area) = composer
        && let Some(composer) = app.drafts().composer.as_ref()
    {
        super::drafts::render_composer(frame, composer_area, app, composer);
    }
    if let Some(chat) = chat {
        super::chat::render(frame, chat, app);
    }
}

/// The tab bar: which PR, and which view of it.
fn render_tabs(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let Some(detail) = app.detail.as_ref() else {
        return;
    };
    let mut spans = vec![
        Span::styled(
            format!(" #{} ", detail.summary.number),
            theme.style(element::TITLE),
        ),
        Span::styled(
            format!("{} ", text::truncate(&detail.summary.title, 60)),
            theme.style(element::FG),
        ),
        Span::styled("[1 Diff] ".to_owned(), theme.style(element::SELECTION)),
    ];

    // The tabs that are not built yet are shown greyed rather than hidden, so the
    // shape of the screen does not change under the user in M2.
    let approvals = detail.review_counts();
    for (label, available) in [
        (format!("2 Checks ({}) ", detail.checks.len()), false),
        (format!("3 Reviews ({}) ", detail.reviews.len()), false),
        (
            match app.chat_state() {
                Some(chat) => format!(
                    "4 Chat ({}) ",
                    chat.session
                        .as_ref()
                        .map_or(0, crate::domain::chat::Session::turns)
                ),
                None => "4 Chat ".to_owned(),
            },
            app.chat_state().is_some(),
        ),
        (
            format!("5 Analysis ({}, {}) ", approvals.0, approvals.1),
            false,
        ),
    ] {
        spans.push(Span::styled(
            label,
            if available {
                theme.style(element::FG)
            } else {
                theme.style(element::MUTED)
            },
        ));
    }
    if detail.comments.is_empty() {
        spans.push(Span::styled(
            "· no comments".to_owned(),
            theme.style(element::MUTED),
        ));
    } else {
        spans.push(Span::styled(
            format!("· {} comments", detail.comments.len()),
            theme.style(element::COMMENT_MARKER),
        ));
    }

    // Instead of a block, the row is painted edge to edge so it reads as a bar.
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(theme.style(element::BG)),
        area,
    );
}

/// The file tree with per-file stats and folder grouping (FR-3.3).
fn render_tree(frame: &mut Frame<'_>, area: Rect, app: &App, view: &DiffView) {
    let theme = &app.theme;
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style(app, Pane::Diff))
        .title(format!(" {} ", tree_title(view)));

    let height = usize::from(area.height.saturating_sub(2));
    // The offset is kept in the view by `prepare`, which the frame calls with the
    // real pane heights before anything is drawn.
    let scroll = view.tree_scroll;
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);

    for (index, row) in view.tree.iter().enumerate().skip(scroll).take(height) {
        let selected = view.tree_focused && index == view.tree_cursor;
        let current =
            matches!(row.kind, TreeKind::File { index } if Some(index) == view.current_file());
        lines.push(tree_line(theme, view, row, selected, current));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG)),
        area,
    );
}

/// The tree pane's title: the order in force (FR-3.5).
///
/// Only the order: the two positions are in the status line, which is wide enough to
/// show them without cutting them off, and a title that says half of something is
/// worse than a title that says one thing.
fn tree_title(view: &DiffView) -> String {
    format!("{} ({})", view.order.label(), view.patch.stats().files)
}

/// One tree row.
fn tree_line(
    theme: &Theme,
    view: &DiffView,
    row: &TreeRow,
    selected: bool,
    current_file: bool,
) -> Line<'static> {
    let base = if selected {
        theme.style(element::SELECTION)
    } else {
        theme.style(element::FG)
    };
    let tint = |style: ratatui::style::Style| if selected { base.patch(style) } else { style };

    let indent = "  ".repeat(usize::from(row.depth));
    match &row.kind {
        // A plan group is a heading, not a directory: it carries the position it is
        // read in, which is the whole point of the ordered view (FR-4.2).
        TreeKind::Group {
            order,
            files,
            folded,
            ..
        } => {
            let marker = if *folded { "▸" } else { "▾" };
            Line::from(vec![
                Span::styled(
                    format!(" {marker} {order}. "),
                    tint(theme.style(element::TREE_DIR)),
                ),
                Span::styled(
                    text::truncate(&format!("{} ({files})", row.label), 24),
                    tint(theme.style(element::ACCENT)),
                ),
            ])
        }
        // The rationale is why this group is read here (FR-4.2). It is dimmed and
        // indented under its heading so the list still scans as a list.
        TreeKind::Rationale { .. } => Line::from(vec![
            Span::styled("    ".to_owned(), base),
            Span::styled(
                text::truncate(&row.label, 24),
                tint(theme.style(element::MUTED)),
            ),
        ]),
        TreeKind::Directory { files, folded, .. } => {
            let marker = if *folded { "▸" } else { "▾" };
            Line::from(vec![
                Span::styled(
                    format!(" {indent}{marker} "),
                    tint(theme.style(element::TREE_DIR)),
                ),
                Span::styled(
                    text::truncate(&format!("{} ({files})", row.label), 20),
                    tint(theme.style(element::TREE_DIR)),
                ),
            ])
        }
        TreeKind::File { index } => {
            let file = view.patch.files.get(*index);
            let (marker, style) = match file.map(|file| (file.status, file.kind())) {
                Some((status, kind)) => (
                    marker_for(status, kind),
                    match status {
                        FileStatus::Added => theme.style(element::TREE_ADDED),
                        FileStatus::Deleted => theme.style(element::TREE_DELETED),
                        _ => theme.style(element::TREE_MODIFIED),
                    },
                ),
                None => ("?", theme.style(element::MUTED)),
            };
            let stats = file.map_or_else(String::new, crate::domain::diff::FileDiff::size_label);
            let cursor = if current_file { "·" } else { " " };
            // A file whose hunks are all folded says so, so the tree and the diff
            // pane cannot disagree about what is hidden (FR-3.3).
            let folded = if view.file_is_folded(*index) {
                "▸"
            } else {
                ""
            };
            Line::from(vec![
                Span::styled(format!(" {indent}{cursor}{marker}{folded}"), tint(style)),
                Span::styled(" ".to_owned(), base),
                Span::styled(
                    text::pad(&row.label, 17),
                    if current_file {
                        tint(theme.style(element::ACCENT))
                    } else {
                        base
                    },
                ),
                Span::styled(text::truncate(&stats, 9), tint(theme.style(element::MUTED))),
            ])
        }
    }
}

/// The letter a tree row shows for a file.
fn marker_for(status: FileStatus, kind: crate::domain::diff::FileKind) -> &'static str {
    crate::tui::diff_view::file_marker(status, kind)
}

/// The diff pane, unified or split (FR-3.3, DEC-4).
fn render_diff(frame: &mut Frame<'_>, area: Rect, app: &App, view: &DiffView) {
    let theme = &app.theme;
    // The threshold is a *terminal* width, not a pane width (DEC-4): 140 columns of
    // terminal has room for a tree and two usable halves, and the toggle uses the
    // same number, so the two can never disagree.
    let split = view.split && app.terminal_width() >= SPLIT_MIN_WIDTH;

    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style(app, Pane::Diff))
        .title(diff_title(theme, view, area.width, app));

    let height = area.height.saturating_sub(2);
    let mut lines = Vec::with_capacity(usize::from(height));

    let width = area.width.saturating_sub(2);
    if view.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "This pull request has no changes to show.".to_owned(),
            theme.style(element::MUTED),
        )));
    } else if split {
        // The split view walks the paired rows, so a replacement occupies one drawn
        // row instead of two.
        let window = view.split_window(height);
        for row in window {
            lines.push(split_line(theme, view, row, width, app.drafts()));
        }
    } else {
        for index in view.visible_rows(height) {
            let selected = index == view.cursor;
            lines.push(unified_line(
                theme,
                &view.rows[index],
                selected,
                width,
                view,
                app.drafts(),
            ));
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG)),
        area,
    );
}

/// The diff pane's title: what is being looked at, and the mode.
fn diff_title(theme: &Theme, view: &DiffView, width: u16, app: &App) -> String {
    let _ = width;
    let stats = view.patch.stats();
    let mode = if view.split && app.terminal_width() >= SPLIT_MIN_WIDTH {
        "split"
    } else if view.split {
        // Asked for split, too narrow: say so rather than silently drawing unified.
        "unified (split needs 140 columns)"
    } else {
        "unified"
    };
    // Only the context size in force is named: in remote mode `gh pr diff` always
    // sends three lines, so printing a configured 10 would be a claim about the pane
    // that is not true (FR-3.2).
    let title = format!(" {} · {} · ctx {} ", stats.label(), mode, view.context);
    let mut title = title;
    // The analysis's note about the file on screen, where the user already is. It is
    // truncated hard: the panel is where the full text lives, and a header that grows
    // without bound stops being a header.
    if let Some(note) = app.current_file_note() {
        let _ = std::fmt::Write::write_fmt(
            &mut title,
            format_args!("· {} ", text::truncate(&note, 40)),
        );
    }
    if app.diff_loading {
        title.push_str("· loading… ");
    }
    if app.diff_offline.is_some() {
        title.push_str("· cached ");
    }
    let _ = theme;
    title
}

/// One row of the unified view: gutters, marker, line.
///
/// The draft marker is a property of the *line*, so it is decided here, where the line
/// number and the file are both in hand, rather than by a second pass over the patch
/// (FR-6.1).
fn unified_line(
    theme: &Theme,
    row: &DiffRow,
    selected: bool,
    width: u16,
    view: &DiffView,
    drafts: &crate::tui::drafts::DraftState,
) -> Line<'static> {
    let gutter = gutter_style(theme, row, selected);
    let mut spans = vec![Span::styled(
        format!(
            "{}{}{}{}",
            text::pad_left(
                &row.old_line
                    .map_or_else(String::new, |line| line.to_string()),
                5
            ),
            text::pad_left(
                &row.new_line
                    .map_or_else(String::new, |line| line.to_string()),
                5
            ),
            row.line_kind.map_or(' ', LineKind::marker),
            draft_marker(view, drafts, row),
        ),
        if draft_marker(view, drafts, row) == '●' {
            theme.style(element::COMMENT_MARKER)
        } else {
            gutter
        },
    )];

    match row.kind {
        RowKind::FileHeader => {
            spans.clear();
            spans.push(Span::styled(
                format!("── {} ", row.text),
                if selected {
                    theme.style(element::SELECTION)
                } else {
                    theme.style(element::TITLE)
                },
            ));
        }
        RowKind::HunkHeader => {
            spans.clear();
            spans.push(Span::styled(
                format!("{} ", text::truncate(&row.text, usize::from(width))),
                body_style(theme, row, selected),
            ));
        }
        // A placeholder, a folded hunk and a line of the discussion about the line
        // above are all "some text, no line numbers": the only difference is the style
        // `body_style` gives them, and the discussion is indented by its own text.
        RowKind::Placeholder | RowKind::Folded | RowKind::Discussion => {
            spans.clear();
            spans.push(Span::styled(
                format!("   {} ", text::truncate(&row.text, usize::from(width))),
                body_style(theme, row, selected),
            ));
        }
        RowKind::Line => {
            let available = usize::from(width).saturating_sub(12);
            let content = if row.no_newline {
                format!(
                    "{} ⏎",
                    text::truncate(&row.text, available.saturating_sub(2))
                )
            } else {
                text::truncate(&row.text, available)
            };
            spans.push(Span::styled(content, body_style(theme, row, selected)));
        }
    }

    Line::from(spans)
}

/// One row of the split view: the old side beside the new side.
///
/// A full-width row (a banner, a hunk header, a placeholder) is drawn the same way
/// in both modes, so the eye has something to anchor on while scrolling sideways.
fn split_line(
    theme: &Theme,
    view: &DiffView,
    row: &SplitRow,
    width: u16,
    drafts: &crate::tui::drafts::DraftState,
) -> Line<'static> {
    if let Some(full) = &row.full {
        let selected = full_is_selected(view, full);
        return unified_line(theme, full, selected, width, view, drafts);
    }

    let half = usize::from(width).saturating_sub(3) / 2;
    let selected = row.covers(view.selected_unified(), view.selected_unified() + 1);
    let mut spans = Vec::with_capacity(6);

    for (left_side, line) in [(true, row.left.as_ref()), (false, row.right.as_ref())] {
        if !left_side {
            spans.push(Span::styled("│".to_owned(), theme.style(element::BORDER)));
        }
        match line {
            Some(line) => {
                // The old side is numbered on the left and the new side on the
                // right; using one number for both would mislabel half the rows.
                let number = if left_side {
                    line.old_line
                } else {
                    line.new_line
                };
                spans.push(Span::styled(
                    format!(
                        "{} ",
                        text::pad_left(
                            &number.map_or_else(String::new, |number| number.to_string()),
                            5
                        )
                    ),
                    if selected {
                        theme.style(element::SELECTION)
                    } else {
                        theme.style(element::DIFF_LINE_NUMBER)
                    },
                ));
                spans.push(Span::styled(
                    format!("{} ", text::truncate(&line.content, half.saturating_sub(8))),
                    body_style_for(theme, line.kind, selected),
                ));
            }
            None => {
                // An empty cell, which is how a one-sided change reads beside its
                // counterpart.
                spans.push(Span::styled(
                    " ".repeat(half.saturating_sub(1)),
                    theme.style(element::BG),
                ));
            }
        }
    }

    Line::from(spans)
}

/// The character that says a line already has a staged comment (FR-6.1).
///
/// Its own column rather than a decoration of the existing marker: the `+`/`-` gutter
/// says what the change is, and overwriting it would trade one answer for another.
fn draft_marker(view: &DiffView, drafts: &crate::tui::drafts::DraftState, row: &DiffRow) -> char {
    let Some(path) = view
        .patch
        .files
        .get(row.file)
        .and_then(crate::domain::diff::FileDiff::path)
    else {
        return ' ';
    };
    // A context line is on both sides; the comment is anchored to the side the user was
    // looking at, and both are marked, because the marker is about the line on screen.
    let marked = drafts.marks(
        path.as_str(),
        Some(crate::domain::draft::Side::New),
        row.new_line,
    ) || drafts.marks(
        path.as_str(),
        Some(crate::domain::draft::Side::Old),
        row.old_line,
    );
    if marked { '●' } else { ' ' }
}

/// Whether a full-width split row is the one the cursor is on.
fn full_is_selected(view: &DiffView, row: &DiffRow) -> bool {
    view.current().is_some_and(|current| {
        current.file == row.file && current.hunk == row.hunk && current.kind == row.kind
    })
}

/// The style for a diff line's body.
fn body_style(theme: &Theme, row: &DiffRow, selected: bool) -> ratatui::style::Style {
    let style = match row.kind {
        RowKind::HunkHeader => theme.style(element::DIFF_HUNK_HEADER),
        RowKind::Placeholder | RowKind::Folded => theme.style(element::DIFF_FOLDED),
        RowKind::FileHeader => theme.style(element::TITLE),
        RowKind::Discussion => theme.style(element::COMMENT_MARKER),
        RowKind::Line => match row.line_kind {
            Some(LineKind::Add) => theme.style(element::DIFF_ADD),
            Some(LineKind::Delete) => theme.style(element::DIFF_DEL),
            _ => theme.style(element::DIFF_CONTEXT),
        },
    };
    if selected {
        theme.style(element::SELECTION).patch(style)
    } else {
        style
    }
}

/// The style for one side of a split row.
fn body_style_for(theme: &Theme, kind: LineKind, selected: bool) -> ratatui::style::Style {
    let style = match kind {
        LineKind::Add => theme.style(element::DIFF_ADD),
        LineKind::Delete => theme.style(element::DIFF_DEL),
        LineKind::Context => theme.style(element::DIFF_CONTEXT),
    };
    if selected {
        theme.style(element::SELECTION).patch(style)
    } else {
        style
    }
}

/// The style for a diff line's gutter.
fn gutter_style(theme: &Theme, row: &DiffRow, selected: bool) -> ratatui::style::Style {
    let style = match row.line_kind {
        Some(LineKind::Add) => theme.style(element::DIFF_ADD),
        Some(LineKind::Delete) => theme.style(element::DIFF_DEL),
        _ => theme.style(element::DIFF_LINE_NUMBER),
    };
    if selected {
        theme.style(element::SELECTION).patch(style)
    } else {
        style
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::domain::diff::parse_patch;
    use crate::domain::pr::PullRequestDetail;
    use crate::test_support::{TempHome, temp_home};

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
";

    fn app_with_patch() -> (TempHome, App) {
        let dir = temp_home();
        let cli = Cli {
            repo: None,
            pr: None,
            path: None,
            remote: None,
            config: None,
            theme: None,
            home: Some(dir.path().to_path_buf()),
            log_level: None,
            check: false,
            dry_run: false,
        };
        let startup = crate::Startup::load(&cli).unwrap();
        let mut app = App::new(startup).unwrap();
        app.open_review(detail(), DiffView::new(parse_patch(PATCH)));
        (dir, app)
    }

    /// A detail good enough for the tabs, which read the PR's number and counts.
    fn detail() -> PullRequestDetail {
        let mut summary = crate::domain::pr::PullRequestSummary {
            number: 141,
            title: "Refactor the billing domain".to_owned(),
            author: "bruno".to_owned(),
            state: crate::domain::pr::PrState::Open,
            is_draft: false,
            base_ref: "main".to_owned(),
            head_ref: "topic".to_owned(),
            head_sha: "abc".to_owned(),
            created_at: crate::domain::time::Timestamp::default(),
            updated_at: crate::domain::time::Timestamp::default(),
            additions: 2,
            deletions: 1,
            changed_files: 1,
            labels: Vec::new(),
            review_decision: None,
            checks: crate::domain::pr::CheckSummary::default(),
            url: String::new(),
            is_cross_repository: false,
        };
        summary.number = 141;
        PullRequestDetail {
            summary,
            body: String::new(),
            merge_state_status: None,
            reviewers: Vec::new(),
            commits: Vec::new(),
            checks: Vec::new(),
            reviews: Vec::new(),
            comments: Vec::new(),
            conversation: Vec::new(),
            base_sha: None,
        }
    }

    /// Draws the whole screen, so the component sees the same geometry the loop
    /// gives it (the split threshold is a terminal-width decision).
    fn draw(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        crate::tui::test_support::buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn the_screen_shows_a_tree_and_a_diff() {
        let (_dir, mut app) = app_with_patch();
        let rendered = draw(&mut app, 120, 30);
        // The pane names the order it is in, which is what the toggle changes
        // (FR-3.5).
        assert!(rendered.contains("path order (1)"), "{rendered}");
        assert!(rendered.contains("invoice.rs"), "{rendered}");
        assert!(rendered.contains("unified"), "{rendered}");
        assert!(
            rendered.contains("impl Invoice"),
            "the hunk heading: {rendered}"
        );
        assert!(rendered.contains("gross - self.discount"), "{rendered}");
    }

    /// An app whose pull request is the one the analysis fixture describes.
    fn app_with_analysed_patch() -> (TempHome, App) {
        let (dir, mut app) = app_with_patch();
        app.set_environment(crate::test_support::environment());
        app.open_review(
            crate::test_support::analysis_detail(),
            DiffView::new(crate::test_support::analysis_patch()),
        );
        app.panel.analysis = Some(Box::new(crate::test_support::stored_analysis("abc123")));
        (dir, app)
    }

    #[test]
    fn the_plan_view_shows_groups_with_their_reason_and_both_positions() {
        let (_dir, mut app) = app_with_analysed_patch();
        let plan = crate::domain::plan::Plan::from_analysis(
            &app.panel.analysis.as_ref().expect("set").analysis,
        );
        if let Some(view) = app.review.as_mut() {
            view.set_plan(Some(plan));
        }
        let rendered = draw(&mut app, 140, 30);
        // FR-4.2: the heading, its position in the order, and the reason it is read
        // there. FR-3.5: where the file sits in both orders, in the status line where
        // there is room for it.
        assert!(rendered.contains("recommended order"), "{rendered}");
        assert!(rendered.contains("1. domain"), "{rendered}");
        assert!(rendered.contains("rules first"), "{rendered}");
        assert!(rendered.contains("plan ·"), "{rendered}");
        assert!(rendered.contains("path"), "{rendered}");
    }

    #[test]
    fn the_diff_header_carries_the_analysis_note_for_the_file_on_screen() {
        let (_dir, mut app) = app_with_analysed_patch();
        let rendered = draw(&mut app, 160, 24);
        // FR-4.1: what the analysis said about this file, where the reader already is.
        assert!(rendered.contains("check the sign"), "{rendered}");
    }

    #[test]
    fn the_tabs_name_the_pull_request_and_grey_out_what_is_not_built() {
        let (_dir, mut app) = app_with_patch();
        let rendered = draw(&mut app, 140, 30);
        assert!(rendered.contains("[1 Diff]"), "{rendered}");
        assert!(rendered.contains("2 Checks"), "{rendered}");
        assert!(rendered.contains("5 Analysis"), "{rendered}");
    }

    #[test]
    fn the_split_toggle_explains_itself_when_the_terminal_is_too_narrow() {
        let (_dir, mut app) = app_with_patch();
        if let Some(view) = app.review.as_mut() {
            view.split = true;
        }

        let narrow = draw(&mut app, 100, 24);
        assert!(
            narrow.contains("split needs 140 columns"),
            "the toggle must say why: {narrow}"
        );

        let wide = draw(&mut app, SPLIT_MIN_WIDTH, 24);
        assert!(wide.contains("split"), "{wide}");
        assert!(!wide.contains("needs 140"), "{wide}");
    }

    #[test]
    fn the_split_view_draws_both_sides_of_a_change() {
        let (_dir, mut app) = app_with_patch();
        if let Some(view) = app.review.as_mut() {
            view.split = true;
        }
        let rendered = draw(&mut app, 160, 24);
        // The deletion and the addition are paired, so both texts appear on one row
        // band and neither is lost.
        assert!(rendered.contains("self.lines.sum()"), "{rendered}");
        assert!(rendered.contains("let gross"), "{rendered}");
    }

    #[test]
    fn a_pane_that_is_too_short_still_renders_the_rows_that_fit() {
        let (_dir, mut app) = app_with_patch();
        let rendered = draw(&mut app, 100, 24);
        assert!(rendered.contains("order"), "{rendered}");
        assert!(rendered.contains("unified"), "{rendered}");
    }

    #[test]
    fn an_empty_patch_says_so_instead_of_showing_an_empty_pane() {
        let (_dir, mut app) = app_with_patch();
        app.set_review(DiffView::new(crate::domain::diff::Patch::default()));
        let rendered = draw(&mut app, 120, 24);
        assert!(rendered.contains("no changes to show"), "{rendered}");
    }

    #[test]
    fn the_tree_marks_the_file_the_cursor_is_in() {
        let (_dir, mut app) = app_with_patch();
        if let Some(view) = app.review.as_mut() {
            view.tree_focused = true;
        }
        let rendered = draw(&mut app, 120, 24);
        assert!(
            rendered.contains('·'),
            "the current-file marker: {rendered}"
        );
    }

    #[test]
    fn the_split_view_pairs_a_deletion_with_the_addition_that_replaced_it() {
        let (_dir, mut app) = app_with_patch();
        if let Some(view) = app.review.as_mut() {
            view.split = true;
        }
        let rendered = draw(&mut app, 160, 24);
        // One drawn row carries both the old and the new text.
        let pair = rendered
            .lines()
            .find(|line| line.contains("self.lines.sum()"))
            .unwrap_or_default();
        assert!(
            pair.contains("let gross"),
            "the deletion and its replacement should share a row: {pair:?}"
        );
    }

    #[test]
    fn prepare_keeps_the_tree_cursor_visible() {
        let mut view = DiffView::new(parse_patch(PATCH));
        for _ in 0..5 {
            view.move_tree(1);
        }
        view.prepare(4, 3);
        assert!(view.tree_scroll <= view.tree_cursor);
        assert!(view.tree_cursor < view.tree_scroll + 3);
    }
}
