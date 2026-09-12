//! The chat pane (FR-5.1–FR-5.4).
//!
//! A pane rather than a popup, and along the bottom of the review screen rather than
//! beside it: the point of asking about a pull request is that the code and the answer
//! are readable at the same time, and a popup over the diff forces the user to choose.
//! It is also the first pane that holds an *input*, so it is the first place the status
//! line's `INSERT` mode means something the user should see rather than infer.
//!
//! The conversation is drawn as messages: a question in the question style, an answer
//! rendered through [`crate::tui::markdown`], the answer in flight as a partial message,
//! and — at the end — the compose box with the cursor in it. Everything below the
//! message list is fixed height, so the list is what scrolls.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::domain::chat::{Message, Role};
use crate::tui::app::{App, Pane};
use crate::tui::chat::{ChatLine, ChatState, ChatStatus};
use crate::tui::components::border_style;
use crate::tui::markdown;
use crate::tui::text as text_util;
use crate::tui::theme::{Theme, element};

/// How much of the review screen the chat pane takes when it is open, in percent.
///
/// A share of the height rather than a fixed number of rows: on a tall terminal a
/// conversation deserves more room, and on a short one it must not eat the diff.
pub const CHAT_HEIGHT_PERCENT: u32 = 45;

/// The fewest rows the chat pane can be useful in (border, one message, the compose
/// box).
pub const CHAT_MIN_HEIGHT: u16 = 7;

/// The rows the diff keeps when the chat pane is open.
pub const DIFF_MIN_HEIGHT: u16 = 6;

/// The rows the compose box takes: up to four lines of typed text plus its border.
pub const COMPOSE_ROWS: u16 = 5;

/// Splits the review body into the diff area and the chat pane.
///
/// One function, called by the renderer *and* by the event loop that places a mouse
/// event, for the same reason [`crate::tui::components::pr_list::row_at`] exists: two
/// copies of this arithmetic is how a click comes to select the row below the one it
/// was aimed at (FR-7.5).
#[must_use]
pub fn chat_split(body: Rect, open: bool) -> (Rect, Option<Rect>) {
    if !open {
        return (body, None);
    }
    // Integer arithmetic on a percentage rather than a float cast: `body.height` is at
    // most a few hundred, so the difference is a row at the boundary and the cast is a
    // lint that would need an allow.
    let wanted =
        u16::try_from(u32::from(body.height) * CHAT_HEIGHT_PERCENT / 100).unwrap_or(body.height);
    let wanted = wanted
        .max(CHAT_MIN_HEIGHT)
        .min(body.height.saturating_sub(DIFF_MIN_HEIGHT));
    // A body too short to hold both gets no chat pane: an unusable sliver of a pane is
    // worse than saying "your terminal is too small", which the layout already does.
    if wanted < CHAT_MIN_HEIGHT || body.height < CHAT_MIN_HEIGHT + DIFF_MIN_HEIGHT {
        return (body, None);
    }
    let rows = Layout::vertical([Constraint::Min(DIFF_MIN_HEIGHT), Constraint::Length(wanted)])
        .split(body);
    (rows[0], Some(rows[1]))
}

/// Renders the chat pane (FR-5.1, FR-5.2).
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(chat) = app.chat_state() else {
        return;
    };
    let theme = &app.theme;
    let focused = app.focus() == Pane::Chat;
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style(app, Pane::Chat))
        .title(title(chat, app));

    let inner = block.inner(area);
    frame.render_widget(block.style(theme.style(element::BG)), area);
    if inner.height < 2 || inner.width < 8 {
        return;
    }

    let rows =
        Layout::vertical([Constraint::Min(1), Constraint::Length(compose_rows(chat))]).split(inner);
    render_messages(frame, rows[0], app, chat);
    render_compose(frame, rows[1], app, chat, focused);
}

/// The pane title: which conversation, and what it has cost (FR-5.1, FR-5.4).
fn title(chat: &ChatState, app: &App) -> String {
    if chat.listing {
        return format!(
            " chat · {} session(s) — Enter opens, Esc closes ",
            chat.sessions.len()
        );
    }
    let totals = chat.totals();
    let provenance = match &chat.session {
        Some(session) => format!(
            "{} · started at {}",
            session.model,
            crate::domain::chat::short_sha(&session.head_sha)
        ),
        None => app.model_label(),
    };
    let mut title = format!(" chat · {provenance} · {} ", totals.label());
    if let Some(session) = &chat.session
        && let Some(detail) = app.detail()
        && session.is_stale_against(&detail.summary.head_sha)
    {
        // DEC-15's ask, for a conversation rather than an analysis: the context the
        // user approved is no longer the code on screen, and saying so is the whole
        // point of recording where the conversation started.
        let _ = std::fmt::Write::write_fmt(
            &mut title,
            format_args!(
                "· the pull request has moved to {} ",
                crate::domain::chat::short_sha(&detail.summary.head_sha)
            ),
        );
    }
    title
}

