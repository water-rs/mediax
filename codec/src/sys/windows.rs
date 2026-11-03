//! Windows Media Foundation hardware encoding and decoding.

use crate::{
    Bitrate, CodecError, DecodePacket, DecodedPixelLayout, EncodedPacket, EncoderCodec,
    EncoderConfig, EncoderProfile, FrameOptions, FrameRate, H264Profile,
    annexb::AnnexBPackets,
    bitstream::{NalStreamConverter, build_h264_avcc_from_annex_b, build_h265_hvcc_from_annex_b},
    config::decoded_pixel_layout,
    encoder_config::signed_nanos,
};
use mediax_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorInfo,
};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fmt;
use std::ptr;
use std::time::Duration;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncCommonRealTime, CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, ICodecAPI, IMF2DBuffer,
    IMFMediaType, IMFSample, IMFTransform, MF_E_ATTRIBUTENOTFOUND, MF_E_NO_MORE_TYPES,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG_SEQUENCE_HEADER,
    MF_MT_MPEG2_LEVEL, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION,
    MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX, MF_VERSION,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFNominalRange, MFNominalRange_0_255,
    MFNominalRange_16_235, MFSTARTUP_NOSOCKET, MFSampleExtension_DecodeTimestamp, MFShutdown,
    MFStartup, MFT_CATEGORY_VIDEO_DECODER, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_INFO, MFTEnumEx,
    MFVideoFormat_P010, MFVideoInterlace_Progressive, MFVideoPrimaries, MFVideoPrimaries_BT709,
    MFVideoPrimaries_BT2020, MFVideoPrimaries_SMPTE170M, MFVideoTransFunc_709,
    MFVideoTransFunc_2084, MFVideoTransFunc_HLG, MFVideoTransferFunction, MFVideoTransferMatrix,
    MFVideoTransferMatrix_BT601, MFVideoTransferMatrix_BT709, MFVideoTransferMatrix_BT2020_10,
    eAVEncCommonRateControlMode_CBR,
};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, Interface};

// Media Foundation GUIDs
const MF_MT_AVG_BITRATE: GUID = GUID::from_u128(0x20332624_fb0d_4d9e_bd0d_cbf6786c102e);

#[allow(non_upper_case_globals)]
const MFMediaType_Video: GUID = GUID::from_u128(0x73646976_0000_0010_8000_00aa00389b71);

#[allow(non_upper_case_globals)]
const MFVideoFormat_H264: GUID = GUID::from_u128(0x34363248_0000_0010_8000_00aa00389b71);

#[allow(non_upper_case_globals)]
const MFVideoFormat_HEVC: GUID = GUID::from_u128(0x43564548_0000_0010_8000_00aa00389b71);

#[allow(non_upper_case_globals)]
const MFVideoFormat_NV12: GUID = GUID::from_u128(0x3231564e_0000_0010_8000_00aa00389b71);

/// Internal codec type for Windows implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecType {
    H264,
    H265,
}

impl CodecType {
    const fn subtype(self) -> GUID {
        match self {
            Self::H264 => MFVideoFormat_H264,
            Self::H265 => MFVideoFormat_HEVC,
        }
    }
}

struct MediaFoundationSession;

impl MediaFoundationSession {
    fn new() -> Result<Self, CodecError> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|error| {
                    CodecError::InitializationFailed(format!("CoInitializeEx failed: {error}"))
                })?;
            if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) {
                CoUninitialize();
                return Err(CodecError::InitializationFailed(format!(
                    "MFStartup failed: {error}"
                )));
            }
        }
        Ok(Self)
    }
}

impl Drop for MediaFoundationSession {
    fn drop(&mut self) {
        unsafe {
            if let Err(error) = MFShutdown() {
                tracing::error!(%error, "MFShutdown failed");
            }
            CoUninitialize();
        }
    }
}

/// Decoded frame from Windows Media Foundation (NV12 format).
#[derive(Clone)]
pub struct WindowsFrame {
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

impl fmt::Debug for WindowsFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("timestamp_ns", &self.timestamp_ns)
            .finish_non_exhaustive()
    }
}

/// Windows Media Foundation hardware decoder.
#[allow(clippy::non_send_fields_in_send_ty)]
pub struct WindowsDecoder {
    transform: IMFTransform,
    codec_type: CodecType,
    width: u32,
    height: u32,
    output_stream_info: MFT_OUTPUT_STREAM_INFO,
    output_layout: DecodedPixelLayout,
    input_bitstream: NalStreamConverter,
    /// The exact presentation time submitted for each Media Foundation sample
    /// time, keyed by that sample time.
    ///
    /// Media Foundation counts in 100ns units, so a presentation time derived
    /// from a container timescale does not survive the round trip: 1024 ticks
    /// at 12288/s is 83.333333ms going in and 83.3333ms coming back. Callers
    /// match a decoded frame to the packet they submitted by exact timestamp,
    /// and 33ns of quantization is enough to miss.
    submitted_times: BTreeMap<i64, Duration>,
    _media_foundation: MediaFoundationSession,
}

