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
use crate::tui::diff_view::DiffView;
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
        // Only worth a column once a diff is on screen: which source answered is
        // what tells the user whether the context and whitespace toggles apply.
        Span::styled(
            if app.review.is_some() {
                format!("· {} ", app.diff_source().label())
            } else {
                String::new()
            },
            background,
        ),
        // In the review screen the cursor's file is what the user is reading, so it
        // belongs in the status line beside the pane name.
        Span::styled(current_file_label(app), background),
        // Where the file sits in both orders, which is what makes the toggle a
        // comparison rather than a leap of faith (FR-3.5).
        Span::styled(order_positions_label(app), background),
        Span::styled(format!("· {} ", model_label(app)), background),
        // The order and the analysis are what a reviewer is looking at in M2, so
        // they get the columns the diff source and the model do not need (FR-3.5).
        Span::styled(analysis_label(app), background),
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

/// The active model and its thinking setting (FR-4.5, FR-4.8).
///
/// Three states, each said plainly: nothing chosen yet, chosen and verified, chosen
/// but unusable — and the last names the reason, because "the model is set but the
/// key is missing" is the one a user has to act on.
fn model_label(app: &App) -> String {
    if let Some(resolved) = app.active_model() {
        return format!(
            "{} thinking:{}",
            resolved.label(),
            resolved.thinking_label()
        );
    }
    match (app.config.llm.active.as_ref(), app.model_problem()) {
        (Some(_), Some(problem)) => format!("{PLACEHOLDER} ({problem})"),
        (Some(active), None) => format!("{}/{} (unverified)", active.provider, active.model),
        (None, _) => PLACEHOLDER.to_owned(),
    }
}

/// The current file's position in both orders (FR-3.5).
///
/// Empty when there is no plan: "1/1 plan · 1/1 path" is a sentence that says nothing.
fn order_positions_label(app: &App) -> String {
    match app.review.as_ref().and_then(DiffView::order_positions) {
        Some(positions) => format!("· {positions} "),
        None => String::new(),
    }
}

/// What the analysis is doing (FR-4.4).
///
/// Only the state, never the order: the order is named in the Files pane's title,
/// where the user is looking when it matters, and a status line that repeats it is a
/// status line that truncates the key hints for no gain. Empty when nothing is
/// happening, so the line does not shift once per analysis.
fn analysis_label(app: &App) -> String {
    if app.review.is_none() {
        return String::new();
    }
    // The stage while a run is in flight, the result otherwise: the status line is
    // where a user looks to find out whether anything is happening (NFR-1.2).
    match app.analysis_state() {
        crate::tui::app::AnalysisState::Idle => String::new(),
        state => format!("· {} ", state.label()),
    }
}

fn notice_style(theme: &Theme, level: NoticeLevel) -> Style {
    match level {
        NoticeLevel::Info => theme.style(element::NOTICE_INFO),
        NoticeLevel::Warn => theme.style(element::NOTICE_WARN),
        NoticeLevel::Error => theme.style(element::NOTICE_ERROR),
    }
}
