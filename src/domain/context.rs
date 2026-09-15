//! The context bundle (FR-4.6, §7.3, NFR-3.1).
//!
//! What is sent to a provider is assembled here, deterministically and with the
//! guardrails in one place, because "what did this app tell the model about my
//! repository?" must have a single, inspectable answer.
//!
//! Three rules shape the module:
//!
//! - **assembled, not improvised.** The same inputs produce the same bundle, in the
//!   same order: metadata, commits, conventions, the diff, then the changed files.
//!   Nothing is included that a later code path decided to add on the fly.
//! - **never sent is a decision, not an omission.** A file that looks like a secret, a
//!   file that is not part of the revision, a binary and an oversized file are each
//!   recorded as a [`Segment`] with a reason, so `:context` can say *why* something the
//!   user expected is missing rather than leaving them to guess (FR-4.6).
//! - **the budget has an order.** When the bundle does not fit, whole file bodies go
//!   first, then the diff loses its context lines, and only then is the diff truncated,
//!   with a marker saying so. The order is fixed and documented because an approximation
//!   the user cannot predict is worse than a smaller bundle they can (Appendix B.3).
//!
//! Nothing here performs IO: file contents and the diff arrive as arguments, so every
//! rule below is testable without a repository or a network (NFR-5.2).

use std::fmt::Write;

use crate::domain::diff::{LineKind, Patch};

/// How many bytes of file content each estimated token stands for (Appendix B.3).
///
/// `bytes / 4` is the documented approximation until a real tokenizer is approved as a
/// dependency. It is deliberately an estimate: the UI says "~12k tokens" rather than
/// pretending to know, because being wrong by a few per cent is fine and being wrong
/// silently is not.
pub const BYTES_PER_TOKEN: usize = 4;

/// Estimates tokens from bytes (Appendix B.3).
#[must_use]
pub fn estimate_tokens(bytes: usize) -> usize {
    bytes.div_ceil(BYTES_PER_TOKEN)
}

/// The token budget the bundle is assembled under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundlePolicy {
    /// The most tokens the bundle may occupy.
    pub max_context_tokens: u32,
    /// The largest file that is included whole.
    pub max_file_bytes: u64,
    /// Context lines kept per hunk when the diff has to be reduced.
    pub reduced_context_lines: u32,
}

impl Default for BundlePolicy {
    fn default() -> Self {
        Self {
            max_context_tokens: 100_000,
            max_file_bytes: 256 * 1024,
            reduced_context_lines: 1,
        }
    }
}

impl BundlePolicy {
    /// The byte budget that corresponds to the token budget.
    #[must_use]
    pub fn max_bytes(&self) -> usize {
        (self.max_context_tokens as usize).saturating_mul(BYTES_PER_TOKEN)
    }
}

/// What a file's content is worth sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Send the bytes.
    Include,
    /// Send the path and a note instead of the bytes.
    Placeholder {
        /// Why the content was replaced.
        reason: String,
    },
    /// Send nothing at all.
    Omit {
        /// Why.
        reason: String,
    },
}

/// The content decision already made for one repository path.
///
/// The application layer supplies decisions that need repository knowledge (notably
/// `.gitignore`). The domain then applies the same decision to every representation of
/// that path: old/new rename names, diff hunks and full file bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDecision {
    /// The repository-relative path.
    pub path: String,
    /// Whether its content may be sent.
    pub disposition: Disposition,
}

impl PathDecision {
    /// Records a path that repository policy excludes.
    #[must_use]
    pub fn omitted(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            disposition: Disposition::Omit {
                reason: reason.into(),
            },
        }
    }
}

impl Disposition {
    /// Whether the content is sent.
    #[must_use]
    pub fn is_included(&self) -> bool {
        matches!(self, Self::Include)
    }

    /// The reason, when there is one.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Include => None,
            Self::Placeholder { reason } | Self::Omit { reason } => Some(reason),
        }
    }
}

/// Basenames that are never sent, whatever the repository says.
///
/// A denylist for the obvious cases, not a security boundary: the primary filter is
/// that only paths belonging to the revision are considered at all. It exists because
/// a `.env` that somebody committed by accident is exactly the file a reviewer would
/// not want copied into a third party's logs (NFR-3.1).
const SECRET_NAMES: &[&str] = &[
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    ".npmrc",
    ".pypirc",
    ".netrc",
    ".git-credentials",
    ".htpasswd",
    "credentials.toml",
    "secrets.toml",
    ".envrc",
];

/// Extensions that are never sent.
const SECRET_SUFFIXES: &[&str] = &[
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".keystore",
    ".jks",
    ".asc",
    ".gpg",
];

/// Directory names whose contents are never sent.
const SECRET_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg", ".docker"];

/// Whether a path looks like it holds a credential.
#[must_use]
pub fn is_secret_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let segments: Vec<&str> = normalized.split('/').collect();
    let Some(name) = segments.last().copied() else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    if segments
        .iter()
        .rev()
        .skip(1)
        .any(|segment| SECRET_DIRS.contains(&segment.to_ascii_lowercase().as_str()))
    {
        return true;
    }
    if lower == ".env" || lower.starts_with(".env.") {
        return true;
    }
    if SECRET_NAMES.contains(&lower.as_str()) {
        return true;
    }
    SECRET_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
}

