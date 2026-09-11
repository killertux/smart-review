//! Layout helpers (FR-7.8).

use ratatui::layout::Rect;

/// Smallest terminal width smart-review is designed for (FR-7.8).
pub const MIN_WIDTH: u16 = 80;
/// Smallest terminal height smart-review is designed for (FR-7.8).
pub const MIN_HEIGHT: u16 = 24;

/// Whether the terminal is too small to render the interface.
#[must_use]
pub const fn is_too_small(area: Rect) -> bool {
    area.width < MIN_WIDTH || area.height < MIN_HEIGHT
}

/// A rectangle of `width` x `height` centred inside `area`, clamped to fit.
#[must_use]
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn detects_small_terminals() {
        assert!(is_too_small(area(79, 24)));
        assert!(is_too_small(area(80, 23)));
        assert!(!is_too_small(area(80, 24)));
        assert!(!is_too_small(area(200, 60)));
    }

    #[test]
    fn centring_keeps_the_popup_inside_the_area() {
        let centred = centered(area(100, 40), 60, 20);
        assert_eq!(centred.width, 60);
        assert_eq!(centred.height, 20);
        assert_eq!(centred.x, 20);
        assert_eq!(centred.y, 10);
        assert!(centred.right() <= 100);
        assert!(centred.bottom() <= 40);
    }

    #[test]
    fn centring_clamps_when_the_area_is_smaller() {
        let centred = centered(area(30, 10), 60, 20);
        assert_eq!(centred.width, 30);
        assert_eq!(centred.height, 10);
        assert_eq!(centred.x, 0);
        assert_eq!(centred.y, 0);
    }
}
