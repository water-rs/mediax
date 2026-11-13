//! `mediax transcode`: decode the video track, encode it again, copy the rest.

use std::io::{Seek, Write};
use std::num::NonZeroU32;
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use mediax_codec::{
    Bitrate, BitstreamFormat, CodecError, CodecType, DecodePacket, DecodedPixelLayout, Decoder,
    EncodedPacket, Encoder, EncoderCodec, EncoderConfig, EncoderProfile, EvenDimension,
    FrameOptions, FrameRate, H264Level, H264Profile, H264Settings, H265Settings,
};
use mediax_container::{
    Codec, EncodedSample, MediaFile, MediaTime, Muxer, TrackInfo, TrackKind, VideoDimensions,
};

use crate::args::OutputCodec;
use crate::error::CliError;
use crate::output::{container_format, remove_on_failure};
use crate::samples::Samples;
use crate::tracks::tracks_with_signaled_color;

const NANOS_PER_SECOND: i128 = 1_000_000_000;

/// How to encode the video track.
#[derive(Debug, Clone, Copy)]
pub struct TranscodeOptions {
    /// The codec to encode with.
    pub codec: OutputCodec,
    /// Target bitrate in bits per second; a rate that gives the codec a
    /// typical bits-per-pixel budget when absent.
    pub bitrate: Option<NonZeroU32>,
    /// Time between keyframes; the encoder's own choice when absent.
    pub keyframe_interval: Option<Duration>,
}

/// Re-encodes the video track of `input` into `output`, whose extension picks
/// the container, and copies the audio samples unchanged.
///
/// # Errors
///
/// Returns an error when the extension names no container, `input` has no
/// single video track, the host cannot decode the source or encode the
/// requested codec, the decoder emits frames the encoder cannot take, or
/// reading or writing fails. The unfinished `output` is removed.
pub fn transcode(input: &Path, output: &Path, options: &TranscodeOptions) -> Result<(), CliError> {
    let format = container_format(output)?;
    let file = MediaFile::open(input)?;
    let infos = tracks_with_signaled_color(input, &file)?;
    let (video_index, video) = single_video_track(input, &infos)?;
    let mut video_pipeline = VideoPipeline::new(video, options)?;
    let samples = Samples::open(input, &file)?;
    remove_on_failure(output, || {
        let mut muxer = Muxer::create(output, format)?;
        for (index, info) in infos.iter().enumerate() {
            if index != video_index {
                muxer.add_track(info.clone())?;
            }
        }
        let video_id = video.id();
        for sample in samples {
            let sample = sample?;
            if sample.track_id() == video_id {
                video_pipeline.push(&sample, &mut muxer)?;
            } else {
                muxer.write_sample(&sample)?;
            }
        }
        video_pipeline.finish(&mut muxer)?;
        muxer.finish()?;
        Ok(())
    })
}

fn single_video_track<'a>(
    path: &Path,
    infos: &'a [TrackInfo],
) -> Result<(usize, &'a TrackInfo), CliError> {
    let mut videos = infos
        .iter()
        .enumerate()
        .filter(|(_, info)| info.kind() == TrackKind::Video);
    let first = videos
        .next()
        .ok_or_else(|| CliError::NoVideoTrack(path.to_path_buf()))?;
    let others = videos.count();
    if others > 0 {
        return Err(CliError::SeveralVideoTracks {
            path: path.to_path_buf(),
            count: others + 1,
        });
    }
    Ok(first)
}

/// The decoder and encoder of the video track.
///
/// Each frame goes to the encoder with its presentation time, and each packet
/// that comes back is timed by the presentation and decode time it reports,
/// so a frame the encoder drops simply leaves no sample and a reordering
/// encoder gets composition offsets.
struct VideoPipeline {
    /// The source track, whose id, size, timescale and colour the output keeps.
    source: TrackInfo,
    dimensions: VideoDimensions,
    output_codec: Codec,
    decoder: Decoder,
    encoder: Encoder,
    frame_rate: FrameRate,
    pixels: Vec<u8>,
    /// The last packet, held until the next one gives its duration.
    held: Option<EncodedPacket>,
    /// Ticks between the decode times of the last two packets, the duration of
    /// the final one.
    last_gap: Option<i64>,
    frames: u64,
    packets: u64,
    track_declared: bool,
}

