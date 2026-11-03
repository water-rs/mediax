//! Video parameters parsed from `avcC`, `hvcC` and `av1C` configuration boxes.

use std::fmt;

use broadcast_common::Parse as _;
use bytes::{BufMut as _, Bytes, BytesMut};
use mediax_core::{Error, FrameRate};
use transmux::{
    AVCConfigurationBox, Av1ConfigurationBox, HEVCConfigurationBox, decode_avc_sps, decode_hevc_sps,
};

use crate::stream::Codec;

/// ISO BMFF header length: 32-bit size and four-character type.
const BOX_HEADER_LEN: usize = 8;
/// H.265 `NAL_unit_type` of a sequence parameter set.
const HEVC_SPS_NAL_TYPE: u8 = 33;
/// `avgFrameRate` counts frames per 256 seconds.
const HEVC_AVG_FRAME_RATE_PERIOD: u64 = 256;

/// Codec level, normalized to the form every codec's specification prints
/// (H.264 `level_idc` 41 is 4.1, H.265 `general_level_idc` 123 is 4.1, AV1
/// `seq_level_idx` 9 is 4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoLevel {
    /// H.264 level 1b, signalled by `level_idc` 9 or, in the Baseline, Main
    /// and Extended profiles, by `level_idc` 11 with `constraint_set3_flag`.
    Level1b,
    /// A level with a `major.minor` number.
    Numbered {
        /// Integer part of the level.
        major: u8,
        /// Fractional part of the level, in tenths.
        minor: u8,
        /// Whether the stream uses the High tier. H.264 has no tiers and
        /// reports `false`.
        high_tier: bool,
    },
}

impl fmt::Display for VideoLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Level1b => formatter.write_str("1b"),
            Self::Numbered {
                major,
                minor,
                high_tier,
            } => {
                write!(formatter, "{major}.{minor}")?;
                if high_tier {
                    formatter.write_str(" (High tier)")?;
                }
                Ok(())
            }
        }
    }
}

/// Chroma subsampling of a coded video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromaFormat {
    /// Luma only.
    Monochrome,
    /// 4:2:0.
    Yuv420,
    /// 4:2:2.
    Yuv422,
    /// 4:4:4.
    Yuv444,
}

impl ChromaFormat {
    /// Maps the `chroma_format_idc` shared by H.264 and H.265.
    fn from_idc(idc: u8) -> Result<Self, Error> {
        match idc {
            0 => Ok(Self::Monochrome),
            1 => Ok(Self::Yuv420),
            2 => Ok(Self::Yuv422),
            3 => Ok(Self::Yuv444),
            other => Err(Error::Container(format!(
                "chroma_format_idc {other} is outside 0..=3"
            ))),
        }
    }
}

/// Decoder parameters read from a video track's configuration record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct VideoParameters {
    /// The codec's profile number: H.264 `profile_idc`, H.265
    /// `general_profile_idc`, AV1 `seq_profile`.
    pub profile: u8,
    /// The codec level, or `None` when the stream declares no limit (AV1
    /// `seq_level_idx` 31, which encoders such as rav1e write).
    pub level: Option<VideoLevel>,
    /// Luma bit depth.
    pub bit_depth: u8,
    /// Chroma subsampling.
    pub chroma_format: ChromaFormat,
    /// The nominal frame rate where the track declares one: the container's
    /// sample timing when it is constant, else the `hvcC` `avgFrameRate` or
    /// the parameter set's VUI timing. AV1 configuration records carry none.
    pub frame_rate: Option<FrameRate>,
}

/// The configuration box type for `codec`, when mediax parses its parameters.
pub const fn configuration_fourcc(codec: Codec) -> Option<[u8; 4]> {
    match codec {
        Codec::H264 => Some(*b"avcC"),
        Codec::H265 => Some(*b"hvcC"),
        Codec::Av1 => Some(*b"av1C"),
        _ => None,
    }
}

