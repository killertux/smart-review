//! The comment composer, the draft panel and the publish modal (FR-6.1–FR-6.3).
//!
//! Three surfaces, one subject: what is about to be sent. They are separated because
//! they answer different questions — *where am I commenting*, *what have I staged*,
//! *is this really what I want to say* — and the last one is the only modal in this
//! application that can put words on the internet.
//!
//! Every function here returns lines rather than drawing, except the top-level
//! `render`s, so what a surface says can be asserted in a test without a frame.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::domain::draft::Decision;
use crate::tui::app::App;
use crate::tui::drafts::{Composer, DraftState};
use crate::tui::markdown;
use crate::tui::text as text_util;
use crate::tui::theme::{Theme, element};

/// How much of the screen the composer takes, in rows, when it is open.
///
/// A share would be wrong here: a comment is a sentence or a paragraph, and the diff
/// underneath it is what the comment is about.
pub const COMPOSER_HEIGHT: u16 = 8;

/// How much of the screen the publish modal takes.
pub const MODAL_PERCENT: u16 = 80;

/// The comment composer, at the bottom of the diff pane (FR-6.2).
///
/// The composer is *not* a popup: the line it will anchor to has to stay visible, or
/// the user is writing about something they cannot see.
pub fn render_composer(frame: &mut Frame<'_>, area: Rect, app: &App, composer: &Composer) {
    let theme = &app.theme;
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.style(element::CHAT_INPUT))
        .title(format!(
            " comment on {} via {} ",
            text_util::truncate(&composer.anchor.label(), area.width.saturating_sub(24) as usize),
            keyboard_hint()
        ));

    let mut lines = composer_lines(composer, theme, area.width);
    if composer.anchor.start_line.is_none() {
        // A range is worth pointing out, because the key that makes one is `V`, which
        // nobody guesses from a composer.
        lines.push(Line::from(Span::styled(
            "   V then j/k then c makes this a range".to_owned(),
            theme.style(element::MUTED),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// How the composer names its own keys.
///
/// One place, because the pane and the help popup disagreeing about how to send a
/// comment is exactly the kind of drift this project keeps paying for.
#[must_use]
pub fn keyboard_hint() -> &'static str {
    "Enter stages it · Alt-Enter for a new line"
}

/// What the composer says: the anchor, the text so far, and why it was refused.
fn composer_lines(composer: &Composer, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let text = composer.input.text();
    if text.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" › ".to_owned(), theme.style(element::MUTED)),
            Span::styled("▏".to_owned(), theme.style(element::ACCENT)),
            Span::styled(
                "what should change, and why?".to_owned(),
                theme.style(element::MUTED),
            ),
        ]));
    } else {
        let inner = usize::from(width.saturating_sub(6));
        for (index, line) in text.lines().enumerate() {
            let mut spans = vec![Span::styled(
                if index == 0 { " › " } else { "   " }.to_owned(),
                theme.style(element::MUTED),
            )];
            spans.push(Span::styled(
                text_util::truncate(line, inner),
                theme.style(element::CHAT_INPUT),
            ));
            if index + 1 == text.lines().count() {
                spans.push(Span::styled("▏".to_owned(), theme.style(element::ACCENT)));
            }
            lines.push(Line::from(spans));
        }
    }

    if let Some(refusal) = &composer.refusal {
        lines.push(Line::from(Span::styled(
            format!("   {refusal}"),
            theme.style(element::NOTICE_ERROR),
        )));
    }
    lines
}

/// The draft panel: every staged comment, numbered (FR-6.1).
pub fn render_panel(frame: &mut Frame<'_>, area: Rect, app: &App, drafts: &DraftState) {
    let theme = &app.theme;
    let area = centered(area, 70, 60);
    frame.render_widget(Clear, area);
    let title = format!(
        " draft · #{} · {} ",
        drafts.draft.pr,
        drafts.draft.summary()
    );
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.style(element::TITLE))
        .title(title);

    let height = usize::from(area.height.saturating_sub(2));
    let lines = panel_lines(app, drafts, area.width.saturating_sub(2));
    let offset = lines.len().saturating_sub(height).saturating_sub(drafts.scroll);
    let visible: Vec<Line<'static>> = lines.into_iter().skip(offset).take(height).collect();
    let footer = Line::from(Span::styled(
        format!(
            " {} ",
            if drafts.draft.comments.is_empty() {
                ":draft list shows the other pull requests · Esc closes".to_owned()
            } else {
                "x removes the selected comment · :draft clear empties it · <leader>rr publishes"
                    .to_owned()
            }
        ),
        theme.style(element::MUTED),
    ));

    let body = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    frame.render_widget(
        Paragraph::new(visible)
            .block(block)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        body[0],
    );
    frame.render_widget(footer.style(theme.style(element::BG)), body[1]);
}

