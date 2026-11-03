//! Apple `VideoToolbox` hardware encoding.

use crate::{
    Bitrate, BitstreamFormat, CodecError, EncodedPacket, EncoderCodec, EncoderConfig,
    EncoderProfile, FrameOptions, FrameRate, H264Level, H264Profile,
    annexb::{START_CODE, length_prefixed_to_annex_b},
};
use mediax_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorInfo,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    CMSampleBuffer, CMTime, CMTimeFlags, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex,
    kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms, kCMSampleAttachmentKey_NotSync,
    kCMVideoCodecType_H264, kCMVideoCodecType_HEVC,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVImageBufferColorPrimaries_ITU_R_709_2,
    kCVImageBufferColorPrimaries_ITU_R_2020, kCVImageBufferColorPrimaries_P3_D65,
    kCVImageBufferColorPrimaries_SMPTE_C, kCVImageBufferTransferFunction_ITU_R_709_2,
    kCVImageBufferTransferFunction_ITU_R_2100_HLG, kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ,
    kCVImageBufferYCbCrMatrix_ITU_R_601_4, kCVImageBufferYCbCrMatrix_ITU_R_709_2,
    kCVImageBufferYCbCrMatrix_ITU_R_2020, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
    kVTCompressionPropertyKey_YCbCrMatrix, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTProfileLevel_HEVC_Main_AutoLevel, kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
};
use std::ffi::c_void;
use std::fmt;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// What the `VideoToolbox` output callback hands back to the encoder.
enum Output {
    /// One finished access unit.
    Packet(EncodedPacket),
    /// The `avcC`/`hvcC` record from the stream's format description.
    CodecConfig(Vec<u8>),
    /// The session reported a failure, or the sample could not be framed.
    Failed(CodecError),
}

/// State the output callback reads; lives at a stable address for the session.
struct CallbackContext {
    output: Sender<Output>,
    bitstream: BitstreamFormat,
    is_hevc: bool,
    /// The H.264 `level_idc` the SPS must not exceed.
    max_level_idc: Option<u8>,
    /// Whether the codec configuration has been sent already.
    config_sent: AtomicBool,
}

/// Apple `VideoToolbox` hardware encoder.
pub struct AppleEncoder {
    session: CFRetained<VTCompressionSession>,
    /// Kept alive and in place for the session's callback.
    _context: Box<CallbackContext>,
    output: Receiver<Output>,
    width: u32,
    height: u32,
    frame_rate: FrameRate,
    frame_index: i64,
    color: VideoColorInfo,
    codec_config: Option<Vec<u8>>,
}

impl fmt::Debug for AppleEncoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppleEncoder")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("frame_index", &self.frame_index)
            .finish_non_exhaustive()
    }
}

impl Drop for AppleEncoder {
    fn drop(&mut self) {
        // SAFETY: the session is live; invalidating stops the callback before
        // `_context` is freed.
        unsafe { self.session.invalidate() };
    }
}

fn init_error(operation: &str, status: i32) -> CodecError {
    CodecError::InitializationFailed(format!("{operation} failed: OSStatus {status}"))
}

fn set_property(
    session: &VTCompressionSession,
    key: &CFString,
    value: &CFType,
) -> Result<(), CodecError> {
    // SAFETY: `VTSession` is `CFType`, and a compression session is a session.
    let status = unsafe { VTSessionSetProperty(session.as_ref(), key, Some(value)) };
    if status == 0 {
        Ok(())
    } else {
        Err(CodecError::InitializationFailed(format!(
            "VTSessionSetProperty {key} failed: OSStatus {status}"
        )))
    }
}

fn set_bool(session: &VTCompressionSession, key: &CFString, value: bool) -> Result<(), CodecError> {
    set_property(session, key, CFBoolean::new(value).as_ref())
}

fn set_i32(session: &VTCompressionSession, key: &CFString, value: i32) -> Result<(), CodecError> {
    set_property(session, key, CFNumber::new_i32(value).as_ref())
}

