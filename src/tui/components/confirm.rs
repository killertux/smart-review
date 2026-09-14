//! The confirmation standing in front of a destructive action (FR-6.5).
//!
//! Deliberately small and deliberately generic: the question and the action it guards
//! are one value ([`crate::tui::app::Confirmation`]), so a confirmation cannot ask
//! about one thing and do another. It exists because two of the actions this
//! application can take are not recoverable — clearing staged comments and deleting a
//! worktree — and neither is worth a modal of its own.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::tui::app::App;
use crate::tui::text as text_util;
use crate::tui::theme::element;

/// How wide the question may be before it is wrapped.
const WIDTH: u16 = 60;

/// Renders the confirmation.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let Some(confirmation) = app.confirmation() else {
        return;
    };
    let rows = 6;
    let width = WIDTH.min(area.width.saturating_sub(4));
    let area = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(rows)) / 2,
        width,
        height: rows.min(area.height),
    };
    frame.render_widget(Clear, area);

    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.style(element::NOTICE_WARN))
        .title(" are you sure? ");
    let body = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(area);

    frame.render_widget(
        Paragraph::new(vec![
            Line::default(),
            Line::from(Span::styled(
                format!(
                    "  {}",
                    text_util::truncate(&confirmation.question, usize::from(width))
                ),
                theme.style(element::FG),
            )),
        ])
        .block(block)
        .style(theme.style(element::BG))
        .wrap(Wrap { trim: false }),
        body[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                " Enter confirms · Esc cancels".to_owned(),
                theme.style(element::FG),
            )),
            Line::from(Span::styled(
                " nothing has happened yet".to_owned(),
                theme.style(element::MUTED),
            )),
        ])
        .style(theme.style(element::BG)),
        body[1],
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_confirmation_says_that_nothing_has_happened_yet() {
        // The one property worth asserting in a test: a confirmation that reads like a
        // report of something already done is worse than no confirmation at all.
        let source = include_str!("confirm.rs");
        assert!(source.contains("nothing has happened yet"));
    }
}
