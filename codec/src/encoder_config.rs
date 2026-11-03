//! Typed encoder configuration and per-frame options.

use crate::{CodecError, CodecType};
use mediax_core::VideoColorInfo;
use std::{num::NonZeroU32, time::Duration};

/// How an [`Encoder`](crate::Encoder) trades compression efficiency for latency.
///
/// The profile is chosen at construction because encoders bake their rate
/// control, threading and speed preset into the session; it cannot be
/// switched on the fly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncoderProfile {
    /// Compression-first encoding for offline jobs: balanced speed preset
    /// and a modest thread count (rav1e preset 6 on four threads).
    #[default]
    Offline,
    /// Latency-first encoding for live capture and WebRTC.
    ///
    /// - `VideoToolbox`: `RealTime`, no frame reordering and the low-latency
    ///   rate controller.
    /// - Media Foundation: low-latency mode and no B-frames.
    /// - `MediaCodec`: the low-latency key and no B-frames, where the API
    ///   level has them.
    /// - rav1e: the fastest speed preset on every available core, its
    ///   low-latency mode, one frame of RDO lookahead and the non-perceptual
    ///   tune that keeps text and UI edges sharp. rav1e's own pipeline still
    ///   releases a frame's packet four submissions after the frame.
    Realtime,
}

/// A frame width or height: non-zero and even, because input is 4:2:0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EvenDimension(NonZeroU32);

impl EvenDimension {
    /// Creates a dimension of `pixels`.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::Unsupported`] when `pixels` is zero or odd.
    pub fn new(pixels: u32) -> Result<Self, CodecError> {
        NonZeroU32::new(pixels)
            .filter(|pixels| pixels.get().is_multiple_of(2))
            .map(Self)
            .ok_or_else(|| {
                CodecError::Unsupported(format!(
                    "{pixels} is not a usable frame dimension; 4:2:0 input needs a non-zero even size"
                ))
            })
    }

    /// Returns the size in pixels.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// A target bitrate in bits per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bitrate(NonZeroU32);

impl Bitrate {
    /// Creates a bitrate of `bits_per_second`.
    #[must_use]
    pub const fn new(bits_per_second: NonZeroU32) -> Self {
        Self(bits_per_second)
    }

    /// Returns the bitrate in bits per second.
    #[must_use]
    pub const fn bits_per_second(self) -> NonZeroU32 {
        self.0
    }
}

/// A frame rate as the exact ratio `numerator / denominator` frames per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameRate {
    numerator: NonZeroU32,
    denominator: NonZeroU32,
}

impl FrameRate {
    /// Creates a frame rate of `numerator / denominator` frames per second.
    #[must_use]
    pub const fn new(numerator: NonZeroU32, denominator: NonZeroU32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    /// Returns the numerator.
    #[must_use]
    pub const fn numerator(self) -> NonZeroU32 {
        self.numerator
    }

    /// Returns the denominator.
    #[must_use]
    pub const fn denominator(self) -> NonZeroU32 {
        self.denominator
    }

    /// Returns the frame rate rounded to the nearest whole frames per second,
    /// for platform keys that only take an integer.
    #[must_use]
    pub fn rounded_fps(self) -> u32 {
        let numerator = u64::from(self.numerator.get());
        let denominator = u64::from(self.denominator.get());
        // Rounding half up never exceeds the numerator, so it fits in u32.
        let rounded = (numerator + denominator / 2) / denominator;
        u32::try_from(rounded).map_or(u32::MAX, |fps| fps.max(1))
    }

    /// Returns how many frames fit in `interval`, at least one.
    #[must_use]
    pub fn frames_in(self, interval: Duration) -> u64 {
        let frames = interval.as_nanos() * u128::from(self.numerator.get())
            / (u128::from(self.denominator.get()) * 1_000_000_000);
        u64::try_from(frames).unwrap_or(u64::MAX).max(1)
    }
}

/// Whether H.264/H.265 access units use length prefixes or start codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BitstreamFormat {
    /// Each NAL unit carries a four-byte big-endian length, as in `avcC`/`hvcC`
    /// samples of an MP4 file. Parameter sets live in [`Encoder::codec_config`](crate::Encoder::codec_config).
    LengthPrefixed,
    /// Each NAL unit follows a `00 00 00 01` start code, and every IDR access
    /// unit starts with its parameter sets (SPS and PPS, plus VPS for H.265),
    /// so a decoder can join the stream at any keyframe.
    AnnexB,
}

