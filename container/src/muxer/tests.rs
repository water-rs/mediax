use std::io::Cursor;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use mediax_codec::{
    Bitrate, BitstreamFormat, CodecError, EncodedPacket, Encoder, EncoderCodec, EncoderConfig,
    EncoderProfile, EvenDimension, FrameOptions, FrameRate, H264Level, H264Profile, H264Settings,
    H265Settings,
};
use mediax_core::{
    ColorPrimaries, ColorRange, ContentLightLevel, MatrixCoefficients, TransferFunction,
};

use super::*;
use crate::stream::{AudioLayout, MediaTime, VideoDimensions};
use crate::{ProgressiveTrackReader, probe_mp4_color_info};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const FRAMES: usize = 12;

/// Video timescale and per-frame duration: 30 frames per second.
const VIDEO_TIMESCALE: u32 = 30_000;
const FRAME_TICKS: i64 = 1_000;

const AAC_SAMPLE_RATE: u32 = 44_100;
const AAC_FRAME_TICKS: i64 = 1_024;
/// `AudioSpecificConfig` of the AAC-LC 44.1 kHz stereo fixture.
const AAC_CONFIG: [u8; 2] = [0x12, 0x10];
/// The same AAC-LC stream the codec crate's decoder tests use, one hex-encoded
/// access unit per line.
const AAC_STREAM: &str = include_str!("../../../codec/src/fixtures/aac-lc-44k-stereo-stream.hex");

fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("test value is non-zero")
}

fn track_id(value: u32) -> TrackId {
    TrackId::new(value).expect("test track id is non-zero")
}

fn time(ticks: i64, timescale: u32) -> MediaTime {
    MediaTime::new(ticks, nz(timescale))
}

fn color(
    primaries: ColorPrimaries,
    transfer: TransferFunction,
    matrix: MatrixCoefficients,
    range: ColorRange,
    content_light_level: Option<ContentLightLevel>,
) -> VideoColorInfo {
    VideoColorInfo {
        matrix,
        primaries,
        transfer,
        range,
        content_light_level,
        dolby_vision: false,
    }
}

fn bt709() -> VideoColorInfo {
    color(
        ColorPrimaries::Bt709,
        TransferFunction::Sdr,
        MatrixCoefficients::Bt709,
        ColorRange::Limited,
        None,
    )
}

/// Where a test writes its file. With `MEDIAX_MUXER_SAMPLES` set, files land
/// in the build directory and stay there for playback checks.
struct Output {
    path: PathBuf,
    _directory: Option<tempfile::TempDir>,
}

fn output(name: &str) -> Output {
    if std::env::var_os("MEDIAX_MUXER_SAMPLES").is_some() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/muxer-samples");
        std::fs::create_dir_all(&directory).expect("create sample directory");
        Output {
            path: directory.join(name),
            _directory: None,
        }
    } else {
        let directory = tempfile::tempdir().expect("temp dir");
        Output {
            path: directory.path().join(name),
            _directory: Some(directory),
        }
    }
}

/// Encoded video, as the encoder produced it.
struct EncodedVideo {
    codec: Codec,
    /// The configuration record without a box header, as encoders emit it.
    record: Vec<u8>,
    packets: Vec<EncodedPacket>,
}

fn nv12_frame(index: usize) -> Vec<u8> {
    let (width, height) = (WIDTH as usize, HEIGHT as usize);
    let mut data = vec![128_u8; width * height * 3 / 2];
    for (offset, luma) in data[..width * height].iter_mut().enumerate() {
        *luma = u8::try_from((offset % width + offset / width + index * 7) % 200 + 20)
            .expect("luma value fits u8");
    }
    data
}

