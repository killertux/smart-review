//! The command palette (FR-7.3).
//!
//! Lists the commands matching what has been typed, so `:` is discoverable
//! rather than something you have to remember. Rendered from the same command
//! table the command line executes, so the two cannot disagree.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::tui::app::{App, PALETTE_ROWS};
use crate::tui::keymap::Mode;
use crate::tui::theme::element;

/// Renders the candidate commands while the command line is open.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;

    if app.mode() != Mode::Command || area.height == 0 {
        return;
    }

    let matches = crate::tui::update::candidates(&app.command.input);
    let mut lines: Vec<Line<'static>> = Vec::new();

    for (name, description) in matches.iter().take(PALETTE_ROWS) {
        let selected = app.command.input.trim() == *name;
        let style = if selected {
            theme.style(element::SELECTION)
        } else {
            theme.style(element::PICKER_SELECTED)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {name:<10}"), style),
            Span::styled((*description).to_owned(), theme.style(element::MUTED)),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no matching command — Tab completes, Esc cancels".to_owned(),
            theme.style(element::MUTED),
        )));
    }

    frame.render_widget(Paragraph::new(lines).style(theme.style(element::BG)), area);
}
