//! The pull request list pane (FR-2.1, FR-2.2, FR-2.3).
//!
//! A table drawn by hand rather than with `ratatui::widgets::Table`: the columns
//! need per-cell styling (a failing check is red, a draft is muted) and the rows
//! need to be cut to the pane's width in *columns*, which `text::truncate` does and
//! `Table` does not.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::domain::pr::{CheckState, PullRequestSummary, ReviewDecision};
use crate::tui::app::{App, Pane};
use crate::tui::components::border_style;
use crate::tui::list_view::PrListState;
use crate::tui::text;
use crate::tui::theme::{Theme, element};

/// The fixed part of the header row and the column widths.
struct Columns {
    number: usize,
    title: usize,
    size: usize,
    author: usize,
    updated: usize,
    checks: usize,
    decision: usize,
}

impl Columns {
    /// Works out the columns for a pane width, giving the title what is left.
    fn for_width(width: u16) -> Self {
        let width = usize::from(width);
        // Every fixed column is as wide as its own heading, so no heading is ever
        // cut off; the title takes what remains, and at least ten columns.
        let number = 6;
        let size = 13; // "±218 −43" and its sign characters
        let author = 12;
        let updated = 7; // "Updated"
        let checks = 6; // "Checks"
        let decision = 4; // "Dec"
        let gaps = 6; // one space after each of the six middle columns
        let fixed = 2 + number + 2 + gaps + size + author + updated + checks + decision;
        let title = width.saturating_sub(fixed).max(10);
        Self {
            number,
            title,
            size,
            author,
            updated,
            checks,
            decision,
        }
    }

    /// Total columns the table needs, which a narrow pane must not exceed.
    ///
    /// Only the tests ask: the rule under the header spans the pane, so the table's
    /// own width is never needed to draw, but it *is* what a narrow terminal has to
    /// fit into.
    #[cfg(test)]
    fn total(&self) -> usize {
        2 + self.number
            + 1
            + 1
            + self.title
            + 1
            + self.author
            + 1
            + self.updated
            + 1
            + self.checks
            + 1
            + self.decision
    }
}

/// Where the rows of the list pane are.
///
/// The pane has a border, a column header and a rule above the first row, and a
/// border below the last. Drawing, scrolling and hit-testing all read this, so a
/// click cannot land on a different row from the one it was aimed at (FR-7.5) — which
/// is exactly what happened while two of them counted the rows independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListLayout {
    /// The first row of the pane, in terminal coordinates.
    pub first_row: u16,
    /// How many rows fit.
    pub height: usize,
}

/// The row layout of a list pane.
#[must_use]
pub fn layout(pane: Rect) -> ListLayout {
    // Border, header, rule above; border below.
    let header = 3;
    ListLayout {
        first_row: pane.y.saturating_add(header),
        height: usize::from(pane.height.saturating_sub(header + 1)),
    }
}

/// The visible row index a terminal row is over, if it is over a row at all.
#[must_use]
pub fn row_at(pane: Rect, scroll: usize, row: u16) -> Option<usize> {
    let layout = layout(pane);
    if row < layout.first_row || row >= pane.bottom() {
        return None;
    }
    Some(scroll + usize::from(row - layout.first_row))
}

/// Renders the list pane (FR-2.1).
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let list = &app.list;

    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style(app, Pane::PullRequests))
        .title(list_title(app));

    // Nothing to draw covers both "no pull requests" and "the search hid them all",
    // and the empty state explains which.
    if list.visible_len() == 0 {
        frame.render_widget(
            Paragraph::new(empty_state(app))
                .block(block)
                .style(theme.style(element::BG))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    let columns = Columns::for_width(area.width.saturating_sub(2));
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(1 + usize::from(area.height));

    // A sticky header row, so a long list stays readable.
    lines.push(header_row(theme, &columns));
    // The rule spans the pane: it separates the header from the rows, so it should
    // not stop where the last column happens to end.
    let rule: String = "─".repeat(usize::from(area.width).saturating_sub(2));
    lines.push(Line::from(Span::styled(rule, theme.style(element::MUTED))));

    // Only the rows that fit are laid out (FR-3.3's rule, applied to the list too).
    let body_height = usize::from(area.height.saturating_sub(4));
    let visible = list.visible();
    // The offset `App::render` worked out, so the rows drawn here are the rows a click
    // maps to.
    let scroll = list.scroll.min(visible.len().saturating_sub(body_height));

    for row in visible.iter().skip(scroll).take(body_height) {
        let selected = *row == list.cursor_index();
        lines.push(item_row(
            theme,
            &columns,
            &list.items[*row],
            selected,
            app.now(),
        ));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG)),
        area,
    );
}

