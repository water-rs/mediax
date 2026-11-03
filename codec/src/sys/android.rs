//! Android `MediaCodec` hardware encoding and decoding.

use crate::{
    Bitrate, BitstreamFormat, CodecError, DecodePacket, DecodedPixelLayout, EncodedPacket,
    EncoderCodec, EncoderConfig, EncoderProfile, FrameOptions, FrameRate, H264Level, H264Profile,
    annexb::AnnexBPackets, bitstream::NalStreamConverter, config::decoded_pixel_layout,
};
use mediax_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorInfo,
};
use ndk::media::media_codec::{MediaCodec, MediaCodecDirection};
use ndk::media::media_format::MediaFormat;
use std::fmt;
use std::mem::MaybeUninit;
use std::time::Duration;

const COLOR_FORMAT_YUV420_SEMIPLANAR: i32 = 0x15;
const COLOR_FORMAT_YUV_P010: i32 = 0x36;
const COLOR_STANDARD_BT709: i32 = 1;
const COLOR_STANDARD_BT601_NTSC: i32 = 4;
const COLOR_STANDARD_BT2020: i32 = 6;
const COLOR_STANDARD_BT2020_CONSTANT_LUMINANCE: i32 = 7;
const COLOR_TRANSFER_SDR_VIDEO: i32 = 3;
const COLOR_TRANSFER_ST2084: i32 = 6;
const COLOR_TRANSFER_HLG: i32 = 7;
const COLOR_RANGE_FULL: i32 = 1;
const COLOR_RANGE_LIMITED: i32 = 2;
const BUFFER_FLAG_END_OF_STREAM: u32 = 4;
const CODEC_INPUT_TIMEOUT: Duration = Duration::from_secs(1);
const CODEC_OUTPUT_TIMEOUT: Duration = Duration::from_secs(1);

/// Internal codec type for Android implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecType {
    H264,
    H265,
}

impl CodecType {
    const fn mime_type(self) -> &'static str {
        match self {
            Self::H264 => "video/avc",
            Self::H265 => "video/hevc",
        }
    }
}

/// Android `MediaCodec` hardware decoder.
pub struct AndroidDecoder {
    codec: MediaCodec,
    codec_type: CodecType,
    width: u32,
    height: u32,
    output_layout: DecodedPixelLayout,
    input_bitstream: NalStreamConverter,
    last_presentation_time_us: u64,
    ended: bool,
}

impl fmt::Debug for AndroidDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AndroidDecoder")
            .field("codec_type", &self.codec_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

// SAFETY: MediaCodec is only accessed from within the struct methods,
// and we ensure no concurrent mutable access through &self/&mut self.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for AndroidDecoder {}

/// Decoded frame from Android `MediaCodec`.
#[derive(Clone)]
pub struct AndroidFrame {
    /// NV12 data: Y plane followed by interleaved UV plane.
    pub data: Vec<u8>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Presentation timestamp in nanoseconds.
    pub timestamp_ns: u64,
    /// Native bi-planar pixel layout.
    pub layout: DecodedPixelLayout,
}

impl fmt::Debug for AndroidFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AndroidFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("timestamp_ns", &self.timestamp_ns)
            .finish_non_exhaustive()
    }
}

