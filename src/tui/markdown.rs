//! Rendering an answer's markdown-ish text (FR-5.1, FR-5.3).
//!
//! "Markdown-ish" is the requirement's word, and it is the right one: a model writes
//! prose with a few conventions in it, and a terminal renders those conventions as
//! *style* rather than as syntax. What is supported is what answers actually contain —
//! paragraphs, bullets, numbered lists, headings, fenced code, inline code and bold —
//! and everything else is shown as the text it is. A line the parser does not recognise
//! is never dropped or reflowed into something the model did not write.
//!
//! The one thing the renderer *does* change is the `[general]` marker the chat system
//! prompt asks for (FR-5.3): a sentence the model flagged as general knowledge is
//! displayed with the marker removed and a muted style, so the distinction is visible
//! rather than spelled out. The marker is a best-effort convention — a model that
//! forgets it produces a line that reads as grounded — which is why the interface also
//! shows what the context contained.

use ratatui::text::{Line, Span};

use crate::tui::text as text_util;
use crate::tui::theme::{Theme, element};

/// A block of an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A heading, `#`-prefixed, with its level.
    Heading {
        /// How many `#`s.
        level: usize,
        /// The text, without the markers.
        text: String,
    },
    /// A paragraph: consecutive lines that are not anything else.
    Paragraph(Vec<String>),
    /// A bullet, `-` or `*` prefixed.
    Bullet(String),
    /// A numbered item, kept with the number the model wrote.
    Numbered(String),
    /// A fenced code block.
    Code {
        /// The language the fence named, if it named one.
        language: Option<String>,
        /// The lines, verbatim.
        lines: Vec<String>,
    },
    /// A quote, `>`-prefixed.
    Quote(String),
    /// A rule: `---`.
    Rule,
}

/// Parses an answer into blocks.
///
/// Total: every line ends up in exactly one block, and a block the parser does not
/// recognise is a paragraph line. That is the property that makes this safe to run on a
/// model's output — there is no text it can produce that this loses.
#[must_use]
pub fn parse(answer: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut code: Option<(Option<String>, Vec<String>)> = None;

    // A `flush` closure rather than a helper method: the two accumulators are local
    // state, and passing them around turns two lines of logic into a signature.
    macro_rules! flush {
        () => {
            if !paragraph.is_empty() {
                blocks.push(Block::Paragraph(std::mem::take(&mut paragraph)));
            }
        };
    }

    for raw in answer.lines() {
        let trimmed = raw.trim_end();
        // Inside a fence nothing is interpreted: a code block that contains `# comment`
        // must not become a heading.
        if let Some((language, lines)) = code.as_mut() {
            if trimmed.trim_start().starts_with("```") {
                blocks.push(Block::Code {
                    language: language.clone(),
                    lines: std::mem::take(lines),
                });
                code = None;
            } else {
                lines.push(trimmed.to_owned());
            }
            continue;
        }

        let bare = trimmed.trim_start();
        if let Some(rest) = bare.strip_prefix("```") {
            flush!();
            let language = rest.trim();
            code = Some((
                (!language.is_empty()).then(|| language.to_owned()),
                Vec::new(),
            ));
            continue;
        }

        if bare.is_empty() {
            flush!();
            continue;
        }

        if let Some((level, text)) = heading(bare) {
            flush!();
            blocks.push(Block::Heading {
                level,
                text: text.to_owned(),
            });
            continue;
        }

        if is_rule(bare) {
            flush!();
            blocks.push(Block::Rule);
            continue;
        }

        if let Some(text) = bullet(bare) {
            flush!();
            blocks.push(Block::Bullet(text.to_owned()));
            continue;
        }

        if let Some(text) = numbered(bare) {
            flush!();
            blocks.push(Block::Numbered(text.to_owned()));
            continue;
        }

        if let Some(text) = bare.strip_prefix('>') {
            flush!();
            blocks.push(Block::Quote(text.trim_start().to_owned()));
            continue;
        }

        paragraph.push(bare.to_owned());
    }

    // An unterminated fence is still a code block: a stream that was cut off mid-answer
    // shows what arrived rather than swallowing it into the paragraph above.
    if let Some((language, lines)) = code {
        blocks.push(Block::Code { language, lines });
    }
    flush!();
    blocks
}

