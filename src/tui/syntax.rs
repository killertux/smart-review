//! Tree-sitter-backed syntax classification for diff lines.
//!
//! Highlighting is prepared with the immutable diff projection on a background
//! worker. The renderer only looks up semantic byte ranges and resolves them through
//! the active UI theme; it never parses source during a frame.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use tree_sitter::Language;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

use crate::domain::diff::{FileDiff, Hunk, LineKind, Patch, RelPath};
use crate::domain::draft::Side;
use crate::ports::Cancel;

/// Maximum source passed to one parser invocation.
///
/// A hunk is normally tiny. Bounding pathological generated lines keeps a single
/// grammar invocation from monopolising a worker indefinitely.
const MAX_HUNK_BYTES: usize = 256 * 1024;
/// Maximum duplicated old/new hunk source retained while preparing one patch.
const MAX_PATCH_BYTES: usize = 4 * 1024 * 1024;
/// Maximum source lines passed to one parser invocation.
const MAX_HUNK_LINES: usize = 10_000;
/// Maximum duplicated old/new source lines highlighted across one patch.
const MAX_PATCH_LINES: usize = 25_000;

/// Semantic syntax classes understood by the theme engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyntaxClass {
    Comment,
    Keyword,
    String,
    Number,
    Type,
    Function,
    Constant,
    Property,
    Variable,
}

impl SyntaxClass {
    fn from_highlight(index: usize) -> Option<Self> {
        HIGHLIGHT_CLASSES.get(index).copied()
    }
}

/// One styled byte range within a diff line's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SyntaxSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) class: SyntaxClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct LineKey {
    file: usize,
    side: Side,
    line: u32,
}

/// Theme-independent syntax ranges for one patch.
#[derive(Debug, Clone, Default)]
pub(crate) struct SyntaxHighlights {
    lines: BTreeMap<LineKey, Vec<SyntaxSpan>>,
    limited: bool,
}

impl SyntaxHighlights {
    /// Highlights every supported textual hunk within a fixed work budget.
    #[must_use]
    pub(crate) fn for_patch(patch: &Patch, cancel: Option<&Cancel>) -> Self {
        if cancelled(cancel) {
            return Self::default();
        }
        let mut result = Self::default();
        let mut remaining = MAX_PATCH_BYTES;
        let mut remaining_lines = MAX_PATCH_LINES;
        let mut highlighter = Highlighter::new();

        for (file_index, file) in patch.files.iter().enumerate() {
            if cancelled(cancel) {
                break;
            }
            for side in [Side::Old, Side::New] {
                let Some(language) = detect_language(file, side) else {
                    continue;
                };
                for hunk in &file.hunks {
                    if cancelled(cancel) {
                        return result;
                    }
                    let estimate = SourceChunk::estimate(hunk, side);
                    if estimate.bytes == 0 {
                        continue;
                    }
                    if estimate.bytes > MAX_HUNK_BYTES
                        || estimate.bytes > remaining
                        || estimate.lines > MAX_HUNK_LINES
                        || estimate.lines > remaining_lines
                    {
                        result.limited = true;
                        continue;
                    }
                    let Some(configuration) = registry().configuration(language) else {
                        continue;
                    };
                    let Some(source) =
                        SourceChunk::from_hunk(file_index, hunk, side, estimate.bytes)
                    else {
                        continue;
                    };
                    remaining -= estimate.bytes;
                    remaining_lines -= estimate.lines;
                    if let Some(lines) =
                        highlight_source(&mut highlighter, configuration, &source, cancel)
                    {
                        for (key, spans) in lines {
                            result.lines.insert(key, spans);
                        }
                    }
                }
            }
        }
        result
    }

    /// Semantic ranges for one side and line number.
    #[must_use]
    pub(crate) fn spans(&self, file: usize, side: Side, line: u32) -> &[SyntaxSpan] {
        self.lines
            .get(&LineKey { file, side, line })
            .map_or(&[], Vec::as_slice)
    }

    /// Whether the bounded highlighter deliberately left part of the patch plain.
    #[must_use]
    pub(crate) const fn limited(&self) -> bool {
        self.limited
    }