impl fmt::Debug for WindowsDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsDecoder")
            .field("codec_type", &self.codec_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl WindowsDecoder {
    /// Create a new Windows hardware decoder.
    pub fn new(
        codec_type: CodecType,
        config: Option<&[u8]>,
        width: u32,
        height: u32,
    ) -> Result<Self, CodecError> {
        let media_foundation = MediaFoundationSession::new()?;

        unsafe {
            let input_bitstream = NalStreamConverter::new(codec_type == CodecType::H265, config)?;
            let input_type = create_video_type(codec_type.subtype(), width, height, (30, 1))?;
            if !input_bitstream.parameter_sets().is_empty() {
                input_type
                    .SetBlob(
                        &MF_MT_MPEG_SEQUENCE_HEADER,
                        input_bitstream.parameter_sets(),
                    )
                    .map_err(|error| {
                        CodecError::InitializationFailed(format!(
                            "SetBlob codec configuration failed: {error}"
                        ))
                    })?;
            }
            let output_layout = decoded_pixel_layout(codec_type == CodecType::H265, config)?;
            let output_subtype = match output_layout {
                DecodedPixelLayout::Nv12 => MFVideoFormat_NV12,
                DecodedPixelLayout::P010 => MFVideoFormat_P010,
            };
            let output_type = create_video_type(output_subtype, width, height, (30, 1))?;

            let transform = activate_hardware_transform(
                MFT_CATEGORY_VIDEO_DECODER,
                create_register_type_info(codec_type.subtype()),
                create_register_type_info(output_subtype),
                "decoder",
            )?;

            transform.SetInputType(0, &input_type, 0).map_err(|e| {
                CodecError::InitializationFailed(format!("SetInputType failed: {e}"))
            })?;

            transform.SetOutputType(0, &output_type, 0).map_err(|e| {
                CodecError::InitializationFailed(format!("SetOutputType failed: {e}"))
            })?;

            let output_stream_info = transform.GetOutputStreamInfo(0).map_err(|e| {
                CodecError::InitializationFailed(format!("GetOutputStreamInfo: {e}"))
            })?;

            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .ok();
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .ok();

            Ok(Self {
                transform,
                codec_type,
                width,
                height,
                output_stream_info,
                output_layout,
                input_bitstream,
                submitted_times: BTreeMap::new(),
                _media_foundation: media_foundation,
            })
        }
    }

    /// Decode compressed video data.
    pub fn decode(&mut self, packet: DecodePacket<'_>) -> Result<Vec<WindowsFrame>, CodecError> {
        unsafe {
            // In-band, not just in the media type: the Media Foundation
            // transform accepts `MF_MT_MPEG_SEQUENCE_HEADER` and still waits for
            // an SPS in the elementary stream, consuming every sample, emitting
            // no frame and reporting no error.
            let annex_b = self
                .input_bitstream
                .convert_sample_with_parameter_sets(packet.data())?;
            let input_sample = create_sample(&annex_b)?;
            let time_100ns =
                i64::try_from(packet.presentation_time().as_nanos() / 100).map_err(|_| {
                    CodecError::DecodingFailed("presentation timestamp exceeds i64".into())
                })?;
            input_sample.SetSampleTime(time_100ns).map_err(|error| {
                CodecError::DecodingFailed(format!("SetSampleTime failed: {error}"))
            })?;
            self.submitted_times
                .insert(time_100ns, packet.presentation_time());

            self.transform
                .ProcessInput(0, &input_sample, 0)
                .map_err(|e| CodecError::DecodingFailed(format!("ProcessInput: {e}")))?;

            self.collect_output()
        }
    }

    /// Drains every delayed Media Foundation output frame.
    pub fn drain(&mut self) -> Result<Vec<WindowsFrame>, CodecError> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                .map_err(|error| {
                    CodecError::DecodingFailed(format!("notify end of stream failed: {error}"))
                })?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .map_err(|error| {
                    CodecError::DecodingFailed(format!("start decoder drain failed: {error}"))
                })?;
            let frames = self.collect_output();
            // Anything still recorded here belongs to a sample the decoder
            // never emitted, and the stream is over; keeping it would grow the
            // map for the life of the decoder.
            self.submitted_times.clear();
            frames
        }
    }

    unsafe fn collect_output(&mut self) -> Result<Vec<WindowsFrame>, CodecError> {
        let mut frames = Vec::new();
        loop {
            let output_sample = if self.output_stream_info.dwFlags & 0x100 != 0 {
                None
            } else {
                let packed_len = self.output_layout.packed_len(self.width, self.height);
                Some(create_empty_sample(
                    (self.output_stream_info.cbSize as usize).max(packed_len),
                )?)
            };

            let mut output_buffer = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(output_sample),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            };

            let mut status = 0u32;
            let result = unsafe {
                self.transform.ProcessOutput(
                    0,
                    std::slice::from_mut(&mut output_buffer),
                    &raw mut status,
                )
            };
            let output_sample = take_output_sample(&mut output_buffer);

            match result {
                Ok(()) => {
                    if let Some(sample) = output_sample.as_ref() {
                        let mut frame = extract_biplanar_frame(
                            sample,
                            self.width,
                            self.height,
                            self.output_layout,
                        )?;
                        // Hand back the presentation time that went in, not the
                        // one Media Foundation's 100ns grid rounded it to.
                        let sample_time =
                            i64::try_from(frame.timestamp_ns / 100).map_err(|_| {
                                CodecError::DecodingFailed("decoded sample time exceeds i64".into())
                            })?;
                        if let Some(submitted) = self.submitted_times.remove(&sample_time) {
                            frame.timestamp_ns =
                                u64::try_from(submitted.as_nanos()).map_err(|_| {
                                    CodecError::DecodingFailed(
                                        "submitted presentation time exceeds u64 nanoseconds"
                                            .into(),
                                    )
                                })?;
                        }
                        frames.push(frame);
                    }
                }
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(frames),
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.renegotiate_output_type()?;
                }
                Err(error) => {
                    return Err(CodecError::DecodingFailed(format!(
                        "ProcessOutput: {error}"
                    )));
                }
            }
        }
    }

    fn renegotiate_output_type(&mut self) -> Result<(), CodecError> {
        let expected_subtype = output_subtype(self.output_layout);
        let mut index = 0;
        loop {
            let media_type = match unsafe { self.transform.GetOutputAvailableType(0, index) } {
                Ok(media_type) => media_type,
                Err(error) if error.code() == MF_E_NO_MORE_TYPES => {
                    return Err(CodecError::Unsupported(format!(
                        "Media Foundation stream change has no {expected_subtype:?} output type"
                    )));
                }
                Err(error) => {
                    return Err(CodecError::DecodingFailed(format!(
                        "GetOutputAvailableType failed during stream change: {error}"
                    )));
                }
            };
            index += 1;
            let subtype = unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) }.map_err(|error| {
                CodecError::DecodingFailed(format!(
                    "stream-change output type has no subtype: {error}"
                ))
            })?;
            if subtype != expected_subtype {
                continue;
            }
            let frame_size =
                unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE) }.map_err(|error| {
                    CodecError::DecodingFailed(format!(
                        "stream-change output type has no frame size: {error}"
                    ))
                })?;
            let width = u32::try_from(frame_size >> 32).expect("frame width is stored as u32");
            let height = u32::try_from(frame_size & u64::from(u32::MAX))
                .expect("frame height is stored as u32");
            unsafe { self.transform.SetOutputType(0, &media_type, 0) }.map_err(|error| {
                CodecError::DecodingFailed(format!(
                    "SetOutputType failed during stream change: {error}"
                ))
            })?;
            self.width = width;
            self.height = height;
            self.output_stream_info =
                unsafe { self.transform.GetOutputStreamInfo(0) }.map_err(|error| {
                    CodecError::DecodingFailed(format!(
                        "GetOutputStreamInfo failed after stream change: {error}"
                    ))
                })?;
            return Ok(());
        }
    }
}