fn h264_profile_level(profile: H264Profile, level: H264Level) -> &'static CFString {
    use objc2_video_toolbox as vt;
    // SAFETY: these are constant strings exported by `VideoToolbox`.
    unsafe {
        match (profile, level) {
            // VideoToolbox only offers the constrained form with an automatic
            // level (macOS 12, iOS 15, tvOS 15, visionOS 1 and later); the
            // requested level is enforced as a ceiling on the emitted SPS.
            (H264Profile::ConstrainedBaseline, _) => {
                vt::kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel
            }
            (H264Profile::Main, H264Level::L3_0) => vt::kVTProfileLevel_H264_Main_3_0,
            (H264Profile::Main, H264Level::L3_1) => vt::kVTProfileLevel_H264_Main_3_1,
            (H264Profile::Main, H264Level::L3_2) => vt::kVTProfileLevel_H264_Main_3_2,
            (H264Profile::Main, H264Level::L4_0) => vt::kVTProfileLevel_H264_Main_4_0,
            (H264Profile::Main, H264Level::L4_1) => vt::kVTProfileLevel_H264_Main_4_1,
            (H264Profile::Main, H264Level::L4_2) => vt::kVTProfileLevel_H264_Main_4_2,
            (H264Profile::Main, H264Level::L5_0) => vt::kVTProfileLevel_H264_Main_5_0,
            (H264Profile::Main, H264Level::L5_1) => vt::kVTProfileLevel_H264_Main_5_1,
            (H264Profile::Main, H264Level::L5_2) => vt::kVTProfileLevel_H264_Main_5_2,
            (H264Profile::High, H264Level::L3_0) => vt::kVTProfileLevel_H264_High_3_0,
            (H264Profile::High, H264Level::L3_1) => vt::kVTProfileLevel_H264_High_3_1,
            (H264Profile::High, H264Level::L3_2) => vt::kVTProfileLevel_H264_High_3_2,
            (H264Profile::High, H264Level::L4_0) => vt::kVTProfileLevel_H264_High_4_0,
            (H264Profile::High, H264Level::L4_1) => vt::kVTProfileLevel_H264_High_4_1,
            (H264Profile::High, H264Level::L4_2) => vt::kVTProfileLevel_H264_High_4_2,
            (H264Profile::High, H264Level::L5_0) => vt::kVTProfileLevel_H264_High_5_0,
            (H264Profile::High, H264Level::L5_1) => vt::kVTProfileLevel_H264_High_5_1,
            (H264Profile::High, H264Level::L5_2) => vt::kVTProfileLevel_H264_High_5_2,
        }
    }
}

const fn cm_time(value: i64, timescale: i32) -> CMTime {
    CMTime {
        value,
        timescale,
        flags: CMTimeFlags(1),
        epoch: 0,
    }
}