impl AndroidDecoder {
    /// Create a new Android hardware decoder.
    ///
    /// # Arguments
    /// * `codec` - The codec type (H264 or H265)
    /// * `config` - Codec configuration data (avcC for H264, hvcC for H265). Can be None.
    /// * `width` - Video width in pixels
    /// * `height` - Video height in pixels
    #[allow(clippy::cast_possible_wrap)] // Video dimensions won't exceed i32::MAX
    pub fn new(
        codec_type: CodecType,
        config: Option<&[u8]>,
        width: u32,
        height: u32,
    ) -> Result<Self, CodecError> {
        let mime_type = codec_type.mime_type();

        // Create MediaCodec decoder
        let codec = MediaCodec::from_decoder_type(mime_type)
            .ok_or_else(|| CodecError::InitializationFailed("MediaCodec create failed".into()))?;

        // Create format with dimensions
        let mut format = MediaFormat::new();
        format.set_str("mime", mime_type);
        format.set_i32("width", width as i32);
        format.set_i32("height", height as i32);
        let output_layout = decoded_pixel_layout(codec_type == CodecType::H265, config)?;
        let color_format = match output_layout {
            DecodedPixelLayout::Nv12 => COLOR_FORMAT_YUV420_SEMIPLANAR,
            DecodedPixelLayout::P010 => COLOR_FORMAT_YUV_P010,
        };
        format.set_i32("color-format", color_format);

        let input_bitstream = NalStreamConverter::new(codec_type == CodecType::H265, config)?;
        let (primary_csd, secondary_csd) = input_bitstream.codec_specific_data();
        if let Some(primary_csd) = primary_csd {
            format.set_buffer("csd-0", primary_csd);
        }
        if let Some(secondary_csd) = secondary_csd {
            format.set_buffer("csd-1", secondary_csd);
        }

        // Configure the codec
        codec
            .configure(
                &format,
                None, // No surface for raw output
                MediaCodecDirection::Decoder,
            )
            .map_err(|e| CodecError::InitializationFailed(format!("MediaCodec configure: {e}")))?;

        // Start the codec
        codec
            .start()
            .map_err(|e| CodecError::InitializationFailed(format!("MediaCodec start: {e}")))?;

        Ok(Self {
            codec,
            codec_type,
            width,
            height,
            output_layout,
            input_bitstream,
            last_presentation_time_us: 0,
            ended: false,
        })
    }

    /// Decode compressed video data.
    pub fn decode(&mut self, packet: DecodePacket<'_>) -> Result<Vec<AndroidFrame>, CodecError> {
        use ndk::media::media_codec::{DequeuedInputBufferResult, DequeuedOutputBufferInfoResult};

        if self.ended {
            return Err(CodecError::DecodingFailed(
                "cannot submit compressed data after end of stream".into(),
            ));
        }
        let data = self.input_bitstream.convert_sample(packet.data())?;

        // Get input buffer
        let input_result = self
            .codec
            .dequeue_input_buffer(CODEC_INPUT_TIMEOUT)
            .map_err(|e| CodecError::DecodingFailed(format!("dequeue_input_buffer: {e}")))?;

        let mut input_buffer = match input_result {
            DequeuedInputBufferResult::Buffer(buf) => buf,
            DequeuedInputBufferResult::TryAgainLater => {
                return Err(CodecError::DecodingFailed(
                    "MediaCodec did not provide an input buffer before the input timeout".into(),
                ));
            }
        };

        let buffer_slice = input_buffer.buffer_mut();
        if data.len() > buffer_slice.len() {
            return Err(CodecError::DecodingFailed(format!(
                "compressed access unit is {} bytes but MediaCodec input capacity is {} bytes",
                data.len(),
                buffer_slice.len()
            )));
        }
        let copy_len = data.len();
        // SAFETY: We're writing valid data into the buffer
        for (i, byte) in data[..copy_len].iter().enumerate() {
            buffer_slice[i] = MaybeUninit::new(*byte);
        }

        // Queue the input buffer
        let presentation_time_us = u64::try_from(packet.presentation_time().as_micros())
            .map_err(|_| CodecError::DecodingFailed("presentation timestamp exceeds u64".into()))?;
        self.last_presentation_time_us = presentation_time_us;
        self.codec
            .queue_input_buffer(input_buffer, 0, copy_len, presentation_time_us, 0)
            .map_err(|e| CodecError::DecodingFailed(format!("queue_input_buffer: {e}")))?;

        // Collect output frames
        let mut frames = Vec::new();

        loop {
            let output_result = self
                .codec
                .dequeue_output_buffer(Duration::ZERO)
                .map_err(|e| CodecError::DecodingFailed(format!("dequeue_output_buffer: {e}")))?;

            match output_result {
                DequeuedOutputBufferInfoResult::Buffer(output_buffer) => {
                    let frame = self.copy_output_frame(&output_buffer);
                    self.codec
                        .release_output_buffer(output_buffer, false)
                        .map_err(|error| {
                            CodecError::DecodingFailed(format!("release_output_buffer: {error}"))
                        })?;
                    frames.push(frame?);
                }
                DequeuedOutputBufferInfoResult::TryAgainLater
                | DequeuedOutputBufferInfoResult::OutputBuffersChanged => break,
                DequeuedOutputBufferInfoResult::OutputFormatChanged => {}
            }
        }

        Ok(frames)
    }

