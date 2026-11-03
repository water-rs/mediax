//! Test media, generated at test time: synthetic frames encoded by
//! `mediax-codec`, muxed with `Muxer`, and repackaged by `transmux` into
//! fragmented MP4 and MPEG-TS. Nothing here is a committed binary.

#![allow(dead_code, missing_docs, reason = "each test binary uses part of this")]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use broadcast_common::{Package as _, Unpackage as _};
use bytes::Bytes;
use mediax_cli::samples::Samples;
use mediax_codec::{
    Bitrate, BitstreamFormat, CodecError, EncodedPacket, Encoder, EncoderCodec, EncoderConfig,
    EncoderProfile, EvenDimension, FrameOptions, FrameRate, H264Level, H264Profile, H264Settings,
    H265Settings,
};
use mediax_container::{
    AudioLayout, Codec, ContainerFormat, EncodedSample, MediaFile, MediaTime, Muxer, TrackId,
    TrackInfo, VideoDimensions,
};
use mediax_core::VideoColorInfo;
use transmux::{CmafMux, ProgressiveDemux, TsMux};

pub const WIDTH: u32 = 320;
pub const HEIGHT: u32 = 240;
pub const FRAMES: usize = 30;
pub const VIDEO_ID: u32 = 1;
pub const AUDIO_ID: u32 = 2;
/// 30 frames per second, in ticks.
pub const VIDEO_TIMESCALE: u32 = 30_000;
const FRAME_TICKS: i64 = 1_000;
const AAC_SAMPLE_RATE: u32 = 44_100;
const AAC_FRAME_TICKS: i64 = 1_024;
/// `AudioSpecificConfig` of the AAC-LC 44.1 kHz stereo stream.
const AAC_CONFIG: [u8; 2] = [0x12, 0x10];
/// The AAC-LC stream the codec crate's decoder tests use, one hex-encoded
/// access unit per line.
const AAC_STREAM: &str = include_str!("../../../codec/src/fixtures/aac-lc-44k-stereo-stream.hex");
/// The MPEG-TS clock.
const TS_TIMESCALE: u32 = 90_000;

pub const fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("non-zero test value")
}

fn track_id(value: u32) -> TrackId {
    TrackId::new(value).expect("non-zero track id")
}

const fn time(ticks: i64, timescale: u32) -> MediaTime {
    MediaTime::new(ticks, nz(timescale))
}

/// Routes `tracing` output of the test to the harness.
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();
}

/// A video codec the tests exercise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Video {
    H264,
    H265,
    Av1,
}

impl Video {
    pub const ALL: [Self; 3] = [Self::H264, Self::H265, Self::Av1];

    pub const fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
            Self::Av1 => "av1",
        }
    }

    pub const fn codec(self) -> Codec {
        match self {
            Self::H264 => Codec::H264,
            Self::H265 => Codec::H265,
            Self::Av1 => Codec::Av1,
        }
    }

    const fn encoder_codec(self) -> EncoderCodec {
        match self {
            Self::H264 => EncoderCodec::H264(H264Settings {
                profile: H264Profile::High,
                level: H264Level::L4_0,
                bitstream: BitstreamFormat::LengthPrefixed,
            }),
            Self::H265 => EncoderCodec::H265(H265Settings {
                bitstream: BitstreamFormat::LengthPrefixed,
            }),
            Self::Av1 => EncoderCodec::Av1,
        }
    }

    fn encoder(self) -> Result<Encoder, CodecError> {
        Encoder::new(EncoderConfig {
            codec: self.encoder_codec(),
            width: EvenDimension::new(WIDTH).expect("even width"),
            height: EvenDimension::new(HEIGHT).expect("even height"),
            color: VideoColorInfo::default(),
            // No frame reordering, so packets are in presentation order.
            profile: EncoderProfile::Realtime,
            bitrate: Bitrate::new(nz(1_000_000)),
            frame_rate: FrameRate::new(nz(30), nz(1)),
            keyframe_interval: None,
        })
    }

    /// Whether this host encodes the codec; logs the reason when it does not.
    pub fn available(self) -> bool {
        init_tracing();
        match self.encoder() {
            Ok(_) => true,
            Err(CodecError::Unsupported(reason)) => {
                tracing::warn!(codec = self.name(), %reason, "skipping: the host cannot encode this codec");
                false
            }
            Err(error) => panic!("{} encoder failed to start: {error}", self.name()),
        }
    }
}

/// Encoded synthetic frames and the configuration record that describes them.
pub struct EncodedVideo {
    pub video: Video,
    pub record: Vec<u8>,
    pub packets: Vec<EncodedPacket>,
}

