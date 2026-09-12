//! A single-line-or-multi-line text buffer with a cursor (FR-5.2).
//!
//! Chat is the first place the app asks the user to *write* something longer than a
//! command, so it needs editing that is not a command line: `Enter` sends and a
//! modified `Enter` inserts a newline, which means the buffer has lines, and a cursor
//! that can be moved inside the text rather than only at the end.
//!
//! Deliberately not modal and not a hook for vim-style editing (FR-5.2 says so): the
//! keys are the ones every terminal user already has in their fingers, and every
//! operation here is a pure function of the buffer, which is what makes the behaviour
//! testable without a terminal.

/// A text buffer and a cursor into it.
///
/// The cursor is a byte offset kept on a character boundary. Nothing here reads the
/// clock, the terminal or the filesystem, so a test can drive it directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    cursor: usize,
    /// The column the cursor is *trying* to be on, while moving vertically.
    ///
    /// Without it, moving down through a short line leaves the cursor at that line's
    /// end and the next move continues from there, so a paragraph with one short line
    /// in it is impossible to walk down with the cursor staying under your eye. Cleared
    /// by every horizontal move and every edit, which is what makes it a goal rather
    /// than a second cursor.
    goal: Option<usize>,
}

impl TextInput {
    /// An empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cursor, as a byte offset.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether there is nothing to send.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// How many bytes are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Replaces the contents and puts the cursor at the end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.goal = None;
    }

    /// Empties the buffer.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.goal = None;
    }

    /// Inserts a character at the cursor.
    pub fn insert(&mut self, character: char) {
        self.text.insert(self.cursor, character);
        self.cursor += character.len_utf8();
        self.goal = None;
    }

    /// Inserts a string at the cursor.
    pub fn insert_str(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.goal = None;
    }

    /// Inserts a line break (FR-5.2's modified-`Enter`).
    pub fn newline(&mut self) {
        self.insert('\n');
    }

    /// Deletes the character before the cursor.
    pub fn backspace(&mut self) {
        let Some(previous) = self.text[..self.cursor].chars().next_back() else {
            return;
        };
        self.cursor -= previous.len_utf8();
        self.text.remove(self.cursor);
        self.goal = None;
    }

    /// Deletes the character under the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
        self.goal = None;
    }

    /// Deletes the word before the cursor, for `<C-w>`.
    pub fn delete_word(&mut self) {
        let start = word_start(&self.text, self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.goal = None;
    }

    /// Deletes the newline before the cursor, joining this line to the previous one.
    ///
    /// `Backspace` at the start of a line does the same thing, which is what makes a
    /// mis-typed newline recoverable without reaching for a kill ring.
    pub fn join_previous_line(&mut self) {
        self.backspace();
    }

    /// Moves one character left.
    pub fn left(&mut self) {
        if let Some(previous) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= previous.len_utf8();
        }
        self.goal = None;
    }

    /// Moves one character right.
    pub fn right(&mut self) {
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
        self.goal = None;
    }

    /// Moves to the start of the line the cursor is on.
    pub fn home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.goal = None;
    }

    /// Moves to the end of the line the cursor is on.
    pub fn end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index);
        self.goal = None;
    }

    /// Moves up a line, keeping the column where it can.
    pub fn up(&mut self) {
        let (row, _) = self.position();
        if row == 0 {
            return;
        }
        self.move_vertical(row - 1);
    }

    /// Moves down a line, keeping the column where it can.
    pub fn down(&mut self) {
        let (row, _) = self.position();
        if row + 1 >= self.lines().len() {
            return;
        }
        self.move_vertical(row + 1);
    }

    /// Moves one word left.
    pub fn word_left(&mut self) {
        self.cursor = word_start(&self.text, self.cursor);
        self.goal = None;
    }

    /// Moves one word right.
    pub fn word_right(&mut self) {
        self.cursor = word_end(&self.text, self.cursor);
        self.goal = None;
    }

    /// The lines of the buffer, without their terminators.
    #[must_use]
    pub fn lines(&self) -> Vec<&str> {
        self.text.split('\n').collect()
    }

    /// The cursor's `(row, column)`, both zero-based and counted in characters.
    #[must_use]
    pub fn position(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let row = before.matches('\n').count();
        let column = before
            .rsplit('\n')
            .next()
            .map_or(0, |line| line.chars().count());
        (row, column)
    }

    /// Moves to a row, aiming for the goal column rather than the current one.
    fn move_vertical(&mut self, row: usize) {
        let (_, column) = self.position();
        let goal = *self.goal.get_or_insert(column);
        self.move_to(row, goal);
        // `move_to` clamps to the line it landed on, but the goal stays where the user
        // put it: walking through a short line and out the other side must not lose the
        // column they were editing in.
        self.goal = Some(goal);
    }

    /// Puts the cursor at a `(row, column)`, clamped to what exists.
    fn move_to(&mut self, row: usize, column: usize) {
        let mut offset = 0;
        for (index, line) in self.text.split('\n').enumerate() {
            if index == row {
                let within: usize = line
                    .char_indices()
                    .nth(column)
                    .map_or(line.len(), |(offset, _)| offset);
                self.cursor = offset + within.min(line.len());
                return;
            }
            offset += line.len() + 1;
        }
        self.cursor = self.text.len();
    }
}

