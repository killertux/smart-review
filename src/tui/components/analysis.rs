//! The analysis popup, the context inspector and the raw-answer view (FR-4.1, FR-4.6).
//!
//! Three views of the same thing: what the model said about the pull request, what was
//! sent to it, and — when it answered something unusable — the text itself. They share
//! a popup shape so that moving between them is not a change of context.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::domain::analysis::Severity;
use crate::tui::app::{AnalysisState, App};
use crate::tui::components::height_for;
use crate::tui::layout;
use crate::tui::theme::{Theme, element};

/// Renders the analysis panel: the answer, or the progress towards it (FR-4.1).
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut lines = panel_lines(app, theme);

    // The footer says how to leave and what else is available, which is the one thing
    // a popup has to do beyond showing content.
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " a reopens this · o switches order · `:analysis raw` shows the model's text · Esc closes"
            .to_owned(),
        theme.style(element::MUTED),
    )));

    let height = height_for(lines.len()).saturating_add(2).min(area.height);
    let popup = layout::centered(area, area.width.saturating_sub(4), height);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_style(theme.style(element::BORDER_FOCUSED))
                    .title(panel_title(app)),
            )
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// The title, which states where the analysis came from (FR-4.3).
fn panel_title(app: &App) -> String {
    match app.analysis_state() {
        AnalysisState::Ready | AnalysisState::Idle => match app.analysis_provenance() {
            Some(provenance) => format!(" analysis · {provenance} "),
            None => " analysis ".to_owned(),
        },
        state => format!(" analysis · {} ", state.label()),
    }
}

/// The body of the panel (FR-4.1, FR-4.2).
fn panel_lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    match app.analysis_state() {
        // While a run is in flight the panel shows what arrived, because the answer
        // streaming in is the progress indicator that costs nothing (FR-4.4).
        AnalysisState::Gathering
        | AnalysisState::Running { .. }
        | AnalysisState::Streaming { .. } => {
            lines.push(Line::from(vec![
                Span::styled(" ", theme.style(element::FG)),
                Span::styled(app.analysis_state().label(), theme.style(element::ACCENT)),
                Span::styled(
                    if app.analysis_state().is_running() {
                        "  (Esc cancels)"
                    } else {
                        ""
                    }
                    .to_owned(),
                    theme.style(element::MUTED),
                ),
            ]));
            lines.push(Line::default());
            let text = app.analysis_stream();
            if text.is_empty() {
                lines.push(Line::from(Span::styled(
                    " waiting for the first token…".to_owned(),
                    theme.style(element::MUTED),
                )));
            }
            // Only the tail is drawn: the interesting part of a stream is the end, and
            // the answer is displayed properly once it has been parsed.
            for line in text
                .lines()
                .rev()
                .take(24)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                lines.push(Line::from(Span::styled(
                    format!(" {}", crate::tui::text::truncate(line, 120)),
                    theme.style(element::FG),
                )));
            }
            if app.analysis_stream_truncated() {
                lines.push(Line::from(Span::styled(
                    " … the preview is bounded; the full text is used once it is complete"
                        .to_owned(),
                    theme.style(element::MUTED),
                )));
            }
        }
        AnalysisState::Unusable { reason } => {
            lines.push(Line::from(Span::styled(
                format!(" the answer could not be used: {reason}"),
                theme.style(element::NOTICE_WARN),
            )));
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                " `:analysis raw` shows what the model actually wrote.".to_owned(),
                theme.style(element::MUTED),
            )));
        }
        AnalysisState::Cancelled => {
            lines.push(Line::from(Span::styled(
                " the analysis was cancelled".to_owned(),
                theme.style(element::MUTED),
            )));
            lines.push(Line::from(Span::styled(
                " the tree keeps the order it had; <leader>a starts again".to_owned(),
                theme.style(element::MUTED),
            )));
        }
        AnalysisState::Ready | AnalysisState::Idle => {
            lines.extend(answer_lines(app, theme));
        }
    }

    lines
}

