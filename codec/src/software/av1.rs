//! AV1 software encoding (rav1e) and decoding (rav1d).

#[cfg(mediax_av1_software_encode)]
use crate::{Bitrate, EncodedPacket, EncoderProfile, FrameOptions};
use crate::{CodecError, DecodePacket, DecodedPixelLayout};
#[cfg(all(
    not(any(target_os = "android", target_arch = "wasm32")),
    any(test, not(target_vendor = "apple"))
))]
use mediax_core::CicpColor;
#[cfg(mediax_av1_software_encode)]
use num_traits::FromPrimitive;
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::{
    DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422,
    DAV1D_PIXEL_LAYOUT_I444,
};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib as rav1d_lib;
#[cfg(mediax_av1_software_encode)]
use rav1e::prelude::*;
use std::fmt;
use std::mem::MaybeUninit;
use std::ptr::NonNull;
use std::{ptr, slice};

/// `EAGAIN` as `rav1d` reports it: "no picture yet, send more data".
///
/// `rav1d` defines its own `EAGAIN` as `libc::EAGAIN`, so this is the same
/// constant it returns — 11 on Linux and Windows, 35 on Apple platforms. Reading
/// the returned code through `std::io::Error::from_raw_os_error` instead is
/// wrong on Windows, where that constructor interprets its argument as a Win32
/// error code rather than a CRT `errno`: 11 becomes `ERROR_BAD_FORMAT`, never
/// matches `ErrorKind::WouldBlock`, and a request for more data is reported as
/// a hard decode failure. Hardcoding a number instead is wrong on Apple.
const DAV1D_EAGAIN: i32 = libc::EAGAIN;

/// CPU-side frame data for software codec output (NV12 or P010 format).
pub struct CpuFrame {
    /// Bi-planar data: Y plane followed by interleaved UV plane.
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: u64,
    pub layout: DecodedPixelLayout,
    /// CICP color description carried by the decoded AV1 sequence header.
    #[cfg(all(
        not(any(target_os = "android", target_arch = "wasm32")),
        any(test, not(target_vendor = "apple"))
    ))]
    pub color: CicpColor,
}

/// AV1 software encoder using rav1e.
#[cfg(mediax_av1_software_encode)]
pub struct Av1Encoder {
    ctx: Context<u8>,
    /// The settings `ctx` was built from, kept to rebuild it at a new rate.
    settings: EncoderConfig,
    threads: usize,
    width: usize,
    height: usize,
    /// A bitrate [`Self::set_bitrate`] asked for, applied at the next frame.
    pending_bitrate: Option<Bitrate>,
}

#[cfg(mediax_av1_software_encode)]
impl fmt::Debug for Av1Encoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Av1Encoder")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// The longest `max_key_frame_interval` rav1e accepts; stands in for "never".
#[cfg(mediax_av1_software_encode)]
const MAX_KEY_FRAME_INTERVAL: u64 = 715_827_882;

/// Tiles a realtime frame gets per encoder thread.
///
/// Four per thread cut the median 720p realtime frame time by about 16% on an
/// idle 8-core M1 against one per thread, for about 0.1 dB of luma PSNR at the
/// same rate.
#[cfg(mediax_av1_software_encode)]
const REALTIME_TILES_PER_THREAD: usize = 4;

/// rav1e's thread pool size: the host's available parallelism.
#[cfg(mediax_av1_software_encode)]
fn encoder_threads() -> Result<usize, CodecError> {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .map_err(|e| CodecError::InitializationFailed(e.to_string()))
}

/// rav1e takes its rate as an `i32` of bits per second.
#[cfg(mediax_av1_software_encode)]
fn rav1e_bitrate(bitrate: Bitrate) -> i32 {
    i32::try_from(bitrate.bits_per_second().get()).unwrap_or(i32::MAX)
}