impl AppleEncoder {
    /// Create a `VideoToolbox` compression session for `config`.
    ///
    /// # Errors
    ///
    /// Returns `CodecError::InitializationFailed` if session creation or one
    /// of the session properties fails, and `CodecError::Unsupported` for
    /// colorimetry or frame rates `VideoToolbox` cannot represent.
    pub fn new(config: &EncoderConfig) -> Result<Self, CodecError> {
        let (codec_type, bitstream, is_hevc) = match config.codec {
            EncoderCodec::H264(settings) => (kCMVideoCodecType_H264, settings.bitstream, false),
            EncoderCodec::H265(settings) => (kCMVideoCodecType_HEVC, settings.bitstream, true),
            EncoderCodec::Av1 => unreachable!("AV1 is encoded in software"),
        };
        // Presentation times use the numerator as their timescale.
        if i32::try_from(config.frame_rate.numerator().get()).is_err() {
            return Err(CodecError::Unsupported(
                "frame rate numerator exceeds i32::MAX".into(),
            ));
        }
        let width = i32::try_from(config.width.get())
            .map_err(|_| CodecError::Unsupported("frame width exceeds i32::MAX".into()))?;
        let height = i32::try_from(config.height.get())
            .map_err(|_| CodecError::Unsupported("frame height exceeds i32::MAX".into()))?;
        let realtime = config.profile == EncoderProfile::Realtime;

        let (sender, output) = channel();
        let context = Box::new(CallbackContext {
            output: sender,
            bitstream,
            is_hevc,
            max_level_idc: match config.codec {
                EncoderCodec::H264(settings) => Some(settings.level.idc()),
                _ => None,
            },
            config_sent: AtomicBool::new(false),
        });

        // The low-latency rate controller is chosen when the session is
        // created, through the encoder specification.
        let specification = realtime.then(|| {
            // SAFETY: a constant key exported by `VideoToolbox`.
            let key = unsafe { kVTVideoEncoderSpecification_EnableLowLatencyRateControl };
            CFDictionary::<CFString, CFType>::from_slices(&[key], &[CFBoolean::new(true).as_ref()])
        });

        let mut session_ptr: *mut VTCompressionSession = ptr::null_mut();
        // SAFETY: the callback context outlives the session (it is boxed in
        // the encoder, whose `Drop` invalidates the session first), and
        // `session_ptr` is a valid out pointer.
        let status = unsafe {
            VTCompressionSession::create(
                None,
                width,
                height,
                codec_type,
                specification.as_deref().map(AsRef::as_ref),
                None,
                None,
                Some(encode_callback),
                ptr::from_ref::<CallbackContext>(&context)
                    .cast_mut()
                    .cast::<c_void>(),
                NonNull::from(&mut session_ptr),
            )
        };
        if status != 0 {
            return Err(init_error("VTCompressionSessionCreate", status));
        }
        let session = NonNull::new(session_ptr)
            .ok_or_else(|| init_error("VTCompressionSessionCreate", status))?;
        // SAFETY: `create` returns a +1 retained session.
        let session = unsafe { CFRetained::from_raw(session) };

        configure_session(&session, config)?;
        // SAFETY: the session is live and fully configured.
        let status = unsafe { session.prepare_to_encode_frames() };
        if status != 0 {
            return Err(init_error(
                "VTCompressionSessionPrepareToEncodeFrames",
                status,
            ));
        }

        Ok(Self {
            session,
            _context: context,
            output,
            width: config.width.get(),
            height: config.height.get(),
            frame_rate: config.frame_rate,
            frame_index: 0,
            color: config.color,
            codec_config: None,
        })
    }

    /// Set the average bitrate; `VideoToolbox` applies it from the next frame.
    ///
    /// # Errors
    ///
    /// Returns `CodecError::Unsupported` for rates above `i32::MAX` bits per
    /// second and `CodecError::EncodingFailed` if the session rejects it.
    pub fn set_bitrate(&self, bitrate: Bitrate) -> Result<(), CodecError> {
        // SAFETY: a constant key exported by `VideoToolbox`.
        let key = unsafe { kVTCompressionPropertyKey_AverageBitRate };
        set_i32(&self.session, key, bitrate_property(bitrate)?)
            .map_err(|error| CodecError::EncodingFailed(error.to_string()))
    }

    /// Get the codec configuration data (the `avcC` or `hvcC` record), known
    /// once the first frame has been encoded.
    #[must_use]
    pub fn get_codec_config(&self) -> Option<Vec<u8>> {
        self.codec_config.clone()
    }