    /// Signals end of stream and returns every delayed output frame.
    pub fn drain(&mut self) -> Result<Vec<AndroidFrame>, CodecError> {
        use ndk::media::media_codec::{DequeuedInputBufferResult, DequeuedOutputBufferInfoResult};

        if self.ended {
            return Ok(Vec::new());
        }
        let input_buffer = match self
            .codec
            .dequeue_input_buffer(CODEC_INPUT_TIMEOUT)
            .map_err(|error| CodecError::DecodingFailed(format!("dequeue EOS input: {error}")))?
        {
            DequeuedInputBufferResult::Buffer(buffer) => buffer,
            DequeuedInputBufferResult::TryAgainLater => {
                return Err(CodecError::DecodingFailed(
                    "MediaCodec did not provide an EOS input buffer before the input timeout"
                        .into(),
                ));
            }
        };
        self.codec
            .queue_input_buffer(
                input_buffer,
                0,
                0,
                self.last_presentation_time_us,
                BUFFER_FLAG_END_OF_STREAM,
            )
            .map_err(|error| CodecError::DecodingFailed(format!("queue EOS input: {error}")))?;
        self.ended = true;

        let mut frames = Vec::new();
        loop {
            match self
                .codec
                .dequeue_output_buffer(CODEC_OUTPUT_TIMEOUT)
                .map_err(|error| {
                    CodecError::DecodingFailed(format!("dequeue EOS output: {error}"))
                })? {
                DequeuedOutputBufferInfoResult::Buffer(output_buffer) => {
                    let end_of_stream =
                        output_buffer.info().flags() & BUFFER_FLAG_END_OF_STREAM != 0;
                    let frame = (output_buffer.info().size() > 0)
                        .then(|| self.copy_output_frame(&output_buffer));
                    self.codec
                        .release_output_buffer(output_buffer, false)
                        .map_err(|error| {
                            CodecError::DecodingFailed(format!("release EOS output: {error}"))
                        })?;
                    if let Some(frame) = frame {
                        frames.push(frame?);
                    }
                    if end_of_stream {
                        return Ok(frames);
                    }
                }
                DequeuedOutputBufferInfoResult::OutputFormatChanged
                | DequeuedOutputBufferInfoResult::OutputBuffersChanged => {}
                DequeuedOutputBufferInfoResult::TryAgainLater => {
                    return Err(CodecError::DecodingFailed(
                        "MediaCodec did not emit EOS before the output timeout".into(),
                    ));
                }
            }
        }
    }

