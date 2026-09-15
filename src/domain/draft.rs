//! The review draft: what the user has staged to send (FR-6.1, §7.2).
//!
//! A draft is the one document in this application that can cause something
//! *irreversible* to happen outside it, so the rules live here, in pure code, rather
//! than in the composer that fills it in or the modal that sends it:
//!
//! * a comment is anchored to a file, a side and a line, and a range must be a range
//!   (`start <= end`, same side, same file) — GitHub answers a bad anchor with a 422
//!   that names no field (FR-6.2);
//! * a comment with an empty body is not a comment (FR-6.2);
//! * a review with nothing in it — no comments, no body, and no decision other than
//!   "comment" — is refused before it is sent, because the alternative is a request
//!   that fails after the user has already confirmed (FR-6.3);
//! * the draft remembers the commit it was written against, so a review that has
//!   drifted can say so instead of anchoring a comment to the wrong line (FR-6.3).
//!
//! Nothing here knows about `gh`, GraphQL or the terminal. What the user typed is
//! kept verbatim: the draft is never rewritten to make a request easier to build.

use crate::domain::diff::RelPath;
use crate::domain::time::Timestamp;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// The draft document's format, for migrations (§7.1's rule, applied here).
///
/// A draft written by a *newer* build is refused rather than half-understood: the
/// fields this build does not know could be the ones that decide what is sent.
pub const DRAFT_VERSION: u32 = 1;

/// GitHub's own limit on a review body and on a comment, in bytes.
///
/// Checked here so the refusal is a sentence in the composer rather than a 422 from
/// the API after the modal has been confirmed.
pub const MAX_TEXT_BYTES: usize = 65_536;

/// What a review says about the pull request (FR-6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Approve it.
    Approve,
    /// Ask for changes.
    RequestChanges,
    /// Say something without a verdict.
    Comment,
}

impl Decision {
    /// Every decision, in the order the modal offers them.
    pub const ALL: [Self; 3] = [Self::Approve, Self::RequestChanges, Self::Comment];

    /// The word shown in the UI and stored in the draft.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::RequestChanges => "request changes",
            Self::Comment => "comment",
        }
    }

    /// What a person types on the command line, and what the palette suggests.
    #[must_use]
    pub const fn command_word(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::RequestChanges => "request-changes",
            Self::Comment => "comment",
        }
    }

    /// The `gh pr review` flag for this decision.
    ///
    /// The short form of a review that carries no inline comments (FR-6.3), which is
    /// the one case `gh pr review` is used for.
    #[must_use]
    pub const fn gh_flag(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::RequestChanges => "request-changes",
            Self::Comment => "comment",
        }
    }

    /// The `PullRequestReviewEvent` this decision submits.
    ///
    /// One spelling, two APIs: REST's `event` and GraphQL's `event` take the same
    /// words, which is why this is named after the event rather than after a route.
    #[must_use]
    pub const fn event(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
            Self::Comment => "COMMENT",
        }
    }

    /// Reads a decision from user input, accepting the command word and the label.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "approve" | "approved" | "lgtm" | "a" => Some(Self::Approve),
            "request-changes" | "request_changes" | "request changes" | "changes" | "r" => {
                Some(Self::RequestChanges)
            }
            "comment" | "commented" | "c" => Some(Self::Comment),
            _ => None,
        }
    }
}

/// Which side of the diff a line number counts from (FR-6.2).
///
/// GitHub calls these `LEFT` and `RIGHT`, and the distinction is not cosmetic: line
/// 31 is a different line on each side of a hunk, so a comment anchored to the wrong
/// one lands on whatever happens to be there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The file as it was before the change; GitHub's `LEFT`.
    Old,
    /// The file as the change leaves it; GitHub's `RIGHT`.
    New,
}

impl Side {
    /// The word shown in the UI and stored in the draft.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }

    /// What the API calls it.
    #[must_use]
    pub const fn api(self) -> &'static str {
        match self {
            Self::Old => "LEFT",
            Self::New => "RIGHT",
        }
    }

    /// Reads a side from user input.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "old" | "left" | "o" | "l" | "-" => Some(Self::Old),
            "new" | "right" | "n" | "r" | "+" => Some(Self::New),
            _ => None,
        }
    }
}

