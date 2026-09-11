//! Command line surface (FR-1.2).

use std::path::PathBuf;

use clap::Parser;

/// A terminal client for reviewing GitHub pull requests.
#[derive(Debug, Parser)]
#[command(name = "smart-review", version, about, long_about = None)]
pub struct Cli {
    /// Repository to review as OWNER/NAME. Defaults to the GitHub remote of the
    /// directory you start in (FR-1.1).
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,

    /// Jump straight into this pull request.
    #[arg(long, value_name = "N")]
    pub pr: Option<u64>,

    /// Run as if started from this directory.
    #[arg(long, value_name = "DIR")]
    pub path: Option<PathBuf>,

    /// Git remote to read repository information from.
    #[arg(long, value_name = "NAME")]
    pub remote: Option<String>,

    /// Use this config file instead of the one inside the smart-review home.
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Override the theme for this run only (FR-7.7).
    #[arg(long, value_name = "NAME")]
    pub theme: Option<String>,

    /// Override the smart-review home directory (default: `~/.smart-review`).
    #[arg(long, value_name = "DIR")]
    pub home: Option<PathBuf>,

    /// Log level: error, warn, info, debug or trace.
    #[arg(long, value_name = "LEVEL")]
    pub log_level: Option<String>,

    /// Print an environment report and exit: 0 ready, 1 degraded, 2 unusable (FR-9.3).
    #[arg(long)]
    pub check: bool,
}