fn nv12_frame(index: usize) -> Vec<u8> {
    let (width, height) = (WIDTH as usize, HEIGHT as usize);
    let mut data = vec![128_u8; width * height * 3 / 2];
    for (offset, luma) in data[..width * height].iter_mut().enumerate() {
        *luma = u8::try_from((offset % width + offset / width + index * 7) % 200 + 20)
            .expect("luma fits u8");
    }
    data
}

/// Encodes [`FRAMES`] synthetic frames, or returns `None` (after logging why)
/// when this host cannot encode `video`.
pub fn encode(video: Video) -> Option<EncodedVideo> {
    if !video.available() {
        return None;
    }
    let mut encoder = video.encoder().expect("encoder starts");
    let mut packets = Vec::new();
    for index in 0..FRAMES {
        let options = FrameOptions {
            keyframe: index == 0 || index == FRAMES / 2,
        };
        let presentation_time = std::time::Duration::from_secs(index as u64) / 30;
        for packet in encoder.encode_nv12(&nv12_frame(index), presentation_time, options) {
            packets.push(packet.expect("encode frame"));
        }
    }
    packets.extend(encoder.flush().expect("flush encoder"));
    assert_eq!(packets.len(), FRAMES, "one packet per frame");
    let record = encoder.codec_config().expect("codec configuration");
    Some(EncodedVideo {
        video,
        record,
        packets,
    })
}

impl EncodedVideo {
    fn track(&self) -> TrackInfo {
        TrackInfo::video(
            track_id(VIDEO_ID),
            self.video.codec(),
            nz(VIDEO_TIMESCALE),
            Bytes::from(self.record.clone()),
            VideoDimensions {
                width: nz(WIDTH),
                height: nz(HEIGHT),
            },
            VideoColorInfo::default(),
        )
        .expect("the encoder's configuration record parses")
    }

    fn samples(&self) -> Vec<EncodedSample> {
        self.packets
            .iter()
            .enumerate()
            .map(|(index, packet)| {
                let ticks = i64::try_from(index).expect("index fits i64") * FRAME_TICKS;
                EncodedSample::new(
                    track_id(VIDEO_ID),
                    time(ticks, VIDEO_TIMESCALE),
                    time(ticks, VIDEO_TIMESCALE),
                    time(FRAME_TICKS, VIDEO_TIMESCALE),
                    packet.keyframe,
                    Bytes::from(packet.data.clone()),
                )
            })
            .collect()
    }
}

fn aac_track() -> TrackInfo {
    TrackInfo::audio(
        track_id(AUDIO_ID),
        Codec::Aac,
        nz(AAC_SAMPLE_RATE),
        Bytes::from_static(&AAC_CONFIG),
        AudioLayout {
            channels: nz(2),
            sample_rate: nz(AAC_SAMPLE_RATE),
            sample_size: 16,
        },
    )
}

fn aac_samples() -> Vec<EncodedSample> {
    AAC_STREAM
        .trim()
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let ticks = i64::try_from(index).expect("index fits i64") * AAC_FRAME_TICKS;
            EncodedSample::new(
                track_id(AUDIO_ID),
                time(ticks, AAC_SAMPLE_RATE),
                time(ticks, AAC_SAMPLE_RATE),
                time(AAC_FRAME_TICKS, AAC_SAMPLE_RATE),
                true,
                Bytes::from(hex::decode(line).expect("fixture is valid hex")),
            )
        })
        .collect()
}

/// The container a fixture is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Mov,
    FragmentedMp4,
    TransportStream,
}

impl Container {
    pub const ALL: [Self; 4] = [
        Self::Mp4,
        Self::Mov,
        Self::FragmentedMp4,
        Self::TransportStream,
    ];

    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp4 | Self::FragmentedMp4 => "mp4",
            Self::Mov => "mov",
            Self::TransportStream => "ts",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::FragmentedMp4 => "fmp4",
            Self::TransportStream => "ts",
        }
    }

    /// What `mediax probe` calls the container.
    pub const fn probe_name(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Mov => "MOV",
            Self::FragmentedMp4 => "fragmented MP4",
            Self::TransportStream => "MPEG-TS",
        }
    }
}