/// How many rows the compose box needs.
fn compose_rows(chat: &ChatState) -> u16 {
    let lines = chat.input.lines().len().clamp(1, 4);
    if chat.is_confirming() {
        // The confirmation takes over the compose box: it is where the user's attention
        // is, and the question it asks is the one that decides whether anything is sent.
        return 3;
    }
    u16::try_from(lines).unwrap_or(1) + 3
}

/// The conversation, scrolled to the bottom unless the user moved up.
fn render_messages(frame: &mut Frame<'_>, area: Rect, app: &App, chat: &ChatState) {
    let theme = &app.theme;
    let width = usize::from(area.width).saturating_sub(2);

    if chat.listing {
        render_list(frame, area, app, chat);
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    if chat.sessions.is_empty() && chat.session.is_none() {
        lines.extend(opening_lines(chat, app, theme));
    }
    for line in chat.messages() {
        match line {
            ChatLine::Stored(index) => {
                if let Some(session) = &chat.session
                    && let Some(message) = session.messages.get(index)
                {
                    lines.extend(message_lines(message, width, theme, app));
                    lines.push(Line::default());
                }
            }
            ChatLine::Streaming => {
                lines.extend(streaming_lines(chat, width, theme));
                lines.push(Line::default());
            }
            ChatLine::Failed => {
                lines.extend(failed_lines(chat, theme));
                lines.push(Line::default());
            }
        }
    }

    let height = usize::from(area.height);
    // The scroll is measured from the bottom, because a conversation is read at its
    // end: without a scroll the newest thing said is what the user wants to see.
    let offset = lines
        .len()
        .saturating_sub(height)
        .saturating_sub(chat.scroll);
    let visible: Vec<Line<'static>> = lines.into_iter().skip(offset).take(height).collect();
    frame.render_widget(
        Paragraph::new(visible)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// What an empty pane says, which is the whole of chat's discoverability.
fn opening_lines(chat: &ChatState, app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            " Ask about this pull request.".to_owned(),
            theme.style(element::CHAT_ANSWER),
        )),
        Line::default(),
        Line::from(Span::styled(
            " The model sees the diff, the changed files and the commit messages — exactly \
             what `:context` lists — and nothing else. It cannot read the repository."
                .to_owned(),
            theme.style(element::MUTED),
        )),
    ];
    if app.active_model().is_none() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!(
                " No model is configured yet: press <leader>m. {}",
                app.model_problem().unwrap_or("")
            ),
            theme.style(element::NOTICE_WARN),
        )));
    }
    if !chat.added.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!(" Added to the context: {}", chat.added.join(", ")),
            theme.style(element::MUTED),
        )));
    }
    lines
}

/// The session list, for `:chat list` (FR-5.1).
fn render_list(frame: &mut Frame<'_>, area: Rect, app: &App, chat: &ChatState) {
    let theme = &app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if chat.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            " no conversations yet".to_owned(),
            theme.style(element::MUTED),
        )));
    }
    for meta in &chat.sessions {
        let current = chat
            .session
            .as_ref()
            .is_some_and(|session| session.id == meta.id);
        let style = if current {
            theme.style(element::SELECTION)
        } else {
            theme.style(element::CHAT_ANSWER)
        };
        lines.push(Line::from(vec![
            Span::styled(if current { " ▶ " } else { "   " }.to_owned(), style),
            Span::styled(format!("{} ", meta.id), style),
            Span::styled(
                format!(
                    "{} · {} turn(s) · ~{} tokens",
                    crate::domain::time::relative(
                        crate::domain::time::from_unix_secs(
                            i64::try_from(app.now_unix()).unwrap_or(i64::MAX)
                        ),
                        crate::domain::time::from_unix_secs(
                            i64::try_from(meta.updated_at).unwrap_or(i64::MAX)
                        )
                    ),
                    meta.turns,
                    crate::domain::chat::thousands(meta.tokens)
                ),
                theme.style(element::MUTED),
            ),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " `:chat open <id>` opens one · `:chat new` starts another · `:chat export md` keeps a copy"
            .to_owned(),
        theme.style(element::MUTED),
    )));
    frame.render_widget(Paragraph::new(lines).style(theme.style(element::BG)), area);
}