/// Windows Media Foundation hardware encoder.
#[allow(clippy::non_send_fields_in_send_ty)]
pub struct WindowsEncoder {
    transform: IMFTransform,
    codec_api: ICodecAPI,
    codec_type: CodecType,
    width: u32,
    height: u32,
    frame_rate: FrameRate,
    /// The presentation time of the last packet that came without a
    /// `MFSampleExtension_DecodeTimestamp`; the next must follow it.
    last_undated_presentation: Option<Duration>,
    output_stream_info: MFT_OUTPUT_STREAM_INFO,
    codec_config: Option<Vec<u8>>,
    packets: AnnexBPackets,
    _media_foundation: MediaFoundationSession,
}

impl fmt::Debug for WindowsEncoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsEncoder")
            .field("codec_type", &self.codec_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// One encoded output sample with the times Media Foundation stamped on it.
struct OutputSample {
    data: Vec<u8>,
    /// `IMFSample::GetSampleTime`: the presentation time the input frame
    /// carried.
    presentation_time: Duration,
    /// `MFSampleExtension_DecodeTimestamp` in nanoseconds, when the transform
    /// sets it (it does for streams with B-frames).
    decode_time_nanos: Option<i64>,
}

/// `eAVEncH264VProfile_ConstrainedBase`.
const H264_PROFILE_CONSTRAINED_BASE: u32 = 256;
/// `eAVEncH264VProfile_Main`.
const H264_PROFILE_MAIN: u32 = 77;
/// `eAVEncH264VProfile_High`.
const H264_PROFILE_HIGH: u32 = 100;
/// `eAVEncH265VProfile_Main_420_8`.
const H265_PROFILE_MAIN: u32 = 1;
/// `MF_MT_AVG_BITRATE`-style 100ns units per second.
const HNS_PER_SECOND: i64 = 10_000_000;

/// Sets one `ICodecAPI` property.
fn set_codec_value(codec_api: &ICodecAPI, key: &GUID, value: &VARIANT) -> Result<(), String> {
    // SAFETY: `key` and `value` are valid for the call.
    unsafe { codec_api.SetValue(key, value) }.map_err(|error| error.to_string())
}

/// Wraps a failed encoder setup step.
fn init_error(what: &str, error: impl std::fmt::Display) -> CodecError {
    CodecError::InitializationFailed(format!("{what} failed: {error}"))
}

/// Sets bitrate, GOP size and, for realtime sessions, constant-bitrate
/// low-latency encoding without B-frames.
fn configure_rate_control(codec_api: &ICodecAPI, config: &EncoderConfig) -> Result<(), CodecError> {
    let bitrate = VARIANT::from(config.bitrate.bits_per_second().get());
    set_codec_value(codec_api, &CODECAPI_AVEncCommonMeanBitRate, &bitrate)
        .map_err(|error| init_error("setting the mean bitrate", error))?;
    let gop_size = config.keyframe_interval.map_or(u32::MAX, |interval| {
        u32::try_from(config.frame_rate.frames_in(interval)).unwrap_or(u32::MAX)
    });
    set_codec_value(
        codec_api,
        &CODECAPI_AVEncMPVGOPSize,
        &VARIANT::from(gop_size),
    )
    .map_err(|error| init_error("setting the GOP size", error))?;
    if config.profile == EncoderProfile::Realtime {
        // Constant bitrate keeps each frame inside its interval's budget;
        // low-latency mode and no B-frames remove the reordering delay.
        for (what, key, value) in [
            (
                "setting constant-bitrate rate control",
                &CODECAPI_AVEncCommonRateControlMode,
                VARIANT::from(eAVEncCommonRateControlMode_CBR.0.cast_unsigned()),
            ),
            (
                "enabling low-latency mode",
                &CODECAPI_AVLowLatencyMode,
                VARIANT::from(true),
            ),
            (
                "disabling B-frames",
                &CODECAPI_AVEncMPVDefaultBPictureCount,
                VARIANT::from(0_u32),
            ),
        ] {
            set_codec_value(codec_api, key, &value).map_err(|error| init_error(what, error))?;
        }
        // The real-time scheduling hint is optional: not every encoder
        // implements it (the one on hosted Windows runners does not), and the
        // settings above already carry the latency guarantees.
        if codec_value_supported(codec_api, &CODECAPI_AVEncCommonRealTime) {
            set_codec_value(
                codec_api,
                &CODECAPI_AVEncCommonRealTime,
                &VARIANT::from(1_u32),
            )
            .map_err(|error| init_error("enabling realtime mode", error))?;
        }
    }
    Ok(())
}

/// Whether the encoder implements an `ICodecAPI` property.
///
/// `ICodecAPI::IsSupported` answers `S_OK` for a supported property and
/// `S_FALSE` or an error otherwise; the generated wrapper folds `S_FALSE` into
/// success, so the raw result is compared here.
fn codec_value_supported(codec_api: &ICodecAPI, key: &GUID) -> bool {
    // SAFETY: `codec_api` is a live interface and `key` is valid for the call.
    unsafe { (Interface::vtable(codec_api).IsSupported)(Interface::as_raw(codec_api), key) == S_OK }
}

/// The compressed output media type: size, frame rate, colour, bitrate,
/// profile and (for H.264) level.
fn create_output_type(
    codec_type: CodecType,
    config: &EncoderConfig,
    frame_rate: (u32, u32),
) -> Result<IMFMediaType, CodecError> {
    let output_type = create_video_type(
        codec_type.subtype(),
        config.width.get(),
        config.height.get(),
        frame_rate,
    )?;
    set_color_metadata(&output_type, config.color)?;
    let (profile, level) = match config.codec {
        EncoderCodec::H264(settings) => (
            match settings.profile {
                H264Profile::ConstrainedBaseline => H264_PROFILE_CONSTRAINED_BASE,
                H264Profile::Main => H264_PROFILE_MAIN,
                H264Profile::High => H264_PROFILE_HIGH,
            },
            // `eAVEncH264VLevel*` values are the level_idc values.
            Some(u32::from(settings.level.idc())),
        ),
        EncoderCodec::H265(_) => (H265_PROFILE_MAIN, None),
        EncoderCodec::Av1 => unreachable!("AV1 is encoded in software"),
    };
    // SAFETY: `output_type` is a live media type and the keys are valid
    // attribute GUIDs.
    unsafe {
        output_type
            .SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate.bits_per_second().get())
            .map_err(|error| init_error("setting the average bitrate", error))?;
        output_type
            .SetUINT32(&MF_MT_MPEG2_PROFILE, profile)
            .map_err(|error| init_error("setting the profile", error))?;
        if let Some(level) = level {
            output_type
                .SetUINT32(&MF_MT_MPEG2_LEVEL, level)
                .map_err(|error| init_error("setting the level", error))?;
        }
    }
    Ok(output_type)
}

