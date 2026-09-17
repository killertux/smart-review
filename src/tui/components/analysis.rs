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
    // The key hints come first, so "how do I leave" is never the thing cut off. The
    // body is longer than most terminals, so the panel scrolls (j/k, g/G, d/u).
    let mut lines = vec![
        Line::from(Span::styled(
            " a reopens this · o switches order · j/k scroll · `:plan` and `:context` show the rest · Esc closes"
                .to_owned(),
            theme.style(element::MUTED),
        )),
        Line::default(),
    ];
    if app.analysis_repaired() {
        // Stated here rather than only in a notice, because a notice is gone by the
        // time the reader wonders why the plan looks like a second attempt.
        lines.push(Line::from(Span::styled(
            " This analysis needed a second attempt: the first answer could not be used."
                .to_owned(),
            theme.style(element::NOTICE_WARN),
        )));
        lines.push(Line::default());
    }
    lines.extend(panel_lines(app, theme));

    let height = height_for(lines.len()).saturating_add(2).min(area.height);
    let popup = layout::centered(area, area.width.saturating_sub(4), height);

    // Two rows are the border; the rest is scrollable content.
    let visible = usize::from(popup.height.saturating_sub(2));
    let offset = app.panel.scroll.min(lines.len().saturating_sub(visible));
    let title = if lines.len() > visible {
        format!(
            "{} [{}/{}]",
            panel_title(app).trim_end(),
            (offset + visible).min(lines.len()),
            lines.len()
        )
    } else {
        panel_title(app)
    };

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_style(theme.style(element::BORDER_FOCUSED))
                    .title(title),
            )
            .style(theme.style(element::BG))
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0)),
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
    match app.analysis_state() {
        // While a run is in flight the panel shows what arrived, because the answer
        // streaming in is the progress indicator that costs nothing (FR-4.4).
        AnalysisState::Gathering
        | AnalysisState::Queued
        | AnalysisState::Running { .. }
        | AnalysisState::Streaming { .. }
        | AnalysisState::Cancelling => running_lines(app, theme),
        // The one thing an analysis must never do is spend money without being asked
        // (FR-4.6), so this state gets a panel of its own rather than a line that
        // scrolls away.
        AnalysisState::Confirming => confirming_lines(app, theme),
        AnalysisState::Unusable { reason } => unusable_lines(theme, reason),
        AnalysisState::Failed { reason } => failed_lines(theme, reason),
        AnalysisState::Cancelled => cancelled_lines(theme),
        AnalysisState::Ready | AnalysisState::Idle => answer_lines(app, theme),
    }
}

/// What a run in flight shows: the stage, and the text as it arrives (FR-4.4).
fn running_lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
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
        ]),
        Line::default(),
    ];
    let text = app.analysis_stream();
    if text.is_empty() {
        lines.push(Line::from(Span::styled(
            " waiting for the first token…".to_owned(),
            theme.style(element::MUTED),
        )));
        return lines;
    }

    // The JSON is still incomplete, so parse whatever has arrived whole and show it
    // in the same shape the completed analysis will have (FR-4.4).
    let preview = crate::domain::analysis::preview(text);
    if preview.is_empty() {
        // Nothing usable yet (prose before the object, or a token cut where a partial
        // parse cannot read it). Show the raw tail, as before.
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
                " … the preview is bounded; the full text is used once it is complete".to_owned(),
                theme.style(element::MUTED),
            )));
        }
        return lines;
    }

    if !preview.summary.is_empty() {
        lines.extend(section(theme, "Summary", &preview.summary));
    }
    if !preview.intent.is_empty() {
        lines.push(Line::default());
        lines.extend(section(theme, "Why", &preview.intent));
    }
    if !preview.risks.is_empty() {
        let risks: Vec<crate::application::analysis::PanelRisk> = preview
            .risks
            .iter()
            .map(|risk| crate::application::analysis::PanelRisk {
                severity: risk.severity,
                title: risk.title.clone(),
                why: risk.why.clone(),
                files: Vec::new(),
                supported: true,
            })
            .collect();
        lines.extend(risk_lines(theme, &risks));
    }
    if !preview.questions.is_empty() {
        lines.extend(question_lines(theme, &preview.questions));
    }
    if !preview.plan.is_empty() {
        lines.extend(preview_plan_lines(theme, &preview.plan));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " … the answer is still streaming; what is shown is what has arrived so far".to_owned(),
        theme.style(element::MUTED),
    )));
    lines
}

