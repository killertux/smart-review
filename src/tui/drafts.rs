//! The draft as the interface holds it: staged comments, the composer, the modal.
//!
//! Everything here is pure. The document itself lives in
//! [`crate::domain::draft::Draft`], edits are ordinary method calls, and the *loop*
//! is what writes it to disk — which is why this module has no `std::fs` in it and
//! why a keypress can never block on a file.
//!
//! The state is deliberately one struct rather than a field per feature: a comment
//! staged, a modal opened and a job started all describe one thing the user is doing,
//! and they change together.

use crate::domain::draft::{Decision, Draft, DraftComment, DraftError, Side};
use crate::domain::time::Timestamp;
use crate::tui::input::TextInput;

/// Where a comment will be anchored (FR-6.2).
///
/// Taken from the diff row under the cursor, which is the only place these three
/// facts agree with what the user is looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// The file, as the diff names it.
    pub path: String,
    /// Which side of the hunk the line numbers count from.
    pub side: Side,
    /// The last line of the anchor.
    pub line: u32,
    /// The first line, for a range.
    pub start_line: Option<u32>,
}

impl Anchor {
    /// An anchor for one line.
    #[must_use]
    pub fn line(path: impl Into<String>, side: Side, line: u32) -> Self {
        Self {
            path: path.into(),
            side,
            line,
            start_line: None,
        }
    }

    /// The anchor covering `other` as well as itself, if it can (FR-6.2).
    ///
    /// A range has to be one file and one side: a selection that crossed a file
    /// boundary, or that ran from a deletion to an addition, is not something GitHub
    /// can anchor, and refusing it here is a sentence instead of a 422.
    #[must_use]
    pub fn extended_to(&self, other: &Self) -> Option<Self> {
        if self.path != other.path || self.side != other.side {
            return None;
        }
        let (start, end) = if self.line <= other.line {
            (self.line, other.line)
        } else {
            (other.line, self.line)
        };
        Some(Self {
            path: self.path.clone(),
            side: self.side,
            line: end,
            start_line: (start != end).then_some(start),
        })
    }

    /// The anchor as it is shown above the composer.
    #[must_use]
    pub fn label(&self) -> String {
        let side = self.side.label();
        match self.start_line {
            Some(start) => format!("{}:{start}-{} ({side})", self.path, self.line),
            None => format!("{}:{} ({side})", self.path, self.line),
        }
    }

    /// How many lines the anchor covers.
    #[must_use]
    pub fn lines(&self) -> u32 {
        self.line - self.start_line.unwrap_or(self.line) + 1
    }
}

/// The comment being written (FR-6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composer {
    /// What the comment will be anchored to.
    pub anchor: Anchor,
    /// The text so far.
    pub input: TextInput,
    /// Why the last attempt to stage it was refused.
    pub refusal: Option<String>,
}

impl Composer {
    /// Opens a composer for an anchor.
    #[must_use]
    pub fn new(anchor: Anchor) -> Self {
        Self {
            anchor,
            input: TextInput::new(),
            refusal: None,
        }
    }

    /// The text, trimmed of the trailing newline a paste tends to leave.
    #[must_use]
    pub fn body(&self) -> &str {
        self.input.text().trim_end()
    }
}

/// What the review actions are doing (FR-6.1–FR-6.3).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DraftStatus {
    /// Nothing in flight.
    #[default]
    Idle,
    /// A publish is in flight; the modal says so and the key does nothing.
    Publishing,
    /// The last publish failed, with the reason (FR-6.3).
    Failed {
        /// What GitHub or the network said.
        reason: String,
    },
}

impl DraftStatus {
    /// Whether a publish is in flight, which is what disables the button (FR-6.3).
    #[must_use]
    pub fn is_publishing(&self) -> bool {
        matches!(self, Self::Publishing)
    }

    /// One line for the status area.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Idle => "idle".to_owned(),
            Self::Publishing => "publishing…".to_owned(),
            Self::Failed { reason } => format!("failed: {reason}"),
        }
    }
}

