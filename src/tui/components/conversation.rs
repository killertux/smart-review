//! The conversation panel: what has been said about the pull request itself (FR-6.4).
//!
//! A panel rather than a block at the top of the diff, for two reasons that came out of
//! looking at the alternatives. The diff's row model is built from a *patch*, and a
//! comment on the conversation has no patch, no file and no line — giving its rows a
//! file index that does not exist would put a special case in every movement, fold and
//! click handler that reads `row.file`. And the pair of things a reader does here — read
//! what was said, then say something — is exactly what a panel is for.
//!
//! The bodies are drawn through the same markdown renderer the review modal and the chat
//! pane use, so a comment with a list or a fenced block reads the same everywhere.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::domain::pr::ConversationComment;
use crate::tui::app::App;
use crate::tui::components::drafts::composer_split;
use crate::tui::text as text_util;
use crate::tui::theme::element;
use crate::tui::{components, markdown};

/// How wide the panel is, as a percentage of the frame.
const PANEL_PERCENT_X: u16 = 80;
/// How tall it is.
const PANEL_PERCENT_Y: u16 = 70;

/// The conversation panel (FR-6.4).
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let area = centered(area, PANEL_PERCENT_X, PANEL_PERCENT_Y);
    frame.render_widget(Clear, area);

    let comments = app.conversation();
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.style(element::TITLE))
        .title(format!(
            " conversation · #{} · {} comment{} ",
            app.detail().map_or(0, |detail| detail.summary.number),
            comments.len(),
            if comments.len() == 1 { "" } else { "s" }
        ));

    // The composer sits *inside* the panel rather than under the diff: the panel is what
    // the user is looking at, and a text box behind an opaque popup is a text box nobody
    // can see.
    let composing = app
        .drafts()
        .composer
        .as_ref()
        .is_some_and(|composer| !composer.target.is_staged());
    let (body, composer_area) = composer_split(area, composing);

    let height = usize::from(body.height.saturating_sub(2));
    let lines = lines(
        comments,
        app.discussion().index(),
        app.now(),
        theme,
        body.width.saturating_sub(2),
    );
    let selected = lines.iter().position(|line| {
        line.spans
            .first()
            .is_some_and(|span| span.content.as_ref().contains('▸'))
    });
    let offset = selected.map_or_else(
        || components::clamp_offset(app.discussion().scroll, height, lines.len()),
        |selected| {
            components::ensure_visible(selected, app.discussion().scroll, height, lines.len())
        },
    );
    let visible: Vec<Line<'static>> = lines.into_iter().skip(offset).take(height).collect();

    let footer = Line::from(Span::styled(
        format!(" {} ", footer_hint(app, comments.len())),
        theme.style(element::MUTED),
    ));
    let split = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(body);
    frame.render_widget(
        Paragraph::new(visible)
            .block(block)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        split[0],
    );
    frame.render_widget(footer.style(theme.style(element::BG)), split[1]);

    if let (Some(area), Some(composer)) = (composer_area, app.drafts().composer.as_ref()) {
        super::drafts::render_composer(frame, area, app, composer);
    }
}

/// What the panel's footer says the keys do.
fn footer_hint(app: &App, count: usize) -> String {
    if app.draft_is_composing() {
        return app
            .drafts()
            .composer
            .as_ref()
            .map_or_else(String::new, |composer| {
                composer.target.enter_hint().to_owned()
            });
    }
    if count == 0 {
        return "c writes the first comment · Esc closes".to_owned();
    }
    "j/k move · c writes a comment · R refreshes · Esc closes".to_owned()
}

/// The panel's contents: one block per comment, newest last.
///
/// Takes the pieces rather than the whole [`App`] so it can be tested by handing it a
/// list of comments, which is what a test of "what does this draw" should have to do.
fn lines(
    comments: &[ConversationComment],
    selected: Option<usize>,
    now: crate::domain::time::Timestamp,
    theme: &crate::tui::theme::Theme,
    width: u16,
) -> Vec<Line<'static>> {
    let inner = usize::from(width.saturating_sub(4));
    if comments.is_empty() {
        return vec![
            Line::from(Span::styled(
                " Nothing has been said about this pull request yet.".to_owned(),
                theme.style(element::MUTED),
            )),
            Line::default(),
            Line::from(Span::styled(
                " The conversation is where the *why* of a pull request usually lives — the \
                 issue it came from, what was already tried, what was deliberately left out."
                    .to_owned(),
                theme.style(element::MUTED),
            )),
        ];
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, comment) in comments.iter().enumerate() {
        let marker = if Some(index) == selected { "▸" } else { " " };
        let style = if Some(index) == selected {
            theme.style(element::ACCENT)
        } else {
            theme.style(element::MUTED)
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), style),
            Span::styled(
                text_util::truncate(
                    &format!(
                        "{} · {}",
                        comment.author,
                        crate::domain::time::relative(now, comment.created_at)
                    ),
                    40,
                ),
                theme.style(element::TITLE),
            ),
        ]));
        for line in markdown::render(&comment.body, inner, theme) {
            let mut spans = vec![Span::styled("     ".to_owned(), theme.style(element::BG))];
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        }
        lines.push(Line::default());
    }
    lines
}

/// A rectangle centred in `area`, as a percentage of it.
fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    let horizontal = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1]);
    horizontal[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::time::from_unix_secs;
    use crate::tui::theme::Theme;

    fn now() -> crate::domain::time::Timestamp {
        from_unix_secs(1_700_000_600)
    }

    fn comment(id: u64, author: &str, body: &str) -> ConversationComment {
        ConversationComment {
            id,
            author: author.to_owned(),
            body: body.to_owned(),
            created_at: from_unix_secs(1_700_000_000),
            url: None,
        }
    }

    fn flatten(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_comment_is_drawn_with_its_author() {
        let comments = vec![
            comment(1, "alice", "this came out of the incident"),
            comment(2, "bob", "and it needs a migration first"),
        ];
        let text = flatten(&lines(&comments, Some(1), now(), &Theme::default(), 100));
        assert!(text.contains("alice · 10m"), "and when it was said: {text}");
        assert!(text.contains("alice"), "{text}");
        assert!(text.contains("this came out of the incident"), "{text}");
        assert!(text.contains("bob"), "{text}");
        assert!(text.contains("and it needs a migration first"), "{text}");
    }

    #[test]
    fn an_empty_conversation_says_so_rather_than_drawing_nothing() {
        let text = flatten(&lines(&[], None, now(), &Theme::default(), 100));
        assert!(text.contains("Nothing has been said"), "{text}");
        assert!(
            text.contains("why"),
            "and says what the panel is for: {text}"
        );
    }

    #[test]
    fn the_body_is_rendered_as_markdown_like_every_other_body() {
        let comments = vec![comment(1, "alice", "- one\n- two")];
        let text = flatten(&lines(&comments, None, now(), &Theme::default(), 100));
        assert!(text.contains("one"), "{text}");
        assert!(text.contains("two"), "{text}");
        // The bullet is the renderer's, not the raw `-`.
        assert!(!text.contains("- one"), "the markdown was rendered: {text}");
    }

    #[test]
    fn the_selected_comment_is_marked_and_the_others_are_not() {
        let comments = vec![comment(1, "alice", "one"), comment(2, "bob", "two")];
        let text = flatten(&lines(&comments, Some(1), now(), &Theme::default(), 100));
        let selected: Vec<&str> = text.lines().filter(|line| line.contains('▸')).collect();
        assert_eq!(selected.len(), 1, "exactly one row is marked: {text}");
        assert!(selected[0].contains("bob"), "{text}");
    }
}
