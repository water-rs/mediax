//! H.264 sequence, picture and slice headers for the VA-API packed-header
//! buffers.
//!
//! The encoder is low delay: one slice per picture, one reference frame,
//! `pic_order_cnt_type` 0 with a POC step of two, CAVLC for Constrained
//! Baseline and CABAC for Main and High. Every header is written here and
//! handed to the driver as a packed header with its emulation prevention
//! bytes already in place.

use super::{
    VaapiError,
    bits::{BitWriter, escape_emulation},
};
use crate::{FrameRate, H264Level, H264Profile, annexb::START_CODE};
use mediax_core::CicpColor;

/// `log2_max_frame_num_minus4 + 4`.
pub const LOG2_MAX_FRAME_NUM: u32 = 8;
/// `log2_max_pic_order_cnt_lsb_minus4 + 4`.
pub const LOG2_MAX_POC_LSB: u32 = 8;
/// `max_num_ref_frames`: each P picture references the previous picture.
pub const MAX_NUM_REF_FRAMES: u32 = 1;
/// `pic_init_qp_minus26 + 26`.
pub const PIC_INIT_QP: u8 = 26;
/// The macroblock edge in luma samples.
pub const MACROBLOCK: u32 = 16;

const NAL_SLICE: u8 = 1;
const NAL_IDR_SLICE: u8 = 5;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;

/// `video_format` 5: unspecified.
const VIDEO_FORMAT_UNSPECIFIED: u64 = 5;
/// `log2_max_mv_length_*` for the largest vectors a level 5.2 stream allows.
const LOG2_MAX_MV_LENGTH: u32 = 15;
/// `max_dec_frame_buffering`: the one reference plus the picture being decoded.
const MAX_DEC_FRAME_BUFFERING: u32 = 1;

/// Whether a picture starts a new coded video sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// An IDR picture, coded as an I slice.
    Idr,
    /// A P picture predicted from the previous picture.
    P,
}

impl FrameKind {
    /// Returns the `slice_type` the slice header carries (all slices of the
    /// picture have this type, so the value is offset by five).
    const fn header_slice_type(self) -> u32 {
        match self {
            Self::Idr => 7,
            Self::P => 5,
        }
    }

    /// Returns the `slice_type` modulo five, the value libva takes.
    pub const fn va_slice_type(self) -> u8 {
        match self {
            Self::Idr => 2,
            Self::P => 0,
        }
    }

    const fn nal(self) -> (u8, u8) {
        match self {
            Self::Idr => (3, NAL_IDR_SLICE),
            Self::P => (2, NAL_SLICE),
        }
    }
}

/// The per-picture values a slice header carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SliceHeader {
    /// The picture type.
    pub kind: FrameKind,
    /// `frame_num`.
    pub frame_num: u32,
    /// `idr_pic_id`, used for IDR pictures only.
    pub idr_pic_id: u32,
    /// `pic_order_cnt_lsb`.
    pub poc_lsb: u32,
}

/// A header in the form `VAEncPackedHeaderDataBuffer` takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedHeader {
    /// The start code, NAL header and escaped payload.
    pub data: Vec<u8>,
    /// The number of meaningful bits in `data`.
    pub bit_length: u32,
}

/// Everything the headers of one stream are written from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParameters {
    /// The profile.
    pub profile: H264Profile,
    /// The level.
    pub level: H264Level,
    /// The visible width in luma samples.
    pub width: u32,
    /// The visible height in luma samples.
    pub height: u32,
    /// The colour description signalled in the VUI.
    pub color: CicpColor,
    /// The frame rate signalled in the VUI timing information.
    pub frame_rate: FrameRate,
}

impl StreamParameters {
    /// Returns the width in macroblocks.
    pub const fn width_mbs(&self) -> u32 {
        self.width.div_ceil(MACROBLOCK)
    }

    /// Returns the height in macroblocks.
    pub const fn height_mbs(&self) -> u32 {
        self.height.div_ceil(MACROBLOCK)
    }

    /// Whether the entropy coder is CABAC.
    pub const fn cabac(&self) -> bool {
        !matches!(self.profile, H264Profile::ConstrainedBaseline)
    }