/// Why a comment or a draft cannot be sent (FR-6.2, FR-6.3).
///
/// Every variant is a sentence the user can act on: these are shown verbatim in the
/// composer and in the publish modal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DraftError {
    /// A comment with no text is not a comment.
    #[error("a comment needs a body")]
    EmptyBody,
    /// The body is longer than GitHub will accept.
    #[error("this is {bytes} bytes; GitHub accepts at most {MAX_TEXT_BYTES}")]
    TooLong {
        /// How long the refused text is.
        bytes: usize,
    },
    /// A line number of zero is not a line.
    #[error("line {line} is not a line of the diff")]
    BadLine {
        /// The line number as given.
        line: u32,
    },
    /// A range that runs backwards.
    #[error("the range starts at line {start} and ends at line {end}")]
    BadRange {
        /// The start of the range.
        start: u32,
        /// The end of the range.
        end: u32,
    },
    /// A path the diff could not have produced.
    #[error("`{path}` is not a usable path")]
    BadPath {
        /// The path as given.
        path: String,
    },
    /// Nothing to say, so there is no review to send.
    #[error("there is nothing to send: write a comment, a body, or choose a decision")]
    NothingToSay,
    /// The stored document could not be read.
    #[error("the draft for this pull request could not be read: {reason}")]
    Malformed {
        /// What went wrong, from the parser.
        reason: String,
    },
    /// The stored document is from a newer build.
    #[error("this draft was written by a newer version of smart-review (format {found})")]
    Version {
        /// The format the document declares.
        found: u32,
    },
}

/// A message posted on its own, outside the review draft (FR-6.4, DEC-16).
///
/// Two things are posted without being part of a review: a reply into a thread that is
/// already there, and a comment on the pull request's own conversation. Neither carries
/// a decision, neither can be batched with anything (GitHub's review API only accepts
/// *new* inline comments), and both are the same value to validate — words and a limit.
///
/// Deliberately not a [`DraftComment`] with empty fields: the two are validated
/// differently (one needs a line, the other must not have one) and a type that could be
/// either would be one every caller has to check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Post {
    /// What the user wrote.
    pub body: String,
}

impl Post {
    /// Validates a message (FR-6.4).
    ///
    /// # Errors
    ///
    /// Returns [`DraftError::EmptyBody`] for a body that is empty once trimmed, and
    /// [`DraftError::TooLong`] past GitHub's limit — the same two sentences the
    /// review composer uses, because they are the same two mistakes.
    pub fn new(body: impl Into<String>) -> Result<Self, DraftError> {
        let body = body.into();
        if body.trim().is_empty() {
            return Err(DraftError::EmptyBody);
        }
        if body.len() > MAX_TEXT_BYTES {
            return Err(DraftError::TooLong { bytes: body.len() });
        }
        Ok(Self { body })
    }
}

/// One staged inline comment (FR-6.1, §7.2).
///
/// `start_line` is the *earlier* line of a range, as GitHub's `startLine` is, and is
/// normalised away when it equals `line`: a range of one line is a line, and sending
/// both fields for it invites the API to disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftComment {
    /// The file, as the diff names it.
    pub path: String,
    /// Which side the line numbers count from.
    pub side: Side,
    /// The last line of the comment's anchor.
    pub line: u32,
    /// The first line, for a multi-line comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    /// What the user wrote.
    pub body: String,
}

impl DraftComment {
    /// Validates and normalises a comment (FR-6.2).
    ///
    /// # Errors
    ///
    /// Returns [`DraftError::EmptyBody`] for a body that is empty once trimmed,
    /// [`DraftError::TooLong`] past GitHub's limit, [`DraftError::BadPath`],
    /// [`DraftError::BadLine`] for a line of zero, and [`DraftError::BadRange`] for a
    /// range that runs backwards.
    pub fn new(
        path: impl Into<String>,
        side: Side,
        line: u32,
        start_line: Option<u32>,
        body: impl Into<String>,
    ) -> Result<Self, DraftError> {
        let path = path.into();
        let body = body.into();
        if body.trim().is_empty() {
            return Err(DraftError::EmptyBody);
        }
        if body.len() > MAX_TEXT_BYTES {
            return Err(DraftError::TooLong { bytes: body.len() });
        }
        if RelPath::parse(path.clone()).is_none() {
            return Err(DraftError::BadPath { path });
        }
        if line == 0 {
            return Err(DraftError::BadLine { line });
        }
        let start_line = match start_line {
            None | Some(0) => None,
            Some(start) if start > line => {
                return Err(DraftError::BadRange { start, end: line });
            }
            Some(start) if start == line => None,
            Some(start) => Some(start),
        };
        Ok(Self {
            path,
            side,
            line,
            start_line,
            body,
        })
    }