    fn copy_output_frame(
        &self,
        output_buffer: &ndk::media::media_codec::OutputBuffer<'_>,
    ) -> Result<AndroidFrame, CodecError> {
        let info = output_buffer.info();
        let buffer_data = output_buffer.buffer();
        let format = output_buffer.format();
        let color_format = format.i32("color-format").ok_or_else(|| {
            CodecError::DecodingFailed("MediaCodec output has no color-format".into())
        })?;
        let expected_color_format = match self.output_layout {
            DecodedPixelLayout::Nv12 => COLOR_FORMAT_YUV420_SEMIPLANAR,
            DecodedPixelLayout::P010 => COLOR_FORMAT_YUV_P010,
        };
        if color_format != expected_color_format {
            return Err(CodecError::Unsupported(format!(
                "MediaCodec returned color format {color_format:#x}; expected {expected_color_format:#x}"
            )));
        }

        let coded_width = positive_format_size(&format, "width", self.width)?;
        let coded_height = positive_format_size(&format, "height", self.height)?;
        let stride = positive_format_size(&format, "stride", coded_width)?;
        let slice_height = positive_format_size(&format, "slice-height", coded_height)?;
        let crop_left = non_negative_format_size(&format, "crop-left", 0)?;
        let crop_top = non_negative_format_size(&format, "crop-top", 0)?;
        let crop_right = non_negative_format_size(&format, "crop-right", coded_width - 1)?;
        let crop_bottom = non_negative_format_size(&format, "crop-bottom", coded_height - 1)?;
        if crop_left > crop_right
            || crop_top > crop_bottom
            || crop_right >= stride
            || crop_bottom >= slice_height
        {
            return Err(CodecError::DecodingFailed(format!(
                "invalid MediaCodec crop [{crop_left},{crop_top}]..=[{crop_right},{crop_bottom}] for stride {stride} and slice height {slice_height}"
            )));
        }
        if crop_left % 2 != 0 || crop_top % 2 != 0 {
            return Err(CodecError::Unsupported(format!(
                "4:2:0 output requires an even crop origin, got ({crop_left}, {crop_top})"
            )));
        }
        let width = crop_right - crop_left + 1;
        let height = crop_bottom - crop_top + 1;
        let bytes_per_sample = match self.output_layout {
            DecodedPixelLayout::Nv12 => 1,
            DecodedPixelLayout::P010 => 2,
        };
        let source_row_bytes = stride as usize * bytes_per_sample;
        let source_y_bytes = source_row_bytes * slice_height as usize;
        let offset = usize::try_from(info.offset()).map_err(|_| {
            CodecError::DecodingFailed("MediaCodec returned a negative output offset".into())
        })?;
        let size = usize::try_from(info.size()).map_err(|_| {
            CodecError::DecodingFailed("MediaCodec returned a negative output size".into())
        })?;
        let end = offset.checked_add(size).ok_or_else(|| {
            CodecError::DecodingFailed("MediaCodec output range overflowed usize".into())
        })?;
        let source = buffer_data.get(offset..end).ok_or_else(|| {
            CodecError::DecodingFailed(format!(
                "MediaCodec output range {offset}..{end} exceeds buffer length {}",
                buffer_data.len()
            ))
        })?;
        let required_source_bytes = source_y_bytes + source_row_bytes * (slice_height as usize / 2);
        if source.len() < required_source_bytes {
            return Err(CodecError::DecodingFailed(format!(
                "MediaCodec output has {} bytes but its declared layout requires {required_source_bytes}",
                source.len()
            )));
        }

        let output_row_bytes = width as usize * bytes_per_sample;
        let mut data = Vec::with_capacity(self.output_layout.packed_len(width, height));
        copy_cropped_rows(
            source,
            source_row_bytes,
            crop_top as usize,
            height as usize,
            crop_left as usize * bytes_per_sample,
            output_row_bytes,
            &mut data,
        );
        copy_cropped_rows(
            &source[source_y_bytes..],
            source_row_bytes,
            crop_top as usize / 2,
            height as usize / 2,
            crop_left as usize * bytes_per_sample,
            output_row_bytes,
            &mut data,
        );

        Ok(AndroidFrame {
            data,
            width,
            height,
            timestamp_ns: u64::try_from(info.presentation_time_us())
                .map_err(|_| {
                    CodecError::DecodingFailed("MediaCodec returned a negative PTS".into())
                })?
                .saturating_mul(1_000),
            layout: self.output_layout,
        })
    }
}

fn positive_format_size(format: &MediaFormat, key: &str, default: u32) -> Result<u32, CodecError> {
    let value = format.i32(key).unwrap_or_else(|| default.cast_signed());
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| CodecError::DecodingFailed(format!("MediaCodec output {key} is {value}")))
}

fn non_negative_format_size(
    format: &MediaFormat,
    key: &str,
    default: u32,
) -> Result<u32, CodecError> {
    let value = format.i32(key).unwrap_or_else(|| default.cast_signed());
    u32::try_from(value)
        .map_err(|_| CodecError::DecodingFailed(format!("MediaCodec output {key} is {value}")))
}

fn copy_cropped_rows(
    source: &[u8],
    source_row_bytes: usize,
    first_row: usize,
    row_count: usize,
    first_byte: usize,
    output_row_bytes: usize,
    output: &mut Vec<u8>,
) {
    for row in first_row..first_row + row_count {
        let start = row * source_row_bytes + first_byte;
        output.extend_from_slice(&source[start..start + output_row_bytes]);
    }
}

impl Drop for AndroidDecoder {
    fn drop(&mut self) {
        let _ = self.codec.stop();
    }
}

