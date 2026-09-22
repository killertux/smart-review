//! The review screen: tabs, file tree, and the diff (FR-3.3, FR-3.4).
//!
//! The diff pane is drawn from [`DiffView`](crate::tui::diff_view::DiffView)'s
//! flattened rows, and only the rows inside the viewport are turned into spans, so
//! the cost of a frame does not depend on the size of the patch (NFR-1.3).
//!
//! Below 140 columns the split view is not offered at all — the toggle says why
//! instead of drawing two unusable half-panes (DEC-4).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::domain::diff::{FileStatus, LineKind};
use crate::domain::draft::Side;
use crate::domain::pr::{CheckState, ReviewComment};
use crate::tui::app::{App, Pane, ReviewTab};
use crate::tui::components::border_style;
use crate::tui::diff_view::{DiffRow, DiffView, RowKind, SplitRow, TreeKind, TreeRow};
use crate::tui::syntax::{SyntaxClass, SyntaxSpan};
use crate::tui::text;
use crate::tui::theme::{Theme, element};

/// The width below which the split view is not offered (DEC-4).
pub const SPLIT_MIN_WIDTH: u16 = 140;

/// The width of the file tree when both panes are shown.
pub const TREE_WIDTH: u16 = 34;

/// Every rectangle that makes up a review frame (IR-09).
///
/// Rendering, focus, scrolling and hit-testing use this one model.  In particular, the
/// diff rectangle is the part left after both the chat and a visible comment composer,
/// rather than an approximation of the review body's height.
#[derive(Debug, Clone, Copy)]
pub struct ReviewLayout {
    /// The numbered tab row.
    pub tabs: Rect,
    /// Full-width content below the tabs, used by non-file destinations.
    pub content: Rect,
    /// The file tree.
    pub tree: Rect,
    /// Compact or expanded current-file guidance above the code.
    pub guidance: Option<Rect>,
    /// The visible diff rows.
    pub diff: Rect,
    /// The optional inline comment composer.
    pub composer: Option<Rect>,
    /// The optional chat pane.
    pub chat: Option<Rect>,
}

/// Calculates the review frame's rectangles once (IR-09).
#[must_use]
pub fn layout(area: Rect, chat_open: bool, composing: bool, guidance_height: u16) -> ReviewLayout {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).split(area);
    let (body, chat) = super::chat::chat_split(rows[1], chat_open);
    let columns =
        Layout::horizontal([Constraint::Length(TREE_WIDTH), Constraint::Min(20)]).split(body);
    let (file_body, composer) = super::drafts::composer_split(columns[1], composing);
    let (guidance, diff) = if guidance_height == 0 {
        (None, file_body)
    } else {
        let rows = Layout::vertical([
            Constraint::Length(guidance_height.min(file_body.height.saturating_sub(3))),
            Constraint::Min(3),
        ])
        .split(file_body);
        (Some(rows[0]), rows[1])
    };
    ReviewLayout {
        tabs: rows[0],
        content: rows[1],
        tree: columns[0],
        guidance,
        diff,
        composer,
        chat,
    }
}

/// Renders the review screen (FR-3.3).
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(view) = app.review.as_ref() else {
        return;
    };

    let layout = app.review_layout().unwrap_or_else(|| {
        layout(
            area,
            false,
            app.drafts().is_composing(),
            if app.current_file_guidance().is_some() {
                7
            } else {
                0
            },
        )
    });
    render_tabs(frame, layout.tabs, app);
    match app.review_tab() {
        ReviewTab::Overview => render_overview(frame, layout.content, app),
        ReviewTab::Files => {
            render_tree(frame, layout.tree, app, view);
            if let Some(guidance) = layout.guidance {
                render_file_guidance(frame, guidance, app);
            }
            render_diff(frame, layout.diff, app, view);
            if let Some(composer_area) = layout.composer
                && let Some(composer) = app.drafts().composer.as_ref()
            {
                super::drafts::render_composer(frame, composer_area, app, composer);
            }
        }
        ReviewTab::Checks => render_checks(frame, layout.content, app),
        ReviewTab::Discussion => {
            let (discussion, composer) =
                super::drafts::composer_split(layout.content, app.drafts().is_composing());
            render_discussion(frame, discussion, app);
            if let Some(composer) = composer
                && let Some(draft) = app.drafts().composer.as_ref()
            {
                super::drafts::render_composer(frame, composer, app, draft);
            }
        }
        ReviewTab::Ask => super::chat::render(frame, layout.content, app),
    }
}

/// Concise What / Why (inferred) / Verify guidance beside the current code (IR-16).
fn render_file_guidance(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(guidance) = app.current_file_guidance() else {
        return;
    };
    let theme = &app.theme;
    let width = usize::from(area.width.saturating_sub(4)).max(1);
    let lines = if app.panel.guidance_expanded {
        expanded_guidance_lines(&guidance, theme, width)
    } else {
        compact_guidance_lines(&guidance, theme, width)
    };
    let visible = usize::from(area.height.saturating_sub(2)).max(1);
    let scroll = app
        .panel
        .guidance_scroll
        .min(lines.len().saturating_sub(visible));
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::new().borders(Borders::ALL).title(format!(
                " guide · {} · e: {}{} ",
                guidance.path,
                if app.panel.guidance_expanded {
                    "collapse"
                } else {
                    "expand"
                },
                if app.panel.guidance_expanded {
                    " · j/k: scroll"
                } else {
                    ""
                },
            )))
            .style(theme.style(element::BG))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        area,
    );
}

fn compact_guidance_lines(
    guidance: &crate::application::analysis::FileGuidance,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let what = present_or(
        &guidance.what_changed,
        "not supplied for this file; open the full analysis for coverage details",
    );
    let why = present_or(
        &guidance.inferred_why,
        "not inferred from the available context",
    );
    let verify = if guidance.verify.is_empty() {
        "no concrete check supplied".to_owned()
    } else {
        guidance.verify.join(" · ")
    };
    [
        (" What ", what, 7),
        (" Why · inferred ", why, 17),
        (" Verify ", verify.as_str(), 9),
    ]
    .into_iter()
    .map(|(label, body, reserved)| {
        Line::from(vec![
            Span::styled(label, theme.style(element::TITLE)),
            Span::styled(
                text::truncate(body, width.saturating_sub(reserved)),
                theme.style(element::FG),
            ),
        ])
    })
    .collect()
}

fn expanded_guidance_lines(
    guidance: &crate::application::analysis::FileGuidance,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    push_guidance_text(
        &mut lines,
        theme,
        "What",
        present_or(
            &guidance.what_changed,
            "not supplied for this file; open the full analysis for coverage details",
        ),
        width,
    );
    push_guidance_text(
        &mut lines,
        theme,
        "Why · inferred",
        present_or(
            &guidance.inferred_why,
            "not inferred from the available context",
        ),
        width,
    );
    append_verification_lines(&mut lines, guidance, theme, width);
    if guidance.truncated {
        lines.push(Line::from(Span::styled(
            " Source was truncated; treat this guidance as partial.",
            theme.style(element::NOTICE_WARN),
        )));
    }
    append_evidence_lines(&mut lines, guidance, theme, width);
    lines
}