/// Everything the review actions own (FR-6.1–FR-6.3).
///
/// Five independent yes/no questions live here — is a pull request open, is the panel
/// shown, is the modal shown, has the confirm been given twice, is there something to
/// write — and they are read one at a time by different code. Folding them into a
/// state machine would be a bigger change than the problem, so the lint is answered
/// rather than obeyed.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub struct DraftState {
    /// The draft for the open pull request.
    pub draft: Draft,
    /// Whether there is a pull request open to draft against.
    pub open: bool,
    /// The comment being written, when the composer is open (FR-6.2).
    pub composer: Option<Composer>,
    /// Where a range selection started, when one is in progress (FR-6.2).
    pub selection: Option<Anchor>,
    /// Whether the draft panel is what the overlay shows (FR-6.1).
    pub panel: bool,
    /// Whether the publish modal is open (FR-6.3).
    pub modal: bool,
    /// Whether the modal's second confirm has been given (FR-6.3).
    pub armed: bool,
    /// What the last action is doing.
    pub status: DraftStatus,
    /// The job id of the publish in flight (FR-6.3).
    pub job: u64,
    /// The number in the draft panel the user is on, for `remove`.
    pub cursor: usize,
    /// Whether the draft has changed since it was last written.
    ///
    /// The reducer sets it; the loop writes and clears it. That split is what keeps
    /// file system work out of the reducer without hiding it in a background thread
    /// that the user cannot see.
    pub dirty: bool,
    /// A draft that could not be read, or a save that failed (FR-9.1).
    pub warning: Option<String>,
    /// How far the modal is scrolled.
    pub scroll: usize,
}

impl Default for DraftState {
    fn default() -> Self {
        Self {
            // Pull request zero is not a pull request: it is what "nothing is open"
            // looks like in a field that is a `Draft` rather than an `Option`, so that
            // every renderer and every edit does not have to unwrap one.
            draft: Draft::new(0, Timestamp::default()),
            open: false,
            composer: None,
            selection: None,
            panel: false,
            modal: false,
            armed: false,
            status: DraftStatus::Idle,
            job: 0,
            cursor: 1,
            dirty: false,
            warning: None,
            scroll: 0,
        }
    }
}

impl DraftState {
    /// Adopts a draft for the open pull request (FR-6.1).
    pub fn open(&mut self, draft: Draft, warning: Option<String>) {
        self.draft = draft;
        self.open = true;
        self.panel = false;
        self.modal = false;
        self.armed = false;
        self.composer = None;
        self.selection = None;
        self.status = DraftStatus::Idle;
        self.cursor = 1;
        self.scroll = 0;
        self.dirty = false;
        self.warning = warning;
    }

    /// Forgets the pull request, keeping nothing (FR-6.1).
    pub fn close(&mut self) {
        self.draft = Draft::new(0, Timestamp::default());
        self.open = false;
        self.panel = false;
        self.modal = false;
        self.armed = false;
        self.composer = None;
        self.selection = None;
        self.status = DraftStatus::Idle;
        self.cursor = 1;
        self.scroll = 0;
        self.dirty = false;
        self.warning = None;
    }

    /// Whether there is a composer on screen.
    #[must_use]
    pub fn is_composing(&self) -> bool {
        self.composer.is_some()
    }

    /// Whether anything blocks the ordinary key handling (FR-7.1).
    #[must_use]
    pub fn takes_keyboard(&self) -> bool {
        self.composer.is_some() || self.modal
    }

    /// Opens the composer for one line (FR-6.2).
    pub fn compose(&mut self, anchor: Anchor) {
        self.composer = Some(Composer::new(anchor));
        self.selection = None;
    }

    /// Marks the start of a range, for `V` (FR-6.2).
    ///
    /// Two presses is the whole mechanism: `V` marks, `j`/`k` move, `c` writes. The
    /// alternative — letting the first `c` mark and the second finish — makes the key
    /// that opens the composer depend on history, which is the kind of hidden state
    /// that turns a review into a puzzle.
    pub fn start_selection(&mut self, anchor: Anchor) {
        self.selection = Some(anchor);
    }

    /// Opens the composer for a range (FR-6.2).
    ///
    /// # Errors
    ///
    /// Returns the reason the range cannot be made — a different file, or the other
    /// side of the hunk. The refusal is recorded on the composer, which is what the
    /// pane shows.
    pub fn compose_range(&mut self, start: &Anchor, end: Anchor) -> Result<(), String> {
        let Some(range) = start.extended_to(&end) else {
            let mut composer = Composer::new(end);
            let reason = format!(
                "a range has to be in one file and on one side — the selection starts at {}",
                start.label()
            );
            composer.refusal = Some(reason.clone());
            self.composer = Some(composer);
            self.selection = None;
            return Err(reason);
        };
        self.compose(range);
        Ok(())
    }

    /// Closes the composer, keeping nothing (FR-6.2).
    pub fn cancel_composer(&mut self) {
        self.composer = None;
        self.selection = None;
    }