#[cfg(mediax_av1_software_encode)]
impl Av1Encoder {
    pub fn new(config: &crate::EncoderConfig) -> Result<Self, CodecError> {
        let realtime = matches!(config.profile, EncoderProfile::Realtime);
        let color = config.color;
        if color.dolby_vision {
            return Err(CodecError::Unsupported(
                "AV1 encoder cannot preserve Dolby Vision metadata".into(),
            ));
        }
        let cicp = color.cicp();
        let color_description = ColorDescription {
            color_primaries: ColorPrimaries::from_u8(cicp.primaries).ok_or_else(|| {
                CodecError::Unsupported(format!(
                    "rav1e does not represent CICP color primaries {}",
                    cicp.primaries
                ))
            })?,
            transfer_characteristics: TransferCharacteristics::from_u8(cicp.transfer).ok_or_else(
                || {
                    CodecError::Unsupported(format!(
                        "rav1e does not represent CICP transfer characteristics {}",
                        cicp.transfer
                    ))
                },
            )?,
            matrix_coefficients: MatrixCoefficients::from_u8(cicp.matrix).ok_or_else(|| {
                CodecError::Unsupported(format!(
                    "rav1e does not represent CICP matrix coefficients {}",
                    cicp.matrix
                ))
            })?,
        };
        let width = config.width.get() as usize;
        let height = config.height.get() as usize;

        // Realtime capture trades compression efficiency for speed; offline
        // encoding keeps the balanced preset.
        let mut speed_settings = SpeedSettings::from_preset(if realtime { 10 } else { 6 });
        if realtime {
            // rav1e holds a frame back until its RDO lookahead is full; one
            // frame of lookahead lets every submitted frame come out at once.
            speed_settings.rdo_lookahead_frames = 1;
        }
        // rav1e has no switch for the AV1 screen-content tools in video mode
        // (it only enables them for still pictures), so the realtime
        // screen-content tuning is the non-perceptual tune, which keeps the
        // hard edges of text and UI that psychovisual smoothing blurs.
        let tune = if realtime {
            Tune::Psnr
        } else {
            Tune::default()
        };
        let max_key_frame_interval =
            config
                .keyframe_interval
                .map_or(MAX_KEY_FRAME_INTERVAL, |interval| {
                    config
                        .frame_rate
                        .frames_in(interval)
                        .min(MAX_KEY_FRAME_INTERVAL)
                });
        let threads = encoder_threads()?;
        let settings = EncoderConfig {
            width,
            height,
            // In low-latency mode rav1e parallelises a frame only across its
            // tiles and waits for the slowest one, so a realtime frame is cut
            // into several tiles per thread for work stealing to even out
            // tiles of unequal cost; rav1e clamps the count to what the frame
            // size allows. Offline encoding keeps
            // rav1e's own tiling.
            tiles: if realtime {
                threads * REALTIME_TILES_PER_THREAD
            } else {
                0
            },
            bit_depth: 8,
            chroma_sampling: ChromaSampling::Cs420,
            time_base: Rational::new(
                u64::from(config.frame_rate.denominator().get()),
                u64::from(config.frame_rate.numerator().get()),
            ),
            speed_settings,
            low_latency: true,
            tune,
            bitrate: rav1e_bitrate(config.bitrate),
            max_key_frame_interval,
            pixel_range: if cicp.full_range {
                PixelRange::Full
            } else {
                PixelRange::Limited
            },
            color_description: Some(color_description),
            content_light: color.content_light_level.map(|level| ContentLight {
                max_content_light_level: level.max_content_light_level(),
                max_frame_average_light_level: level.max_frame_average_light_level(),
            }),
            mastering_display: None,
            ..Default::default()
        };
        let ctx = Config::new()
            .with_encoder_config(settings.clone())
            .with_threads(threads)
            .new_context()
            .map_err(|e| CodecError::InitializationFailed(e.to_string()))?;

        Ok(Self {
            ctx,
            settings,
            threads,
            width,
            height,
            pending_bitrate: None,
        })
    }

    /// Holds `bitrate` until the next frame, which then restarts the encoder
    /// context at the new rate as a keyframe (see [`crate::Encoder::set_bitrate`]).
    pub const fn set_bitrate(&mut self, bitrate: Bitrate) {
        self.pending_bitrate = Some(bitrate);
    }