fn append_verification_lines(
    lines: &mut Vec<Line<'static>>,
    guidance: &crate::application::analysis::FileGuidance,
    theme: &Theme,
    width: usize,
) {
    if guidance.verify.is_empty() {
        push_guidance_text(lines, theme, "Verify", "no concrete check supplied", width);
        return;
    }
    for (index, check) in guidance.verify.iter().enumerate() {
        push_guidance_text(lines, theme, &format!("Verify {}", index + 1), check, width);
    }
}

fn append_evidence_lines(
    lines: &mut Vec<Line<'static>>,
    guidance: &crate::application::analysis::FileGuidance,
    theme: &Theme,
    width: usize,
) {
    if guidance.evidence.is_empty() {
        lines.push(Line::from(Span::styled(
            " Evidence: none supplied or no coordinate survived validation.",
            theme.style(element::MUTED),
        )));
        return;
    }
    for (index, evidence) in guidance.evidence.iter().enumerate() {
        let coordinate = match (evidence.side, evidence.line) {
            (Some(side), Some(line)) => format!("{}:{line}", side.label()),
            _ => "file".to_owned(),
        };
        let label = evidence
            .label
            .is_empty()
            .then(String::new)
            .unwrap_or_else(|| format!(" — {}", evidence.label));
        push_guidance_text(
            lines,
            theme,
            &format!("Evidence {}", index + 1),
            &format!(
                "{} ({coordinate}){label} · :evidence {}",
                evidence.path,
                index + 1
            ),
            width,
        );
    }
}

fn present_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
}

fn push_guidance_text(
    lines: &mut Vec<Line<'static>>,
    theme: &Theme,
    label: &str,
    body: &str,
    width: usize,
) {
    let prefix = format!(" {label} ");
    let prefix_width = text::width(&prefix);
    let body_width = width.saturating_sub(prefix_width).max(1);
    let wrapped = text::wrap(body, body_width);
    for (index, row) in wrapped.into_iter().enumerate() {
        if index == 0 {
            lines.push(Line::from(vec![
                Span::styled(prefix.clone(), theme.style(element::TITLE)),
                Span::styled(row, theme.style(element::FG)),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                format!("{}{row}", " ".repeat(prefix_width)),
                theme.style(element::FG),
            )));
        }
    }
}

/// The tab bar: which PR, and which view of it.
fn render_tabs(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let Some(detail) = app.detail.as_ref() else {
        return;
    };
    let compact = area.width < 120;
    let title_width = if compact { 15 } else { 60 };
    let mut spans = vec![
        Span::styled(
            format!(" #{} ", detail.summary.number),
            theme.style(element::TITLE),
        ),
        Span::styled(
            format!("{} ", text::truncate(&detail.summary.title, title_width)),
            theme.style(element::FG),
        ),
    ];
    for tab in ReviewTab::ALL {
        let label = tab_label(app, tab, !compact);
        spans.push(Span::styled(
            label,
            if tab == app.review_tab() {
                theme.style(element::SELECTION)
            } else {
                theme.style(element::FG)
            },
        ));
    }

    // Instead of a block, the row is painted edge to edge so it reads as a bar.
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(theme.style(element::BG)),
        area,
    );
}

/// Finds the tab label rendered under a pointer column (IR-10).
#[must_use]
pub fn tab_at(area: Rect, app: &App, column: u16) -> Option<ReviewTab> {
    let detail = app.detail.as_ref()?;
    let compact = area.width < 120;
    let title_width = if compact { 15 } else { 60 };
    let prefix = format!(
        " #{} {} ",
        detail.summary.number,
        text::truncate(&detail.summary.title, title_width)
    );
    let mut x = area
        .x
        .saturating_add(u16::try_from(text::width(&prefix)).unwrap_or(u16::MAX));
    for tab in ReviewTab::ALL {
        let label = tab_label(app, tab, !compact);
        let end = x.saturating_add(u16::try_from(text::width(&label)).unwrap_or(u16::MAX));
        if (x..end).contains(&column) {
            return Some(tab);
        }
        x = end;
    }
    None
}

/// The rendered label and click extent for a tab.
fn tab_label(app: &App, tab: ReviewTab, show_count: bool) -> String {
    match (show_count, tab_count(app, tab)) {
        (true, Some(count)) => format!("[{} {} ({count})] ", tab.number(), tab.label()),
        _ => format!("[{} {}] ", tab.number(), tab.label()),
    }
}

/// Count shown beside each tab; each number labels the data available at that destination.
fn tab_count(app: &App, tab: ReviewTab) -> Option<usize> {
    let detail = app.detail.as_ref()?;
    match tab {
        ReviewTab::Overview => None,
        ReviewTab::Files => {
            Some(usize::try_from(detail.summary.changed_files).unwrap_or(usize::MAX))
        }
        ReviewTab::Checks => Some(detail.checks.len()),
        ReviewTab::Discussion => {
            Some(detail.reviews.len() + detail.comments.len() + detail.conversation.len())
        }
        ReviewTab::Ask => app
            .chat_state()
            .and_then(|chat| chat.session.as_ref())
            .map(crate::domain::chat::Session::turns),
    }
}

/// Renders the author-supplied pull-request brief separately from analysis (IR-10).
fn render_overview(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(detail) = app.detail.as_ref() else {
        return;
    };
    let theme = &app.theme;
    let width = usize::from(area.width.saturating_sub(2));
    let mut lines = overview_header_lines(detail, theme, width);
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " Guided review",
        theme.style(element::TITLE),
    )));
    if let Some(analysis) = app.analysis_panel() {
        append_analysis_overview(&mut lines, app, detail, &analysis, theme, width);
    } else {
        append_missing_analysis(&mut lines, app, theme);
    }
    let scroll = app.tab_scroll.min(lines.len().saturating_sub(1));
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::new().borders(Borders::ALL).title(" overview "))
            .style(theme.style(element::BG))
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
}

fn overview_header_lines(
    detail: &crate::domain::pr::PullRequestDetail,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            format!(" {} ", detail.summary.title),
            theme.style(element::TITLE),
        )),
        Line::from(format!(
            " author: {} · {} → {} · {}",
            detail.summary.author,
            detail.summary.head_ref,
            detail.summary.base_ref,
            detail.summary.state.label()
        )),
        Line::from(format!(
            " {} commit(s) · {} changed file(s) · {} check(s) · {} discussion item(s)",
            detail.commits.len(),
            detail.summary.changed_files,
            detail.checks.len(),
            detail.reviews.len() + detail.comments.len() + detail.conversation.len(),
        )),
        Line::from(format!(
            " review decision: {} · checks: {}",
            detail
                .summary
                .review_decision
                .map_or("not reported", crate::domain::pr::ReviewDecision::label),
            detail.summary.checks.label(),
        )),
        Line::default(),
        Line::from(Span::styled(
            " Author description",
            theme.style(element::TITLE),
        )),
    ];
    if detail.body.trim().is_empty() {
        lines.push(Line::from(Span::styled(
            " No description was provided.",
            theme.style(element::MUTED),
        )));
    } else {
        lines.extend(crate::tui::markdown::render(&detail.body, width, theme));
    }
    lines
}