/// A heading's level and text, if the line is one.
fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    // `#hashtag` is not a heading; a heading has a space after its hashes.
    if hashes == 0 || hashes > 6 || !line[hashes..].starts_with(' ') {
        return None;
    }
    Some((hashes, line[hashes..].trim()))
}

/// Whether a line is a horizontal rule.
fn is_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3 && (trimmed.chars().all(|c| c == '-') || trimmed.chars().all(|c| c == '*'))
}

/// The text of a bullet, if the line is one.
fn bullet(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest.trim_start());
        }
    }
    None
}

/// The text of a numbered item, keeping the number.
fn numbered(line: &str) -> Option<&str> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = &line[digits..];
    let rest = rest.strip_prefix(['.', ')'])?;
    rest.starts_with(' ').then(|| rest.trim_start())
}

/// Renders an answer at a width.
///
/// The width is the pane's inner width, so the caller does not have to wrap anything
/// itself: a paragraph is reflowed to fit, a code block is clipped rather than wrapped
/// (wrapping code changes what it says), and a bullet's continuation lines are indented
/// under its text.
#[must_use]
pub fn render(answer: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for block in parse(answer) {
        match block {
            Block::Heading { level, text } => {
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                let style = if level <= 2 {
                    theme.style(element::TITLE)
                } else {
                    theme.style(element::ACCENT)
                };
                lines.push(Line::from(Span::styled(text, style)));
            }
            Block::Paragraph(parts) => {
                let joined = parts.join(" ");
                push_wrapped(&mut lines, &joined, width, 0, theme);
            }
            Block::Bullet(text) => {
                push_wrapped(&mut lines, &format!("• {text}"), width, 2, theme);
            }
            Block::Numbered(text) => {
                push_wrapped(&mut lines, &format!("  {text}"), width, 2, theme);
            }
            Block::Quote(text) => {
                push_wrapped(&mut lines, &text, width.saturating_sub(2), 2, theme);
                if let Some(last) = lines.last_mut() {
                    last.spans.insert(
                        0,
                        Span::styled("│ ".to_owned(), theme.style(element::MUTED)),
                    );
                }
            }
            Block::Code {
                language,
                lines: body,
            } => {
                if let Some(language) = language {
                    lines.push(Line::from(Span::styled(
                        format!("  {language}"),
                        theme.style(element::MUTED),
                    )));
                }
                for line in body {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", text_util::truncate(&line, width.saturating_sub(2))),
                        theme.style(element::CODE),
                    )));
                }
            }
            Block::Rule => lines.push(Line::from(Span::styled(
                "─".repeat(width.min(40)),
                theme.style(element::MUTED),
            ))),
        }
    }
    lines
}

/// Appends a block of prose, wrapped, with a hanging indent.
fn push_wrapped(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    width: usize,
    indent: usize,
    theme: &Theme,
) {
    let available = width.saturating_sub(indent).max(1);
    for (index, piece) in text_util::wrap(text, available).into_iter().enumerate() {
        let padding = if index == 0 { 0 } else { indent };
        lines.push(inline(&format!("{}{piece}", " ".repeat(padding)), theme));
    }
}

