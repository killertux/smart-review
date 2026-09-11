//! Binary entry point.
//!
//! Kept deliberately thin: parse arguments, bootstrap, hand control to either the
//! doctor or the TUI, and translate errors into exit codes (FR-9.3).

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

use smart_review::bootstrap::Startup;
use smart_review::cli::Cli;
use smart_review::doctor::{self, Health};
use smart_review::{Error, logging};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(error) => {
            // Exit code 2 means "unusable" (FR-9.3).
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "smart-review: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode, Error> {
    let startup = Startup::load(cli)?;
    logging::init(
        &startup.home,
        &startup.config.log.level,
        cli.log_level.as_deref(),
    )?;

    for warning in &startup.warnings {
        logging::log(logging::Level::Warn, warning);
    }

    if cli.check {
        let mut stdout = std::io::stdout();
        let health = doctor::run(&mut stdout, &startup.doctor_context())?;
        return Ok(match health {
            Health::Ready => ExitCode::SUCCESS,
            Health::Degraded => ExitCode::from(1),
            Health::Unusable => ExitCode::from(2),
        });
    }

    smart_review::tui::run(startup)?;
    Ok(ExitCode::SUCCESS)
}
