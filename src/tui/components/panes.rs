//! The main panes.
//!
//! In M0 they describe what is coming and report the running configuration, so
//! the shell is honest rather than a fake. M1 replaces the left pane with the
//! pull request list and the right one with the diff.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::tui::app::App;
use crate::tui::components::review;
use crate::tui::layout::{self, MIN_HEIGHT, MIN_WIDTH};
use crate::tui::theme::{Theme, element};

/// Renders the body of the interface (FR-7.8).
///
/// Two screens share the body: the pull request list, and the review screen for the
/// open pull request. Which one is drawn is read from the state rather than from a
/// flag of its own, so there is no way for the two to disagree.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if app.review_screen().is_some() {
        review::render(frame, area, app);
        return;
    }
    // The list screen is the filter bar above the list itself. Both rectangles come
    // from the same function the event loop uses to place a mouse event, so the rows a
    // click maps to are the rows that were drawn.
    let (filter_bar, list) = body_split(area);
    super::filter_bar::render(frame, filter_bar, app);
    super::pr_list::render(frame, list, app);
}

/// Splits the body into the filter bar and the list pane.
///
/// One function, called by the renderer *and* by the loop that records the geometry a
/// mouse event is tested against: two copies of this arithmetic is how a click ends up
/// selecting the row below the one it was aimed at (FR-7.5).
#[must_use]
pub fn body_split(body: Rect) -> (Rect, Rect) {
    let rows = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Length(super::filter_bar::HEIGHT),
        ratatui::layout::Constraint::Min(3),
    ])
    .split(body);
    (rows[0], rows[1])
}

/// Renders the "terminal too small" message (FR-7.8).
pub fn render_too_small(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let message = vec![
        Line::from(Span::styled(
            "terminal too small".to_owned(),
            theme.style(element::STATUS_ERROR),
        )),
        Line::default(),
        Line::from(Span::styled(
            format!(
                "smart-review needs at least {MIN_WIDTH}x{MIN_HEIGHT}; this terminal is {}x{}",
                area.width, area.height
            ),
            theme.style(element::MUTED),
        )),
    ];

    // Centred, per FR-7.8, rather than pinned to the top-left corner.
    let width = message.iter().map(Line::width).max().unwrap_or(0);
    let popup = layout::centered(
        area,
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(message.len()).unwrap_or(u16::MAX),
    );

    frame.render_widget(
        Paragraph::new(message).style(theme.style(element::BG)),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::test_support::{TempHome, temp_home};
    use crate::tui::app::App;
    use crate::tui::diff_view::DiffView;

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

    #[test]
    fn the_too_small_message_names_the_requirement() {
        let (_dir, app) = app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 10)).unwrap();
        terminal
            .draw(|frame| render_too_small(frame, frame.area(), &app.theme))
            .unwrap();
        let rendered = crate::tui::test_support::buffer_to_string(terminal.backend().buffer());
        assert!(rendered.contains("terminal too small"), "{rendered}");
        assert!(rendered.contains("80x24"), "{rendered}");
    }

    #[test]
    fn the_body_shows_the_list_when_no_pull_request_is_open() {
        let (_dir, app) = app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &app))
            .unwrap();
        let rendered = crate::tui::test_support::buffer_to_string(terminal.backend().buffer());

        assert!(rendered.contains("filters"), "{rendered}");
        assert!(rendered.contains("[is:open]"), "{rendered}");
        // No file tree, which belongs to the review screen.
        assert!(!rendered.contains("order ("), "{rendered}");
    }

    #[test]
    fn the_body_shows_the_review_when_a_pull_request_is_open() {
        let (_dir, mut app) = app();
        app.set_review(DiffView::new(crate::domain::diff::Patch::default()));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &app))
            .unwrap();
        let rendered = crate::tui::test_support::buffer_to_string(terminal.backend().buffer());

        assert!(rendered.contains("path order · patch (0)"), "{rendered}");
        assert!(
            !rendered.contains("[is:open]"),
            "the list is not drawn behind the review screen: {rendered}"
        );
    }
}