/// Android `MediaCodec` hardware encoder.
pub struct AndroidEncoder {
    codec: MediaCodec,
    codec_type: CodecType,
    width: u32,
    height: u32,
    frame_rate: FrameRate,
    frame_count: i64,
    codec_config: Option<Vec<u8>>,
    packets: AnnexBPackets,
}

impl fmt::Debug for AndroidEncoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AndroidEncoder")
            .field("codec_type", &self.codec_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("frame_count", &self.frame_count)
            .finish_non_exhaustive()
    }
}

// SAFETY: MediaCodec is only accessed from within the struct methods,
// and we ensure no concurrent mutable access through &self/&mut self.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for AndroidEncoder {}
unsafe impl Sync for AndroidEncoder {}

/// `MediaCodecInfo.CodecProfileLevel.AVCProfileConstrainedBaseline`.
const AVC_PROFILE_CONSTRAINED_BASELINE: i32 = 0x1_0000;
/// `MediaCodecInfo.CodecProfileLevel.AVCProfileMain`.
const AVC_PROFILE_MAIN: i32 = 0x2;
/// `MediaCodecInfo.CodecProfileLevel.AVCProfileHigh`.
const AVC_PROFILE_HIGH: i32 = 0x8;
/// `MediaFormat.KEY_BITRATE_MODE` value `BITRATE_MODE_CBR`.
const BITRATE_MODE_CBR: i32 = 2;
/// `MediaFormat.KEY_PRIORITY` value for realtime work.
const PRIORITY_REALTIME: i32 = 0;

/// The `MediaCodecInfo.CodecProfileLevel.AVCLevel*` constant of `level`.
const fn avc_level(level: H264Level) -> i32 {
    match level {
        H264Level::L3_0 => 0x100,
        H264Level::L3_1 => 0x200,
        H264Level::L3_2 => 0x400,
        H264Level::L4_0 => 0x800,
        H264Level::L4_1 => 0x1000,
        H264Level::L4_2 => 0x2000,
        H264Level::L5_0 => 0x4000,
        H264Level::L5_1 => 0x8000,
        H264Level::L5_2 => 0x1_0000,
    }
}

/// A bitrate as `MediaFormat` takes it, an `i32` of bits per second.
fn format_bitrate(bitrate: Bitrate) -> Result<i32, CodecError> {
    i32::try_from(bitrate.bits_per_second().get()).map_err(|_| {
        CodecError::Unsupported("MediaCodec takes bitrates up to i32::MAX bits per second".into())
    })
}