/// Returns `configuration` as the ISO BMFF box of its codec, adding the box
/// header to a bare record. Codecs without a configuration box pass through.
pub fn canonical_configuration(codec: Codec, configuration: Bytes) -> Result<Bytes, Error> {
    let Some(fourcc) = configuration_fourcc(codec) else {
        return Ok(configuration);
    };
    let declared_size = configuration
        .get(..4)
        .and_then(|size| <[u8; 4]>::try_from(size).ok())
        .map(u32::from_be_bytes)
        .and_then(|size| usize::try_from(size).ok());
    let is_box = configuration.get(4..BOX_HEADER_LEN) == Some(&fourcc[..])
        && declared_size == Some(configuration.len());
    if is_box {
        return Ok(configuration);
    }
    let size = u32::try_from(configuration.len() + BOX_HEADER_LEN).map_err(|_| {
        Error::Container(format!("{codec:?} configuration record exceeds a box size"))
    })?;
    let mut boxed = BytesMut::with_capacity(configuration.len() + BOX_HEADER_LEN);
    boxed.put_u32(size);
    boxed.put_slice(&fourcc);
    boxed.put_slice(&configuration);
    Ok(boxed.freeze())
}

impl VideoParameters {
    /// Parses the configuration box of an H.264, H.265 or AV1 track, or
    /// returns `None` for a codec whose parameters mediax does not read.
    ///
    /// `configuration` is the whole `avcC`, `hvcC` or `av1C` box, header
    /// included.
    ///
    /// # Errors
    ///
    /// Returns an error when the box is malformed, or an AV1 record declares
    /// a reserved level.
    pub fn parse(codec: Codec, configuration: &[u8]) -> Result<Option<Self>, Error> {
        if configuration_fourcc(codec).is_none() {
            return Ok(None);
        }
        let Some(record) = configuration.get(BOX_HEADER_LEN..) else {
            return Err(Error::Container(format!(
                "{codec:?} configuration box is shorter than its header"
            )));
        };
        match codec {
            Codec::H264 => Self::from_avc(record).map(Some),
            Codec::H265 => Self::from_hevc(record).map(Some),
            Codec::Av1 => Self::from_av1(record).map(Some),
            _ => Ok(None),
        }
    }

    fn from_avc(record: &[u8]) -> Result<Self, Error> {
        let record = AVCConfigurationBox::parse_body(record)
            .map_err(container_error)?
            .config;
        let frame_rate = match record.sps.first() {
            Some(sps) => {
                let sps = decode_avc_sps(&sps.0).map_err(container_error)?;
                // Two field ticks make one frame (H.264 E.2.1).
                match (sps.time_scale, sps.num_units_in_tick) {
                    (Some(scale), Some(ticks)) => rational(u64::from(scale), 2 * u64::from(ticks)),
                    _ => None,
                }
            }
            None => None,
        };
        let level = record.level_indication;
        // `constraint_set3_flag` is bit 4 of the byte, counting from the MSB
        // as constraint_set0.
        let constraint_set3 = record.profile_compatibility & 0x10 != 0;
        let level = if level == 9
            || (level == 11 && constraint_set3 && matches!(record.profile_indication, 66 | 77 | 88))
        {
            VideoLevel::Level1b
        } else {
            VideoLevel::Numbered {
                major: level / 10,
                minor: level % 10,
                high_tier: false,
            }
        };
        Ok(Self {
            profile: record.profile_indication,
            level: Some(level),
            bit_depth: record.bit_depth_luma_minus8.unwrap_or(0) + 8,
            chroma_format: ChromaFormat::from_idc(record.chroma_format.unwrap_or(1))?,
            frame_rate,
        })
    }

    fn from_hevc(record: &[u8]) -> Result<Self, Error> {
        let record = HEVCConfigurationBox::parse_body(record)
            .map_err(container_error)?
            .config;
        let frame_rate = if record.avg_frame_rate == 0 {
            let sps = record
                .arrays
                .iter()
                .filter(|array| array.nal_unit_type == HEVC_SPS_NAL_TYPE)
                .find_map(|array| array.nalus.first());
            match sps {
                Some(sps) => {
                    let sps = decode_hevc_sps(&sps.0).map_err(container_error)?;
                    match (sps.time_scale, sps.num_units_in_tick) {
                        (Some(scale), Some(ticks)) => rational(u64::from(scale), u64::from(ticks)),
                        _ => None,
                    }
                }
                None => None,
            }
        } else {
            rational(u64::from(record.avg_frame_rate), HEVC_AVG_FRAME_RATE_PERIOD)
        };
        let level = record.general_level_idc;
        Ok(Self {
            profile: record.general_profile_idc,
            level: Some(VideoLevel::Numbered {
                major: level / 30,
                minor: level % 30 / 3,
                high_tier: record.general_tier_flag,
            }),
            bit_depth: record.bit_depth_luma_minus8 + 8,
            chroma_format: ChromaFormat::from_idc(record.chroma_format_idc)?,
            frame_rate,
        })
    }