impl VideoPipeline {
    fn new(source: &TrackInfo, options: &TranscodeOptions) -> Result<Self, CliError> {
        let dimensions = source.video_dimensions().ok_or(CliError::NoFrames)?;
        let source_codec = match source.codec() {
            Codec::H264 => CodecType::H264,
            Codec::H265 => CodecType::H265,
            Codec::Av1 => CodecType::Av1,
            other => return Err(CliError::UndecodableSource(other)),
        };
        let rate = source
            .video_parameters()
            .and_then(|parameters| parameters.frame_rate)
            .ok_or(CliError::NoFrameRate)?;
        let frame_rate = FrameRate::new(rate.numerator(), rate.denominator());

        let (width, height) = (dimensions.width.get(), dimensions.height.get());
        let codec = encoder_codec(options.codec, width, height, frame_rate)?;
        let output_codec = match options.codec {
            OutputCodec::H264 => Codec::H264,
            OutputCodec::H265 => Codec::H265,
            OutputCodec::Av1 => Codec::Av1,
        };
        let bitrate = options.bitrate.map_or_else(
            || default_bitrate(options.codec, width, height, frame_rate),
            Ok,
        )?;
        let color = source.video_color_info().ok_or(CliError::NoFrames)?;
        let encoder = Encoder::new(EncoderConfig {
            codec,
            width: EvenDimension::new(width)?,
            height: EvenDimension::new(height)?,
            color,
            profile: EncoderProfile::Offline,
            bitrate: Bitrate::new(bitrate),
            frame_rate,
            keyframe_interval: options.keyframe_interval,
        })
        .map_err(|error| match error {
            CodecError::Unsupported(reason) => CliError::EncoderUnavailable {
                codec: codec.codec_type(),
                reason,
            },
            other => other.into(),
        })?;

        // H.264 and H.265 decoders take the configuration record without its
        // box header.
        let configuration = match source_codec {
            CodecType::H264 | CodecType::H265 => source.decoder_configuration().get(8..),
            CodecType::Av1 => None,
        };
        let decoder = Decoder::new(source_codec, configuration, width, height).map_err(
            |error| match error {
                CodecError::Unsupported(reason) => CliError::DecoderUnavailable {
                    codec: source_codec,
                    reason,
                },
                other => other.into(),
            },
        )?;
        Ok(Self {
            source: source.clone(),
            dimensions,
            output_codec,
            decoder,
            encoder,
            frame_rate,
            pixels: Vec::new(),
            held: None,
            last_gap: None,
            frames: 0,
            packets: 0,
            track_declared: false,
        })
    }

    fn push<W: Write + Seek>(
        &mut self,
        sample: &EncodedSample,
        muxer: &mut Muxer<W>,
    ) -> Result<(), CliError> {
        let packet = DecodePacket::new(sample.data(), sample.presentation_time().to_duration()?);
        let frames = self.decoder.decode(packet).collect::<Result<Vec<_>, _>>()?;
        for frame in frames {
            self.encode(&frame, muxer)?;
        }
        Ok(())
    }

    fn encode<W: Write + Seek>(
        &mut self,
        frame: &mediax_codec::DecodedFrame,
        muxer: &mut Muxer<W>,
    ) -> Result<(), CliError> {
        let layout = frame.pixel_layout();
        if layout != DecodedPixelLayout::Nv12 {
            return Err(CliError::UnsupportedPixelLayout { layout });
        }
        if (frame.width(), frame.height())
            != (self.dimensions.width.get(), self.dimensions.height.get())
        {
            return Err(CliError::FrameSizeChanged {
                expected: format!("{}x{}", self.dimensions.width, self.dimensions.height),
                actual: format!("{}x{}", frame.width(), frame.height()),
            });
        }
        self.pixels
            .resize(layout.packed_len(frame.width(), frame.height()), 0);
        frame.copy_to_buffer(&mut self.pixels);
        let options = FrameOptions {
            keyframe: self.frames == 0,
        };
        self.frames += 1;
        let packets = self
            .encoder
            .encode_nv12(&self.pixels, frame.timestamp(), options)
            .collect::<Result<Vec<_>, _>>()?;
        for packet in packets {
            self.emit(packet, muxer)?;
        }
        Ok(())
    }

    fn emit<W: Write + Seek>(
        &mut self,
        packet: EncodedPacket,
        muxer: &mut Muxer<W>,
    ) -> Result<(), CliError> {
        self.packets += 1;
        let decode = self.decode_ticks(&packet);
        if let Some(previous) = self.held.replace(packet) {
            let gap = decode - self.decode_ticks(&previous);
            self.last_gap = Some(gap);
            self.write(previous, gap, muxer)?;
        }
        Ok(())
    }

    fn write<W: Write + Seek>(
        &mut self,
        packet: EncodedPacket,
        duration_ticks: i64,
        muxer: &mut Muxer<W>,
    ) -> Result<(), CliError> {
        if !self.track_declared {
            let record = self
                .encoder
                .codec_config()
                .ok_or(CliError::NoCodecConfiguration)?;
            let track = TrackInfo::video(
                self.source.id(),
                self.output_codec,
                self.source.timescale(),
                Bytes::from(record),
                self.dimensions,
                self.source.video_color_info().ok_or(CliError::NoFrames)?,
            )?;
            muxer.add_track(track)?;
            self.track_declared = true;
        }
        let timescale = self.source.timescale();
        let presentation = MediaTime::new(
            self.ticks(packet.presentation_time.as_nanos().cast_signed()),
            timescale,
        );
        let decode = MediaTime::new(self.decode_ticks(&packet), timescale);
        muxer.write_sample(&EncodedSample::new(
            self.source.id(),
            decode,
            presentation,
            MediaTime::new(duration_ticks, timescale),
            packet.keyframe,
            Bytes::from(packet.data),
        ))?;
        Ok(())
    }

