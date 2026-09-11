//! TUI components (ARCH-6).
//!
//! Each component is a function of application state and a rectangle, which
//! keeps rendering testable with `TestBackend` and free of hidden state.

pub mod command_line;
pub mod doctor;
pub mod filter_bar;
pub mod header;
pub mod help;
pub mod leader;
pub mod palette;
pub mod panes;
pub mod pr_list;
pub mod review;
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

/// The border style for a pane, brightened when it has focus (FR-7.8).
pub(crate) fn border_style(app: &App, pane: crate::tui::app::Pane) -> ratatui::style::Style {
    if app.focus() == pane {
        app.theme.style(crate::tui::theme::element::BORDER_FOCUSED)
    } else {
        app.theme.style(crate::tui::theme::element::BORDER)
    }
}

/// The first visible row of a scrolling list, keeping `cursor` inside the window.
///
/// Shared by the list and the file tree so both scroll the same way: the previous
/// offset is used as a hint but the cursor always wins, which is what keeps a
/// refresh from scrolling the user somewhere unexpected (FR-2.3).
pub(crate) fn scroll_for(cursor: usize, previous: usize, height: usize, len: usize) -> usize {
    let height = height.max(1);
    let mut scroll = previous.min(cursor);
    if cursor >= scroll + height {
        scroll = cursor + 1 - height;
    }
    if cursor < scroll {
        scroll = cursor;
    }
    scroll.min(len.saturating_sub(height))
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
