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
pub mod loading;
pub mod model_picker;
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
    // The opening indicator is not an overlay: it takes no keyboard focus, so a popup
    // behind it still works and `Esc` means what it always means (give up).
    loading::render(frame, area, app);

    // The model picker is modal but is not an `Overlay`: it takes every key while it
    // is open (it is a text-entry surface), which `Overlay` does not model.
    if let Some(picker) = app.picker() {
        let label = if picker.is_checking() {
            Some("checking with the provider…")
        } else {
            None
        };
        picker.render(frame, area, &app.theme, label);
        return;
    }

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

/// Clamps a view offset to the rows that exist.
pub(crate) fn clamp_offset(offset: usize, height: usize, len: usize) -> usize {
    offset.min(len.saturating_sub(height.max(1)))
}

/// The offset that keeps `cursor` inside the window.
///
/// This runs before every frame, and it never moves the window further than it has to:
/// a cursor that is already visible leaves the offset alone, so a view the user
/// scrolled by hand is not dragged back under the cursor.
pub(crate) fn ensure_visible(cursor: usize, offset: usize, height: usize, len: usize) -> usize {
    let height = height.max(1);
    let offset = clamp_offset(offset, height, len);
    if cursor < offset {
        return clamp_offset(cursor, height, len);
    }
    if cursor >= offset + height {
        return clamp_offset(cursor + 1 - height, height, len);
    }
    offset
}

/// Scrolls the *view* by `delta` rows, dragging the cursor only if it would fall
/// outside the window.
///
/// A wheel moves the text, which is what a wheel is for; the cursor follows only when
/// it would otherwise be left behind, because the cursor is what `Enter` acts on.
pub(crate) fn scroll_view(
    cursor: &mut usize,
    offset: &mut usize,
    delta: i32,
    height: usize,
    len: usize,
) {
    let height = height.max(1);
    *offset = clamp_offset(offset.saturating_add_signed(delta as isize), height, len);

    let last = len.saturating_sub(1);
    if *cursor < *offset {
        *cursor = *offset;
    } else if *cursor >= *offset + height {
        *cursor = (*offset + height - 1).min(last);
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
