//! The help popup (FR-7.3).
//!
//! Generated from the action registry and the active keymap, so it can never
//! disagree with what the keys actually do.

use std::fmt::Write as _;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::tui::action;
use crate::tui::app::App;
use crate::tui::components::height_for;
use crate::tui::keymap::{Mode, Scope, describe_sequence};
use crate::tui::layout;
use crate::tui::theme::element;

/// Renders the help popup.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let lines = build_lines(app);
    let height = height_for(lines.len()).saturating_add(2).min(area.height);
    let popup = layout::centered(area, 88, height);

    frame.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.style(element::BORDER_FOCUSED))
        .title(" Help ");

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn build_lines(app: &App) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let mut lines = Vec::new();

    for group in action::groups() {
        lines.push(Line::from(Span::styled(
            format!(" {}", group.label().to_uppercase()),
            theme.style(element::HELP_GROUP),
        )));
        for definition in action::all()
            .iter()
            .filter(|definition| definition.group == *group)
        {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    format!("{:<20}", keys_for(app, definition.id)),
                    theme.style(element::HELP_KEY),
                ),
                Span::styled(
                    definition.description.to_owned(),
                    theme.style(element::HELP_DESCRIPTION),
                ),
            ]));
        }
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " Esc closes · : commands · <leader> action menu · q quits from a popup".to_owned(),
        theme.style(element::MUTED),
    )));

    lines
}

/// Every key sequence bound to an action, with the mode when it is not `normal`.
fn keys_for(app: &App, action_id: &str) -> String {
    let mut labels: Vec<String> = app
        .keymap
        .bindings()
        .iter()
        .filter(|binding| binding.action == action_id)
        .map(|binding| {
            let mut label = describe_sequence(&binding.keys);
            if let Scope::In(mode) = binding.scope
                && mode != Mode::Normal
            {
                let _ = write!(label, " ({})", mode.as_str());
            }
            label
        })
        .collect();

    labels.sort();
    if labels.is_empty() {
        return "·".to_owned();
    }
    labels.join(", ")
}