impl WindowsEncoder {
    /// Create a new Windows hardware encoder.
    pub fn new(config: &EncoderConfig) -> Result<Self, CodecError> {
        let (codec_type, bitstream) = match config.codec {
            EncoderCodec::H264(settings) => (CodecType::H264, settings.bitstream),
            EncoderCodec::H265(settings) => (CodecType::H265, settings.bitstream),
            EncoderCodec::Av1 => unreachable!("AV1 is encoded in software"),
        };
        let (width, height) = (config.width.get(), config.height.get());
        let media_foundation = MediaFoundationSession::new()?;

        unsafe {
            let transform = activate_hardware_transform(
                MFT_CATEGORY_VIDEO_ENCODER,
                create_register_type_info(MFVideoFormat_NV12),
                create_register_type_info(codec_type.subtype()),
                "encoder",
            )?;
            let codec_api = transform.cast::<ICodecAPI>().map_err(|error| {
                CodecError::Unsupported(format!(
                    "the hardware encoder does not expose ICodecAPI: {error}"
                ))
            })?;

            // Rate control and latency are set before the media types, which
            // is when Media Foundation encoders read them.
            configure_rate_control(&codec_api, config)?;

            let frame_rate = (
                config.frame_rate.numerator().get(),
                config.frame_rate.denominator().get(),
            );
            let output_type = create_output_type(codec_type, config, frame_rate)?;

            transform.SetOutputType(0, &output_type, 0).map_err(|e| {
                CodecError::InitializationFailed(format!("SetOutputType failed: {e}"))
            })?;

            let input_type = create_video_type(MFVideoFormat_NV12, width, height, frame_rate)?;
            set_color_metadata(&input_type, config.color)?;
            transform.SetInputType(0, &input_type, 0).map_err(|e| {
                CodecError::InitializationFailed(format!("SetInputType failed: {e}"))
            })?;

            let output_stream_info = transform.GetOutputStreamInfo(0).map_err(|e| {
                CodecError::InitializationFailed(format!("GetOutputStreamInfo: {e}"))
            })?;

            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .ok();
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .ok();

            // The encoder publishes its parameter sets on the output media
            // type as soon as the types are negotiated; read them right away
            // and keep re-reading lazily for transforms that fill the
            // attribute only once encoding has started.
            let sequence_header = sequence_header_from_media_type(&transform);
            let codec_config = sequence_header
                .as_deref()
                .and_then(|header| codec_config_from_sequence_header(header, codec_type));

            Ok(Self {
                transform,
                codec_api,
                codec_type,
                width,
                height,
                frame_rate: config.frame_rate,
                last_undated_presentation: None,
                output_stream_info,
                codec_config,
                packets: AnnexBPackets::new(
                    codec_type == CodecType::H265,
                    bitstream,
                    sequence_header.unwrap_or_default(),
                ),
                _media_foundation: media_foundation,
            })
        }
    }

    /// Change the mean bitrate; the encoder applies it from the next frame.
    pub fn set_bitrate(&self, bitrate: Bitrate) -> Result<(), CodecError> {
        set_codec_value(
            &self.codec_api,
            &CODECAPI_AVEncCommonMeanBitRate,
            &VARIANT::from(bitrate.bits_per_second().get()),
        )
        .map_err(|error| CodecError::EncodingFailed(format!("setting the mean bitrate: {error}")))
    }

    /// Encode NV12 data to compressed video.
    pub fn encode_nv12(
        &mut self,
        nv12: &[u8],
        presentation_time: Duration,
        options: FrameOptions,
    ) -> Result<Vec<EncodedPacket>, CodecError> {
        let y_size = self.width as usize * self.height as usize;
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
            // Applies to the next frame submitted.
            set_codec_value(
                &self.codec_api,
                &CODECAPI_AVEncVideoForceKeyFrame,
                &VARIANT::from(1_u32),
            )
            .map_err(|error| CodecError::EncodingFailed(format!("forcing a keyframe: {error}")))?;
        }