    /// Encode NV12 data to AV1.
    pub fn encode_nv12(
        &mut self,
        nv12: &[u8],
        options: FrameOptions,
    ) -> Result<Vec<EncodedPacket>, CodecError> {
        let y_size = self.width * self.height;
        let uv_size = y_size / 2; // Interleaved UV
        let expected_size = y_size + uv_size;

        if nv12.len() != expected_size {
            return Err(CodecError::EncodingFailed(format!(
                "Data size {} doesn't match expected {} for {}x{} NV12",
                nv12.len(),
                expected_size,
                self.width,
                self.height
            )));
        }

        // A pending rate change restarts the context: rav1e's rate control
        // is fixed at construction. Frames the old context still holds are
        // emitted first, at the old rate.
        let mut output = Vec::new();
        let keyframe = if let Some(bitrate) = self.pending_bitrate.take() {
            output = self.flush()?;
            self.settings.bitrate = rav1e_bitrate(bitrate);
            self.ctx = Config::new()
                .with_encoder_config(self.settings.clone())
                .with_threads(self.threads)
                .new_context()
                .map_err(|e| CodecError::EncodingFailed(e.to_string()))?;
            true
        } else {
            options.keyframe
        };

        let mut f = self.ctx.new_frame();

        // Copy Y plane
        let y_data = &nv12[..y_size];
        for (row_idx, row) in f.planes[0].rows_iter_mut().take(self.height).enumerate() {
            let src_start = row_idx * self.width;
            let src_end = src_start + self.width;
            row[..self.width].copy_from_slice(&y_data[src_start..src_end]);
        }

        // De-interleave UV and copy to U/V planes
        let uv_data = &nv12[y_size..];
        let uv_width = self.width / 2;
        let uv_height = self.height / 2;

        // First copy U plane
        for (row_idx, u_row) in f.planes[1].rows_iter_mut().take(uv_height).enumerate() {
            for (col_idx, pixel) in u_row.iter_mut().enumerate().take(uv_width) {
                let src_idx = row_idx * self.width + col_idx * 2;
                *pixel = uv_data[src_idx];
            }
        }

        // Then copy V plane
        for (row_idx, v_row) in f.planes[2].rows_iter_mut().take(uv_height).enumerate() {
            for (col_idx, pixel) in v_row.iter_mut().enumerate().take(uv_width) {
                let src_idx = row_idx * self.width + col_idx * 2 + 1;
                *pixel = uv_data[src_idx];
            }
        }

        let parameters = FrameParameters {
            frame_type_override: if keyframe {
                FrameTypeOverride::Key
            } else {
                FrameTypeOverride::No
            },
            ..Default::default()
        };
        self.ctx
            .send_frame((f, parameters))
            .map_err(|e| CodecError::EncodingFailed(e.to_string()))?;

        loop {
            match self.ctx.receive_packet() {
                Ok(pkt) => output.push(EncodedPacket {
                    keyframe: pkt.frame_type == FrameType::KEY,
                    data: pkt.data,
                }),
                Err(
                    EncoderStatus::Encoded
                    | EncoderStatus::NeedMoreData
                    | EncoderStatus::LimitReached,
                ) => break,
                Err(e) => return Err(CodecError::EncodingFailed(e.to_string())),
            }
        }

        Ok(output)
    }

    /// The `av1C` (`AV1CodecConfigurationRecord`) payload for this stream.
    ///
    /// rav1e knows the sequence-level fields at construction, so the record
    /// is available before the first encoded packet; the container gets the
    /// sequence header OBU itself inside the first keyframe sample.
    pub fn codec_config(&self) -> Vec<u8> {
        self.ctx.container_sequence_header()
    }