impl AndroidEncoder {
    /// Create a new Android hardware encoder.
    pub fn new(config: &EncoderConfig) -> Result<Self, CodecError> {
        let (codec_type, bitstream) = match config.codec {
            EncoderCodec::H264(settings) => (CodecType::H264, settings.bitstream),
            EncoderCodec::H265(settings) => (CodecType::H265, settings.bitstream),
            EncoderCodec::Av1 => unreachable!("AV1 is encoded in software"),
        };
        let color_format = media_color_format(config.color)?;
        let mime_type = codec_type.mime_type();
        let width = i32::try_from(config.width.get())
            .map_err(|_| CodecError::Unsupported("frame width exceeds i32::MAX".into()))?;
        let height = i32::try_from(config.height.get())
            .map_err(|_| CodecError::Unsupported("frame height exceeds i32::MAX".into()))?;

        // Create MediaCodec encoder
        let codec = MediaCodec::from_encoder_type(mime_type)
            .ok_or_else(|| CodecError::InitializationFailed("MediaCodec create failed".into()))?;

        // Create format
        let mut format = MediaFormat::new();
        format.set_str("mime", mime_type);
        format.set_i32("width", width);
        format.set_i32("height", height);
        format.set_i32("color-format", COLOR_FORMAT_YUV420_SEMIPLANAR);
        format.set_i32("bitrate", format_bitrate(config.bitrate)?);
        #[allow(clippy::cast_precision_loss)] // frame rates are far below 2^24
        format.set_f32(
            "frame-rate",
            config.frame_rate.numerator().get() as f32
                / config.frame_rate.denominator().get() as f32,
        );
        // A negative interval asks for no periodic sync frames: only the first
        // frame and requested ones. `i-frame-interval` is in seconds.
        #[allow(clippy::cast_precision_loss)]
        format.set_f32(
            "i-frame-interval",
            config
                .keyframe_interval
                .map_or(-1.0, |interval| interval.as_secs_f32()),
        );
        format.set_i32("color-standard", color_format.standard);
        format.set_i32("color-transfer", color_format.transfer);
        format.set_i32("color-range", color_format.range);
        if let EncoderCodec::H264(settings) = config.codec {
            format.set_i32(
                "profile",
                match settings.profile {
                    H264Profile::ConstrainedBaseline => AVC_PROFILE_CONSTRAINED_BASELINE,
                    H264Profile::Main => AVC_PROFILE_MAIN,
                    H264Profile::High => AVC_PROFILE_HIGH,
                },
            );
            format.set_i32("level", avc_level(settings.level));
        }
        if bitstream == BitstreamFormat::AnnexB {
            // Ask the encoder for SPS/PPS in front of each IDR; `AnnexBPackets`
            // adds them for encoders that ignore the key.
            format.set_i32("prepend-sps-pps-to-idr-frames", 1);
        }
        if config.profile == EncoderProfile::Realtime {
            // Keys an older API level does not know are ignored by the codec.
            format.set_i32("bitrate-mode", BITRATE_MODE_CBR);
            format.set_i32("priority", PRIORITY_REALTIME);
            format.set_i32("low-latency", 1);
            format.set_i32("max-b-frames", 0);
            format.set_i32("latency", 1);
            format.set_i32(
                "operating-rate",
                config
                    .frame_rate
                    .rounded_fps()
                    .try_into()
                    .map_err(|_| CodecError::Unsupported("frame rate exceeds i32::MAX".into()))?,
            );
        }

        // Configure the codec
        codec
            .configure(&format, None, MediaCodecDirection::Encoder)
            .map_err(|e| CodecError::InitializationFailed(format!("MediaCodec configure: {e}")))?;

        // Start the codec
        codec
            .start()
            .map_err(|e| CodecError::InitializationFailed(format!("MediaCodec start: {e}")))?;

        Ok(Self {
            codec,
            codec_type,
            width: config.width.get(),
            height: config.height.get(),
            frame_rate: config.frame_rate,
            frame_count: 0,
            codec_config: None,
            packets: AnnexBPackets::new(codec_type == CodecType::H265, bitstream, Vec::new()),
        })
    }

    /// Change the target bitrate; `MediaCodec` applies it from the next frame.
    pub fn set_bitrate(&self, bitrate: Bitrate) -> Result<(), CodecError> {
        let mut parameters = MediaFormat::new();
        parameters.set_i32("video-bitrate", format_bitrate(bitrate)?);
        self.codec
            .set_parameters(parameters)
            .map_err(|e| CodecError::EncodingFailed(format!("MediaCodec set_parameters: {e}")))
    }