    /// `nanos` in ticks of the source track's timescale, to the nearest tick
    /// (halves round up, negative times included).
    fn ticks(&self, nanos: i128) -> i64 {
        let scaled = nanos * i128::from(self.source.timescale().get()) + NANOS_PER_SECOND / 2;
        i64::try_from(scaled.div_euclid(NANOS_PER_SECOND)).unwrap_or(i64::MAX)
    }

    /// The decode time of `packet` in ticks of the source track's timescale.
    fn decode_ticks(&self, packet: &EncodedPacket) -> i64 {
        self.ticks(i128::from(packet.decode_time_nanos))
    }

    fn finish<W: Write + Seek>(mut self, muxer: &mut Muxer<W>) -> Result<(), CliError> {
        let frames = self.decoder.drain().collect::<Result<Vec<_>, _>>()?;
        for frame in frames {
            self.encode(&frame, muxer)?;
        }
        for packet in self.encoder.flush()? {
            self.emit(packet, muxer)?;
        }
        if self.frames == 0 {
            return Err(CliError::NoFrames);
        }
        if let Some(packet) = self.held.take() {
            // The final packet lasts as long as the one before it, or one
            // frame period when it is the only one.
            let duration = self.last_gap.unwrap_or_else(|| {
                let scaled = u128::from(self.source.timescale().get())
                    * u128::from(self.frame_rate.denominator().get());
                let rate = u128::from(self.frame_rate.numerator().get());
                i64::try_from((scaled + rate / 2) / rate).unwrap_or(i64::MAX)
            });
            self.write(packet, duration, muxer)?;
        }
        tracing::info!(
            frames = self.frames,
            kept = self.packets,
            "transcoded the video track"
        );
        Ok(())
    }
}

fn encoder_codec(
    codec: OutputCodec,
    width: u32,
    height: u32,
    frame_rate: FrameRate,
) -> Result<EncoderCodec, CliError> {
    Ok(match codec {
        OutputCodec::H264 => EncoderCodec::H264(H264Settings {
            profile: H264Profile::High,
            level: h264_level(width, height, frame_rate)?,
            bitstream: BitstreamFormat::LengthPrefixed,
        }),
        OutputCodec::H265 => EncoderCodec::H265(H265Settings {
            bitstream: BitstreamFormat::LengthPrefixed,
        }),
        OutputCodec::Av1 => EncoderCodec::Av1,
    })
}

/// The lowest H.264 level (ITU-T H.264 table A-1) whose frame size and
/// macroblock rate limits hold the video.
fn h264_level(width: u32, height: u32, frame_rate: FrameRate) -> Result<H264Level, CliError> {
    /// Level, `MaxMBPS` (macroblocks per second) and `MaxFS` (macroblocks per
    /// frame).
    const LEVELS: [(H264Level, u64, u64); 8] = [
        (H264Level::L3_0, 40_500, 1_620),
        (H264Level::L3_1, 108_000, 3_600),
        (H264Level::L3_2, 216_000, 5_120),
        (H264Level::L4_0, 245_760, 8_192),
        (H264Level::L4_2, 522_240, 8_704),
        (H264Level::L5_0, 589_824, 22_080),
        (H264Level::L5_1, 983_040, 36_864),
        (H264Level::L5_2, 2_073_600, 36_864),
    ];
    let (columns, rows) = (
        u64::from(width.div_ceil(16)),
        u64::from(height.div_ceil(16)),
    );
    let frame_size = columns * rows;
    let rate = u128::from(frame_rate.numerator().get());
    let period = u128::from(frame_rate.denominator().get());
    let macroblock_rate =
        u64::try_from((u128::from(frame_size) * rate).div_ceil(period)).unwrap_or(u64::MAX);
    LEVELS
        .into_iter()
        .find(|&(_, max_rate, max_size)| {
            // Each dimension is also capped at sqrt(8 * MaxFS) macroblocks.
            macroblock_rate <= max_rate
                && frame_size <= max_size
                && columns * columns <= 8 * max_size
                && rows * rows <= 8 * max_size
        })
        .map(|(level, _, _)| level)
        .ok_or_else(|| {
            CliError::InvalidArgument(format!(
                "{width}x{height} at {} fps exceeds the highest H.264 level the encoder offers",
                frame_rate.rounded_fps()
            ))
        })
}

/// A bitrate giving `codec` its usual bits-per-pixel budget at this size and
/// rate: newer codecs reach the same quality with fewer bits.
fn default_bitrate(
    codec: OutputCodec,
    width: u32,
    height: u32,
    frame_rate: FrameRate,
) -> Result<NonZeroU32, CliError> {
    let bits_per_pixel = match codec {
        OutputCodec::H264 => 0.10,
        OutputCodec::H265 => 0.07,
        OutputCodec::Av1 => 0.06,
    };
    let fps = f64::from(frame_rate.numerator().get()) / f64::from(frame_rate.denominator().get());
    let bits = (f64::from(width) * f64::from(height) * fps * bits_per_pixel).round();
    // Clamped into the u32 range, so the cast is exact.
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    NonZeroU32::new(bits.clamp(1.0, f64::from(u32::MAX)) as u32)
        .ok_or_else(|| CliError::InvalidArgument(String::from("the default bitrate is zero")))
}
