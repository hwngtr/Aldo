//! Aldo: lossless music search and tagging, for one person on one machine.
//!
//! Every command is a subcommand of `aldo`. Nothing here listens on a network
//! unless the `api` feature is on, and even then only on loopback.

mod actions;
mod cli;
mod commands;
mod credentials;
mod format;
mod out;
mod settings;
mod tui;

#[cfg(feature = "api")]
mod serve;

use std::process::ExitCode;

use clap::Parser as _;

#[tokio::main]
async fn main() -> ExitCode {
    // Read the installed user's config first, then the project-local file.
    // Existing shell variables take precedence over both files.
    if let Some(path) = credentials::config_path() {
        let _ = dotenvy::from_path(path);
    }
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        // Diagnostics go to stderr so that stdout carries only results and can
        // be piped.
        .with_writer(std::io::stderr)
        .init();

    cli::run(cli::Cli::parse()).await
}
