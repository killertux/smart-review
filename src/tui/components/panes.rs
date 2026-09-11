//! The main panes.
//!
//! In M0 they describe what is coming and report the running configuration, so
//! the shell is honest rather than a fake. M1 replaces the left pane with the
//! pull request list and the right one with the diff.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::tui::app::{App, Pane, ROADMAP};
use crate::tui::layout::{self, MIN_HEIGHT, MIN_WIDTH};
use crate::tui::theme::{Theme, element};

/// Renders the body of the interface (FR-7.8).
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let columns =
        Layout::horizontal([Constraint::Percentage(56), Constraint::Percentage(44)]).split(area);
    render_roadmap(frame, columns[0], app);
    render_shell_status(frame, columns[1], app);
}

/// Renders the "terminal too small" message (FR-7.8).
pub fn render_too_small(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let message = vec![
        Line::from(Span::styled(
            "terminal too small".to_owned(),
            theme.style(element::STATUS_ERROR),
        )),
        Line::default(),
        Line::from(Span::styled(
            format!(
                "smart-review needs at least {MIN_WIDTH}x{MIN_HEIGHT}; this terminal is {}x{}",
                area.width, area.height
            ),
            theme.style(element::MUTED),
        )),
    ];

    // Centred, per FR-7.8, rather than pinned to the top-left corner.
    let width = message.iter().map(Line::width).max().unwrap_or(0);
    let popup = layout::centered(
        area,
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(message.len()).unwrap_or(u16::MAX),
    );

    frame.render_widget(
        Paragraph::new(message).style(theme.style(element::BG)),
        popup,
    );
}

fn render_roadmap(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();

    for (index, entry) in ROADMAP.iter().enumerate() {
        let selected = index == app.cursor;
        let marker = if selected { ">" } else { " " };
        let style = if selected {
            theme.style(element::SELECTION)
        } else {
            theme.style(element::FG)
        };
        lines.push(Line::from(Span::styled(
            format!(" {marker} {entry}"),
            style,
        )));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " This build is the M0 shell.".to_owned(),
        theme.style(element::MUTED),
    )));
    lines.push(Line::from(Span::styled(
        " Navigation, themes, keybindings and the".to_owned(),
        theme.style(element::MUTED),
    )));
    lines.push(Line::from(Span::styled(
        " command line are live. The list and diff".to_owned(),
        theme.style(element::MUTED),
    )));
    lines.push(Line::from(Span::styled(
        " panes arrive in M1.".to_owned(),
        theme.style(element::MUTED),
    )));

    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style(app, Pane::PullRequests))
        .title(" Planned ");

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_shell_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Paths are shortened to the pane's inner width so they never wrap.
    let value_width = usize::from(area.width.saturating_sub(2)).saturating_sub(" home     ".len());

    let row = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!(" {label:<9}"), theme.style(element::HELP_KEY)),
            Span::styled(value, theme.style(element::FG)),
        ])
    };

    lines.push(row(
        "home",
        crate::paths::shorten_for_display(app.home.root(), value_width),
    ));
    lines.push(row(
        "config",
        crate::paths::shorten_for_display(&app.config_path, value_width),
    ));
    lines.push(row("keys", format!("{} parsed", app.document_key_count())));
    lines.push(row(
        "theme",
        format!("{} ({})", app.theme.name(), app.theme_source),
    ));
    lines.push(row(
        "keybinds",
        format!("{} bindings", app.keymap.bindings().len()),
    ));
    lines.push(row("focus", app.focus.label().to_owned()));
    lines.push(row("uptime", format!("{}s", app.uptime_secs())));
    lines.push(Line::default());

    if app.warnings.is_empty() {
        lines.push(row("warnings", "none".to_owned()));
    } else {
        lines.push(row("warnings", app.warnings.len().to_string()));
        for warning in app.warnings.iter().take(4) {
            lines.push(Line::from(Span::styled(
                format!("   · {warning}"),
                theme.style(element::NOTICE_WARN),
            )));
        }
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " ?  help          <leader>  action menu".to_owned(),
        theme.style(element::MUTED),
    )));
    lines.push(Line::from(Span::styled(
        " :  command       <C-c>     quit".to_owned(),
        theme.style(element::MUTED),
    )));

    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style(app, Pane::Diff))
        .title(" This shell ");

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn border_style(app: &App, pane: Pane) -> ratatui::style::Style {
    if app.focus() == pane {
        app.theme.style(element::BORDER_FOCUSED)
    } else {
        app.theme.style(element::BORDER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::test_support::{TempHome, temp_home};
    use crate::tui::app::App;

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
        };
        let startup = crate::Startup::load(&cli).unwrap();
        (dir, App::new(startup).unwrap())
    }

    #[test]
    fn the_too_small_message_names_the_requirement() {
        let (_dir, app) = app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 10)).unwrap();
        terminal
            .draw(|frame| render_too_small(frame, frame.area(), &app.theme))
            .unwrap();
        let rendered = crate::tui::test_support::buffer_to_string(terminal.backend().buffer());
        assert!(rendered.contains("terminal too small"), "{rendered}");
        assert!(rendered.contains("80x24"), "{rendered}");
    }

    #[test]
    fn the_shell_status_reports_where_things_live() {
        let (dir, app) = app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &app))
            .unwrap();
        let rendered = crate::tui::test_support::buffer_to_string(terminal.backend().buffer());

        assert!(rendered.contains("Planned"), "{rendered}");
        assert!(rendered.contains("This shell"), "{rendered}");
        assert!(rendered.contains("warnings"), "{rendered}");
        assert!(rendered.contains("home"), "{rendered}");
        assert!(rendered.contains("config"), "{rendered}");
        // Long paths are shortened rather than wrapped, which would wreck the
        // pane layout (FR-7.8).
        assert!(rendered.contains("…/"), "{rendered}");
        assert!(
            !rendered.contains(&dir.path().display().to_string()),
            "the full path should not be rendered: {rendered}"
        );
        assert!(crate::tui::layout::is_too_small(
            ratatui::layout::Rect::new(0, 0, 60, 10)
        ));
    }
}
