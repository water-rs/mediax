//! One entry point that opens any container this crate reads and lists its
//! tracks.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use mediax_core::Error;

use crate::isobmff::{TopLevelBoxSpan, scan_child_boxes, scan_top_level_boxes};
use crate::progressive::{ProgressiveTrack, ProgressiveTrackReader};
use crate::stream::{MpegTsDemuxer, MpegTsEvent, TrackId, TrackInfo, TrackKind};

/// Bytes of an MPEG-TS stream fed to the demuxer per read.
const TS_READ_CHUNK: usize = 64 * 1024;
/// Length of one MPEG-TS packet.
const TS_PACKET_LEN: usize = 188;
/// MPEG-TS packet sync byte.
const TS_SYNC_BYTE: u8 = 0x47;
/// Consecutive packets whose sync bytes identify a transport stream.
const TS_SYNC_PACKETS: usize = 3;
/// Top-level box types an ISO BMFF file may open with.
const ISO_BMFF_OPENING_BOXES: [[u8; 4]; 11] = [
    *b"ftyp", *b"styp", *b"moov", *b"moof", *b"mdat", *b"free", *b"skip", *b"wide", *b"sidx",
    *b"pnot", *b"uuid",
];

/// A container format `mediax-container` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MediaFormat {
    /// Progressive MP4 (ISO/IEC 14496-14).
    Mp4,
    /// Progressive MOV (Apple `QuickTime`).
    Mov,
    /// Fragmented MP4, including CMAF.
    FragmentedMp4,
    /// MPEG-2 transport stream.
    MpegTs,
}

impl MediaFormat {
    /// Identifies the container of the file at `path` from its content.
    ///
    /// The file name plays no part: an ISO BMFF file is told apart by its
    /// top-level boxes (a `qt  ` brand for MOV, `moof` boxes or a movie
    /// extends box for fragmented MP4) and a transport stream by the sync
    /// byte repeating every 188 bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, is not a container this
    /// crate reads, or has malformed ISO BMFF boxes.
    pub fn detect(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let mut prefix = Vec::with_capacity(TS_PACKET_LEN * TS_SYNC_PACKETS);
        std::fs::File::open(path)?
            .take((TS_PACKET_LEN * TS_SYNC_PACKETS) as u64)
            .read_to_end(&mut prefix)?;

        if is_transport_stream(&prefix) {
            return Ok(Self::MpegTs);
        }
        let opening_box = prefix.get(4..8);
        if !ISO_BMFF_OPENING_BOXES
            .iter()
            .any(|kind| opening_box == Some(&kind[..]))
        {
            return Err(Error::Unsupported(format!(
                "{} is neither an ISO BMFF file nor an MPEG transport stream",
                path.display()
            )));
        }

        let boxes = scan_top_level_boxes(path)?;
        let movie = boxes.iter().find(|span| span.kind == *b"moov");
        let fragmented = boxes.iter().any(|span| span.kind == *b"moof")
            || movie.map_or(Ok(false), |movie| has_movie_extends(path, movie))?;
        if fragmented {
            Ok(Self::FragmentedMp4)
        } else if prefix.get(8..12) == Some(b"qt  ") {
            Ok(Self::Mov)
        } else {
            Ok(Self::Mp4)
        }
    }
}

fn is_transport_stream(prefix: &[u8]) -> bool {
    let packets = prefix.len().div_ceil(TS_PACKET_LEN).min(TS_SYNC_PACKETS);
    packets > 0
        && (0..packets).all(|packet| prefix.get(packet * TS_PACKET_LEN) == Some(&TS_SYNC_BYTE))
}

fn has_movie_extends(path: &Path, movie: &TopLevelBoxSpan) -> Result<bool, Error> {
    Ok(scan_child_boxes(path, movie)?
        .iter()
        .any(|child| child.kind == *b"mvex"))
}

/// One track of a [`MediaFile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaTrack {
    info: TrackInfo,
    language: Option<String>,
    duration: Option<Duration>,
    sample_count: Option<u32>,
}

impl MediaTrack {
    /// Returns the decoder parameters and stable track identity.
    #[must_use]
    pub const fn info(&self) -> &TrackInfo {
        &self.info
    }