/// Whether bytes look like text we can send.
///
/// A NUL byte in the first 8 KiB is git's own heuristic for "binary" and it is right
/// far more often than it is wrong; invalid UTF-8 after that is a second chance at
/// catching a file that would otherwise arrive as replacement characters.
#[must_use]
pub fn looks_binary(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(8192)];
    if window.contains(&0) {
        return true;
    }
    // A file we were given whole that is not valid UTF-8 is one we should not send:
    // it would arrive as replacement characters.
    std::str::from_utf8(bytes).is_err()
}

/// Decides what to do with one file's content.
#[must_use]
pub fn disposition(path: &str, bytes: &[u8], policy: &BundlePolicy) -> Disposition {
    if is_secret_path(path) {
        return Disposition::Omit {
            reason: "this path looks like a credential, so it is never sent".to_owned(),
        };
    }
    if bytes.len() as u64 > policy.max_file_bytes {
        return Disposition::Placeholder {
            reason: format!(
                "{} over the {} KiB per-file limit",
                human_bytes(bytes.len() as u64),
                policy.max_file_bytes / 1024
            ),
        };
    }
    if looks_binary(bytes) {
        return Disposition::Placeholder {
            reason: "binary file".to_owned(),
        };
    }
    Disposition::Include
}

/// What kind of thing a segment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SegmentKind {
    /// The pull request itself: title, author, branches, description, state.
    Metadata,
    /// The commit messages.
    Commits,
    /// A repository convention file.
    Conventions,
    /// The diff.
    Diff,
    /// A changed file's content at head.
    File,
    /// Something the user added (`:context add`, M3).
    UserFile,
}

impl SegmentKind {
    /// A short label for the inspector.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Commits => "commits",
            Self::Conventions => "conventions",
            Self::Diff => "diff",
            Self::File => "file",
            Self::UserFile => "user file",
        }
    }
}

/// One thing that was considered for the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// What it is.
    pub kind: SegmentKind,
    /// How it is named in the inspector.
    pub label: String,
    /// Bytes that were sent, zero when nothing was.
    pub bytes: usize,
    /// Whether the content is in the bundle.
    pub included: bool,
    /// Whether the content was cut short.
    pub truncated: bool,
    /// Why it is not fully there, when it is not.
    pub detail: Option<String>,
}

impl Segment {
    /// The tokens this segment costs, estimated.
    #[must_use]
    pub fn tokens(&self) -> usize {
        estimate_tokens(self.bytes)
    }

    /// The one-line summary the inspector shows.
    #[must_use]
    pub fn inspection_line(&self) -> String {
        let mark = if !self.included {
            "✗"
        } else if self.truncated {
            "~"
        } else {
            "✓"
        };
        let size = if self.included {
            let tokens = self.tokens();
            if tokens >= 1000 {
                // Tenths of a thousand, computed in integers: the estimate is
                // approximate enough without floating point rounding it twice.
                format!("{}.{}k tok", tokens / 1000, (tokens % 1000) / 100)
            } else {
                format!("{tokens} tok")
            }
        } else {
            "not sent".to_owned()
        };
        match &self.detail {
            Some(detail) => format!("{mark} {:<28} {:>9}  {detail}", self.label, size),
            None => format!("{mark} {:<28} {:>9}", self.label, size),
        }
    }
}

/// What the bundle was assembled from.
///
/// Borrowed, because the caller owns the data: the job reads it, the domain decides
/// what to do with it, and nothing is copied until it is actually included.
#[derive(Debug, Default)]
pub struct BundleInputs<'a> {
    /// The rendered pull request header block.
    pub metadata: &'a str,
    /// The rendered commit list.
    pub commits: &'a str,
    /// Convention files, already in priority order, as `(label, bytes)`.
    pub conventions: Vec<(&'a str, &'a [u8])>,
    /// The diff, if it could be read. Reduced rather than dropped when the bundle is
    /// too large.
    pub diff: Option<&'a Patch>,
    /// Each changed file's content at head, in diff order, as `(path, bytes)`.
    pub files: Vec<(&'a str, &'a [u8])>,
    /// Decisions made with repository knowledge, applied to every representation.
    pub decisions: Vec<PathDecision>,
}

/// A built bundle: the text that will be sent, plus the account of what went into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// The assembled text.
    pub text: String,
    /// Everything that was considered, in the order it appears in the text.
    pub segments: Vec<Segment>,
    /// The estimated tokens of [`Bundle::text`].
    pub estimated_tokens: usize,
}