/// Encodes [`FRAMES`] frames, or `None` when this host has no encoder for
/// `codec`.
fn encode(codec: EncoderCodec) -> Option<EncodedVideo> {
    let mut encoder = match Encoder::new(EncoderConfig {
        codec,
        width: EvenDimension::new(WIDTH).expect("even width"),
        height: EvenDimension::new(HEIGHT).expect("even height"),
        color: bt709(),
        profile: EncoderProfile::Offline,
        bitrate: Bitrate::new(nz(1_000_000)),
        frame_rate: FrameRate::new(nz(30), nz(1)),
        keyframe_interval: None,
    }) {
        Ok(encoder) => encoder,
        Err(CodecError::Unsupported(reason)) => {
            eprintln!("skipping {codec:?}: {reason}");
            return None;
        }
        Err(error) => panic!("{codec:?} encoder failed to start: {error}"),
    };
    let mut packets = Vec::new();
    for index in 0..FRAMES {
        let options = FrameOptions {
            keyframe: index == 0 || index == FRAMES / 2,
        };
        for packet in encoder.encode_nv12(&nv12_frame(index), options) {
            packets.push(packet.expect("encode frame"));
        }
    }
    packets.extend(encoder.flush().expect("flush encoder"));
    assert_eq!(packets.len(), FRAMES, "one packet per frame");
    assert!(packets[0].keyframe, "the stream opens with a keyframe");
    let record = encoder.codec_config().expect("codec configuration");
    let codec = match codec {
        EncoderCodec::H264(_) => Codec::H264,
        EncoderCodec::H265(_) => Codec::H265,
        EncoderCodec::Av1 => Codec::Av1,
    };
    Some(EncodedVideo {
        codec,
        record,
        packets,
    })
}

fn h264() -> Option<EncodedVideo> {
    encode(EncoderCodec::H264(H264Settings {
        profile: H264Profile::High,
        level: H264Level::L4_0,
        bitstream: BitstreamFormat::LengthPrefixed,
    }))
}

fn h265() -> Option<EncodedVideo> {
    encode(EncoderCodec::H265(H265Settings {
        bitstream: BitstreamFormat::LengthPrefixed,
    }))
}

fn av1() -> EncodedVideo {
    encode(EncoderCodec::Av1).expect("software AV1 encoder is always available")
}

/// The configuration box a demuxer reports for `record`.
fn config_box(fourcc: [u8; 4], record: &[u8]) -> Bytes {
    let mut bytes = u32::try_from(record.len() + 8)
        .expect("record fits a box")
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(&fourcc);
    bytes.extend_from_slice(record);
    Bytes::from(bytes)
}

impl EncodedVideo {
    fn fourcc(&self) -> [u8; 4] {
        match self.codec {
            Codec::H264 => *b"avcC",
            Codec::H265 => *b"hvcC",
            _ => *b"av1C",
        }
    }

    /// The configuration a demuxer reports for `record`: the whole box for
    /// H.264 and H.265, the bare record for AV1.
    fn expected_configuration(&self) -> Bytes {
        match self.codec {
            Codec::Av1 => Bytes::from(self.record.clone()),
            _ => config_box(self.fourcc(), &self.record),
        }
    }

    fn track(&self, id: u32, color: VideoColorInfo) -> TrackInfo {
        TrackInfo::video(
            track_id(id),
            self.codec,
            nz(VIDEO_TIMESCALE),
            Bytes::from(self.record.clone()),
            VideoDimensions {
                width: nz(WIDTH),
                height: nz(HEIGHT),
            },
            color,
        )
    }

    /// Decode order samples. `reorder` gives each sample's display slot within
    /// its group of four (a decode-order `I P B B` pattern), `lead` the frames
    /// presentation trails decode by.
    fn samples(&self, id: u32, reorder: Option<[i64; 4]>, lead: i64) -> Vec<EncodedSample> {
        self.packets
            .iter()
            .enumerate()
            .map(|(index, packet)| {
                let index = i64::try_from(index).expect("index fits i64");
                let decode = index * FRAME_TICKS;
                let presentation = reorder.map_or(decode, |slots| {
                    let group = index / 4 * 4;
                    (group + slots[usize::try_from(index % 4).expect("slot")] + lead) * FRAME_TICKS
                });
                EncodedSample::new(
                    track_id(id),
                    time(decode, VIDEO_TIMESCALE),
                    time(presentation, VIDEO_TIMESCALE),
                    time(FRAME_TICKS, VIDEO_TIMESCALE),
                    packet.keyframe,
                    Bytes::from(packet.data.clone()),
                )
            })
            .collect()
    }
}