    fn from_av1(record: &[u8]) -> Result<Self, Error> {
        let record = Av1ConfigurationBox::parse(record).map_err(container_error)?;
        // AV1 levels 2.0 to 7.3 are indices 0 to 23 (AV1 A.3); 31 asks for no
        // limit; the rest are reserved.
        let index = record.seq_level_idx_0;
        let level = match index {
            0..=23 => Some(VideoLevel::Numbered {
                major: 2 + index / 4,
                minor: index % 4,
                high_tier: record.seq_tier_0,
            }),
            31 => None,
            reserved => {
                return Err(Error::Container(format!(
                    "av1C declares reserved seq_level_idx {reserved}"
                )));
            }
        };
        let chroma_format = match (
            record.monochrome,
            record.chroma_subsampling_x,
            record.chroma_subsampling_y,
        ) {
            (true, _, _) => ChromaFormat::Monochrome,
            (false, true, true) => ChromaFormat::Yuv420,
            (false, true, false) => ChromaFormat::Yuv422,
            (false, false, false) => ChromaFormat::Yuv444,
            (false, false, true) => {
                return Err(Error::Container(String::from(
                    "av1C declares vertical chroma subsampling without horizontal",
                )));
            }
        };
        Ok(Self {
            profile: record.seq_profile,
            level,
            bit_depth: record.bit_depth(),
            chroma_format,
            frame_rate: None,
        })
    }
}

/// `numerator / denominator` in lowest terms, or `None` when it is zero or
/// does not fit the 32-bit fields of [`FrameRate`].
pub fn rational(numerator: u64, denominator: u64) -> Option<FrameRate> {
    let (mut a, mut b) = (numerator, denominator);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    if a == 0 {
        return None;
    }
    let part = |value: u64| {
        u32::try_from(value / a)
            .ok()
            .and_then(std::num::NonZeroU32::new)
    };
    Some(FrameRate::new(part(numerator)?, part(denominator)?))
}

fn container_error(error: impl fmt::Display) -> Error {
    Error::Container(error.to_string())
}

#[cfg(test)]
pub mod tests {
    use std::num::NonZeroU32;

    use bytes::Bytes;
    use mediax_core::FrameRate;
    use transmux::{
        AVCConfigurationBox, AVCDecoderConfigurationRecord, AvcPps, AvcSps, HEVCConfigurationBox,
        HEVCDecoderConfigurationRecord, HevcNalArray, HevcNalUnit,
    };

    use super::{ChromaFormat, VideoLevel, VideoParameters, canonical_configuration, rational};
    use crate::stream::Codec;