/// One line of prose, with its inline markup turned into spans.
///
/// Inline markup is `**bold**`, `*italic*` and `` `code` ``. An unmatched marker is
/// left alone: half of a pair is the model's text, not a formatting instruction.
#[must_use]
pub fn inline(text: &str, theme: &Theme) -> Line<'static> {
    let (body, general) = match crate::application::chat::general_knowledge_marker(text) {
        Some(rest) => (rest.to_owned(), true),
        None => (text.to_owned(), false),
    };
    let base = if general {
        theme.style(element::MUTED)
    } else {
        theme.style(element::FG)
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    if general {
        spans.push(Span::styled("· ".to_owned(), base));
    }
    let mut rest = body.as_str();
    while let Some((before, marker, after)) = next_marker(rest) {
        if !before.is_empty() {
            spans.push(Span::styled(before.to_owned(), base));
        }
        let style = match marker {
            '`' => theme.style(element::CODE),
            _ => theme.style(element::ACCENT),
        };
        spans.push(Span::styled(after.0.clone(), style));
        rest = after.1;
    }
    if !rest.is_empty() || spans.is_empty() {
        spans.push(Span::styled(rest.to_owned(), base));
    }
    Line::from(spans)
}

/// The next inline marker, as `(text before, marker, (text inside, rest))`.
fn next_marker(text: &str) -> Option<(&str, char, (String, &str))> {
    let mut search = 0;
    while let Some(offset) = text[search..].find(['*', '`']) {
        let at = search + offset;
        let character = text[at..].chars().next()?;
        // `**bold**` and `*italic*` are the same marker repeated; a single `*` opens and
        // closes its own span either way, which is why the closing search starts after
        // the run of markers.
        let opening = text[at..].chars().take_while(|c| *c == character).count();
        let body_start = at + opening;
        if let Some(close) = text[body_start..].find(character) {
            let body = &text[body_start..body_start + close];
            // An empty span is not markup: `**` with nothing in it is literal text, and
            // reading it as an empty bold run would hide two characters the model wrote.
            if !body.is_empty() {
                // The *whole* closing run belongs to the span. Skipping one marker of a
                // `**bold**` pair leaves the other behind, and that stray marker then
                // pairs with the next unrelated `*` in the answer — which is how "2 * 3"
                // turned into "2  3" and a sentence lost two characters to emphasis.
                let closing = text[body_start + close..]
                    .chars()
                    .take_while(|c| *c == character)
                    .count();
                let after = &text[body_start + close + closing..];
                return Some((&text[..at], character, (body.to_owned(), after)));
            }
        }
        search = body_start;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paragraph_is_prose_and_a_bullet_is_a_bullet() {
        let blocks = parse("The retry loop changed.\n\n- one\n- two\n\nThen it ends.");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph(vec!["The retry loop changed.".to_owned()]),
                Block::Bullet("one".to_owned()),
                Block::Bullet("two".to_owned()),
                Block::Paragraph(vec!["Then it ends.".to_owned()]),
            ]
        );
    }

    #[test]
    fn consecutive_lines_are_one_paragraph() {
        // A model wraps its prose at whatever width it likes; the renderer reflows it,
        // so a hard line break inside a paragraph is not a new paragraph.
        let blocks = parse("one line\nand its continuation");
        assert_eq!(
            blocks,
            vec![Block::Paragraph(vec![
                "one line".to_owned(),
                "and its continuation".to_owned()
            ])]
        );
    }

    #[test]
    fn a_fenced_block_is_never_interpreted() {
        let blocks = parse("before\n```rust\n# not a heading\n- not a bullet\n```\nafter");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph(vec!["before".to_owned()]),
                Block::Code {
                    language: Some("rust".to_owned()),
                    lines: vec!["# not a heading".to_owned(), "- not a bullet".to_owned()],
                },
                Block::Paragraph(vec!["after".to_owned()]),
            ]
        );
    }

    #[test]
    fn an_unterminated_fence_still_shows_what_arrived() {
        // A stream cut off mid-answer, which is exactly when the user most wants to read
        // the part that did arrive.
        let blocks = parse("look:\n```rust\nlet x = 1;");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph(vec!["look:".to_owned()]),
                Block::Code {
                    language: Some("rust".to_owned()),
                    lines: vec!["let x = 1;".to_owned()],
                },
            ]
        );
    }

    #[test]
    fn headings_numbers_quotes_and_rules_are_recognised() {
        let blocks = parse("## Summary\n1. first\n2) second\n> quoted\n---\n#hashtag");
        assert_eq!(
            blocks,
            vec![
                Block::Heading {
                    level: 2,
                    text: "Summary".to_owned()
                },
                Block::Numbered("first".to_owned()),
                Block::Numbered("second".to_owned()),
                Block::Quote("quoted".to_owned()),
                Block::Rule,
                // Not a heading: there is no space after the hash.
                Block::Paragraph(vec!["#hashtag".to_owned()]),
            ]
        );
    }

    #[test]
    fn every_line_of_an_answer_ends_up_in_a_block() {
        // The property that makes this safe on model output: nothing is silently lost,
        // whatever the model writes.
        let answer = "para\n\n```\ncode\n```\n# h\n- b\n1. n\n> q\n---\nplain";
        let rendered = render(answer, 40, &crate::tui::theme::Theme::default());
        let text: String = rendered
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for piece in ["para", "code", "h", "b", "n", "q", "plain"] {
            assert!(text.contains(piece), "{piece} is missing from {text}");
        }
    }

    #[test]
    fn inline_code_and_bold_become_spans_and_unmatched_markers_stay() {
        let theme = crate::tui::theme::Theme::default();
        let line = inline("use `PathIndex` and **never** panic, 2 * 3 = 6", &theme);
        let texts: Vec<&str> = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(texts.contains(&"PathIndex"), "{texts:?}");
        assert!(texts.contains(&"never"), "{texts:?}");
        // The multiplication signs are not emphasis: a `*` with no partner is text. The
        // trap is the closing run of `**bold**` — leaving half of it behind makes it pair
        // with the next `*` in the answer and quietly eat the words between them.
        let joined: String = texts.concat();
        assert!(joined.contains("2 * 3 = 6"), "{joined}");
        // The code span's backticks are gone; the multiplication sign is still there,
        // because it was never markup.
        assert!(texts.iter().all(|text| !text.contains('`')), "{texts:?}");

        // The same, with italic: `*a*` followed by a multiplication sign.
        let line = inline("*see* 2 * 3", &theme);
        let joined: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(joined, "see 2 * 3");
    }

    #[test]
    fn a_general_knowledge_line_is_marked_and_styled_differently() {
        let theme = crate::tui::theme::Theme::default();
        let line = inline("[general] Postgres takes a table lock here.", &theme);
        let joined: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(!joined.contains("[general]"), "{joined}");
        assert!(joined.starts_with("· Postgres"), "{joined}");
        assert_eq!(line.spans[0].style, theme.style(element::MUTED));
    }

    #[test]
    fn a_long_paragraph_is_wrapped_to_the_pane_width() {
        let theme = crate::tui::theme::Theme::default();
        let answer = "word ".repeat(40);
        let lines = render(&answer, 30, &theme);
        assert!(lines.len() > 1, "it wrapped");
        for line in &lines {
            let width: usize = line
                .spans
                .iter()
                .map(|span| text_util::width(span.content.as_ref()))
                .sum();
            assert!(width <= 30, "{width} is wider than the pane: {line:?}");
        }
    }

    #[test]
    fn code_is_clipped_rather_than_wrapped() {
        // Wrapping code changes what it says, which is worse than not showing its end.
        let theme = crate::tui::theme::Theme::default();
        let lines = render(
            "```\nlet value = some_function(a, b, c, d);\n```",
            20,
            &theme,
        );
        let code: String = lines
            .last()
            .expect("a code line")
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(code.contains('…'), "{code}");
        assert_eq!(text_util::width(&code), 20);
    }

    #[test]
    fn a_bullet_continuation_is_indented_under_its_text() {
        let theme = crate::tui::theme::Theme::default();
        let lines = render(
            "- a bullet long enough to wrap in a narrow pane",
            24,
            &theme,
        );
        assert!(lines.len() > 1);
        let first = lines[0].spans[0].content.to_string();
        assert!(first.starts_with("• a bullet"), "{first}");
        let second: String = lines[1]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(second.starts_with("  "), "{second:?}");
    }
}
