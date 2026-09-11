//! System clock (ARCH-3).

use crate::ports::Clock;

/// Reads the real system clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_secs(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_plausible_time() {
        // Later than 2020-01-01, so a broken implementation returning 0 fails.
        assert!(SystemClock.now_unix_secs() > 1_577_836_800);
    }
}
