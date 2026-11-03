//! `mediax probe`: describe a file's container and tracks.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use askama::Template;
use mediax_container::{
    Codec, MediaFile, MediaFormat, MediaTrack, TrackId, TrackInfo, TrackKind, VideoParameters,
};
use serde::Serialize;

use crate::error::CliError;
use crate::samples::Samples;
use crate::tracks::tracks_with_signaled_color;

/// What `mediax probe` reports about a file.
#[derive(Debug, Serialize, Template)]
#[template(path = "probe.txt")]
pub struct Report {
    /// The probed path.
    pub file: String,
    /// The container, as `MP4`, `MOV`, `fragmented MP4` or `MPEG-TS`.
    pub format: &'static str,
    /// Length of the longest track, as `1.250 s`.
    pub duration: Option<String>,
    /// The audio and video tracks.
    pub tracks: Vec<TrackReport>,
}

/// One track.
#[derive(Debug, Serialize)]
pub struct TrackReport {
    /// The container's track id.
    pub id: u32,
    /// `video`, `audio`, `subtitle` or `metadata`.
    pub kind: &'static str,
    /// The codec, as `H264`.
    pub codec: String,
    /// ISO 639 language code, where the container declares one.
    pub language: Option<String>,
    /// Presentation length in seconds.
    pub duration_seconds: Option<f64>,
    /// Number of access units.
    pub sample_count: Option<u64>,
    /// Video parameters of a video track.
    pub video: Option<VideoReport>,
    /// Audio parameters of an audio track.
    pub audio: Option<AudioReport>,
}

/// Video parameters.
#[derive(Debug, Serialize)]
pub struct VideoReport {
    /// Coded width in pixels.
    pub width: u32,
    /// Coded height in pixels.
    pub height: u32,
    /// Nominal frame rate, where the track declares one.
    pub frame_rate: Option<FrameRateReport>,
    /// Profile, as `High (100)`.
    pub profile: Option<String>,
    /// Level, as `4.1`.
    pub level: Option<String>,
    /// Luma bit depth.
    pub bit_depth: Option<u8>,
    /// Chroma subsampling, as `4:2:0`.
    pub chroma: Option<&'static str>,
    /// Colour description.
    pub color: ColorReport,
}

/// A rational frame rate.
#[derive(Debug, Serialize)]
pub struct FrameRateReport {
    /// Numerator.
    pub numerator: u32,
    /// Denominator.
    pub denominator: u32,
    /// The rate in frames per second, rounded to three decimals.
    pub fps: String,
}

/// Colour description of a video track.
#[derive(Debug, Serialize)]
pub struct ColorReport {
    /// Colour primaries.
    pub primaries: String,
    /// Transfer function.
    pub transfer: String,
    /// Matrix coefficients.
    pub matrix: String,
    /// Component range.
    pub range: String,
    /// Static HDR content-light metadata.
    pub content_light_level: Option<LightLevelReport>,
    /// Whether Dolby Vision is signalled.
    pub dolby_vision: bool,
}

/// `MaxCLL` and `MaxFALL`.
#[derive(Debug, Serialize)]
pub struct LightLevelReport {
    /// `MaxCLL` in nits.
    pub max_content: u16,
    /// `MaxFALL` in nits.
    pub max_frame_average: u16,
}

/// Audio parameters.
#[derive(Debug, Serialize)]
pub struct AudioReport {
    /// Samples per second.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u32,
}

/// Probes the file at `path`.
///
/// # Errors
///
/// Returns an error when the file cannot be read or is not a container
/// `mediax-container` reads.
pub fn probe(path: &Path) -> Result<Report, CliError> {
    let file = MediaFile::open(path)?;
    let infos = tracks_with_signaled_color(path, &file)?;
    // A transport stream has no sample tables, so its lengths come from
    // reading every sample's timing.
    let scanned = match file.format() {
        MediaFormat::MpegTs => scan_transport_stream(path, &file)?,
        _ => BTreeMap::new(),
    };
    let tracks: Vec<TrackReport> = file
        .tracks()
        .iter()
        .zip(&infos)
        .map(|(track, info)| track_report(track, info, scanned.get(&info.id())))
        .collect();
    let duration = tracks
        .iter()
        .filter_map(|track| track.duration_seconds)
        .max_by(f64::total_cmp)
        .map(|seconds| format!("{seconds:.3} s"));
    Ok(Report {
        file: path.display().to_string(),
        format: format_name(file.format())?,
        duration,
        tracks,
    })
}

const fn format_name(format: MediaFormat) -> Result<&'static str, CliError> {
    match format {
        MediaFormat::Mp4 => Ok("MP4"),
        MediaFormat::Mov => Ok("MOV"),
        MediaFormat::FragmentedMp4 => Ok("fragmented MP4"),
        MediaFormat::MpegTs => Ok("MPEG-TS"),
        other => Err(CliError::UnsupportedFormat(other)),
    }
}