/// The draft panel's contents.
fn panel_lines(app: &App, drafts: &DraftState, width: u16) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(" decision  ".to_owned(), theme.style(element::MUTED)),
        Span::styled(
            drafts
                .draft
                .decision
                .map_or("(not chosen: it will be a comment)".to_owned(), |decision| {
                    decision.label().to_owned()
                }),
            theme.style(element::FG),
        ),
    ]));
    let body = drafts.draft.body.as_deref().unwrap_or("").trim();
    lines.push(Line::from(vec![
        Span::styled(" body      ".to_owned(), theme.style(element::MUTED)),
        Span::styled(
            if body.is_empty() {
                "(empty)".to_owned()
            } else {
                text_util::truncate(body, usize::from(width.saturating_sub(14)))
            },
            theme.style(element::FG),
        ),
    ]));
    if drafts.drifted(app.open_head_sha()) {
        // Anchors are line numbers: a force-push makes them quietly mean something
        // else, which is precisely the accident this line prevents (FR-6.3).
        lines.push(Line::from(Span::styled(
            " ! the diff has moved since these were written: re-check the line numbers"
                .to_owned(),
            theme.style(element::NOTICE_WARN),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!(" staged comments ({})", drafts.draft.comments.len()),
        theme.style(element::TITLE),
    )));

    if drafts.draft.comments.is_empty() {
        lines.push(Line::from(Span::styled(
            "   none yet — `c` on a line of the diff writes one".to_owned(),
            theme.style(element::MUTED),
        )));
        return lines;
    }

    for (index, comment) in drafts.draft.comments.iter().enumerate() {
        let selected = index + 1 == drafts.cursor;
        let style = if selected {
            theme.style(element::SELECTION)
        } else {
            theme.style(element::FG)
        };
        lines.push(Line::from(Span::styled(
            format!(" {}. {}", index + 1, comment.anchor()),
            style,
        )));
        for line in markdown::render(&comment.body, usize::from(width.saturating_sub(6)), theme) {
            let mut spans = vec![Span::styled("    ".to_owned(), style)];
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// The publish modal: everything that is about to be sent, and nothing else (FR-6.3).
pub fn render_modal(frame: &mut Frame<'_>, area: Rect, app: &App, drafts: &DraftState) {
    let theme = &app.theme;
    let area = centered(area, MODAL_PERCENT, MODAL_PERCENT);
    frame.render_widget(Clear, area);

    let decision = drafts.draft.effective_decision();
    let mut title = format!(" publish review · #{} · {} ", drafts.draft.pr, decision.label());
    if drafts.status.is_publishing() {
        title.push_str("· sending… ");
    }
    if app.is_dry_run() {
        title.push_str("· DRY RUN ");
    }
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(theme.style(element::TITLE))
        .title(title);

    let height = usize::from(area.height.saturating_sub(2));
    let lines = modal_lines(app, drafts, area.width.saturating_sub(2));
    let offset = lines.len().saturating_sub(height).saturating_sub(drafts.scroll);
    let visible: Vec<Line<'static>> = lines.into_iter().skip(offset).take(height).collect();

    let body = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(area);
    frame.render_widget(
        Paragraph::new(visible)
            .block(block)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        body[0],
    );

    let mut footer: Vec<Line<'static>> = Vec::new();
    if let crate::tui::drafts::DraftStatus::Failed { reason } = &drafts.status {
        footer.push(Line::from(Span::styled(
            format!(" {reason}"),
            theme.style(element::NOTICE_ERROR),
        )));
        footer.push(Line::from(Span::styled(
            " nothing was posted · the draft is still here · Enter tries again".to_owned(),
            theme.style(element::MUTED),
        )));
    } else if app.is_dry_run() {
        footer.push(Line::from(Span::styled(
            " DRY RUN — nothing will be sent".to_owned(),
            theme.style(element::NOTICE_WARN),
        )));
        footer.push(Line::from(Span::styled(
            " Enter records the commands in logs/dry-run.log".to_owned(),
            theme.style(element::MUTED),
        )));
    } else if drafts.armed {
        footer.push(Line::from(Span::styled(
            " Enter again sends it to GitHub".to_owned(),
            theme.style(element::NOTICE_WARN),
        )));
        footer.push(Line::from(Span::styled(
            " a is approve · r requests changes · c comments · Esc goes back".to_owned(),
            theme.style(element::MUTED),
        )));
    } else {
        footer.push(Line::from(Span::styled(
            " Enter to review it once more, then Enter again to send".to_owned(),
            theme.style(element::FG),
        )));
        footer.push(Line::from(Span::styled(
            " a is approve · r requests changes · c comments · :draft body writes the body"
                .to_owned(),
            theme.style(element::MUTED),
        )));
    }
    frame.render_widget(
        Paragraph::new(footer)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        body[1],
    );
}

/// The modal's contents: the decision, the body, then every comment verbatim.
fn modal_lines(app: &App, drafts: &DraftState, width: u16) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let inner = usize::from(width.saturating_sub(6));
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(" decision  ".to_owned(), theme.style(element::MUTED)),
        Span::styled(
            drafts
                .draft
                .decision
                .map_or("comment (no verdict)".to_owned(), |decision| {
                    format!("{} — {}", decision.label(), verdict_note(decision))
                }),
            theme.style(element::FG),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        " body".to_owned(),
        theme.style(element::MUTED),
    )));
    let body = drafts.draft.body.as_deref().unwrap_or("").trim();
    if body.is_empty() {
        lines.push(Line::from(Span::styled(
            "   (empty — `:draft body <text>` writes one)".to_owned(),
            theme.style(element::MUTED),
        )));
    } else {
        for line in markdown::render(body, inner, theme) {
            let mut spans = vec![Span::styled("   ".to_owned(), theme.style(element::BG))];
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        }
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!(" {} — sent verbatim", drafts.draft.summary()),
        theme.style(element::TITLE),
    )));
    for (index, comment) in drafts.draft.comments.iter().enumerate() {
        lines.push(Line::from(Span::styled(
            format!(" {}. {}", index + 1, comment.anchor()),
            theme.style(element::ACCENT),
        )));
        for line in markdown::render(&comment.body, inner, theme) {
            let mut spans = vec![Span::styled("    ".to_owned(), theme.style(element::BG))];
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// What a decision does, in a few words, on the line that chooses it.
fn verdict_note(decision: Decision) -> &'static str {
    match decision {
        Decision::Approve => "this unblocks the pull request",
        Decision::RequestChanges => "this blocks the merge until it is re-reviewed",
        Decision::Comment => "this decides nothing",
    }
}

/// A rectangle centered in `area`, as a percentage of it.
fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::drafts::{Anchor, Composer, DraftState};

    /// The text of a set of lines, for assertions about what a surface says.
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
    fn the_composer_shows_what_is_typed_and_where_it_will_land() {
        let mut composer = Composer::new(Anchor::line(
            "src/domain/invoice.rs",
            crate::domain::draft::Side::New,
            31,
        ));
        composer.input.insert_str("this rounds up");
        let text = flatten(&composer_lines(&composer, &Theme::default(), 100));
        assert!(text.contains("this rounds up"), "{text}");

        // An empty composer says what to write rather than showing nothing.
        let empty = Composer::new(Anchor::line(
            "src/a.rs",
            crate::domain::draft::Side::New,
            1,
        ));
        let text = flatten(&composer_lines(&empty, &Theme::default(), 100));
        assert!(text.contains("what should change"), "{text}");
    }

    #[test]
    fn a_refusal_is_shown_in_the_composer_rather_than_only_reported() {
        // A keypress that appears to do nothing is the failure mode this avoids: the
        // reason appears where the text is, not in a notice that expires.
        let mut composer = Composer::new(Anchor::line(
            "src/a.rs",
            crate::domain::draft::Side::New,
            1,
        ));
        composer.refusal = Some("a comment needs a body".to_owned());
        let text = flatten(&composer_lines(&composer, &Theme::default(), 100));
        assert!(text.contains("a comment needs a body"), "{text}");
    }

    #[test]
    fn a_range_anchor_is_labelled_as_one() {
        let mut composer = Composer::new(Anchor {
            path: "src/a.rs".to_owned(),
            side: crate::domain::draft::Side::Old,
            line: 31,
            start_line: Some(28),
        });
        composer.input.insert_str("this whole block");
        let text = flatten(&composer_lines(&composer, &Theme::default(), 100));
        assert!(text.contains("this whole block"), "{text}");
        assert_eq!(composer.anchor.label(), "src/a.rs:28-31 (old)");
    }

    #[test]
    fn the_modal_names_the_verdict_it_will_give() {
        assert!(verdict_note(Decision::Approve).contains("unblocks"));
        assert!(verdict_note(Decision::RequestChanges).contains("blocks"));
        assert!(verdict_note(Decision::Comment).contains("decides nothing"));
    }

    #[test]
    fn the_panel_numbers_its_comments_from_one() {
        let state = DraftState::default();
        assert_eq!(state.cursor, 1, "the panel starts on the first row");
    }
}