    /// Approximate heap bytes retained by semantic line ranges.
    #[must_use]
    pub(crate) fn projection_bytes(&self) -> usize {
        self.lines
            .len()
            .saturating_mul(std::mem::size_of::<(LineKey, Vec<SyntaxSpan>)>())
            .saturating_add(
                self.lines
                    .values()
                    .map(|spans| {
                        spans
                            .capacity()
                            .saturating_mul(std::mem::size_of::<SyntaxSpan>())
                    })
                    .sum::<usize>(),
            )
    }
}

fn cancelled(cancel: Option<&Cancel>) -> bool {
    cancel.is_some_and(Cancel::is_cancelled)
}

#[derive(Debug)]
struct SourceLine {
    key: LineKey,
    start: usize,
    end: usize,
}

#[derive(Debug)]
struct SourceChunk {
    text: String,
    lines: Vec<SourceLine>,
}

#[derive(Debug, Default)]
struct SourceEstimate {
    bytes: usize,
    lines: usize,
}

impl SourceChunk {
    fn estimate(hunk: &Hunk, side: Side) -> SourceEstimate {
        hunk.lines
            .iter()
            .fold(SourceEstimate::default(), |mut estimate, line| {
                if belongs_to(line.kind, side) {
                    estimate.bytes = estimate
                        .bytes
                        .saturating_add(line.content.len())
                        .saturating_add(1);
                    estimate.lines = estimate.lines.saturating_add(1);
                }
                estimate
            })
    }

    fn from_hunk(file: usize, hunk: &Hunk, side: Side, bytes: usize) -> Option<Self> {
        let mut text = String::with_capacity(bytes);
        let mut lines = Vec::with_capacity(hunk.lines.len());
        for line in &hunk.lines {
            if !belongs_to(line.kind, side) {
                continue;
            }
            let number = match side {
                Side::Old => line.old_line,
                Side::New => line.new_line,
            };
            let Some(number) = number else {
                continue;
            };
            let start = text.len();
            text.push_str(&line.content);
            let end = text.len();
            text.push('\n');
            lines.push(SourceLine {
                key: LineKey {
                    file,
                    side,
                    line: number,
                },
                start,
                end,
            });
        }
        (!lines.is_empty()).then_some(Self { text, lines })
    }
}

const fn belongs_to(kind: LineKind, side: Side) -> bool {
    !matches!(
        (kind, side),
        (LineKind::Add, Side::Old) | (LineKind::Delete, Side::New)
    )
}

fn highlight_source(
    highlighter: &mut Highlighter,
    configuration: &HighlightConfiguration,
    source: &SourceChunk,
    cancel: Option<&Cancel>,
) -> Option<BTreeMap<LineKey, Vec<SyntaxSpan>>> {
    let events = highlighter
        .highlight(configuration, source.text.as_bytes(), None, |_| None)
        .ok()?;
    let mut classes = Vec::new();
    let mut ranges = BTreeMap::<LineKey, Vec<SyntaxSpan>>::new();
    let mut line_cursor = 0usize;

    for event in events {
        if cancelled(cancel) {
            return None;
        }
        match event.ok()? {
            HighlightEvent::HighlightStart(highlight) => {
                classes.push(SyntaxClass::from_highlight(highlight.0));
            }
            HighlightEvent::HighlightEnd => {
                classes.pop();
            }
            HighlightEvent::Source { start, end } => {
                let class = classes.iter().rev().find_map(|class| *class);
                if let Some(class) = class {
                    map_range(
                        &mut ranges,
                        &source.lines,
                        &mut line_cursor,
                        start,
                        end,
                        class,
                    );
                }
            }
        }
    }
    Some(ranges)
}