        unsafe {
            let input_sample = create_sample(nv12)?;
            let frame_duration_hns = HNS_PER_SECOND
                * i64::from(self.frame_rate.denominator().get())
                / i64::from(self.frame_rate.numerator().get());
            // The transform copies the sample time onto the output sample of
            // the frame it encodes.
            let time_100ns = i64::try_from(presentation_time.as_nanos() / 100).map_err(|_| {
                CodecError::EncodingFailed("presentation time exceeds i64 100 ns units".into())
            })?;
            tracing::debug!(
                time_100ns,
                duration_100ns = frame_duration_hns,
                keyframe = options.keyframe,
                "Media Foundation encoder input sample"
            );
            input_sample
                .SetSampleTime(time_100ns)
                .map_err(|e| CodecError::EncodingFailed(format!("SetSampleTime: {e}")))?;
            input_sample.SetSampleDuration(frame_duration_hns).ok();

            self.transform
                .ProcessInput(0, &input_sample, 0)
                .map_err(|e| CodecError::EncodingFailed(format!("ProcessInput: {e}")))?;
        }

        let raw = self.pump_output_samples()?;
        self.frame_packets(raw)
    }

    /// Notify end-of-stream, drain the access units the encoder still buffers
    /// for lookahead, and return each drained sample as its own packet.
    ///
    /// Media Foundation encoders emit with several frames of delay, so the
    /// tail of a recording only ever arrives here; each entry is one access
    /// unit and becomes one container sample.
    pub fn drain(&mut self) -> Result<Vec<EncodedPacket>, CodecError> {
        tracing::debug!("Media Foundation encoder end of stream and drain");
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                .map_err(|e| CodecError::EncodingFailed(format!("encoder end-of-stream: {e}")))?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .map_err(|e| CodecError::EncodingFailed(format!("encoder drain: {e}")))?;
        }

        let raw = self.pump_output_samples()?;
        self.frame_packets(raw)
    }

    /// Frames raw Annex B samples as the configured bitstream, first taking
    /// the parameter sets the transform has published since the last call.
    ///
    /// The decode time is the sample's `MFSampleExtension_DecodeTimestamp`. A
    /// sample without one is decoded at
    /// its presentation time, which only holds while the transform does not
    /// reorder, so presentation times must then keep increasing.
    fn frame_packets(&mut self, raw: Vec<OutputSample>) -> Result<Vec<EncodedPacket>, CodecError> {
        if self.codec_config.is_none()
            && let Some(header) = sequence_header_from_media_type(&self.transform)
        {
            self.codec_config = codec_config_from_sequence_header(&header, self.codec_type);
            self.packets.set_parameter_sets(header);
        }
        raw.into_iter()
            .map(|sample| {
                let decode_time_nanos = if let Some(decode_time) = sample.decode_time_nanos {
                    decode_time
                } else {
                    if self
                        .last_undated_presentation
                        .is_some_and(|last| sample.presentation_time <= last)
                    {
                        return Err(CodecError::EncodingFailed(
                            "the encoder reordered frames without reporting decode timestamps"
                                .into(),
                        ));
                    }
                    self.last_undated_presentation = Some(sample.presentation_time);
                    signed_nanos(sample.presentation_time)?
                };
                self.packets
                    .packet(sample.data, sample.presentation_time, decode_time_nanos)
            })
            .collect()
    }

    /// Pump every output sample the transform can currently produce, one
    /// access unit per element. Returns when the transform asks for more
    /// input.
    fn pump_output_samples(&mut self) -> Result<Vec<OutputSample>, CodecError> {
        let mut packets = Vec::new();
        unsafe {
            loop {
                let output_sample = if self.output_stream_info.dwFlags & 0x100 != 0 {
                    None
                } else {
                    Some(create_empty_sample(
                        self.output_stream_info.cbSize as usize,
                    )?)
                };

                let mut output_buffer = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: std::mem::ManuallyDrop::new(output_sample),
                    dwStatus: 0,
                    pEvents: std::mem::ManuallyDrop::new(None),
                };

                let mut status = 0u32;
                let result = self.transform.ProcessOutput(
                    0,
                    std::slice::from_mut(&mut output_buffer),
                    &raw mut status,
                );
                let output_sample = take_output_sample(&mut output_buffer);

                match result {
                    Ok(()) => {
                        if let Some(sample) = output_sample.as_ref()
                            && let Ok(data) = extract_sample_data(sample)
                        {
                            let (presentation_time, decode_time_nanos) = sample_times(sample)?;
                            tracing::debug!(
                                sample_time_100ns = presentation_time.as_nanos() / 100,
                                decode_timestamp_100ns = ?decode_time_nanos.map(|nanos| nanos / 100),
                                duration_100ns = ?sample.GetSampleDuration().ok(),
                                bytes = data.len(),
                                "Media Foundation encoder output sample"
                            );
                            packets.push(OutputSample {
                                data,
                                presentation_time,
                                decode_time_nanos,
                            });
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                    Err(e) => {
                        return Err(CodecError::EncodingFailed(format!("ProcessOutput: {e}")));
                    }
                }
            }
        }
        Ok(packets)
    }

    /// Get the codec configuration data if available.
    #[must_use]
    pub fn get_codec_config(&self) -> Option<Vec<u8>> {
        self.codec_config.clone()
    }
}

// Helper functions

