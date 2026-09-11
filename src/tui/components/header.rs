//! The header row: application identity, version, config source and target.

use std::fmt::Write as _;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use crate::tui::app::App;
use crate::tui::components::padded_line;
use crate::tui::theme::element;

/// Renders the header (FR-7.6, FR-7.8).
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;

    let left = vec![
        Span::styled(" smart-review ", theme.style(element::TITLE)),
        Span::styled(
            format!("v{} ", env!("CARGO_PKG_VERSION")),
            theme.style(element::MUTED),
        ),
        Span::styled(
            format!("config: {}", app.config_source),
            theme.style(element::MUTED),
        ),
    ];

    let mut target = app
        .repo
        .clone()
        .unwrap_or_else(|| "repository: auto-detect (M1)".to_owned());
    if let Some(pull_request) = app.requested_pr {
        let _ = write!(target, " #{pull_request}");
    }
    if let Some(path) = &app.requested_path {
        let _ = write!(target, " · {}", path.display());
    }
    if let Some(remote) = &app.remote {
        let _ = write!(target, " · remote {remote}");
    }
    let right = vec![Span::styled(format!("{target} "), theme.style(element::FG))];

    frame.render_widget(
        Paragraph::new(padded_line(left, right, area.width)).style(theme.style(element::BG)),
        area,
    );
}
