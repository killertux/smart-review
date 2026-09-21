//! The analysis document (FR-4.1, FR-4.2, §7.1).
//!
//! One request produces one document: what changed, why, what is risky, and in what
//! order the files should be read. Three properties shape everything here:
//!
//! - **the model's output is untrusted input.** It is prose wrapped around JSON,
//!   sometimes fenced, sometimes with an extra key, sometimes with a file that is not
//!   in the diff. Parsing is therefore tolerant per entry — the same lesson the model
//!   model catalog — and normalization is what makes the document *safe to
//!   display*: every path it mentions is checked against the diff, and anything that
//!   is not there is dropped with a warning rather than rendered as a link to a file
//!   that does not exist (FR-4.1).
//! - **the plan is advisory, the diff is truth.** Missing files are appended under an
//!   `unclassified` group, so the ordered view can never hide a file the model forgot
//!   (FR-4.1, FR-4.2).
//! - **a failed parse is a normal outcome, not an exception.** The caller gets the raw
//!   text back so it can show it, and the failure carries the reason the repair retry
//!   is built from.
//!
//! Nothing here performs IO, so the whole of it is testable without a network or a
//! repository (NFR-5.2).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::domain::diff::Patch;
use crate::domain::draft::Side;

/// The prompt version. Bumping it invalidates every cached analysis (FR-4.3).
///
/// Change this when the *meaning* of the request changes: the schema, the grounding
/// rules, or the ordering the plan is asked for. Reworded instructions that ask for the
/// same thing do not need it.
pub const PROMPT_VERSION: u32 = 2;

/// The document version, for migrations (§7.1).
pub const ANALYSIS_VERSION: u32 = 2;

/// The group that catches files the plan did not mention (FR-4.1).
pub const UNCLASSIFIED: &str = "unclassified";

/// How much raw model text is kept for the failure view. A document that has to be
/// repaired is small; this only bounds a pathological answer.
pub const MAX_RAW_BYTES: usize = 64 * 1024;

/// How risky a change is, as the model judges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Worth a look.
    Low,
    /// Worth a careful read.
    Medium,
    /// Worth reading first.
    High,
}

impl Severity {
    /// The word shown in the interface.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// A place the model thinks deserves attention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskArea {
    /// A short name, shown as a heading.
    pub title: String,
    /// How much attention it deserves.
    pub severity: Severity,
    /// The files it concerns, already restricted to files in the diff.
    #[serde(default)]
    pub files: Vec<String>,
    /// Whether at least one cited file survived normalization. A false value keeps the
    /// model's diagnostic visible without presenting it as evidenced actionable risk.
    #[serde(default = "supported_by_default")]
    pub supported: bool,
    /// Why it is a risk.
    pub why: String,
}

const fn supported_by_default() -> bool {
    true
}

/// One step of the review plan (FR-4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanGroup {
    /// Where it sits in the order, from one.
    pub order: u32,
    /// The group's name, usually an architectural role.
    pub group: String,
    /// Why this group is read at this point.
    pub rationale: String,
    /// The files in it, already restricted to files in the diff.
    #[serde(default)]
    pub files: Vec<String>,
}

/// What the model noticed about one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileNote {
    /// Which file.
    pub path: String,
    /// What changed in it.
    #[serde(default, alias = "change")]
    pub what_changed: String,
    /// Why this file changed, explicitly an inference rather than author-provided fact.
    #[serde(default, alias = "notes")]
    pub why: String,
    /// Concrete things to verify. The prompt asks for at most two; excess model output
    /// is retained for the expanded view rather than silently discarded.
    #[serde(default, alias = "review_focus")]
    pub verify: Vec<String>,
    /// Validated source references supporting this guidance.
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

/// A validated source location supporting an analysis claim (IR-16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// Canonical changed-file path.
    pub path: String,
    /// Side of the patch, when the model supplied a coordinate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
    /// Validated line number, when the referenced line exists in the patch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Short explanation of what the reference supports.
    #[serde(default)]
    pub label: String,
}

/// What the model could and could not cover (IR-16).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Coverage {
    /// Changed files the model says it analyzed, normalized to canonical paths.
    #[serde(default)]
    pub analyzed_files: Vec<String>,
    /// Changed files whose source was truncated or unavailable.
    #[serde(default)]
    pub truncated_files: Vec<String>,
    /// Other explicit limits on the answer.
    #[serde(default)]
    pub limitations: Vec<String>,
}

/// Token accounting for one analysis (§7.1).
///
/// Duplicated from the port deliberately: the document is persisted, so its shape is
/// the domain's to keep stable, and the adapter's numbers are copied into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AnalysisUsage {
    /// Whether every executed attempt reported usage. A false value means the numeric
    /// fields must not be read as a complete cost (IR-02).
    #[serde(default)]
    pub complete: bool,
    /// Prompt tokens.
    #[serde(default)]
    pub prompt: u32,
    /// Completion tokens.
    #[serde(default)]
    pub completion: u32,
    /// Reasoning tokens, when the provider reports them (FR-4.8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u32>,
}

/// A complete analysis (§7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Analysis {
    /// The document version.
    pub version: u32,
    /// The prompt version that produced it.
    pub prompt_version: u32,
    /// `provider/model`, for the "analysed by" line.
    pub model: String,
    /// The commit analysed. A different one makes this stale (FR-4.3).
    pub head_sha: String,
    /// When it was produced, RFC 3339.
    pub created_at: String,
    /// What the request cost, when the provider said.
    #[serde(default)]
    pub token_usage: AnalysisUsage,
    /// What changed, in approximately two concise sentences.
    #[serde(alias = "summary")]
    pub brief: String,
    /// Why, inferred from the diff, commits and conventions rather than copied from the author.
    #[serde(alias = "intent")]
    pub inferred_purpose: String,
    /// What deserves attention.
    #[serde(default)]
    pub risk_areas: Vec<RiskArea>,
    /// What to read, and in what order.
    #[serde(default)]
    pub review_plan: Vec<PlanGroup>,
    /// Per-file notes.
    #[serde(default)]
    pub per_file_notes: Vec<FileNote>,
    /// Questions worth asking the author.
    #[serde(default)]
    pub suggested_questions: Vec<String>,
    /// Explicit coverage and limitations for this answer.
    #[serde(default)]
    pub coverage: Coverage,
}

impl Analysis {
    /// The note for a path, if there is one.
    #[must_use]
    pub fn note(&self, path: &str) -> Option<&FileNote> {
        self.per_file_notes.iter().find(|note| note.path == path)
    }

    /// The group a path was placed in, if any.
    #[must_use]
    pub fn group_of(&self, path: &str) -> Option<&PlanGroup> {
        self.review_plan
            .iter()
            .find(|group| group.files.iter().any(|file| file == path))
    }

    /// One line summarising what was analysed and when, for the panel header.
    ///
    /// `now` is passed in rather than read here because this module performs no IO
    /// and holds no clock (NFR-5.2).
    #[must_use]
    pub fn provenance(&self, now: crate::domain::time::Timestamp) -> String {
        let age = match chrono::DateTime::parse_from_rfc3339(&self.created_at) {
            Ok(created) => crate::domain::time::relative(now, created.with_timezone(&chrono::Utc)),
            // A document written by something other than this build still has to be
            // displayable; its age is simply unknown.
            Err(_) => "age unknown".to_owned(),
        };
        format!("{} · prompt v{} · {age}", self.model, self.prompt_version)
    }

    /// Whether this analysis was produced by the given prompt version.
    #[must_use]
    pub fn matches_prompt(&self) -> bool {
        self.prompt_version == PROMPT_VERSION
    }
}

/// What normalizing a model answer produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalized {
    /// The document, with the caller's fields (`model`, `head_sha`) stamped in.
    pub analysis: Analysis,
    /// What had to be corrected or thrown away, in the order it was found.
    ///
    /// Reported rather than swallowed: "3 unknown paths dropped" is the difference
    /// between a trustworthy plan and one that quietly lost a file.
    pub warnings: Vec<String>,
}