/// One stored message, as lines.
fn message_lines(message: &Message, width: usize, theme: &Theme, app: &App) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    match message.role {
        Role::User => {
            lines.push(Line::from(Span::styled(
                " you".to_owned(),
                theme.style(element::CHAT_QUESTION),
            )));
            for line in text_util::wrap(&message.text, width) {
                lines.push(Line::from(Span::styled(
                    format!(" {line}"),
                    theme.style(element::CHAT_ANSWER),
                )));
            }
        }
        Role::Assistant => {
            let marker = if message.partial {
                " model (stopped)"
            } else {
                " model"
            };
            lines.push(Line::from(Span::styled(
                marker.to_owned(),
                if message.partial {
                    theme.style(element::CHAT_STOPPED)
                } else {
                    theme.style(element::MUTED)
                },
            )));
            let body = markdown::render(&message.text, width.saturating_sub(1), theme);
            for line in body {
                let mut spans = vec![Span::styled(" ".to_owned(), theme.style(element::BG))];
                spans.extend(line.spans);
                lines.push(Line::from(spans));
            }
        }
    }
    lines.extend(footer_for(message, theme, app));
    lines
}

/// What goes under a message: its references, its cost, and what it left out.
fn footer_for(message: &Message, theme: &Theme, app: &App) -> Vec<Line<'static>> {
    let mut footer = Vec::new();
    if !message.references.is_empty() {
        // The references are what makes an answer jumpable (FR-5.1). They are named as
        // paths the diff knows, and the jump itself is `:plan`/`<Enter>` on the tree —
        // a click on a path is M5's `<C-click>` (§5.5).
        let in_diff = message
            .references
            .iter()
            .filter(|reference| app.path_in_diff(&reference.path))
            .map(crate::domain::chat::Reference::label)
            .collect::<Vec<_>>();
        if !in_diff.is_empty() {
            footer.push(Line::from(Span::styled(
                format!("   in this change: {}", in_diff.join(", ")),
                theme.style(element::CHAT_REFERENCE),
            )));
        }
    }
    if let Some(usage) = message.usage {
        let cost = crate::application::chat::answer_cost(Some(usage), app.model_cost().as_ref());
        let mut text = format!("   ~{} tokens", crate::domain::chat::thousands(usage.total));
        if let Some(cost) = cost {
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!(" · ~{}", crate::domain::chat::format_cost(cost)),
            );
        }
        footer.push(Line::from(Span::styled(text, theme.style(element::MUTED))));
    }
    footer
}

/// A request that produced no answer, with the reason (FR-9.1).
fn failed_lines(chat: &ChatState, theme: &Theme) -> Vec<Line<'static>> {
    let ChatStatus::Failed { reason } = &chat.status else {
        return Vec::new();
    };
    vec![
        Line::from(Span::styled(
            " model (failed)".to_owned(),
            theme.style(element::NOTICE_ERROR),
        )),
        Line::from(Span::styled(
            format!("   {reason}"),
            theme.style(element::FG),
        )),
        Line::from(Span::styled(
            "   r repeats the question · `:model` shows what is configured".to_owned(),
            theme.style(element::MUTED),
        )),
    ]
}