    /// Stages what the composer holds (FR-6.1, FR-6.2).
    ///
    /// # Errors
    ///
    /// As [`DraftComment::new`]. The refusal is also recorded on the composer, which
    /// is what the pane shows: a refusal the user cannot see is a keypress that did
    /// nothing.
    pub fn stage(&mut self, now: Timestamp) -> Result<(), DraftError> {
        let Some(composer) = self.composer.as_mut() else {
            return Err(DraftError::EmptyBody);
        };
        let staged = DraftComment::new(
            composer.anchor.path.clone(),
            composer.anchor.side,
            composer.anchor.line,
            composer.anchor.start_line,
            composer.body(),
        );
        match staged {
            Ok(comment) => {
                self.draft.insert(comment, now);
                self.composer = None;
                self.selection = None;
                self.cursor = 1;
                self.dirty = true;
                Ok(())
            }
            Err(error) => {
                if let Some(composer) = self.composer.as_mut() {
                    composer.refusal = Some(error.to_string());
                }
                Err(error)
            }
        }
    }

    /// Removes a staged comment by its panel number (FR-6.1).
    pub fn remove(&mut self, number: usize, now: Timestamp) -> Option<DraftComment> {
        let removed = self.draft.remove(number, now);
        if removed.is_some() {
            self.dirty = true;
            self.clamp_cursor();
        }
        removed
    }

    /// Clears every staged comment, the decision and the body (FR-6.1).
    pub fn clear(&mut self, now: Timestamp) {
        self.draft.clear(now);
        self.dirty = true;
        self.cursor = 1;
        self.armed = false;
    }

    /// Records the decision (FR-6.1).
    pub fn set_decision(&mut self, decision: Option<Decision>, now: Timestamp) {
        self.draft.set_decision(decision, now);
        self.dirty = true;
    }

    /// Records the review body (FR-6.1).
    pub fn set_body(&mut self, body: impl Into<String>, now: Timestamp) {
        self.draft.set_body(body, now);
        self.dirty = true;
    }

    /// Remembers the commit the comments are written against (FR-6.3).
    pub fn anchor_to(&mut self, head_sha: &str) {
        if self.draft.head_sha.as_deref() == Some(head_sha) || head_sha.is_empty() {
            return;
        }
        self.draft.head_sha = Some(head_sha.to_owned());
        self.dirty = true;
    }

    /// Whether the draft has drifted from the commit on screen (FR-6.3).
    #[must_use]
    pub fn drifted(&self, head_sha: Option<&str>) -> bool {
        self.draft.drifted_from(head_sha)
    }

    /// The comments anchored at a line, for the gutter marker (FR-6.1).
    #[must_use]
    pub fn at(&self, path: &str, side: Side, line: u32) -> usize {
        self.draft.at(path, side, line).len()
    }

    /// Whether a row's line carries a staged comment (FR-6.1).
    #[must_use]
    pub fn marks(&self, path: &str, side: Option<Side>, line: Option<u32>) -> bool {
        let (Some(side), Some(line)) = (side, line) else {
            return false;
        };
        self.at(path, side, line) > 0
    }

    /// Moves the draft panel's cursor (FR-6.1).
    pub fn move_cursor(&mut self, delta: isize) {
        let count = isize::try_from(self.draft.comments.len()).unwrap_or(isize::MAX);
        if count == 0 {
            self.cursor = 1;
            return;
        }
        let current = isize::try_from(self.cursor).unwrap_or(1);
        self.cursor = usize::try_from((current + delta).clamp(1, count)).unwrap_or(1);
    }

    /// The comment the panel's cursor is on (FR-6.1).
    #[must_use]
    pub fn selected(&self) -> Option<&DraftComment> {
        self.draft.comments.get(self.cursor.checked_sub(1)?)
    }

    fn clamp_cursor(&mut self) {
        let count = self.draft.comments.len();
        self.cursor = self.cursor.clamp(1, count.max(1));
    }

    /// Opens the publish modal, or says why it will not (FR-6.3).
    ///
    /// # Errors
    ///
    /// As [`Draft::publishable`].
    pub fn open_modal(&mut self) -> Result<(), DraftError> {
        self.draft.publishable()?;
        self.modal = true;
        self.armed = false;
        self.scroll = 0;
        self.status = DraftStatus::Idle;
        Ok(())
    }

    /// Closes the modal (FR-6.3).
    pub fn close_modal(&mut self) {
        self.modal = false;
        self.armed = false;
        self.scroll = 0;
    }

