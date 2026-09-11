//! Fuzzy matching, shared by command completion and PR search.
//!
//! One implementation, used by both, so that "fuzzy" means the same thing
//! everywhere: the typed characters must appear in order, and contiguous runs
//! score higher than scattered ones (FR-2.2, FR-7.4).

/// Scores `needle` against `haystack`, or `None` when they do not match.
///
/// Matching is case-insensitive. A higher score is a better match; the scale is
/// only meaningful relative to other candidates.
#[must_use]
pub fn score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }

    let characters: Vec<char> = haystack.chars().flat_map(char::to_lowercase).collect();
    let mut score: i32 = 0;
    let mut cursor = 0;
    let mut previous: Option<usize> = None;

    for wanted in needle.chars().flat_map(char::to_lowercase) {
        let found = cursor
            + characters
                .get(cursor..)?
                .iter()
                .position(|candidate| *candidate == wanted)?;
        score += 1;
        if previous == Some(found.wrapping_sub(1)) {
            score += 2;
        }
        cursor = found + 1;
        previous = Some(found);
    }

    // Prefer shorter candidates, then earlier matches.
    let length_penalty = i32::try_from(haystack.chars().count()).unwrap_or(i32::MAX);
    let cursor_penalty = i32::try_from(cursor).unwrap_or(i32::MAX);
    Some(score * 10 - length_penalty - cursor_penalty)
}

/// Whether `needle` matches `haystack` at all.
#[must_use]
pub fn matches(needle: &str, haystack: &str) -> bool {
    score(needle, haystack).is_some()
}

/// Whether a query written as words matches a haystack, requiring every word to
/// match somewhere.
///
/// `"retry hook"` matches `"retry the webhook dispatcher"` because both words
/// appear, in any order, which is what a user typing into a search box expects.
#[must_use]
pub fn matches_all_words(query: &str, haystack: &str) -> bool {
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return true;
    }
    words.iter().all(|word| matches(word, haystack))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contiguous_matches_beat_scattered_ones() {
        let contiguous = score("doc", "doctor").unwrap();
        let scattered = score("doc", "d-o-c-nonsense").unwrap();
        assert!(contiguous > scattered, "{contiguous} vs {scattered}");
    }

    #[test]
    fn shorter_candidates_win_all_else_being_equal() {
        let short = score("do", "doctor").unwrap();
        let long = score("do", "doctor-with-a-much-longer-name").unwrap();
        assert!(short > long, "{short} vs {long}");
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(matches("INVOICE", "src/domain/invoice.rs"));
        assert!(matches("invoice", "src/domain/INVOICE.rs"));
    }

    #[test]
    fn order_matters() {
        assert!(matches("inv", "invoice"));
        assert!(!matches("vni", "invoice"));
    }

    #[test]
    fn an_empty_needle_matches_anything() {
        assert_eq!(score("", "anything"), Some(0));
        assert!(matches_all_words("", "anything"));
        assert!(matches_all_words("   ", "anything"));
    }

    #[test]
    fn every_word_must_appear_but_the_order_is_free() {
        assert!(matches_all_words(
            "retry hook",
            "retry the webhook dispatcher"
        ));
        assert!(matches_all_words(
            "hook retry",
            "retry the webhook dispatcher"
        ));
        assert!(!matches_all_words("retry missing", "retry the webhook"));
    }
}
