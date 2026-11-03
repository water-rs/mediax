//! Builders for small H.264 streams, for the tests and the fuzz targets.

use super::{
    dpb::{Marking, Mmco, Modification},
    slice::SliceKind,
};
use crate::annexb::START_CODE;
use h264_reader::nal::{
    NalHeader, WritableNal,
    sps::{
        BitstreamRestrictions, ChromaInfo, ConstraintFlags, FrameCropping, FrameMbsFlags,
        PicOrderCntType, SeqParamSetId, SeqParameterSet, VuiParameters,
    },
};

/// How a test stream codes the picture order count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestPoc {
    /// Type 0 with `log2_max_pic_order_cnt_lsb`.
    Zero(u8),
    /// Type 1 with `delta_pic_order_always_zero_flag` set: the offsets of
    /// the reference frame cycle, the non-reference offset and the
    /// top-to-bottom offset.
    One {
        /// `offset_for_ref_frame`.
        cycle: Vec<i32>,
        /// `offset_for_non_ref_pic`.
        non_ref: i32,
        /// `offset_for_top_to_bottom_field`.
        top_to_bottom: i32,
    },
    /// Type 2.
    Two,
}

/// The parameter sets of a test stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    /// `profile_idc`.
    pub profile_idc: u8,
    /// `level_idc`.
    pub level_idc: u8,
    /// The picture order count coding.
    pub poc: TestPoc,
    /// `log2_max_frame_num_minus4 + 4`.
    pub log2_max_frame_num: u8,
    /// `max_num_ref_frames`.
    pub max_num_ref_frames: u32,
    /// `gaps_in_frame_num_value_allowed_flag`.
    pub gaps_allowed: bool,
    /// The width in macroblocks.
    pub width_mbs: u32,
    /// The height in macroblocks.
    pub height_mbs: u32,
    /// `max_num_reorder_frames` and `max_dec_frame_buffering`.
    pub restrictions: Option<(u32, u32)>,
    /// The cropping offsets: left, right, top, bottom, in units of two samples.
    pub crop: Option<[u32; 4]>,
    /// `num_ref_idx_l0_default_active_minus1` and the list 1 value.
    pub default_ref_idx_minus1: [u32; 2],
    /// `entropy_coding_mode_flag`.
    pub cabac: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            profile_idc: 77,
            level_idc: 40,
            poc: TestPoc::Zero(8),
            log2_max_frame_num: 4,
            max_num_ref_frames: 4,
            gaps_allowed: false,
            width_mbs: 2,
            height_mbs: 2,
            restrictions: None,
            crop: None,
            default_ref_idx_minus1: [0, 0],
            cabac: false,
        }
    }
}

/// Appends syntax elements bit by bit.
#[derive(Debug, Default, Clone)]
pub struct Bits(Vec<bool>);

impl Bits {
    /// The number of bits written.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether nothing was written.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Writes a flag.
    pub fn bit(&mut self, value: bool) {
        self.0.push(value);
    }

    /// Writes `u(width)`.
    pub fn u(&mut self, width: u8, value: u32) {
        for shift in (0..width).rev() {
            self.0.push(value >> shift & 1 == 1);
        }
    }

    /// Writes `ue(v)`.
    pub fn ue(&mut self, value: u32) {
        let code = u64::from(value) + 1;
        let width = 64 - code.leading_zeros();
        for _ in 1..width {
            self.0.push(false);
        }
        for shift in (0..width).rev() {
            self.0.push(code >> shift & 1 == 1);
        }
    }

    /// Writes `se(v)`.
    pub fn se(&mut self, value: i32) {
        self.ue(if value > 0 {
            value.unsigned_abs() * 2 - 1
        } else {
            value.unsigned_abs() * 2
        });
    }