fn append_analysis_overview(
    lines: &mut Vec<Line<'static>>,
    app: &App,
    detail: &crate::domain::pr::PullRequestDetail,
    analysis: &crate::application::analysis::PanelModel,
    theme: &Theme,
    width: usize,
) {
    lines.push(Line::from(Span::styled(
        " Review brief · AI",
        theme.style(element::ACCENT),
    )));
    lines.extend(crate::tui::markdown::render(
        &analysis.summary,
        width,
        theme,
    ));
    lines.push(Line::from(Span::styled(
        " Purpose · inferred",
        theme.style(element::ACCENT),
    )));
    lines.extend(crate::tui::markdown::render(&analysis.intent, width, theme));
    append_reading_plan(lines, analysis, theme);
    append_coverage(lines, detail, analysis, theme);
    append_suggested_questions(lines, analysis, theme);
    if let Some(provenance) = app.analysis_provenance() {
        lines.push(Line::from(Span::styled(
            format!(
                " {provenance} · {} · {}",
                app.analysis_state().label(),
                if app.analysis_is_stale() {
                    "stale"
                } else {
                    "current"
                }
            ),
            theme.style(element::MUTED),
        )));
    }
}

fn append_reading_plan(
    lines: &mut Vec<Line<'static>>,
    analysis: &crate::application::analysis::PanelModel,
    theme: &Theme,
) {
    lines.push(Line::from(Span::styled(
        " Reading plan",
        theme.style(element::ACCENT),
    )));
    for step in &analysis.steps {
        lines.push(Line::from(format!(
            " {}. {} — {}",
            step.order, step.name, step.rationale
        )));
        lines.push(Line::from(Span::styled(
            format!("    {}", step.files.join(" · ")),
            theme.style(element::MUTED),
        )));
    }
}

fn append_coverage(
    lines: &mut Vec<Line<'static>>,
    detail: &crate::domain::pr::PullRequestDetail,
    analysis: &crate::application::analysis::PanelModel,
    theme: &Theme,
) {
    let changed = usize::try_from(detail.summary.changed_files).unwrap_or(usize::MAX);
    lines.push(Line::from(Span::styled(
        format!(
            " Coverage: {} / {changed} file(s) explicitly analyzed · {} truncated",
            analysis.analyzed_files.len(),
            analysis.truncated_files.len()
        ),
        if analysis.truncated_files.is_empty() {
            theme.style(element::MUTED)
        } else {
            theme.style(element::NOTICE_WARN)
        },
    )));
    for limitation in &analysis.limitations {
        lines.push(Line::from(Span::styled(
            format!("  limitation: {limitation}"),
            theme.style(element::NOTICE_WARN),
        )));
    }
}

fn append_suggested_questions(
    lines: &mut Vec<Line<'static>>,
    analysis: &crate::application::analysis::PanelModel,
    theme: &Theme,
) {
    if analysis.questions.is_empty() {
        return;
    }
    lines.push(Line::from(Span::styled(
        " Suggested questions (populate Ask; never auto-send)",
        theme.style(element::ACCENT),
    )));
    for (index, question) in analysis.questions.iter().enumerate() {
        lines.push(Line::from(format!(
            "  {}. {} · :chat suggested {}",
            index + 1,
            question,
            index + 1
        )));
    }
}

fn append_missing_analysis(lines: &mut Vec<Line<'static>>, app: &App, theme: &Theme) {
    let message = if app.analysis_state().is_running() {
        " Analysis is running; Files remains usable and Esc cancels."
    } else if app.active_model.is_some() {
        " No analysis yet; press <leader>a to generate guided review."
    } else {
        " Select a model with <leader>m; Files, Checks, Discussion and drafts remain available."
    };
    lines.push(Line::from(Span::styled(
        message,
        theme.style(element::MUTED),
    )));
}

/// Renders the forge's individual check records, including a truthful empty state.
fn render_checks(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let Some(detail) = app.detail.as_ref() else {
        return;
    };
    let mut lines = Vec::new();
    if app.detail_offline.is_some() {
        lines.push(Line::from(Span::styled(
            " Cached check data; refresh was unavailable. Press R to retry.",
            theme.style(element::STATUS_ERROR),
        )));
    }
    if detail.checks.is_empty() {
        lines.push(Line::from(Span::styled(
            " No checks are configured or GitHub did not report any.",
            theme.style(element::MUTED),
        )));
    } else {
        for (index, check) in detail.checks.iter().enumerate() {
            let marker = match check.state {
                CheckState::Success => "✓",
                CheckState::Failure => "✗",
                CheckState::Pending => "…",
                CheckState::Neutral => "•",
                CheckState::Skipped => "−",
                CheckState::Unknown => "?",
            };
            let state = match check.lifecycle {
                crate::domain::pr::CheckLifecycle::Queued => "queued",
                crate::domain::pr::CheckLifecycle::Running => "running",
                crate::domain::pr::CheckLifecycle::Completed
                | crate::domain::pr::CheckLifecycle::Unknown => match check.state {
                    CheckState::Success => "success",
                    CheckState::Failure => "failed",
                    CheckState::Pending => "pending",
                    CheckState::Neutral => "neutral",
                    CheckState::Skipped => "skipped",
                    CheckState::Unknown => "unknown",
                },
            };
            lines.push(Line::from(Span::styled(
                format!(" {marker} {} — {state}", check.name),
                if index == app.check_cursor {
                    theme.style(element::SELECTION)
                } else if check.state.is_failure() {
                    theme.style(element::STATUS_ERROR)
                } else {
                    theme.style(element::FG)
                },
            )));
            if let Some(description) = &check.description {
                lines.push(Line::from(Span::styled(
                    format!("     {description}"),
                    theme.style(element::MUTED),
                )));
            }
            if let Some(conclusion) = &check.conclusion {
                lines.push(Line::from(Span::styled(
                    format!("     conclusion: {conclusion}"),
                    theme.style(element::MUTED),
                )));
            }
            if let Some(url) = &check.url {
                lines.push(Line::from(Span::styled(
                    format!("     {url} · Enter opens this run in the browser."),
                    theme.style(element::MUTED),
                )));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::new().borders(Borders::ALL).title(" checks "))
            .style(theme.style(element::BG))
            .scroll((u16::try_from(app.check_scroll).unwrap_or(u16::MAX), 0)),
        area,
    );
}

/// Renders every remote discussion record, including comments with no current diff row.
fn render_discussion(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let theme = &app.theme;
    let Some(detail) = app.detail.as_ref() else {
        return;
    };
    let mut lines = Vec::new();
    if app.detail_offline.is_some() {
        lines.push(Line::from(Span::styled(
            " Cached discussion data; refresh was unavailable. Press R to retry.",
            theme.style(element::STATUS_ERROR),
        )));
    }
    if app.discussion.filter == crate::tui::discussion::DiscussionFilter::All {
        for review in &detail.reviews {
            lines.push(Line::from(Span::styled(
                format!(" review · {} · {}", review.author, review.state.label()),
                theme.style(element::TITLE),
            )));
            if review.body.trim().is_empty() {
                lines.push(Line::from(Span::styled(
                    "   (no review body)",
                    theme.style(element::MUTED),
                )));
            } else {
                lines.extend(crate::tui::markdown::render(
                    &review.body,
                    usize::from(area.width.saturating_sub(4)),
                    theme,
                ));
            }
        }
    }
    for comment in root_comments(&detail.comments).filter(|comment| matches_filter(app, comment)) {
        let state = if comment.outdated {
            "outdated"
        } else if comment.resolved {
            "resolved"
        } else {
            "open"
        };
        lines.push(Line::from(Span::styled(
            format!(" thread · {state} · {}", comment.path),
            if app.discussion.selected_root == Some(comment.id) {
                theme.style(element::SELECTION)
            } else {
                theme.style(element::TITLE)
            },
        )));
        append_thread(
            &mut lines,
            &detail.comments,
            comment,
            theme,
            1,
            usize::from(area.width.saturating_sub(6)),
        );
    }
    if app.discussion.filter == crate::tui::discussion::DiscussionFilter::All {
        for comment in &detail.conversation {
            lines.push(Line::from(Span::styled(
                format!(" conversation · {}", comment.author),
                theme.style(element::TITLE),
            )));
            lines.extend(crate::tui::markdown::render(
                &comment.body,
                usize::from(area.width.saturating_sub(4)),
                theme,
            ));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " No discussion items match this filter.",
            theme.style(element::MUTED),
        )));
    }
    let scroll = app.tab_scroll.min(lines.len().saturating_sub(1));
    let visible_threads = root_comments(&detail.comments)
        .filter(|comment| matches_filter(app, comment))
        .count();
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::new().borders(Borders::ALL).title(format!(
                " discussion · {} ({visible_threads}) · f: cycle all/open/resolved/outdated · Enter: jump ",
                app.discussion.filter.label(),
            )))
            .style(theme.style(element::BG))
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
}