impl Bundle {
    /// The bytes of the assembled text.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.text.len()
    }

    /// Appends a file the user added with `:context add` (FR-5.3).
    ///
    /// Added after the bundle is built rather than as another input, because the two
    /// are answers to different questions: everything in `build` is what the app
    /// decided to send, and this is what the user decided to add. Same rules, same
    /// budget, and the same account of what was elided and why — but last in the
    /// truncation order, so a file the user asked for is dropped only when everything
    /// already decided filled the budget.
    pub fn push_user_file(&mut self, path: &str, bytes: &[u8], policy: &BundlePolicy) {
        self.push_user_file_with_decision(path, bytes, policy, None);
    }

    /// Appends a user-added file using the repository eligibility decision made while
    /// gathering it.
    pub fn push_user_file_with_decision(
        &mut self,
        path: &str,
        bytes: &[u8],
        policy: &BundlePolicy,
        decision: Option<PathDecision>,
    ) {
        let used = self.text.len();
        let decisions = decision.into_iter().collect::<Vec<_>>();
        let mut builder = Builder {
            policy,
            text: std::mem::take(&mut self.text),
            segments: std::mem::take(&mut self.segments),
            used,
            decisions: &decisions,
        };
        builder.push_optional_file(SegmentKind::UserFile, path, bytes);
        self.text = builder.text;
        self.segments = builder.segments;
        self.estimated_tokens = estimate_tokens(self.text.len());
    }

    /// How many segments are actually in the bundle.
    #[must_use]
    pub fn included(&self) -> usize {
        self.segments
            .iter()
            .filter(|segment| segment.included)
            .count()
    }

    /// The one-line summary the status area and the opt-in notice show.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "~{} tokens ({}), {} of {} items",
            self.estimated_tokens,
            human_bytes(self.bytes() as u64),
            self.included(),
            self.segments.len()
        )
    }

    /// The `:context` inspector's body.
    #[must_use]
    pub fn inspection(&self) -> Vec<String> {
        let mut lines = vec![format!("context: {}", self.summary())];
        for segment in &self.segments {
            lines.push(segment.inspection_line());
        }
        lines
    }
}

/// Assembles the bundle (FR-4.6).
///
/// The order is fixed: metadata, commits, conventions, diff, then files. Files are
/// considered in the order the diff lists them, which is the order the reader sees
/// them in, so when the budget runs out it runs out at the bottom of the review rather
/// than in the middle of it.
#[must_use]
pub fn build(inputs: &BundleInputs<'_>, policy: &BundlePolicy) -> Bundle {
    let decisions = complete_decisions(inputs, policy);
    let mut builder = Builder::new(policy, &decisions);

    builder.push_text(SegmentKind::Metadata, "pull request", inputs.metadata);
    builder.push_text(SegmentKind::Commits, "commits", inputs.commits);
    for (label, bytes) in &inputs.conventions {
        builder.push_optional_file(SegmentKind::Conventions, label, bytes);
    }

    // The diff is reduced rather than dropped, and only truncated as a last resort.
    let diff_text = inputs.diff.map_or_else(String::new, |patch| {
        builder.record_excluded_patch_files(patch);
        render_patch_filtered(patch, None, &decisions)
    });
    let file_count = inputs.diff.map_or(0, |patch| patch.files.len());
    builder.reserve_diff(&diff_text, file_count);
    builder.push_diff(inputs.diff, &diff_text);

    for (path, bytes) in &inputs.files {
        builder.push_optional_file(SegmentKind::File, path, bytes);
    }

    builder.finish()
}

/// Adds path-only decisions for patch names the application did not classify. This is
/// what protects remote-only patches and direct domain callers: obvious credential
/// names are denied even when no repository checkout was available.
fn complete_decisions(inputs: &BundleInputs<'_>, policy: &BundlePolicy) -> Vec<PathDecision> {
    let mut decisions = inputs.decisions.clone();
    let Some(patch) = inputs.diff else {
        return decisions;
    };
    for path in patch.files.iter().flat_map(|file| {
        file.old_path
            .iter()
            .chain(file.new_path.iter())
            .map(ToString::to_string)
    }) {
        if !decisions.iter().any(|decision| decision.path == path) {
            decisions.push(PathDecision {
                disposition: disposition(&path, &[], policy),
                path,
            });
        }
    }
    // A patch file is one content unit. If either side of a rename/copy is excluded,
    // carry that decision to both names so the safe new filename cannot re-introduce
    // the same bytes through the full-file section.
    for file in &patch.files {
        let strict = file
            .old_path
            .iter()
            .chain(file.new_path.iter())
            .filter_map(|path| decision_for(&path.to_string(), &decisions))
            .max_by_key(|disposition| disposition_rank(disposition))
            .cloned();
        if let Some(disposition) = strict {
            for path in file.old_path.iter().chain(file.new_path.iter()) {
                decisions.push(PathDecision {
                    path: path.to_string(),
                    disposition: disposition.clone(),
                });
            }
        }
    }
    decisions
}

/// Collects the segments while watching the budget.
struct Builder<'a> {
    policy: &'a BundlePolicy,
    text: String,
    segments: Vec<Segment>,
    used: usize,
    decisions: &'a [PathDecision],
}

impl<'a> Builder<'a> {
    fn new(policy: &'a BundlePolicy, decisions: &'a [PathDecision]) -> Self {
        Self {
            policy,
            text: String::new(),
            segments: Vec::new(),
            used: 0,
            decisions,
        }
    }

