//! The command line.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

/// Hardware-accelerated media tool: inspect, transcode and remux media files.
#[derive(Debug, Parser)]
#[command(name = "mediax", version, about)]
pub struct Cli {
    /// Log more: once for `debug`, twice for `trace`.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// A `mediax` subcommand.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print a file's container, tracks and codec parameters.
    Probe {
        /// The media file.
        input: PathBuf,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Re-encode the video track and copy the audio samples unchanged.
    Transcode {
        /// The source file.
        input: PathBuf,
        /// The file to write; its extension (`mp4` or `mov`) picks the container.
        output: PathBuf,
        /// The video codec to encode with.
        #[arg(long, value_enum)]
        codec: OutputCodec,
        /// Target video bitrate in bits per second; accepts `k`, `M` and `G`
        /// suffixes, as in `4M` or `800k`.
        #[arg(long, value_parser = parse_bitrate)]
        bitrate: Option<NonZeroU32>,
        /// Seconds between keyframes.
        #[arg(long, value_parser = parse_seconds)]
        keyframe_interval: Option<Duration>,
    },
    /// Copy every track sample for sample into another container.
    Remux {
        /// The source file.
        input: PathBuf,
        /// The file to write; its extension (`mp4` or `mov`) picks the container.
        output: PathBuf,
    },
}

/// A video codec `transcode` encodes with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputCodec {
    /// H.264 / AVC, on the platform's hardware encoder.
    H264,
    /// H.265 / HEVC, on the platform's hardware encoder.
    H265,
    /// AV1, in software.
    Av1,
}

/// Parses `1500000`, `800k`, `4M` or `2.5M` into bits per second.
fn parse_bitrate(text: &str) -> Result<NonZeroU32, String> {
    let (number, multiplier) = [(['k', 'K'], 1e3), (['m', 'M'], 1e6), (['g', 'G'], 1e9)]
        .into_iter()
        .find_map(|(suffix, multiplier)| Some((text.strip_suffix(suffix)?, multiplier)))
        .unwrap_or((text, 1.0));
    let value: f64 = number
        .parse()
        .map_err(|_| format!("{text:?} is not a bitrate like 4M, 800k or 1500000"))?;
    let bits = (value * multiplier).round();
    if !(1.0..=f64::from(u32::MAX)).contains(&bits) {
        return Err(format!("{text:?} is outside 1 to {} bit/s", u32::MAX));
    }
    // The range check above makes the cast exact.
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    NonZeroU32::new(bits as u32).ok_or_else(|| format!("{text:?} is not a positive bitrate"))
}

/// Parses a positive number of seconds.
fn parse_seconds(text: &str) -> Result<Duration, String> {
    let seconds: f64 = text
        .parse()
        .map_err(|_| format!("{text:?} is not a number of seconds"))?;
    Duration::try_from_secs_f64(seconds)
        .ok()
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| format!("{text:?} is not a positive number of seconds"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrates_take_suffixes() {
        assert_eq!(parse_bitrate("1500000").unwrap().get(), 1_500_000);
        assert_eq!(parse_bitrate("800k").unwrap().get(), 800_000);
        assert_eq!(parse_bitrate("2.5M").unwrap().get(), 2_500_000);
        assert!(parse_bitrate("0").is_err());
        assert!(parse_bitrate("9G").is_err());
        assert!(parse_bitrate("fast").is_err());
    }

    #[test]
    fn keyframe_interval_is_positive_seconds() {
        assert_eq!(parse_seconds("2").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_seconds("0.5").unwrap(), Duration::from_millis(500));
        assert!(parse_seconds("0").is_err());
        assert!(parse_seconds("-1").is_err());
        assert!(parse_seconds("nan").is_err());
    }
}