    /// Returns the ISO 639 language code the container declares, if any.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Returns the presentation duration when the container's sample tables
    /// give it without scanning the media data.
    #[must_use]
    pub const fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Returns the number of access units when the container's sample tables
    /// give it without scanning the media data.
    #[must_use]
    pub const fn sample_count(&self) -> Option<u32> {
        self.sample_count
    }
}

impl From<&ProgressiveTrack> for MediaTrack {
    fn from(track: &ProgressiveTrack) -> Self {
        Self {
            info: track.info().clone(),
            language: Some(track.language().to_owned()),
            duration: Some(track.duration()),
            sample_count: Some(track.sample_count()),
        }
    }
}

/// A media file opened from a path, whatever its container.
///
/// [`open`](Self::open) identifies the container from the file's content and
/// lists its audio and video tracks: from the sample tables of MP4, MOV and
/// fragmented MP4, and from the program map and in-band parameter sets at the
/// start of an MPEG transport stream. Samples are read with
/// [`ProgressiveTrackReader`] (MP4, MOV, fragmented MP4) or
/// [`MpegTsDemuxer`] (MPEG-TS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFile {
    format: MediaFormat,
    tracks: Vec<MediaTrack>,
}

impl MediaFile {
    /// Opens the file at `path` and lists its tracks.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, is not a container this
    /// crate reads, or declares a track whose codec parameters mediax cannot
    /// represent.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let format = MediaFormat::detect(path)?;
        let tracks = match format {
            MediaFormat::Mp4 | MediaFormat::Mov | MediaFormat::FragmentedMp4 => {
                iso_bmff_tracks(path)?
            }
            MediaFormat::MpegTs => transport_stream_tracks(path)?,
        };
        Ok(Self { format, tracks })
    }

    /// Returns the container format detected from the file's content.
    #[must_use]
    pub const fn format(&self) -> MediaFormat {
        self.format
    }

    /// Returns the tracks, video before audio in an ISO BMFF file and in
    /// track-id order otherwise.
    #[must_use]
    pub fn tracks(&self) -> &[MediaTrack] {
        &self.tracks
    }
}

fn iso_bmff_tracks(path: &Path) -> Result<Vec<MediaTrack>, Error> {
    let mut tracks = Vec::new();
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let reader = ProgressiveTrackReader::open(path, kind)?;
        tracks.extend(reader.tracks().iter().map(MediaTrack::from));
    }
    Ok(tracks)
}