    /// Whether the profile is High, which adds the chroma format fields to
    /// the SPS and the 8x8 transform to the PPS.
    pub const fn high(&self) -> bool {
        matches!(self.profile, H264Profile::High)
    }

    /// Checks the picture size and macroblock rate against the level
    /// (ITU-T H.264, table A-1).
    ///
    /// # Errors
    ///
    /// Returns [`VaapiError::LevelExceeded`] when the stream does not fit.
    pub fn check_level(&self) -> Result<(), VaapiError> {
        let (max_frame_size, max_macroblock_rate) = level_limits(self.level);
        let width = u64::from(self.width_mbs());
        let height = u64::from(self.height_mbs());
        let frame_size = width * height;
        // Section A.3.1: the sides are bounded by sqrt(MaxFS * 8).
        let side_limit_squared = u64::from(max_frame_size) * 8;
        let rate = frame_size * u64::from(self.frame_rate.numerator().get());
        let fits = frame_size <= u64::from(max_frame_size)
            && width * width <= side_limit_squared
            && height * height <= side_limit_squared
            && rate
                <= u64::from(max_macroblock_rate) * u64::from(self.frame_rate.denominator().get());
        if fits {
            Ok(())
        } else {
            Err(VaapiError::LevelExceeded {
                level: self.level,
                width: self.width,
                height: self.height,
            })
        }
    }

    /// Returns the `time_scale` and `num_units_in_tick` of the VUI, where a
    /// frame lasts two ticks.
    ///
    /// # Errors
    ///
    /// Returns [`VaapiError::FrameRateOutOfRange`] when the time scale does
    /// not fit 32 bits.
    pub fn vui_timing(&self) -> Result<(u32, u32), VaapiError> {
        let time_scale = self
            .frame_rate
            .numerator()
            .get()
            .checked_mul(2)
            .ok_or(VaapiError::FrameRateOutOfRange(self.frame_rate))?;
        Ok((time_scale, self.frame_rate.denominator().get()))
    }

    /// Returns the cropping offsets, in samples, of the right and bottom
    /// edges that the macroblock-aligned coded picture exceeds the visible
    /// picture by.
    pub const fn crop(&self) -> (u32, u32) {
        (
            self.width_mbs() * MACROBLOCK - self.width,
            self.height_mbs() * MACROBLOCK - self.height,
        )
    }

    fn sps_rbsp(&self) -> Result<Vec<u8>, VaapiError> {
        let [profile_idc, constraints, level_idc] = self.profile_level();
        let (time_scale, num_units_in_tick) = self.vui_timing()?;
        let (crop_right, crop_bottom) = self.crop();
        let mut writer = BitWriter::new();
        writer.put_bits(u64::from(profile_idc), 8);
        writer.put_bits(u64::from(constraints), 8);
        writer.put_bits(u64::from(level_idc), 8);
        writer.put_ue(0); // seq_parameter_set_id
        if self.high() {
            writer.put_ue(1); // chroma_format_idc: 4:2:0
            writer.put_ue(0); // bit_depth_luma_minus8
            writer.put_ue(0); // bit_depth_chroma_minus8
            writer.put_flag(false); // qpprime_y_zero_transform_bypass_flag
            writer.put_flag(false); // seq_scaling_matrix_present_flag
        }
        writer.put_ue(LOG2_MAX_FRAME_NUM - 4);
        writer.put_ue(0); // pic_order_cnt_type
        writer.put_ue(LOG2_MAX_POC_LSB - 4);
        writer.put_ue(MAX_NUM_REF_FRAMES);
        writer.put_flag(false); // gaps_in_frame_num_value_allowed_flag
        writer.put_ue(self.width_mbs() - 1);
        writer.put_ue(self.height_mbs() - 1);
        writer.put_flag(true); // frame_mbs_only_flag
        writer.put_flag(true); // direct_8x8_inference_flag
        let cropped = crop_right != 0 || crop_bottom != 0;
        writer.put_flag(cropped); // frame_cropping_flag
        if cropped {
            // 4:2:0 frame coding: crop units are two samples.
            writer.put_ue(0);
            writer.put_ue(crop_right / 2);
            writer.put_ue(0);
            writer.put_ue(crop_bottom / 2);
        }
        writer.put_flag(true); // vui_parameters_present_flag
        writer.put_flag(false); // aspect_ratio_info_present_flag
        writer.put_flag(false); // overscan_info_present_flag
        writer.put_flag(true); // video_signal_type_present_flag
        writer.put_bits(VIDEO_FORMAT_UNSPECIFIED, 3);
        writer.put_flag(self.color.full_range);
        writer.put_flag(true); // colour_description_present_flag
        writer.put_bits(u64::from(self.color.primaries), 8);
        writer.put_bits(u64::from(self.color.transfer), 8);
        writer.put_bits(u64::from(self.color.matrix), 8);
        writer.put_flag(false); // chroma_loc_info_present_flag
        writer.put_flag(true); // timing_info_present_flag
        writer.put_bits(u64::from(num_units_in_tick), 32);
        writer.put_bits(u64::from(time_scale), 32);
        writer.put_flag(true); // fixed_frame_rate_flag
        writer.put_flag(false); // nal_hrd_parameters_present_flag
        writer.put_flag(false); // vcl_hrd_parameters_present_flag
        writer.put_flag(false); // pic_struct_present_flag
        writer.put_flag(true); // bitstream_restriction_flag
        writer.put_flag(true); // motion_vectors_over_pic_boundaries_flag
        writer.put_ue(0); // max_bytes_per_pic_denom
        writer.put_ue(0); // max_bits_per_mb_denom
        writer.put_ue(LOG2_MAX_MV_LENGTH);
        writer.put_ue(LOG2_MAX_MV_LENGTH);
        writer.put_ue(0); // max_num_reorder_frames
        writer.put_ue(MAX_DEC_FRAME_BUFFERING);
        Ok(writer.finish_rbsp())
    }