    /// The lines this comment covers, as a pair, first line first.
    #[must_use]
    pub fn range(&self) -> (u32, u32) {
        (self.start_line.unwrap_or(self.line), self.line)
    }

    /// Whether the comment covers more than one line.
    #[must_use]
    pub fn is_range(&self) -> bool {
        self.start_line.is_some()
    }

    /// How the comment's anchor is shown wherever a person reads it.
    #[must_use]
    pub fn anchor(&self) -> String {
        let (start, end) = self.range();
        let side = self.side.label();
        if start == end {
            format!("{}:{end} ({side})", self.path)
        } else {
            format!("{}:{start}-{end} ({side})", self.path)
        }
    }

    /// Whether this comment is anchored at `line` on `side` of `path`.
    #[must_use]
    pub fn covers(&self, path: &str, side: Side, line: u32) -> bool {
        if self.path != path || self.side != side {
            return false;
        }
        let (start, end) = self.range();
        (start..=end).contains(&line)
    }
}

/// A review that has not been sent yet (FR-6.1, §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// The format this document was written in.
    #[serde(default = "default_version")]
    pub version: u32,
    /// The pull request it belongs to.
    pub pr: u64,
    /// What the review says, once the user has decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    /// The review body, which may be empty or absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The staged comments, in the order they were written.
    #[serde(default)]
    pub comments: Vec<DraftComment>,
    /// The commit the comments were anchored against, when it is known (FR-6.3).
    ///
    /// Not part of §7.2's document, and read defensively: a draft written before this
    /// field existed simply has no head, and is treated as current rather than stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    /// When it was last changed.
    pub updated_at: Timestamp,
}

const fn default_version() -> u32 {
    DRAFT_VERSION
}

impl Draft {
    /// An empty draft for a pull request.
    #[must_use]
    pub fn new(pr: u64, now: Timestamp) -> Self {
        Self {
            version: DRAFT_VERSION,
            pr,
            decision: None,
            body: None,
            comments: Vec::new(),
            head_sha: None,
            updated_at: now,
        }
    }