fn create_video_type(
    subtype: GUID,
    width: u32,
    height: u32,
    (frame_rate_numerator, frame_rate_denominator): (u32, u32),
) -> Result<IMFMediaType, CodecError> {
    unsafe {
        let media_type: IMFMediaType = MFCreateMediaType()
            .map_err(|e| CodecError::InitializationFailed(format!("MFCreateMediaType: {e}")))?;

        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| CodecError::InitializationFailed(format!("SetGUID major: {e}")))?;

        media_type
            .SetGUID(&MF_MT_SUBTYPE, &raw const subtype)
            .map_err(|e| CodecError::InitializationFailed(format!("SetGUID subtype: {e}")))?;

        let frame_size = (u64::from(width) << 32) | u64::from(height);
        media_type
            .SetUINT64(&MF_MT_FRAME_SIZE, frame_size)
            .map_err(|e| CodecError::InitializationFailed(format!("SetUINT64 frame_size: {e}")))?;

        let frame_rate =
            (u64::from(frame_rate_numerator) << 32) | u64::from(frame_rate_denominator);
        media_type
            .SetUINT64(&MF_MT_FRAME_RATE, frame_rate)
            .map_err(|e| CodecError::InitializationFailed(format!("SetUINT64 frame_rate: {e}")))?;

        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| CodecError::InitializationFailed(format!("SetUINT32 interlace: {e}")))?;

        Ok(media_type)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaFoundationColor {
    primaries: MFVideoPrimaries,
    transfer: MFVideoTransferFunction,
    matrix: MFVideoTransferMatrix,
    range: MFNominalRange,
}

fn media_foundation_color(color: VideoColorInfo) -> Result<MediaFoundationColor, CodecError> {
    let primaries = match color.primaries {
        ColorPrimaries::Bt709 => MFVideoPrimaries_BT709,
        ColorPrimaries::Bt601 => MFVideoPrimaries_SMPTE170M,
        ColorPrimaries::Bt2020 => MFVideoPrimaries_BT2020,
        ColorPrimaries::DisplayP3 => {
            return Err(CodecError::Unsupported(
                "Media Foundation cannot represent Display P3 primaries".into(),
            ));
        }
    };
    let transfer = match color.transfer {
        TransferFunction::Sdr => MFVideoTransFunc_709,
        TransferFunction::Pq => MFVideoTransFunc_2084,
        TransferFunction::Hlg => MFVideoTransFunc_HLG,
    };
    let matrix = match color.matrix {
        MatrixCoefficients::Bt601 => MFVideoTransferMatrix_BT601,
        MatrixCoefficients::Bt709 => MFVideoTransferMatrix_BT709,
        MatrixCoefficients::Bt2020NonConstantLuminance => MFVideoTransferMatrix_BT2020_10,
        MatrixCoefficients::Bt2020ConstantLuminance => {
            return Err(CodecError::Unsupported(
                "Media Foundation cannot represent BT.2020 constant-luminance matrix coefficients"
                    .into(),
            ));
        }
    };
    let range = match color.range {
        ColorRange::Full => MFNominalRange_0_255,
        ColorRange::Limited => MFNominalRange_16_235,
    };
    if color.dolby_vision {
        return Err(CodecError::Unsupported(
            "Media Foundation cannot signal Dolby Vision metadata".into(),
        ));
    }
    Ok(MediaFoundationColor {
        primaries,
        transfer,
        matrix,
        range,
    })
}

fn set_color_metadata(media_type: &IMFMediaType, color: VideoColorInfo) -> Result<(), CodecError> {
    let color = media_foundation_color(color)?;
    let to_u32 = |value: i32| {
        u32::try_from(value).map_err(|_| {
            CodecError::InitializationFailed("negative Media Foundation color codepoint".into())
        })
    };
    unsafe {
        media_type
            .SetUINT32(&MF_MT_VIDEO_PRIMARIES, to_u32(color.primaries.0)?)
            .map_err(|error| {
                CodecError::InitializationFailed(format!(
                    "SetUINT32 video primaries failed: {error}"
                ))
            })?;
        media_type
            .SetUINT32(&MF_MT_TRANSFER_FUNCTION, to_u32(color.transfer.0)?)
            .map_err(|error| {
                CodecError::InitializationFailed(format!(
                    "SetUINT32 transfer function failed: {error}"
                ))
            })?;
        media_type
            .SetUINT32(&MF_MT_YUV_MATRIX, to_u32(color.matrix.0)?)
            .map_err(|error| {
                CodecError::InitializationFailed(format!("SetUINT32 YUV matrix failed: {error}"))
            })?;
        media_type
            .SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, to_u32(color.range.0)?)
            .map_err(|error| {
                CodecError::InitializationFailed(format!("SetUINT32 nominal range failed: {error}"))
            })?;
    }
    Ok(())
}

const fn create_register_type_info(
    subtype: GUID,
) -> windows::Win32::Media::MediaFoundation::MFT_REGISTER_TYPE_INFO {
    windows::Win32::Media::MediaFoundation::MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    }
}

const fn output_subtype(layout: DecodedPixelLayout) -> GUID {
    match layout {
        DecodedPixelLayout::Nv12 => MFVideoFormat_NV12,
        DecodedPixelLayout::P010 => MFVideoFormat_P010,
    }
}

fn activate_hardware_transform(
    category: GUID,
    input_type: windows::Win32::Media::MediaFoundation::MFT_REGISTER_TYPE_INFO,
    output_type: windows::Win32::Media::MediaFoundation::MFT_REGISTER_TYPE_INFO,
    kind: &str,
) -> Result<IMFTransform, CodecError> {
    unsafe {
        let mut count = 0u32;
        let mut activates = ptr::null_mut();
        MFTEnumEx(
            category,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER | MFT_ENUM_FLAG_SYNCMFT,
            Some(&raw const input_type),
            Some(&raw const output_type),
            &raw mut activates,
            &raw mut count,
        )
        .map_err(|error| {
            CodecError::InitializationFailed(format!("MFTEnumEx {kind} discovery failed: {error}"))
        })?;

        if count == 0 {
            if !activates.is_null() {
                CoTaskMemFree(Some(activates.cast::<c_void>()));
            }
            return Err(CodecError::Unsupported(format!(
                "no hardware {kind} is available for the requested formats"
            )));
        }
        if activates.is_null() {
            return Err(CodecError::InitializationFailed(format!(
                "MFTEnumEx returned {count} {kind} activations through a null array"
            )));
        }

        let slots = std::slice::from_raw_parts_mut(activates, count as usize);
        let activation_objects = slots
            .iter_mut()
            .filter_map(Option::take)
            .collect::<Vec<_>>();
        CoTaskMemFree(Some(activates.cast::<c_void>()));

        let activate = activation_objects.into_iter().next().ok_or_else(|| {
            CodecError::InitializationFailed(format!(
                "MFTEnumEx returned no usable {kind} activation"
            ))
        })?;
        activate.ActivateObject().map_err(|error| {
            CodecError::InitializationFailed(format!("hardware {kind} activation failed: {error}"))
        })
    }
}

