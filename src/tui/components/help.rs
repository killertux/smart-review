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
///
/// The list is longer than most terminals once every group is listed, so it scrolls:
/// clipping the tail silently would hide exactly the bindings a user opened the help
/// to find.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let lines = build_lines(app);
    let height = height_for(lines.len()).saturating_add(2).min(area.height);
    let popup = layout::centered(area, 88, height);

    // Two rows are lost to the border and one to the hint line.
    let visible = usize::from(popup.height.saturating_sub(3));
    let offset = app.help_scroll.min(lines.len().saturating_sub(visible));
    let more = lines.len() > offset + visible;

    let title = if lines.len() > visible {
        format!(
            " Help  [{}/{}] ",
            (offset + visible).min(lines.len()),
            lines.len()
        )
    } else {
        " Help ".to_owned()
    };

    frame.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.style(element::BORDER_FOCUSED))
        .title(title);

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0)),
        popup,
    );

    let _ = more;
}

fn build_lines(app: &App) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let mut lines = Vec::new();

    for group in action::groups() {
        let definitions: Vec<&action::ActionDef> = action::all()
            .iter()
            .filter(|definition| definition.group == *group)
            .filter(|definition| matches_filter(app, definition.id))
            .collect();
        if definitions.is_empty() {
            continue;
        }

        lines.push(Line::from(Span::styled(
            format!(" {}", group.label().to_uppercase()),
            theme.style(element::HELP_GROUP),
        )));
        for definition in definitions {
            lines.push(Line::from(vec![
                Span::raw("   "),
                // Truncated *and* padded: a key list longer than the column used to push
                // the description right up against it, and a description that starts in a
                // different column on one row reads as a different field.
                Span::styled(
                    crate::tui::text::pad(
                        &crate::tui::text::truncate(&keys_for(app, definition.id), KEY_COLUMN),
                        KEY_COLUMN,
                    ),
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
        match &app.help_filter {
            Some(id) => format!(" Filtered to `{id}` by :keymap — Esc closes"),
            None => " Esc closes · j/k scroll · : commands · <leader> menu · :keymap <action>"
                .to_owned(),
        },
        theme.style(element::MUTED),
    )));

    lines
}

/// Whether an action is shown, given `:keymap <action>`.
fn matches_filter(app: &App, action_id: &str) -> bool {
    app.help_filter
        .as_deref()
        .is_none_or(|filter| filter == action_id)
}

/// Every key sequence bound to an action, with the mode when it is not `normal`.
/// How wide the key column is before the description starts.
const KEY_COLUMN: usize = 20;

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
