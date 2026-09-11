//! Putting text on the user's clipboard without a clipboard library (FR-3.4).
//!
//! `:copy-path` uses OSC 52, which asks the *terminal* to set the clipboard. It
//! needs no dependency, works over SSH where a real clipboard library cannot, and
//! degrades to doing nothing on a terminal that does not implement it — which is why
//! the caller always reports what it did rather than claiming success silently.

/// The base64 alphabet, standard indexing, with padding.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes bytes as standard base64 with padding.
///
/// Hand-rolled rather than a dependency: the whole requirement is thirty lines, and
/// a base64 crate would be a new approval for it (DEV-2).
#[must_use]
pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = chunk.get(1).copied().map_or(0, u32::from);
        let third = chunk.get(2).copied().map_or(0, u32::from);
        let group = (first << 16) | (second << 8) | third;

        out.push(char::from(ALPHABET[((group >> 18) & 0x3f) as usize]));
        out.push(char::from(ALPHABET[((group >> 12) & 0x3f) as usize]));
        if chunk.len() > 1 {
            out.push(char::from(ALPHABET[((group >> 6) & 0x3f) as usize]));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(ALPHABET[(group & 0x3f) as usize]));
        } else {
            out.push('=');
        }
    }
    out
}

/// The escape sequence that asks the terminal to set the clipboard.
///
/// `c` selects the clipboard rather than the primary selection, which is what a
/// paste will use.
#[must_use]
pub fn osc52(text: &str) -> String {
    format!("\u{1b}]52;c;{}\u{7}", base64(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_matches_the_standard_vectors() {
        // RFC 4648's examples, so a broken implementation cannot pass by accident.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_two_non_ascii_vectors_encode_correctly() {
        // '+' and '/' are the two characters an alphabet mistake gets wrong.
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
        assert_eq!(base64(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(base64(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn a_path_survives_the_trip() {
        assert_eq!(
            base64(b"src/domain/invoice.rs"),
            "c3JjL2RvbWFpbi9pbnZvaWNlLnJz"
        );
    }

    #[test]
    fn the_sequence_names_the_clipboard_and_is_terminated() {
        let sequence = osc52("a");
        assert!(sequence.starts_with("\u{1b}]52;c;"));
        assert!(sequence.ends_with('\u{7}'));
        assert!(sequence.contains("YQ=="), "{sequence:?}");
        // No newline: the sequence is written into the middle of the screen.
        assert!(!sequence.contains('\n'));
    }

    #[test]
    fn an_empty_string_still_produces_a_valid_sequence() {
        assert_eq!(osc52(""), "\u{1b}]52;c;\u{7}");
    }
}