/// Read the codec configuration the encoder publishes on its output media
/// type: `MF_MT_MPEG_SEQUENCE_HEADER` carries the Annex-B parameter sets
/// (SPS/PPS for H.264, plus VPS for H.265), which convert to the avcC/hvcC
/// record the container stores. `None` while the transform has not exposed
/// the attribute yet.
/// The Annex B parameter sets the transform publishes on its output type.
fn sequence_header_from_media_type(transform: &IMFTransform) -> Option<Vec<u8>> {
    unsafe {
        let media_type = transform.GetOutputCurrentType(0).ok()?;
        let size = media_type.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
        let mut annex_b = vec![0u8; usize::try_from(size).ok()?];
        media_type
            .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut annex_b, None)
            .ok()?;
        Some(annex_b)
    }
}

/// The `avcC`/`hvcC` record built from Annex B parameter sets.
fn codec_config_from_sequence_header(annex_b: &[u8], codec_type: CodecType) -> Option<Vec<u8>> {
    match codec_type {
        CodecType::H264 => build_h264_avcc_from_annex_b(annex_b),
        CodecType::H265 => build_h265_hvcc_from_annex_b(annex_b),
    }
}

fn take_output_sample(output: &mut MFT_OUTPUT_DATA_BUFFER) -> Option<IMFSample> {
    unsafe {
        let sample = std::mem::ManuallyDrop::take(&mut output.pSample);
        drop(std::mem::ManuallyDrop::take(&mut output.pEvents));
        sample
    }
}

fn create_sample(data: &[u8]) -> Result<IMFSample, CodecError> {
    unsafe {
        let sample: IMFSample = MFCreateSample()
            .map_err(|e| CodecError::InitializationFailed(format!("MFCreateSample: {e}")))?;

        let len = u32::try_from(data.len())
            .map_err(|_| CodecError::EncodingFailed("data too large for u32".into()))?;

        let buffer = MFCreateMemoryBuffer(len)
            .map_err(|e| CodecError::InitializationFailed(format!("MFCreateMemoryBuffer: {e}")))?;

        let mut buffer_ptr = ptr::null_mut();
        buffer
            .Lock(&raw mut buffer_ptr, None, None)
            .map_err(|e| CodecError::InitializationFailed(format!("Buffer Lock: {e}")))?;

        ptr::copy_nonoverlapping(data.as_ptr(), buffer_ptr, data.len());

        buffer
            .SetCurrentLength(len)
            .map_err(|e| CodecError::InitializationFailed(format!("SetCurrentLength: {e}")))?;

        buffer
            .Unlock()
            .map_err(|e| CodecError::InitializationFailed(format!("Buffer Unlock: {e}")))?;

        sample
            .AddBuffer(&buffer)
            .map_err(|e| CodecError::InitializationFailed(format!("AddBuffer: {e}")))?;

        Ok(sample)
    }
}

fn create_empty_sample(size: usize) -> Result<IMFSample, CodecError> {
    unsafe {
        let sample: IMFSample = MFCreateSample()
            .map_err(|e| CodecError::InitializationFailed(format!("MFCreateSample: {e}")))?;

        let len = u32::try_from(size)
            .map_err(|_| CodecError::EncodingFailed("size too large for u32".into()))?;

        let buffer = MFCreateMemoryBuffer(len)
            .map_err(|e| CodecError::InitializationFailed(format!("MFCreateMemoryBuffer: {e}")))?;

        sample
            .AddBuffer(&buffer)
            .map_err(|e| CodecError::InitializationFailed(format!("AddBuffer: {e}")))?;

        Ok(sample)
    }
}

#[allow(clippy::cast_sign_loss)]
fn extract_biplanar_frame(
    sample: &IMFSample,
    width: u32,
    height: u32,
    layout: DecodedPixelLayout,
) -> Result<WindowsFrame, CodecError> {
    unsafe {
        let buffer = sample
            .GetBufferByIndex(0)
            .map_err(|e| CodecError::DecodingFailed(format!("GetBufferByIndex: {e}")))?;

        let expected_size = layout.packed_len(width, height);
        let data = if let Ok(buffer_2d) = buffer.cast::<IMF2DBuffer>() {
            copy_2d_biplanar_buffer(&buffer_2d, width, height, layout)?
        } else {
            let mut buffer_ptr = ptr::null_mut();
            let mut current_len = 0u32;
            buffer
                .Lock(&raw mut buffer_ptr, None, Some(&raw mut current_len))
                .map_err(|error| CodecError::DecodingFailed(format!("Lock: {error}")))?;
            let current_len = current_len as usize;
            if current_len != expected_size {
                buffer.Unlock().map_err(|error| {
                    CodecError::DecodingFailed(format!("Unlock after size mismatch: {error}"))
                })?;
                return Err(CodecError::DecodingFailed(format!(
                    "Media Foundation output has {current_len} bytes; expected {expected_size} for {width}x{height} {layout:?}"
                )));
            }
            let mut data = vec![0_u8; expected_size];
            ptr::copy_nonoverlapping(buffer_ptr, data.as_mut_ptr(), expected_size);
            buffer
                .Unlock()
                .map_err(|error| CodecError::DecodingFailed(format!("Unlock: {error}")))?;
            data
        };

        let sample_time = sample.GetSampleTime().map_err(|error| {
            CodecError::DecodingFailed(format!("decoded sample has no PTS: {error}"))
        })?;
        let timestamp_ns = u64::try_from(sample_time)
            .map_err(|_| {
                CodecError::DecodingFailed("decoded sample returned a negative PTS".into())
            })?
            .saturating_mul(100);

        Ok(WindowsFrame {
            data,
            width,
            height,
            timestamp_ns,
            layout,
        })
    }
}