    /// Signal end of input and drain every packet the encoder still holds.
    ///
    /// rav1e keeps submitted frames queued internally; `encode_nv12` only
    /// returns packets once the encoder decides to emit, so a recording must
    /// call `flush` to receive the frames it already submitted. Each element
    /// is one encoded frame.
    pub fn flush(&mut self) -> Result<Vec<EncodedPacket>, CodecError> {
        self.ctx.flush();

        let mut packets = Vec::new();
        loop {
            match self.ctx.receive_packet() {
                Ok(pkt) => packets.push(EncodedPacket {
                    keyframe: pkt.frame_type == FrameType::KEY,
                    data: pkt.data,
                }),
                Err(EncoderStatus::Encoded | EncoderStatus::NeedMoreData) => {}
                Err(EncoderStatus::LimitReached) => break,
                Err(e) => return Err(CodecError::EncodingFailed(e.to_string())),
            }
        }

        Ok(packets)
    }
}

/// AV1 software decoder using rav1d.
pub struct Av1Decoder {
    ctx: Option<Dav1dContext>,
}

impl fmt::Debug for Av1Decoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Av1Decoder").finish()
    }
}

struct PictureLayout {
    width: usize,
    height: usize,
    width_u32: u32,
    height_u32: u32,
    bit_depth: usize,
    y_stride: usize,
    uv_stride: usize,
    source_uv_width: usize,
    source_uv_height: usize,
    uv_width: usize,
    uv_height: usize,
    has_chroma: bool,
    y_size: usize,
    uv_size: usize,
}

trait Av1Sample {
    const BYTES: usize;
    const NEUTRAL_CHROMA: u16;

    unsafe fn read(ptr: *const u8, byte_offset: usize) -> u16;
    fn append(value: u16, output: &mut Vec<u8>);
}

struct EightBitSample;

impl Av1Sample for EightBitSample {
    const BYTES: usize = 1;
    const NEUTRAL_CHROMA: u16 = 128;

    unsafe fn read(ptr: *const u8, byte_offset: usize) -> u16 {
        u16::from(unsafe { *ptr.add(byte_offset) })
    }

    fn append(value: u16, output: &mut Vec<u8>) {
        output.push(u8::try_from(value).expect("8-bit AV1 sample exceeds u8"));
    }
}

struct TenBitSample;

impl Av1Sample for TenBitSample {
    const BYTES: usize = 2;
    const NEUTRAL_CHROMA: u16 = 512;

    unsafe fn read(ptr: *const u8, byte_offset: usize) -> u16 {
        let bytes = unsafe { slice::from_raw_parts(ptr.add(byte_offset), Self::BYTES) };
        u16::from_ne_bytes([bytes[0], bytes[1]])
    }

    fn append(value: u16, output: &mut Vec<u8>) {
        output.extend_from_slice(&(value << 6).to_le_bytes());
    }
}

impl Av1Decoder {
    pub fn new() -> Result<Self, CodecError> {
        let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
        unsafe {
            rav1d_lib::dav1d_default_settings(NonNull::from(&mut settings).cast());
        }
        let mut settings = unsafe { settings.assume_init() };
        let mut ctx = None;
        let status = unsafe {
            rav1d_lib::dav1d_open(
                Some(NonNull::from(&mut ctx)),
                Some(NonNull::from(&mut settings)),
            )
        };
        if status.0 != 0 {
            return Err(CodecError::InitializationFailed(format!(
                "rav1d open failed with code {}",
                status.0
            )));
        }
        Ok(Self { ctx })
    }

