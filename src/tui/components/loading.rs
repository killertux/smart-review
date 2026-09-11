//! The "opening a pull request" indicator (FR-7.6, NFR-1.4).
//!
//! Opening a pull request fetches its detail and then its diff, and on a large pull
//! request over a slow link that is seconds of nothing. The indicator says what is
//! happening, which step it is on, and how long it has been going — and it says that
//! `Esc` cancels, because that is the only thing the user can do about it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::tui::app::App;
use crate::tui::layout;
use crate::tui::theme::element;

/// The frames of the spinner, in order.
///
/// Braille, because it is one cell wide and present in every font that matters.
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The spinner frame for a tick count.
#[must_use]
pub fn frame_for(tick: usize) -> &'static str {
    FRAMES[tick % FRAMES.len()]
}

/// Renders the indicator, if something is being opened.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(opening) = app.opening() else {
        return;
    };
    let theme = &app.theme;

    let lines = vec![
        Line::from(vec![
            Span::styled(
                format!(" {} ", frame_for(app.spinner())),
                theme.style(element::ACCENT),
            ),
            Span::styled(opening.headline(), theme.style(element::FG)),
        ]),
        Line::default(),
        Line::from(Span::styled(
            format!(" {}", opening.detail(app.now_unix_secs())),
            theme.style(element::MUTED),
        )),
        Line::from(Span::styled(" Esc gives up", theme.style(element::MUTED))),
    ];

    let popup = layout::centered(area, 52, 6);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_style(theme.style(element::BORDER_FOCUSED))
                    .title(" Opening "),
            )
            .style(theme.style(element::BG)),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_home;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn app() -> (crate::test_support::TempHome, App) {
        let dir = temp_home();
        let cli = crate::cli::Cli {
            repo: None,
            pr: None,
            path: None,
            remote: None,
            config: None,
            theme: None,
            home: Some(dir.path().to_path_buf()),
            log_level: None,
            check: false,
        };
        let startup = crate::Startup::load(&cli).unwrap();
        (dir, App::new(startup).unwrap())
    }

    fn draw(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), app))
            .unwrap();
        crate::tui::test_support::buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn nothing_is_drawn_when_nothing_is_opening() {
        let (_dir, app) = app();
        assert!(draw(&app).trim().is_empty());
    }

    #[test]
    fn the_indicator_names_the_pull_request_the_step_and_the_way_out() {
        let (_dir, mut app) = app();
        app.begin_opening(141);

        let rendered = draw(&app);
        assert!(rendered.contains("Opening"), "{rendered}");
        assert!(rendered.contains("#141"), "{rendered}");
        assert!(rendered.contains("pull request"), "{rendered}");
        assert!(rendered.contains("Esc gives up"), "{rendered}");

        // The second stage says so: it is the slow one on a large diff.
        app.advance_opening();
        let rendered = draw(&app);
        assert!(rendered.contains("diff"), "{rendered}");
    }

    #[test]
    fn the_spinner_moves_and_wraps() {
        let first = frame_for(0);
        assert_ne!(first, frame_for(1));
        assert_eq!(frame_for(0), frame_for(FRAMES.len()));
        assert_eq!(frame_for(FRAMES.len() + 3), FRAMES[3]);
    }

    #[test]
    fn the_elapsed_time_is_shown_once_it_is_worth_showing() {
        let (_dir, mut app) = app();
        app.set_now(1_000);
        app.begin_opening(7);
        assert!(
            !draw(&app).contains("s ·") && !draw(&app).contains(" 0s"),
            "a fraction of a second is not worth a notice"
        );

        app.set_now(1_004);
        assert!(draw(&app).contains("4s"), "{}", draw(&app));
    }
}
