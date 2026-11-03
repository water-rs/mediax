//! The `mediax` binary.

use std::io::Write as _;
use std::process::ExitCode;

use askama::Template as _;
use clap::Parser as _;
use mediax_cli::args::{Cli, Command};
use mediax_cli::error::CliError;
use mediax_cli::transcode::TranscodeOptions;
use mediax_cli::{probe, remux, transcode};
use tracing::level_filters::LevelFilter;

fn main() -> ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_max_level(match cli.verbose {
            0 => LevelFilter::INFO,
            1 => LevelFilter::DEBUG,
            _ => LevelFilter::TRACE,
        })
        .with_writer(std::io::stderr)
        .init();
    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), CliError> {
    match command {
        Command::Probe { input, json } => {
            let report = probe::probe(&input)?;
            let text = if json {
                serde_json::to_string_pretty(&report)?
            } else {
                report.render()?
            };
            writeln!(std::io::stdout().lock(), "{text}")?;
            Ok(())
        }
        Command::Transcode {
            input,
            output,
            codec,
            bitrate,
            keyframe_interval,
        } => transcode::transcode(
            &input,
            &output,
            &TranscodeOptions {
                codec,
                bitrate,
                keyframe_interval,
            },
        ),
        Command::Remux { input, output } => remux::remux(&input, &output),
    }
}