    /// Decode AV1 data to NV12 or P010 frames without discarding source precision.
    pub fn decode(&mut self, packet: DecodePacket<'_>) -> Result<Vec<CpuFrame>, CodecError> {
        let data = packet.data();
        let mut input = Dav1dData::default();
        let input_ptr =
            unsafe { rav1d_lib::dav1d_data_create(Some(NonNull::from(&mut input)), data.len()) };
        if input_ptr.is_null() {
            return Err(CodecError::DecodingFailed(
                "rav1d data_create returned null".to_string(),
            ));
        }
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr(), input_ptr, data.len());
        }
        input.m.timestamp = i64::try_from(packet.presentation_time().as_nanos())
            .map_err(|_| CodecError::DecodingFailed("presentation timestamp exceeds i64".into()))?;
        let send_status =
            unsafe { rav1d_lib::dav1d_send_data(self.ctx, Some(NonNull::from(&mut input))) };
        if send_status.0 != 0 {
            unsafe { rav1d_lib::dav1d_data_unref(Some(NonNull::from(&mut input))) };
            return Err(CodecError::DecodingFailed(format!(
                "rav1d send_data failed with code {}",
                send_status.0
            )));
        }

        self.collect_pictures()
    }

    /// Returns every delayed rav1d output frame.
    pub fn drain(&mut self) -> Result<Vec<CpuFrame>, CodecError> {
        self.collect_pictures()
    }

    fn collect_pictures(&mut self) -> Result<Vec<CpuFrame>, CodecError> {
        let mut frames = Vec::new();
        let mut saw_would_block_once = false;
        loop {
            let mut picture = Dav1dPicture::default();
            let status = unsafe {
                rav1d_lib::dav1d_get_picture(self.ctx, Some(NonNull::from(&mut picture)))
            };
            if status.0 == 0 {
                let frame = Self::picture_to_cpu_frame(&picture);
                unsafe { rav1d_lib::dav1d_picture_unref(Some(NonNull::from(&mut picture))) };
                frames.push(frame?);
                saw_would_block_once = false;
                continue;
            }
            if status.0 == -DAV1D_EAGAIN {
                if saw_would_block_once {
                    break;
                }
                // `dav1d` may require one extra poll after the first EAGAIN to drain
                // frames that became available during internal scheduling.
                saw_would_block_once = true;
                continue;
            }
            return Err(CodecError::DecodingFailed(format!(
                "rav1d get_picture failed with code {}",
                status.0
            )));
        }

        Ok(frames)
    }

    fn picture_to_cpu_frame(picture: &Dav1dPicture) -> Result<CpuFrame, CodecError> {
        let layout = Self::picture_layout(picture)?;
        let y_ptr = Self::plane_ptr(picture, 0, "Y")?;
        let chroma_ptrs = if layout.has_chroma {
            Some((
                Self::plane_ptr(picture, 1, "U")?,
                Self::plane_ptr(picture, 2, "V")?,
            ))
        } else {
            None
        };

        let pixel_layout = match layout.bit_depth {
            8 => DecodedPixelLayout::Nv12,
            10 => DecodedPixelLayout::P010,
            bit_depth => {
                return Err(CodecError::Unsupported(format!(
                    "AV1 {bit_depth}-bit output has no supported bi-planar GPU layout"
                )));
            }
        };
        let mut biplanar = Vec::with_capacity(layout.y_size + layout.uv_size);
        #[cfg(all(
            not(any(target_os = "android", target_arch = "wasm32")),
            any(test, not(target_vendor = "apple"))
        ))]
        let sequence_header = unsafe {
            picture
                .seq_hdr
                .ok_or_else(|| {
                    CodecError::DecodingFailed("rav1d returned no AV1 sequence header".into())
                })?
                .as_ref()
        };
        #[cfg(all(
            not(any(target_os = "android", target_arch = "wasm32")),
            any(test, not(target_vendor = "apple"))
        ))]
        let color = CicpColor {
            primaries: u8::try_from(sequence_header.pri).map_err(|_| {
                CodecError::DecodingFailed("AV1 color primaries exceed CICP range".into())
            })?,
            transfer: u8::try_from(sequence_header.trc).map_err(|_| {
                CodecError::DecodingFailed("AV1 transfer characteristics exceed CICP range".into())
            })?,
            matrix: u8::try_from(sequence_header.mtrx).map_err(|_| {
                CodecError::DecodingFailed("AV1 matrix coefficients exceed CICP range".into())
            })?,
            full_range: sequence_header.color_range != 0,
        };

        match layout.bit_depth {
            8 => {
                Self::copy_to_biplanar::<EightBitSample>(
                    &layout,
                    y_ptr,
                    chroma_ptrs,
                    &mut biplanar,
                );
            }
            10 => {
                Self::copy_to_biplanar::<TenBitSample>(&layout, y_ptr, chroma_ptrs, &mut biplanar);
            }
            _ => unreachable!("pixel layout rejects unsupported AV1 bit depths"),
        }

        Ok(CpuFrame {
            data: biplanar,
            width: layout.width_u32,
            height: layout.height_u32,
            timestamp_ns: u64::try_from(picture.m.timestamp).map_err(|_| {
                CodecError::DecodingFailed(format!(
                    "rav1d returned invalid timestamp {}",
                    picture.m.timestamp
                ))
            })?,
            layout: pixel_layout,
            #[cfg(all(
                not(any(target_os = "android", target_arch = "wasm32")),
                any(test, not(target_vendor = "apple"))
            ))]
            color,
        })
    }

    fn picture_layout(picture: &Dav1dPicture) -> Result<PictureLayout, CodecError> {
        let width = usize::try_from(picture.p.w).map_err(|_| {
            CodecError::DecodingFailed(format!("rav1d returned invalid width {}", picture.p.w))
        })?;
        let width_u32 = u32::try_from(width).map_err(|_| {
            CodecError::DecodingFailed(format!("rav1d width {width} exceeds supported range"))
        })?;
        let height = usize::try_from(picture.p.h).map_err(|_| {
            CodecError::DecodingFailed(format!("rav1d returned invalid height {}", picture.p.h))
        })?;
        let height_u32 = u32::try_from(height).map_err(|_| {
            CodecError::DecodingFailed(format!("rav1d height {height} exceeds supported range"))
        })?;
        let (source_uv_width, source_uv_height, has_chroma) = match picture.p.layout {
            DAV1D_PIXEL_LAYOUT_I400 => (0, 0, false),
            DAV1D_PIXEL_LAYOUT_I420 => (width.div_ceil(2), height.div_ceil(2), true),
            DAV1D_PIXEL_LAYOUT_I422 => (width.div_ceil(2), height, true),
            DAV1D_PIXEL_LAYOUT_I444 => (width, height, true),
            layout => {
                return Err(CodecError::DecodingFailed(format!(
                    "rav1d returned unknown pixel layout {layout}"
                )));
            }
        };
        let bit_depth = usize::try_from(picture.p.bpc).map_err(|_| {
            CodecError::DecodingFailed(format!(
                "rav1d returned invalid bit depth {}",
                picture.p.bpc
            ))
        })?;
        if !matches!(bit_depth, 8 | 10 | 12) {
            return Err(CodecError::DecodingFailed(format!(
                "rav1d returned unsupported bit depth {}",
                picture.p.bpc
            )));
        }
        let y_stride = usize::try_from(picture.stride[0]).map_err(|_| {
            CodecError::DecodingFailed(format!(
                "rav1d returned invalid Y stride {}",
                picture.stride[0]
            ))
        })?;
        let uv_stride = if has_chroma {
            usize::try_from(picture.stride[1]).map_err(|_| {
                CodecError::DecodingFailed(format!(
                    "rav1d returned invalid UV stride {}",
                    picture.stride[1]
                ))
            })?
        } else {
            0
        };

        let sample_bytes = if bit_depth <= 8 { 1 } else { 2 };
        let uv_width = width.div_ceil(2);
        let uv_height = height.div_ceil(2);
        let y_size = width * height * sample_bytes;
        let uv_size = uv_width * uv_height * 2 * sample_bytes;
        let y_min_stride = width
            .checked_mul(sample_bytes)
            .ok_or_else(|| CodecError::DecodingFailed("rav1d Y stride overflow".to_string()))?;
        if y_stride < y_min_stride {
            return Err(CodecError::DecodingFailed(format!(
                "rav1d Y stride {y_stride} is smaller than required {y_min_stride}"
            )));
        }
        let uv_min_stride = source_uv_width
            .checked_mul(sample_bytes)
            .ok_or_else(|| CodecError::DecodingFailed("rav1d UV stride overflow".to_string()))?;
        if has_chroma && uv_stride < uv_min_stride {
            return Err(CodecError::DecodingFailed(format!(
                "rav1d UV stride {uv_stride} is smaller than required {uv_min_stride}"
            )));
        }

        Ok(PictureLayout {
            width,
            height,
            width_u32,
            height_u32,
            bit_depth,
            y_stride,
            uv_stride,
            source_uv_width,
            source_uv_height,
            uv_width,
            uv_height,
            has_chroma,
            y_size,
            uv_size,
        })
    }

    fn copy_to_biplanar<S: Av1Sample>(
        layout: &PictureLayout,
        y_ptr: *const u8,
        chroma_ptrs: Option<(*const u8, *const u8)>,
        output: &mut Vec<u8>,
    ) {
        for row in 0..layout.height {
            for column in 0..layout.width {
                let offset = row * layout.y_stride + column * S::BYTES;
                let sample = unsafe { S::read(y_ptr, offset) };
                S::append(sample, output);
            }
        }

        for row in 0..layout.uv_height {
            for column in 0..layout.uv_width {
                let (u, v) =
                    chroma_ptrs.map_or((S::NEUTRAL_CHROMA, S::NEUTRAL_CHROMA), |(u_ptr, v_ptr)| {
                        (
                            Self::downsample_chroma::<S>(layout, u_ptr, column, row),
                            Self::downsample_chroma::<S>(layout, v_ptr, column, row),
                        )
                    });
                S::append(u, output);
                S::append(v, output);
            }
        }
    }

    fn downsample_chroma<S: Av1Sample>(
        layout: &PictureLayout,
        plane: *const u8,
        output_column: usize,
        output_row: usize,
    ) -> u16 {
        let horizontal_samples = layout.source_uv_width.div_ceil(layout.uv_width);
        let vertical_samples = layout.source_uv_height.div_ceil(layout.uv_height);
        let source_column = output_column * horizontal_samples;
        let source_row = output_row * vertical_samples;
        let end_column = (source_column + horizontal_samples).min(layout.source_uv_width);
        let end_row = (source_row + vertical_samples).min(layout.source_uv_height);
        let mut sum = 0_u32;
        let mut count = 0_u32;
        for row in source_row..end_row {
            for column in source_column..end_column {
                let offset = row * layout.uv_stride + column * S::BYTES;
                sum += u32::from(unsafe { S::read(plane, offset) });
                count += 1;
            }
        }
        u16::try_from((sum + count / 2) / count).expect("averaged AV1 chroma exceeds u16")
    }

    fn plane_ptr(
        picture: &Dav1dPicture,
        index: usize,
        name: &'static str,
    ) -> Result<*const u8, CodecError> {
        let plane = picture.data[index].ok_or_else(|| {
            CodecError::DecodingFailed(format!("rav1d returned missing {name} plane"))
        })?;
        Ok(plane.cast::<u8>().as_ptr().cast_const())
    }
}