/// Why a model answer could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFailure {
    /// What was wrong, in one sentence, for the repair prompt and the failure view.
    pub reason: String,
}

impl std::fmt::Display for ParseFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// What the model said, before normalization.
///
/// Every field is optional and every collection is tolerant: the model is asked for a
/// shape, and what it returns is a suggestion. A field that is missing is a warning;
/// a field with the wrong type is dropped with a warning; only an answer with nothing
/// usable in it is a failure.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawAnalysis {
    #[serde(alias = "summary")]
    brief: Option<String>,
    #[serde(alias = "intent")]
    inferred_purpose: Option<String>,
    risk_areas: Vec<serde_json::Value>,
    review_plan: Vec<serde_json::Value>,
    per_file_notes: Vec<serde_json::Value>,
    suggested_questions: Vec<serde_json::Value>,
    coverage: Option<RawCoverage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawRisk {
    title: Option<String>,
    severity: Option<String>,
    files: Vec<serde_json::Value>,
    why: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawGroup {
    order: Option<serde_json::Value>,
    group: Option<String>,
    rationale: Option<String>,
    files: Vec<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawNote {
    path: Option<String>,
    #[serde(alias = "change")]
    what_changed: Option<String>,
    #[serde(alias = "notes")]
    why: Option<String>,
    #[serde(alias = "review_focus")]
    verify: Vec<serde_json::Value>,
    evidence: Vec<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawEvidence {
    path: Option<String>,
    side: Option<String>,
    line: Option<serde_json::Value>,
    label: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawCoverage {
    analyzed_files: Vec<serde_json::Value>,
    truncated_files: Vec<serde_json::Value>,
    limitations: Vec<serde_json::Value>,
}

/// Which paths the diff actually contains, and where each one leads.
///
/// A rename is the reason this is not just a set: the model sees both names in the
/// diff and may use either, and a note about the old name is a note about the file.
#[derive(Debug, Clone, Default)]
pub struct PathIndex {
    paths: BTreeSet<String>,
    aliases: BTreeMap<String, BTreeSet<String>>,
    lines: BTreeMap<String, (BTreeSet<u32>, BTreeSet<u32>)>,
}

impl PathIndex {
    /// Builds the index from a patch.
    #[must_use]
    pub fn from_patch(patch: &Patch) -> Self {
        let mut index = Self::default();
        for file in &patch.files {
            // The path a reader would open, not `display_path`'s "old → new" label:
            // this is the string the diff view and `:copy-path` use.
            let Some(canonical) = file.path().map(ToString::to_string) else {
                continue;
            };
            index.paths.insert(canonical);
        }
        for canonical in &index.paths {
            let name = canonical.rsplit('/').next().unwrap_or(canonical).to_owned();
            // A bare file name is only an alias when it is unambiguous, otherwise
            // `mod.rs` would attach notes to whichever one was indexed first.
            if index
                .paths
                .iter()
                .filter(|path| path.rsplit('/').next() == Some(name.as_str()))
                .count()
                == 1
            {
                index
                    .aliases
                    .entry(name)
                    .or_default()
                    .insert(canonical.clone());
            }
        }
        for file in &patch.files {
            let Some(canonical) = file.path().map(ToString::to_string) else {
                continue;
            };
            let entry = index.lines.entry(canonical.clone()).or_default();
            for line in file.hunks.iter().flat_map(|hunk| &hunk.lines) {
                if let Some(number) = line.old_line {
                    entry.0.insert(number);
                }
                if let Some(number) = line.new_line {
                    entry.1.insert(number);
                }
            }
            if let Some(old) = &file.old_path {
                let old = old.as_str().to_owned();
                if old != canonical {
                    index.aliases.entry(old).or_default().insert(canonical);
                }
            }
        }
        index
    }

    /// Builds the index from a list of paths, for callers that have no patch.
    #[must_use]
    pub fn from_paths(paths: impl IntoIterator<Item = String>) -> Self {
        let mut index = Self::default();
        for path in paths {
            index.paths.insert(path);
        }
        index
    }

    /// How many files are in the diff.
    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the diff is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Every canonical path, in path order.
    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.paths.iter()
    }

    /// Resolves a path the model wrote to the canonical path in the diff.
    ///
    /// Tolerates the shapes a model produces that are not wrong, only untidy: a
    /// leading `./` or `/`, backslashes from a
    /// Windows-shaped example, and surrounding whitespace or backticks.
    #[must_use]
    pub fn resolve(&self, raw: &str) -> Option<String> {
        let cleaned = tidy_path(raw);
        // A path the diff knows exactly, after tidying only.
        if self.paths.contains(&cleaned) {
            return Some(cleaned);
        }
        if let Some(canonical) = unique_alias(&self.aliases, &cleaned) {
            return Some(canonical);
        }
        // Interpret `a/` as a patch-header prefix only after trying it literally: an
        // exact source path can itself begin with `a/`.
        if let Some(without_patch_prefix) = cleaned.strip_prefix("a/") {
            if self.paths.contains(without_patch_prefix) {
                return Some(without_patch_prefix.to_owned());
            }
            return unique_alias(&self.aliases, without_patch_prefix);
        }
        None
    }

    /// Whether a coordinate exists on a changed line or its visible context.
    #[must_use]
    pub fn contains_line(&self, path: &str, side: Side, line: u32) -> bool {
        self.lines.get(path).is_some_and(|(old, new)| match side {
            Side::Old => old.contains(&line),
            Side::New => new.contains(&line),
        })
    }
}

fn unique_alias(aliases: &BTreeMap<String, BTreeSet<String>>, path: &str) -> Option<String> {
    let candidates = aliases.get(path)?;
    (candidates.len() == 1)
        .then(|| candidates.iter().next().cloned())
        .flatten()
}

/// Tidies a path for matching, without inventing one.
#[must_use]
pub fn tidy_path(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .replace('\\', "/");
    let without_prefix = trimmed
        .strip_prefix("./")
        .or_else(|| trimmed.strip_prefix("/"))
        .unwrap_or(&trimmed);
    without_prefix.trim_end_matches('/').to_owned()
}

/// What could be read from an answer that has not finished streaming (FR-4.4).
///
/// The model streams its JSON object field by field, so the interface shows the
/// fields that have arrived whole and simply omits the rest, instead of painting the
/// raw JSON as it accumulates. It is a display convenience: the completed answer is
/// parsed and normalized the moment the stream ends, and that is what is stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    /// What changed, in the model's words (may be cut mid-sentence).
    pub summary: String,
    /// Why, inferred (may be cut mid-sentence).
    pub intent: String,
    /// The risk areas that have streamed in whole.
    pub risks: Vec<PreviewRisk>,
    /// The plan steps that have streamed in whole.
    pub plan: Vec<PreviewPlan>,
    /// The questions that have streamed in whole.
    pub questions: Vec<String>,
}

impl Preview {
    /// Whether nothing usable has arrived yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.summary.is_empty()
            && self.intent.is_empty()
            && self.risks.is_empty()
            && self.plan.is_empty()
            && self.questions.is_empty()
    }
}

/// One risk the model has finished describing, before path normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRisk {
    pub title: String,
    pub severity: Severity,
    pub why: String,
}

/// One plan step the model has finished describing, before path normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewPlan {
    pub group: String,
    pub rationale: String,
}