fn map_range(
    highlighted: &mut BTreeMap<LineKey, Vec<SyntaxSpan>>,
    lines: &[SourceLine],
    line_cursor: &mut usize,
    start: usize,
    end: usize,
    class: SyntaxClass,
) {
    // `HighlightEvent::Source` ranges arrive in document order. Retain the first line
    // that can overlap the next range rather than replaying every preceding source line
    // for every token in a dense hunk.
    while lines
        .get(*line_cursor)
        .is_some_and(|line| line.end <= start)
    {
        *line_cursor += 1;
    }
    for line in lines[*line_cursor..]
        .iter()
        .take_while(|line| line.start < end)
    {
        let local_start = start.max(line.start).saturating_sub(line.start);
        let local_end = end.min(line.end).saturating_sub(line.start);
        if local_start >= local_end {
            continue;
        }
        let spans = highlighted.entry(line.key).or_default();
        if let Some(previous) = spans.last_mut()
            && previous.end == local_start
            && previous.class == class
        {
            previous.end = local_end;
        } else {
            spans.push(SyntaxSpan {
                start: local_start,
                end: local_end,
                class,
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupportedLanguage {
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Java,
    CSharp,
    C,
    Cpp,
    Go,
    Rust,
    Php,
    Bash,
}

fn detect_language(file: &FileDiff, side: Side) -> Option<SupportedLanguage> {
    let path = match side {
        Side::Old => file.old_path.as_ref().or(file.new_path.as_ref()),
        Side::New => file.new_path.as_ref().or(file.old_path.as_ref()),
    };
    path.and_then(language_for_path).or_else(|| {
        file.hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .find_map(|line| {
                if !belongs_to(line.kind, side) {
                    return None;
                }
                let number = match side {
                    Side::Old => line.old_line,
                    Side::New => line.new_line,
                };
                (number == Some(1))
                    .then(|| language_for_first_line(&line.content))
                    .flatten()
            })
    })
}

fn language_for_path(path: &RelPath) -> Option<SupportedLanguage> {
    let extension = path.extension()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "ts" | "mts" | "cts" => SupportedLanguage::TypeScript,
        "tsx" => SupportedLanguage::Tsx,
        "js" | "mjs" | "cjs" | "jsx" => SupportedLanguage::JavaScript,
        "py" | "pyw" => SupportedLanguage::Python,
        "java" => SupportedLanguage::Java,
        "cs" => SupportedLanguage::CSharp,
        "c" | "h" => SupportedLanguage::C,
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => SupportedLanguage::Cpp,
        "go" => SupportedLanguage::Go,
        "rs" => SupportedLanguage::Rust,
        "php" | "php3" | "php4" | "php5" | "phtml" => SupportedLanguage::Php,
        "sh" | "bash" | "zsh" => SupportedLanguage::Bash,
        _ => return None,
    })
}

fn language_for_first_line(source: &str) -> Option<SupportedLanguage> {
    let first = source.lines().next()?.to_ascii_lowercase();
    if !first.starts_with("#!") {
        return None;
    }
    if first.contains("python") {
        Some(SupportedLanguage::Python)
    } else if first.contains("node") || first.contains("deno") {
        Some(SupportedLanguage::JavaScript)
    } else if first.contains("bash") || first.contains("/sh") || first.contains("zsh") {
        Some(SupportedLanguage::Bash)
    } else {
        None
    }
}

struct Registry {
    typescript: Option<HighlightConfiguration>,
    tsx: Option<HighlightConfiguration>,
    javascript: Option<HighlightConfiguration>,
    python: Option<HighlightConfiguration>,
    java: Option<HighlightConfiguration>,
    c_sharp: Option<HighlightConfiguration>,
    c: Option<HighlightConfiguration>,
    cpp: Option<HighlightConfiguration>,
    go: Option<HighlightConfiguration>,
    rust: Option<HighlightConfiguration>,
    php: Option<HighlightConfiguration>,
    bash: Option<HighlightConfiguration>,
}

impl Registry {
    fn new() -> Self {
        let (javascript_query, typescript_query, tsx_query, cpp_query) = inherited_queries();
        Self {
            typescript: configuration(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                "typescript",
                &typescript_query,
                "",
                tree_sitter_typescript::LOCALS_QUERY,
            ),
            tsx: configuration(
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                "tsx",
                &tsx_query,
                "",
                tree_sitter_typescript::LOCALS_QUERY,
            ),
            javascript: configuration(
                tree_sitter_javascript::LANGUAGE.into(),
                "javascript",
                &javascript_query,
                tree_sitter_javascript::INJECTIONS_QUERY,
                tree_sitter_javascript::LOCALS_QUERY,
            ),
            python: configuration(
                tree_sitter_python::LANGUAGE.into(),
                "python",
                tree_sitter_python::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            java: configuration(
                tree_sitter_java::LANGUAGE.into(),
                "java",
                tree_sitter_java::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            // The published C# crate only packages its highlights query.
            c_sharp: configuration(
                tree_sitter_c_sharp::LANGUAGE.into(),
                "csharp",
                tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            c: configuration(
                tree_sitter_c::LANGUAGE.into(),
                "c",
                tree_sitter_c::HIGHLIGHT_QUERY,
                "",
                "",
            ),
            cpp: configuration(tree_sitter_cpp::LANGUAGE.into(), "cpp", &cpp_query, "", ""),
            go: configuration(
                tree_sitter_go::LANGUAGE.into(),
                "go",
                tree_sitter_go::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            rust: configuration(
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY,
                tree_sitter_rust::INJECTIONS_QUERY,
                "",
            ),
            // Hunk fragments commonly omit `<?php`; the PHP-only grammar remains
            // useful in that incomplete-source setting.
            php: configuration(
                tree_sitter_php::LANGUAGE_PHP_ONLY.into(),
                "php",
                tree_sitter_php::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
            bash: configuration(
                tree_sitter_bash::LANGUAGE.into(),
                "bash",
                tree_sitter_bash::HIGHLIGHT_QUERY,
                "",
                "",
            ),
        }
    }

    const fn configuration(&self, language: SupportedLanguage) -> Option<&HighlightConfiguration> {
        match language {
            SupportedLanguage::TypeScript => self.typescript.as_ref(),
            SupportedLanguage::Tsx => self.tsx.as_ref(),
            SupportedLanguage::JavaScript => self.javascript.as_ref(),
            SupportedLanguage::Python => self.python.as_ref(),
            SupportedLanguage::Java => self.java.as_ref(),
            SupportedLanguage::CSharp => self.c_sharp.as_ref(),
            SupportedLanguage::C => self.c.as_ref(),
            SupportedLanguage::Cpp => self.cpp.as_ref(),
            SupportedLanguage::Go => self.go.as_ref(),
            SupportedLanguage::Rust => self.rust.as_ref(),
            SupportedLanguage::Php => self.php.as_ref(),
            SupportedLanguage::Bash => self.bash.as_ref(),
        }
    }
}

fn inherited_queries() -> (String, String, String, String) {
    let javascript = format!(
        "{}\n{}",
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
    );
    let typescript = format!(
        "{}\n{}",
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_typescript::HIGHLIGHTS_QUERY
    );
    let tsx = format!(
        "{}\n{}\n{}",
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        tree_sitter_typescript::HIGHLIGHTS_QUERY
    );
    let cpp = format!(
        "{}\n{}",
        tree_sitter_c::HIGHLIGHT_QUERY,
        tree_sitter_cpp::HIGHLIGHT_QUERY
    );
    (javascript, typescript, tsx, cpp)
}

fn configuration(
    language: Language,
    name: &str,
    highlights: &str,
    injections: &str,
    locals: &str,
) -> Option<HighlightConfiguration> {
    let mut configuration =
        HighlightConfiguration::new(language, name, highlights, injections, locals).ok()?;
    configuration.configure(HIGHLIGHT_NAMES);
    Some(configuration)
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::new)
}

const HIGHLIGHT_NAMES: &[&str] = &[
    "comment",
    "keyword",
    "string",
    "number",
    "type",
    "function",
    "constant",
    "property",
    "variable",
    "tag",
    "attribute",
    "operator",
    "constructor",
    "module",
    "label",
];

const HIGHLIGHT_CLASSES: &[SyntaxClass] = &[
    SyntaxClass::Comment,
    SyntaxClass::Keyword,
    SyntaxClass::String,
    SyntaxClass::Number,
    SyntaxClass::Type,
    SyntaxClass::Function,
    SyntaxClass::Constant,
    SyntaxClass::Property,
    SyntaxClass::Variable,
    SyntaxClass::Property,
    SyntaxClass::Property,
    SyntaxClass::Keyword,
    SyntaxClass::Type,
    SyntaxClass::Type,
    SyntaxClass::Constant,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::diff::parse_patch;

    #[test]
    fn popular_language_extensions_are_recognised() {
        let cases = [
            ("app.ts", SupportedLanguage::TypeScript),
            ("app.tsx", SupportedLanguage::Tsx),
            ("app.jsx", SupportedLanguage::JavaScript),
            ("app.py", SupportedLanguage::Python),
            ("App.java", SupportedLanguage::Java),
            ("App.cs", SupportedLanguage::CSharp),
            ("app.c", SupportedLanguage::C),
            ("app.cpp", SupportedLanguage::Cpp),
            ("app.go", SupportedLanguage::Go),
            ("app.rs", SupportedLanguage::Rust),
            ("app.php", SupportedLanguage::Php),
            ("app.sh", SupportedLanguage::Bash),
        ];
        for (path, expected) in cases {
            let path = RelPath::parse(path).unwrap_or_else(|| panic!("test path should be valid"));
            assert_eq!(language_for_path(&path), Some(expected));
        }
    }

    #[test]
    fn extensionless_scripts_use_their_shebang() {
        assert_eq!(
            language_for_first_line("#!/usr/bin/env python3\nprint('ok')"),
            Some(SupportedLanguage::Python)
        );
        assert_eq!(
            language_for_first_line("#!/bin/bash\necho ok"),
            Some(SupportedLanguage::Bash)
        );
    }

    #[test]
    fn every_supported_grammar_builds_its_highlight_query() {
        let registry = registry();
        for language in [
            SupportedLanguage::TypeScript,
            SupportedLanguage::Tsx,
            SupportedLanguage::JavaScript,
            SupportedLanguage::Python,
            SupportedLanguage::Java,
            SupportedLanguage::CSharp,
            SupportedLanguage::C,
            SupportedLanguage::Cpp,
            SupportedLanguage::Go,
            SupportedLanguage::Rust,
            SupportedLanguage::Php,
            SupportedLanguage::Bash,
        ] {
            assert!(
                registry.configuration(language).is_some(),
                "{language:?} query should compile"
            );
        }
    }

    #[test]
    fn every_priority_language_produces_semantic_ranges() {
        let cases = [
            ("app.ts", "const value: string = \"ok\";"),
            ("app.tsx", "const view = <Button label=\"ok\" />;"),
            ("app.jsx", "const view = <Button label=\"ok\" />;"),
            ("app.py", "value = \"ok\""),
            ("App.java", "String value = \"ok\";"),
            ("App.cs", "string value = \"ok\";"),
            ("app.c", "const char *value = \"ok\";"),
            ("app.cpp", "std::string value = \"ok\";"),
            ("app.go", "value := \"ok\""),
            ("app.rs", "let value = \"ok\";"),
            ("app.php", "$value = \"ok\";"),
            ("app.sh", "value=\"ok\""),
        ];

        for (path, source) in cases {
            let patch = parse_patch(&format!(
                "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old\n+{source}\n"
            ));
            let highlights = SyntaxHighlights::for_patch(&patch, None);
            assert!(
                highlights
                    .spans(0, Side::New, 1)
                    .iter()
                    .any(|span| span.class == SyntaxClass::String),
                "{path} should classify its string literal"
            );
        }
    }

    #[test]
    fn old_and_new_sides_receive_independent_ranges() {
        let patch = parse_patch(concat!(
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1,2 +1,2 @@\n",
            "-let old = \"gone\";\n",
            "+let new = \"here\";\n",
            " // shared\n",
        ));
        let highlights = SyntaxHighlights::for_patch(&patch, None);
        assert!(
            highlights
                .spans(0, Side::Old, 1)
                .iter()
                .any(|span| span.class == SyntaxClass::String)
        );
        assert!(
            highlights
                .spans(0, Side::New, 1)
                .iter()
                .any(|span| span.class == SyntaxClass::String)
        );
        assert!(
            highlights
                .spans(0, Side::New, 2)
                .iter()
                .any(|span| span.class == SyntaxClass::Comment),
            "new context spans: {:?}",
            highlights.spans(0, Side::New, 2)
        );
    }

    #[test]
    fn an_oversized_hunk_falls_back_to_plain_text() {
        let source = "x".repeat(MAX_HUNK_BYTES + 1);
        let patch = parse_patch(&format!(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+{source}\n"
        ));
        let highlights = SyntaxHighlights::for_patch(&patch, None);

        assert!(highlights.limited());
        assert!(highlights.spans(0, Side::New, 1).is_empty());
    }

    #[test]
    fn too_many_short_lines_fall_back_without_parsing() {
        let source = "\"x\"\n".repeat(MAX_HUNK_LINES + 1);
        let mut additions = String::with_capacity(source.len() + MAX_HUNK_LINES + 1);
        for line in source.lines() {
            additions.push('+');
            additions.push_str(line);
            additions.push('\n');
        }
        let line_count = MAX_HUNK_LINES + 1;
        let patch = parse_patch(&format!(
            "diff --git a/dense.rs b/dense.rs\n--- a/dense.rs\n+++ b/dense.rs\n@@ -0,0 +1,{line_count} @@\n{additions}"
        ));
        let highlights = SyntaxHighlights::for_patch(&patch, None);

        assert!(highlights.limited());
        assert!(highlights.lines.is_empty());
    }

    #[test]
    fn cancellation_stops_highlight_preparation() {
        let patch = parse_patch(concat!(
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1 +1 @@\n",
            "-let old = \"gone\";\n",
            "+let new = \"here\";\n",
        ));
        let cancel = Cancel::new();
        cancel.cancel();
        let highlights = SyntaxHighlights::for_patch(&patch, Some(&cancel));

        assert!(highlights.spans(0, Side::New, 1).is_empty());
    }

    #[test]
    fn dense_highlight_ranges_map_without_replaying_prior_lines() {
        const LINE_COUNT: usize = 10_000;
        let lines = (0..LINE_COUNT)
            .map(|index| SourceLine {
                key: LineKey {
                    file: 0,
                    side: Side::New,
                    line: u32::try_from(index + 1).unwrap_or(u32::MAX),
                },
                start: index * 4,
                end: index * 4 + 3,
            })
            .collect::<Vec<_>>();
        let mut highlighted = BTreeMap::new();
        let mut line_cursor = 0;

        for line in &lines {
            map_range(
                &mut highlighted,
                &lines,
                &mut line_cursor,
                line.start,
                line.end,
                SyntaxClass::String,
            );
        }

        assert_eq!(highlighted.len(), LINE_COUNT);
        assert_eq!(
            highlighted.get(&lines[LINE_COUNT - 1].key),
            Some(&vec![SyntaxSpan {
                start: 0,
                end: 3,
                class: SyntaxClass::String,
            }])
        );
    }

    #[test]
    #[ignore = "opt-in IR-17 reference workload; run in release mode"]
    fn ir_17_reference_dense_syntax_hunk() {
        const LINE_COUNT: usize = 50_000;
        let source = "\"x\"\n".repeat(LINE_COUNT);
        let mut additions = String::with_capacity(source.len() + LINE_COUNT);
        for line in source.lines() {
            additions.push('+');
            additions.push_str(line);
            additions.push('\n');
        }
        let patch = parse_patch(&format!(
            "diff --git a/dense.rs b/dense.rs\n--- a/dense.rs\n+++ b/dense.rs\n@@ -0,0 +1,{LINE_COUNT} @@\n{additions}"
        ));

        let started = std::time::Instant::now();
        let highlights = SyntaxHighlights::for_patch(&patch, None);
        let duration = started.elapsed();
        let highlighted_lines = highlights.lines.len();

        eprintln!(
            "IR17_METRIC profile={} workload=dense_syntax_hunk source_bytes={} lines={LINE_COUNT} highlighted_lines={highlighted_lines} limited={} duration_us={} projection_bytes={}",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            source.len(),
            highlights.limited(),
            duration.as_micros(),
            highlights.projection_bytes(),
        );
        assert_eq!(highlighted_lines, 0);
        assert!(highlights.limited());
    }
}