impl Drop for Av1Decoder {
    fn drop(&mut self) {
        unsafe { rav1d_lib::dav1d_close(Some(NonNull::from(&mut self.ctx))) };
    }
}

#[cfg(test)]
mod tests {
    use super::{Av1Decoder, PictureLayout, TenBitSample};

    #[test]
    fn ten_bit_444_is_downsampled_to_p010() {
        let y = [0_u16, 256, 512, 1023];
        let u = [0_u16, 100, 200, 300];
        let v = [400_u16, 500, 600, 700];
        let layout = PictureLayout {
            width: 2,
            height: 2,
            width_u32: 2,
            height_u32: 2,
            bit_depth: 10,
            y_stride: 4,
            uv_stride: 4,
            source_uv_width: 2,
            source_uv_height: 2,
            uv_width: 1,
            uv_height: 1,
            has_chroma: true,
            y_size: 8,
            uv_size: 4,
        };
        let mut output = Vec::new();
        Av1Decoder::copy_to_biplanar::<TenBitSample>(
            &layout,
            y.as_ptr().cast(),
            Some((u.as_ptr().cast(), v.as_ptr().cast())),
            &mut output,
        );
        let samples: Vec<u16> = output
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| u16::from_le_bytes(*bytes) >> 6)
            .collect();
        assert_eq!(samples, [0, 256, 512, 1023, 150, 550]);
    }
}