/// The pane title: what is being listed, and how much of it.
fn list_title(app: &App) -> String {
    let repo = app
        .environment
        .as_ref()
        .map_or_else(|| "no repository".to_owned(), |env| env.repo.slug());
    format!(
        " {} · {} · {} ",
        repo,
        app.list.state_filter.label(),
        app.list.status_label()
    )
}

/// The header row of the table.
fn header_row(theme: &Theme, columns: &Columns) -> Line<'static> {
    let style = theme.style(element::MUTED);
    let mut spans = vec![Span::styled("  ".to_owned(), style)];
    for (label, width) in [
        ("#", columns.number),
        ("T", 2),
        ("Title", columns.title),
        ("Size", columns.size),
        ("Author", columns.author),
        ("Updated", columns.updated),
        ("Checks", columns.checks),
        ("Dec", columns.decision),
    ] {
        spans.push(Span::styled(format!("{} ", text::pad(label, width)), style));
    }
    Line::from(spans)
}

/// One row: number, draft marker, title, author, age, checks, decision.
fn item_row(
    theme: &Theme,
    columns: &Columns,
    pr: &PullRequestSummary,
    selected: bool,
    now: crate::domain::time::Timestamp,
) -> Line<'static> {
    let base = if selected {
        theme.style(element::SELECTION)
    } else {
        theme.style(element::FG)
    };
    // Styling a selected row means the *cell* styles must sit on top of the
    // selection background, so they are built from the selection style rather than
    // replacing it.
    let tint = |style: ratatui::style::Style| {
        if selected { base.patch(style) } else { style }
    };

    let marker = if selected { "▶" } else { " " };
    let mut spans = vec![
        Span::styled(format!("{marker} "), base),
        Span::styled(
            text::pad_left(&pr.number.to_string(), columns.number),
            tint(theme.style(element::ACCENT)),
        ),
        Span::styled(" ".to_owned(), base),
    ];

    let draft = if pr.is_work_in_progress() { "◌" } else { " " };
    spans.push(Span::styled(
        format!("{draft} "),
        tint(theme.style(element::LIST_DRAFT)),
    ));

    let title_style = if pr.is_work_in_progress() {
        tint(theme.style(element::LIST_DRAFT))
    } else {
        base
    };
    spans.push(Span::styled(
        format!("{} ", text::pad(&pr.title, columns.title)),
        title_style,
    ));
    // The size is what makes a list scannable for "which of these is a big change"
    // (FR-2.1), and the sign characters sort the two halves apart visually.
    spans.push(Span::styled(
        format!("{} ", text::pad(&pr.size_label(), columns.size)),
        tint(theme.style(element::MUTED)),
    ));
    spans.push(Span::styled(
        format!("{} ", text::pad(&pr.author, columns.author)),
        tint(theme.style(element::MUTED)),
    ));
    spans.push(Span::styled(
        format!("{} ", text::pad(&pr.updated_label(now), columns.updated)),
        tint(theme.style(element::MUTED)),
    ));
    spans.push(Span::styled(
        format!("{} ", text::pad(&pr.checks.label(), columns.checks)),
        tint(check_style(theme, pr.checks.state)),
    ));
    spans.push(Span::styled(
        decision_marker(pr.review_decision).to_owned(),
        tint(decision_style(theme, pr.review_decision)),
    ));

    Line::from(spans)
}

/// The style for a check summary.
fn check_style(theme: &Theme, state: CheckState) -> ratatui::style::Style {
    match state {
        CheckState::Success => theme.style(element::LIST_CHECK_OK),
        CheckState::Failure => theme.style(element::LIST_CHECK_FAIL),
        CheckState::Pending | CheckState::Neutral | CheckState::Skipped => {
            theme.style(element::LIST_CHECK_PENDING)
        }
        CheckState::Unknown => theme.style(element::MUTED),
    }
}

/// The two-character decision marker.
fn decision_marker(decision: Option<ReviewDecision>) -> &'static str {
    match decision {
        Some(ReviewDecision::Approved) => "ok",
        Some(ReviewDecision::ChangesRequested) => "no",
        Some(ReviewDecision::ReviewRequired) => "..",
        None => "  ",
    }
}

/// The style for a decision marker.
fn decision_style(theme: &Theme, decision: Option<ReviewDecision>) -> ratatui::style::Style {
    match decision {
        Some(ReviewDecision::Approved) => theme.style(element::LIST_CHECK_OK),
        Some(ReviewDecision::ChangesRequested) => theme.style(element::LIST_CHECK_FAIL),
        Some(ReviewDecision::ReviewRequired) => theme.style(element::LIST_CHECK_PENDING),
        None => theme.style(element::MUTED),
    }
}

