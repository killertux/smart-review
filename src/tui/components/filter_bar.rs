//! The filter bar: the chips that shape the query, and the search box (FR-2.2).
//!
//! Two different mechanisms sit next to each other on purpose, because they behave
//! differently and the user can see which is which: the chips change what is
//! *asked of GitHub*, and the search box filters what has *already arrived*.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::app::App;
use crate::tui::keymap::Mode;
use crate::tui::theme::{Theme, element};

/// How many rows the bar occupies: the chips, then the search box.
pub const HEIGHT: u16 = 2;

/// Renders the bar, two rows tall: chips, then the search box.
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let lines: Vec<Line<'static>> = vec![chip_line(theme, app), search_line(theme, app)];

    let block = Block::new()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_style(theme.style(element::BORDER));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG)),
        area,
    );
}

/// The chips, plus the hint that says how to change them.
fn chip_line(theme: &Theme, app: &App) -> Line<'static> {
    let mut spans = vec![Span::styled(
        " filters ".to_owned(),
        theme.style(element::MUTED),
    )];

    for chip in app.list.chips() {
        spans.push(Span::styled(
            format!("[{chip}] "),
            theme.style(element::ACCENT),
        ));
    }

    spans.push(Span::styled(
        if app.list.is_filtered() {
            "<leader>f add · x clear".to_owned()
        } else {
            "<leader>f adds one".to_owned()
        },
        theme.style(element::MUTED),
    ));

    Line::from(spans)
}

/// The search box, which is a real input when the search mode is active.
fn search_line(theme: &Theme, app: &App) -> Line<'static> {
    let editing = app.mode == Mode::Search;
    let prompt = if editing { "/" } else { "search" };
    let text = app.list.search.clone();

    let mut spans = vec![
        Span::styled(" ".to_owned(), theme.style(element::FG)),
        Span::styled(
            format!("{prompt} "),
            if editing {
                theme.style(element::COMMAND_PROMPT)
            } else {
                theme.style(element::MUTED)
            },
        ),
    ];

    if text.is_empty() && !editing {
        spans.push(Span::styled(
            "/ filters what has been fetched; :filter asks GitHub".to_owned(),
            theme.style(element::MUTED),
        ));
    } else {
        spans.push(Span::styled(text, theme.style(element::FG)));
        if editing {
            spans.push(Span::styled("_".to_owned(), theme.style(element::ACCENT)));
        } else {
            let matches = app.list.visible_len();
            spans.push(Span::styled(
                format!("  ({matches} matching)"),
                theme.style(element::MUTED),
            ));
        }
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::domain::pr::{CheckSummary, PrState, PullRequestSummary};
    use crate::domain::query::Filter;
    use crate::ports::forge::PullRequestPage;
    use crate::test_support::{TempHome, temp_home};

    fn app() -> (TempHome, App) {
        let dir = temp_home();
        let cli = Cli {
            repo: None,
            pr: None,
            path: None,
            remote: None,
            config: None,
            theme: None,
            home: Some(dir.path().to_path_buf()),
            log_level: None,
            check: false,
            dry_run: false,
        };
        let startup = crate::Startup::load(&cli).unwrap();
        (dir, App::new(startup).unwrap())
    }

    fn summary(number: u64, title: &str) -> PullRequestSummary {
        PullRequestSummary {
            number,
            title: title.to_owned(),
            author: "alice".to_owned(),
            state: PrState::Open,
            is_draft: false,
            base_ref: "main".to_owned(),
            head_ref: "topic".to_owned(),
            head_sha: "abc".to_owned(),
            created_at: crate::domain::time::Timestamp::default(),
            updated_at: crate::domain::time::Timestamp::default(),
            additions: 1,
            deletions: 1,
            changed_files: 1,
            labels: Vec::new(),
            review_decision: None,
            checks: CheckSummary::default(),
            url: String::new(),
            is_cross_repository: false,
        }
    }

    fn draw(app: &App) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 2)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), app))
            .unwrap();
        crate::tui::test_support::buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn a_fresh_list_shows_the_state_chip_and_a_hint() {
        let (_dir, app) = app();
        let rendered = draw(&app);
        assert!(rendered.contains("[is:open]"), "{rendered}");
        assert!(rendered.contains("<leader>f adds one"), "{rendered}");
        assert!(rendered.contains("/ filters"), "{rendered}");
    }

    #[test]
    fn every_chip_is_visible_and_the_hint_changes() {
        let (_dir, mut app) = app();
        app.list.push_filter(Filter::Author("alice".to_owned()));
        app.list
            .push_filter(Filter::Label("needs review".to_owned()));
        let rendered = draw(&app);
        assert!(rendered.contains("[author:alice]"), "{rendered}");
        // The chip quotes a value with a space, which is also how it is typed into
        // `:filter`, so the chip is copyable.
        assert!(rendered.contains("[label:\"needs review\"]"), "{rendered}");
        assert!(rendered.contains("x clear"), "{rendered}");
    }

    #[test]
    fn the_search_box_shows_the_count_while_filtering() {
        let (_dir, mut app) = app();
        app.list.replace(PullRequestPage::complete(
            vec![summary(1, "retry the webhook"), summary(2, "billing")],
            50,
        ));
        app.list.set_search("webhook");
        let rendered = draw(&app);
        assert!(rendered.contains("webhook"), "{rendered}");
        assert!(rendered.contains("(1 matching)"), "{rendered}");
    }

    #[test]
    fn the_search_box_looks_like_an_input_while_it_is_being_typed_in() {
        let (_dir, mut app) = app();
        app.mode = Mode::Search;
        app.list.set_search("bill");
        let rendered = draw(&app);
        assert!(rendered.contains("/ bill"), "{rendered}");
        assert!(rendered.contains('_'), "a caret while typing: {rendered}");
    }
}