fn copy_2d_biplanar_buffer(
    buffer: &IMF2DBuffer,
    width: u32,
    height: u32,
    layout: DecodedPixelLayout,
) -> Result<Vec<u8>, CodecError> {
    unsafe {
        let mut scanline = ptr::null_mut();
        let mut pitch = 0_i32;
        buffer
            .Lock2D(&raw mut scanline, &raw mut pitch)
            .map_err(|error| CodecError::DecodingFailed(format!("Lock2D failed: {error}")))?;

        let result = (|| {
            if scanline.is_null() {
                return Err(CodecError::DecodingFailed(
                    "Lock2D returned a null scanline".into(),
                ));
            }
            let pitch = usize::try_from(pitch).map_err(|_| {
                CodecError::DecodingFailed(format!(
                    "Media Foundation returned negative YUV pitch {pitch}"
                ))
            })?;
            let row_bytes = layout.bytes_per_row(width);
            if pitch < row_bytes {
                return Err(CodecError::DecodingFailed(format!(
                    "Media Foundation YUV pitch {pitch} is smaller than row size {row_bytes}"
                )));
            }

            let y_rows = height as usize;
            let uv_rows = y_rows / 2;
            let y_size = row_bytes * y_rows;
            let mut data = vec![0_u8; layout.packed_len(width, height)];
            for row in 0..y_rows {
                ptr::copy_nonoverlapping(
                    scanline.add(row * pitch),
                    data.as_mut_ptr().add(row * row_bytes),
                    row_bytes,
                );
            }
            let uv_scanline = scanline.add(pitch * y_rows);
            for row in 0..uv_rows {
                ptr::copy_nonoverlapping(
                    uv_scanline.add(row * pitch),
                    data.as_mut_ptr().add(y_size + row * row_bytes),
                    row_bytes,
                );
            }
            Ok(data)
        })();

        buffer
            .Unlock2D()
            .map_err(|error| CodecError::DecodingFailed(format!("Unlock2D failed: {error}")))?;
        result
    }
}

/// The presentation time of an encoder output sample and, when the transform
/// stamped one, its decode time in nanoseconds.
///
/// Both are 100 ns units; the decode timestamp is a signed value stored in an
/// unsigned attribute.
fn sample_times(sample: &IMFSample) -> Result<(Duration, Option<i64>), CodecError> {
    let missing = |what: &str, error: windows::core::Error| {
        CodecError::EncodingFailed(format!("encoded sample {what}: {error}"))
    };
    // SAFETY: `sample` is a live output sample.
    let sample_time = unsafe { sample.GetSampleTime() }.map_err(|e| missing("has no time", e))?;
    let presentation_time = u64::try_from(sample_time)
        .map(|hns| Duration::from_nanos(hns * 100))
        .map_err(|_| CodecError::EncodingFailed("encoded sample has a negative time".into()))?;
    // SAFETY: as above; the key is a valid attribute GUID.
    let decode_time = match unsafe { sample.GetUINT64(&MFSampleExtension_DecodeTimestamp) } {
        Ok(hns) => Some(hns.cast_signed().checked_mul(100).ok_or_else(|| {
            CodecError::EncodingFailed("encoded sample decode timestamp overflows".into())
        })?),
        Err(error) if error.code() == MF_E_ATTRIBUTENOTFOUND => None,
        Err(error) => return Err(missing("has an unreadable decode timestamp", error)),
    };
    Ok((presentation_time, decode_time))
}

fn extract_sample_data(sample: &IMFSample) -> Result<Vec<u8>, CodecError> {
    unsafe {
        let buffer = sample
            .GetBufferByIndex(0)
            .map_err(|e| CodecError::EncodingFailed(format!("GetBufferByIndex: {e}")))?;

        let mut buffer_ptr = ptr::null_mut();
        let mut current_len = 0u32;

        buffer
            .Lock(&raw mut buffer_ptr, None, Some(&raw mut current_len))
            .map_err(|e| CodecError::EncodingFailed(format!("Lock: {e}")))?;

        let mut data = vec![0u8; current_len as usize];
        ptr::copy_nonoverlapping(buffer_ptr, data.as_mut_ptr(), current_len as usize);

        buffer
            .Unlock()
            .map_err(|e| CodecError::EncodingFailed(format!("Unlock: {e}")))?;

        Ok(data)
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
    WindowsDecoder::new(codec, config, width, height).map(crate::DecoderInner::Windows)
}

/// Open a hardware encoder for `config`.
pub fn open_encoder(config: &crate::EncoderConfig) -> Result<crate::EncoderInner, CodecError> {
    WindowsEncoder::new(config).map(crate::EncoderInner::Windows)
}

#[cfg(test)]
mod color_tests {
    use super::*;

    fn bt709() -> VideoColorInfo {
        VideoColorInfo {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferFunction::Sdr,
            matrix: MatrixCoefficients::Bt709,
            range: ColorRange::Limited,
            content_light_level: None,
            dolby_vision: false,
        }
    }

    #[test]
    fn media_foundation_color_maps_bt709_sdr_limited() {
        assert_eq!(
            media_foundation_color(bt709()).expect("BT.709 is supported"),
            MediaFoundationColor {
                primaries: MFVideoPrimaries_BT709,
                transfer: MFVideoTransFunc_709,
                matrix: MFVideoTransferMatrix_BT709,
                range: MFNominalRange_16_235,
            }
        );
    }

    #[test]
    fn media_foundation_color_rejects_unsupported_colorimetry() {
        let mut color = bt709();
        color.primaries = ColorPrimaries::DisplayP3;
        assert!(media_foundation_color(color).is_err());
        color = bt709();
        color.matrix = MatrixCoefficients::Bt2020ConstantLuminance;
        assert!(media_foundation_color(color).is_err());
    }
}