    /// Encode NV12 data to compressed video.
    #[allow(clippy::similar_names)] // y_size, uv_size are intentionally similar
    #[allow(clippy::cast_sign_loss)] // MediaCodec API uses signed integers
    pub fn encode_nv12(
        &mut self,
        nv12: &[u8],
        options: FrameOptions,
    ) -> Result<Vec<EncodedPacket>, CodecError> {
        use ndk::media::media_codec::{DequeuedInputBufferResult, DequeuedOutputBufferInfoResult};

        let y_size = (self.width * self.height) as usize;
        let uv_size = y_size / 2;
        let expected_size = y_size + uv_size;

        if nv12.len() != expected_size {
            return Err(CodecError::EncodingFailed(format!(
                "NV12 data size {} doesn't match expected {} for {}x{}",
                nv12.len(),
                expected_size,
                self.width,
                self.height
            )));
        }

        if options.keyframe {
            // `MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME`: the next input
            // frame becomes a sync frame.
            let mut parameters = MediaFormat::new();
            parameters.set_i32("request-sync", 0);
            self.codec
                .set_parameters(parameters)
                .map_err(|e| CodecError::EncodingFailed(format!("MediaCodec request-sync: {e}")))?;
        }

        // Get input buffer
        let input_result = self
            .codec
            .dequeue_input_buffer(Duration::from_millis(100))
            .map_err(|e| CodecError::EncodingFailed(format!("dequeue_input_buffer: {e}")))?;

        let mut input_buffer = match input_result {
            DequeuedInputBufferResult::Buffer(buf) => buf,
            DequeuedInputBufferResult::TryAgainLater => {
                return Err(CodecError::EncodingFailed(
                    "No input buffer available".into(),
                ));
            }
        };

        // Copy NV12 data to input buffer
        // SAFETY: We're writing valid data into the buffer
        let buffer_slice = input_buffer.buffer_mut();
        for (i, byte) in nv12.iter().enumerate() {
            buffer_slice[i] = MaybeUninit::new(*byte);
        }

        // Queue input buffer with the presentation time of this frame.
        let presentation_time_us =
            self.frame_count * 1_000_000 * i64::from(self.frame_rate.denominator().get())
                / i64::from(self.frame_rate.numerator().get());
        self.frame_count += 1;

        self.codec
            .queue_input_buffer(
                input_buffer,
                0,
                expected_size,
                presentation_time_us as u64,
                0,
            )
            .map_err(|e| CodecError::EncodingFailed(format!("queue_input_buffer: {e}")))?;

        // Collect encoded output
        let mut packets = Vec::new();

        loop {
            let output_result = self
                .codec
                .dequeue_output_buffer(Duration::from_millis(10))
                .map_err(|e| CodecError::EncodingFailed(format!("dequeue_output_buffer: {e}")))?;

            match output_result {
                DequeuedOutputBufferInfoResult::Buffer(output_buffer) => {
                    let info = output_buffer.info();
                    let buffer_data = output_buffer.buffer();

                    let offset = info.offset() as usize;
                    let size = info.size() as usize;
                    let bytes = buffer_data[offset..offset + size].to_vec();

                    // Check if this is codec config (SPS/PPS)
                    let flags = info.flags();
                    if flags & 2 != 0 {
                        // BUFFER_FLAG_CODEC_CONFIG
                        self.packets.set_parameter_sets(bytes.clone());
                        self.codec_config = Some(bytes);
                    } else if !bytes.is_empty() {
                        packets.push(self.packets.packet(bytes)?);
                    }

                    self.codec
                        .release_output_buffer(output_buffer, false)
                        .map_err(|e| {
                            CodecError::EncodingFailed(format!("release_output_buffer: {e}"))
                        })?;
                }
                DequeuedOutputBufferInfoResult::TryAgainLater
                | DequeuedOutputBufferInfoResult::OutputFormatChanged
                | DequeuedOutputBufferInfoResult::OutputBuffersChanged => break,
            }
        }

        Ok(packets)
    }

    /// Get the codec configuration data (avcC/hvcC) if available.
    #[must_use]
    pub fn get_codec_config(&self) -> Option<Vec<u8>> {
        self.codec_config.clone()
    }
}

impl Drop for AndroidEncoder {
    fn drop(&mut self) {
        let _ = self.codec.stop();
    }
}

/// Open a hardware decoder for a crate-level codec type.
pub fn open_decoder(
    codec: crate::CodecType,
    config: Option<&[u8]>,
    width: u32,
    height: u32,
) -> Result<crate::DecoderInner, CodecError> {
    let codec = match codec {
        crate::CodecType::H264 => CodecType::H264,
        crate::CodecType::H265 => CodecType::H265,
        crate::CodecType::Av1 => unreachable!(),
    };
    AndroidDecoder::new(codec, config, width, height).map(crate::DecoderInner::Android)
}