#[cfg(all(test, mediax_av1_software_encode))]
#[allow(
    clippy::cast_possible_truncation,
    clippy::missing_const_for_fn,
    reason = "test patterns on small known ranges"
)]
mod encoder_tests {
    use crate::{
        Bitrate, Encoder, EncoderCodec, EncoderConfig, EncoderProfile, EvenDimension, FrameOptions,
        FrameRate,
    };
    use mediax_core::{
        ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorInfo,
    };
    use std::{num::NonZeroU32, time::Instant};

    fn nz(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("test value is non-zero")
    }

    fn encoder(profile: EncoderProfile) -> Encoder {
        Encoder::new(EncoderConfig {
            codec: EncoderCodec::Av1,
            width: EvenDimension::new(320).unwrap(),
            height: EvenDimension::new(240).unwrap(),
            color: VideoColorInfo {
                primaries: ColorPrimaries::Bt709,
                transfer: TransferFunction::Sdr,
                matrix: MatrixCoefficients::Bt709,
                range: ColorRange::Limited,
                content_light_level: None,
                dolby_vision: false,
            },
            profile,
            bitrate: Bitrate::new(nz(500_000)),
            frame_rate: FrameRate::new(nz(30), nz(1)),
            keyframe_interval: None,
        })
        .expect("software AV1 encoder")
    }