/// The answer arriving now, plus what the last request left out (FR-5.2, FR-4.6).
fn streaming_lines(chat: &ChatState, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        match chat.status {
            ChatStatus::Stopped => " model (stopped)".to_owned(),
            _ => " model…".to_owned(),
        },
        if chat.status == ChatStatus::Stopped {
            theme.style(element::CHAT_STOPPED)
        } else {
            theme.style(element::MUTED)
        },
    ))];
    let text = chat.stream.text();
    if text.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("   {}", chat.status.label()),
            theme.style(element::MUTED),
        )));
        return lines;
    }
    // The preview is rendered the same way a stored answer is, so the text does not
    // visibly reflow when it lands — the only difference is the cursor at the end.
    let body = markdown::render(text, width.saturating_sub(1), theme);
    let last = body.len().saturating_sub(1);
    for (index, line) in body.into_iter().enumerate() {
        let mut spans = vec![Span::styled(" ".to_owned(), theme.style(element::BG))];
        spans.extend(line.spans);
        if index == last {
            spans.push(Span::styled("▌".to_owned(), theme.style(element::ACCENT)));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// The compose box: the confirmation, or what the user is typing (FR-5.2, FR-4.6).
fn render_compose(frame: &mut Frame<'_>, area: Rect, app: &App, chat: &ChatState, focused: bool) {
    let theme = &app.theme;
    let style = theme.style(if focused {
        element::CHAT_INPUT
    } else {
        element::MUTED
    });
    let mut lines: Vec<Line<'static>> = Vec::new();

    if chat.is_confirming() {
        lines.push(Line::from(Span::styled(
            " Nothing has been sent yet.".to_owned(),
            theme.style(element::NOTICE_WARN),
        )));
        lines.push(Line::from(Span::styled(
            match &chat.estimate {
                Some(estimate) => format!(
                    " This would send {} to {}. Press Enter again to send, Esc to cancel.",
                    estimate.label(),
                    app.model_label()
                ),
                None => " The context is being gathered…".to_owned(),
            },
            style,
        )));
    } else {
        let (row, column) = chat.input.position();
        let line = chat.input.lines().get(row).copied().unwrap_or("");
        let before: String = line.chars().take(column).collect();
        let after: String = line.chars().skip(column).collect();
        lines.push(Line::from(vec![
            Span::styled(" › ".to_owned(), theme.style(element::ACCENT)),
            Span::styled(before, style),
            // The cursor is a real block, not a hopeful one: the terminal's own cursor
            // is not visible in a ratatui pane, so it is drawn.
            Span::styled("▏".to_owned(), theme.style(element::ACCENT)),
            Span::styled(after, style),
        ]));
        if chat.input.lines().len() > 1 {
            lines.push(Line::from(Span::styled(
                format!(
                    "   ({} lines; Enter sends, Alt-Enter or Ctrl-J adds a line)",
                    chat.input.lines().len()
                ),
                theme.style(element::MUTED),
            )));
        }
        // The footer says what is available, including what is not: a chat that is
        // waiting for a model, or that failed, must not look like one that is ready.
        let mut hints = Vec::new();
        if chat.status.is_running() {
            hints.push("Esc stops it".to_owned());
        } else if chat
            .session
            .as_ref()
            .is_some_and(|session| session.turns() > 0)
        {
            hints.push("r repeats the last question".to_owned());
        }
        if !chat.added.is_empty() {
            hints.push(format!("{} added file(s)", chat.added.len()));
        }
        if chat.dropped > 0 {
            hints.push(format!("{} earlier message(s) not sent", chat.dropped));
        }
        if hints.is_empty() {
            hints.push("`:context` shows what is sent".to_owned());
        }
        lines.push(Line::from(Span::styled(
            format!("  {}", hints.join(" · ")),
            theme.style(element::MUTED),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(height: u16) -> Rect {
        Rect::new(0, 0, 120, height)
    }

    #[test]
    fn a_closed_pane_leaves_the_body_whole() {
        let (main, chat) = chat_split(body(40), false);
        assert_eq!(main, body(40));
        assert!(chat.is_none());
    }

    #[test]
    fn an_open_pane_takes_a_share_and_leaves_the_diff_usable() {
        let (main, chat) = chat_split(body(40), true);
        let chat = chat.expect("the pane is there");
        assert!(chat.height >= CHAT_MIN_HEIGHT, "{}", chat.height);
        assert!(main.height >= DIFF_MIN_HEIGHT, "{}", main.height);
        assert_eq!(main.height + chat.height, 40, "they tile the body");
        // The chat is below the diff: reading the code and asking about it.
        assert!(chat.y > main.y);
    }

    #[test]
    fn a_short_body_gets_no_chat_pane_rather_than_a_sliver() {
        // Below this the pane could show neither a message nor the compose box, and an
        // unusable pane is worse than telling the user the terminal is too small.
        let (main, chat) = chat_split(body(CHAT_MIN_HEIGHT + DIFF_MIN_HEIGHT - 1), true);
        assert!(chat.is_none());
        assert_eq!(main, body(CHAT_MIN_HEIGHT + DIFF_MIN_HEIGHT - 1));
    }

    #[test]
    fn the_split_never_exceeds_the_body_it_was_given() {
        for height in 1..60u16 {
            let (main, chat) = chat_split(body(height), true);
            if let Some(chat) = chat {
                assert_eq!(main.height + chat.height, height, "height {height}");
                assert_eq!(main.y, 0);
                assert_eq!(chat.y, main.height);
            }
        }
    }

    #[test]
    fn the_compose_box_grows_with_the_lines_that_were_typed() {
        let mut chat = ChatState::default();
        assert_eq!(compose_rows(&chat), 4, "one line plus the hints");
        chat.input.set_text("one\ntwo\nthree\nfour\nfive\nsix");
        assert_eq!(
            compose_rows(&chat),
            7,
            "capped at four lines plus the hints"
        );
        chat.awaiting_confirmation = Some("why?".to_owned());
        assert_eq!(compose_rows(&chat), 3, "the confirmation takes the box");
    }
}