/// Muxes `video` and the AAC stream into `path` as `container`.
pub fn write_fixture(video: &EncodedVideo, container: Container, path: &Path) {
    let format = match container {
        Container::Mov => ContainerFormat::Mov,
        _ => ContainerFormat::Mp4,
    };
    let progressive = path.with_extension("progressive.mp4");
    let target = if matches!(container, Container::Mp4 | Container::Mov) {
        path
    } else {
        &progressive
    };
    let mut muxer = Muxer::create(target, format).expect("create muxer");
    muxer.add_track(video.track()).expect("add video track");
    muxer.add_track(aac_track()).expect("add audio track");
    let mut samples: Vec<EncodedSample> = video.samples();
    samples.extend(aac_samples());
    samples.sort_by_key(|sample| {
        let time = sample.decode_time();
        i128::from(time.ticks()) * 1_000_000 / i128::from(time.timescale().get())
    });
    for sample in &samples {
        muxer.write_sample(sample).expect("write sample");
    }
    muxer.finish().expect("finish muxer");

    match container {
        Container::Mp4 | Container::Mov => {}
        Container::FragmentedMp4 => {
            let media = demux_progressive(&progressive);
            let bytes = CmafMux::default().package(&media).expect("CMAF mux");
            std::fs::write(path, bytes).expect("write fragmented MP4");
        }
        Container::TransportStream => {
            let mut media = demux_progressive(&progressive);
            for track in &mut media.tracks {
                // MPEG-TS runs on a 90 kHz clock.
                let from = i64::from(track.spec.timescale);
                let rescale = |ticks: i64| ticks * i64::from(TS_TIMESCALE) / from;
                for sample in &mut track.samples {
                    sample.dts = sample.dts.map(rescale);
                    sample.pts = sample.pts.map(rescale);
                    sample.duration = sample.duration.map(|duration| {
                        u32::try_from(rescale(i64::from(duration))).expect("duration fits u32")
                    });
                }
                track.start_decode_time = u64::try_from(rescale(
                    i64::try_from(track.start_decode_time).expect("start fits i64"),
                ))
                .expect("non-negative start");
                track.spec.timescale = TS_TIMESCALE;
            }
            let bytes = TsMux::new().package(&media).expect("TS mux");
            std::fs::write(path, bytes).expect("write transport stream");
        }
    }
    if target != path {
        std::fs::remove_file(target).expect("remove the intermediate file");
    }
}

fn demux_progressive(path: &Path) -> transmux::Media {
    let bytes = std::fs::read(path).expect("read progressive fixture");
    ProgressiveDemux::new(1 << 30)
        .expect("demuxer")
        .unpackage(&bytes)
        .expect("demux progressive MP4")
}

/// Runs the built `mediax` binary.
pub fn mediax(args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mediax"))
        .args(args)
        .output()
        .expect("run mediax")
}

pub fn path_arg(path: &Path) -> &OsStr {
    path.as_os_str()
}

/// Every sample of the file at `path`, grouped by track id.
pub fn read_samples(path: &Path) -> BTreeMap<u32, Vec<EncodedSample>> {
    let file = MediaFile::open(path).expect("open media");
    let mut tracks: BTreeMap<u32, Vec<EncodedSample>> = BTreeMap::new();
    for sample in Samples::open(path, &file).expect("read samples") {
        let sample = sample.expect("read sample");
        tracks
            .entry(sample.track_id().get())
            .or_default()
            .push(sample);
    }
    tracks
}

/// `a == b` as points in time, whatever their timescales.
pub fn same_time(a: MediaTime, b: MediaTime) -> bool {
    i128::from(a.ticks()) * i128::from(b.timescale().get())
        == i128::from(b.ticks()) * i128::from(a.timescale().get())
}

/// Asserts `actual` carries `expected`'s samples: the same bytes, keyframe
/// flags, durations and timestamps.
pub fn assert_same_samples(actual: &[EncodedSample], expected: &[EncodedSample], label: &str) {
    assert_eq!(actual.len(), expected.len(), "{label}: sample count");
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(
            actual.data(),
            expected.data(),
            "{label}: sample {index} bytes"
        );
        assert_eq!(
            actual.is_keyframe(),
            expected.is_keyframe(),
            "{label}: sample {index} keyframe flag"
        );
        for (what, actual, expected) in [
            ("decode time", actual.decode_time(), expected.decode_time()),
            (
                "presentation time",
                actual.presentation_time(),
                expected.presentation_time(),
            ),
            ("duration", actual.duration(), expected.duration()),
        ] {
            assert!(
                same_time(actual, expected),
                "{label}: sample {index} {what}: {actual:?} != {expected:?}"
            );
        }
    }
}

/// A fresh scratch directory.
pub fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("create a scratch directory")
}

pub fn fixture_path(dir: &Path, name: &str, container: Container) -> PathBuf {
    dir.join(format!("{name}.{}", container.extension()))
}