    fn frame(index: usize) -> Vec<u8> {
        let mut data = vec![128_u8; 320 * 240 * 3 / 2];
        for (offset, luma) in data[..320 * 240].iter_mut().enumerate() {
            *luma = ((offset % 320 + offset / 320 + index * 5) % 200 + 20) as u8;
        }
        data
    }

    /// Submits `count` frames, `request` and `rate_change` naming the frames
    /// that ask for a keyframe and for a new bitrate; returns every packet's
    /// keyframe flag in output order and the per-call latencies.
    fn run(
        encoder: &mut Encoder,
        count: usize,
        request: Option<usize>,
        rate_change: Option<usize>,
    ) -> (Vec<bool>, Vec<std::time::Duration>) {
        let mut keyframes = Vec::new();
        let mut latencies = Vec::new();
        for index in 0..count {
            if rate_change == Some(index) {
                encoder
                    .set_bitrate(Bitrate::new(nz(2_000_000)))
                    .expect("rav1e takes the rate for the next keyframe");
            }
            let started = Instant::now();
            let packets = encoder
                .encode_nv12(
                    &frame(index),
                    FrameOptions {
                        keyframe: request == Some(index),
                    },
                )
                .collect::<Result<Vec<_>, _>>()
                .expect("encode");
            latencies.push(started.elapsed());
            keyframes.extend(packets.iter().map(|packet| packet.keyframe));
        }
        keyframes.extend(
            encoder
                .flush()
                .expect("flush")
                .iter()
                .map(|packet| packet.keyframe),
        );
        (keyframes, latencies)
    }

    #[test]
    fn realtime_emits_every_frame_and_honours_keyframe_requests() {
        let mut encoder = encoder(EncoderProfile::Realtime);
        let (keyframes, mut latencies) = run(&mut encoder, 30, Some(12), None);
        assert_eq!(keyframes.len(), 30);
        let positions: Vec<_> = (0..30).filter(|&index| keyframes[index]).collect();
        assert_eq!(positions, [0, 12]);
        latencies.sort();
        crate::init_test_tracing();
        tracing::info!(
            median = ?latencies[latencies.len() / 2],
            max = ?latencies[latencies.len() - 1],
            "AV1 rav1e 320x240 Realtime latency"
        );
    }

    #[test]
    fn bitrate_change_restarts_at_a_keyframe_after_draining_the_old_context() {
        let mut encoder = encoder(EncoderProfile::Realtime);
        let (keyframes, _) = run(&mut encoder, 16, None, Some(10));
        assert_eq!(keyframes.len(), 16, "no frame is lost across the restart");
        let positions: Vec<_> = (0..16).filter(|&index| keyframes[index]).collect();
        assert_eq!(
            positions,
            [0, 10],
            "the first frame after set_bitrate is a keyframe"
        );
    }
}
