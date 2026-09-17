//! The conversation a pull request already has, as the interface walks it (FR-6.4).
//!
//! The comments themselves live in [`crate::domain::pr::PullRequestDetail`], because
//! they are read with the rest of the detail. What lives here is only the *interface*
//! around them: whether the panel is open, where its cursor is, how far it is scrolled,
//! and which resolve is in flight. No file, no network, no clock — the same rule as
//! every other pane's state, so it can be tested by pressing keys.
//!
//! The composer is deliberately **not** here. There is one compose box in this
//! application, and its target says what it is writing about: a line of the diff, a
//! thread that is already there, or the conversation. A second text box would be a
//! second place for `Enter` to mean something different.

/// The thread state shown in the Discussion tab (IR-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiscussionFilter {
    /// Every record.
    #[default]
    All,
    /// Current unresolved threads.
    Open,
    /// Resolved threads.
    Resolved,
    /// Threads whose anchor is no longer current.
    Outdated,
}

impl DiscussionFilter {
    /// Label shown in the tab title.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Outdated => "outdated",
        }
    }

    /// The next visible filter.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::All => Self::Open,
            Self::Open => Self::Resolved,
            Self::Resolved => Self::Outdated,
            Self::Outdated => Self::All,
        }
    }
}

/// What the conversation panel is doing (FR-6.4).
#[derive(Debug, Clone, Default)]
pub struct DiscussionState {
    /// Whether the panel is what the overlay shows.
    pub panel: bool,
    /// The comment the cursor is on, one-based, as the panel numbers them.
    pub cursor: usize,
    /// Root comment selected in the full Discussion destination (IR-10).
    pub selected_root: Option<u64>,
    /// How far the panel is scrolled from the top.
    pub scroll: usize,
    /// Which inline-thread state is displayed in the Discussion tab.
    pub filter: DiscussionFilter,
    /// The cursor for which `scroll` was last made visible.
    visible_cursor: usize,
    /// The job id of the resolve in flight.
    ///
    /// Its own field rather than shared with the draft or the chat: three things can be
    /// in flight at once from this screen, and one field for two of them drops the
    /// completion of whichever started first.
    pub job: u64,
}

impl DiscussionState {
    /// Opens the panel on its newest comment.
    ///
    /// The newest, because a conversation is read from the end: the last thing said is
    /// what the reader came for, and the panel scrolls back from there.
    pub fn open(&mut self, count: usize) {
        self.panel = true;
        self.cursor = count.max(1);
        self.scroll = 0;
        self.visible_cursor = 0;
    }

    /// Forgets the panel, keeping nothing.
    pub fn close(&mut self) {
        self.panel = false;
        self.cursor = 1;
        self.scroll = 0;
        self.visible_cursor = 0;
    }

    /// Moves the cursor, keeping it inside the list.
    pub fn move_cursor(&mut self, delta: isize, count: usize) {
        if count == 0 {
            self.cursor = 1;
            return;
        }
        let current = isize::try_from(self.cursor).unwrap_or(1);
        let last = isize::try_from(count).unwrap_or(isize::MAX);
        self.cursor = usize::try_from((current + delta).clamp(1, last)).unwrap_or(1);
    }

    /// Keeps the cursor inside the list after the list changed under it.
    pub fn clamp(&mut self, count: usize) {
        self.cursor = self.cursor.clamp(1, count.max(1));
    }

    /// The index into the comment list that the cursor is on, if there is one.
    #[must_use]
    pub fn index(&self) -> Option<usize> {
        self.cursor.checked_sub(1)
    }

    /// Whether a cursor movement needs its wrapped comment brought into view.
    #[must_use]
    pub const fn needs_visibility_sync(&self) -> bool {
        self.cursor != self.visible_cursor
    }

    /// Records that the current cursor is visible at the supplied offset.
    pub fn set_visible(&mut self, offset: usize) {
        self.scroll = offset;
        self.visible_cursor = self.cursor;
    }

    /// Advances the Discussion tab's filter.
    pub fn cycle_filter(&mut self) {
        self.filter = self.filter.next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_opens_on_the_newest_comment() {
        let mut state = DiscussionState::default();
        state.open(4);
        assert!(state.panel);
        assert_eq!(state.cursor, 4, "a conversation is read from the end");
        assert_eq!(state.index(), Some(3));
    }

    #[test]
    fn an_empty_conversation_still_has_a_cursor_that_is_not_an_index() {
        // Pull request zero is not a pull request, and comment zero is not a comment:
        // the cursor is one-based, and an empty list must not offer an index into it.
        let mut state = DiscussionState::default();
        state.open(0);
        assert_eq!(state.cursor, 1);
        assert_eq!(state.index(), Some(0));
        state.move_cursor(-5, 0);
        assert_eq!(state.cursor, 1, "and it cannot move off an empty list");
    }

    #[test]
    fn moving_stops_at_both_ends() {
        let mut state = DiscussionState::default();
        state.open(3);
        state.move_cursor(9, 3);
        assert_eq!(state.cursor, 3);
        state.move_cursor(-9, 3);
        assert_eq!(state.cursor, 1);
        state.move_cursor(1, 3);
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn a_shorter_list_pulls_the_cursor_back_inside_it() {
        // Comments do not usually disappear, but a refresh after somebody deleted one
        // can make the list shorter, and a cursor past the end would draw nothing and
        // reply to nothing.
        let mut state = DiscussionState::default();
        state.open(5);
        state.clamp(2);
        assert_eq!(state.cursor, 2);
        state.clamp(0);
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn closing_forgets_the_cursor_as_well_as_the_panel() {
        let mut state = DiscussionState::default();
        state.open(5);
        state.close();
        assert!(!state.panel);
        assert_eq!(state.cursor, 1);
        assert_eq!(state.scroll, 0);
    }
}