    fn pps_rbsp(&self) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.put_ue(0); // pic_parameter_set_id
        writer.put_ue(0); // seq_parameter_set_id
        writer.put_flag(self.cabac()); // entropy_coding_mode_flag
        writer.put_flag(false); // bottom_field_pic_order_in_frame_present_flag
        writer.put_ue(0); // num_slice_groups_minus1
        writer.put_ue(0); // num_ref_idx_l0_default_active_minus1
        writer.put_ue(0); // num_ref_idx_l1_default_active_minus1
        writer.put_flag(false); // weighted_pred_flag
        writer.put_bits(0, 2); // weighted_bipred_idc
        writer.put_se(i32::from(PIC_INIT_QP) - 26); // pic_init_qp_minus26
        writer.put_se(0); // pic_init_qs_minus26
        writer.put_se(0); // chroma_qp_index_offset
        writer.put_flag(true); // deblocking_filter_control_present_flag
        writer.put_flag(false); // constrained_intra_pred_flag
        writer.put_flag(false); // redundant_pic_cnt_present_flag
        if self.high() {
            writer.put_flag(true); // transform_8x8_mode_flag
            writer.put_flag(false); // pic_scaling_matrix_present_flag
            writer.put_se(0); // second_chroma_qp_index_offset
        }
        writer.finish_rbsp()
    }

    /// Returns `profile_idc`, the constraint flags byte and `level_idc`.
    pub const fn profile_level(&self) -> [u8; 3] {
        crate::H264Settings {
            profile: self.profile,
            level: self.level,
            bitstream: crate::BitstreamFormat::AnnexB,
        }
        .profile_level_id()
    }

    /// Returns the SPS NAL unit, without a start code.
    ///
    /// # Errors
    ///
    /// Returns [`VaapiError::FrameRateOutOfRange`] when the VUI timing does
    /// not fit.
    pub fn sps_nal(&self) -> Result<Vec<u8>, VaapiError> {
        Ok(nal_unit(3, NAL_SPS, &self.sps_rbsp()?))
    }

    /// Returns the PPS NAL unit, without a start code.
    pub fn pps_nal(&self) -> Vec<u8> {
        nal_unit(3, NAL_PPS, &self.pps_rbsp())
    }

    /// Returns the packed SPS.
    ///
    /// # Errors
    ///
    /// See [`Self::sps_nal`].
    pub fn packed_sps(&self) -> Result<PackedHeader, VaapiError> {
        Ok(packed_whole_bytes(&self.sps_nal()?))
    }

    /// Returns the packed PPS.
    pub fn packed_pps(&self) -> PackedHeader {
        packed_whole_bytes(&self.pps_nal())
    }

    /// Returns the SPS and PPS as one Annex B byte string.
    ///
    /// # Errors
    ///
    /// See [`Self::sps_nal`].
    pub fn annex_b_parameter_sets(&self) -> Result<Vec<u8>, VaapiError> {
        let mut data = Vec::new();
        for nal in [self.sps_nal()?, self.pps_nal()] {
            data.extend_from_slice(&START_CODE);
            data.extend_from_slice(&nal);
        }
        Ok(data)
    }

    /// Returns the packed slice header for `header`.
    ///
    /// The slice data follows the header bit-aligned, so the last byte is
    /// only partly meaningful and `bit_length` says how much.
    pub fn packed_slice_header(&self, header: &SliceHeader) -> PackedHeader {
        let mut writer = BitWriter::new();
        writer.put_ue(0); // first_mb_in_slice
        writer.put_ue(header.kind.header_slice_type());
        writer.put_ue(0); // pic_parameter_set_id
        writer.put_bits(u64::from(header.frame_num), LOG2_MAX_FRAME_NUM);
        if header.kind == FrameKind::Idr {
            writer.put_ue(header.idr_pic_id);
        }
        writer.put_bits(u64::from(header.poc_lsb), LOG2_MAX_POC_LSB);
        if header.kind == FrameKind::P {
            writer.put_flag(false); // num_ref_idx_active_override_flag
            writer.put_flag(false); // ref_pic_list_modification_flag_l0
        }
        // dec_ref_pic_marking(): every picture is a reference.
        // IDR: no_output_of_prior_pics_flag, then long_term_reference_flag;
        // otherwise adaptive_ref_pic_marking_mode_flag. All of them 0.
        writer.put_flag(false);
        if header.kind == FrameKind::Idr {
            writer.put_flag(false);
        }
        if self.cabac() && header.kind == FrameKind::P {
            writer.put_ue(0); // cabac_init_idc
        }
        writer.put_se(0); // slice_qp_delta
        writer.put_ue(0); // disable_deblocking_filter_idc
        writer.put_se(0); // slice_alpha_c0_offset_div2
        writer.put_se(0); // slice_beta_offset_div2

        let (reference_idc, unit_type) = header.kind.nal();
        let (payload, bits) = writer.into_parts();
        let mut data = START_CODE.to_vec();
        data.push((reference_idc << 5) | unit_type);
        data.extend_from_slice(&escape_emulation(&payload));
        let last_bits = bits % 8;
        let total = if last_bits == 0 {
            data.len() * 8
        } else {
            (data.len() - 1) * 8 + last_bits
        };
        PackedHeader {
            data,
            bit_length: u32::try_from(total).unwrap_or(u32::MAX),
        }
    }
}