/// Timing of one track of a transport stream, from its samples.
#[derive(Debug, Clone, Copy)]
struct Scanned {
    samples: u64,
    duration: Duration,
}

fn scan_transport_stream(
    path: &Path,
    file: &MediaFile,
) -> Result<BTreeMap<TrackId, Scanned>, CliError> {
    // Per track: sample count, earliest decode time, latest end of presentation.
    let mut spans: BTreeMap<TrackId, (u64, Duration, Duration)> = BTreeMap::new();
    for sample in Samples::open(path, file)? {
        let sample = sample?;
        let start = sample.decode_time().to_duration()?;
        let end = sample.presentation_time().to_duration()? + sample.duration().to_duration()?;
        let span = spans.entry(sample.track_id()).or_insert((0, start, end));
        span.0 += 1;
        span.1 = span.1.min(start);
        span.2 = span.2.max(end);
    }
    Ok(spans
        .into_iter()
        .map(|(id, (samples, start, end))| {
            (
                id,
                Scanned {
                    samples,
                    duration: end.saturating_sub(start),
                },
            )
        })
        .collect())
}

fn track_report(track: &MediaTrack, info: &TrackInfo, scanned: Option<&Scanned>) -> TrackReport {
    TrackReport {
        id: info.id().get(),
        kind: match info.kind() {
            TrackKind::Video => "video",
            TrackKind::Audio => "audio",
            TrackKind::Subtitle => "subtitle",
            TrackKind::Metadata => "metadata",
        },
        codec: format!("{:?}", info.codec()),
        language: track.language().map(str::to_owned),
        duration_seconds: track
            .duration()
            .or_else(|| scanned.map(|scanned| scanned.duration))
            .map(|duration| (duration.as_secs_f64() * 1e3).round() / 1e3),
        sample_count: track
            .sample_count()
            .map(u64::from)
            .or_else(|| scanned.map(|scanned| scanned.samples)),
        video: video_report(info),
        audio: info.audio_layout().map(|layout| AudioReport {
            sample_rate: layout.sample_rate.get(),
            channels: layout.channels.get(),
        }),
    }
}

fn video_report(info: &TrackInfo) -> Option<VideoReport> {
    let size = info.video_dimensions()?;
    let parameters = info.video_parameters();
    let color = info.video_color_info()?;
    Some(VideoReport {
        width: size.width.get(),
        height: size.height.get(),
        frame_rate: parameters
            .and_then(|parameters| parameters.frame_rate)
            .map(|rate| FrameRateReport {
                numerator: rate.numerator().get(),
                denominator: rate.denominator().get(),
                fps: format!("{:.3}", rate.as_f64())
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_owned(),
            }),
        profile: parameters.map(|parameters| profile(info.codec(), parameters)),
        level: parameters
            .and_then(|parameters| parameters.level)
            .map(|level| level.to_string()),
        bit_depth: parameters.map(|parameters| parameters.bit_depth),
        chroma: parameters.map(chroma),
        color: ColorReport {
            primaries: format!("{:?}", color.primaries),
            transfer: format!("{:?}", color.transfer),
            matrix: format!("{:?}", color.matrix),
            range: format!("{:?}", color.range),
            content_light_level: color.content_light_level.map(|level| LightLevelReport {
                max_content: level.max_content_light_level(),
                max_frame_average: level.max_frame_average_light_level(),
            }),
            dolby_vision: color.dolby_vision,
        },
    })
}

const fn chroma(parameters: VideoParameters) -> &'static str {
    use mediax_container::ChromaFormat;
    match parameters.chroma_format {
        ChromaFormat::Monochrome => "4:0:0",
        ChromaFormat::Yuv420 => "4:2:0",
        ChromaFormat::Yuv422 => "4:2:2",
        ChromaFormat::Yuv444 => "4:4:4",
    }
}

/// The profile's name from the codec specification, with its number.
fn profile(codec: Codec, parameters: VideoParameters) -> String {
    let number = parameters.profile;
    let name = match codec {
        Codec::H264 => match number {
            66 => Some("Baseline"),
            77 => Some("Main"),
            88 => Some("Extended"),
            100 => Some("High"),
            110 => Some("High 10"),
            122 => Some("High 4:2:2"),
            244 => Some("High 4:4:4 Predictive"),
            _ => None,
        },
        Codec::H265 => match number {
            1 => Some("Main"),
            2 => Some("Main 10"),
            3 => Some("Main Still Picture"),
            4 => Some("Range Extensions"),
            _ => None,
        },
        Codec::Av1 => match number {
            0 => Some("Main"),
            1 => Some("High"),
            2 => Some("Professional"),
            _ => None,
        },
        _ => None,
    };
    name.map_or_else(|| number.to_string(), |name| format!("{name} ({number})"))
}