    /// The status the publish in flight should end in (FR-6.3).
    pub fn published(&mut self, url: Option<&str>, now: Timestamp) {
        self.draft.clear(now);
        self.dirty = true;
        self.job = 0;
        self.status = DraftStatus::Idle;
        self.modal = false;
        self.armed = false;
        self.cursor = 1;
        self.warning = url.map(|url| format!("review posted: {url}"));
    }

    /// Records that the publish failed, keeping the draft (FR-6.3).
    pub fn publish_failed(&mut self, reason: impl Into<String>) {
        self.job = 0;
        self.armed = false;
        self.status = DraftStatus::Failed {
            reason: reason.into(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::time::from_unix_secs;

    fn now() -> Timestamp {
        from_unix_secs(1_700_000_000)
    }

    fn fresh() -> DraftState {
        let mut state = DraftState::default();
        state.open(Draft::new(141, now()), None);
        state
    }

    #[test]
    fn a_composer_stages_a_comment_on_the_line_it_was_opened_for() {
        let mut state = fresh();
        state.compose(Anchor::line("src/a.rs", Side::New, 31));
        state
            .composer
            .as_mut()
            .expect("open")
            .input
            .insert_str("this rounds up");
        state.stage(now()).expect("staged");

        assert!(state.composer.is_none(), "the composer closes on success");
        assert!(state.dirty, "and the loop is told to write");
        let comment = &state.draft.comments[0];
        assert_eq!(comment.path, "src/a.rs");
        assert_eq!(comment.side, Side::New);
        assert_eq!(comment.line, 31);
        assert_eq!(comment.body, "this rounds up");
    }

    #[test]
    fn an_empty_comment_is_refused_in_the_pane_not_at_the_api() {
        let mut state = fresh();
        state.compose(Anchor::line("src/a.rs", Side::New, 31));
        let error = state.stage(now()).expect_err("refused");
        assert_eq!(error, DraftError::EmptyBody);
        assert!(state.draft.comments.is_empty());
        assert!(
            state
                .composer
                .as_ref()
                .expect("still open")
                .refusal
                .is_some(),
            "and the reason is on screen"
        );
        assert!(!state.dirty, "a refusal writes nothing");
    }

    #[test]
    fn a_range_is_read_in_either_direction() {
        let mut state = fresh();
        let top = Anchor::line("src/a.rs", Side::New, 28);
        let bottom = Anchor::line("src/a.rs", Side::New, 31);

        state.start_selection(top.clone());
        assert!(state.selection.is_some(), "the first end waits");
        state
            .compose_range(&top.clone(), bottom.clone())
            .expect("a range");
        let composer = state.composer.as_ref().expect("a range");
        assert_eq!(composer.anchor.start_line, Some(28));
        assert_eq!(composer.anchor.line, 31);
        assert_eq!(composer.anchor.lines(), 4);

        // Bottom to top gives the same range.
        let mut state = fresh();
        state.start_selection(bottom.clone());
        state.compose_range(&bottom, top).expect("a range");
        let composer = state.composer.as_ref().expect("a range");
        assert_eq!(composer.anchor.start_line, Some(28));
        assert_eq!(composer.anchor.line, 31);
    }

    #[test]
    fn a_selection_across_files_is_refused_with_a_reason() {
        let mut state = fresh();
        let start = Anchor::line("src/a.rs", Side::New, 28);
        state.start_selection(start.clone());
        let refusal = state
            .compose_range(&start, Anchor::line("src/b.rs", Side::New, 3))
            .expect_err("refused");
        assert!(refusal.contains("one file"), "{refusal}");
        let composer = state.composer.as_ref().expect("the composer explains");
        assert_eq!(composer.anchor.path, "src/b.rs");
        assert_eq!(composer.anchor.start_line, None, "no range was made");
        assert!(composer.refusal.is_some(), "and the reason is on screen");
    }

    #[test]
    fn a_selection_across_sides_is_refused_too() {
        // An old-side line and a new-side line are not a range, whatever the numbers
        // say: they are different coordinate systems.
        let mut state = fresh();
        let start = Anchor::line("src/a.rs", Side::Old, 28);
        state.start_selection(start.clone());
        assert!(
            state
                .compose_range(&start, Anchor::line("src/a.rs", Side::New, 31))
                .is_err()
        );
        assert!(state.composer.as_ref().expect("one line").refusal.is_some());
    }

    #[test]
    fn cancelling_a_composer_keeps_the_draft() {
        let mut state = fresh();
        state.compose(Anchor::line("src/a.rs", Side::New, 31));
        state
            .composer
            .as_mut()
            .expect("open")
            .input
            .insert_str("half a thought");
        state.cancel_composer();
        assert!(state.composer.is_none());
        assert!(state.draft.is_empty(), "nothing was staged");
        assert!(!state.dirty);
    }

    #[test]
    fn removing_keeps_the_cursor_inside_the_list() {
        let mut state = fresh();
        for line in 1..=3 {
            state.compose(Anchor::line("src/a.rs", Side::New, line));
            state
                .composer
                .as_mut()
                .expect("open")
                .input
                .insert_str("why?");
            state.stage(now()).expect("staged");
        }
        assert_eq!(state.cursor, 1, "the newest comment is the first row");
        state.move_cursor(5);
        assert_eq!(state.cursor, 3);
        state.move_cursor(5);
        assert_eq!(state.cursor, 3, "and it stops at the end");
        state.remove(3, now()).expect("removed");
        assert_eq!(state.cursor, 2, "the cursor follows the list");
        state.clear(now());
        assert_eq!(state.cursor, 1, "and resets when there is nothing to be on");
    }

    #[test]
    fn a_modal_will_not_open_on_an_empty_draft() {
        let mut state = fresh();
        assert_eq!(state.open_modal(), Err(DraftError::NothingToSay));
        assert!(!state.modal);
        state.compose(Anchor::line("src/a.rs", Side::New, 1));
        state
            .composer
            .as_mut()
            .expect("open")
            .input
            .insert_str("why?");
        state.stage(now()).expect("staged");
        assert_eq!(state.open_modal(), Ok(()));
        assert!(state.modal);
    }

    #[test]
    fn publishing_twice_is_prevented_by_the_status_not_by_hope() {
        let mut state = fresh();
        state.compose(Anchor::line("src/a.rs", Side::New, 1));
        state
            .composer
            .as_mut()
            .expect("open")
            .input
            .insert_str("why?");
        state.stage(now()).expect("staged");
        state.open_modal().expect("publishable");

        state.status = DraftStatus::Publishing;
        state.job = 7;
        assert!(
            state.status.is_publishing(),
            "the second Enter does nothing"
        );

        state.published(Some("https://example.test/review/7"), now());
        assert!(state.draft.is_empty(), "a success clears the draft");
        assert!(!state.modal, "and closes the modal");
        assert_eq!(state.job, 0);
        assert!(state.dirty);
    }

    #[test]
    fn a_failed_publish_keeps_the_draft_and_the_reason() {
        let mut state = fresh();
        state.compose(Anchor::line("src/a.rs", Side::New, 1));
        state
            .composer
            .as_mut()
            .expect("open")
            .input
            .insert_str("why?");
        state.stage(now()).expect("staged");
        state.open_modal().expect("publishable");

        state.publish_failed("the token is not allowed");
        assert_eq!(state.draft.comments.len(), 1);
        assert!(state.modal, "the modal stays open: the draft is still here");
        assert!(!state.armed, "and the confirmation is forgotten");
        assert_eq!(
            state.status.label(),
            "failed: the token is not allowed",
            "in the pane, not in a notice that expires"
        );
    }

    #[test]
    fn the_gutter_asks_about_the_line_it_is_drawing() {
        let mut state = fresh();
        state.compose(Anchor::line("src/a.rs", Side::New, 31));
        state
            .composer
            .as_mut()
            .expect("open")
            .input
            .insert_str("why?");
        state.stage(now()).expect("staged");

        assert!(state.marks("src/a.rs", Some(Side::New), Some(31)));
        assert!(!state.marks("src/a.rs", Some(Side::Old), Some(31)));
        assert!(!state.marks("src/a.rs", Some(Side::New), Some(30)));
        assert!(
            !state.marks("src/a.rs", Some(Side::New), None),
            "a header row has no line to anchor to"
        );
    }

    #[test]
    fn a_closed_draft_has_no_pull_request_to_draft_against() {
        let mut state = fresh();
        state.close();
        assert!(!state.open);
        assert!(state.draft.is_empty());
        assert!(state.draft.pr == 0, "no pull request, no number");
        assert!(!state.takes_keyboard());
    }

    #[test]
    fn the_head_is_remembered_and_compared() {
        let mut state = fresh();
        state.anchor_to("abc123");
        assert_eq!(state.draft.head_sha.as_deref(), Some("abc123"));
        assert!(state.dirty);
        assert!(state.drifted(Some("def456")), "the diff has moved");
        assert!(!state.drifted(Some("abc123")));
        assert!(
            !state.drifted(None),
            "a diff that is not open cannot have moved"
        );

        state.dirty = false;
        state.anchor_to("abc123");
        assert!(!state.dirty, "the same head is not a change");
    }
}