/// Reads whatever fields a not-yet-complete answer already contains (FR-4.4).
///
/// Tolerant of every truncation shape: a string cut mid-word, an array or object cut
/// mid-entry, a value cut before it started, or a key cut before its colon. A field
/// appears only once it is whole; the moment its closing token streams in it is picked
/// up on the next frame.
#[must_use]
pub fn preview(text: &str) -> Preview {
    let Some(object) = repair_object(text) else {
        return Preview::default();
    };
    let risks = object
        .get("risk_areas")
        .and_then(serde_json::Value::as_array)
        .map(|entries| entries.iter().filter_map(preview_risk).collect())
        .unwrap_or_default();
    let plan = object
        .get("review_plan")
        .and_then(serde_json::Value::as_array)
        .map(|entries| entries.iter().filter_map(preview_plan).collect())
        .unwrap_or_default();
    let questions = object
        .get("suggested_questions")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Preview {
        summary: first_string_field(&object, &["brief", "summary"]),
        intent: first_string_field(&object, &["inferred_purpose", "intent"]),
        risks,
        plan,
        questions,
    }
}

/// A top-level string field, or the empty string when it has not arrived yet.
fn string_field(object: &serde_json::Value, key: &str) -> String {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_owned()
}

fn first_string_field(object: &serde_json::Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|key| string_field(object, key))
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

/// Rebuilds a balanced JSON object from a possibly-truncated answer.
///
/// Closes the string, array and object tokens the stream cut off, drops a key that was
/// cut before its colon, and fills a value that never started with `null` — so the
/// fields that did arrive whole can be read. The result is only used for display; the
/// completed answer is parsed and normalized separately.
fn repair_object(text: &str) -> Option<serde_json::Value> {
    let start = text.find('{')?;
    let mut out = String::new();
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut string_start = 0_usize;
    let mut string_is_key = false;
    let mut last_significant = '\0';

    for ch in text[start..].chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            out.push(ch);
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                string_is_key = last_significant == '{'
                    || (last_significant == ',' && stack.last() == Some(&'}'));
                string_start = out.len();
                out.push(ch);
            }
            '{' => {
                stack.push('}');
                last_significant = '{';
                out.push(ch);
            }
            '[' => {
                stack.push(']');
                last_significant = '[';
                out.push(ch);
            }
            '}' | ']' => {
                if stack.last() == Some(&ch) {
                    stack.pop();
                }
                last_significant = ch;
                out.push(ch);
            }
            ':' => {
                last_significant = ':';
                out.push(ch);
            }
            ',' => {
                last_significant = ',';
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }

    // An incomplete escape at the very end is dropped.
    if escaped {
        out.pop();
    }
    if in_string {
        if string_is_key {
            // A key cut before its colon: drop it, and the comma that preceded it.
            out.truncate(string_start);
            while out.chars().next_back().is_some_and(char::is_whitespace) {
                out.pop();
            }
            if out.ends_with(',') {
                out.pop();
            }
        } else {
            out.push('"');
        }
    }

    // A value that never arrived, or an entry that never began.
    while out.chars().next_back().is_some_and(char::is_whitespace) {
        out.pop();
    }
    if out.ends_with(':') {
        out.push_str("null");
    } else if out.ends_with(',') {
        out.pop();
    }

    while let Some(close) = stack.pop() {
        out.push(close);
    }
    serde_json::from_str(&out).ok()
}

/// A complete risk area, or nothing when the entry is still missing its title.
fn preview_risk(value: &serde_json::Value) -> Option<PreviewRisk> {
    let title = value.get("title")?.as_str()?.trim().to_owned();
    if title.is_empty() {
        return None;
    }
    let severity = match value
        .get("severity")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
    {
        Some("high") => Severity::High,
        Some("low") => Severity::Low,
        _ => Severity::Medium,
    };
    Some(PreviewRisk {
        title,
        severity,
        why: value
            .get("why")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned(),
    })
}

/// A complete plan step, or nothing when the entry is still missing its group.
fn preview_plan(value: &serde_json::Value) -> Option<PreviewPlan> {
    let group = value.get("group")?.as_str()?.trim().to_owned();
    if group.is_empty() {
        return None;
    }
    Some(PreviewPlan {
        group,
        rationale: value
            .get("rationale")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned(),
    })
}

/// Parses the model's answer and normalizes it against the diff.
///
/// # Errors
///
/// Returns [`ParseFailure`] when no JSON object can be found, or when the object holds
/// nothing usable at all. Anything narrower than that — a missing field, a mistyped
/// entry, a file that is not in the diff — is a warning on the returned document.
pub fn normalize(
    text: &str,
    index: &PathIndex,
    model: &str,
    head_sha: &str,
    created_at: &str,
    usage: AnalysisUsage,
) -> Result<Normalized, ParseFailure> {
    let mut warnings = Vec::new();
    let object = extract_object(text)?;

    let raw: RawAnalysis =
        serde_json::from_value(object.clone()).map_err(|error| ParseFailure {
            reason: format!("the JSON object did not match the analysis shape: {error}"),
        })?;

    let brief = raw.brief.unwrap_or_default().trim().to_owned();
    let inferred_purpose = raw.inferred_purpose.unwrap_or_default().trim().to_owned();
    if brief.is_empty() {
        warnings.push("the analysis stated no brief".to_owned());
    }
    if inferred_purpose.is_empty() {
        warnings.push("the analysis inferred no purpose".to_owned());
    }

    let risk_areas = normalize_risks(&raw.risk_areas, index, &mut warnings);
    let mut review_plan = normalize_plan(&raw.review_plan, index, &mut warnings);
    let per_file_notes = normalize_notes(&raw.per_file_notes, index, &mut warnings);
    let coverage = normalize_coverage(raw.coverage.as_ref(), index, &mut warnings);
    let suggested_questions = raw
        .suggested_questions
        .iter()
        .filter_map(|value| value.as_str())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();

    // The safety net below adds every unplaced file, so an object the model left
    // empty would otherwise be dressed up as a complete analysis of all of them. It
    // is checked before the net is cast, against what the model actually said.
    if brief.is_empty()
        && inferred_purpose.is_empty()
        && risk_areas.is_empty()
        && per_file_notes.is_empty()
        && review_plan.is_empty()
    {
        return Err(ParseFailure {
            reason: "the answer's JSON object said nothing: no brief, no inferred purpose, no \
                     risks, no file guidance and no review plan"
                .to_owned(),
        });
    }

    // Everything the plan did not mention still has to be reviewable, and this is the
    // step that guarantees it: a file the model forgot is appended rather than dropped
    // (FR-4.1).
    append_unclassified(&mut review_plan, index, &mut warnings);

    let analysis = Analysis {
        version: ANALYSIS_VERSION,
        prompt_version: PROMPT_VERSION,
        model: model.to_owned(),
        head_sha: head_sha.to_owned(),
        created_at: created_at.to_owned(),
        token_usage: usage,
        brief,
        inferred_purpose,
        risk_areas,
        review_plan,
        per_file_notes,
        suggested_questions,
        coverage,
    };
    Ok(Normalized { analysis, warnings })
}