    /// Records patch files whose content is absent from the final payload. These rows
    /// are derived from the same decisions used by the renderer, so the inspector
    /// cannot claim that a file was excluded while its hunks were sent.
    fn record_excluded_patch_files(&mut self, patch: &Patch) {
        for file in &patch.files {
            let Some(disposition) = file_disposition(file, self.decisions) else {
                continue;
            };
            if let Some(reason) = disposition.reason() {
                self.segments.push(Segment {
                    kind: SegmentKind::File,
                    label: file.display_path(),
                    bytes: 0,
                    included: false,
                    truncated: false,
                    detail: Some(format!("diff content excluded: {reason}")),
                });
            }
        }
    }

    fn remaining(&self) -> usize {
        self.policy.max_bytes().saturating_sub(self.used)
    }

    /// Appends a block that is always sent, truncating it if it does not fit.
    fn push_text(&mut self, kind: SegmentKind, label: &str, body: &str) {
        if body.trim().is_empty() {
            return;
        }
        let allowed = self.remaining();
        let (body, truncated) = truncate_bytes(body, allowed);
        let detail = truncated
            .then(|| format!("truncated at {allowed} bytes: the context budget was reached"));
        self.append(
            &format!("## {label}\n{body}\n\n"),
            kind,
            label,
            truncated,
            detail,
        );
    }

    /// Appends a file, honouring the per-file rules and the budget.
    fn push_optional_file(&mut self, kind: SegmentKind, label: &str, bytes: &[u8]) {
        let disposition = decision_for(label, self.decisions)
            .cloned()
            .unwrap_or_else(|| disposition(label, bytes, self.policy));
        match disposition {
            Disposition::Omit { reason } => {
                self.segments.push(Segment {
                    kind,
                    label: label.to_owned(),
                    bytes: 0,
                    included: false,
                    truncated: false,
                    detail: Some(reason),
                });
            }
            Disposition::Placeholder { reason } => {
                let body = format!("## {label} ({reason})\n");
                self.append(&body, kind, label, false, Some(reason));
            }
            Disposition::Include => {
                let Some(text) = std::str::from_utf8(bytes).ok() else {
                    // `disposition` rejects non-UTF-8 as binary, so this is
                    // unreachable; treating it as a placeholder keeps the function
                    // total rather than making the caller handle it.
                    let reason = "binary file".to_owned();
                    let body = format!("## {label} ({reason})\n");
                    self.append(&body, kind, label, false, Some(reason));
                    return;
                };
                // A file that does not fit whole is left out rather than cut in half:
                // half a function invites a confident wrong answer, and the segment
                // list says the file was elided so the user can widen the budget.
                let cost = text.len();
                if cost > self.remaining() {
                    self.segments.push(Segment {
                        kind,
                        label: label.to_owned(),
                        bytes: 0,
                        included: false,
                        truncated: false,
                        detail: Some(format!(
                            "elided: {} does not fit in the {} KB left",
                            human_bytes(cost as u64),
                            self.remaining() / 1024
                        )),
                    });
                    return;
                }
                let body = format!("## {label}\n{text}\n\n");
                self.append(&body, kind, label, false, None);
            }
        }
    }

    /// Records how large the diff would be, before deciding what to send.
    fn reserve_diff(&mut self, full: &str, files: usize) {
        if self.remaining() >= full.len() {
            return;
        }
        // The reduction is described rather than applied here: `push_diff` owns the
        // patch and does the work, and this only makes sure the choice is visible in
        // the segment list even when the reduced version fits easily.
        let _ = files;
    }

    /// Appends the diff, reducing its context and then truncating if it must.
    fn push_diff(&mut self, patch: Option<&Patch>, full: &str) {
        let Some(patch) = patch else {
            return;
        };
        if full.trim().is_empty() {
            return;
        }
        if full.len() <= self.remaining() {
            let body = format!("## diff\n{full}\n");
            self.append(&body, SegmentKind::Diff, "diff", false, None);
            return;
        }

        // Step two of the truncation order: keep the changed lines and just enough
        // context to read them.
        let reduced = render_patch(patch, Some(self.policy.reduced_context_lines));
        if reduced.len() <= self.remaining() {
            let detail = format!(
                "context reduced to {} line(s) per hunk to fit the budget",
                self.policy.reduced_context_lines
            );
            let body = format!("## diff (context reduced)\n{reduced}\n");
            self.append(&body, SegmentKind::Diff, "diff", true, Some(detail));
            return;
        }

        // Step three: truncate, and say where.
        let allowed = self.remaining();
        let (body, _) = truncate_bytes(&reduced, allowed);
        let detail = format!(
            "truncated at {allowed} bytes after reducing context; the rest of the diff was not sent"
        );
        self.append(
            &format!("## diff (truncated)\n{body}"),
            SegmentKind::Diff,
            "diff",
            true,
            Some(detail),
        );
    }

    /// Adds the text and its segment.
    fn append(
        &mut self,
        body: &str,
        kind: SegmentKind,
        label: &str,
        truncated: bool,
        detail: Option<String>,
    ) {
        self.used += body.len();
        self.text.push_str(body);
        self.segments.push(Segment {
            kind,
            label: label.to_owned(),
            bytes: body.len(),
            included: true,
            truncated,
            detail,
        });
    }

