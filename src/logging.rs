//! Logging to a rotated file inside the smart-review home (FR-9.2).
//!
//! Secrets never reach this module's callers: nothing here filters content, so
//! callers must not log keys, tokens or file contents (NFR-3.1).
//!
//! A small logger keeps content redaction and file rotation in one place. Replacing it
//! with another logging backend would not change call sites.

use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::error::Error;
use crate::paths::Home;

/// Rotate once the active file passes this size.
const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Number of rotated files to keep, so the log directory stays bounded.
const KEEP: usize = 5;

/// Severity of a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    /// Parses a level name, case-insensitively.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" | "err" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }

    /// The canonical lowercase name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug)]
struct Logger {
    level: Level,
    file: Mutex<std::fs::File>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Initialises logging. Precedence: `--log-level`, then `RUST_LOG`, then the
/// configured level, then `info`.
///
/// # Errors
///
/// Returns an error when the log file cannot be rotated or opened.
pub fn init(home: &Home, configured: &str, override_level: Option<&str>) -> Result<Level, Error> {
    let from_env = std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| Level::parse(&v));
    let level = override_level
        .and_then(Level::parse)
        .or(from_env)
        .or_else(|| Level::parse(configured))
        .unwrap_or(Level::Info);

    let path = home.log_file();
    rotate_if_needed(&path)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| Error::io("open log file", &path, source))?;

    // Logs record paths and command lines, so keep them owner-only (NFR-3.1).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    let _ = LOGGER.set(Logger {
        level,
        file: Mutex::new(file),
    });

    log(
        Level::Info,
        format!("smart-review started (log level {level})"),
    );
    Ok(level)
}

/// Writes a line if logging has been initialised and `level` is enabled.
pub fn log(level: Level, message: impl AsRef<str>) {
    if let Some(logger) = LOGGER.get() {
        logger.write(level, message.as_ref());
    }
}

/// The active level, if logging has been initialised.
pub fn level() -> Option<Level> {
    LOGGER.get().map(|logger| logger.level)
}

impl Logger {
    fn write(&self, level: Level, message: &str) {
        if level > self.level {
            return;
        }
        // A poisoned lock means another thread panicked mid-write; dropping the
        // log line is better than panicking again (FR-9.1).
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{} {:<5} {}", timestamp(), level.as_str(), message);
        }
    }
}

fn rotate_if_needed(path: &std::path::Path) -> Result<(), Error> {
    let too_big = std::fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_BYTES);
    if !too_big {
        return Ok(());
    }

    for index in (1..KEEP).rev() {
        let from = rotated_path(path, index);
        let to = rotated_path(path, index + 1);
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    let first = rotated_path(path, 1);
    std::fs::rename(path, &first).map_err(|source| Error::io("rotate log file", path, source))?;
    Ok(())
}

fn rotated_path(path: &std::path::Path, index: usize) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(format!(".{index}"));
    path.with_file_name(name)
}

/// Current time as `YYYY-MM-DDTHH:MM:SSZ`, computed from the Unix epoch so no
/// date library is needed.
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let (year, month, day, hour, minute, second) = civil_from_unix(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400).cast_signed();
    let rest = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    (
        year,
        month,
        day,
        (rest / 3600) as u32,
        ((rest % 3600) / 60) as u32,
        (rest % 60) as u32,
    )
}

/// Howard Hinnant's `civil_from_days` algorithm.
///
/// The casts are exact by construction: `day_of_year` is in `0..=365` and
/// `month_position` in `0..=11`, so nothing is truncated.
#[allow(clippy::cast_possible_truncation)]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097).cast_unsigned();
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era.cast_signed() + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_parsing_is_forgiving() {
        assert_eq!(Level::parse("WARN"), Some(Level::Warn));
        assert_eq!(Level::parse(" warning "), Some(Level::Warn));
        assert_eq!(Level::parse("Trace"), Some(Level::Trace));
        assert_eq!(Level::parse("nonsense"), None);
    }

    #[test]
    fn levels_are_ordered_by_severity() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
    }

    #[test]
    fn epoch_converts_to_a_known_timestamp() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn leap_day_is_computed_correctly() {
        // 2024-02-29T12:34:56Z exercises the calendar maths on a leap day.
        assert_eq!(civil_from_unix(1_709_210_096), (2024, 2, 29, 12, 34, 56));
    }

    #[test]
    fn rotation_names_are_ordered() {
        let path = std::path::Path::new("/tmp/smart-review.log");
        assert!(
            rotated_path(path, 1)
                .to_string_lossy()
                .ends_with("smart-review.log.1")
        );
        assert!(
            rotated_path(path, 2)
                .to_string_lossy()
                .ends_with("smart-review.log.2")
        );
    }
}
