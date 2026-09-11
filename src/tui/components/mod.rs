//! TUI components (ARCH-6).
//!
//! Each component is a function of application state and a rectangle, which
//! keeps rendering testable with `TestBackend` and free of hidden state.

pub mod command_line;
pub mod doctor;
pub mod header;
pub mod help;
pub mod leader;
pub mod panes;
pub mod status_line;
pub mod theme_picker;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::tui::app::{App, Overlay};

/// Renders whichever popup is open, if any (FR-7.3, FR-7.7, FR-9.3).
pub fn render_overlay(frame: &mut Frame<'_>, area: Rect, app: &App) {
    match app.overlay() {
        Overlay::None => {}
        Overlay::Help => help::render(frame, area, app),
        Overlay::Leader => leader::render(frame, area, app),
        Overlay::Doctor => doctor::render(frame, area, app),
        Overlay::ThemePicker => theme_picker::render(frame, area, app),
    }
}

/// Converts a number of lines into a `u16` height, saturating rather than
/// truncating when the terminal is impossibly tall.
pub(crate) fn height_for(lines: usize) -> u16 {
    u16::try_from(lines).unwrap_or(u16::MAX)
}

/// Lays a left-aligned and a right-aligned group of spans on one row.
///
/// Widths are counted in characters rather than columns, which is exact for the
/// ASCII content used here and avoids another dependency. Text containing wide
/// characters would need `unicode-width` (PLAN.md §5).
pub(crate) fn padded_line(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: u16,
) -> Line<'static> {
    let used: usize = left
        .iter()
        .chain(right.iter())
        .map(|span| span.content.chars().count())
        .sum();
    let padding = (width as usize).saturating_sub(used);

    let mut spans = left;
    if padding > 0 {
        spans.push(Span::raw(" ".repeat(padding)));
    }
    spans.extend(right);
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn padding_fills_the_row_exactly() {
        let line = padded_line(
            vec![Span::styled("ab".to_owned(), Style::default())],
            vec![Span::styled("cd".to_owned(), Style::default())],
            10,
        );
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(rendered, "ab      cd");
    }

    #[test]
    fn padding_never_goes_negative() {
        let line = padded_line(
            vec![Span::raw("abcdefghij".to_owned())],
            vec![Span::raw("kl".to_owned())],
            5,
        );
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(rendered, "abcdefghijkl");
    }
}