/// The review order as it streams in, before path normalization (FR-4.4).
fn preview_plan_lines(
    theme: &Theme,
    plan: &[crate::domain::analysis::PreviewPlan],
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default(), heading(theme, "Review order")];
    for (index, group) in plan.iter().enumerate() {
        lines.push(Line::from(Span::styled(
            format!("  {}. {}", index + 1, group.group),
            theme.style(element::ACCENT),
        )));
        for line in wrap_paragraph(&group.rationale, 88) {
            lines.push(Line::from(Span::styled(
                format!("      {line}"),
                theme.style(element::MUTED),
            )));
        }
    }
    lines
}

/// What the user is asked before the first send of a repository (FR-4.6).
fn confirming_lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            " Nothing has been sent yet.".to_owned(),
            theme.style(element::NOTICE_WARN),
        )),
        Line::default(),
    ];
    match app.context_bundle() {
        Some(bundle) => {
            lines.push(Line::from(Span::styled(
                format!(
                    " This would send {} to {}.",
                    bundle.summary(),
                    app.model_label()
                ),
                theme.style(element::FG),
            )));
            for segment in bundle.segments.iter().take(12) {
                lines.push(Line::from(vec![
                    Span::styled(
                        if segment.included { "  ✓ " } else { "  ✗ " },
                        if segment.included {
                            theme.style(element::FG)
                        } else {
                            theme.style(element::MUTED)
                        },
                    ),
                    Span::styled(
                        crate::tui::text::truncate(&segment.label, 60),
                        theme.style(element::FG),
                    ),
                ]));
            }
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                " `:context` lists everything, included and not.".to_owned(),
                theme.style(element::MUTED),
            )));
        }
        None => lines.push(Line::from(Span::styled(
            " The context is being gathered…".to_owned(),
            theme.style(element::MUTED),
        ))),
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " <leader>a again sends it · Esc closes this and sends nothing".to_owned(),
        theme.style(element::ACCENT),
    )));
    lines
}

/// An answer that could not be used (FR-4.1).
fn unusable_lines(theme: &Theme, reason: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            format!(" the answer could not be used: {reason}"),
            theme.style(element::NOTICE_WARN),
        )),
        Line::default(),
        Line::from(Span::styled(
            " `:analyze raw` shows what the model actually wrote.".to_owned(),
            theme.style(element::MUTED),
        )),
    ]
}

/// A request that never produced an answer (FR-9.1).
///
/// The reason is the provider's or the transport's own words, because that is the only
/// thing that distinguishes "the key is wrong" from "the model name is wrong" from
/// "this provider cannot do it".
fn failed_lines(theme: &Theme, reason: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            " the provider did not answer".to_owned(),
            theme.style(element::NOTICE_ERROR),
        )),
        Line::default(),
        Line::from(Span::styled(format!(" {reason}"), theme.style(element::FG))),
        Line::default(),
        Line::from(Span::styled(
            " <leader>a tries again · `:model` shows what is configured · `:doctor` checks\
             \nthe key and the network"
                .to_owned(),
            theme.style(element::MUTED),
        )),
    ]
}

/// A run the user stopped (FR-4.4).
fn cancelled_lines(theme: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            " the analysis was cancelled".to_owned(),
            theme.style(element::MUTED),
        )),
        Line::from(Span::styled(
            " the tree keeps the order it had; <leader>a starts again".to_owned(),
            theme.style(element::MUTED),
        )),
    ]
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
    // Corrections before the plan: what was dropped changes how the rest is read, and
    // the plan is also in the tree beside this panel (FR-4.2).
    lines.extend(warning_lines(app, theme));
    lines.extend(question_lines(theme, &panel.questions));
    lines.extend(plan_lines(app, theme));
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
fn risk_lines(
    theme: &Theme,
    risks: &[crate::application::analysis::PanelRisk],
) -> Vec<Line<'static>> {
    if risks.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![Line::default(), heading(theme, "Risks")];
    for risk in risks {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<6} ", risk.severity.label()),
                Severity::style(risk.severity, theme),
            ),
            Span::styled(
                if risk.supported {
                    if risk.files.is_empty() {
                        risk.title.clone()
                    } else {
                        format!("{} [{}]", risk.title, risk.files.join(", "))
                    }
                } else {
                    format!(
                        "{} (unsupported: cited files are not in this change)",
                        risk.title
                    )
                },
                if risk.supported {
                    theme.style(element::FG)
                } else {
                    theme.style(element::NOTICE_WARN)
                },
            ),
        ]));
        for line in wrap_paragraph(&risk.why, 88) {
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
            lines.push(Line::from(Span::styled(
                " LLM input uses the canonical review diff; visible whitespace and context-line toggles do not change this inventory."
                    .to_owned(),
                theme.style(element::MUTED),
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