    /// Encode NV12 data.
    ///
    /// # Errors
    ///
    /// Returns `CodecError::EncodingFailed` if `nv12` has the wrong size or
    /// `VideoToolbox` fails.
    pub fn encode_nv12(
        &mut self,
        nv12: &[u8],
        options: FrameOptions,
    ) -> Result<Vec<EncodedPacket>, CodecError> {
        let width = self.width as usize;
        let height = self.height as usize;
        let y_size = width * height;
        let expected_size = y_size + y_size / 2;
        if nv12.len() != expected_size {
            return Err(CodecError::EncodingFailed(format!(
                "NV12 data size {} doesn't match expected {expected_size} for {width}x{height}",
                nv12.len(),
            )));
        }

        // An IOSurface-backed buffer lets the media engine read it directly.
        // SAFETY: a constant key exported by `CoreVideo`.
        let surface_key = unsafe { kCVPixelBufferIOSurfacePropertiesKey };
        let attributes = CFDictionary::<CFString, CFType>::from_slices(
            &[surface_key],
            &[CFDictionary::<CFString, CFType>::empty().as_ref()],
        );
        let mut pixel_buffer_ptr: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: `pixel_buffer_ptr` is a valid out pointer.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width,
                height,
                match self.color.range {
                    ColorRange::Full => kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                    ColorRange::Limited => kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                },
                Some(attributes.as_ref()),
                NonNull::from(&mut pixel_buffer_ptr),
            )
        };
        let pixel_buffer = NonNull::new(pixel_buffer_ptr)
            .filter(|_| status == 0)
            .ok_or_else(|| {
                CodecError::EncodingFailed(format!("CVPixelBufferCreate failed: {status}"))
            })?;
        // SAFETY: `CVPixelBufferCreate` returns a +1 retained buffer.
        let pixel_buffer = unsafe { CFRetained::from_raw(pixel_buffer) };

        // SAFETY: the buffer is locked for the copy, both planes are at least
        // `bytes_per_row * rows` bytes, and the source is `expected_size` long.
        unsafe {
            let flags = CVPixelBufferLockFlags(0);
            let lock_status = CVPixelBufferLockBaseAddress(&pixel_buffer, flags);
            if lock_status != 0 {
                return Err(CodecError::EncodingFailed(format!(
                    "CVPixelBufferLockBaseAddress failed: {lock_status}"
                )));
            }
            for (plane, source, rows) in [
                (0, &nv12[..y_size], height),
                (1, &nv12[y_size..], height / 2),
            ] {
                let base = CVPixelBufferGetBaseAddressOfPlane(&pixel_buffer, plane).cast::<u8>();
                let stride = CVPixelBufferGetBytesPerRowOfPlane(&pixel_buffer, plane);
                for (row, line) in source.chunks_exact(width).take(rows).enumerate() {
                    ptr::copy_nonoverlapping(line.as_ptr(), base.add(row * stride), width);
                }
            }
            CVPixelBufferUnlockBaseAddress(&pixel_buffer, flags);
        }

        self.submit(&pixel_buffer, options)
    }

    /// Encode a BGRA or NV12 pixel buffer.
    ///
    /// # Errors
    ///
    /// Returns `CodecError::Unsupported` for other pixel formats or a size
    /// that differs from the configured one, and `CodecError::EncodingFailed`
    /// if `VideoToolbox` fails.
    pub fn encode_pixel_buffer(
        &mut self,
        buffer: &CVPixelBuffer,
        options: FrameOptions,
    ) -> Result<Vec<EncodedPacket>, CodecError> {
        let (format, width, height) = (
            CVPixelBufferGetPixelFormatType(buffer),
            CVPixelBufferGetWidth(buffer),
            CVPixelBufferGetHeight(buffer),
        );
        if ![
            kCVPixelFormatType_32BGRA,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        ]
        .contains(&format)
        {
            return Err(CodecError::Unsupported(format!(
                "pixel buffer format {format:#010x} is neither BGRA nor 8-bit NV12"
            )));
        }
        if width != self.width as usize || height != self.height as usize {
            return Err(CodecError::Unsupported(format!(
                "pixel buffer is {width}x{height}, the encoder is {}x{}",
                self.width, self.height
            )));
        }
        self.submit(buffer, options)
    }

    /// Submit one pixel buffer and collect what the session finishes for it.
    fn submit(
        &mut self,
        buffer: &CVPixelBuffer,
        options: FrameOptions,
    ) -> Result<Vec<EncodedPacket>, CodecError> {
        let numerator = i32::try_from(self.frame_rate.numerator().get())
            .expect("the numerator was checked against i32::MAX at construction");
        let denominator = i64::from(self.frame_rate.denominator().get());
        let presentation_time = cm_time(self.frame_index * denominator, numerator);
        let duration = cm_time(denominator, numerator);
        self.frame_index += 1;

        let frame_properties = options.keyframe.then(|| {
            // SAFETY: a constant key exported by `VideoToolbox`.
            let key = unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame };
            CFDictionary::<CFString, CFType>::from_slices(&[key], &[CFBoolean::new(true).as_ref()])
        });

        let mut info_flags = VTEncodeInfoFlags(0);
        // SAFETY: `buffer` and the session are live, and `info_flags` is a
        // valid out pointer.
        let status = unsafe {
            self.session.encode_frame(
                buffer,
                presentation_time,
                duration,
                frame_properties.as_deref().map(AsRef::as_ref),
                ptr::null_mut(),
                &raw mut info_flags,
            )
        };
        if status != 0 {
            return Err(CodecError::EncodingFailed(format!(
                "VTCompressionSessionEncodeFrame failed: OSStatus {status}"
            )));
        }
        // A non-numeric time completes every pending frame, so the packet for
        // this frame is out before the call returns.
        // SAFETY: the session is live.
        let status = unsafe { self.session.complete_frames(cm_time(0, 0)) };
        if status != 0 {
            return Err(CodecError::EncodingFailed(format!(
                "VTCompressionSessionCompleteFrames failed: OSStatus {status}"
            )));
        }

        let mut packets = Vec::new();
        while let Ok(output) = self.output.try_recv() {
            match output {
                Output::Packet(packet) => packets.push(packet),
                Output::CodecConfig(config) => self.codec_config = Some(config),
                Output::Failed(error) => return Err(error),
            }
        }
        Ok(packets)
    }
}

