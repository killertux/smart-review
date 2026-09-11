//! Time helpers shared by the list, the cache and the tests (FR-2.1, FR-2.3).

use chrono::{DateTime, Duration, Utc};

/// Every timestamp in the application is UTC.
///
/// GitHub reports timestamps with an offset; they are converted to UTC on the way
/// in, so comparisons and lifetimes never depend on the machine's timezone.
pub type Timestamp = DateTime<Utc>;

/// A timestamp from seconds since the Unix epoch.
///
/// Exists so that callers — tests, fixtures and the cache, which stores epoch
/// seconds — do not each have to know which date library is behind [`Timestamp`].
#[must_use]
pub fn from_unix_secs(seconds: i64) -> Timestamp {
    DateTime::from_timestamp(seconds, 0).unwrap_or_default()
}

/// Formats the gap between two instants the way a PR list does: short, and never
/// more precise than it needs to be.
///
/// A timestamp slightly in the future (a clock that moved, or a server that is
/// ahead) is reported as "just now" rather than as a negative age.
#[must_use]
pub fn relative(now: Timestamp, then: Timestamp) -> String {
    let seconds = (now - then).num_seconds();
    if seconds < 60 {
        return "just now".to_owned();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    if days < 30 {
        return format!("{}w", days / 7);
    }
    if days < 365 {
        return format!("{}mo", days / 30);
    }
    format!("{}y", days / 365)
}

/// How old something is, in seconds, clamped at zero so a clock that moved
/// backwards cannot make a cache entry look infinitely old or brand new.
#[must_use]
pub fn age_secs(now: Timestamp, then: Timestamp) -> u64 {
    (now - then).num_seconds().max(0).unsigned_abs()
}

/// Whether `then` is older than `ttl` relative to `now` (FR-2.3).
#[must_use]
pub fn is_stale(now: Timestamp, then: Timestamp, ttl: Duration) -> bool {
    now - then > ttl
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(seconds: i64) -> Timestamp {
        Utc.timestamp_opt(1_700_000_000 + seconds, 0).unwrap()
    }

    #[test]
    fn relative_times_read_like_a_pr_list() {
        let now = at(0);
        assert_eq!(relative(now, at(0)), "just now");
        assert_eq!(relative(now, at(-30)), "just now");
        assert_eq!(relative(now, at(-60)), "1m");
        assert_eq!(relative(now, at(-59 * 60)), "59m");
        assert_eq!(relative(now, at(-3600)), "1h");
        assert_eq!(relative(now, at(-23 * 3600)), "23h");
        assert_eq!(relative(now, at(-24 * 3600)), "1d");
        assert_eq!(relative(now, at(-6 * 24 * 3600)), "6d");
        assert_eq!(relative(now, at(-7 * 24 * 3600)), "1w");
        assert_eq!(relative(now, at(-29 * 24 * 3600)), "4w");
        assert_eq!(relative(now, at(-30 * 24 * 3600)), "1mo");
        assert_eq!(relative(now, at(-400 * 24 * 3600)), "1y");
    }

    #[test]
    fn a_future_timestamp_is_not_a_negative_age() {
        assert_eq!(relative(at(0), at(600)), "just now");
        assert_eq!(age_secs(at(0), at(600)), 0);
    }

    #[test]
    fn staleness_is_a_strict_comparison() {
        let ttl = Duration::seconds(60);
        assert!(!is_stale(at(0), at(-60), ttl), "exactly the TTL is fresh");
        assert!(is_stale(at(0), at(-61), ttl));
        assert!(is_stale(at(0), at(-3600), ttl));
    }
}