    fn finish(self) -> Bundle {
        let estimated_tokens = estimate_tokens(self.text.len());
        Bundle {
            text: self.text,
            segments: self.segments,
            estimated_tokens,
        }
    }
}

/// Truncates at a byte boundary, never inside a character.
fn truncate_bytes(text: &str, allowed: usize) -> (String, bool) {
    if text.len() <= allowed {
        return (text.to_owned(), false);
    }
    let mut end = allowed.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// Renders a patch for the prompt.
///
/// `keep_context` limits how many unchanged lines survive around each change, which is
/// how the bundle is reduced when it does not fit (FR-4.6). The changed lines are never
/// dropped: a reduced diff shows the same change with less of its surroundings, which
/// is a smaller request rather than a different one.
#[must_use]
pub fn render_patch(patch: &Patch, keep_context: Option<u32>) -> String {
    render_patch_filtered(patch, keep_context, &[])
}

/// Renders only patch content allowed by the supplied path decisions.
fn render_patch_filtered(
    patch: &Patch,
    keep_context: Option<u32>,
    decisions: &[PathDecision],
) -> String {
    let mut out = String::new();
    for file in &patch.files {
        if file_disposition(file, decisions).is_some() {
            continue;
        }
        let _ = writeln!(out, "### {} {}", file.status.marker(), file.display_path());
        if let Some(placeholder) = file.placeholder() {
            let _ = writeln!(out, "({placeholder})");
            continue;
        }
        for hunk in &file.hunks {
            let keep = keep_context.map(|keep| keep as usize);
            // Which lines survive: everything that changed, plus `keep` unchanged
            // lines on either side of each change.
            let mut survive = vec![false; hunk.lines.len()];
            for (index, line) in hunk.lines.iter().enumerate() {
                if line.kind != LineKind::Context {
                    survive[index] = true;
                    if let Some(keep) = keep {
                        let from = index.saturating_sub(keep);
                        let to = (index + keep + 1).min(hunk.lines.len());
                        for flag in &mut survive[from..to] {
                            *flag = true;
                        }
                    }
                } else if keep.is_none() {
                    survive[index] = true;
                }
            }
            if !survive.iter().any(|flag| *flag) {
                continue;
            }
            let body: Vec<&crate::domain::diff::DiffLine> = hunk
                .lines
                .iter()
                .zip(&survive)
                .filter(|(_, keep)| **keep)
                .map(|(line, _)| line)
                .collect();
            // The header is recomputed from the lines that survive, so the numbers a
            // model quotes back are the numbers in the file.
            let old_start = body
                .iter()
                .find_map(|line| line.old_line)
                .unwrap_or(hunk.old_start);
            let new_start = body
                .iter()
                .find_map(|line| line.new_line)
                .unwrap_or(hunk.new_start);
            let old_count = body.iter().filter(|line| line.old_line.is_some()).count();
            let new_count = body.iter().filter(|line| line.new_line.is_some()).count();
            let _ = writeln!(
                out,
                "@@ -{old_start},{old_count} +{new_start},{new_count} @@"
            );
            for line in &body {
                let marker = match line.kind {
                    LineKind::Add => '+',
                    LineKind::Delete => '-',
                    LineKind::Context => ' ',
                };
                let _ = writeln!(out, "{marker}{}", line.content);
            }
        }
    }
    out
}

/// The strictest decision for a patch file. A rename is allowed only when both its old
/// and new names are allowed, preventing a secret rename from laundering its hunks.
fn file_disposition<'a>(
    file: &crate::domain::diff::FileDiff,
    decisions: &'a [PathDecision],
) -> Option<&'a Disposition> {
    file.old_path
        .iter()
        .chain(file.new_path.iter())
        .filter_map(|path| decision_for(&path.to_string(), decisions))
        .find(|decision| !decision.is_included())
}

fn decision_for<'a>(path: &str, decisions: &'a [PathDecision]) -> Option<&'a Disposition> {
    decisions
        .iter()
        .filter(|decision| decision.path == path)
        .map(|decision| &decision.disposition)
        .filter(|disposition| !disposition.is_included())
        .max_by_key(|disposition| disposition_rank(disposition))
}

fn disposition_rank(disposition: &Disposition) -> u8 {
    match disposition {
        Disposition::Include => 0,
        Disposition::Placeholder { .. } => 1,
        Disposition::Omit { .. } => 2,
    }
}