/// Sets the session's colour metadata, rate, keyframe and latency properties.
fn configure_session(
    session: &VTCompressionSession,
    config: &EncoderConfig,
) -> Result<(), CodecError> {
    let realtime = config.profile == EncoderProfile::Realtime;
    set_video_toolbox_color(session, config.color)?;
    // SAFETY: constant keys exported by `VideoToolbox`.
    unsafe {
        set_i32(
            session,
            kVTCompressionPropertyKey_AverageBitRate,
            bitrate_property(config.bitrate)?,
        )?;
        set_i32(
            session,
            kVTCompressionPropertyKey_ExpectedFrameRate,
            i32::try_from(config.frame_rate.rounded_fps())
                .map_err(|_| CodecError::Unsupported("frame rate exceeds i32::MAX".into()))?,
        )?;
        let interval = config.keyframe_interval.map_or(i32::MAX, |interval| {
            i32::try_from(config.frame_rate.frames_in(interval)).unwrap_or(i32::MAX)
        });
        set_i32(
            session,
            kVTCompressionPropertyKey_MaxKeyFrameInterval,
            interval,
        )?;
        if realtime {
            set_bool(session, kVTCompressionPropertyKey_RealTime, true)?;
            set_bool(
                session,
                kVTCompressionPropertyKey_AllowFrameReordering,
                false,
            )?;
        }
        let profile_level = match config.codec {
            EncoderCodec::H264(settings) => h264_profile_level(settings.profile, settings.level),
            EncoderCodec::H265(_) => kVTProfileLevel_HEVC_Main_AutoLevel,
            EncoderCodec::Av1 => unreachable!("AV1 is encoded in software"),
        };
        set_property(
            session,
            kVTCompressionPropertyKey_ProfileLevel,
            profile_level.as_ref(),
        )?;
    }
    Ok(())
}

fn bitrate_property(bitrate: Bitrate) -> Result<i32, CodecError> {
    i32::try_from(bitrate.bits_per_second().get()).map_err(|_| {
        CodecError::Unsupported("VideoToolbox takes bitrates up to i32::MAX bits per second".into())
    })
}

/// `VideoToolbox` output callback: frames the sample and queues it.
unsafe extern "C-unwind" fn encode_callback(
    output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: i32,
    _info_flags: VTEncodeInfoFlags,
    sample_buffer: *mut CMSampleBuffer,
) {
    // SAFETY: the context is boxed in the encoder, which outlives the session.
    let context = unsafe { &*output_callback_ref_con.cast::<CallbackContext>() };
    let results = if status != 0 {
        vec![Output::Failed(CodecError::EncodingFailed(format!(
            "VideoToolbox reported OSStatus {status}"
        )))]
    } else if let Some(sample) = NonNull::new(sample_buffer) {
        // SAFETY: VideoToolbox passes a live sample buffer to the callback.
        match unsafe { frame_sample(context, sample.as_ref()) } {
            Ok(results) => results,
            Err(error) => vec![Output::Failed(error)],
        }
    } else {
        // A dropped frame has no sample buffer.
        return;
    };
    for result in results {
        // The receiver only goes away with the encoder.
        let _ = context.output.send(result);
    }
}

