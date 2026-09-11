//! The status line (FR-7.6).
//!
//! Left: the persistent segments — mode, focus, repository, pull request, model
//! and draft count. Right: the newest notification, or the key hints.
//!
//! The segments that need M1/M2 data render an em dash placeholder rather than
//! disappearing, so the layout does not shift when the features land and the
//! absence of a value is visible instead of implied.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use crate::tui::app::{App, NoticeLevel};
use crate::tui::components::padded_line;
use crate::tui::keymap::Mode;
use crate::tui::theme::{Theme, element};

/// Rendered where a value exists in a later milestone.
const PLACEHOLDER: &str = "—";

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
        Span::styled(format!("· {} ", repository_label(app)), background),
        Span::styled(format!("· {} ", pull_request_label(app)), background),
        // In the review screen the cursor's file is what the user is reading, so it
        // belongs in the status line beside the pane name.
        Span::styled(current_file_label(app), background),
        Span::styled(format!("· {} ", model_label(app)), background),
        Span::styled("· 0 drafts ", background),
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

fn repository_label(app: &App) -> String {
    app.repo.clone().unwrap_or_else(|| PLACEHOLDER.to_owned())
}

/// The file under the cursor in the review screen, with its stats.
fn current_file_label(app: &App) -> String {
    app.review_screen()
        .and_then(crate::tui::diff_view::DiffView::current_file_label)
        .map_or_else(String::new, |label| format!("· {label} "))
}

fn pull_request_label(app: &App) -> String {
    app.requested_pr
        .map_or_else(|| PLACEHOLDER.to_owned(), |number| format!("#{number}"))
}

fn model_label(app: &App) -> String {
    app.config.llm.active.as_ref().map_or_else(
        || PLACEHOLDER.to_owned(),
        |active| format!("{}/{}", active.provider, active.model),
    )
}

fn notice_style(theme: &Theme, level: NoticeLevel) -> Style {
    match level {
        NoticeLevel::Info => theme.style(element::NOTICE_INFO),
        NoticeLevel::Warn => theme.style(element::NOTICE_WARN),
        NoticeLevel::Error => theme.style(element::NOTICE_ERROR),
    }
}