    /// Ends the bits with the RBSP trailing bits and packs them.
    #[must_use]
    pub fn into_rbsp(mut self) -> Vec<u8> {
        self.0.push(true);
        while !self.0.len().is_multiple_of(8) {
            self.0.push(false);
        }
        self.0
            .chunks(8)
            .map(|byte| byte.iter().fold(0, |acc, bit| acc << 1 | u8::from(*bit)))
            .collect()
    }
}

/// Inserts emulation prevention bytes into an RBSP and prepends the header.
#[must_use]
pub fn nal_unit(header: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut nal = vec![header];
    let mut zeros = 0;
    for &byte in rbsp {
        if zeros >= 2 && byte <= 3 {
            nal.push(3);
            zeros = 0;
        }
        nal.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    nal
}

/// Frames NAL units with four-byte start codes.
#[must_use]
pub fn annex_b(nals: &[Vec<u8>]) -> Vec<u8> {
    let mut stream = Vec::new();
    for nal in nals {
        stream.extend_from_slice(&START_CODE);
        stream.extend_from_slice(nal);
    }
    stream
}

impl StreamConfig {
    /// The sequence parameter set NAL unit.
    ///
    /// # Panics
    ///
    /// Panics when the configuration cannot be written.
    #[must_use]
    pub fn sps(&self) -> Vec<u8> {
        let pic_order_cnt = match &self.poc {
            TestPoc::Zero(log2) => PicOrderCntType::TypeZero {
                log2_max_pic_order_cnt_lsb_minus4: log2 - 4,
            },
            TestPoc::One {
                cycle,
                non_ref,
                top_to_bottom,
            } => PicOrderCntType::TypeOne {
                delta_pic_order_always_zero_flag: true,
                offset_for_non_ref_pic: *non_ref,
                offset_for_top_to_bottom_field: *top_to_bottom,
                offsets_for_ref_frame: cycle.clone(),
            },
            TestPoc::Two => PicOrderCntType::TypeTwo,
        };
        let sps = SeqParameterSet {
            profile_idc: self.profile_idc.into(),
            constraint_flags: ConstraintFlags::from(0),
            level_idc: self.level_idc,
            seq_parameter_set_id: SeqParamSetId::from_u32(0).unwrap(),
            chroma_info: ChromaInfo::default(),
            log2_max_frame_num_minus4: self.log2_max_frame_num - 4,
            pic_order_cnt,
            max_num_ref_frames: self.max_num_ref_frames,
            gaps_in_frame_num_value_allowed_flag: self.gaps_allowed,
            pic_width_in_mbs_minus1: self.width_mbs - 1,
            pic_height_in_map_units_minus1: self.height_mbs - 1,
            frame_mbs_flags: FrameMbsFlags::Frames,
            direct_8x8_inference_flag: true,
            frame_cropping: self.crop.map(|[left, right, top, bottom]| FrameCropping {
                left_offset: left,
                right_offset: right,
                top_offset: top,
                bottom_offset: bottom,
            }),
            vui_parameters: self.restrictions.map(|(reorder, buffering)| VuiParameters {
                bitstream_restrictions: Some(BitstreamRestrictions {
                    motion_vectors_over_pic_boundaries_flag: true,
                    max_bytes_per_pic_denom: 0,
                    max_bits_per_mb_denom: 0,
                    log2_max_mv_length_horizontal: 15,
                    log2_max_mv_length_vertical: 15,
                    max_num_reorder_frames: reorder,
                    max_dec_frame_buffering: buffering,
                }),
                ..VuiParameters::default()
            }),
        };
        sps.to_vec_with_header(NalHeader::new(0x67).unwrap())
    }

    /// The picture parameter set NAL unit.
    #[must_use]
    pub fn pps(&self) -> Vec<u8> {
        let mut bits = Bits::default();
        bits.ue(0);
        bits.ue(0);
        bits.bit(self.cabac);
        bits.bit(false);
        bits.ue(0);
        bits.ue(self.default_ref_idx_minus1[0]);
        bits.ue(self.default_ref_idx_minus1[1]);
        bits.bit(false);
        bits.u(2, 0);
        bits.se(0);
        bits.se(0);
        bits.se(0);
        bits.bit(true);
        bits.bit(false);
        bits.bit(false);
        nal_unit(0x68, &bits.into_rbsp())
    }

    /// The parameter sets in Annex B framing.
    #[must_use]
    pub fn parameter_sets(&self) -> Vec<u8> {
        annex_b(&[self.sps(), self.pps()])
    }

    /// The slice NAL unit of `spec`. The slice data is a few arbitrary bits.
    #[must_use]
    pub fn slice(&self, spec: &SliceSpec) -> Vec<u8> {
        let (nal, _) = self.slice_with_offset(spec);
        nal
    }

    /// The slice NAL unit and the number of RBSP bits of its header.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "writes the slice header syntax elements in order"
    )]
    pub fn slice_with_offset(&self, spec: &SliceSpec) -> (Vec<u8>, usize) {
        let mut bits = Bits::default();
        bits.ue(spec.first_mb);
        bits.ue(match spec.kind {
            SliceKind::P => 0,
            SliceKind::B => 1,
            SliceKind::I => 2,
        });
        bits.ue(0);
        bits.u(self.log2_max_frame_num, spec.frame_num);
        if spec.idr {
            bits.ue(spec.idr_pic_id);
        }
        match self.poc {
            TestPoc::Zero(log2) => bits.u(log2, spec.poc_lsb),
            TestPoc::One { .. } | TestPoc::Two => {}
        }
        if spec.kind == SliceKind::B {
            bits.bit(spec.direct_spatial);
        }
        if spec.kind != SliceKind::I {
            if let Some(minus1) = spec.num_ref_idx_override {
                bits.bit(true);
                bits.ue(minus1[0]);
                if spec.kind == SliceKind::B {
                    bits.ue(minus1[1]);
                }
            } else {
                bits.bit(false);
            }
            let lists = if spec.kind == SliceKind::B { 2 } else { 1 };
            for operations in &spec.modifications[..lists] {
                bits.bit(!operations.is_empty());
                if !operations.is_empty() {
                    for operation in operations {
                        match *operation {
                            Modification::Subtract {
                                abs_diff_pic_num_minus1,
                            } => {
                                bits.ue(0);
                                bits.ue(abs_diff_pic_num_minus1);
                            }
                            Modification::Add {
                                abs_diff_pic_num_minus1,
                            } => {
                                bits.ue(1);
                                bits.ue(abs_diff_pic_num_minus1);
                            }
                            Modification::LongTerm { long_term_pic_num } => {
                                bits.ue(2);
                                bits.ue(long_term_pic_num);
                            }
                        }
                    }
                    bits.ue(3);
                }
            }
        }
        if spec.nal_ref_idc != 0 {
            match &spec.marking {
                Some(Marking::Idr {
                    no_output_of_prior_pics,
                    long_term_reference,
                }) => {
                    bits.bit(*no_output_of_prior_pics);
                    bits.bit(*long_term_reference);
                }
                Some(Marking::Adaptive(operations)) => {
                    bits.bit(true);
                    for operation in operations {
                        match *operation {
                            Mmco::ShortTermUnused {
                                difference_of_pic_nums_minus1,
                            } => {
                                bits.ue(1);
                                bits.ue(difference_of_pic_nums_minus1);
                            }
                            Mmco::LongTermUnused { long_term_pic_num } => {
                                bits.ue(2);
                                bits.ue(long_term_pic_num);
                            }
                            Mmco::ShortToLong {
                                difference_of_pic_nums_minus1,
                                long_term_frame_idx,
                            } => {
                                bits.ue(3);
                                bits.ue(difference_of_pic_nums_minus1);
                                bits.ue(long_term_frame_idx);
                            }
                            Mmco::MaxLongTermIdx {
                                max_long_term_frame_idx_plus1,
                            } => {
                                bits.ue(4);
                                bits.ue(max_long_term_frame_idx_plus1);
                            }
                            Mmco::AllUnused => bits.ue(5),
                            Mmco::CurrentToLong {
                                long_term_frame_idx,
                            } => {
                                bits.ue(6);
                                bits.ue(long_term_frame_idx);
                            }
                        }
                    }
                    bits.ue(0);
                }
                Some(Marking::SlidingWindow) | None => bits.bit(false),
            }
        }
        if self.cabac && spec.kind != SliceKind::I {
            bits.ue(spec.cabac_init_idc);
        }
        bits.se(spec.slice_qp_delta);
        bits.ue(spec.disable_deblocking_filter_idc);
        if spec.disable_deblocking_filter_idc != 1 {
            bits.se(spec.alpha_c0_offset_div2);
            bits.se(spec.beta_offset_div2);
        }
        let header_bits = bits.len();
        // A few bits of slice data.
        bits.u(8, 0xA5);
        let header = (spec.nal_ref_idc << 5) | if spec.idr { 5 } else { 1 };
        (nal_unit(header, &bits.into_rbsp()), header_bits)
    }
}