/// What to show when there is nothing to show, and why.
fn empty_state(app: &App) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let muted = theme.style(element::MUTED);

    if let Some(error) = &app.environment_error {
        lines.push(Line::from(Span::styled(
            "Cannot read pull requests".to_owned(),
            theme.style(element::NOTICE_ERROR),
        )));
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            error.to_string(),
            theme.style(element::FG),
        )));
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!("→ {}", error.advice()),
            theme.style(element::ACCENT),
        )));
        return lines;
    }

    if let Some(error) = &app.list.error {
        lines.push(Line::from(Span::styled(
            "Could not list pull requests".to_owned(),
            theme.style(element::NOTICE_ERROR),
        )));
        lines.push(Line::default());
        for line in text::wrap(error, 80) {
            lines.push(Line::from(Span::styled(line, theme.style(element::FG))));
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "R retries · :doctor shows the environment".to_owned(),
            muted,
        )));
        return lines;
    }

    if app.list.is_empty_because_of_search() {
        lines.push(Line::from(Span::styled(
            "Nothing matches.".to_owned(),
            theme.style(element::FG),
        )));
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!("query: {}", app.list.describe_query()),
            muted,
        )));
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Esc clears the search · :clear-filters resets the filters".to_owned(),
            muted,
        )));
        return lines;
    }

    // Detection first, then the fetch: whichever is still outstanding is the one
    // that explains the empty pane.
    if app.environment_running {
        lines.push(Line::from(Span::styled(
            "Looking for the repository and gh…".to_owned(),
            muted,
        )));
        return lines;
    }

    if app.list.loading {
        lines.push(Line::from(Span::styled(
            "Loading pull requests…".to_owned(),
            muted,
        )));
        return lines;
    }

    lines.push(Line::from(Span::styled(
        "No pull requests match the current filters.".to_owned(),
        theme.style(element::FG),
    )));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!("query: {}", app.list.describe_query()),
        muted,
    )));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Esc clears · :clear-filters resets · :filter adds a chip".to_owned(),
        muted,
    )));
    lines
}