/// The parsed answer: summary, intent, risks, questions and the plan (FR-4.1).
///
/// One function per section, because a section is what a reviewer reads and what a
/// future milestone will want to reorder or fold.
fn answer_lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let Some(panel) = app.analysis_panel() else {
        lines.push(Line::from(Span::styled(
            " no analysis yet: <leader>a analyses this pull request".to_owned(),
            theme.style(element::MUTED),
        )));
        if !app.has_model() {
            lines.push(Line::from(Span::styled(
                " choose a provider and model first: <leader>m".to_owned(),
                theme.style(element::MUTED),
            )));
        }
        return lines;
    };

    if app.analysis_is_stale() {
        // Never presented as current, but never thrown away either (FR-4.3, DEC-15).
        lines.push(Line::from(Span::styled(
            " this analysed an older commit; <leader>a re-runs it for the current one".to_owned(),
            theme.style(element::NOTICE_WARN),
        )));
        lines.push(Line::default());
    }

    if !panel.summary.is_empty() {
        lines.extend(section(theme, "Summary", &panel.summary));
    }
    if !panel.intent.is_empty() {
        lines.push(Line::default());
        lines.extend(section(theme, "Why", &panel.intent));
    }
    lines.extend(risk_lines(theme, &panel.risks));
    lines.extend(plan_lines(app, theme));
    lines.extend(question_lines(theme, &panel.questions));
    lines.extend(warning_lines(app, theme));
    lines
}

/// A heading and its wrapped body.
fn section(theme: &Theme, title: &str, body: &str) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        format!(" {title}"),
        theme.style(element::TITLE),
    ))];
    for line in wrap_paragraph(body, 96) {
        lines.push(Line::from(Span::styled(line, theme.style(element::FG))));
    }
    lines
}

/// The risk areas, most serious first (FR-4.1).
fn risk_lines(theme: &Theme, risks: &[(Severity, String, String)]) -> Vec<Line<'static>> {
    if risks.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![Line::default(), heading(theme, "Risks")];
    for (severity, title, why) in risks {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<6} ", severity.label()),
                Severity::style(*severity, theme),
            ),
            Span::styled(title.clone(), theme.style(element::FG)),
        ]));
        for line in wrap_paragraph(why, 88) {
            lines.push(Line::from(Span::styled(
                format!("          {line}"),
                theme.style(element::MUTED),
            )));
        }
    }
    lines
}

/// The review order, with the reason each group sits where it does (FR-4.2).
fn plan_lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let Some(plan) = app.plan() else {
        return Vec::new();
    };
    let mut lines = vec![
        Line::default(),
        Line::from(vec![
            Span::styled(" Review order".to_owned(), theme.style(element::TITLE)),
            Span::styled(
                format!("  ({})", plan.source.label()),
                theme.style(element::MUTED),
            ),
        ]),
    ];
    for group in &plan.groups {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {}. {}", group.order, group.group),
                theme.style(element::ACCENT),
            ),
            Span::styled(
                format!("  {} file(s)", group.files.len()),
                theme.style(element::MUTED),
            ),
        ]));
        for line in wrap_paragraph(&group.rationale, 88) {
            lines.push(Line::from(Span::styled(
                format!("      {line}"),
                theme.style(element::MUTED),
            )));
        }
    }
    if plan.overridden {
        lines.push(Line::from(Span::styled(
            "  your manual order is in force; `:plan reset` returns to the analysis's".to_owned(),
            theme.style(element::MUTED),
        )));
    }
    lines.push(Line::from(Span::styled(
        "  J/K move the group under the tree cursor; `:plan move <file> <group>` pins a file"
            .to_owned(),
        theme.style(element::MUTED),
    )));
    lines
}

/// The questions worth asking the author (FR-4.1).
fn question_lines(theme: &Theme, questions: &[String]) -> Vec<Line<'static>> {
    if questions.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![Line::default(), heading(theme, "Worth asking")];
    for question in questions {
        for (index, line) in wrap_paragraph(question, 92).into_iter().enumerate() {
            let prefix = if index == 0 { "  · " } else { "    " };
            lines.push(Line::from(Span::styled(
                format!("{prefix}{line}"),
                theme.style(element::FG),
            )));
        }
    }
    lines
}

/// What normalization corrected, so a plan that lost a file is never a silent loss
/// (FR-4.1).
fn warning_lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    if app.analysis_warnings().is_empty() {
        return Vec::new();
    }
    let mut lines = vec![Line::default(), heading(theme, "Corrections")];
    for warning in app.analysis_warnings() {
        for (index, line) in wrap_paragraph(warning, 92).into_iter().enumerate() {
            let prefix = if index == 0 { "  ! " } else { "    " };
            lines.push(Line::from(Span::styled(
                format!("{prefix}{line}"),
                theme.style(element::NOTICE_WARN),
            )));
        }
    }
    lines
}

