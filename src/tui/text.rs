//! Text fitting for the panes (NFR-1.3, FR-7.8).
//!
//! `str::len` is bytes and `chars().count()` is code points; neither is columns. A
//! title containing CJK characters or an emoji is *wider* than its character count,
//! and a diff line containing a combining accent is narrower, so the two operations
//! that matter — truncating to a width and padding to a width — go through
//! `unicode-width` and nowhere else.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// What is appended when text has to be cut.
pub const ELLIPSIS: char = '…';

/// Cuts `text` to at most `width` columns, marking a cut with an ellipsis.
///
/// A double-width character that would straddle the boundary is dropped rather than
/// half-drawn, and the ellipsis is accounted for inside `width`.
#[must_use]
pub fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }

    // One column is spent on the ellipsis.
    let budget = width - 1;
    let mut out = String::new();
    let mut used = 0;
    for character in text.chars() {
        let char_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + char_width > budget {
            break;
        }
        out.push(character);
        used += char_width;
    }
    out.push(ELLIPSIS);
    out
}

/// Pads `text` with spaces to exactly `width` columns, cutting it if it is longer.
#[must_use]
pub fn pad(text: &str, width: usize) -> String {
    let text = truncate(text, width);
    let used = UnicodeWidthStr::width(text.as_str());
    let mut out = text;
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(used)));
    out
}

/// Pads on the left, which is what a numeric gutter needs.
#[must_use]
pub fn pad_left(text: &str, width: usize) -> String {
    let text = truncate(text, width);
    let used = UnicodeWidthStr::width(text.as_str());
    let mut out = String::new();
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(used)));
    out.push_str(&text);
    out
}

/// The display width of `text` in columns.
#[must_use]
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Breaks `text` into pieces of at most `width` columns, without splitting a
/// double-width character.
///
/// Used for the diff's split view, where a long line has to be shown inside half the
/// pane rather than being lost.
#[must_use]
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for character in text.chars() {
        let char_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + char_width > width && !current.is_empty() {
            pieces.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(character);
        used += char_width;
    }
    pieces.push(current);
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_untouched() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis_inside_the_width() {
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(width(&truncate("hello world", 8)), 8);
        assert_eq!(truncate("hello", 1), "…");
    }

    #[test]
    fn a_zero_width_leaves_nothing() {
        assert_eq!(truncate("hello", 0), "");
        assert_eq!(pad("hello", 0), "");
        assert!(wrap("hello", 0).is_empty());
    }

    #[test]
    fn wide_characters_are_measured_in_columns_not_characters() {
        // Three CJK characters are six columns wide.
        assert_eq!(width("你好吗"), 6);
        assert_eq!(truncate("你好吗", 6), "你好吗");
        assert_eq!(truncate("你好吗", 5), "你好…");
        assert_eq!(width(&truncate("你好吗", 5)), 5);

        // An emoji is two columns.
        assert_eq!(width("🎉"), 2);
        // A cut is always *marked*: filling the width exactly with two emoji would
        // fit, but would silently hide that there was a third.
        assert_eq!(truncate("🎉🎉🎉", 4), "🎉…");
        assert_eq!(width(&truncate("🎉🎉🎉", 4)), 3);
        assert_eq!(truncate("🎉🎉🎉", 3), "🎉…");
        assert_eq!(
            truncate("🎉🎉", 4),
            "🎉🎉",
            "nothing was cut, so nothing is marked"
        );
    }

    #[test]
    fn a_double_width_character_is_never_half_drawn() {
        // Two columns of budget minus the ellipsis leaves one, which cannot hold a
        // two-column character, so the result is just the ellipsis.
        assert_eq!(truncate("你好", 2), "…");
        assert_eq!(width(&truncate("你好", 2)), 1);
    }

    #[test]
    fn padding_reaches_exactly_the_width() {
        assert_eq!(pad("ab", 5), "ab   ");
        assert_eq!(width(&pad("ab", 5)), 5);
        assert_eq!(pad("abcdef", 3), "ab…");
        assert_eq!(
            width(&pad("您好", 5)),
            5,
            "padded by columns, not characters"
        );
    }

    #[test]
    fn left_padding_lines_up_numbers() {
        assert_eq!(pad_left("7", 3), "  7");
        assert_eq!(pad_left("12345", 3), "12…");
        assert_eq!(width(&pad_left("您好", 5)), 5);
    }

    #[test]
    fn wrapping_keeps_every_character_and_respects_the_width() {
        let pieces = wrap("hello world", 5);
        assert_eq!(pieces, vec!["hello", " worl", "d"]);
        assert_eq!(pieces.concat(), "hello world");

        let pieces = wrap("你好吗", 4);
        assert_eq!(pieces, vec!["你好", "吗"]);
        assert!(pieces.iter().all(|piece| width(piece) <= 4));

        assert_eq!(wrap("", 5), vec![String::new()]);
    }

    #[test]
    fn a_very_narrow_wrap_still_makes_progress() {
        // One column cannot hold a two-column character, but it must not loop
        // forever either.
        let pieces = wrap("你好", 1);
        assert_eq!(pieces.concat(), "你好");
        assert!(pieces.len() >= 2);
    }

    #[test]
    fn combining_and_zero_width_characters_do_not_add_columns() {
        // A zero-width joiner and a combining accent take no columns.
        assert_eq!(width("a\u{0301}"), 1);
        assert_eq!(width("\u{200d}"), 0);
        assert_eq!(truncate("a\u{0301}b", 2), "a\u{0301}b");
    }
}