fn matches_filter(app: &App, comment: &ReviewComment) -> bool {
    match app.discussion.filter {
        crate::tui::discussion::DiscussionFilter::All => true,
        crate::tui::discussion::DiscussionFilter::Open => !comment.resolved && !comment.outdated,
        crate::tui::discussion::DiscussionFilter::Resolved => comment.resolved,
        crate::tui::discussion::DiscussionFilter::Outdated => comment.outdated,
    }
}

fn root_comments(comments: &[ReviewComment]) -> impl Iterator<Item = &ReviewComment> {
    comments.iter().filter(|comment| {
        comment.in_reply_to.is_none()
            || !comments
                .iter()
                .any(|candidate| Some(candidate.id) == comment.in_reply_to)
    })
}

fn append_thread(
    lines: &mut Vec<Line<'static>>,
    comments: &[ReviewComment],
    comment: &ReviewComment,
    theme: &Theme,
    depth: usize,
    width: usize,
) {
    let indent = "  ".repeat(depth);
    lines.push(Line::from(Span::styled(
        format!(" {indent}{}:", comment.author),
        theme.style(element::MUTED),
    )));
    lines.extend(crate::tui::markdown::render(
        &comment.body,
        width.max(1),
        theme,
    ));
    for reply in comments
        .iter()
        .filter(|reply| reply.in_reply_to == Some(comment.id))
    {
        append_thread(
            lines,
            comments,
            reply,
            theme,
            depth.saturating_add(1),
            width,
        );
    }
}

/// The file tree with per-file stats and folder grouping (FR-3.3).
fn render_tree(frame: &mut Frame<'_>, area: Rect, app: &App, view: &DiffView) {
    let theme = &app.theme;
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style(app, Pane::Diff))
        .title(format!(" {} ", tree_title(view)));

    let height = usize::from(area.height.saturating_sub(2));
    // The offset is kept in the view by `prepare`, which the frame calls with the
    // real pane heights before anything is drawn.
    let scroll = view.tree_scroll;
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);

    for (index, row) in view.tree.iter().enumerate().skip(scroll).take(height) {
        let selected = view.tree_focused && index == view.tree_cursor;
        let current =
            matches!(row.kind, TreeKind::File { index } if Some(index) == view.current_file());
        lines.push(tree_line(theme, view, row, selected, current));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG)),
        area,
    );
}

/// The tree pane's title: the order in force (FR-3.5).
///
/// Only the order: the two positions are in the status line, which is wide enough to
/// show them without cutting them off, and a title that says half of something is
/// worse than a title that says one thing.
fn tree_title(view: &DiffView) -> String {
    let provenance = view
        .order_provenance()
        .map_or_else(String::new, |source| format!(" · {source}"));
    let progress = view
        .review_progress()
        .map_or_else(String::new, |(done, revisit, total)| {
            if revisit == 0 {
                format!(" · {done}/{total} reviewed")
            } else {
                format!(" · {done}/{total} reviewed · {revisit} revisit")
            }
        });
    format!(
        "{}{} ({}){}",
        view.order.label(),
        provenance,
        view.stats().files,
        progress,
    )
}

/// One tree row.
fn tree_line(
    theme: &Theme,
    view: &DiffView,
    row: &TreeRow,
    selected: bool,
    current_file: bool,
) -> Line<'static> {
    let base = if selected {
        theme.style(element::SELECTION)
    } else {
        theme.style(element::FG)
    };
    let tint = |style: ratatui::style::Style| if selected { base.patch(style) } else { style };

    let indent = "  ".repeat(usize::from(row.depth));
    match &row.kind {
        // A plan group is a heading, not a directory: it carries the position it is
        // read in, which is the whole point of the ordered view (FR-4.2).
        TreeKind::Group {
            order,
            files,
            folded,
            ..
        } => {
            let marker = if *folded { "▸" } else { "▾" };
            Line::from(vec![
                Span::styled(
                    format!(" {marker} {order}. "),
                    tint(theme.style(element::TREE_DIR)),
                ),
                Span::styled(
                    text::truncate(&format!("{} ({files})", row.label), 24),
                    tint(theme.style(element::ACCENT)),
                ),
            ])
        }
        // The rationale is why this group is read here (FR-4.2). It is dimmed and
        // indented under its heading so the list still scans as a list.
        TreeKind::Rationale { .. } => Line::from(vec![
            Span::styled("    ".to_owned(), base),
            Span::styled(
                text::truncate(&row.label, 24),
                tint(theme.style(element::MUTED)),
            ),
        ]),
        TreeKind::Directory { files, folded, .. } => {
            let marker = if *folded { "▸" } else { "▾" };
            Line::from(vec![
                Span::styled(
                    format!(" {indent}{marker} "),
                    tint(theme.style(element::TREE_DIR)),
                ),
                Span::styled(
                    text::truncate(&format!("{} ({files})", row.label), 20),
                    tint(theme.style(element::TREE_DIR)),
                ),
            ])
        }
        TreeKind::File { index } => {
            let file = view.patch.files.get(*index);
            let (marker, style) = match file.map(|file| (file.status, file.kind())) {
                Some((status, kind)) => (
                    marker_for(status, kind),
                    match status {
                        FileStatus::Added => theme.style(element::TREE_ADDED),
                        FileStatus::Deleted => theme.style(element::TREE_DELETED),
                        _ => theme.style(element::TREE_MODIFIED),
                    },
                ),
                None => ("?", theme.style(element::MUTED)),
            };
            let stats = file.map_or_else(String::new, crate::domain::diff::FileDiff::size_label);
            let cursor = if current_file { "·" } else { " " };
            // A file whose hunks are all folded says so, so the tree and the diff
            // pane cannot disagree about what is hidden (FR-3.3).
            let folded = if view.file_is_folded(*index) {
                "▸"
            } else {
                ""
            };
            let progress = file
                .and_then(crate::domain::diff::FileDiff::path)
                .map_or("□", |path| view.review_status(path.as_str()).marker());
            Line::from(vec![
                Span::styled(
                    format!(" {indent}{cursor}{progress}{marker}{folded}"),
                    tint(style),
                ),
                Span::styled(" ".to_owned(), base),
                Span::styled(
                    text::pad(&row.label, 17),
                    if current_file {
                        tint(theme.style(element::ACCENT))
                    } else {
                        base
                    },
                ),
                Span::styled(text::truncate(&stats, 9), tint(theme.style(element::MUTED))),
            ])
        }
    }
}