fn transport_stream_tracks(path: &Path) -> Result<Vec<MediaTrack>, Error> {
    let mut file = std::fs::File::open(path)?;
    let mut demuxer = MpegTsDemuxer::new();
    let mut tracks = BTreeMap::<TrackId, TrackInfo>::new();
    let mut chunk = vec![0_u8; TS_READ_CHUNK];
    let mut resolved = false;
    while !resolved {
        let read = file.read(&mut chunk)?;
        let events = if read == 0 {
            resolved = true;
            demuxer.finish()?
        } else {
            demuxer.feed(&chunk[..read])?
        };
        for event in events {
            match event {
                MpegTsEvent::Track(info) => {
                    tracks.insert(info.id(), info);
                }
                MpegTsEvent::TracksResolved => resolved = true,
                MpegTsEvent::Sample(_) => {}
            }
        }
    }
    if tracks.is_empty() {
        return Err(Error::Container(format!(
            "{} declares no elementary streams",
            path.display()
        )));
    }
    Ok(tracks
        .into_values()
        .map(|info| MediaTrack {
            info,
            language: None,
            duration: None,
            sample_count: None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use broadcast_common::Package as _;
    use mediax_core::FrameRate;
    use transmux::{CmafMux, Media, Sample, Track, TrackSpec, TsMux};

    use super::{MediaFile, MediaFormat};
    use crate::muxer::codec_config;
    use crate::muxer::tests::{
        EncodedVideo, FRAMES, Output, VIDEO_TIMESCALE, aac_samples, aac_track, av1, bt709, h264,
        h265, nz, output, track_id, write,
    };
    use crate::stream::{Codec, EncodedSample, MediaTime, TrackInfo, TrackKind};
    use crate::{ChromaFormat, ContainerFormat, VideoLevel};

    /// 90 kHz, the MPEG-TS clock.
    const TS_TIMESCALE: u32 = 90_000;
    const AAC_TIMESCALE: u32 = 44_100;
    const VIDEO_ID: u32 = 1;
    const AUDIO_ID: u32 = 2;

    fn frame_rate(numerator: u32, denominator: u32) -> FrameRate {
        FrameRate::new(nz(numerator), nz(denominator))
    }

    fn only_video(file: &MediaFile) -> &TrackInfo {
        let mut videos = file
            .tracks()
            .iter()
            .filter(|track| track.info().kind() == TrackKind::Video);
        let video = videos.next().expect("a video track").info();
        assert!(videos.next().is_none(), "exactly one video track");
        video
    }

    /// Profile and level the encoder's configuration record carries, read
    /// byte by byte here rather than through the parser under test.
    fn record_profile_and_level(video: &EncodedVideo) -> (u8, Option<VideoLevel>) {
        let record = &video.record;
        match video.codec {
            Codec::H264 => (
                record[1],
                Some(VideoLevel::Numbered {
                    major: record[3] / 10,
                    minor: record[3] % 10,
                    high_tier: false,
                }),
            ),
            Codec::H265 => (
                record[1] & 0x1f,
                Some(VideoLevel::Numbered {
                    major: record[12] / 30,
                    minor: record[12] % 30 / 3,
                    high_tier: record[1] & 0x20 != 0,
                }),
            ),
            Codec::Av1 => {
                let index = record[1] & 0x1f;
                // Index 31 is "no limit" and has no level number.
                let level = (index != 31).then(|| VideoLevel::Numbered {
                    major: 2 + index / 4,
                    minor: index % 4,
                    high_tier: record[2] & 0x80 != 0,
                });
                (record[1] >> 5, level)
            }
            other => panic!("no fixture for {other:?}"),
        }
    }

    fn assert_parameters(info: &TrackInfo, video: &EncodedVideo, frame_rate: Option<FrameRate>) {
        assert_eq!(info.codec(), video.codec);
        let (profile, level) = record_profile_and_level(video);
        let parameters = info.video_parameters().expect("video parameters");
        assert_eq!(parameters.profile, profile, "profile");
        assert_eq!(parameters.level, level, "level");
        assert_eq!(parameters.bit_depth, 8, "the fixtures are 8-bit");
        assert_eq!(
            parameters.chroma_format,
            ChromaFormat::Yuv420,
            "the fixtures are 4:2:0"
        );
        assert_eq!(parameters.frame_rate, frame_rate, "frame rate");
        eprintln!(
            "{:?}: profile {profile}, level {level:?}, frame rate {:?}",
            video.codec, parameters.frame_rate
        );
    }

    fn assert_track_list(file: &MediaFile, format: MediaFormat) {
        assert_eq!(file.format(), format);
        assert_eq!(file.tracks().len(), 2, "{format:?} track count");
        assert_eq!(only_video(file).id(), track_id(VIDEO_ID));
        let audio = file
            .tracks()
            .iter()
            .find(|track| track.info().kind() == TrackKind::Audio)
            .expect("audio track")
            .info();
        assert_eq!(audio.id(), track_id(AUDIO_ID));
        assert_eq!(audio.codec(), Codec::Aac);
        assert_eq!(
            audio.audio_layout().expect("audio layout").channels.get(),
            2
        );
    }

    fn write_progressive(video: &EncodedVideo, format: ContainerFormat, name: &str) -> Output {
        let file = output(name);
        write(
            &file.path,
            format,
            vec![
                (
                    video.track(VIDEO_ID, bt709()),
                    video.samples(VIDEO_ID, None, 0),
                ),
                (aac_track(AUDIO_ID), aac_samples(AUDIO_ID, 0)),
            ],
        );
        file
    }

    /// A transmux track with every timestamp rescaled to `timescale`.
    fn transmux_track(info: &TrackInfo, timescale: u32, samples: &[EncodedSample]) -> Track {
        let spec = TrackSpec::new(
            info.id().get(),
            timescale,
            codec_config(info).expect("transmux codec configuration"),
        );
        let rescale = |time: MediaTime| {
            time.ticks() * i64::from(timescale) / i64::from(time.timescale().get())
        };
        let samples = samples
            .iter()
            .map(|sample| {
                Sample::new(
                    sample.data().to_vec(),
                    Some(rescale(sample.decode_time())),
                    Some(rescale(sample.presentation_time())),
                    Some(u32::try_from(rescale(sample.duration())).expect("duration fits u32")),
                    sample.is_keyframe(),
                )
            })
            .collect();
        Track::new(spec, samples)
    }

    fn media(video: &EncodedVideo, video_timescale: u32, audio_timescale: u32) -> Media {
        Media::new(
            vec![
                transmux_track(
                    &video.track(VIDEO_ID, bt709()),
                    video_timescale,
                    &video.samples(VIDEO_ID, None, 0),
                ),
                transmux_track(
                    &aac_track(AUDIO_ID),
                    audio_timescale,
                    &aac_samples(AUDIO_ID, 0),
                ),
            ],
            1_000,
        )
    }

    fn write_bytes(bytes: &[u8], name: &str) -> Output {
        let file = output(name);
        std::fs::write(&file.path, bytes).expect("write fixture");
        file
    }

    fn fragmented(video: &EncodedVideo, name: &str) -> Output {
        let media = media(video, VIDEO_TIMESCALE, AAC_TIMESCALE);
        write_bytes(&CmafMux::default().package(&media).expect("CMAF mux"), name)
    }

    fn transport_stream(video: &EncodedVideo, name: &str) -> Output {
        let media = media(video, TS_TIMESCALE, TS_TIMESCALE);
        write_bytes(&TsMux::new().package(&media).expect("TS mux"), name)
    }

    fn check_isobmff(video: &EncodedVideo, label: &str) {
        let thirty = Some(frame_rate(30, 1));
        for (format, extension, expected) in [
            (ContainerFormat::Mp4, "mp4", MediaFormat::Mp4),
            (ContainerFormat::Mov, "mov", MediaFormat::Mov),
        ] {
            let file = write_progressive(video, format, &format!("{label}.{extension}"));
            let opened = MediaFile::open(&file.path).expect("open progressive file");
            assert_track_list(&opened, expected);
            assert_parameters(only_video(&opened), video, thirty);
            let video_track = &opened.tracks()[0];
            assert_eq!(
                video_track.sample_count(),
                Some(u32::try_from(FRAMES).expect("frame count fits u32"))
            );
        }

        let file = fragmented(video, &format!("{label}-fragmented.mp4"));
        let opened = MediaFile::open(&file.path).expect("open fragmented file");
        assert_track_list(&opened, MediaFormat::FragmentedMp4);
        assert_parameters(only_video(&opened), video, thirty);
    }

    #[test]
    fn lists_av1_in_mp4_mov_and_fragmented_mp4() {
        check_isobmff(&av1(), "av1");
    }

    #[test]
    fn lists_h264_in_mp4_mov_and_fragmented_mp4() {
        let Some(video) = h264() else { return };
        check_isobmff(&video, "h264");
    }

    #[test]
    fn lists_h265_in_mp4_mov_and_fragmented_mp4() {
        let Some(video) = h265() else { return };
        check_isobmff(&video, "h265");
    }

    /// What an H.264 stream's first SPS declares through VUI timing, read with
    /// transmux's SPS decoder rather than the code under test.
    fn declared_h264_frame_rate(video: &EncodedVideo) -> Option<FrameRate> {
        let length = usize::from(u16::from_be_bytes([video.record[6], video.record[7]]));
        let sps = transmux::decode_avc_sps(&video.record[8..8 + length]).expect("decode SPS");
        let (scale, ticks) = (sps.time_scale?, sps.num_units_in_tick?);
        crate::parameters::rational(u64::from(scale), 2 * u64::from(ticks))
    }

    fn check_transport_stream(video: &EncodedVideo, label: &str) {
        let file = transport_stream(video, &format!("{label}.ts"));
        let opened = MediaFile::open(&file.path).expect("open transport stream");
        assert_eq!(opened.format(), MediaFormat::MpegTs);
        assert_eq!(opened.tracks().len(), 2);
        let info = only_video(&opened);
        assert!(info.source_pid().is_some());
        let audio = opened
            .tracks()
            .iter()
            .find(|track| track.info().kind() == TrackKind::Audio)
            .expect("audio track");
        assert_eq!(audio.info().codec(), Codec::Aac);
        // The stream declares a rate only through VUI timing in its SPS.
        let declared = (video.codec == Codec::H264)
            .then(|| declared_h264_frame_rate(video))
            .flatten();
        assert_parameters(info, video, declared);
    }

    #[test]
    fn lists_h264_in_a_transport_stream() {
        let Some(video) = h264() else { return };
        check_transport_stream(&video, "h264");
    }

    #[test]
    fn lists_h265_in_a_transport_stream() {
        let Some(video) = h265() else { return };
        check_transport_stream(&video, "h265");
    }

    #[test]
    fn transport_stream_reports_vui_timing_declared_in_band() {
        use transmux::{
            AVCConfigurationBox, AVCDecoderConfigurationRecord, AvcPps, AvcSps, CodecConfig,
        };

        let config = AVCConfigurationBox::new(AVCDecoderConfigurationRecord {
            configuration_version: 1,
            profile_indication: 66,
            profile_compatibility: 0xc0,
            level_indication: 30,
            length_size_minus_one: 3,
            sps: vec![AvcSps(crate::parameters::tests::avc_sps(Some((50, 1))))],
            pps: vec![AvcPps(vec![0x68, 0xce, 0x06, 0xe2])],
            chroma_format: None,
            bit_depth_luma_minus8: None,
            bit_depth_chroma_minus8: None,
            sps_ext: Vec::new(),
        });
        let spec = TrackSpec::new(
            VIDEO_ID,
            TS_TIMESCALE,
            CodecConfig::Avc {
                config,
                width: 320,
                height: 240,
            },
        );
        // Length-prefixed IDR slices at 25 frames per second.
        let samples = (0..8)
            .map(|index| {
                let time = index * 3_600;
                Sample::new(
                    vec![0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0xa0],
                    Some(time),
                    Some(time),
                    Some(3_600),
                    true,
                )
            })
            .collect();
        let media = Media::new(vec![Track::new(spec, samples)], TS_TIMESCALE);
        let file = write_bytes(&TsMux::new().package(&media).expect("TS mux"), "vui.ts");

        let opened = MediaFile::open(&file.path).expect("open transport stream");
        let parameters = only_video(&opened)
            .video_parameters()
            .expect("video parameters");
        assert_eq!(parameters.profile, 66);
        assert_eq!(parameters.frame_rate, Some(frame_rate(25, 1)));
    }

    #[test]
    fn detection_follows_content_not_extension() {
        let mp4 = write_progressive(&av1(), ContainerFormat::Mp4, "source.mp4");
        let bytes = std::fs::read(&mp4.path).expect("read fixture");
        let disguised = write_bytes(&bytes, "mp4-named.ts");
        assert_eq!(
            MediaFormat::detect(&disguised.path).expect("detect"),
            MediaFormat::Mp4
        );

        let Some(video) = h264() else { return };
        let ts = transport_stream(&video, "source.ts");
        let bytes = std::fs::read(&ts.path).expect("read fixture");
        let disguised = write_bytes(&bytes, "ts-named.mp4");
        assert_eq!(
            MediaFormat::detect(&disguised.path).expect("detect"),
            MediaFormat::MpegTs
        );
    }

    #[test]
    fn rejects_files_that_are_not_containers() {
        let zeros = write_bytes(&[0_u8; 1024], "zeros.bin");
        assert!(matches!(
            MediaFile::open(&zeros.path),
            Err(mediax_core::Error::Unsupported(_))
        ));
        let empty = write_bytes(&[], "empty.bin");
        assert!(MediaFile::open(&empty.path).is_err());
    }
}