/// What a test slice header says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceSpec {
    /// Whether the slice is of an IDR picture.
    pub idr: bool,
    /// `nal_ref_idc`.
    pub nal_ref_idc: u8,
    /// `first_mb_in_slice`.
    pub first_mb: u32,
    /// The slice type.
    pub kind: SliceKind,
    /// `frame_num`.
    pub frame_num: u32,
    /// `idr_pic_id`.
    pub idr_pic_id: u32,
    /// `pic_order_cnt_lsb`, for POC type 0.
    pub poc_lsb: u32,
    /// `direct_spatial_mv_pred_flag`.
    pub direct_spatial: bool,
    /// The `num_ref_idx_active_minus1` override of both lists.
    pub num_ref_idx_override: Option<[u32; 2]>,
    /// The modification operations of both lists.
    pub modifications: [Vec<Modification>; 2],
    /// `dec_ref_pic_marking`; ignored for non-reference pictures.
    pub marking: Option<Marking>,
    /// `cabac_init_idc`.
    pub cabac_init_idc: u32,
    /// `slice_qp_delta`.
    pub slice_qp_delta: i32,
    /// `disable_deblocking_filter_idc`.
    pub disable_deblocking_filter_idc: u32,
    /// `slice_alpha_c0_offset_div2`.
    pub alpha_c0_offset_div2: i32,
    /// `slice_beta_offset_div2`.
    pub beta_offset_div2: i32,
}