/// One section heading, in the style every section uses.
fn heading(theme: &Theme, title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {title}"),
        theme.style(element::TITLE),
    ))
}

/// Renders the context inspector: what would be sent, and what was not (FR-4.6).
pub fn render_context(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();

    match app.context_bundle() {
        Some(bundle) => {
            lines.push(Line::from(Span::styled(
                format!(" {}", bundle.summary()),
                theme.style(element::ACCENT),
            )));
            lines.push(Line::default());
            for segment in &bundle.segments {
                let (mark, style) = if !segment.included {
                    ("✗", theme.style(element::MUTED))
                } else if segment.truncated {
                    ("~", theme.style(element::NOTICE_WARN))
                } else {
                    ("✓", theme.style(element::FG))
                };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {mark} "), style),
                    Span::styled(
                        format!("{:<30}", crate::tui::text::truncate(&segment.label, 30)),
                        theme.style(element::FG),
                    ),
                    Span::styled(
                        if segment.included {
                            format!("{:>8} tok", segment.tokens())
                        } else {
                            "  not sent".to_owned()
                        },
                        theme.style(element::MUTED),
                    ),
                ]));
                if let Some(detail) = &segment.detail {
                    lines.push(Line::from(Span::styled(
                        format!("     {}", crate::tui::text::truncate(detail, 100)),
                        theme.style(element::MUTED),
                    )));
                }
            }
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                " ~12k tok means an estimate at four bytes per token, not a tokenizer.".to_owned(),
                theme.style(element::MUTED),
            )));
        }
        None => {
            lines.push(Line::from(Span::styled(
                " nothing has been gathered yet".to_owned(),
                theme.style(element::MUTED),
            )));
        }
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " `:context` gathers if needed · Esc closes".to_owned(),
        theme.style(element::MUTED),
    )));

    let height = height_for(lines.len()).saturating_add(2).min(area.height);
    let popup = layout::centered(area, area.width.saturating_sub(4), height);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_style(theme.style(element::BORDER_FOCUSED))
                    .title(" context "),
            )
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// Renders the model's raw text when it could not be used (FR-4.1).
pub fn render_raw(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();

    match app.raw_answer() {
        Some((reason, raw)) => {
            lines.push(Line::from(Span::styled(
                format!(" why it could not be used: {reason}"),
                theme.style(element::NOTICE_WARN),
            )));
            lines.push(Line::default());
            for line in raw.lines().take(400) {
                lines.push(Line::from(Span::styled(
                    format!(" {}", crate::tui::text::truncate(line, 120)),
                    theme.style(element::FG),
                )));
            }
        }
        None => lines.push(Line::from(Span::styled(
            " the last answer was usable".to_owned(),
            theme.style(element::MUTED),
        ))),
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " Esc closes · <leader>a asks again".to_owned(),
        theme.style(element::MUTED),
    )));

    let height = height_for(lines.len()).saturating_add(2).min(area.height);
    let popup = layout::centered(area, area.width.saturating_sub(4), height);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_style(theme.style(element::BORDER_FOCUSED))
                    .title(" the model's answer "),
            )
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// Wraps a paragraph to a width, because a popup is not a text editor.
fn wrap_paragraph(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            word.clone_into(&mut current);
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            word.clone_into(&mut current);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// The style a severity is shown in.
trait SeverityStyle {
    fn style(severity: Severity, theme: &Theme) -> Style;
}

impl SeverityStyle for Severity {
    fn style(severity: Severity, theme: &Theme) -> Style {
        match severity {
            Severity::High => theme.style(element::NOTICE_ERROR),
            Severity::Medium => theme.style(element::NOTICE_WARN),
            Severity::Low => theme.style(element::MUTED),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paragraph_is_wrapped_at_the_width_it_was_given() {
        let text = "one two three four five six seven eight nine ten";
        let lines = wrap_paragraph(text, 20);
        assert!(lines.iter().all(|line| line.len() <= 20), "{lines:?}");
        // Nothing is lost in the wrapping.
        assert_eq!(
            lines.join(" ").split_whitespace().count(),
            text.split_whitespace().count()
        );
    }

    #[test]
    fn an_empty_paragraph_is_one_empty_line_rather_than_none() {
        assert_eq!(wrap_paragraph("   ", 20), [String::new()]);
    }

    #[test]
    fn a_single_long_word_is_not_split() {
        let word = "x".repeat(50);
        assert_eq!(wrap_paragraph(&word, 20), [word]);
    }
}