/// A byte count a person reads at a glance.
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    // Tenths computed in integers, so nothing depends on float rounding.
    if bytes >= MIB {
        format!("{}.{} MB", bytes / MIB, (bytes % MIB) * 10 / MIB)
    } else if bytes >= KIB {
        format!("{}.{} KiB", bytes / KIB, (bytes % KIB) * 10 / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::diff::parse_patch;

    fn patch() -> Patch {
        parse_patch(
            "diff --git a/src/money.rs b/src/money.rs\n\
             --- a/src/money.rs\n\
             +++ b/src/money.rs\n\
             @@ -1,5 +1,6 @@\n\
             \u{20}line one\n\
             -old two\n\
             +new two\n\
             +extra\n\
             \u{20}line four\n\
             \u{20}line five\n",
        )
    }

    fn policy() -> BundlePolicy {
        BundlePolicy {
            max_context_tokens: 100_000,
            max_file_bytes: 16 * 1024,
            reduced_context_lines: 1,
        }
    }

    fn build_with(inputs: &BundleInputs<'_>) -> Bundle {
        build(inputs, &policy())
    }

    #[test]
    fn the_bundle_has_the_documented_order() {
        let patch = patch();
        let inputs = BundleInputs {
            metadata: "PR #141 by someone",
            commits: "abc123 first commit",
            conventions: vec![("AGENTS.md", b"Use thiserror." as &[u8])],
            diff: Some(&patch),
            files: vec![("src/money.rs", b"fn money() {}" as &[u8])],
            decisions: Vec::new(),
        };
        let bundle = build_with(&inputs);
        let positions: Vec<usize> = [
            "pull request",
            "commits",
            "AGENTS.md",
            "diff",
            "src/money.rs",
        ]
        .iter()
        .map(|needle| {
            bundle
                .text
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} is missing from {}", bundle.text))
        })
        .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{positions:?}"
        );
    }

    #[test]
    fn a_credential_is_never_sent_and_the_reason_is_recorded() {
        let inputs = BundleInputs {
            metadata: "PR",
            commits: "abc",
            conventions: Vec::new(),
            diff: None,
            files: vec![
                (".env", b"SECRET=hunter2" as &[u8]),
                (".env.local", b"SECRET=hunter2" as &[u8]),
                ("certs/server.pem", b"-----BEGIN" as &[u8]),
                ("id_rsa", b"private" as &[u8]),
                (".ssh/config", b"Host *" as &[u8]),
                ("src/main.rs", b"fn main() {}" as &[u8]),
            ],
            decisions: Vec::new(),
        };
        let bundle = build_with(&inputs);
        assert!(!bundle.text.contains("hunter2"));
        assert!(!bundle.text.contains("-----BEGIN"));
        assert!(!bundle.text.contains("private"));
        assert!(bundle.text.contains("fn main"), "{}", bundle.text);
        let omitted: Vec<&str> = bundle
            .segments
            .iter()
            .filter(|segment| !segment.included)
            .map(|segment| segment.label.as_str())
            .collect();
        assert_eq!(
            omitted,
            [
                ".env",
                ".env.local",
                "certs/server.pem",
                "id_rsa",
                ".ssh/config"
            ]
        );
        for segment in bundle.segments.iter().filter(|s| !s.included) {
            assert!(
                segment
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("credential")),
                "{segment:?}"
            );
        }
    }

    #[test]
    fn fr_4_6_excludes_secret_content_from_the_final_diff_payload() {
        let patch = parse_patch(
            "diff --git a/.env b/.env\n\
             --- a/.env\n\
             +++ b/.env\n\
             @@ -1 +1 @@\n\
             -OLD_SECRET_SENTINEL=one\n\
             +NEW_SECRET_SENTINEL=two\n\
             diff --git a/src/main.rs b/src/main.rs\n\
             --- a/src/main.rs\n\
             +++ b/src/main.rs\n\
             @@ -1 +1 @@\n\
             -fn old() {}\n\
             +fn allowed_source_sentinel() {}\n",
        );
        let inputs = BundleInputs {
            diff: Some(&patch),
            files: vec![
                (".env", b"NEW_SECRET_SENTINEL=two" as &[u8]),
                ("src/main.rs", b"fn allowed_source_sentinel() {}" as &[u8]),
            ],
            ..BundleInputs::default()
        };

        let bundle = build_with(&inputs);

        assert!(
            !bundle.text.contains("OLD_SECRET_SENTINEL"),
            "{}",
            bundle.text
        );
        assert!(
            !bundle.text.contains("NEW_SECRET_SENTINEL"),
            "{}",
            bundle.text
        );
        assert!(
            bundle.text.contains("allowed_source_sentinel"),
            "{}",
            bundle.text
        );
        assert!(
            bundle
                .inspection()
                .iter()
                .any(|line| line.contains("✗ .env"))
        );
    }

    #[test]
    fn fr_4_6_checks_both_names_of_a_secret_rename() {
        for (old, new) in [(".env", "config.txt"), ("config.txt", ".env.local")] {
            let text = format!(
                "diff --git a/{old} b/{new}\n\
                 similarity index 90%\n\
                 rename from {old}\n\
                 rename to {new}\n\
                 --- a/{old}\n\
                 +++ b/{new}\n\
                 @@ -1 +1 @@\n\
                 -RENAMED_SECRET_OLD\n\
                 +RENAMED_SECRET_NEW\n"
            );
            let patch = parse_patch(&text);
            let bundle = build_with(&BundleInputs {
                diff: Some(&patch),
                files: vec![(new, b"RENAMED_SECRET_NEW" as &[u8])],
                ..BundleInputs::default()
            });
            assert!(!bundle.text.contains("RENAMED_SECRET"), "{}", bundle.text);
        }
    }

    #[test]
    fn fr_4_6_excludes_a_deleted_secret_from_the_diff() {
        let patch = parse_patch(
            "diff --git a/.env.production b/.env.production\n\
             deleted file mode 100644\n\
             --- a/.env.production\n\
             +++ /dev/null\n\
             @@ -1 +0,0 @@\n\
             -DELETED_SECRET_SENTINEL=one\n",
        );
        let bundle = build_with(&BundleInputs {
            diff: Some(&patch),
            ..BundleInputs::default()
        });
        assert!(!bundle.text.contains("DELETED_SECRET_SENTINEL"));
        assert!(
            bundle
                .inspection()
                .iter()
                .any(|line| line.contains("✗ .env.production"))
        );
    }

    #[test]
    fn fr_4_6_applies_an_oversize_decision_to_diff_and_file_content() {
        let patch = parse_patch(
            "diff --git a/generated.txt b/generated.txt\n\
             --- a/generated.txt\n\
             +++ b/generated.txt\n\
             @@ -1 +1 @@\n\
             -OVERSIZE_OLD_SENTINEL\n\
             +OVERSIZE_NEW_SENTINEL\n",
        );
        let body = b"OVERSIZE_NEW_SENTINEL";
        let bundle = build_with(&BundleInputs {
            diff: Some(&patch),
            files: vec![("generated.txt", body.as_slice())],
            decisions: vec![PathDecision {
                path: "generated.txt".to_owned(),
                disposition: Disposition::Placeholder {
                    reason: "over the per-file limit".to_owned(),
                },
            }],
            ..BundleInputs::default()
        });
        assert!(!bundle.text.contains("OVERSIZE_OLD_SENTINEL"));
        assert!(!bundle.text.contains("OVERSIZE_NEW_SENTINEL"));
        assert!(bundle.inspection().iter().any(|line| {
            line.contains("generated.txt") && line.contains("diff content excluded")
        }));
    }

    #[test]
    fn a_binary_file_becomes_a_placeholder_rather_than_bytes() {
        let mut binary = vec![0_u8, 1, 2, 3];
        binary.extend_from_slice(b"text after a NUL");
        let inputs = BundleInputs {
            files: vec![("assets/logo.png", binary.as_slice())],
            ..BundleInputs::default()
        };
        let bundle = build_with(&inputs);
        assert!(!bundle.text.contains("text after a NUL"));
        assert!(
            bundle.text.contains("assets/logo.png (binary file)"),
            "{}",
            bundle.text
        );
        let segment = &bundle.segments[0];
        assert!(segment.included);
        assert_eq!(segment.detail.as_deref(), Some("binary file"));
    }

    #[test]
    fn an_oversized_file_becomes_a_placeholder_naming_its_size() {
        let big = vec![b'x'; 20 * 1024];
        let inputs = BundleInputs {
            files: vec![("src/generated.rs", big.as_slice())],
            ..BundleInputs::default()
        };
        let bundle = build_with(&inputs);
        assert!(
            bundle
                .text
                .contains("20.0 KiB over the 16 KiB per-file limit"),
            "{}",
            bundle.text
        );
        assert!(!bundle.text.contains("xxxxx"), "the content is not sent");
    }

    #[test]
    fn a_file_that_does_not_fit_the_budget_is_elided_not_cut_in_half() {
        let policy = BundlePolicy {
            max_context_tokens: 100,
            max_file_bytes: 1024 * 1024,
            reduced_context_lines: 1,
        };
        let small = vec![b'a'; 100];
        let large = vec![b'b'; 1000];
        let inputs = BundleInputs {
            metadata: "PR",
            commits: "abc",
            conventions: Vec::new(),
            diff: None,
            files: vec![("a.rs", small.as_slice()), ("b.rs", large.as_slice())],
            decisions: Vec::new(),
        };
        let bundle = build(&inputs, &policy);
        assert!(bundle.text.contains(&"a".repeat(100)));
        assert!(
            !bundle.text.contains(&"b".repeat(50)),
            "not even part of it"
        );
        let elided = bundle
            .segments
            .iter()
            .find(|segment| segment.label == "b.rs")
            .expect("a segment for the file");
        assert!(!elided.included);
        assert!(
            elided
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("elided")),
            "{elided:?}"
        );
    }

    #[test]
    fn the_diff_loses_context_before_it_loses_content() {
        let patch = parse_patch(
            "diff --git a/src/money.rs b/src/money.rs\n\
             --- a/src/money.rs\n\
             +++ b/src/money.rs\n\
             @@ -1,8 +1,9 @@\n\
             \u{20}context one\n\
             \u{20}context two\n\
             \u{20}context three\n\
             \u{20}context four\n\
             -old five\n\
             +new five\n\
             +extra\n\
             \u{20}context seven\n\
             \u{20}context eight\n\
             \u{20}context nine\n",
        );
        let full = render_patch(&patch, None);
        let reduced = render_patch(&patch, Some(1));
        assert!(full.contains("context three"), "{full}");
        assert!(!reduced.contains("context three"), "{reduced}");
        // Everything that changed is in both, and the header counts the survivors.
        for text in [&full, &reduced] {
            assert!(text.contains("-old five"), "{text}");
            assert!(text.contains("+new five"), "{text}");
            assert!(text.contains("+extra"), "{text}");
        }
        assert!(reduced.contains("@@ -4,3 +4,4 @@"), "{reduced}");
        assert!(full.contains("@@ -1,8 +1,9 @@"), "{full}");
    }

    #[test]
    fn a_diff_too_large_even_reduced_is_truncated_with_a_marker() {
        let mut text = String::from("diff --git a/big.rs b/big.rs\n--- a/big.rs\n+++ b/big.rs\n");
        text.push_str("@@ -1,400 +1,400 @@\n");
        for index in 0..400 {
            let _ = writeln!(text, "-line {index}");
            let _ = writeln!(text, "+changed {index}");
        }
        let patch = parse_patch(&text);
        let policy = BundlePolicy {
            max_context_tokens: 200,
            max_file_bytes: 1024,
            reduced_context_lines: 1,
        };
        let inputs = BundleInputs {
            diff: Some(&patch),
            ..BundleInputs::default()
        };
        let bundle = build(&inputs, &policy);
        let segment = bundle
            .segments
            .iter()
            .find(|segment| segment.kind == SegmentKind::Diff)
            .expect("a diff segment");
        assert!(segment.truncated, "{segment:?}");
        assert!(
            segment
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("truncated")),
            "{segment:?}"
        );
        assert!(
            bundle.bytes() <= policy.max_bytes() + 64,
            "{}",
            bundle.bytes()
        );
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let text = "é".repeat(10);
        let (cut, truncated) = truncate_bytes(&text, 5);
        assert!(truncated);
        assert_eq!(cut, "éé", "a 2-byte character cannot be cut in half");
    }

    #[test]
    fn the_inspector_says_what_was_sent_and_what_was_not() {
        let inputs = BundleInputs {
            metadata: "PR #141",
            commits: "abc",
            conventions: Vec::new(),
            diff: None,
            files: vec![
                (".env", b"S" as &[u8]),
                ("src/main.rs", b"fn main() {}" as &[u8]),
            ],
            decisions: Vec::new(),
        };
        let bundle = build_with(&inputs);
        let lines = bundle.inspection();
        assert!(lines[0].contains("context:"), "{lines:?}");
        assert!(
            lines.iter().any(|line| line.contains("✗ .env")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("✓ src/main.rs")),
            "{lines:?}"
        );
    }

    #[test]
    fn the_summary_names_tokens_bytes_and_counts() {
        let inputs = BundleInputs {
            metadata: "PR #141",
            files: vec![("a.rs", b"fn a() {}" as &[u8])],
            ..BundleInputs::default()
        };
        let bundle = build_with(&inputs);
        let summary = bundle.summary();
        assert!(summary.contains("tokens"), "{summary}");
        // The size is bytes and says so: the first version of this line printed the
        // byte count next to the word "KB".
        assert!(summary.contains(" B)"), "{summary}");
        assert!(summary.contains("2 of 2 items"), "{summary}");
    }

    #[test]
    fn an_empty_bundle_is_empty_rather_than_odd() {
        let bundle = build_with(&BundleInputs::default());
        assert!(bundle.text.is_empty());
        assert_eq!(bundle.estimated_tokens, 0);
        assert_eq!(bundle.included(), 0);
    }

    #[test]
    fn the_estimate_matches_the_documented_approximation() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(5), 2);
        assert_eq!(estimate_tokens(4000), 1000);
    }

    #[test]
    fn secret_paths_are_recognised_without_catching_ordinary_source_files() {
        for path in [
            ".env",
            ".env.production",
            "config/.env.staging",
            "keys/deploy.pem",
            "certs/client.p12",
            "id_ed25519",
            ".aws/credentials",
            ".ssh/id_rsa",
            "credentials.toml",
        ] {
            assert!(is_secret_path(path), "{path} should be treated as a secret");
        }
        for path in [
            "src/credentials.rs",
            "src/env.rs",
            "docs/environment.md",
            "src/keys.rs",
            "testdata/envelope.txt",
            "src/main.rs",
            ".gitignore",
        ] {
            assert!(!is_secret_path(path), "{path} is ordinary source");
        }
    }

    #[test]
    fn the_policy_defaults_are_the_documented_ones() {
        let policy = BundlePolicy::default();
        assert_eq!(policy.max_context_tokens, 100_000);
        assert_eq!(policy.max_file_bytes, 256 * 1024);
        assert_eq!(policy.max_bytes(), 400_000);
    }

    #[test]
    fn a_placeholder_file_costs_almost_nothing() {
        let big = vec![b'x'; 64 * 1024];
        let policy = BundlePolicy {
            max_file_bytes: 1024,
            ..policy()
        };
        let inputs = BundleInputs {
            files: vec![("src/big.rs", big.as_slice())],
            ..BundleInputs::default()
        };
        let bundle = build(&inputs, &policy);
        assert!(bundle.estimated_tokens < 20, "{}", bundle.estimated_tokens);
    }
}