/// An H.264 profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum H264Profile {
    /// Constrained Baseline: no B-frames, CAVLC only.
    ConstrainedBaseline,
    /// Main.
    Main,
    /// High.
    High,
}

/// An H.264 level, 3.0 through 5.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum H264Level {
    /// Level 3.0.
    L3_0,
    /// Level 3.1.
    L3_1,
    /// Level 3.2.
    L3_2,
    /// Level 4.0.
    L4_0,
    /// Level 4.1.
    L4_1,
    /// Level 4.2.
    L4_2,
    /// Level 5.0.
    L5_0,
    /// Level 5.1.
    L5_1,
    /// Level 5.2.
    L5_2,
}

impl H264Level {
    /// Returns the `level_idc` the specification assigns this level.
    #[must_use]
    pub const fn idc(self) -> u8 {
        match self {
            Self::L3_0 => 30,
            Self::L3_1 => 31,
            Self::L3_2 => 32,
            Self::L4_0 => 40,
            Self::L4_1 => 41,
            Self::L4_2 => 42,
            Self::L5_0 => 50,
            Self::L5_1 => 51,
            Self::L5_2 => 52,
        }
    }
}

/// H.264 stream settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct H264Settings {
    /// The profile the stream conforms to.
    pub profile: H264Profile,
    /// The level the stream conforms to.
    pub level: H264Level,
    /// The framing of the emitted access units.
    pub bitstream: BitstreamFormat,
}

impl H264Settings {
    /// Returns the three bytes of the SDP `profile-level-id` for these
    /// settings: `profile_idc`, the constraint flags and `level_idc`
    /// (RFC 6184, section 8.1).
    ///
    /// The constraint byte follows the values WebRTC offers: `E0` for
    /// Constrained Baseline and `00` for Main and High.
    #[must_use]
    pub const fn profile_level_id(self) -> [u8; 3] {
        let (profile_idc, constraints) = match self.profile {
            H264Profile::ConstrainedBaseline => (0x42, 0xE0),
            H264Profile::Main => (0x4D, 0x00),
            H264Profile::High => (0x64, 0x00),
        };
        [profile_idc, constraints, self.level.idc()]
    }
}

/// H.265 stream settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct H265Settings {
    /// The framing of the emitted access units.
    pub bitstream: BitstreamFormat,
}

/// The codec an [`Encoder`](crate::Encoder) produces, with its stream settings.
///
/// AV1 always emits OBUs, so it carries no [`BitstreamFormat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncoderCodec {
    /// H.264 (AVC), hardware only.
    H264(H264Settings),
    /// H.265 (HEVC), hardware only.
    H265(H265Settings),
    /// AV1, in software.
    Av1,
}

impl EncoderCodec {
    /// Returns the codec family.
    #[must_use]
    pub const fn codec_type(self) -> CodecType {
        match self {
            Self::H264(_) => CodecType::H264,
            Self::H265(_) => CodecType::H265,
            Self::Av1 => CodecType::Av1,
        }
    }

    /// Returns the framing of the emitted access units, or `None` for AV1,
    /// which always emits OBUs.
    #[must_use]
    pub const fn bitstream(self) -> Option<BitstreamFormat> {
        match self {
            Self::H264(settings) => Some(settings.bitstream),
            Self::H265(settings) => Some(settings.bitstream),
            Self::Av1 => None,
        }
    }
}

/// Everything an [`Encoder`](crate::Encoder) is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    /// The codec and its stream settings.
    pub codec: EncoderCodec,
    /// Frame width in pixels.
    pub width: EvenDimension,
    /// Frame height in pixels.
    pub height: EvenDimension,
    /// The colorimetry of the input samples, signalled on the output.
    pub color: VideoColorInfo,
    /// Offline or realtime tuning.
    pub profile: EncoderProfile,
    /// The initial target bitrate; see [`Encoder::set_bitrate`](crate::Encoder::set_bitrate).
    pub bitrate: Bitrate,
    /// The rate frames are submitted at.
    pub frame_rate: FrameRate,
    /// The longest span between keyframes, or `None` for keyframes only on the
    /// first frame and on request ([`FrameOptions::keyframe`]).
    pub keyframe_interval: Option<Duration>,
}

/// Options for one submitted frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameOptions {
    /// Encode this frame as an IDR, a random access point.
    pub keyframe: bool,
}