/// Frames one sample as an [`EncodedPacket`] and, for the first one, the
/// codec configuration record.
///
/// # Safety
///
/// `sample` must be a live sample buffer from the compression session.
unsafe fn frame_sample(
    context: &CallbackContext,
    sample: &CMSampleBuffer,
) -> Result<Vec<Output>, CodecError> {
    let failed = |what: &str| CodecError::EncodingFailed(format!("VideoToolbox sample: {what}"));
    let mut results = Vec::new();

    // SAFETY: caller contract; the accessors are read-only.
    let (data_buffer, description, attachments) = unsafe {
        (
            sample
                .data_buffer()
                .ok_or_else(|| failed("no data buffer"))?,
            sample
                .format_description()
                .ok_or_else(|| failed("no format description"))?,
            sample.sample_attachments_array(false),
        )
    };
    // A sample is a sync sample unless VideoToolbox marks it `NotSync`.
    let keyframe = attachments.is_none_or(|attachments| {
        // SAFETY: sample attachments are an array of dictionaries.
        let attachments = unsafe {
            CFRetained::cast_unchecked::<CFArray<CFDictionary<CFString, CFType>>>(attachments)
        };
        attachments.get(0).is_none_or(|attachment| {
            // SAFETY: a constant key exported by `CoreMedia`.
            !attachment.contains_key(unsafe { kCMSampleAttachmentKey_NotSync })
        })
    });

    // SAFETY: the data buffer is live and `sample_bytes` is `length` long.
    let mut sample_bytes = vec![0_u8; unsafe { data_buffer.data_length() }];
    if let Some(destination) = NonNull::new(sample_bytes.as_mut_ptr().cast::<c_void>()) {
        // SAFETY: `destination` points at `sample_bytes.len()` writable bytes.
        let status = unsafe { data_buffer.copy_data_bytes(0, sample_bytes.len(), destination) };
        if status != 0 {
            return Err(failed("CMBlockBufferCopyDataBytes failed"));
        }
    }

    let (parameter_sets, nal_length_size) = parameter_sets(&description, context.is_hevc)?;

    if !context.config_sent.swap(true, Ordering::Relaxed) {
        // An automatic level (Constrained Baseline) may pick above the request.
        if let Some(max) = context.max_level_idc
            && let Some(level) = parameter_sets.first().and_then(|sps| sps.get(3))
            && *level > max
        {
            return Err(failed(&format!(
                "the encoder chose level_idc {level}, above the requested {max}"
            )));
        }
        results.push(Output::CodecConfig(codec_config(
            &description,
            context.is_hevc,
        )?));
    }

    let data = match context.bitstream {
        BitstreamFormat::LengthPrefixed => sample_bytes,
        BitstreamFormat::AnnexB => {
            let mut data = Vec::with_capacity(sample_bytes.len() + 128);
            if keyframe {
                for set in &parameter_sets {
                    data.extend_from_slice(&START_CODE);
                    data.extend_from_slice(set);
                }
            }
            data.extend_from_slice(
                &length_prefixed_to_annex_b(&sample_bytes, nal_length_size).map_err(failed)?,
            );
            data
        }
    };
    results.push(Output::Packet(EncodedPacket { data, keyframe }));
    Ok(results)
}

