//! The status line: mode, focus, theme and the latest notification (FR-7.6).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use crate::tui::app::{App, NoticeLevel};
use crate::tui::components::padded_line;
use crate::tui::keymap::Mode;
use crate::tui::theme::{Theme, element};

/// Renders the status line.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let failed = app
        .latest_notice()
        .is_some_and(|notice| notice.level == NoticeLevel::Error);

    let background = match app.mode() {
        Mode::Command | Mode::Search => theme.style(element::STATUS_COMMAND),
        Mode::Insert => theme.style(element::STATUS_INSERT),
        _ if failed => theme.style(element::STATUS_ERROR),
        _ => theme.style(element::STATUS_NORMAL),
    };

    let left = vec![
        Span::styled(format!(" {} ", app.mode().label()), background),
        Span::styled(format!("{} ", app.focus().label()), background),
        Span::styled(
            format!(
                "· {} · leader {} · {} keys ",
                app.theme.name(),
                app.keymap.leader().describe(),
                app.keymap.bindings().len()
            ),
            background,
        ),
    ];

    let right = match app.latest_notice() {
        Some(notice) => vec![Span::styled(
            format!("{} ", notice.text),
            notice_style(theme, notice.level),
        )],
        None => vec![Span::styled(
            "? help · <leader> actions · : commands ".to_owned(),
            theme.style(element::MUTED),
        )],
    };

    frame.render_widget(
        Paragraph::new(padded_line(left, right, area.width)).style(background),
        area,
    );
}

fn notice_style(theme: &Theme, level: NoticeLevel) -> Style {
    match level {
        NoticeLevel::Info => theme.style(element::NOTICE_INFO),
        NoticeLevel::Warn => theme.style(element::NOTICE_WARN),
        NoticeLevel::Error => theme.style(element::NOTICE_ERROR),
    }
}