/// One encoded access unit and the timing of the frame it encodes.
///
/// A frame the encoder drops under rate control produces no packet, so
/// packets pair with the submitted frames by [`presentation_time`], never by
/// count or order.
///
/// [`presentation_time`]: Self::presentation_time
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedPacket {
    /// The access unit in the configured bitstream format.
    pub data: Vec<u8>,
    /// Whether this is a random access point: an IDR for H.264 and H.265, a
    /// key frame for AV1.
    pub keyframe: bool,
    /// The presentation time the frame was submitted with, to the resolution
    /// of the platform encoder's own clock (100 ns for Media Foundation, 1 us
    /// for `MediaCodec`, exact for the others).
    pub presentation_time: Duration,
    /// The time the frame is decoded at, in signed nanoseconds on the
    /// encoder's decode timeline, as the platform encoder reports it. Decode
    /// times strictly increase from packet to packet. An encoder that does not
    /// reorder frames (VA-API, `MediaCodec`, rav1e, and `VideoToolbox` and
    /// Media Foundation in the realtime profile) decodes each frame at its
    /// presentation time. A reordering encoder (B-frames, offline profile)
    /// decodes in a different order than it presents, and its decode timeline
    /// is offset from the presentation timeline by the platform's own lead: a
    /// decode time can fall after the presentation time of the same frame, a
    /// negative composition offset, which a container muxer must absorb.
    pub decode_time_nanos: i64,
}

/// `time` in the signed nanoseconds of [`EncodedPacket::decode_time_nanos`].
///
/// # Errors
///
/// Returns [`CodecError::EncodingFailed`] for a time past `i64::MAX`
/// nanoseconds (about 292 years).
#[cfg(any(
    mediax_av1_software_encode,
    mediax_hw_codec_android,
    mediax_hw_codec_windows,
    mediax_hw_codec_vaapi,
    // The VA-API module also compiles for its driver-double tests.
    all(test, unix)
))]
pub fn signed_nanos(time: Duration) -> Result<i64, CodecError> {
    i64::try_from(time.as_nanos())
        .map_err(|_| CodecError::EncodingFailed(format!("{time:?} does not fit i64 nanoseconds")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("test value is non-zero")
    }

    fn id(profile: H264Profile, level: H264Level) -> String {
        let bytes = H264Settings {
            profile,
            level,
            bitstream: BitstreamFormat::AnnexB,
        }
        .profile_level_id();
        format!("{:02x}{:02x}{:02x}", bytes[0], bytes[1], bytes[2])
    }

    #[test]
    fn profile_level_id_matches_the_values_webrtc_negotiates() {
        assert_eq!(
            id(H264Profile::ConstrainedBaseline, H264Level::L3_1),
            "42e01f"
        );
        assert_eq!(id(H264Profile::Main, H264Level::L3_1), "4d001f");
        assert_eq!(id(H264Profile::High, H264Level::L4_0), "640028");
        assert_eq!(id(H264Profile::High, H264Level::L5_2), "640034");
        assert_eq!(
            id(H264Profile::ConstrainedBaseline, H264Level::L3_0),
            "42e01e"
        );
    }

    #[test]
    fn dimensions_must_be_even_and_non_zero() {
        assert_eq!(EvenDimension::new(1280).unwrap().get(), 1280);
        assert!(EvenDimension::new(0).is_err());
        assert!(EvenDimension::new(1279).is_err());
    }

    #[test]
    fn level_idc_follows_the_specification() {
        let idcs = [
            H264Level::L3_0,
            H264Level::L3_1,
            H264Level::L3_2,
            H264Level::L4_0,
            H264Level::L4_1,
            H264Level::L4_2,
            H264Level::L5_0,
            H264Level::L5_1,
            H264Level::L5_2,
        ]
        .map(H264Level::idc);
        assert_eq!(idcs, [30, 31, 32, 40, 41, 42, 50, 51, 52]);
    }

    #[test]
    fn frame_rate_rounding_and_spans() {
        let ntsc = FrameRate::new(nz(30_000), nz(1001));
        assert_eq!(ntsc.rounded_fps(), 30);
        assert_eq!(FrameRate::new(nz(60), nz(1)).rounded_fps(), 60);
        assert_eq!(FrameRate::new(nz(1), nz(4)).rounded_fps(), 1);
        assert_eq!(
            FrameRate::new(nz(30), nz(1)).frames_in(Duration::from_secs(2)),
            60
        );
        assert_eq!(
            FrameRate::new(nz(30), nz(1)).frames_in(Duration::from_millis(1)),
            1
        );
    }
}