/// Open a hardware encoder for `config`.
pub fn open_encoder(config: &crate::EncoderConfig) -> Result<crate::EncoderInner, CodecError> {
    AndroidEncoder::new(config).map(crate::EncoderInner::Android)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaColorFormat {
    standard: i32,
    transfer: i32,
    range: i32,
}

fn media_color_format(color: VideoColorInfo) -> Result<MediaColorFormat, CodecError> {
    let standard = match (color.primaries, color.matrix) {
        (ColorPrimaries::Bt709, MatrixCoefficients::Bt709) => COLOR_STANDARD_BT709,
        (ColorPrimaries::Bt601, MatrixCoefficients::Bt601) => COLOR_STANDARD_BT601_NTSC,
        (ColorPrimaries::Bt2020, MatrixCoefficients::Bt2020NonConstantLuminance) => {
            COLOR_STANDARD_BT2020
        }
        (ColorPrimaries::Bt2020, MatrixCoefficients::Bt2020ConstantLuminance) => {
            COLOR_STANDARD_BT2020_CONSTANT_LUMINANCE
        }
        (primaries, matrix) => {
            return Err(CodecError::Unsupported(format!(
                "Android MediaCodec cannot represent color primaries/matrix pair ({primaries:?}, {matrix:?})"
            )));
        }
    };
    let transfer = match color.transfer {
        TransferFunction::Sdr => COLOR_TRANSFER_SDR_VIDEO,
        TransferFunction::Pq => COLOR_TRANSFER_ST2084,
        TransferFunction::Hlg => COLOR_TRANSFER_HLG,
    };
    let range = match color.range {
        ColorRange::Full => COLOR_RANGE_FULL,
        ColorRange::Limited => COLOR_RANGE_LIMITED,
    };
    if color.dolby_vision {
        return Err(CodecError::Unsupported(
            "Android MediaCodec cannot signal Dolby Vision metadata".into(),
        ));
    }
    Ok(MediaColorFormat {
        standard,
        transfer,
        range,
    })
}

#[cfg(test)]
mod color_tests {
    use super::*;

    fn color(
        primaries: ColorPrimaries,
        transfer: TransferFunction,
        matrix: MatrixCoefficients,
        range: ColorRange,
    ) -> VideoColorInfo {
        VideoColorInfo {
            primaries,
            transfer,
            matrix,
            range,
            content_light_level: None,
            dolby_vision: false,
        }
    }

    #[test]
    fn media_color_format_maps_supported_colorimetry() {
        let cases = [
            (
                color(
                    ColorPrimaries::Bt709,
                    TransferFunction::Sdr,
                    MatrixCoefficients::Bt709,
                    ColorRange::Limited,
                ),
                MediaColorFormat {
                    standard: COLOR_STANDARD_BT709,
                    transfer: COLOR_TRANSFER_SDR_VIDEO,
                    range: COLOR_RANGE_LIMITED,
                },
            ),
            (
                color(
                    ColorPrimaries::Bt601,
                    TransferFunction::Pq,
                    MatrixCoefficients::Bt601,
                    ColorRange::Full,
                ),
                MediaColorFormat {
                    standard: COLOR_STANDARD_BT601_NTSC,
                    transfer: COLOR_TRANSFER_ST2084,
                    range: COLOR_RANGE_FULL,
                },
            ),
            (
                color(
                    ColorPrimaries::Bt2020,
                    TransferFunction::Hlg,
                    MatrixCoefficients::Bt2020NonConstantLuminance,
                    ColorRange::Limited,
                ),
                MediaColorFormat {
                    standard: COLOR_STANDARD_BT2020,
                    transfer: COLOR_TRANSFER_HLG,
                    range: COLOR_RANGE_LIMITED,
                },
            ),
            (
                color(
                    ColorPrimaries::Bt2020,
                    TransferFunction::Pq,
                    MatrixCoefficients::Bt2020ConstantLuminance,
                    ColorRange::Full,
                ),
                MediaColorFormat {
                    standard: COLOR_STANDARD_BT2020_CONSTANT_LUMINANCE,
                    transfer: COLOR_TRANSFER_ST2084,
                    range: COLOR_RANGE_FULL,
                },
            ),
        ];

        for (color, expected) in cases {
            assert_eq!(media_color_format(color).expect("supported pair"), expected);
        }
    }

    #[test]
    fn media_color_format_rejects_unrepresentable_primaries_matrix_pair() {
        let color = color(
            ColorPrimaries::Bt709,
            TransferFunction::Sdr,
            MatrixCoefficients::Bt601,
            ColorRange::Limited,
        );
        let error = media_color_format(color).expect_err("mismatched pair is unsupported");
        assert!(error.to_string().contains("(Bt709, Bt601)"));
    }
}