fn aac_packets() -> Vec<Vec<u8>> {
    AAC_STREAM
        .trim()
        .lines()
        .map(|line| hex::decode(line).expect("fixture is valid hex"))
        .collect()
}

fn aac_track(id: u32) -> TrackInfo {
    TrackInfo::audio(
        track_id(id),
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

/// AAC access units starting `start` ticks into the timeline.
fn aac_samples(id: u32, start: i64) -> Vec<EncodedSample> {
    aac_packets()
        .into_iter()
        .enumerate()
        .map(|(index, data)| {
            let ticks = start + i64::try_from(index).expect("index fits i64") * AAC_FRAME_TICKS;
            EncodedSample::new(
                track_id(id),
                time(ticks, AAC_SAMPLE_RATE),
                time(ticks, AAC_SAMPLE_RATE),
                time(AAC_FRAME_TICKS, AAC_SAMPLE_RATE),
                true,
                Bytes::from(data),
            )
        })
        .collect()
}

/// Writes `tracks` with their samples interleaved by decode time.
fn write(path: &Path, format: ContainerFormat, tracks: Vec<(TrackInfo, Vec<EncodedSample>)>) {
    let mut muxer = Muxer::create(path, format).expect("create muxer");
    let mut samples = Vec::new();
    for (info, track_samples) in tracks {
        muxer.add_track(info).expect("add track");
        samples.extend(track_samples);
    }
    samples.sort_by_key(|sample| {
        let time = sample.decode_time();
        i128::from(time.ticks()) * 1_000_000 / i128::from(time.timescale().get())
    });
    for sample in &samples {
        muxer.write_sample(sample).expect("write sample");
    }
    muxer.finish().expect("finish");
}

/// Every track of `kind`, with all its samples.
fn read(path: &Path, kind: TrackKind) -> Vec<(TrackInfo, Vec<EncodedSample>)> {
    let mut reader = ProgressiveTrackReader::open(path, kind).expect("open file");
    let infos: Vec<TrackInfo> = reader
        .tracks()
        .iter()
        .map(|track| track.info().clone())
        .collect();
    infos
        .into_iter()
        .enumerate()
        .map(|(index, info)| {
            reader.select_track(index).expect("select track");
            let mut samples = Vec::new();
            while let Some(sample) = reader.read_sample().expect("read sample") {
                samples.push(sample);
            }
            (info, samples)
        })
        .collect()
}

/// Asserts `read` returned `expected`'s samples unchanged.
fn assert_samples(actual: &[EncodedSample], expected: &[EncodedSample]) {
    assert_eq!(actual.len(), expected.len(), "sample count");
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(actual.data(), expected.data(), "sample {index} bytes");
        assert_eq!(
            actual.decode_time(),
            expected.decode_time(),
            "sample {index} decode time"
        );
        assert_eq!(
            actual.presentation_time(),
            expected.presentation_time(),
            "sample {index} presentation time"
        );
        assert_eq!(
            actual.duration(),
            expected.duration(),
            "sample {index} duration"
        );
        assert_eq!(
            actual.is_keyframe(),
            expected.is_keyframe(),
            "sample {index} sync flag"
        );
    }
}

fn first_brand(path: &Path) -> [u8; 4] {
    let bytes = std::fs::read(path).expect("read file");
    assert_eq!(&bytes[4..8], b"ftyp");
    bytes[8..12].try_into().expect("brand")
}

/// Writes `video` with `I P B B` reordering and reads it back.
fn assert_video_round_trip(video: &EncodedVideo, format: ContainerFormat, name: &str) {
    let file = output(name);
    // Decode-order slots 0,3,1,2: both positive and negative composition
    // offsets (3 - 1, 1 - 2, 2 - 3), so the `ctts` is signed.
    let samples = video.samples(1, Some([0, 3, 1, 2]), 0);
    write(
        &file.path,
        format,
        vec![(video.track(1, bt709()), samples.clone())],
    );

    let mut tracks = read(&file.path, TrackKind::Video);
    assert_eq!(tracks.len(), 1);
    let (info, read_back) = tracks.remove(0);
    assert_eq!(info.id(), track_id(1));
    assert_eq!(info.codec(), video.codec);
    assert_eq!(info.timescale(), nz(VIDEO_TIMESCALE));
    assert_eq!(
        info.video_dimensions(),
        Some(VideoDimensions {
            width: nz(WIDTH),
            height: nz(HEIGHT)
        })
    );
    assert_eq!(
        info.decoder_configuration(),
        &video.expected_configuration()[..],
        "codec configuration"
    );
    assert_samples(&read_back, &samples);
    assert!(
        read_back
            .iter()
            .any(|s| s.presentation_time().ticks() < s.decode_time().ticks()),
        "the pattern has a negative composition offset"
    );
    assert_eq!(
        read_back.iter().filter(|s| s.is_keyframe()).count(),
        video.packets.iter().filter(|p| p.keyframe).count(),
        "sync samples"
    );
    assert_eq!(
        probe_mp4_color_info(&file.path, None).expect("probe"),
        bt709()
    );
}

#[test]
fn h264_with_composition_offsets_round_trips() {
    let Some(video) = h264() else { return };
    assert_video_round_trip(&video, ContainerFormat::Mp4, "h264-reordered.mp4");
    assert_video_round_trip(&video, ContainerFormat::Mov, "h264-reordered.mov");
}

#[test]
fn h265_with_composition_offsets_round_trips() {
    let Some(video) = h265() else { return };
    assert_video_round_trip(&video, ContainerFormat::Mp4, "h265-reordered.mp4");
}

#[test]
fn av1_with_composition_offsets_round_trips() {
    let video = av1();
    assert_video_round_trip(&video, ContainerFormat::Mp4, "av1-reordered.mp4");
    assert_video_round_trip(&video, ContainerFormat::Mov, "av1-reordered.mov");
}

#[test]
fn positive_composition_offsets_use_an_unsigned_ctts() {
    let video = av1();
    let file = output("av1-delayed.mp4");
    // Presentation trails decode by two frames throughout, with no reorder.
    let mut samples = video.samples(1, None, 0);
    samples = samples
        .into_iter()
        .map(|sample| {
            EncodedSample::new(
                sample.track_id(),
                sample.decode_time(),
                time(
                    sample.decode_time().ticks() + 2 * FRAME_TICKS,
                    VIDEO_TIMESCALE,
                ),
                sample.duration(),
                sample.is_keyframe(),
                sample.data().clone(),
            )
        })
        .collect();
    write(
        &file.path,
        ContainerFormat::Mp4,
        vec![(video.track(1, bt709()), samples.clone())],
    );
    let (_, read_back) = read(&file.path, TrackKind::Video).remove(0);
    assert_samples(&read_back, &samples);

    let bytes = std::fs::read(&file.path).expect("read file");
    let ctts = bytes
        .windows(4)
        .position(|window| window == b"ctts")
        .expect("ctts box");
    assert_eq!(bytes[ctts + 4], 0, "ctts version");
}

/// Video and AAC in one file, with honest timing (decode order is display
/// order), so the outputs play back correctly.
fn assert_video_and_audio_interleave(video: &EncodedVideo, label: &str) {
    for (format, extension) in [(ContainerFormat::Mp4, "mp4"), (ContainerFormat::Mov, "mov")] {
        let file = output(&format!("{label}-aac.{extension}"));
        let video_samples = video.samples(1, None, 0);
        let audio_samples = aac_samples(2, 0);
        write(
            &file.path,
            format,
            vec![
                (video.track(1, bt709()), video_samples.clone()),
                (aac_track(2), audio_samples.clone()),
            ],
        );

        let mut videos = read(&file.path, TrackKind::Video);
        let mut audios = read(&file.path, TrackKind::Audio);
        assert_eq!((videos.len(), audios.len()), (1, 1));
        let (video_info, video_read) = videos.remove(0);
        let (audio_info, audio_read) = audios.remove(0);

        assert_eq!(video_info.id(), track_id(1));
        assert_eq!(
            video_info.decoder_configuration(),
            &video.expected_configuration()[..]
        );
        assert_samples(&video_read, &video_samples);

        assert_eq!(audio_info.id(), track_id(2));
        assert_eq!(audio_info.codec(), Codec::Aac);
        assert_eq!(audio_info.timescale(), nz(AAC_SAMPLE_RATE));
        assert_eq!(audio_info.decoder_configuration(), &AAC_CONFIG[..]);
        assert_eq!(
            audio_info.audio_layout(),
            Some(AudioLayout {
                channels: nz(2),
                sample_rate: nz(AAC_SAMPLE_RATE),
                sample_size: 16,
            })
        );
        assert_samples(&audio_read, &audio_samples);
    }
}

#[test]
fn av1_and_audio_interleave_in_one_file() {
    assert_video_and_audio_interleave(&av1(), "av1");
}

#[test]
fn h264_and_audio_interleave_in_one_file() {
    let Some(video) = h264() else { return };
    assert_video_and_audio_interleave(&video, "h264");
}

/// The first box named `fourcc` inside `haystack`, header included.
fn find_box(haystack: &[u8], fourcc: [u8; 4]) -> Option<Vec<u8>> {
    let tag = haystack
        .windows(4)
        .position(|window| window == fourcc.as_slice())?;
    let start = tag.checked_sub(4)?;
    let size = usize::try_from(u32::from_be_bytes(haystack[start..tag].try_into().ok()?)).ok()?;
    haystack.get(start..start + size).map(<[u8]>::to_vec)
}

/// `AVFoundation` opens a MOV only when its audio carries the `QuickTime`
/// variable-bitrate compression id and a `chan` layout. The MP4 flavour keeps
/// the plain ISO entry.
#[test]
fn mov_audio_is_marked_for_quicktime() {
    let video = av1();
    let mut entries = Vec::new();
    for (format, name) in [
        (ContainerFormat::Mov, "qt-audio.mov"),
        (ContainerFormat::Mp4, "iso-audio.mp4"),
    ] {
        let file = output(name);
        write(
            &file.path,
            format,
            vec![
                (video.track(1, bt709()), video.samples(1, None, 0)),
                (aac_track(2), aac_samples(2, 0)),
            ],
        );
        let bytes = std::fs::read(&file.path).expect("read file");
        let smhd = bytes
            .windows(4)
            .position(|window| window == b"smhd")
            .expect("audio track");
        entries.push(find_box(&bytes[smhd..], *b"stsd").expect("audio stsd"));
        // Reading it back is unaffected by the flavour.
        assert_eq!(read(&file.path, TrackKind::Audio).len(), 1);
    }
    let (mov, mp4) = (&entries[0], &entries[1]);

    // stsd header (16) + entry header (8) + reserved/dref (8) + version,
    // revision, vendor (8) + channels, sample size (4) put the compression id
    // at 44.
    assert_eq!(mov[44..46], [0xff, 0xfe], "VBR compression id");
    let chan = find_box(mov, *b"chan").expect("chan box");
    assert_eq!(chan[12..16], 0x0065_0002_u32.to_be_bytes(), "stereo layout");

    assert_eq!(mp4[44..46], [0, 0], "no compression id in MP4");
    assert!(find_box(mp4, *b"chan").is_none());
}

#[test]
fn brand_follows_the_format() {
    let video = av1();
    for (format, name, brand) in [
        (ContainerFormat::Mp4, "brand.mp4", *b"isom"),
        (ContainerFormat::Mov, "brand.mov", *b"qt  "),
    ] {
        let file = output(name);
        write(
            &file.path,
            format,
            vec![(video.track(1, bt709()), video.samples(1, None, 0))],
        );
        assert_eq!(first_brand(&file.path), brand);
    }
}

#[test]
fn late_audio_start_becomes_an_empty_edit() {
    let video = av1();
    let file = output("late-audio.mp4");
    let video_samples = video.samples(1, None, 0);
    // Audio begins half a second in.
    let audio_samples = aac_samples(2, i64::from(AAC_SAMPLE_RATE) / 2);
    write(
        &file.path,
        ContainerFormat::Mp4,
        vec![
            (video.track(1, bt709()), video_samples),
            (aac_track(2), audio_samples),
        ],
    );

    let length = file.path.metadata().expect("metadata").len();
    let reader = mp4::Mp4Reader::read_header(
        std::io::BufReader::new(std::fs::File::open(&file.path).expect("open")),
        length,
    )
    .expect("mp4 reader");
    let audio = &reader.tracks()[&2];
    let edits = &audio
        .trak
        .edts
        .as_ref()
        .and_then(|edts| edts.elst.as_ref())
        .expect("audio track has an edit list")
        .entries;
    assert_eq!(edits.len(), 2);
    assert_eq!(
        edits[0].segment_duration, 500,
        "half a second in movie ticks"
    );
    assert_eq!(edits[1].media_time, 0);
    assert!(
        reader.tracks()[&1].trak.edts.is_none(),
        "the track that starts the movie needs no edit"
    );
}

/// A file read through the demuxer and written back sample for sample keeps
/// its tracks, codec configuration and timing.
#[test]
fn remuxing_a_file_keeps_tracks_configuration_and_timing() {
    let video = av1();
    let source = output("remux-source.mov");
    let remuxed = output("remux-copy.mp4");
    let light = ContentLightLevel::new(1_000, 400);
    let hdr = color(
        ColorPrimaries::Bt2020,
        TransferFunction::Pq,
        MatrixCoefficients::Bt2020NonConstantLuminance,
        ColorRange::Limited,
        Some(light),
    );
    write(
        &source.path,
        ContainerFormat::Mov,
        vec![
            (video.track(1, hdr), video.samples(1, Some([0, 3, 1, 2]), 2)),
            (aac_track(2), aac_samples(2, 0)),
        ],
    );

    let mut tracks = read(&source.path, TrackKind::Video);
    tracks.extend(read(&source.path, TrackKind::Audio));
    let signaled = probe_mp4_color_info(&source.path, None).expect("probe source");
    assert_eq!(signaled, hdr);
    write(
        &remuxed.path,
        ContainerFormat::Mp4,
        tracks
            .iter()
            .map(|(info, samples)| {
                let info = if info.kind() == TrackKind::Video {
                    info.clone().with_video_color_info(signaled)
                } else {
                    info.clone()
                };
                (info, samples.clone())
            })
            .collect(),
    );

    let mut copied = read(&remuxed.path, TrackKind::Video);
    copied.extend(read(&remuxed.path, TrackKind::Audio));
    assert_eq!(copied.len(), tracks.len());
    for ((info, samples), (copied_info, copied_samples)) in tracks.iter().zip(&copied) {
        assert_eq!(info.id(), copied_info.id());
        assert_eq!(info.codec(), copied_info.codec());
        assert_eq!(info.timescale(), copied_info.timescale());
        assert_eq!(info.video_dimensions(), copied_info.video_dimensions());
        assert_eq!(info.audio_layout(), copied_info.audio_layout());
        assert_eq!(
            info.decoder_configuration(),
            copied_info.decoder_configuration()
        );
        assert_samples(copied_samples, samples);
    }
    assert_eq!(
        probe_mp4_color_info(&remuxed.path, None).expect("probe copy"),
        signaled
    );
    assert_eq!(first_brand(&remuxed.path), *b"isom");
}

#[test]
fn color_signaling_round_trips_through_the_probe() {
    let colors = [
        bt709(),
        color(
            ColorPrimaries::Bt601,
            TransferFunction::Sdr,
            MatrixCoefficients::Bt601,
            ColorRange::Full,
            None,
        ),
        color(
            ColorPrimaries::Bt2020,
            TransferFunction::Hlg,
            MatrixCoefficients::Bt2020NonConstantLuminance,
            ColorRange::Limited,
            None,
        ),
        color(
            ColorPrimaries::DisplayP3,
            TransferFunction::Pq,
            MatrixCoefficients::Bt709,
            ColorRange::Full,
            Some(ContentLightLevel::new(1_000, 400)),
        ),
    ];
    let video = av1();
    for (index, signaled) in colors.into_iter().enumerate() {
        let file = output(&format!("color-{index}.mp4"));
        write(
            &file.path,
            ContainerFormat::Mp4,
            vec![(video.track(1, signaled), video.samples(1, None, 0))],
        );
        assert_eq!(
            probe_mp4_color_info(&file.path, None).expect("probe"),
            signaled
        );
    }
}

#[test]
fn mdat_larger_than_the_compact_header_is_patched_in_place() {
    let video = av1();
    let mut muxer = Muxer::new(Cursor::new(Vec::new()), ContainerFormat::Mp4).expect("muxer");
    muxer.add_track(video.track(1, bt709())).expect("track");
    let samples = video.samples(1, None, 0);
    for sample in &samples {
        muxer.write_sample(sample).expect("sample");
    }
    let bytes = muxer.finish().expect("finish").into_inner();

    let payload: usize = samples.iter().map(|s| s.data().len()).sum();
    let mdat = bytes
        .windows(4)
        .position(|window| window == b"mdat")
        .expect("mdat");
    assert_eq!(
        bytes[mdat - 4..mdat],
        1_u32.to_be_bytes(),
        "largesize marker"
    );
    let size = u64::from_be_bytes(bytes[mdat + 4..mdat + 12].try_into().expect("size"));
    assert_eq!(size, u64::try_from(payload).expect("payload") + 16);
}

#[test]
fn chunk_offsets_switch_to_co64_past_32_bits() {
    let track = |offsets: &[u64]| MuxTrack {
        info: aac_track(1),
        spec: TrackSpec::new(
            1,
            AAC_SAMPLE_RATE,
            codec_config(&aac_track(1)).expect("config"),
        ),
        samples: Vec::new(),
        chunks: offsets
            .iter()
            .map(|&offset| Chunk { offset, samples: 1 })
            .collect(),
    };
    assert!(matches!(
        track(&[40, u64::from(u32::MAX)]).chunk_offsets(),
        StblChild::Stco(_)
    ));
    let StblChild::Co64(table) = track(&[40, u64::from(u32::MAX) + 1]).chunk_offsets() else {
        panic!("an offset past u32::MAX must select co64");
    };
    assert_eq!(table.entries, [40, u64::from(u32::MAX) + 1]);
}

#[test]
fn invalid_input_is_rejected() {
    let video = av1();
    let mut muxer = Muxer::new(Cursor::new(Vec::new()), ContainerFormat::Mp4).expect("muxer");
    muxer.add_track(video.track(1, bt709())).expect("track");
    let samples = video.samples(1, None, 0);

    assert!(
        muxer.add_track(video.track(1, bt709())).is_err(),
        "duplicate track id"
    );
    assert!(
        muxer.write_sample(&video.samples(9, None, 0)[0]).is_err(),
        "undeclared track"
    );
    muxer.write_sample(&samples[0]).expect("first sample");
    assert!(
        muxer.write_sample(&samples[0]).is_err(),
        "decode time must advance"
    );
    let wrong_scale = EncodedSample::new(
        track_id(1),
        time(FRAME_TICKS, 90_000),
        time(FRAME_TICKS, 90_000),
        time(FRAME_TICKS, 90_000),
        false,
        samples[1].data().clone(),
    );
    assert!(
        muxer.write_sample(&wrong_scale).is_err(),
        "timescale must match the track"
    );
    let empty = EncodedSample::new(
        track_id(1),
        samples[1].decode_time(),
        samples[1].presentation_time(),
        samples[1].duration(),
        false,
        Bytes::new(),
    );
    assert!(muxer.write_sample(&empty).is_err(), "empty sample");

    let unsupported = TrackInfo::audio(
        track_id(3),
        Codec::Opus,
        nz(48_000),
        Bytes::new(),
        AudioLayout {
            channels: nz(2),
            sample_rate: nz(48_000),
            sample_size: 16,
        },
    );
    assert!(muxer.add_track(unsupported).is_err(), "unsupported codec");
    let mut dolby = bt709();
    dolby.dolby_vision = true;
    assert!(
        muxer.add_track(video.track(4, dolby)).is_err(),
        "Dolby Vision cannot be signaled"
    );

    muxer.add_track(aac_track(2)).expect("audio track");
    assert!(muxer.finish().is_err(), "a declared track without samples");
    assert!(
        Muxer::new(Cursor::new(Vec::new()), ContainerFormat::Mp4)
            .expect("muxer")
            .finish()
            .is_err(),
        "a file without tracks"
    );
}
