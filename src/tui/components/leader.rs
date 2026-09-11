//! The leader menu (FR-7.3).
//!
//! Lists the bindings that continue from the leader key, the way which-key does.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::tui::action;
use crate::tui::app::App;
use crate::tui::components::height_for;
use crate::tui::keymap::describe_sequence;
use crate::tui::layout;
use crate::tui::theme::element;

/// Renders the leader menu.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();

    for binding in app.keymap.leader_menu() {
        let continuation = binding.keys.get(1..).unwrap_or(&[]);
        let description = action::find(&binding.action)
            .map_or("(unknown action)", |definition| definition.description);
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{:<12}", describe_sequence(continuation)),
                theme.style(element::HELP_KEY),
            ),
            Span::styled(
                description.to_owned(),
                theme.style(element::HELP_DESCRIPTION),
            ),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no leader bindings are configured".to_owned(),
            theme.style(element::MUTED),
        )));
    }

    let height = height_for(lines.len()).saturating_add(2).min(area.height);
    let popup = layout::centered(area, 48, height);

    frame.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.style(element::BORDER_FOCUSED))
        .title(format!(" leader {} ", app.keymap.leader().describe()));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG)),
        popup,
    );
}