/// Finds the JSON object in a model's answer.
///
/// Models fence JSON, prefix it with a sentence, or add a closing remark after it, and
/// none of that is a reason to fail. The first balanced object is taken, with string
/// literals and escapes respected so a brace inside a string does not end it early.
fn extract_object(text: &str) -> Result<serde_json::Value, ParseFailure> {
    let start = text.find('{').ok_or_else(|| ParseFailure {
        reason: "the answer contained no JSON object".to_owned(),
    })?;
    let bytes = text.as_bytes();
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut end = None;
    for (offset, byte) in bytes[start..].iter().enumerate() {
        let index = start + offset;
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(index + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.ok_or_else(|| ParseFailure {
        reason: "the JSON object in the answer was never closed (the reply was probably cut off)"
            .to_owned(),
    })?;
    serde_json::from_str(&text[start..end]).map_err(|error| ParseFailure {
        reason: format!("the answer's JSON object could not be read: {error}"),
    })
}

/// Normalizes the risk areas, dropping the ones that name nothing in the diff.
fn normalize_risks(
    raw: &[serde_json::Value],
    index: &PathIndex,
    warnings: &mut Vec<String>,
) -> Vec<RiskArea> {
    let mut out = Vec::new();
    for (position, value) in raw.iter().enumerate() {
        let Ok(risk) = serde_json::from_value::<RawRisk>(value.clone()) else {
            warnings.push(format!(
                "risk area {} was not in the expected shape and was dropped",
                position + 1
            ));
            continue;
        };
        let title = risk.title.unwrap_or_default().trim().to_owned();
        if title.is_empty() {
            warnings.push(format!("risk area {} had no title", position + 1));
            continue;
        }
        let mut files = Vec::new();
        for file in &risk.files {
            match file.as_str().and_then(|raw| index.resolve(raw)) {
                Some(path) => files.push(path),
                None => warnings.push(format!(
                    "risk area \"{title}\" named {file}, which is not in this change"
                )),
            }
        }
        let severity = match risk.severity.as_deref().map(str::trim) {
            Some("high") => Severity::High,
            Some("low") => Severity::Low,
            // Medium, a synonym the model sometimes reaches for, and no answer at all
            // are the same outcome: the middle.
            Some("medium" | "med") | None => Severity::Medium,
            Some(other) => {
                warnings.push(format!(
                    "risk area \"{title}\" asked for severity \"{other}\", which is not one of \
                     low, medium or high; it is shown as medium"
                ));
                Severity::Medium
            }
        };
        out.push(RiskArea {
            title,
            severity,
            supported: risk.files.is_empty() || !files.is_empty(),
            files,
            why: risk.why.unwrap_or_default().trim().to_owned(),
        });
    }
    // Most serious first: the panel is read top-down and the first line matters most.
    out.sort_by_key(|risk| std::cmp::Reverse(risk.severity));
    out
}

/// Normalizes the plan: keeps the model's order, merges duplicate names, and drops
/// files that are not in the diff.
fn normalize_plan(
    raw: &[serde_json::Value],
    index: &PathIndex,
    warnings: &mut Vec<String>,
) -> Vec<PlanGroup> {
    let mut groups: Vec<PlanGroup> = Vec::new();
    let mut positions: BTreeMap<String, usize> = BTreeMap::new();

    for (position, value) in raw.iter().enumerate() {
        let Ok(group) = serde_json::from_value::<RawGroup>(value.clone()) else {
            warnings.push(format!(
                "review plan step {} was not in the expected shape and was dropped",
                position + 1
            ));
            continue;
        };
        let name = group.group.unwrap_or_default().trim().to_owned();
        if name.is_empty() {
            warnings.push(format!(
                "review plan step {} had no group name",
                position + 1
            ));
            continue;
        }
        let order = group
            .order
            .as_ref()
            .and_then(serde_json::Value::as_u64)
            .and_then(|order| u32::try_from(order).ok())
            .unwrap_or_else(|| u32::try_from(position + 1).unwrap_or(u32::MAX));

        // Files first, so a group with no files left after normalization disappears
        // rather than showing an empty heading.
        let mut files = Vec::new();
        for file in &group.files {
            match file.as_str().and_then(|raw| index.resolve(raw)) {
                Some(path) => files.push(path),
                None => warnings.push(format!(
                    "the plan put {file} in \"{name}\", but it is not in this change"
                )),
            }
        }
        if files.is_empty() && !index.is_empty() {
            warnings.push(format!(
                "\"{name}\" named no file in this change and was dropped"
            ));
            continue;
        }

        let identity = name.to_lowercase();
        if let Some(existing) = positions.get(&identity).copied() {
            let group = &mut groups[existing];
            for file in files {
                if !group.files.contains(&file) {
                    group.files.push(file);
                }
            }
            continue;
        }
        positions.insert(identity, groups.len());
        groups.push(PlanGroup {
            order,
            group: name,
            rationale: group.rationale.unwrap_or_default().trim().to_owned(),
            files,
        });
    }

    // A file appears in exactly one group — the first the model put it in — because
    // the ordered view is meant to be a reading order, and the same file twice is the
    // same file read twice.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for group in &mut groups {
        group.files.retain(|file| seen.insert(file.clone()));
    }

    // The model's own `order` decides the sequence, ties broken by the order it wrote
    // them in; a plan whose numbers are all wrong is still usable.
    groups.sort_by_key(|group| group.order);
    groups.retain(|group| !group.files.is_empty());
    for (position, group) in groups.iter_mut().enumerate() {
        group.order = u32::try_from(position + 1).unwrap_or(u32::MAX);
    }
    groups
}

/// Normalizes the per-file notes, dropping notes about files that are not there.
fn normalize_notes(
    raw: &[serde_json::Value],
    index: &PathIndex,
    warnings: &mut Vec<String>,
) -> Vec<FileNote> {
    let mut out: Vec<FileNote> = Vec::new();
    for (position, value) in raw.iter().enumerate() {
        let Ok(note) = serde_json::from_value::<RawNote>(value.clone()) else {
            warnings.push(format!(
                "file note {} was not in the expected shape and was dropped",
                position + 1
            ));
            continue;
        };
        let Some(path) = note.path.as_deref().and_then(|raw| index.resolve(raw)) else {
            warnings.push(format!(
                "file note {} named {}, which is not in this change",
                position + 1,
                note.path.as_deref().unwrap_or("<no path>")
            ));
            continue;
        };
        let verify = note
            .verify
            .iter()
            .filter_map(|value| value.as_str())
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
            .collect();
        let evidence = normalize_evidence(&note.evidence, index, &path, warnings);
        let entry = FileNote {
            path: path.clone(),
            what_changed: note.what_changed.unwrap_or_default().trim().to_owned(),
            why: note.why.unwrap_or_default().trim().to_owned(),
            verify,
            evidence,
        };
        // A second note for the same file (the model sometimes repeats itself) is
        // merged rather than dropped, because both halves usually say something.
        if let Some(existing) = out.iter_mut().find(|entry| entry.path == path) {
            existing.what_changed = join_sentences(&existing.what_changed, &entry.what_changed);
            existing.why = join_sentences(&existing.why, &entry.why);
            for item in entry.verify {
                if !existing.verify.contains(&item) {
                    existing.verify.push(item);
                }
            }
            for evidence in entry.evidence {
                if !existing.evidence.contains(&evidence) {
                    existing.evidence.push(evidence);
                }
            }
            continue;
        }
        out.push(entry);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn normalize_evidence(
    raw: &[serde_json::Value],
    index: &PathIndex,
    note_path: &str,
    warnings: &mut Vec<String>,
) -> Vec<Evidence> {
    let mut out = Vec::new();
    for (position, value) in raw.iter().enumerate() {
        let Ok(reference) = serde_json::from_value::<RawEvidence>(value.clone()) else {
            warnings.push(format!(
                "evidence {} for {note_path} was not in the expected shape and was dropped",
                position + 1
            ));
            continue;
        };
        let Some(path) = reference
            .path
            .as_deref()
            .and_then(|path| index.resolve(path))
        else {
            warnings.push(format!(
                "evidence {} for {note_path} named {}, which is not in this change",
                position + 1,
                reference.path.as_deref().unwrap_or("<no path>")
            ));
            continue;
        };
        let side = reference.side.as_deref().and_then(Side::parse);
        let line = reference
            .line
            .as_ref()
            .and_then(serde_json::Value::as_u64)
            .and_then(|line| u32::try_from(line).ok());
        let coordinate = match (side, line) {
            (Some(side), Some(line)) if index.contains_line(&path, side, line) => {
                (Some(side), Some(line))
            }
            (None, None) => (None, None),
            _ => {
                warnings.push(format!(
                    "evidence {} for {note_path} named an unavailable coordinate in {path}; the file link remains available",
                    position + 1
                ));
                (None, None)
            }
        };
        let evidence = Evidence {
            path,
            side: coordinate.0,
            line: coordinate.1,
            label: reference.label.unwrap_or_default().trim().to_owned(),
        };
        if !out.contains(&evidence) {
            out.push(evidence);
        }
    }
    out
}

fn normalize_coverage(
    raw: Option<&RawCoverage>,
    index: &PathIndex,
    warnings: &mut Vec<String>,
) -> Coverage {
    let Some(raw) = raw else {
        warnings.push("the analysis stated no coverage or limitations".to_owned());
        return Coverage::default();
    };
    let analyzed_files = normalize_path_list(
        &raw.analyzed_files,
        index,
        "coverage analyzed_files",
        warnings,
    );
    let truncated_files = normalize_path_list(
        &raw.truncated_files,
        index,
        "coverage truncated_files",
        warnings,
    );
    let limitations = raw
        .limitations
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .collect();
    Coverage {
        analyzed_files,
        truncated_files,
        limitations,
    }
}

fn normalize_path_list(
    raw: &[serde_json::Value],
    index: &PathIndex,
    label: &str,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let mut paths = Vec::new();
    for value in raw {
        match value.as_str().and_then(|path| index.resolve(path)) {
            Some(path) if !paths.contains(&path) => paths.push(path),
            Some(_) => {}
            None => warnings.push(format!(
                "{label} named {value}, which is not in this change"
            )),
        }
    }
    paths
}

/// Joins two sentences with a space, keeping the first when the second is empty.
fn join_sentences(first: &str, second: &str) -> String {
    match (first.is_empty(), second.is_empty()) {
        (true, _) => second.to_owned(),
        (_, true) => first.to_owned(),
        _ => format!("{first} {second}"),
    }
}

/// Appends the files the plan never mentioned (FR-4.1).
///
/// This is the guarantee that makes the ordered view safe to use as the only view:
/// however incomplete the model's plan was, every changed file is in it.
fn append_unclassified(plan: &mut Vec<PlanGroup>, index: &PathIndex, warnings: &mut Vec<String>) {
    if index.is_empty() {
        return;
    }
    let placed: BTreeSet<&str> = plan
        .iter()
        .flat_map(|group| group.files.iter().map(String::as_str))
        .collect();
    let missing: Vec<String> = index
        .paths()
        .filter(|path| !placed.contains(path.as_str()))
        .cloned()
        .collect();
    if missing.is_empty() {
        return;
    }
    if let Some(group) = plan.iter_mut().find(|group| group.group == UNCLASSIFIED) {
        group.files.extend(missing.iter().cloned());
    } else {
        let order = u32::try_from(plan.len() + 1).unwrap_or(u32::MAX);
        plan.push(PlanGroup {
            order,
            group: UNCLASSIFIED.to_owned(),
            rationale: "the analysis did not place these files; they are read last, in path order"
                .to_owned(),
            files: missing.clone(),
        });
    }
    if let Some(group) = plan.iter_mut().find(|group| group.group == UNCLASSIFIED) {
        group.files.sort();
        group.files.dedup();
    }
    warnings.push(format!(
        "{} file(s) were not in the review plan and were added as \"{UNCLASSIFIED}\"",
        missing.len()
    ));
}

/// The prompt the analysis is asked with (§7.3).
///
/// Normative parts, from the requirements: role, output format, the grounding rules
/// and the repository conventions. The wording is free; the guarantees are not — the
/// prompt asks for JSON only, for claims grounded in the context, for the plan to
/// cover every file, and for the answer to be in the pull request's language.
#[must_use]
pub fn system_prompt(conventions: Option<&str>) -> String {
    let mut prompt = String::from(
        "You are a senior software engineer reviewing a pull request. You are precise, \
         concrete and brief, and you never invent code you were not shown.\n\
         \n\
         Answer with one JSON object and nothing else: no prose before it, no markdown \
         fence, no commentary after it. Its shape is:\n\
         {\n  \
         \"brief\": \"what changed, approximately two concise sentences\",\n  \
         \"inferred_purpose\": \"the goal and motivation you infer; do not copy the author's description\",\n  \
         \"risk_areas\": [{\"title\": \"...\", \"severity\": \"high|medium|low\", \
         \"files\": [\"path\"], \"why\": \"...\"}],\n  \
         \"review_plan\": [{\"order\": 1, \"group\": \"short human action-oriented step name\", \
         \"rationale\": \"one sentence explaining why this step comes now\", \"files\": [\"path\"]}],\n  \
         \"per_file_notes\": [{\"path\": \"...\", \"what_changed\": \"one short sentence\", \
         \"why\": \"one short inferred reason\", \"verify\": [\"up to two concrete checks\"], \
         \"evidence\": [{\"path\": \"...\", \"side\": \"old|new\", \"line\": 1, \
         \"label\": \"what this supports\"}]}],\n  \
         \"suggested_questions\": [\"...\"],\n  \
         \"coverage\": {\"analyzed_files\": [\"path\"], \"truncated_files\": [\"path\"], \
         \"limitations\": [\"...\"]}\n}\n\
         \n\
         Rules:\n\
         - Use only the file paths given to you, spelled exactly as they appear. A path \
         you were not given does not exist for you.\n\
         - Every file in the change must appear in exactly one review_plan step. Choose \
         a dependency and understanding sequence that fits this pull request. Contracts, \
         examples, design notes or migrations may lead when they establish intent; tests \
         may accompany the behavior they explain. Put generated or purely mechanical \
         changes later, but keep them visibly included.\n\
         - Keep the brief, inferred purpose, step rationale, what_changed and why fields \
         concise. Preserve useful detail in checks, evidence, risks and limitations.\n\
         - Give every per-file note up to two concrete verify items and cite only evidence \
         coordinates that appear in the supplied diff. Omit a coordinate when only the \
         file, not a specific line, supports the claim.\n\
         - Name a file whenever you assert something about code.\n\
         - Ground every claim in the provided context. If the context does not answer \
         something, say so in `suggested_questions` instead of guessing.\n\
         - Write in the language the pull request is written in.\n",
    );
    if let Some(conventions) = conventions.map(str::trim).filter(|text| !text.is_empty()) {
        prompt.push_str("\nThe repository's own conventions, which override your defaults:\n");
        prompt.push_str(conventions);
        prompt.push('\n');
    }
    prompt
}

/// The user message: the context bundle, then the instruction.
#[must_use]
pub fn user_prompt(bundle: &str) -> String {
    format!(
        "Here is the pull request.\n\n{bundle}\n\n\
         Analyse it and answer with the JSON object described in your instructions."
    )
}

/// The repair message, sent when the first answer could not be used (FR-4.1).
///
/// The model is shown its own answer and the reason it failed, because "invalid JSON"
/// alone makes a model repeat the same mistake. The instruction is deliberately narrow:
/// fix the shape, keep the content.
#[must_use]
pub fn repair_prompt(previous: &str, failure: &ParseFailure) -> String {
    let trimmed: String = previous.chars().take(MAX_RAW_BYTES).collect();
    format!(
        "Your previous answer could not be used: {reason}\n\
         \n\
         Answer again with only the JSON object described in your instructions, with no \
         prose and no markdown fence. Keep the analysis the same; fix its shape. The \
         paths must be exactly the ones you were given.\n\
         \n\
         Your previous answer was:\n{trimmed}",
        reason = failure.reason,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> PathIndex {
        PathIndex::from_paths(
            [
                "src/domain/invoice.rs",
                "src/domain/money.rs",
                "src/application/billing.rs",
                "tests/billing.rs",
            ]
            .map(str::to_owned),
        )
    }

    fn normalize_text(text: &str) -> Result<Normalized, ParseFailure> {
        normalize(
            text,
            &index(),
            "deepseek/deepseek-chat",
            "abc123",
            "2026-01-01T00:00:00Z",
            AnalysisUsage::default(),
        )
    }

    const GOOD: &str = r#"{
        "brief": "Billing now rounds half-up.",
        "inferred_purpose": "Fix a rounding bug reported by finance.",
        "risk_areas": [{"title": "Rounding", "severity": "high",
                        "files": ["src/domain/money.rs"], "why": "money arithmetic"}],
        "review_plan": [
            {"order": 2, "group": "application", "rationale": "callers",
             "files": ["src/application/billing.rs"]},
            {"order": 1, "group": "domain", "rationale": "rules first",
             "files": ["src/domain/money.rs", "src/domain/invoice.rs"]}
        ],
        "per_file_notes": [{"path": "src/domain/money.rs", "what_changed": "rounding",
                            "why": "check the sign", "verify": ["negative totals"],
                            "evidence": [{"path": "src/domain/money.rs",
                                          "label": "rounding implementation"}]}],
        "suggested_questions": ["Is the rounding rule documented?"],
        "coverage": {"analyzed_files": ["src/domain/money.rs", "src/domain/invoice.rs",
                                          "src/application/billing.rs"],
                     "truncated_files": [], "limitations": ["tests were not analyzed"]}
    }"#;

    #[test]
    fn a_good_answer_normalizes_into_the_document() {
        let normalized = normalize_text(GOOD).expect("parses");
        let analysis = normalized.analysis;
        assert_eq!(analysis.brief, "Billing now rounds half-up.");
        assert_eq!(analysis.model, "deepseek/deepseek-chat");
        assert_eq!(analysis.head_sha, "abc123");
        assert_eq!(analysis.prompt_version, PROMPT_VERSION);
        assert_eq!(analysis.version, ANALYSIS_VERSION);
        assert_eq!(analysis.risk_areas.len(), 1);
        assert_eq!(analysis.risk_areas[0].severity, Severity::High);
        assert_eq!(analysis.per_file_notes[0].verify, ["negative totals"]);
        assert_eq!(analysis.per_file_notes[0].evidence.len(), 1);
        assert_eq!(analysis.coverage.analyzed_files.len(), 3);
        // The only warning is the honest one: the fixture's plan never mentions the
        // test file, and saying so is the point of the safety net.
        assert_eq!(normalized.warnings.len(), 1, "{:?}", normalized.warnings);
        assert!(normalized.warnings[0].contains("unclassified"));
    }

    #[test]
    fn the_models_order_decides_the_plan_not_the_array_order() {
        let analysis = normalize_text(GOOD).expect("parses").analysis;
        let names: Vec<&str> = analysis
            .review_plan
            .iter()
            .map(|group| group.group.as_str())
            .collect();
        assert_eq!(names, ["domain", "application", "unclassified"]);
        // The numbers are renumbered from one so the panel never shows "2, 1".
        assert_eq!(analysis.review_plan[0].order, 1);
        assert_eq!(analysis.review_plan[1].order, 2);
        // `tests/billing.rs` was in no group, so it is appended rather than lost.
        assert_eq!(analysis.review_plan[2].files, ["tests/billing.rs"]);
    }

    #[test]
    fn a_fenced_answer_with_prose_around_it_is_read() {
        let text = format!(
            "Sure, here is the analysis:\n\n```json\n{GOOD}\n```\n\nLet me know if you want \
             more detail."
        );
        let analysis = normalize_text(&text).expect("parses").analysis;
        assert_eq!(analysis.brief, "Billing now rounds half-up.");
    }

    #[test]
    fn a_brace_inside_a_string_does_not_end_the_object() {
        let text = r#"{"summary": "added a function that returns {}", "intent": "x",
                       "per_file_notes": [{"path": "src/domain/money.rs", "notes": "a { b"}]}"#;
        let analysis = normalize_text(text).expect("parses").analysis;
        assert_eq!(analysis.brief, "added a function that returns {}");
        assert_eq!(analysis.per_file_notes[0].why, "a { b");
    }

    #[test]
    fn a_truncated_answer_fails_with_a_reason_that_says_so() {
        let text = r#"{"summary": "the reply was cut"#;
        let failure = normalize_text(text).expect_err("fails");
        assert!(
            failure.reason.contains("never closed"),
            "{}",
            failure.reason
        );
        // The reason is what the repair prompt is built from, so it has to be usable.
        let repair = repair_prompt(text, &failure);
        assert!(repair.contains("never closed"), "{repair}");
        assert!(repair.contains("only the JSON object"), "{repair}");
    }

    #[test]
    fn prose_without_json_fails_and_the_raw_text_is_still_available() {
        let failure = normalize_text("I could not analyse this pull request.").expect_err("fails");
        assert!(
            failure.reason.contains("no JSON object"),
            "{}",
            failure.reason
        );
    }

    #[test]
    fn an_empty_object_is_a_failure_not_an_analysis_of_every_file() {
        // The safety net appends files the plan forgot, which would turn "{}" into a
        // confident-looking analysis of the whole change. It is checked before that.
        let failure = normalize_text("{}").expect_err("fails");
        assert!(
            failure.reason.contains("said nothing"),
            "{}",
            failure.reason
        );
    }

    #[test]
    fn unknown_paths_are_dropped_with_a_warning() {
        let text = r#"{
            "summary": "s", "intent": "i",
            "risk_areas": [{"title": "R", "severity": "high", "files": ["src/nope.rs"],
                            "why": "w"}],
            "review_plan": [{"order": 1, "group": "domain", "rationale": "r",
                             "files": ["src/domain/money.rs", "src/ghost.rs"]}],
            "per_file_notes": [{"path": "docs/ghost.md", "notes": "n"}]
        }"#;
        let normalized = normalize_text(text).expect("parses");
        let analysis = normalized.analysis;
        assert_eq!(analysis.review_plan[0].files, ["src/domain/money.rs"]);
        assert!(analysis.risk_areas[0].files.is_empty());
        assert!(analysis.per_file_notes.is_empty());
        let warnings = normalized.warnings.join("\n");
        assert!(warnings.contains("src/ghost.rs"), "{warnings}");
        assert!(warnings.contains("src/nope.rs"), "{warnings}");
        assert!(warnings.contains("docs/ghost.md"), "{warnings}");
        // And the files the plan lost are still reviewable.
        assert!(
            analysis
                .review_plan
                .iter()
                .flat_map(|group| &group.files)
                .any(|file| file == "src/application/billing.rs"),
            "the dropped plan still covers every changed file"
        );
    }

    #[test]
    fn a_group_with_no_files_left_is_dropped_rather_than_shown_empty() {
        let text = r#"{"summary": "s", "intent": "i",
            "review_plan": [{"order": 1, "group": "docs", "rationale": "r",
                             "files": ["docs/ghost.md"]}]}"#;
        let normalized = normalize_text(text).expect("parses");
        assert!(
            !normalized
                .analysis
                .review_plan
                .iter()
                .any(|group| group.group == "docs")
        );
        assert!(
            normalized
                .warnings
                .iter()
                .any(|warning| warning.contains("named no file"))
        );
    }

    #[test]
    fn duplicate_group_names_merge_instead_of_appearing_twice() {
        let text = r#"{"summary": "s", "intent": "i",
            "review_plan": [
                {"order": 1, "group": "Domain", "rationale": "r", "files": ["src/domain/money.rs"]},
                {"order": 2, "group": "domain", "rationale": "again",
                 "files": ["src/domain/invoice.rs"]}]}"#;
        let analysis = normalize_text(text).expect("parses").analysis;
        let domains: Vec<&PlanGroup> = analysis
            .review_plan
            .iter()
            .filter(|group| group.group.eq_ignore_ascii_case("domain"))
            .collect();
        assert_eq!(domains.len(), 1, "{:?}", analysis.review_plan);
        assert_eq!(domains[0].files.len(), 2);
    }

    #[test]
    fn ir_16_contextual_steps_preserve_contract_first_and_paired_test_plans() {
        let text = r#"{"brief":"b","inferred_purpose":"p",
            "review_plan":[
              {"order":1,"group":"Understand the contract","rationale":"intent first",
               "files":["docs/contract.md"]},
              {"order":2,"group":"Change and verify billing","rationale":"behavior with proof",
               "files":["src/domain/money.rs","tests/billing.rs"]},
              {"order":3,"group":"Apply the migration","rationale":"data follows contract",
               "files":["src/application/billing.rs"]}],
            "coverage":{"analyzed_files":["docs/contract.md","src/domain/money.rs",
                                             "tests/billing.rs","src/application/billing.rs"],
                        "truncated_files":[],"limitations":[]}}"#;
        let index = PathIndex::from_paths(
            [
                "docs/contract.md",
                "src/domain/money.rs",
                "tests/billing.rs",
                "src/application/billing.rs",
            ]
            .map(str::to_owned),
        );
        let analysis = normalize(
            text,
            &index,
            "provider/model",
            "head",
            "2026-01-01T00:00:00Z",
            AnalysisUsage::default(),
        )
        .expect("normalizes")
        .analysis;
        assert_eq!(analysis.review_plan[0].group, "Understand the contract");
        assert_eq!(
            analysis.review_plan[1].files,
            ["src/domain/money.rs", "tests/billing.rs"]
        );
        assert_eq!(analysis.review_plan[2].group, "Apply the migration");
    }

    #[test]
    fn a_bad_severity_word_is_shown_as_medium_with_a_warning() {
        let text = r#"{"summary": "s", "intent": "i",
            "risk_areas": [{"title": "R", "severity": "catastrophic", "files": [], "why": "w"}],
            "per_file_notes": [{"path": "src/domain/money.rs", "notes": "n"}]}"#;
        let normalized = normalize_text(text).expect("parses");
        assert_eq!(normalized.analysis.risk_areas[0].severity, Severity::Medium);
        assert!(
            normalized
                .warnings
                .iter()
                .any(|warning| warning.contains("catastrophic")),
            "{:?}",
            normalized.warnings
        );
    }

    #[test]
    fn a_note_entry_that_is_not_an_object_is_dropped_with_a_warning() {
        // The shape a model produces when it runs two ideas together.
        let text = r#"{"summary": "s", "intent": "i",
            "review_plan": [{"order": 1, "group": "domain", "rationale": "r",
                             "files": ["src/domain/money.rs"]}],
            "per_file_notes": ["src/domain/money.rs: check the sign", 42]}"#;
        let normalized = normalize_text(text).expect("parses");
        assert!(normalized.analysis.per_file_notes.is_empty());
        assert_eq!(
            normalized
                .warnings
                .iter()
                .filter(|warning| warning.contains("not in the expected shape"))
                .count(),
            2
        );
    }

    #[test]
    fn most_serious_risk_comes_first() {
        let text = r#"{"summary": "s", "intent": "i",
            "risk_areas": [
                {"title": "small", "severity": "low", "files": [], "why": "w"},
                {"title": "big", "severity": "high", "files": [], "why": "w"},
                {"title": "middle", "severity": "medium", "files": [], "why": "w"}],
            "per_file_notes": [{"path": "src/domain/money.rs", "notes": "n"}]}"#;
        let analysis = normalize_text(text).expect("parses").analysis;
        let titles: Vec<&str> = analysis
            .risk_areas
            .iter()
            .map(|risk| risk.title.as_str())
            .collect();
        assert_eq!(titles, ["big", "middle", "small"]);
    }

    #[test]
    fn a_missing_summary_is_a_warning_not_a_failure() {
        let text = r#"{"intent": "i", "per_file_notes": [{"path": "src/domain/money.rs",
            "notes": "n"}]}"#;
        let normalized = normalize_text(text).expect("parses");
        assert!(normalized.analysis.brief.is_empty());
        assert!(
            normalized
                .warnings
                .iter()
                .any(|warning| warning.contains("no brief"))
        );
    }

    #[test]
    fn one_plan_covers_a_file_the_model_repeated() {
        let text = r#"{"summary": "s", "intent": "i",
            "review_plan": [
                {"order": 1, "group": "domain", "rationale": "r",
                 "files": ["src/domain/money.rs"]},
                {"order": 2, "group": "application", "rationale": "r",
                 "files": ["src/domain/money.rs", "src/application/billing.rs"]}]}"#;
        let analysis = normalize_text(text).expect("parses").analysis;
        let placing: Vec<&str> = analysis
            .review_plan
            .iter()
            .filter(|group| group.files.iter().any(|file| file == "src/domain/money.rs"))
            .map(|group| group.group.as_str())
            .collect();
        // It stays where the model put it first, and the reader is never sent to the
        // same file twice.
        assert_eq!(placing, ["domain"]);
        assert!(
            !analysis.review_plan[1]
                .files
                .iter()
                .any(|file| file == "src/domain/money.rs")
        );
    }

    #[test]
    fn paths_are_matched_despite_the_shapes_models_write_them_in() {
        let text = r#"{"summary": "s", "intent": "i",
            "review_plan": [{"order": 1, "group": "domain", "rationale": "r",
                             "files": ["./src/domain/money.rs", "a/src/domain/invoice.rs",
                                       "/src/domain/money.rs", "`src/domain/money.rs`"]}],
            "per_file_notes": [{"path": "src\\domain\\money.rs", "notes": "n"}]}"#;
        let normalized = normalize_text(text).expect("parses");
        assert_eq!(
            normalized.analysis.review_plan[0].files,
            ["src/domain/money.rs", "src/domain/invoice.rs"]
        );
        assert_eq!(normalized.analysis.per_file_notes.len(), 1);
        assert_eq!(
            normalized.analysis.per_file_notes[0].path,
            "src/domain/money.rs"
        );
    }

    #[test]
    fn a_renamed_file_is_matched_by_either_name() {
        let patch = crate::domain::diff::parse_patch(
            "diff --git a/src/old.rs b/src/new.rs\n\
             similarity index 90%\n\
             rename from src/old.rs\n\
             rename to src/new.rs\n\
             --- a/src/old.rs\n\
             +++ b/src/new.rs\n\
             @@ -1 +1 @@\n\
             -a\n\
             +b\n",
        );
        let index = PathIndex::from_patch(&patch);
        assert_eq!(index.resolve("src/old.rs").as_deref(), Some("src/new.rs"));
        assert_eq!(index.resolve("src/new.rs").as_deref(), Some("src/new.rs"));
        // The bare name resolves when it is unambiguous.
        assert_eq!(index.resolve("new.rs").as_deref(), Some("src/new.rs"));
    }

    #[test]
    fn a_bare_name_is_not_guessed_when_two_files_share_it() {
        let index = PathIndex::from_paths(["src/a/mod.rs", "src/b/mod.rs"].map(str::to_owned));
        assert_eq!(index.resolve("mod.rs"), None);
        assert_eq!(
            index.resolve("src/b/mod.rs").as_deref(),
            Some("src/b/mod.rs")
        );
    }

    #[test]
    fn ir_12_path_resolution_keeps_exact_a_prefixes_and_rejects_duplicate_basenames() {
        let patch = crate::domain::diff::parse_patch(
            "diff --git a/a/config.rs b/a/config.rs\n--- a/a/config.rs\n+++ b/a/config.rs\n@@ -1 +1 @@\n-old\n+new\n\
             diff --git a/src/config.rs b/src/config.rs\n--- a/src/config.rs\n+++ b/src/config.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let index = PathIndex::from_patch(&patch);

        assert_eq!(index.resolve("a/config.rs").as_deref(), Some("a/config.rs"));
        assert_eq!(index.resolve("config.rs"), None);
    }

    #[test]
    fn the_system_prompt_states_the_schema_and_the_grounding_rules() {
        let prompt = system_prompt(Some("Always use thiserror for errors."));
        for required in [
            "one JSON object",
            "brief",
            "inferred_purpose",
            "review_plan",
            "per_file_notes",
            "evidence",
            "coverage",
            "suggested_questions",
            "spelled exactly as they appear",
            "Graund every claim",
        ] {
            let needle = required.replace("Graund", "Ground");
            assert!(prompt.contains(&needle), "missing {needle:?} in {prompt}");
        }
        assert!(prompt.contains("Always use thiserror"), "{prompt}");
        assert!(prompt.contains("tests may accompany"), "{prompt}");
        assert!(
            !prompt.contains("tests and documentation come last"),
            "{prompt}"
        );
        // Without conventions there is no dangling heading.
        assert!(!system_prompt(None).contains("repository's own conventions"));
    }

    #[test]
    fn ir_16_evidence_coordinates_are_validated_without_losing_the_file_link() {
        let patch = crate::domain::diff::parse_patch(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let index = PathIndex::from_patch(&patch);
        let normalized = normalize(
            r#"{"brief":"b","inferred_purpose":"p",
                "per_file_notes":[{"path":"src/a.rs","what_changed":"w","why":"y",
                  "verify":["one","two","retained detail"],
                  "evidence":[{"path":"src/a.rs","side":"new","line":1,"label":"valid"},
                              {"path":"src/a.rs","side":"new","line":99,"label":"bad line"},
                              {"path":"src/missing.rs","label":"bad path"}]}],
                "coverage":{"analyzed_files":["src/a.rs"],"truncated_files":[],"limitations":[]}}"#,
            &index,
            "provider/model",
            "head",
            "2026-01-01T00:00:00Z",
            AnalysisUsage::default(),
        )
        .expect("normalizes");
        let note = &normalized.analysis.per_file_notes[0];
        assert_eq!(note.verify.len(), 3, "useful excess detail is retained");
        assert_eq!(note.evidence.len(), 2, "the unknown file is dropped");
        assert_eq!(note.evidence[0].line, Some(1));
        assert_eq!(
            note.evidence[1].line, None,
            "invalid coordinates degrade to a file link"
        );
        assert!(
            normalized
                .warnings
                .iter()
                .any(|warning| warning.contains("unavailable coordinate"))
        );
        assert!(
            normalized
                .warnings
                .iter()
                .any(|warning| warning.contains("not in this change"))
        );
    }

    #[test]
    fn ir_16_legacy_fields_remain_readable_but_the_prompt_version_invalidates_the_cache() {
        let analysis = normalize_text(
            r#"{"summary":"legacy brief","intent":"legacy purpose",
                "per_file_notes":[{"path":"src/domain/money.rs","change":"changed",
                                   "notes":"reason","review_focus":["check"]}]}"#,
        )
        .expect("legacy aliases normalize")
        .analysis;
        assert_eq!(analysis.brief, "legacy brief");
        assert_eq!(analysis.per_file_notes[0].verify, ["check"]);
        assert_eq!(analysis.prompt_version, 2);
    }

    #[test]
    fn the_repair_prompt_carries_the_reason_and_the_previous_answer() {
        let failure = ParseFailure {
            reason: "the answer's JSON object could not be read: expected value".to_owned(),
        };
        let repair = repair_prompt("{\"summary\": ", &failure);
        assert!(repair.contains("expected value"), "{repair}");
        assert!(repair.contains("{\"summary\": "), "{repair}");
        assert!(repair.contains("Keep the analysis the same"), "{repair}");
    }

    #[test]
    fn the_repair_prompt_is_bounded_even_when_the_answer_was_huge() {
        let huge = "x".repeat(MAX_RAW_BYTES * 2);
        let failure = ParseFailure {
            reason: "r".to_owned(),
        };
        let repair = repair_prompt(&huge, &failure);
        assert!(
            repair.len() < MAX_RAW_BYTES + 1024,
            "{} bytes",
            repair.len()
        );
    }

    #[test]
    fn the_document_round_trips_through_json() {
        let analysis = normalize_text(GOOD).expect("parses").analysis;
        let text = serde_json::to_string(&analysis).expect("serialises");
        let back: Analysis = serde_json::from_str(&text).expect("deserialises");
        assert_eq!(back, analysis);
    }

    #[test]
    fn an_unknown_field_in_the_json_is_ignored() {
        let text = r#"{"summary": "s", "intent": "i", "future_field": {"a": 1},
            "review_plan": [{"order": 1, "group": "domain", "rationale": "r",
                             "files": ["src/domain/money.rs"], "confidence": 0.9}]}"#;
        let analysis = normalize_text(text).expect("parses").analysis;
        assert_eq!(analysis.brief, "s");
    }

    #[test]
    fn provenance_names_the_model_and_the_prompt_version() {
        let analysis = normalize_text(GOOD).expect("parses").analysis;
        let label = analysis.provenance(chrono::Utc::now());
        assert!(label.contains("deepseek/deepseek-chat"), "{label}");
        assert!(label.contains("prompt v2"), "{label}");
        assert!(
            label.split(" · ").count() == 3,
            "model, version and age: {label}"
        );
    }

    #[test]
    fn a_document_from_an_older_prompt_is_recognisable_as_such() {
        let mut analysis = normalize_text(GOOD).expect("parses").analysis;
        assert!(analysis.matches_prompt());
        analysis.prompt_version = PROMPT_VERSION + 1;
        assert!(!analysis.matches_prompt());
    }

    #[test]
    fn a_plan_that_names_nothing_leaves_every_file_in_one_group() {
        let text = r#"{"summary": "s", "intent": "i", "review_plan": []}"#;
        let analysis = normalize_text(text).expect("parses").analysis;
        assert_eq!(analysis.review_plan.len(), 1);
        assert_eq!(analysis.review_plan[0].group, UNCLASSIFIED);
        assert_eq!(analysis.review_plan[0].files.len(), index().len());
    }

    #[test]
    fn a_summary_is_read_before_the_object_is_closed() {
        let preview = preview(r#"{"summary": "Billing now rounds half-up.", "int"#);
        assert_eq!(preview.summary, "Billing now rounds half-up.");
        assert!(preview.intent.is_empty());
        assert!(!preview.is_empty());
    }

    #[test]
    fn a_value_cut_mid_word_is_shown_as_it_arrived() {
        assert_eq!(
            preview(r#"{"summary": "Money now ro"#).summary,
            "Money now ro"
        );
    }

    #[test]
    fn a_truncated_risk_array_shows_what_arrived() {
        let preview = preview(
            r#"{"risk_areas": [{"title": "Rounding", "severity": "high", "why": "money"},
                               {"title": "Par"#,
        );
        // The first entry is whole; the second shows its title as it streams in.
        assert_eq!(preview.risks.len(), 2);
        assert_eq!(preview.risks[0].title, "Rounding");
        assert_eq!(preview.risks[0].severity, Severity::High);
        assert_eq!(preview.risks[1].title, "Par");
        assert_eq!(preview.risks[1].severity, Severity::Medium);
    }

    #[test]
    fn a_key_cut_before_its_colon_is_dropped_not_a_failure() {
        let preview = preview(r#"{"summary": "done", "int"#);
        assert_eq!(preview.summary, "done");
        assert!(preview.intent.is_empty());
    }

    #[test]
    fn a_value_that_never_started_is_not_a_failure() {
        let preview = preview(r#"{"summary": "done", "intent":"#);
        assert_eq!(preview.summary, "done");
        assert!(preview.intent.is_empty());
    }

    #[test]
    fn a_complete_object_previews_everything() {
        let preview = preview(
            r#"{"summary": "s", "intent": "i",
                "risk_areas": [{"title": "R", "severity": "low", "why": "w"}],
                "review_plan": [{"group": "domain", "rationale": "r"}],
                "suggested_questions": ["q?"]}"#,
        );
        assert_eq!(preview.summary, "s");
        assert_eq!(preview.intent, "i");
        assert_eq!(preview.risks.len(), 1);
        assert_eq!(preview.risks[0].severity, Severity::Low);
        assert_eq!(preview.plan.len(), 1);
        assert_eq!(preview.plan[0].group, "domain");
        assert_eq!(preview.questions, ["q?"]);
    }

    #[test]
    fn prose_without_json_yields_an_empty_preview() {
        assert!(preview("I could not analyse this pull request.").is_empty());
    }
}