/// The byte offset of the start of the word before `cursor`.
fn word_start(text: &str, cursor: usize) -> usize {
    let before = &text[..cursor];
    let trimmed = before.trim_end();
    match trimmed.rfind(char::is_whitespace) {
        Some(index) => index + trimmed[index..].chars().next().map_or(1, char::len_utf8),
        None => 0,
    }
}

/// The byte offset just past the word after `cursor`.
fn word_end(text: &str, cursor: usize) -> usize {
    let after = &text[cursor..];
    let skipped = after.len() - after.trim_start().len();
    let rest = &after[skipped..];
    let length = rest.find(char::is_whitespace).unwrap_or(rest.len());
    cursor + skipped + length
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str) -> TextInput {
        let mut input = TextInput::new();
        input.set_text(text);
        input
    }

    #[test]
    fn typing_puts_text_where_the_cursor_is() {
        let mut input = TextInput::new();
        input.insert_str("hello");
        input.insert('!');
        assert_eq!(input.text(), "hello!");
        input.left();
        input.left();
        input.insert('?');
        assert_eq!(input.text(), "hell?o!");
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn backspace_and_delete_are_character_wise_not_byte_wise() {
        // `é` is two bytes, so a byte-wise delete would split it and the next insert
        // would panic inside `String::insert`.
        let mut input = input("héllo");
        input.home();
        input.right();
        input.right();
        input.backspace();
        assert_eq!(input.text(), "hllo", "the accented character went whole");
        input.home();
        input.delete();
        assert_eq!(input.text(), "llo");
        // At the ends they do nothing rather than panicking.
        input.end();
        input.delete();
        input.home();
        input.backspace();
        assert_eq!(input.text(), "llo");
    }

    #[test]
    fn a_modified_enter_adds_a_line_and_enter_sends_what_was_typed() {
        let mut input = input("first line");
        input.newline();
        input.insert_str("second line");
        assert_eq!(input.lines(), vec!["first line", "second line"]);
        // The buffer is what would be sent, with its newline intact.
        assert_eq!(input.text(), "first line\nsecond line");
        assert!(!input.is_empty());
    }

    #[test]
    fn an_empty_or_whitespace_buffer_is_not_something_to_send() {
        assert!(TextInput::new().is_empty());
        assert!(input("   \n  ").is_empty());
        assert!(!input("x").is_empty());
    }

    #[test]
    fn moving_by_line_keeps_the_column_the_user_aimed_at() {
        let mut input = input("abcdef\nxy\nlonger line");
        // The cursor starts at the end, and `home` is line-local, which is deliberate:
        // these are the moves a person makes while editing a paragraph.
        input.home();
        input.up();
        input.up();
        input.home();
        for _ in 0..3 {
            input.right();
        }
        assert_eq!(input.position(), (0, 3));
        input.down();
        assert_eq!(input.position(), (1, 2), "the short line clamps the column");
        input.down();
        assert_eq!(
            input.position(),
            (2, 3),
            "…but the column the user was on is not forgotten"
        );
        input.up();
        assert_eq!(input.position(), (1, 2));
        input.up();
        assert_eq!(input.position(), (0, 3));
        // A horizontal move is a new goal: the cursor is where it now is.
        input.right();
        input.down();
        assert_eq!(input.position(), (1, 2));
    }

    #[test]
    fn moving_past_the_ends_is_not_an_error() {
        let mut input = input("one line");
        input.home();
        input.up();
        input.left();
        assert_eq!(input.position(), (0, 0));
        input.end();
        input.down();
        input.right();
        assert_eq!(input.position(), (0, 8));
        input.word_right();
        assert_eq!(input.position(), (0, 8));
    }

    #[test]
    fn word_movement_stops_at_word_boundaries() {
        let mut input = input("does this break the retry?");
        assert_eq!(input.position(), (0, 26));
        input.word_left();
        assert_eq!(input.position(), (0, 20), "the start of `retry?`");
        input.word_left();
        assert_eq!(input.position(), (0, 16), "the start of `the`");
        input.word_right();
        assert_eq!(input.position(), (0, 19), "just past `the`");
        input.word_right();
        assert_eq!(input.position(), (0, 26), "past the last word, at the end");
    }

    #[test]
    fn deleting_a_word_takes_the_word_and_stops_at_it() {
        let mut at_end = input("does this break the");
        at_end.delete_word();
        assert_eq!(at_end.text(), "does this break ");
        assert_eq!(at_end.position(), (0, 16));
        // From the first word: the space that followed it stays, because the cursor is
        // a position the user put somewhere, not the tokenizer's idea of a word.
        let mut second = TextInput::new();
        second.set_text("does this break the");
        second.home();
        second.word_right();
        assert_eq!(second.position(), (0, 4), "on the space, not past it");
        second.delete_word();
        assert_eq!(second.text(), " this break the");
        assert_eq!(second.position(), (0, 0));
    }

    #[test]
    fn home_and_end_are_line_local() {
        let mut input = input("abc\ndef");
        input.end();
        assert_eq!(input.position(), (1, 3));
        input.home();
        assert_eq!(input.position(), (1, 0));
        input.up();
        assert_eq!(input.position(), (0, 0));
        input.end();
        assert_eq!(input.position(), (0, 3));
    }

    #[test]
    fn backspace_at_the_start_of_a_line_joins_it_to_the_previous_one() {
        let mut input = input("one\ntwo");
        input.home();
        input.join_previous_line();
        assert_eq!(input.text(), "onetwo");
        assert_eq!(input.position(), (0, 3));
    }

    #[test]
    fn clearing_leaves_an_empty_buffer_with_the_cursor_at_the_start() {
        let mut input = input("something");
        input.clear();
        assert_eq!(input.text(), "");
        assert_eq!(input.cursor(), 0);
        assert!(input.is_empty());
        assert_eq!(input.lines(), vec![""]);
    }

    #[test]
    fn a_multibyte_line_survives_every_move() {
        // Every offset the cursor can hold must be a character boundary, or the next
        // insert panics inside `String::insert`.
        let mut input = input("héllo 世界\nok");
        for _ in 0..40 {
            input.right();
            input.insert('x');
            input.backspace();
            input.word_right();
            input.insert('y');
            input.backspace();
        }
        input.down();
        input.end();
        input.insert('!');
        assert!(input.text().ends_with("ok!"), "{}", input.text());
    }
}
