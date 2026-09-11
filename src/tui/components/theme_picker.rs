//! The theme picker (FR-7.7).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::tui::app::App;
use crate::tui::components::height_for;
use crate::tui::layout;
use crate::tui::theme::element;

/// Renders the list of themes the user can choose from.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let names = app.theme_names();

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let selected = index == app.picker_cursor();
        let current = name == app.theme.name();
        let style = if selected {
            theme.style(element::PICKER_SELECTED)
        } else {
            theme.style(element::FG)
        };
        let marker = if current { "*" } else { " " };
        lines.push(Line::from(Span::styled(
            format!(" {marker} {name} "),
            style,
        )));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no themes found".to_owned(),
            theme.style(element::MUTED),
        )));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " j/k move · Enter applies · Esc cancels".to_owned(),
        theme.style(element::MUTED),
    )));

    let height = height_for(lines.len()).saturating_add(2).min(area.height);
    let popup = layout::centered(area, 52, height);

    frame.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.style(element::BORDER_FOCUSED))
        .title(" theme ");

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG)),
        popup,
    );
}
