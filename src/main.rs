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
        // `--check` is the headless form of "can this machine do the job", so it runs
        // the same detection the interface would, and reports it (FR-1.1, FR-1.2).
        let mut context = startup.doctor_context();
        let workspace = smart_review::adapters::git::GitCli::new();
        let probe = smart_review::adapters::gh::probe::GhCliProbe::new(
            startup.config.forge.gh_path.clone(),
        );
        let request = smart_review::application::DetectRequest {
            repo: startup.repo.clone(),
            remote: startup
                .remote
                .clone()
                .or_else(|| startup.config.forge.remote.clone()),
            gh_program: Some(startup.config.forge.gh_path.clone()),
        };
        match smart_review::application::detect(
            &workspace,
            &probe,
            &request,
            &smart_review::ports::Cancel::new(),
        ) {
            Ok(environment) => context.environment = Some(environment),
            Err(error) => context.environment_error = Some(error),
        }

        let mut stdout = std::io::stdout();
        let health = doctor::run(&mut stdout, &context)?;
        return Ok(match health {
            Health::Ready => ExitCode::SUCCESS,
            Health::Degraded => ExitCode::from(1),
            Health::Unusable => ExitCode::from(2),
        });
    }

    smart_review::tui::run(startup)?;
    Ok(ExitCode::SUCCESS)
}
