use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();

    match ripsem::cli::run(ripsem::cli::Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        // The chain quotes values from the configuration, which the repository
        // may hold, so it is escaped as the output of a search is.
        Err(e) => {
            eprintln!(
                "Error: {}",
                ripsem::cli::printable_message(&format!("{e:?}"))
            );
            ExitCode::FAILURE
        }
    }
}