/// The letter a tree row shows for a file.
fn marker_for(status: FileStatus, kind: crate::domain::diff::FileKind) -> &'static str {
    crate::tui::diff_view::file_marker(status, kind)
}

/// The diff pane, unified or split (FR-3.3, DEC-4).
fn render_diff(frame: &mut Frame<'_>, area: Rect, app: &App, view: &DiffView) {
    let theme = &app.theme;
    // The threshold is a *terminal* width, not a pane width (DEC-4): 140 columns of
    // terminal has room for a tree and two usable halves, and the toggle uses the
    // same number, so the two can never disagree.
    let split = view.split && app.terminal_width() >= SPLIT_MIN_WIDTH;

    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style(app, Pane::Diff))
        .title(diff_title(theme, view, area.width, app));

    let height = area.height.saturating_sub(2);
    let mut lines = Vec::with_capacity(usize::from(height));

    let width = area.width.saturating_sub(2);
    if view.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "This pull request has no changes to show.".to_owned(),
            theme.style(element::MUTED),
        )));
    } else if split {
        // The split view walks the paired rows, so a replacement occupies one drawn
        // row instead of two.
        let window = view.split_window(height);
        for row in window {
            lines.push(split_line(theme, view, row, width, app.drafts()));
        }
    } else {
        for index in view.visible_rows(height) {
            let selected = index == view.cursor;
            lines.push(unified_line(
                theme,
                &view.rows[index],
                selected,
                width,
                view,
                app.drafts(),
            ));
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(theme.style(element::BG)),
        area,
    );
}

/// The diff pane's title: what is being looked at, and the mode.
fn diff_title(theme: &Theme, view: &DiffView, width: u16, app: &App) -> String {
    let _ = width;
    let stats = view.stats();
    let mode = if view.split && app.terminal_width() >= SPLIT_MIN_WIDTH {
        "split"
    } else if view.split {
        // Asked for split, too narrow: say so rather than silently drawing unified.
        "unified (split needs 140 columns)"
    } else {
        "unified"
    };
    // Only the context size in force is named: in remote mode `gh pr diff` always
    // sends three lines, so printing a configured 10 would be a claim about the pane
    // that is not true (FR-3.2).
    let title = format!(" {} · {} · ctx {} ", stats.label(), mode, view.context);
    let mut title = title;
    if app.diff_loading {
        title.push_str("· loading… ");
    }
    if app.diff_offline.is_some() {
        title.push_str("· cached ");
    }
    if view.syntax_limited() {
        title.push_str("· syntax partial ");
    }
    let _ = theme;
    title
}

/// One row of the unified view: gutters, marker, line.
///
/// The draft marker is a property of the *line*, so it is decided here, where the line
/// number and the file are both in hand, rather than by a second pass over the patch
/// (FR-6.1).
fn unified_line(
    theme: &Theme,
    row: &DiffRow,
    selected: bool,
    width: u16,
    view: &DiffView,
    drafts: &crate::tui::drafts::DraftState,
) -> Line<'static> {
    let gutter = gutter_style(theme, row, selected);
    let mut spans = vec![Span::styled(
        format!(
            "{}{}{}{}",
            text::pad_left(
                &row.old_line
                    .map_or_else(String::new, |line| line.to_string()),
                5
            ),
            text::pad_left(
                &row.new_line
                    .map_or_else(String::new, |line| line.to_string()),
                5
            ),
            row.line_kind.map_or(' ', LineKind::marker),
            draft_marker(view, drafts, row),
        ),
        if draft_marker(view, drafts, row) == '●' {
            theme.style(element::COMMENT_MARKER)
        } else {
            gutter
        },
    )];

    match row.kind {
        RowKind::FileHeader => {
            spans.clear();
            spans.push(Span::styled(
                format!("── {} ", row.text),
                if selected {
                    theme.style(element::SELECTION)
                } else {
                    theme.style(element::TITLE)
                },
            ));
        }
        RowKind::HunkHeader => {
            spans.clear();
            spans.push(Span::styled(
                format!("{} ", text::truncate(&row.text, usize::from(width))),
                body_style(theme, row, selected),
            ));
        }
        // A placeholder, a folded hunk and a line of the discussion about the line
        // above are all "some text, no line numbers": the only difference is the style
        // `body_style` gives them, and the discussion is indented by its own text.
        RowKind::Placeholder | RowKind::Folded | RowKind::Discussion => {
            spans.clear();
            spans.push(Span::styled(
                format!("   {} ", text::truncate(&row.text, usize::from(width))),
                body_style(theme, row, selected),
            ));
        }
        RowKind::Line => {
            let available = usize::from(width).saturating_sub(12);
            let body = body_style(theme, row, selected);
            let (side, number) = match row.line_kind {
                Some(LineKind::Delete) => (Side::Old, row.old_line),
                Some(LineKind::Add | LineKind::Context) | None => (Side::New, row.new_line),
            };
            let syntax = number.map_or(&[][..], |number| view.syntax_spans(row.file, side, number));
            let content_width = if row.no_newline {
                available.saturating_sub(2)
            } else {
                available
            };
            spans.extend(highlighted_content(
                theme,
                &row.text,
                syntax,
                content_width,
                body,
                false,
            ));
            if row.no_newline {
                spans.push(Span::styled(" ⏎".to_owned(), body));
            }
        }
    }

    Line::from(spans)
}