impl SliceSpec {
    /// An IDR picture's only slice.
    #[must_use]
    pub const fn idr() -> Self {
        Self {
            idr: true,
            nal_ref_idc: 3,
            first_mb: 0,
            kind: SliceKind::I,
            frame_num: 0,
            idr_pic_id: 0,
            poc_lsb: 0,
            direct_spatial: true,
            num_ref_idx_override: None,
            modifications: [Vec::new(), Vec::new()],
            marking: Some(Marking::Idr {
                no_output_of_prior_pics: false,
                long_term_reference: false,
            }),
            cabac_init_idc: 0,
            slice_qp_delta: 0,
            disable_deblocking_filter_idc: 0,
            alpha_c0_offset_div2: 0,
            beta_offset_div2: 0,
        }
    }

    /// A reference P picture.
    #[must_use]
    pub fn p(frame_num: u32, poc_lsb: u32) -> Self {
        Self {
            idr: false,
            kind: SliceKind::P,
            frame_num,
            poc_lsb,
            marking: Some(Marking::SlidingWindow),
            ..Self::idr()
        }
    }

    /// A non-reference B picture.
    #[must_use]
    pub fn b(frame_num: u32, poc_lsb: u32) -> Self {
        Self {
            idr: false,
            nal_ref_idc: 0,
            kind: SliceKind::B,
            frame_num,
            poc_lsb,
            marking: None,
            ..Self::idr()
        }
    }
}