/// The state the list pane reads, exposed so tests can build one.
#[must_use]
pub fn state(app: &App) -> &PrListState {
    &app.list
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::domain::pr::{CheckSummary, PrState, ReviewDecision};
    use crate::domain::time::Timestamp;
    use crate::ports::forge::PullRequestPage;
    use crate::test_support::{TempHome, temp_home};

    fn app() -> (TempHome, App) {
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
        (dir, App::new(startup).unwrap())
    }

    fn summary(number: u64, title: &str) -> PullRequestSummary {
        PullRequestSummary {
            number,
            title: title.to_owned(),
            author: "alice".to_owned(),
            state: PrState::Open,
            is_draft: false,
            base_ref: "main".to_owned(),
            head_ref: "topic".to_owned(),
            head_sha: "abc".to_owned(),
            created_at: Timestamp::default(),
            updated_at: Timestamp::default(),
            additions: 10,
            deletions: 2,
            changed_files: 1,
            labels: Vec::new(),
            review_decision: Some(ReviewDecision::Approved),
            checks: CheckSummary {
                state: CheckState::Success,
                passed: 3,
                total: 3,
            },
            url: String::new(),
            is_cross_repository: false,
        }
    }

    fn draw(app: &App, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), app))
            .unwrap();
        crate::tui::test_support::buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn the_columns_fit_the_title_into_whatever_is_left() {
        let columns = Columns::for_width(100);
        assert!(columns.title >= 10);
        assert!(
            columns.total() <= 100,
            "total {} exceeds 100",
            columns.total()
        );

        // A narrow pane still leaves a usable title rather than panicking.
        let narrow = Columns::for_width(10);
        assert_eq!(narrow.title, 10);
    }

    #[test]
    fn an_empty_list_explains_itself_rather_than_showing_a_blank_pane() {
        let (_dir, mut app) = app();
        app.environment_running = false;
        app.list.loading = true;
        let rendered = draw(&app, 100, 12);
        assert!(rendered.contains("Loading pull requests"), "{rendered}");
    }

    #[test]
    fn a_failed_environment_shows_the_message_and_the_next_step() {
        let (_dir, mut app) = app();
        app.environment_running = false;
        app.environment_error = Some(crate::domain::environment::EnvironmentError::GhMissing {
            tried: std::path::PathBuf::from("gh"),
            advice: "install the GitHub CLI from https://cli.github.com".to_owned(),
        });
        let rendered = draw(&app, 100, 14);
        assert!(rendered.contains("Cannot read pull requests"), "{rendered}");
        assert!(rendered.contains("was not found"), "{rendered}");
        assert!(rendered.contains("cli.github.com"), "{rendered}");
    }

    #[test]
    fn rows_show_the_number_draft_marker_title_author_age_checks_and_decision() {
        let (_dir, mut app) = app();
        let mut draft = summary(141, "WIP refactor of billing");
        draft.is_draft = true;
        draft.author = "bruno".to_owned();
        draft.review_decision = Some(ReviewDecision::ChangesRequested);
        draft.checks = CheckSummary {
            state: CheckState::Failure,
            passed: 1,
            total: 3,
        };
        app.list.replace(PullRequestPage::complete(
            vec![summary(142, "Add retry"), draft],
            50,
        ));

        let rendered = draw(&app, 110, 14);
        assert!(rendered.contains("Add retry"), "{rendered}");
        assert!(rendered.contains("142"), "{rendered}");
        assert!(rendered.contains('◌'), "the draft marker: {rendered}");
        assert!(rendered.contains("bruno"), "{rendered}");
        assert!(rendered.contains("3/3"), "{rendered}");
        assert!(rendered.contains("+10 −2"), "the size column: {rendered}");
        assert!(rendered.contains("1/3"), "{rendered}");
        assert!(rendered.contains("ok"), "an approval marker: {rendered}");
        assert!(
            rendered.contains("no"),
            "a changes-requested marker: {rendered}"
        );
        assert!(rendered.contains('▶'), "the cursor: {rendered}");
    }

    #[test]
    fn a_search_that_matches_nothing_echoes_the_query() {
        let (_dir, mut app) = app();
        app.environment_running = false;
        app.list
            .replace(PullRequestPage::complete(vec![summary(1, "one")], 50));
        app.list.set_search("nothing matches this");

        let rendered = draw(&app, 100, 14);
        assert!(rendered.contains("Nothing matches"), "{rendered}");
        assert!(rendered.contains("query:"), "{rendered}");
        assert!(rendered.contains("nothing matches this"), "{rendered}");
    }

    #[test]
    fn a_fetch_failure_offers_a_retry() {
        let (_dir, mut app) = app();
        app.environment_running = false;
        app.list.error = Some("gh pr list failed (exit 1): no network".to_owned());
        let rendered = draw(&app, 100, 14);
        assert!(
            rendered.contains("Could not list pull requests"),
            "{rendered}"
        );
        assert!(rendered.contains("no network"), "{rendered}");
        assert!(rendered.contains("R retries"), "{rendered}");
    }

    #[test]
    fn the_title_says_what_is_being_listed_and_how_much_of_it() {
        let (_dir, mut app) = app();
        app.list
            .replace(PullRequestPage::complete(vec![summary(1, "one")], 50));
        app.list.set_total(137);
        let rendered = draw(&app, 120, 10);
        assert!(rendered.contains("showing 1 of 137"), "{rendered}");
        assert!(rendered.contains("open"), "{rendered}");
    }

    #[test]
    fn a_long_title_is_cut_to_the_column_and_never_wraps() {
        let (_dir, mut app) = app();
        app.list.replace(PullRequestPage::complete(
            vec![summary(1, &"很长的标题".repeat(40))],
            50,
        ));
        let rendered = draw(&app, 100, 10);
        // The buffer is 100 columns wide by construction, so the useful assertion is
        // that nothing wrapped: two rows would mean the row overflowed. (A
        // double-width character occupies two cells and the backend fills the second
        // with a space, so measuring the text is not the same as measuring cells.)
        for line in rendered.lines() {
            assert!(line.chars().count() <= 100, "a row wrapped: {line:?}");
        }
        assert!(rendered.contains('…'), "the cut is marked: {rendered}");
    }

    #[test]
    fn a_row_that_does_not_fit_the_height_is_not_laid_out() {
        let (_dir, mut app) = app();
        app.list.replace(PullRequestPage::complete(
            (1..=50).map(|number| summary(number, "x")).collect(),
            50,
        ));
        let rendered = draw(&app, 100, 12);
        assert!(rendered.contains("showing 50"), "{rendered}");
        // The pane is 12 rows: header, rule, and at most 8 rows.
        let rows = rendered
            .lines()
            .filter(|line| line.contains("alice"))
            .count();
        assert!(rows <= 8, "drew {rows} rows in a 12-row pane");
    }

    #[test]
    fn the_state_accessor_matches_the_app() {
        let (_dir, app) = app();
        assert_eq!(state(&app).items.len(), 0);
    }
}