/// One row of the split view: the old side beside the new side.
///
/// A full-width row (a banner, a hunk header, a placeholder) is drawn the same way
/// in both modes, so the eye has something to anchor on while scrolling sideways.
fn split_line(
    theme: &Theme,
    view: &DiffView,
    row: &SplitRow,
    width: u16,
    drafts: &crate::tui::drafts::DraftState,
) -> Line<'static> {
    if let Some(full) = &row.full {
        let selected = full_is_selected(view, full);
        return unified_line(theme, full, selected, width, view, drafts);
    }

    let left_width = usize::from(width.saturating_sub(1)) / 2;
    let right_width = usize::from(width).saturating_sub(left_width + 1);
    let selected = row.contains(view.selected_unified());
    let mut spans = Vec::with_capacity(6);

    for (left_side, line, cell_width) in [
        (true, row.left.as_ref(), left_width),
        (false, row.right.as_ref(), right_width),
    ] {
        if !left_side {
            spans.push(Span::styled("│".to_owned(), theme.style(element::BORDER)));
        }
        match line {
            Some(line) => {
                // The old side is numbered on the left and the new side on the
                // right; using one number for both would mislabel half the rows.
                let number = if left_side {
                    line.old_line
                } else {
                    line.new_line
                };
                let gutter = format!(
                    "{} ",
                    text::pad_left(
                        &number.map_or_else(String::new, |number| number.to_string()),
                        5
                    )
                );
                spans.push(Span::styled(
                    gutter,
                    if selected {
                        theme.style(element::SELECTION)
                    } else {
                        theme.style(element::DIFF_LINE_NUMBER)
                    },
                ));
                let side = if left_side { Side::Old } else { Side::New };
                let syntax =
                    number.map_or(&[][..], |number| view.syntax_spans(row.file, side, number));
                spans.extend(highlighted_content(
                    theme,
                    &line.content,
                    syntax,
                    cell_width.saturating_sub(6),
                    body_style_for(theme, line.kind, selected),
                    true,
                ));
            }
            None => {
                // An empty cell, which is how a one-sided change reads beside its
                // counterpart.
                spans.push(Span::styled(
                    " ".repeat(cell_width),
                    theme.style(element::BG),
                ));
            }
        }
    }

    Line::from(spans)
}

/// The character that says a line already has a staged comment (FR-6.1).
///
/// Its own column rather than a decoration of the existing marker: the `+`/`-` gutter
/// says what the change is, and overwriting it would trade one answer for another.
fn draft_marker(view: &DiffView, drafts: &crate::tui::drafts::DraftState, row: &DiffRow) -> char {
    let Some(path) = view
        .patch
        .files
        .get(row.file)
        .and_then(crate::domain::diff::FileDiff::path)
    else {
        return ' ';
    };
    // A context line is on both sides; the comment is anchored to the side the user was
    // looking at, and both are marked, because the marker is about the line on screen.
    let marked = drafts.marks(
        path.as_str(),
        Some(crate::domain::draft::Side::New),
        row.new_line,
    ) || drafts.marks(
        path.as_str(),
        Some(crate::domain::draft::Side::Old),
        row.old_line,
    );
    if marked { '●' } else { ' ' }
}

/// Whether a full-width split row is the one the cursor is on.
fn full_is_selected(view: &DiffView, row: &DiffRow) -> bool {
    view.current().is_some_and(|current| {
        current.file == row.file && current.hunk == row.hunk && current.kind == row.kind
    })
}

/// The style for a diff line's body.
fn body_style(theme: &Theme, row: &DiffRow, selected: bool) -> ratatui::style::Style {
    let style = match row.kind {
        RowKind::HunkHeader => theme.style(element::DIFF_HUNK_HEADER),
        RowKind::Placeholder | RowKind::Folded => theme.style(element::DIFF_FOLDED),
        RowKind::FileHeader => theme.style(element::TITLE),
        RowKind::Discussion => theme.style(element::COMMENT_MARKER),
        RowKind::Line => match row.line_kind {
            Some(LineKind::Add) => theme.style(element::DIFF_ADD),
            Some(LineKind::Delete) => theme.style(element::DIFF_DEL),
            _ => theme.style(element::DIFF_CONTEXT),
        },
    };
    if selected {
        theme.style(element::SELECTION).patch(style)
    } else {
        style
    }
}

/// The style for one side of a split row.
fn body_style_for(theme: &Theme, kind: LineKind, selected: bool) -> ratatui::style::Style {
    let style = match kind {
        LineKind::Add => theme.style(element::DIFF_ADD),
        LineKind::Delete => theme.style(element::DIFF_DEL),
        LineKind::Context => theme.style(element::DIFF_CONTEXT),
    };
    if selected {
        theme.style(element::SELECTION).patch(style)
    } else {
        style
    }
}

/// The style for a diff line's gutter.
fn gutter_style(theme: &Theme, row: &DiffRow, selected: bool) -> ratatui::style::Style {
    let style = match row.line_kind {
        Some(LineKind::Add) => theme.style(element::DIFF_ADD),
        Some(LineKind::Delete) => theme.style(element::DIFF_DEL),
        _ => theme.style(element::DIFF_LINE_NUMBER),
    };
    if selected {
        theme.style(element::SELECTION).patch(style)
    } else {
        style
    }
}