    /// MSB-first bit writer for hand-built parameter sets.
    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        used: usize,
    }

    impl BitWriter {
        fn bit(&mut self, bit: bool) {
            if self.used.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if bit {
                *self.bytes.last_mut().expect("a byte was pushed") |= 0x80 >> (self.used % 8);
            }
            self.used += 1;
        }

        fn bits(&mut self, value: u32, count: u32) {
            for shift in (0..count).rev() {
                self.bit(value >> shift & 1 == 1);
            }
        }

        /// Unsigned Exp-Golomb code.
        fn ue(&mut self, value: u32) {
            let coded = value + 1;
            let length = 32 - coded.leading_zeros();
            self.bits(0, length - 1);
            self.bits(coded, length);
        }

        /// The RBSP with emulation-prevention bytes inserted.
        fn finish(mut self) -> Vec<u8> {
            self.bit(true);
            let mut escaped = Vec::new();
            let mut zeros = 0;
            for byte in self.bytes {
                if zeros >= 2 && byte <= 3 {
                    escaped.push(3);
                    zeros = 0;
                }
                zeros = if byte == 0 { zeros + 1 } else { 0 };
                escaped.push(byte);
            }
            escaped
        }
    }

    /// A Baseline-profile level 3.0 SPS NAL unit, 320x240, optionally with
    /// VUI timing of `time_scale` ticks per second and `ticks` per field.
    pub fn avc_sps(timing: Option<(u32, u32)>) -> Vec<u8> {
        let mut sps = BitWriter::default();
        sps.ue(0); // seq_parameter_set_id
        sps.ue(0); // log2_max_frame_num_minus4
        sps.ue(0); // pic_order_cnt_type
        sps.ue(0); // log2_max_pic_order_cnt_lsb_minus4
        sps.ue(1); // max_num_ref_frames
        sps.bit(false); // gaps_in_frame_num_value_allowed_flag
        sps.ue(19); // pic_width_in_mbs_minus1
        sps.ue(14); // pic_height_in_map_units_minus1
        sps.bit(true); // frame_mbs_only_flag
        sps.bit(true); // direct_8x8_inference_flag
        sps.bit(false); // frame_cropping_flag
        sps.bit(timing.is_some()); // vui_parameters_present_flag
        if let Some((time_scale, ticks)) = timing {
            sps.bit(false); // aspect_ratio_info_present_flag
            sps.bit(false); // overscan_info_present_flag
            sps.bit(false); // video_signal_type_present_flag
            sps.bit(false); // chroma_loc_info_present_flag
            sps.bit(true); // timing_info_present_flag
            sps.bits(ticks, 32);
            sps.bits(time_scale, 32);
            sps.bit(true); // fixed_frame_rate_flag
            sps.bit(false); // nal_hrd_parameters_present_flag
            sps.bit(false); // vcl_hrd_parameters_present_flag
            sps.bit(false); // pic_struct_present_flag
            sps.bit(false); // bitstream_restriction_flag
        }
        let mut nal = vec![0x67, 66, 0xc0, 30];
        nal.extend(sps.finish());
        nal
    }

    fn avcc(sps: Vec<u8>) -> Bytes {
        avcc_with(sps, 66, 0xc0, 30)
    }

    fn avcc_with(sps: Vec<u8>, profile: u8, compatibility: u8, level: u8) -> Bytes {
        let record = AVCDecoderConfigurationRecord {
            configuration_version: 1,
            profile_indication: profile,
            profile_compatibility: compatibility,
            level_indication: level,
            length_size_minus_one: 3,
            sps: vec![AvcSps(sps)],
            pps: vec![AvcPps(vec![0x68, 0xce, 0x06, 0xe2])],
            chroma_format: None,
            bit_depth_luma_minus8: None,
            bit_depth_chroma_minus8: None,
            sps_ext: Vec::new(),
        };
        boxed(&AVCConfigurationBox::new(record))
    }

    fn boxed(value: &impl broadcast_common::Serialize<Error = transmux::Error>) -> Bytes {
        let mut bytes = vec![0; value.serialized_len()];
        value.serialize_into(&mut bytes).expect("serialize box");
        Bytes::from(bytes)
    }

    fn rate(numerator: u32, denominator: u32) -> FrameRate {
        FrameRate::new(
            NonZeroU32::new(numerator).unwrap(),
            NonZeroU32::new(denominator).unwrap(),
        )
    }

    #[test]
    fn h264_vui_timing_gives_the_frame_rate() {
        // Two field ticks per frame: 60000 / (2 * 1001) is 30000 / 1001.
        let parameters = VideoParameters::parse(Codec::H264, &avcc(avc_sps(Some((60_000, 1_001)))))
            .expect("parse")
            .expect("H.264 has parameters");
        assert_eq!(parameters.profile, 66);
        assert_eq!(
            parameters.level,
            Some(VideoLevel::Numbered {
                major: 3,
                minor: 0,
                high_tier: false
            })
        );
        assert_eq!(parameters.bit_depth, 8);
        assert_eq!(parameters.chroma_format, ChromaFormat::Yuv420);
        assert_eq!(parameters.frame_rate, Some(rate(30_000, 1_001)));
    }

    #[test]
    fn h264_level_1b_has_two_signallings() {
        let level = |profile: u8, compatibility: u8, level: u8| {
            let configuration = avcc_with(avc_sps(None), profile, compatibility, level);
            VideoParameters::parse(Codec::H264, &configuration)
                .expect("parse")
                .expect("H.264 has parameters")
                .level
        };
        let one_b = Some(VideoLevel::Level1b);
        assert_eq!(level(66, 0xc0, 9), one_b);
        assert_eq!(level(66, 0x10, 11), one_b);
        assert_eq!(level(77, 0x10, 11), one_b);
        assert_eq!(level(88, 0x10, 11), one_b);
        assert_eq!(VideoLevel::Level1b.to_string(), "1b");
        // Level 1.1 without the flag, or in a profile where the flag means
        // something else.
        let numbered = VideoLevel::Numbered {
            major: 1,
            minor: 1,
            high_tier: false,
        };
        let one_one = Some(numbered);
        assert_eq!(level(66, 0xc0 & !0x10, 11), one_one);
        assert_eq!(level(100, 0x10, 11), one_one);
        assert_eq!(numbered.to_string(), "1.1");
    }

    #[test]
    fn h264_without_vui_declares_no_frame_rate() {
        let parameters = VideoParameters::parse(Codec::H264, &avcc(avc_sps(None)))
            .expect("parse")
            .expect("H.264 has parameters");
        assert_eq!(parameters.frame_rate, None);
    }

    #[test]
    fn hevc_average_frame_rate_is_frames_per_256_seconds() {
        let record = HEVCDecoderConfigurationRecord {
            configuration_version: 1,
            general_profile_space: 0,
            general_tier_flag: true,
            general_profile_idc: 2,
            general_profile_compatibility_flags: 0x2000_0000,
            general_constraint_indicator_flags: 0,
            general_level_idc: 123,
            min_spatial_segmentation_idc: 0,
            parallelism_type: 0,
            chroma_format_idc: 1,
            bit_depth_luma_minus8: 2,
            bit_depth_chroma_minus8: 2,
            avg_frame_rate: 7_680,
            constant_frame_rate: 1,
            num_temporal_layers: 1,
            temporal_id_nested: true,
            length_size_minus_one: 3,
            arrays: vec![HevcNalArray::new(
                true,
                32,
                vec![HevcNalUnit(vec![0x40, 0x01, 0x0c])],
            )],
        };
        let configuration = boxed(&HEVCConfigurationBox::new(record));
        let parameters = VideoParameters::parse(Codec::H265, &configuration)
            .expect("parse")
            .expect("H.265 has parameters");
        assert_eq!(parameters.profile, 2);
        assert_eq!(
            parameters.level,
            Some(VideoLevel::Numbered {
                major: 4,
                minor: 1,
                high_tier: true
            })
        );
        assert_eq!(parameters.bit_depth, 10);
        assert_eq!(parameters.frame_rate, Some(rate(30, 1)));
    }

    #[test]
    fn av1_record_gives_profile_level_depth_and_chroma() {
        // seq_profile 2, seq_level_idx 9 (4.1), High tier, 12-bit 4:2:2.
        // Tier, high_bitdepth, twelve_bit, mono_chrome, subsampling x and y.
        let record = [0x81, (2 << 5) | 9, 0b1110_1000, 0];
        let configuration = canonical_configuration(Codec::Av1, Bytes::copy_from_slice(&record))
            .expect("box the record");
        assert_eq!(&configuration[4..8], b"av1C");
        let parameters = VideoParameters::parse(Codec::Av1, &configuration)
            .expect("parse")
            .expect("AV1 has parameters");
        assert_eq!(parameters.profile, 2);
        assert_eq!(
            parameters.level,
            Some(VideoLevel::Numbered {
                major: 4,
                minor: 1,
                high_tier: true
            })
        );
        assert_eq!(parameters.bit_depth, 12);
        assert_eq!(parameters.chroma_format, ChromaFormat::Yuv422);
        assert_eq!(parameters.frame_rate, None);
    }

    #[test]
    fn av1_level_31_is_unrestricted_and_reserved_levels_are_rejected() {
        let parse = |level: u8| {
            let configuration =
                canonical_configuration(Codec::Av1, Bytes::copy_from_slice(&[0x81, level, 0, 0]))
                    .expect("box the record");
            VideoParameters::parse(Codec::Av1, &configuration)
        };
        assert_eq!(parse(31).expect("parse").expect("AV1").level, None);
        assert!(parse(24).is_err());
    }

    #[test]
    fn configuration_is_boxed_once() {
        let boxed_once = avcc(avc_sps(None));
        let record = boxed_once.slice(8..);
        assert_eq!(
            canonical_configuration(Codec::H264, record).expect("box"),
            boxed_once
        );
        assert_eq!(
            canonical_configuration(Codec::H264, boxed_once.clone()).expect("box"),
            boxed_once
        );
    }

    #[test]
    fn rational_reduces_and_rejects_zero() {
        assert_eq!(rational(60_000, 2_002), Some(rate(30_000, 1_001)));
        assert_eq!(rational(0, 5), None);
        assert_eq!(rational(5, 0), None);
        assert_eq!(rational(u64::MAX, 1), None);
    }
}
