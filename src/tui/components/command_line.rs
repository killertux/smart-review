//! The `:` command line (FR-7.4).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::tui::app::App;
use crate::tui::keymap::Mode;
use crate::tui::theme::element;

/// Renders the command line, or an empty row outside command mode.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;

    if app.mode() != Mode::Command {
        frame.render_widget(Paragraph::new("").style(theme.style(element::BG)), area);
        return;
    }

    let mut spans = vec![Span::styled(":", theme.style(element::COMMAND_PROMPT))];
    match &app.command.error {
        Some(error) => spans.push(Span::styled(
            format!(" {error}"),
            theme.style(element::COMMAND_ERROR),
        )),
        None => spans.push(Span::styled(
            format!(" {}", app.command.input),
            theme.style(element::FG),
        )),
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(theme.style(element::BG)),
        area,
    );
}