/// Turns cached semantic byte ranges into visible spans without replacing the
/// addition/deletion background carried by `base`.
fn highlighted_content(
    theme: &Theme,
    content: &str,
    syntax: &[SyntaxSpan],
    width: usize,
    base: Style,
    pad: bool,
) -> Vec<Span<'static>> {
    let fitted = text::truncate(content, width);
    let truncated = text::width(content) > width;
    let visible = if truncated {
        fitted
            .strip_suffix(text::ELLIPSIS)
            .unwrap_or(fitted.as_str())
    } else {
        fitted.as_str()
    };
    let Some(source) = content.get(..visible.len()) else {
        return vec![Span::styled(fitted, base)];
    };

    let mut spans = Vec::with_capacity(syntax.len().saturating_mul(2).saturating_add(2));
    let mut cursor = 0usize;
    for range in syntax {
        let start = range.start.max(cursor).min(source.len());
        let end = range.end.min(source.len());
        if start >= end {
            continue;
        }
        let (Some(before), Some(token)) = (source.get(cursor..start), source.get(start..end))
        else {
            return vec![Span::styled(fitted, base)];
        };
        if !before.is_empty() {
            spans.push(Span::styled(before.to_owned(), base));
        }
        spans.push(Span::styled(
            token.to_owned(),
            base.patch(syntax_foreground(theme, range.class)),
        ));
        cursor = end;
    }
    if let Some(rest) = source.get(cursor..)
        && !rest.is_empty()
    {
        spans.push(Span::styled(rest.to_owned(), base));
    }
    if truncated {
        spans.push(Span::styled(text::ELLIPSIS.to_string(), base));
    }
    if pad {
        let used = text::width(&fitted);
        let padding = width.saturating_sub(used);
        if padding > 0 {
            spans.push(Span::styled(" ".repeat(padding), base));
        }
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

/// Syntax contributes foreground and modifiers only; diff and cursor backgrounds
/// remain visible whatever a user theme specifies for a token.
fn syntax_foreground(theme: &Theme, class: SyntaxClass) -> Style {
    let element = match class {
        SyntaxClass::Comment => element::SYNTAX_COMMENT,
        SyntaxClass::Keyword => element::SYNTAX_KEYWORD,
        SyntaxClass::String => element::SYNTAX_STRING,
        SyntaxClass::Number => element::SYNTAX_NUMBER,
        SyntaxClass::Type => element::SYNTAX_TYPE,
        SyntaxClass::Function => element::SYNTAX_FUNCTION,
        SyntaxClass::Constant => element::SYNTAX_CONSTANT,
        SyntaxClass::Property => element::SYNTAX_PROPERTY,
        SyntaxClass::Variable => element::SYNTAX_VARIABLE,
    };
    let syntax = theme.style(element);
    let mut foreground = Style::default()
        .add_modifier(syntax.add_modifier)
        .remove_modifier(syntax.sub_modifier);
    if let Some(color) = syntax.fg {
        foreground = foreground.fg(color);
    }
    foreground
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::domain::diff::parse_patch;
    use crate::domain::pr::PullRequestDetail;
    use crate::test_support::{TempHome, temp_home};

    const PATCH: &str = "\
diff --git a/src/domain/invoice.rs b/src/domain/invoice.rs
index 1a2b3c4..5d6e7f8 100644
--- a/src/domain/invoice.rs
+++ b/src/domain/invoice.rs
@@ -12,3 +14,4 @@ impl Invoice {
     pub fn total(&self) -> Money {
-        self.lines.sum()
+        let gross = self.lines.sum();
+        gross - self.discount
     }
";

    #[test]
    fn syntax_foregrounds_keep_the_diff_background() {
        let theme = Theme::default();
        let base = Style::default()
            .bg(ratatui::style::Color::Red)
            .fg(ratatui::style::Color::Green);
        let spans = highlighted_content(
            &theme,
            "let answer = 42;",
            &[SyntaxSpan {
                start: 0,
                end: 3,
                class: SyntaxClass::Keyword,
            }],
            40,
            base,
            false,
        );

        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "let answer = 42;"
        );
        assert!(
            spans
                .iter()
                .all(|span| span.style.bg == Some(ratatui::style::Color::Red))
        );
        assert_eq!(spans[0].style.fg, theme.style(element::SYNTAX_KEYWORD).fg);
    }

    #[test]
    fn highlighted_split_content_remains_column_bounded() {
        let theme = Theme::default();
        let spans = highlighted_content(
            &theme,
            "let 東京 = \"wide\";",
            &[SyntaxSpan {
                start: 0,
                end: 3,
                class: SyntaxClass::Keyword,
            }],
            10,
            Style::default(),
            true,
        );
        let rendered = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(text::width(&rendered), 10);
        assert!(rendered.contains(text::ELLIPSIS));
    }

    #[test]
    fn a_diff_row_uses_its_cached_language_ranges() {
        let (_dir, app) = app_with_patch();
        let view = app.review.as_ref().expect("review");
        let row = view
            .rows
            .iter()
            .find(|row| row.text.contains("let gross"))
            .expect("Rust addition");
        let rendered = unified_line(&app.theme, row, false, 100, view, app.drafts());
        let keyword = rendered
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "let")
            .expect("highlighted keyword");

        assert_eq!(
            keyword.style.fg,
            app.theme.style(element::SYNTAX_KEYWORD).fg
        );
        assert_eq!(keyword.style.bg, app.theme.style(element::DIFF_ADD).bg);
    }

    fn app_with_patch() -> (TempHome, App) {
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
        let mut app = App::new(startup).unwrap();
        app.open_review(detail(), DiffView::new(parse_patch(PATCH)));
        (dir, app)
    }

    /// A detail good enough for the tabs, which read the PR's number and counts.
    fn detail() -> PullRequestDetail {
        let mut summary = crate::domain::pr::PullRequestSummary {
            number: 141,
            title: "Refactor the billing domain".to_owned(),
            author: "bruno".to_owned(),
            state: crate::domain::pr::PrState::Open,
            is_draft: false,
            base_ref: "main".to_owned(),
            head_ref: "topic".to_owned(),
            head_sha: "abc".to_owned(),
            created_at: crate::domain::time::Timestamp::default(),
            updated_at: crate::domain::time::Timestamp::default(),
            additions: 2,
            deletions: 1,
            changed_files: 1,
            labels: Vec::new(),
            review_decision: None,
            checks: crate::domain::pr::CheckSummary::default(),
            url: String::new(),
            is_cross_repository: false,
        };
        summary.number = 141;
        PullRequestDetail {
            summary,
            body: String::new(),
            merge_state_status: None,
            reviewers: Vec::new(),
            commits: Vec::new(),
            checks: Vec::new(),
            reviews: Vec::new(),
            comments: Vec::new(),
            conversation: Vec::new(),
            base_sha: None,
        }
    }

    /// Draws the whole screen, so the component sees the same geometry the loop
    /// gives it (the split threshold is a terminal-width decision).
    fn draw(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        crate::tui::test_support::buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn the_screen_shows_a_tree_and_a_diff() {
        let (_dir, mut app) = app_with_patch();
        let rendered = draw(&mut app, 120, 30);
        // The pane names the order it is in, which is what the toggle changes
        // (FR-3.5).
        assert!(
            rendered.contains("recommended order · rules (1)"),
            "{rendered}"
        );
        assert!(rendered.contains("□M invoice.rs"), "{rendered}");
        assert!(rendered.contains("invoice.rs"), "{rendered}");
        assert!(rendered.contains("unified"), "{rendered}");
        assert!(
            rendered.contains("impl Invoice"),
            "the hunk heading: {rendered}"
        );
        assert!(rendered.contains("gross - self.discount"), "{rendered}");
    }

    /// An app whose pull request is the one the analysis fixture describes.
    fn app_with_analysed_patch() -> (TempHome, App) {
        let (dir, mut app) = app_with_patch();
        app.set_environment(crate::test_support::environment());
        app.open_review(
            crate::test_support::analysis_detail(),
            DiffView::new(crate::test_support::analysis_patch()),
        );
        app.panel.analysis = Some(Box::new(crate::test_support::stored_analysis("abc123")));
        (dir, app)
    }

    #[test]
    fn the_plan_view_shows_groups_with_their_reason_and_both_positions() {
        let (_dir, mut app) = app_with_analysed_patch();
        let plan = crate::domain::plan::Plan::from_analysis(
            &app.panel.analysis.as_ref().expect("set").analysis,
        );
        if let Some(view) = app.review.as_mut() {
            view.set_plan(Some(plan));
        }
        let rendered = draw(&mut app, 140, 30);
        // FR-4.2: the heading, its position in the order, and the reason it is read
        // there. FR-3.5: where the file sits in both orders, in the status line where
        // there is room for it.
        assert!(rendered.contains("recommended order"), "{rendered}");
        assert!(rendered.contains("1. domain"), "{rendered}");
        assert!(rendered.contains("rules first"), "{rendered}");
        assert!(rendered.contains("plan ·"), "{rendered}");
        assert!(rendered.contains("path"), "{rendered}");
        assert!(rendered.contains("AI"), "{rendered}");
    }

    #[test]
    fn the_diff_header_carries_the_analysis_note_for_the_file_on_screen() {
        let (_dir, mut app) = app_with_analysed_patch();
        let rendered = draw(&mut app, 160, 24);
        // FR-4.1: what the analysis said about this file, where the reader already is.
        assert!(rendered.contains("check the sign"), "{rendered}");
    }

    #[test]
    fn ir_16_compact_guidance_keeps_code_visible_at_80_by_24() {
        let (_dir, mut app) = app_with_analysed_patch();
        let rendered = draw(&mut app, 80, 24);
        assert!(rendered.contains("What"), "{rendered}");
        assert!(rendered.contains("Why · inferred"), "{rendered}");
        assert!(rendered.contains("Verify"), "{rendered}");
        assert!(
            rendered.contains("@@ -1,1"),
            "code remains the main surface: {rendered}"
        );
        assert!(rendered.contains("e: expand"), "{rendered}");
    }

    #[test]
    fn ir_16_expanded_guidance_shows_validated_evidence_and_scroll_hint() {
        let (_dir, mut app) = app_with_analysed_patch();
        app.panel.guidance_expanded = true;
        let rendered = draw(&mut app, 120, 30);
        assert!(rendered.contains("Evidence 1"), "{rendered}");
        assert!(rendered.contains("new rounding rule"), "{rendered}");
        assert!(rendered.contains(":evidence 1"), "{rendered}");
        assert!(rendered.contains("j/k: scroll"), "{rendered}");
    }

    #[test]
    fn ir_16_missing_note_and_truncated_source_are_visible_not_invented() {
        let (_dir, mut app) = app_with_analysed_patch();
        let stored = app.panel.analysis.as_mut().expect("analysis");
        stored.analysis.per_file_notes.clear();
        stored
            .analysis
            .coverage
            .truncated_files
            .push("src/domain/money.rs".to_owned());
        app.panel.guidance_expanded = true;
        let rendered = draw(&mut app, 120, 30);
        assert!(
            rendered.contains("not supplied for this file"),
            "{rendered}"
        );
        assert!(rendered.contains("Source was truncated"), "{rendered}");
        assert!(rendered.contains("Evidence: none supplied"), "{rendered}");
    }

    #[test]
    fn ir_16_overview_separates_ai_brief_inference_plan_and_coverage() {
        let (_dir, mut app) = app_with_analysed_patch();
        app.select_review_tab(ReviewTab::Overview);
        let rendered = draw(&mut app, 120, 36);
        for expected in [
            "Review brief · AI",
            "Purpose · inferred",
            "Reading plan",
            "Coverage: 2 / 2",
            "Suggested questions",
            ":chat suggested 1",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected}: {rendered}"
            );
        }
    }

    #[test]
    fn ir_10_tabs_name_real_pull_request_destinations() {
        let (_dir, mut app) = app_with_patch();
        let rendered = draw(&mut app, 140, 30);
        assert!(rendered.contains("[1 Overview]"), "{rendered}");
        assert!(rendered.contains("[2 Files"), "{rendered}");
        assert!(rendered.contains("[3 Checks"), "{rendered}");
        assert!(rendered.contains("[4 Discussion"), "{rendered}");
        assert!(rendered.contains("[5 Ask]"), "{rendered}");
    }

    #[test]
    fn ir_10_compact_tabs_keep_every_destination_visible_at_80_columns() {
        let (_dir, mut app) = app_with_patch();
        let rendered = draw(&mut app, 80, 24);
        for label in [
            "[1 Overview]",
            "[2 Files]",
            "[3 Checks]",
            "[4 Discussion]",
            "[5 Ask]",
        ] {
            assert!(rendered.contains(label), "missing {label}: {rendered}");
        }
    }

    #[test]
    fn the_split_toggle_explains_itself_when_the_terminal_is_too_narrow() {
        let (_dir, mut app) = app_with_patch();
        if let Some(view) = app.review.as_mut() {
            view.split = true;
        }

        let narrow = draw(&mut app, 100, 24);
        assert!(
            narrow.contains("split needs 140 columns"),
            "the toggle must say why: {narrow}"
        );

        let wide = draw(&mut app, SPLIT_MIN_WIDTH, 24);
        assert!(wide.contains("split"), "{wide}");
        assert!(!wide.contains("needs 140"), "{wide}");
    }

    #[test]
    fn the_split_view_draws_both_sides_of_a_change() {
        let (_dir, mut app) = app_with_patch();
        if let Some(view) = app.review.as_mut() {
            view.split = true;
        }
        let rendered = draw(&mut app, 160, 24);
        // The deletion and the addition are paired, so both texts appear on one row
        // band and neither is lost.
        assert!(rendered.contains("self.lines.sum()"), "{rendered}");
        assert!(rendered.contains("let gross"), "{rendered}");
    }

    #[test]
    fn a_pane_that_is_too_short_still_renders_the_rows_that_fit() {
        let (_dir, mut app) = app_with_patch();
        let rendered = draw(&mut app, 100, 24);
        assert!(rendered.contains("order"), "{rendered}");
        assert!(rendered.contains("unified"), "{rendered}");
    }

    #[test]
    fn an_empty_patch_says_so_instead_of_showing_an_empty_pane() {
        let (_dir, mut app) = app_with_patch();
        app.set_review(DiffView::new(crate::domain::diff::Patch::default()));
        let rendered = draw(&mut app, 120, 24);
        assert!(rendered.contains("no changes to show"), "{rendered}");
    }

    #[test]
    fn the_tree_marks_the_file_the_cursor_is_in() {
        let (_dir, mut app) = app_with_patch();
        if let Some(view) = app.review.as_mut() {
            view.tree_focused = true;
        }
        let rendered = draw(&mut app, 120, 24);
        assert!(
            rendered.contains('·'),
            "the current-file marker: {rendered}"
        );
    }

    #[test]
    fn the_split_view_pairs_a_deletion_with_the_addition_that_replaced_it() {
        let (_dir, mut app) = app_with_patch();
        if let Some(view) = app.review.as_mut() {
            view.split = true;
        }
        let rendered = draw(&mut app, 160, 24);
        // One drawn row carries both the old and the new text.
        let pair = rendered
            .lines()
            .find(|line| line.contains("self.lines.sum()"))
            .unwrap_or_default();
        assert!(
            pair.contains("let gross"),
            "the deletion and its replacement should share a row: {pair:?}"
        );
    }

    #[test]
    fn prepare_keeps_the_tree_cursor_visible() {
        let mut view = DiffView::new(parse_patch(PATCH));
        for _ in 0..5 {
            view.move_tree(1);
        }
        view.prepare(4, 3);
        assert!(view.tree_scroll <= view.tree_cursor);
        assert!(view.tree_cursor < view.tree_scroll + 3);
    }
}