    /// Whether there is anything staged at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.comments.is_empty() && self.body.as_deref().unwrap_or("").trim().is_empty()
    }

    /// Whether the draft has a decision, a body or a comment to send (FR-6.3).
    ///
    /// # Errors
    ///
    /// Returns [`DraftError::NothingToSay`] when the review would be an empty
    /// `COMMENT`, which GitHub accepts and nobody can read anything into.
    pub fn publishable(&self) -> Result<(), DraftError> {
        let has_body = !self.body.as_deref().unwrap_or("").trim().is_empty();
        if self.comments.is_empty() && !has_body {
            match self.decision {
                Some(Decision::Approve | Decision::RequestChanges) => return Ok(()),
                Some(Decision::Comment) | None => {}
            }
            return Err(DraftError::NothingToSay);
        }
        Ok(())
    }

    /// The decision as it will be sent, defaulting to a plain comment (FR-6.3).
    ///
    /// "Comment" is the only safe default: it is the one event that cannot approve or
    /// block a pull request by accident.
    #[must_use]
    pub fn effective_decision(&self) -> Decision {
        self.decision.unwrap_or(Decision::Comment)
    }

    /// Records the decision and the moment it changed.
    pub fn set_decision(&mut self, decision: Option<Decision>, now: Timestamp) {
        self.decision = decision;
        self.updated_at = now;
    }

    /// Replaces the review body.
    pub fn set_body(&mut self, body: impl Into<String>, now: Timestamp) {
        let body = body.into();
        self.body = if body.trim().is_empty() {
            None
        } else {
            Some(body)
        };
        self.updated_at = now;
    }

    /// Adds a staged comment (FR-6.1).
    pub fn add(&mut self, comment: DraftComment, now: Timestamp) {
        self.comments.push(comment);
        self.updated_at = now;
    }

    /// Adds a staged comment where the diff currently is, first in the list (FR-6.1).
    ///
    /// Used by "comment on this line": the newest comment is the one the user is
    /// looking at, and the draft panel is read from the top.
    pub fn insert(&mut self, comment: DraftComment, now: Timestamp) {
        self.comments.insert(0, comment);
        self.updated_at = now;
    }

    /// Removes the comment at `index`, returning it.
    ///
    /// The index is 1-based, because it is the number the draft panel and `:draft
    /// remove` show; 0 and past-the-end are refused rather than clamped, so a typo
    /// cannot quietly remove the last comment.
    pub fn remove(&mut self, number: usize, now: Timestamp) -> Option<DraftComment> {
        if number == 0 || number > self.comments.len() {
            return None;
        }
        let removed = self.comments.remove(number - 1);
        self.updated_at = now;
        Some(removed)
    }

    /// Empties the draft, keeping the pull request it belongs to (FR-6.1).
    pub fn clear(&mut self, now: Timestamp) {
        self.comments.clear();
        self.decision = None;
        self.body = None;
        // With no staged line coordinates left, this is a new draft. Keeping the old
        // revision would incorrectly make its first new comment look drifted.
        self.head_sha = None;
        self.updated_at = now;
    }

    /// The comments anchored at a line of the diff, for the gutter marker (FR-6.1).
    #[must_use]
    pub fn at(&self, path: &str, side: Side, line: u32) -> Vec<&DraftComment> {
        self.comments
            .iter()
            .filter(|comment| comment.covers(path, side, line))
            .collect()
    }

    /// The 1-based number the draft panel shows for a comment, by identity.
    #[must_use]
    pub fn number_of(&self, comment: &DraftComment) -> Option<usize> {
        self.comments
            .iter()
            .position(|candidate| candidate == comment)
            .map(|index| index + 1)
    }

    /// Whether the comments were anchored against a different commit (FR-6.3).
    ///
    /// A draft with no recorded head is not stale: it was written before the app
    /// recorded heads, and refusing to send it would be a worse guess than sending it.
    #[must_use]
    pub fn drifted_from(&self, head_sha: Option<&str>) -> bool {
        match (self.head_sha.as_deref(), head_sha) {
            (Some(stored), Some(current)) => stored != current,
            _ => false,
        }
    }

    /// Reads a stored draft (§7.2).
    ///
    /// # Errors
    ///
    /// Returns [`DraftError::Malformed`] when the document is not a draft, and
    /// [`DraftError::Version`] when it declares a newer format than this build knows.
    pub fn from_json(text: &str) -> Result<Self, DraftError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|error| DraftError::Malformed {
                reason: error.to_string(),
            })?;
        if let Some(version) = value.get("version").and_then(serde_json::Value::as_u64)
            && version > u64::from(DRAFT_VERSION)
        {
            return Err(DraftError::Version {
                found: u32::try_from(version).unwrap_or(u32::MAX),
            });
        }
        let mut draft: Self =
            serde_json::from_value(value).map_err(|error| DraftError::Malformed {
                reason: error.to_string(),
            })?;
        // Re-validate on the way in: a hand-edited file is a real thing (the composer
        // is not the only way a draft gets written), and an unvalidated range is
        // exactly what gets a 422 with no field name on it.
        for comment in &mut draft.comments {
            *comment = Self::revalidated(comment)?;
        }
        draft.version = DRAFT_VERSION;
        Ok(draft)
    }

    fn revalidated(comment: &DraftComment) -> Result<DraftComment, DraftError> {
        Self::comment(
            comment.path.clone(),
            comment.side,
            comment.line,
            comment.start_line,
            comment.body.clone(),
        )
    }

    /// Validates a comment without staging it, for `:draft check`-style feedback.
    ///
    /// # Errors
    ///
    /// As [`DraftComment::new`].
    pub fn comment(
        path: impl Into<String>,
        side: Side,
        line: u32,
        start_line: Option<u32>,
        body: impl Into<String>,
    ) -> Result<DraftComment, DraftError> {
        DraftComment::new(path, side, line, start_line, body)
    }

    /// The draft as stored (§7.2), pretty-printed so a person can read it.
    ///
    /// # Errors
    ///
    /// Returns [`DraftError::Malformed`] if the document cannot be serialized, which
    /// would mean a field type this module got wrong.
    pub fn to_json(&self) -> Result<String, DraftError> {
        serde_json::to_string_pretty(self).map_err(|error| DraftError::Malformed {
            reason: error.to_string(),
        })
    }

    /// The draft as markdown, for `:draft show` and `:draft export` (FR-6.1).
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = format!("# Review draft for #{}\n\n", self.pr);
        let _ = writeln!(
            out,
            "- **Decision:** {}",
            self.decision
                .map_or("(not chosen)".to_owned(), |decision| decision
                    .label()
                    .to_owned())
        );
        let _ = writeln!(
            out,
            "- **Body:** {}",
            match self.body.as_deref().map(str::trim) {
                Some(body) if !body.is_empty() => body.replace('\n', "\n  "),
                _ => "(empty)".to_owned(),
            }
        );
        let _ = writeln!(out, "- **Updated:** {}", self.updated_at.to_rfc3339());
        if let Some(head) = &self.head_sha {
            let _ = writeln!(out, "- **Written against:** {head}");
        }
        let _ = write!(out, "\n## Comments ({})\n", self.comments.len());
        for (index, comment) in self.comments.iter().enumerate() {
            let indented = comment
                .body
                .lines()
                .map(|line| format!("   {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            let _ = write!(
                out,
                "\n{}. `{}`\n\n{indented}\n",
                index + 1,
                comment.anchor()
            );
        }
        out
    }

    /// What the review will send, as counted in the status line and the modal title.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.comments.len() == 1 {
            "1 comment".to_owned()
        } else {
            format!("{} comments", self.comments.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::time::from_unix_secs;

    fn now() -> Timestamp {
        from_unix_secs(1_700_000_000)
    }

    fn comment(path: &str, line: u32, body: &str) -> DraftComment {
        DraftComment::new(path, Side::New, line, None, body).expect("valid comment")
    }

    #[test]
    fn a_post_is_validated_like_the_words_it_is() {
        assert_eq!(Post::new("   \n "), Err(DraftError::EmptyBody));
        assert_eq!(
            Post::new("x".repeat(MAX_TEXT_BYTES + 1)),
            Err(DraftError::TooLong {
                bytes: MAX_TEXT_BYTES + 1
            })
        );
        // A reply and a review comment share the limit, so a user who pastes
        // something enormous is told the same thing in both composers.
        let long = "y".repeat(MAX_TEXT_BYTES + 1);
        assert_eq!(
            Post::new(long.clone()).expect_err("too long").to_string(),
            DraftComment::new("src/a.rs", Side::New, 1, None, long)
                .expect_err("too long")
                .to_string()
        );
        let post = Post::new("agreed, fixed in 9f2c1ab").expect("valid");
        assert_eq!(post.body, "agreed, fixed in 9f2c1ab");
    }

    #[test]
    fn a_comment_needs_a_body() {
        assert_eq!(
            DraftComment::new("src/a.rs", Side::New, 1, None, "   \n\t"),
            Err(DraftError::EmptyBody)
        );
        assert_eq!(
            DraftComment::new("src/a.rs", Side::New, 1, None, "  why?  "),
            Ok(DraftComment {
                path: "src/a.rs".to_owned(),
                side: Side::New,
                line: 1,
                start_line: None,
                body: "  why?  ".to_owned(),
            }),
            "the body is kept verbatim, only checked"
        );
    }

    #[test]
    fn a_comment_needs_a_line_and_a_path() {
        assert_eq!(
            DraftComment::new("src/a.rs", Side::Old, 0, None, "why?"),
            Err(DraftError::BadLine { line: 0 })
        );
        assert_eq!(
            DraftComment::new("/etc/passwd", Side::New, 3, None, "why?"),
            Err(DraftError::BadPath {
                path: "/etc/passwd".to_owned()
            })
        );
        assert_eq!(
            DraftComment::new("../../secrets", Side::New, 3, None, "why?"),
            Err(DraftError::BadPath {
                path: "../../secrets".to_owned()
            })
        );
    }

    #[test]
    fn a_range_that_runs_backwards_is_refused() {
        assert_eq!(
            DraftComment::new("src/a.rs", Side::New, 10, Some(11), "why?"),
            Err(DraftError::BadRange { start: 11, end: 10 })
        );
    }

    #[test]
    fn a_one_line_range_is_stored_as_a_line() {
        let comment =
            DraftComment::new("src/a.rs", Side::New, 10, Some(10), "why?").expect("valid");
        assert_eq!(comment.start_line, None);
        assert!(!comment.is_range());
        assert_eq!(comment.range(), (10, 10));
        // A start of zero is a line number nobody typed; it means "no range".
        let comment = DraftComment::new("src/a.rs", Side::New, 10, Some(0), "why?").expect("valid");
        assert_eq!(comment.start_line, None);
    }

    #[test]
    fn a_range_from_an_earlier_line_is_kept_in_order() {
        let comment =
            DraftComment::new("src/a.rs", Side::New, 31, Some(28), "why?").expect("valid");
        assert!(comment.is_range());
        assert_eq!(comment.range(), (28, 31));
        assert_eq!(comment.anchor(), "src/a.rs:28-31 (new)");
        assert_eq!(comment.anchor(), comment.anchor());
    }

    #[test]
    fn a_comment_covers_the_lines_of_its_range() {
        let range = DraftComment::new("src/a.rs", Side::New, 31, Some(28), "why?").expect("valid");
        assert!(range.covers("src/a.rs", Side::New, 28));
        assert!(range.covers("src/a.rs", Side::New, 31));
        assert!(range.covers("src/a.rs", Side::New, 30));
        assert!(!range.covers("src/a.rs", Side::New, 27));
        assert!(!range.covers("src/a.rs", Side::Old, 31), "wrong side");
        assert!(!range.covers("src/b.rs", Side::New, 31), "wrong file");
    }

    #[test]
    fn a_decision_round_trips_through_its_spellings() {
        for decision in Decision::ALL {
            assert_eq!(Decision::parse(decision.label()), Some(decision));
            assert_eq!(Decision::parse(decision.command_word()), Some(decision));
            assert_eq!(
                Decision::parse(decision.event()),
                Some(decision),
                "the GraphQL event must parse back"
            );
        }
        assert_eq!(Decision::parse("lgtm"), Some(Decision::Approve));
        assert_eq!(Decision::parse("nonsense"), None);
        assert_eq!(Decision::Approve.event(), "APPROVE");
        assert_eq!(Decision::RequestChanges.event(), "REQUEST_CHANGES");
        assert_eq!(Decision::Comment.event(), "COMMENT");
    }

    #[test]
    fn a_side_round_trips_through_its_spellings() {
        for side in [Side::Old, Side::New] {
            assert_eq!(Side::parse(side.label()), Some(side));
            assert_eq!(Side::parse(side.api()), Some(side));
        }
        assert_eq!(Side::Old.api(), "LEFT");
        assert_eq!(Side::New.api(), "RIGHT");
    }

    #[test]
    fn a_draft_with_nothing_in_it_is_not_publishable() {
        let draft = Draft::new(141, now());
        assert!(draft.is_empty());
        assert_eq!(draft.publishable(), Err(DraftError::NothingToSay));

        let mut with_comment = Draft::new(141, now());
        with_comment.add(comment("src/a.rs", 3, "why?"), now());
        assert!(!with_comment.is_empty());
        assert_eq!(with_comment.publishable(), Ok(()));

        let mut with_body = Draft::new(141, now());
        with_body.set_body("Looks good overall.", now());
        assert_eq!(with_body.publishable(), Ok(()));

        // A bare verdict is a review: GitHub accepts an approval with no text.
        for decision in [Decision::Approve, Decision::RequestChanges] {
            let mut draft = Draft::new(141, now());
            draft.set_decision(Some(decision), now());
            assert_eq!(
                draft.publishable(),
                Ok(()),
                "{decision:?} alone is a review"
            );
        }
        // "Comment" with nothing to comment on is not.
        let mut draft = Draft::new(141, now());
        draft.set_decision(Some(Decision::Comment), now());
        assert_eq!(draft.publishable(), Err(DraftError::NothingToSay));
    }

    #[test]
    fn a_draft_with_only_whitespace_for_a_body_is_empty() {
        let mut draft = Draft::new(141, now());
        draft.set_body("   \n  ", now());
        assert_eq!(draft.body, None, "whitespace is not a body");
        assert!(draft.is_empty());
    }

    #[test]
    fn the_default_decision_cannot_approve_by_accident() {
        let draft = Draft::new(141, now());
        assert_eq!(draft.effective_decision(), Decision::Comment);
        let mut draft = draft;
        draft.add(comment("src/a.rs", 1, "why?"), now());
        assert_eq!(
            draft.effective_decision(),
            Decision::Comment,
            "a staged comment does not imply a verdict"
        );
    }

    #[test]
    fn removing_a_comment_counts_from_one_and_refuses_nonsense() {
        let mut draft = Draft::new(141, now());
        draft.add(comment("src/a.rs", 1, "first"), now());
        draft.add(comment("src/b.rs", 2, "second"), now());

        assert_eq!(draft.remove(0, now()), None);
        assert_eq!(draft.remove(3, now()), None);
        assert_eq!(
            draft.comments.len(),
            2,
            "nothing was removed by a bad number"
        );

        let removed = draft.remove(1, now()).expect("the first comment");
        assert_eq!(removed.body, "first");
        assert_eq!(draft.comments.len(), 1);
        assert_eq!(draft.comments[0].body, "second");
    }

    #[test]
    fn the_newest_comment_comes_first_in_the_panel() {
        let mut draft = Draft::new(141, now());
        draft.add(comment("src/a.rs", 1, "older"), now());
        draft.insert(comment("src/b.rs", 2, "newer"), now());
        assert_eq!(draft.comments[0].body, "newer");
        assert_eq!(draft.number_of(&draft.comments[0]), Some(1));
    }

    #[test]
    fn clearing_keeps_the_pull_request() {
        let mut draft = Draft::new(141, now());
        draft.add(comment("src/a.rs", 1, "why?"), now());
        draft.set_decision(Some(Decision::Approve), now());
        draft.set_body("nice", now());
        draft.head_sha = Some("h1".to_owned());
        draft.clear(now());
        assert_eq!(draft.pr, 141);
        assert!(draft.is_empty());
        assert_eq!(draft.decision, None);
        assert_eq!(draft.head_sha, None, "the next comment gets a fresh anchor");
    }

    #[test]
    fn a_draft_round_trips_through_its_document() {
        let mut draft = Draft::new(141, now());
        draft.set_decision(Some(Decision::RequestChanges), now());
        draft.set_body("One thing to fix.\n\nAnd another.", now());
        draft.add(
            DraftComment::new("src/a.rs", Side::New, 31, Some(28), "this rounds up")
                .expect("valid"),
            now(),
        );
        draft.add(comment("src/b.rs", 7, "and this is unused"), now());
        draft.head_sha = Some("deadbeef".to_owned());

        let text = draft.to_json().expect("serialisable");
        assert_eq!(Draft::from_json(&text).expect("readable"), draft);
        assert!(text.contains("\"start_line\": 28"));
    }

    #[test]
    fn a_stored_draft_is_revalidated_on_the_way_in() {
        // The composer is not the only way a draft gets written: the file is JSON in
        // the user's home, and a hand-edited range is exactly what GitHub answers with
        // a 422 that names no field.
        let text = r#"{
            "pr": 141,
            "comments": [
                {"path": "src/a.rs", "side": "new", "line": 10, "start_line": 20, "body": "why?"}
            ],
            "updated_at": "2023-11-14T22:13:20Z"
        }"#;
        assert_eq!(
            Draft::from_json(text),
            Err(DraftError::BadRange { start: 20, end: 10 })
        );
    }

    #[test]
    fn a_stored_draft_with_an_empty_body_is_refused() {
        let text = r#"{
            "pr": 141,
            "comments": [{"path": "src/a.rs", "side": "new", "line": 10, "body": "  "}],
            "updated_at": "2023-11-14T22:13:20Z"
        }"#;
        assert_eq!(Draft::from_json(text), Err(DraftError::EmptyBody));
    }

    #[test]
    fn a_tolerated_draft_takes_unknown_fields_and_missing_ones() {
        // §7.1's rule: unknown fields are tolerated so a newer build's draft does not
        // read as corrupt. Missing optional fields default rather than fail.
        let text = r#"{
            "pr": 141,
            "decision": "approve",
            "something_new": {"nested": true},
            "comments": [],
            "updated_at": "2023-11-14T22:13:20Z"
        }"#;
        let draft = Draft::from_json(text).expect("tolerated");
        assert_eq!(draft.decision, Some(Decision::Approve));
        assert_eq!(draft.body, None);
        assert_eq!(draft.head_sha, None);
        assert_eq!(draft.version, DRAFT_VERSION);
    }

    #[test]
    fn a_draft_from_a_newer_build_is_refused_not_guessed_at() {
        let text = r#"{"version": 99, "pr": 141, "updated_at": "2023-11-14T22:13:20Z"}"#;
        assert_eq!(
            Draft::from_json(text),
            Err(DraftError::Version { found: 99 })
        );
    }

    #[test]
    fn a_document_that_is_not_a_draft_is_reported_as_such() {
        assert!(matches!(
            Draft::from_json("not json at all"),
            Err(DraftError::Malformed { .. })
        ));
        assert!(matches!(
            Draft::from_json(r#"{"decision": "maybe"}"#),
            Err(DraftError::Malformed { .. })
        ));
    }

    #[test]
    fn drift_is_reported_only_when_both_heads_are_known() {
        let mut draft = Draft::new(141, now());
        assert!(!draft.drifted_from(Some("abc")), "no recorded head");
        draft.head_sha = Some("abc".to_owned());
        assert!(!draft.drifted_from(Some("abc")));
        assert!(draft.drifted_from(Some("def")));
        assert!(
            !draft.drifted_from(None),
            "a diff that is not open cannot have drifted"
        );
    }

    #[test]
    fn the_gutter_asks_whether_a_line_has_a_comment() {
        let mut draft = Draft::new(141, now());
        draft.add(
            DraftComment::new("src/a.rs", Side::New, 31, Some(28), "why?").expect("valid"),
            now(),
        );
        assert_eq!(draft.at("src/a.rs", Side::New, 29).len(), 1);
        assert!(draft.at("src/a.rs", Side::Old, 29).is_empty());
        assert!(draft.at("src/b.rs", Side::New, 29).is_empty());
    }

    #[test]
    fn the_markdown_says_what_will_be_sent() {
        let mut draft = Draft::new(141, now());
        draft.set_decision(Some(Decision::RequestChanges), now());
        draft.set_body("One thing to fix.", now());
        draft.add(comment("src/a.rs", 31, "this rounds up"), now());
        draft.head_sha = Some("deadbeef".to_owned());
        let markdown = draft.to_markdown();
        assert!(markdown.contains("# Review draft for #141"));
        assert!(markdown.contains("**Decision:** request changes"));
        assert!(markdown.contains("**Body:** One thing to fix."));
        assert!(markdown.contains("deadbeef"));
        assert!(markdown.contains("`src/a.rs:31 (new)`"));
        assert!(markdown.contains("this rounds up"));
        assert!(markdown.contains("## Comments (1)"));
    }

    #[test]
    fn the_summary_counts_comments_the_way_a_person_says_them() {
        let mut draft = Draft::new(141, now());
        assert_eq!(draft.summary(), "0 comments");
        draft.add(comment("src/a.rs", 1, "why?"), now());
        assert_eq!(draft.summary(), "1 comment");
        draft.add(comment("src/b.rs", 1, "why?"), now());
        assert_eq!(draft.summary(), "2 comments");
    }

    #[test]
    fn an_over_long_body_is_refused_with_its_size() {
        let long = "x".repeat(MAX_TEXT_BYTES + 1);
        assert_eq!(
            DraftComment::new("src/a.rs", Side::New, 1, None, long.clone()),
            Err(DraftError::TooLong {
                bytes: MAX_TEXT_BYTES + 1
            })
        );
        // Exactly at the limit is allowed: GitHub rejects *more* than it.
        let at_limit = "x".repeat(MAX_TEXT_BYTES);
        assert!(DraftComment::new("src/a.rs", Side::New, 1, None, at_limit).is_ok());
    }
}