/// The parameter sets (VPS/SPS/PPS) of a format description and the width of
/// its NAL length fields.
fn parameter_sets(
    description: &objc2_core_media::CMFormatDescription,
    is_hevc: bool,
) -> Result<(Vec<Vec<u8>>, usize), CodecError> {
    let mut sets = Vec::new();
    let mut count = 1;
    let mut header_length = 0;
    let mut index = 0;
    while index < count {
        let mut pointer: *const u8 = ptr::null();
        let mut size = 0;
        // SAFETY: all out pointers are valid; the returned bytes belong to
        // `description`, which outlives the copy below.
        let status = unsafe {
            if is_hevc {
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    description,
                    index,
                    &raw mut pointer,
                    &raw mut size,
                    &raw mut count,
                    &raw mut header_length,
                )
            } else {
                CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    description,
                    index,
                    &raw mut pointer,
                    &raw mut size,
                    &raw mut count,
                    &raw mut header_length,
                )
            }
        };
        if status != 0 || pointer.is_null() {
            return Err(CodecError::EncodingFailed(format!(
                "reading parameter set {index} failed: OSStatus {status}"
            )));
        }
        // SAFETY: `pointer` addresses `size` bytes owned by `description`.
        sets.push(unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec());
        index += 1;
    }
    let nal_length_size = usize::try_from(header_length)
        .map_err(|_| CodecError::EncodingFailed("negative NAL length size".into()))?;
    Ok((sets, nal_length_size))
}

/// The `avcC`/`hvcC` record `VideoToolbox` writes into the format description.
fn codec_config(
    description: &objc2_core_media::CMFormatDescription,
    is_hevc: bool,
) -> Result<Vec<u8>, CodecError> {
    let missing = || {
        CodecError::EncodingFailed(
            "the VideoToolbox format description has no avcC/hvcC sample-description atom".into(),
        )
    };
    // SAFETY: a constant key exported by `CoreMedia`.
    let atoms_key = unsafe { kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms };
    // SAFETY: read-only accessor on a live format description.
    let atoms = unsafe { description.extension(atoms_key) }.ok_or_else(missing)?;
    let atoms = atoms.downcast::<CFDictionary>().map_err(|_| missing())?;
    // SAFETY: the sample-description atoms map four-character code strings to
    // `CFData` records.
    let atoms = unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFData>>(atoms) };
    let record = atoms
        .get(&CFString::from_str(if is_hevc { "hvcC" } else { "avcC" }))
        .ok_or_else(missing)?;
    Ok(record.to_vec())
}

fn set_video_toolbox_color(
    session: &VTCompressionSession,
    color: VideoColorInfo,
) -> Result<(), CodecError> {
    if color.dolby_vision {
        return Err(CodecError::Unsupported(
            "VideoToolbox cannot signal Dolby Vision metadata".into(),
        ));
    }
    // SAFETY: constant strings exported by `CoreVideo` and `VideoToolbox`.
    unsafe {
        let primaries = match color.primaries {
            ColorPrimaries::Bt709 => kCVImageBufferColorPrimaries_ITU_R_709_2,
            ColorPrimaries::Bt601 => kCVImageBufferColorPrimaries_SMPTE_C,
            ColorPrimaries::Bt2020 => kCVImageBufferColorPrimaries_ITU_R_2020,
            ColorPrimaries::DisplayP3 => kCVImageBufferColorPrimaries_P3_D65,
        };
        let transfer = match color.transfer {
            TransferFunction::Sdr => kCVImageBufferTransferFunction_ITU_R_709_2,
            TransferFunction::Pq => kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ,
            TransferFunction::Hlg => kCVImageBufferTransferFunction_ITU_R_2100_HLG,
        };
        let matrix = match color.matrix {
            MatrixCoefficients::Bt601 => kCVImageBufferYCbCrMatrix_ITU_R_601_4,
            MatrixCoefficients::Bt709 => kCVImageBufferYCbCrMatrix_ITU_R_709_2,
            MatrixCoefficients::Bt2020NonConstantLuminance => kCVImageBufferYCbCrMatrix_ITU_R_2020,
            MatrixCoefficients::Bt2020ConstantLuminance => {
                return Err(CodecError::Unsupported(
                    "VideoToolbox cannot represent BT.2020 constant-luminance matrix coefficients"
                        .into(),
                ));
            }
        };
        set_property(
            session,
            kVTCompressionPropertyKey_ColorPrimaries,
            primaries.as_ref(),
        )?;
        set_property(
            session,
            kVTCompressionPropertyKey_TransferFunction,
            transfer.as_ref(),
        )?;
        set_property(
            session,
            kVTCompressionPropertyKey_YCbCrMatrix,
            matrix.as_ref(),
        )?;
    }
    Ok(())
}
