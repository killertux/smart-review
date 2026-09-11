//! The environment check popup (FR-9.3).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::doctor::Status;
use crate::tui::app::App;
use crate::tui::components::height_for;
use crate::tui::layout;
use crate::tui::theme::{Theme, element};

/// Renders the doctor report collected when the popup opened.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let checks = app.checks();

    let mut lines: Vec<Line<'static>> = Vec::new();
    for check in checks {
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", check.status.marker()),
                status_style(theme, check.status),
            ),
            Span::styled(format!("{:<9}", check.name), theme.style(element::HELP_KEY)),
            Span::styled(check.detail.clone(), theme.style(element::FG)),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no checks were run".to_owned(),
            theme.style(element::MUTED),
        )));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " Same checks as `smart-review --check`. Esc closes.".to_owned(),
        theme.style(element::MUTED),
    )));

    let height = height_for(lines.len()).saturating_add(2).min(area.height);
    let popup = layout::centered(area, area.width.saturating_sub(4), height);

    frame.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.style(element::BORDER_FOCUSED))
        .title(" doctor ");

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn status_style(theme: &Theme, status: Status) -> Style {
    match status {
        Status::Ok => theme.style(element::NOTICE_SUCCESS),
        Status::Warn => theme.style(element::NOTICE_WARN),
        Status::Fail => theme.style(element::NOTICE_ERROR),
    }
}