fn nal_unit(reference_idc: u8, unit_type: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut nal = vec![(reference_idc << 5) | unit_type];
    nal.extend_from_slice(&escape_emulation(rbsp));
    nal
}

fn packed_whole_bytes(nal: &[u8]) -> PackedHeader {
    let mut data = START_CODE.to_vec();
    data.extend_from_slice(nal);
    PackedHeader {
        bit_length: u32::try_from(data.len() * 8).unwrap_or(u32::MAX),
        data,
    }
}

/// Returns `(MaxFS, MaxMBPS)` of `level`: the largest frame in macroblocks
/// and the largest macroblock rate (ITU-T H.264, table A-1).
const fn level_limits(level: H264Level) -> (u32, u32) {
    match level {
        H264Level::L3_0 => (1_620, 40_500),
        H264Level::L3_1 => (3_600, 108_000),
        H264Level::L3_2 => (5_120, 216_000),
        H264Level::L4_0 | H264Level::L4_1 => (8_192, 245_760),
        H264Level::L4_2 => (8_704, 522_240),
        H264Level::L5_0 => (22_080, 589_824),
        H264Level::L5_1 => (36_864, 983_040),
        H264Level::L5_2 => (36_864, 2_073_600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitstream::build_h264_avcc_from_annex_b;
    use std::num::NonZeroU32;

    /// Packs a string of `0`/`1` (other characters are layout) into bytes,
    /// zero padded.
    fn from_bits(bits: &str) -> Vec<u8> {
        let bits: Vec<bool> = bits
            .chars()
            .filter(|bit| matches!(bit, '0' | '1'))
            .map(|bit| bit == '1')
            .collect();
        bits.chunks(8)
            .map(|chunk| {
                chunk.iter().enumerate().fold(0, |byte, (index, bit)| {
                    byte | (u8::from(*bit) << (7 - index))
                })
            })
            .collect()
    }

    fn frame_rate(numerator: u32, denominator: u32) -> FrameRate {
        FrameRate::new(
            NonZeroU32::new(numerator).unwrap(),
            NonZeroU32::new(denominator).unwrap(),
        )
    }

    fn bt709() -> CicpColor {
        CicpColor {
            primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: false,
        }
    }

    fn parameters(profile: H264Profile) -> StreamParameters {
        StreamParameters {
            profile,
            level: H264Level::L3_1,
            width: 1280,
            height: 720,
            color: bt709(),
            frame_rate: frame_rate(30, 1),
        }
    }

    /// The SPS of 1280x720 Constrained Baseline level 3.1, field by field.
    const SPS_BASELINE_720P: &str = "
        01000010 11100000 00011111
        1 00101 1 00101 010 0
        0000001010000 00000101101
        1 1 0
        1
          0 0 1 101 0 1 00000001 00000001 00000001
          0
          1 00000000000000000000000000000001 00000000000000000000000000111100 1
          0 0 0
          1 1 1 1 000010000 000010000 1 010
        1";

    /// A NAL unit from its header byte and unescaped payload.
    fn nal(header: u8, payload: &[u8]) -> Vec<u8> {
        [&[header][..], &escape_emulation(payload)].concat()
    }

    #[test]
    fn sps_of_a_known_stream_matches_the_field_by_field_encoding() {
        let sps = parameters(H264Profile::ConstrainedBaseline)
            .sps_nal()
            .unwrap();
        assert_eq!(sps, nal(0x67, &from_bits(SPS_BASELINE_720P)));
        // The same bytes, written out; the two zero runs of the VUI timing
        // carry emulation prevention bytes.
        assert_eq!(
            sps,
            [
                0x67, 0x42, 0xE0, 0x1F, 0x96, 0x54, 0x02, 0x80, 0x2D, 0xD3, 0x50, 0x10, 0x10, 0x14,
                0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xF2, 0x3C, 0x20, 0x10, 0xA8
            ]
        );
    }

    #[test]
    fn pps_of_a_known_stream_matches_the_field_by_field_encoding() {
        // CAVLC, as Constrained Baseline.
        let baseline = parameters(H264Profile::ConstrainedBaseline).pps_nal();
        assert_eq!(
            baseline,
            nal(0x68, &from_bits("1 1 0 0 1 1 1 0 00 1 1 1 1 0 0 1"))
        );
        // CABAC, as Main.
        let main = parameters(H264Profile::Main).pps_nal();
        assert_eq!(
            main,
            nal(0x68, &from_bits("1 1 1 0 1 1 1 0 00 1 1 1 1 0 0 1"))
        );
        assert_eq!(main, [0x68, 0xEE, 0x3C, 0x80]);
        // High adds the 8x8 transform, no scaling matrix and the second
        // chroma offset.
        let high = parameters(H264Profile::High).pps_nal();
        assert_eq!(
            high,
            nal(0x68, &from_bits("1 1 1 0 1 1 1 0 00 1 1 1 1 0 0 1 0 1 1"))
        );
    }

    #[test]
    fn high_profile_sps_carries_the_chroma_format_fields() {
        let sps = parameters(H264Profile::High).sps_nal().unwrap();
        // profile_idc 100, no constraint flags, level 3.1, then after the
        // SPS id: chroma_format_idc 1, two bit depths of 0, two flags of 0.
        let bits = "01100100 00000000 00011111
             1 010 1 1 0 0
             00101 1 00101 010 0 0000001010000 00000101101 1 1 0
             1 0 0 1 101 0 1 00000001 00000001 00000001 0
             1 00000000000000000000000000000001 00000000000000000000000000111100 1
             0 0 0 1 1 1 1 000010000 000010000 1 010 1";
        assert_eq!(sps, nal(0x67, &from_bits(bits)));
    }

    #[test]
    fn sps_signals_cropping_and_the_colour_description() {
        let mut stream = parameters(H264Profile::Main);
        stream.width = 1366;
        stream.height = 768;
        stream.color = CicpColor {
            primaries: 9,
            transfer: 16,
            matrix: 9,
            full_range: true,
        };
        stream.frame_rate = frame_rate(30_000, 1001);
        assert_eq!(stream.crop(), (10, 0));
        let bits = format!(
            "01001101 00000000 00011111
             1 00101 1 00101 010 0
             0000001010110 00000110000
             1 1
             1 1 00110 1 1
             1
               0 0 1 101 1 1 {:08b} {:08b} {:08b}
               0
               1 {:032b} {:032b} 1
               0 0 0
               1 1 1 1 000010000 000010000 1 010
             1",
            9, 16, 9, 1001, 60_000
        );
        assert_eq!(stream.sps_nal().unwrap(), nal(0x67, &from_bits(&bits)));
    }

    #[test]
    fn sps_emulation_prevention_bytes_are_in_place() {
        let sps = parameters(H264Profile::ConstrainedBaseline)
            .sps_nal()
            .unwrap();
        assert!(sps.windows(3).any(|window| window == [0, 0, 3]));
        // Un-escaping gives the payload back.
        let mut payload = Vec::new();
        let mut zeros = 0;
        for &byte in &sps[1..] {
            if zeros >= 2 && byte == 3 {
                zeros = 0;
                continue;
            }
            zeros = if byte == 0 { zeros + 1 } else { 0 };
            payload.push(byte);
        }
        assert_eq!(payload, from_bits(SPS_BASELINE_720P));
    }

    #[test]
    fn packed_parameter_sets_carry_a_start_code_and_count_every_bit() {
        let stream = parameters(H264Profile::Main);
        let sps = stream.packed_sps().unwrap();
        assert_eq!(sps.data[..5], [0, 0, 0, 1, 0x67]);
        assert_eq!(sps.bit_length as usize, sps.data.len() * 8);
        let pps = stream.packed_pps();
        assert_eq!(pps.data, [0, 0, 0, 1, 0x68, 0xEE, 0x3C, 0x80]);
        assert_eq!(pps.bit_length, 64);

        let annex_b = stream.annex_b_parameter_sets().unwrap();
        assert_eq!([sps.data.as_slice(), pps.data.as_slice()].concat(), annex_b);
        let avcc = build_h264_avcc_from_annex_b(&annex_b).unwrap();
        assert_eq!(avcc[..6], [1, 0x4D, 0x00, 0x1F, 0xFF, 0xE1]);
        let sps_length = usize::from(u16::from_be_bytes([avcc[6], avcc[7]]));
        assert_eq!(avcc[8..8 + sps_length], annex_b[4..4 + sps_length]);
    }

    #[test]
    fn slice_headers_match_the_field_by_field_encoding() {
        let main = parameters(H264Profile::Main);
        let idr = main.packed_slice_header(&SliceHeader {
            kind: FrameKind::Idr,
            frame_num: 0,
            idr_pic_id: 0,
            poc_lsb: 0,
        });
        // first_mb, slice_type 7, pps 0, frame_num, idr_pic_id, poc lsb,
        // marking flags, qp delta, deblocking idc and offsets.
        let bits = "1 0001000 1 00000000 1 00000000 0 0 1 1 1 1";
        assert_eq!(idr.data[..5], [0, 0, 0, 1, 0x65]);
        assert_eq!(idr.data[5..], from_bits(bits));
        assert_eq!(idr.bit_length, 72);

        let p = main.packed_slice_header(&SliceHeader {
            kind: FrameKind::P,
            frame_num: 1,
            idr_pic_id: 0,
            poc_lsb: 2,
        });
        // slice_type 5, override and list modification flags, adaptive
        // marking flag, cabac_init_idc 0.
        let bits = "1 00110 1 00000001 00000010 0 0 0 1 1 1 1 1";
        assert_eq!(p.data[..5], [0, 0, 0, 1, 0x41]);
        assert_eq!(p.data[5..], from_bits(bits));
        assert_eq!(p.bit_length, 40 + 31);

        // CAVLC has no cabac_init_idc, one bit fewer.
        let baseline =
            parameters(H264Profile::ConstrainedBaseline).packed_slice_header(&SliceHeader {
                kind: FrameKind::P,
                frame_num: 1,
                idr_pic_id: 0,
                poc_lsb: 2,
            });
        assert_eq!(baseline.bit_length, 40 + 30);
    }

    #[test]
    fn slice_headers_never_need_emulation_prevention() {
        // The packed slice header goes to the driver with the slice data
        // following bit-aligned, so the header is only correct unescaped if no
        // `00 00 0x` (x <= 3) occurs in it, and none can form across its end:
        // after `00 00` the last byte must keep a header bit above the two low
        // bits the data may fill, and a byte-aligned end must not follow `00 00`.
        for profile in [
            H264Profile::ConstrainedBaseline,
            H264Profile::Main,
            H264Profile::High,
        ] {
            let stream = parameters(profile);
            for frame_num in 0..(1 << LOG2_MAX_FRAME_NUM) {
                for poc_lsb in 0..(1 << LOG2_MAX_POC_LSB) {
                    for (kind, idr_pic_id) in [
                        (FrameKind::P, 0),
                        (FrameKind::Idr, 0),
                        (FrameKind::Idr, 1),
                        (FrameKind::Idr, 255),
                        (FrameKind::Idr, 256),
                        (FrameKind::Idr, 65_535),
                    ] {
                        let header = stream.packed_slice_header(&SliceHeader {
                            kind,
                            frame_num,
                            idr_pic_id,
                            poc_lsb,
                        });
                        let bytes = &header.data[4..];
                        let context =
                            format!("{kind:?} {frame_num} {poc_lsb} {idr_pic_id}: {bytes:x?}");
                        assert!(
                            !bytes
                                .windows(3)
                                .any(|window| window[0] == 0 && window[1] == 0 && window[2] <= 3),
                            "{context}"
                        );
                        let (last, before) = bytes.split_last().unwrap();
                        if header.bit_length.is_multiple_of(8) {
                            assert!(!bytes.ends_with(&[0, 0]), "{context}");
                        } else if before.ends_with(&[0, 0]) {
                            assert!(last & 0xFC != 0, "{context}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn levels_bound_the_frame_size_and_the_macroblock_rate() {
        let check = |level, width, height, fps| {
            StreamParameters {
                level,
                width,
                height,
                frame_rate: frame_rate(fps, 1),
                ..parameters(H264Profile::High)
            }
            .check_level()
            .is_ok()
        };
        assert!(check(H264Level::L3_1, 1280, 720, 30));
        assert!(
            !check(H264Level::L3_1, 1280, 720, 31),
            "108000 MB/s at 3600 MB"
        );
        assert!(check(H264Level::L4_0, 1920, 1080, 30));
        assert!(!check(H264Level::L4_0, 1920, 1080, 60));
        assert!(check(H264Level::L4_2, 1920, 1080, 60));
        assert!(!check(H264Level::L5_1, 3840, 2160, 60));
        assert!(check(H264Level::L5_2, 3840, 2160, 60));
        assert!(
            !check(H264Level::L3_0, 1280, 720, 30),
            "3600 MB exceeds 1620"
        );
        // Narrow and tall fits the area but not sqrt(MaxFS * 8).
        assert!(!check(H264Level::L3_0, 16, 2400, 1));
        let ntsc = StreamParameters {
            frame_rate: frame_rate(30_000, 1001),
            ..parameters(H264Profile::High)
        };
        assert!(ntsc.check_level().is_ok());
    }

    #[test]
    fn vui_timing_counts_two_ticks_per_frame() {
        let ntsc = StreamParameters {
            frame_rate: frame_rate(30_000, 1001),
            ..parameters(H264Profile::Main)
        };
        assert_eq!(ntsc.vui_timing().unwrap(), (60_000, 1001));
        let huge = StreamParameters {
            frame_rate: frame_rate(u32::MAX, 1),
            ..parameters(H264Profile::Main)
        };
        assert!(matches!(
            huge.vui_timing(),
            Err(VaapiError::FrameRateOutOfRange(_))
        ));
        assert!(huge.sps_nal().is_err());
    }
}
