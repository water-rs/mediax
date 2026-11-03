//! Builders for small H.265 streams, for the tests and the fuzz targets.
//!
//! The slice data is a few arbitrary bytes: nothing here decodes samples.

use super::dpb::ShortTermRps;
use crate::rbsp::testing::nal_unit;
pub use crate::rbsp::testing::{Bits, annex_b};

/// How a short-term reference picture set is coded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpsCoding {
    /// Without prediction.
    Explicit(ShortTermRps),
    /// Predicted from an earlier set.
    Inter {
        /// `delta_idx_minus1`, coded in slice headers only.
        delta_idx_minus1: u32,
        /// `RefRpsIdx` delta order count: `(1 - 2 * delta_rps_sign) *
        /// (abs_delta_rps_minus1 + 1)`.
        delta_rps: i32,
        /// `used_by_curr_pic_flag` and `use_delta_flag` of each entry of the
        /// earlier set and of the picture itself.
        flags: Vec<(bool, bool)>,
    },
}

/// The scaling lists a stream codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingData {
    /// Every list predicted from the default.
    Defaults,
    /// Every list coded explicitly with all coefficients 16 and DC 8, then
    /// every matrix after the first predicted from its predecessor.
    Flat,
}

/// The parameter sets of a test stream.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "mirrors the syntax")]
pub struct StreamConfig {
    /// `general_profile_idc`.
    pub profile_idc: u8,
    /// `general_interlaced_source_flag`.
    pub interlaced: bool,
    /// The luma and chroma bit depth.
    pub bit_depth: u32,
    /// `chroma_format_idc`.
    pub chroma_format_idc: u32,
    /// The coded width in luma samples.
    pub width: u32,
    /// The coded height in luma samples.
    pub height: u32,
    /// The conformance window offsets in chroma units: left, right, top,
    /// bottom.
    pub crop: Option<[u32; 4]>,
    /// `CtbLog2SizeY`.
    pub log2_ctb: u32,
    /// `sps_max_dec_pic_buffering_minus1 + 1`.
    pub max_dec_pic_buffering: u32,
    /// `sps_max_num_reorder_pics`.
    pub max_num_reorder: u32,
    /// `sps_max_latency_increase_plus1`.
    pub max_latency_increase_plus1: u32,
    /// `log2_max_pic_order_cnt_lsb_minus4 + 4`.
    pub log2_max_poc_lsb: u32,
    /// The short-term reference picture sets of the SPS.
    pub short_term_sets: Vec<RpsCoding>,
    /// The long-term pictures of the SPS, when the stream uses them.
    pub long_term_sps: Option<Vec<(u32, bool)>>,
    /// `sps_temporal_mvp_enabled_flag`.
    pub temporal_mvp: bool,
    /// `sample_adaptive_offset_enabled_flag`.
    pub sao: bool,
    /// `scaling_list_enabled_flag` and the SPS lists.
    #[allow(
        clippy::option_option,
        reason = "disabled, enabled with the default lists, enabled with coded lists"
    )]
    pub scaling: Option<Option<ScalingData>>,
    /// The PPS lists.
    pub pps_scaling: Option<ScalingData>,
    /// `dependent_slice_segments_enabled_flag`.
    pub dependent_slices: bool,
    /// `output_flag_present_flag`.
    pub output_flag_present: bool,
    /// `num_extra_slice_header_bits`.
    pub extra_slice_header_bits: u32,
    /// `cabac_init_present_flag`.
    pub cabac_init_present: bool,
    /// `num_ref_idx_lX_default_active_minus1`.
    pub default_ref_idx_minus1: [u32; 2],
    /// `pps_slice_chroma_qp_offsets_present_flag`.
    pub slice_chroma_qp_offsets: bool,
    /// `weighted_pred_flag`.
    pub weighted_pred: bool,
    /// `weighted_bipred_flag`.
    pub weighted_bipred: bool,
    /// Uniformly spaced tile columns and rows.
    pub tiles: Option<(u32, u32)>,
    /// `entropy_coding_sync_enabled_flag`.
    pub entropy_sync: bool,
    /// `deblocking_filter_override_enabled_flag`.
    pub deblocking_override: bool,
    /// `lists_modification_present_flag`.
    pub lists_modification: bool,
    /// `slice_segment_header_extension_present_flag`.
    pub header_extension: bool,
    /// The extension flags of the PPS, set to make it unsupported.
    pub pps_range_extension: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            profile_idc: 1,
            interlaced: false,
            bit_depth: 8,
            chroma_format_idc: 1,
            width: 64,
            height: 64,
            crop: None,
            log2_ctb: 4,
            max_dec_pic_buffering: 5,
            max_num_reorder: 2,
            max_latency_increase_plus1: 0,
            log2_max_poc_lsb: 8,
            short_term_sets: Vec::new(),
            long_term_sps: None,
            temporal_mvp: false,
            sao: false,
            scaling: None,
            pps_scaling: None,
            dependent_slices: false,
            output_flag_present: false,
            extra_slice_header_bits: 0,
            cabac_init_present: false,
            default_ref_idx_minus1: [0, 0],
            slice_chroma_qp_offsets: false,
            weighted_pred: false,
            weighted_bipred: false,
            tiles: None,
            entropy_sync: false,
            deblocking_override: false,
            lists_modification: false,
            header_extension: false,
            pps_range_extension: false,
        }
    }
}

/// The two bytes of a NAL unit header.
#[must_use]
pub const fn nal_header(kind: u8, temporal_id: u8) -> [u8; 2] {
    [kind << 1, temporal_id + 1]
}

fn profile_tier_level(bits: &mut Bits, profile_idc: u8, interlaced: bool) {
    bits.u(2, 0);
    bits.bit(false);
    bits.u(5, u32::from(profile_idc));
    bits.u(32, 1_u32 << (31 - u32::from(profile_idc)));
    bits.bit(!interlaced);
    bits.bit(interlaced);
    bits.bit(false);
    bits.bit(true);
    bits.u(32, 0);
    bits.u(12, 0);
    bits.u(8, 120);
}

fn write_scaling(bits: &mut Bits, data: ScalingData) {
    for size_id in 0..4_u32 {
        let matrices = if size_id == 3 { 2 } else { 6 };
        for matrix in 0..matrices {
            match data {
                ScalingData::Defaults => {
                    bits.bit(false);
                    bits.ue(0);
                }
                ScalingData::Flat if matrix > 0 => {
                    bits.bit(false);
                    bits.ue(1);
                }
                ScalingData::Flat => {
                    bits.bit(true);
                    let count = 64.min(1_u32 << (4 + (size_id << 1)));
                    if size_id > 1 {
                        bits.se(0);
                    }
                    // From the DC value 8 (or the initial 8) up to 16, then
                    // unchanged.
                    bits.se(8);
                    for _ in 1..count {
                        bits.se(0);
                    }
                }
            }
        }
    }
}

fn write_rps(bits: &mut Bits, index: usize, coding: &RpsCoding, in_slice: bool) {
    if index != 0 {
        bits.bit(matches!(coding, RpsCoding::Inter { .. }));
    }
    match coding {
        RpsCoding::Explicit(set) => {
            bits.ue(u32::try_from(set.negative.len()).unwrap());
            bits.ue(u32::try_from(set.positive.len()).unwrap());
            let mut previous = 0;
            for entry in &set.negative {
                bits.ue(u32::try_from(previous - entry.delta_poc - 1).unwrap());
                bits.bit(entry.used_by_curr);
                previous = entry.delta_poc;
            }
            previous = 0;
            for entry in &set.positive {
                bits.ue(u32::try_from(entry.delta_poc - previous - 1).unwrap());
                bits.bit(entry.used_by_curr);
                previous = entry.delta_poc;
            }
        }
        RpsCoding::Inter {
            delta_idx_minus1,
            delta_rps,
            flags,
        } => {
            if in_slice {
                bits.ue(*delta_idx_minus1);
            }
            bits.bit(*delta_rps < 0);
            bits.ue(delta_rps.unsigned_abs() - 1);
            for &(used, use_delta) in flags {
                bits.bit(used);
                if !used {
                    bits.bit(use_delta);
                }
            }
        }
    }
}

fn ceil_log2(value: u32) -> u8 {
    u8::try_from(value.saturating_sub(1).bit_width()).unwrap()
}

impl StreamConfig {
    /// The video parameter set NAL unit.
    #[must_use]
    pub fn vps(&self) -> Vec<u8> {
        let mut bits = Bits::default();
        bits.u(4, 0);
        bits.u(2, 3);
        bits.u(6, 0);
        bits.u(3, 0);
        bits.bit(true);
        bits.u(16, 0xffff);
        profile_tier_level(&mut bits, self.profile_idc, self.interlaced);
        bits.bit(true);
        bits.ue(self.max_dec_pic_buffering - 1);
        bits.ue(self.max_num_reorder);
        bits.ue(self.max_latency_increase_plus1);
        nal_unit(&nal_header(32, 0), &bits.into_rbsp())
    }

    /// The sequence parameter set NAL unit.
    #[must_use]
    pub fn sps(&self) -> Vec<u8> {
        let mut bits = Bits::default();
        bits.u(4, 0);
        bits.u(3, 0);
        bits.bit(true);
        profile_tier_level(&mut bits, self.profile_idc, self.interlaced);
        bits.ue(0);
        bits.ue(self.chroma_format_idc);
        if self.chroma_format_idc == 3 {
            bits.bit(false);
        }
        bits.ue(self.width);
        bits.ue(self.height);
        bits.bit(self.crop.is_some());
        for offset in self.crop.into_iter().flatten() {
            bits.ue(offset);
        }
        bits.ue(self.bit_depth - 8);
        bits.ue(self.bit_depth - 8);
        bits.ue(self.log2_max_poc_lsb - 4);
        bits.bit(true);
        bits.ue(self.max_dec_pic_buffering - 1);
        bits.ue(self.max_num_reorder);
        bits.ue(self.max_latency_increase_plus1);
        // Minimum coding block 8x8, transform blocks from 4x4 up to the
        // largest the coding tree block allows.
        bits.ue(0);
        bits.ue(self.log2_ctb - 3);
        bits.ue(0);
        bits.ue(self.log2_ctb.min(5) - 2);
        bits.ue(1);
        bits.ue(1);
        bits.bit(self.scaling.is_some());
        if let Some(lists) = self.scaling {
            bits.bit(lists.is_some());
            if let Some(data) = lists {
                write_scaling(&mut bits, data);
            }
        }
        bits.bit(false);
        bits.bit(self.sao);
        bits.bit(false);
        bits.ue(u32::try_from(self.short_term_sets.len()).unwrap());
        for (index, set) in self.short_term_sets.iter().enumerate() {
            write_rps(&mut bits, index, set, false);
        }
        bits.bit(self.long_term_sps.is_some());
        if let Some(entries) = &self.long_term_sps {
            bits.ue(u32::try_from(entries.len()).unwrap());
            for &(lsb, used) in entries {
                bits.u(u8::try_from(self.log2_max_poc_lsb).unwrap(), lsb);
                bits.bit(used);
            }
        }
        bits.bit(self.temporal_mvp);
        bits.bit(false);
        bits.bit(false);
        bits.bit(false);
        nal_unit(&nal_header(33, 0), &bits.into_rbsp())
    }

    /// The picture parameter set NAL unit.
    #[must_use]
    pub fn pps(&self) -> Vec<u8> {
        let mut bits = Bits::default();
        bits.ue(0);
        bits.ue(0);
        bits.bit(self.dependent_slices);
        bits.bit(self.output_flag_present);
        bits.u(3, self.extra_slice_header_bits);
        bits.bit(false);
        bits.bit(self.cabac_init_present);
        bits.ue(self.default_ref_idx_minus1[0]);
        bits.ue(self.default_ref_idx_minus1[1]);
        bits.se(0);
        bits.bit(false);
        bits.bit(false);
        bits.bit(false);
        bits.se(0);
        bits.se(0);
        bits.bit(self.slice_chroma_qp_offsets);
        bits.bit(self.weighted_pred);
        bits.bit(self.weighted_bipred);
        bits.bit(false);
        bits.bit(self.tiles.is_some());
        bits.bit(self.entropy_sync);
        if let Some((columns, rows)) = self.tiles {
            bits.ue(columns - 1);
            bits.ue(rows - 1);
            bits.bit(true);
            bits.bit(true);
        }
        bits.bit(true);
        bits.bit(true);
        bits.bit(self.deblocking_override);
        bits.bit(false);
        bits.se(0);
        bits.se(0);
        bits.bit(self.pps_scaling.is_some());
        if let Some(data) = self.pps_scaling {
            write_scaling(&mut bits, data);
        }
        bits.bit(self.lists_modification);
        bits.ue(0);
        bits.bit(self.header_extension);
        bits.bit(self.pps_range_extension);
        if self.pps_range_extension {
            bits.bit(true);
            bits.u(7, 0);
        }
        nal_unit(&nal_header(34, 0), &bits.into_rbsp())
    }

    /// The three parameter sets, framed.
    #[must_use]
    pub fn parameter_sets(&self) -> Vec<Vec<u8>> {
        vec![self.vps(), self.sps(), self.pps()]
    }

    /// `PicSizeInCtbsY`.
    #[must_use]
    pub const fn size_in_ctbs(&self) -> u32 {
        let ctb = 1 << self.log2_ctb;
        self.width.div_ceil(ctb) * self.height.div_ceil(ctb)
    }
}

/// How a slice codes its short-term reference picture set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SliceRps {
    /// By index into the sequence's sets.
    Sps(u32),
    /// In the header.
    Inline(RpsCoding),
}

/// A long-term picture of a slice header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongTermSpec {
    /// By index into the sequence's list.
    Sps {
        /// `lt_idx_sps`.
        index: u32,
        /// `delta_poc_msb_cycle_lt`, as coded.
        msb_cycle: Option<u32>,
    },
    /// In the header.
    Explicit {
        /// `poc_lsb_lt`.
        poc_lsb: u32,
        /// `used_by_curr_pic_lt_flag`.
        used: bool,
        /// `delta_poc_msb_cycle_lt`, as coded.
        msb_cycle: Option<u32>,
    },
}

/// A slice segment of a test stream.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "mirrors the syntax")]
pub struct SliceSpec {
    /// `nal_unit_type`.
    pub nal_unit_type: u8,
    /// `TemporalId`.
    pub temporal_id: u8,
    /// `first_slice_segment_in_pic_flag`.
    pub first: bool,
    /// `no_output_of_prior_pics_flag`.
    pub no_output_of_prior_pics: bool,
    /// `dependent_slice_segment_flag`.
    pub dependent: bool,
    /// `slice_segment_address`.
    pub address: u32,
    /// `slice_type`: 0 B, 1 P, 2 I.
    pub slice_type: u32,
    /// `pic_output_flag`.
    pub pic_output: bool,
    /// `slice_pic_order_cnt_lsb`.
    pub poc_lsb: u32,
    /// The short-term reference picture set.
    pub rps: SliceRps,
    /// The long-term pictures, those from the SPS first.
    pub long_term: Vec<LongTermSpec>,
    /// `slice_temporal_mvp_enabled_flag`.
    pub temporal_mvp: bool,
    /// `num_ref_idx_lX_active_minus1`, when overridden.
    pub active_override_minus1: Option<[u32; 2]>,
    /// The `list_entry_lX` values.
    pub list_entries: [Option<Vec<u32>>; 2],
    /// `collocated_from_l0_flag`.
    pub collocated_from_l0: bool,
    /// `collocated_ref_idx`.
    pub collocated_ref_idx: u32,
    /// `slice_qp_delta`.
    pub qp_delta: i32,
    /// `slice_deblocking_filter_disabled_flag`, when overridden.
    pub deblocking_override: Option<bool>,
    /// The entry point offsets, as `offset_len_minus1 + 1` bits wide.
    pub entry_points: Vec<u32>,
    /// The slice segment data.
    pub data: Vec<u8>,
}

impl SliceSpec {
    /// A slice of `kind` (0 B, 1 P, 2 I) that is the first of its picture.
    #[must_use]
    pub fn new(nal_unit_type: u8, slice_type: u32, poc_lsb: u32) -> Self {
        Self {
            nal_unit_type,
            temporal_id: 0,
            first: true,
            no_output_of_prior_pics: false,
            dependent: false,
            address: 0,
            slice_type,
            pic_output: true,
            poc_lsb,
            rps: SliceRps::Inline(RpsCoding::Explicit(ShortTermRps::default())),
            long_term: Vec::new(),
            temporal_mvp: false,
            active_override_minus1: None,
            list_entries: [None, None],
            collocated_from_l0: true,
            collocated_ref_idx: 0,
            qp_delta: 0,
            deblocking_override: None,
            entry_points: Vec::new(),
            data: vec![0xAB, 0xCD, 0xEF],
        }
    }

    /// An IDR picture (`IDR_W_RADL`).
    #[must_use]
    pub fn idr() -> Self {
        Self::new(19, 2, 0)
    }

    /// The NAL unit, written for `config`.
    ///
    /// # Panics
    ///
    /// Panics when the specification cannot be written.
    #[must_use]
    #[allow(clippy::too_many_lines, reason = "mirrors the syntax")]
    pub fn nal(&self, config: &StreamConfig) -> Vec<u8> {
        let mut bits = Bits::default();
        let irap = (16..=23).contains(&self.nal_unit_type);
        let idr = matches!(self.nal_unit_type, 19 | 20);
        bits.bit(self.first);
        if irap {
            bits.bit(self.no_output_of_prior_pics);
        }
        bits.ue(0);
        if !self.first {
            if config.dependent_slices {
                bits.bit(self.dependent);
            }
            bits.u(ceil_log2(config.size_in_ctbs()), self.address);
        }
        if !self.dependent {
            bits.u(u8::try_from(config.extra_slice_header_bits).unwrap(), 0);
            bits.ue(self.slice_type);
            if config.output_flag_present {
                bits.bit(self.pic_output);
            }
            let mut total_curr = 0;
            if !idr {
                bits.u(u8::try_from(config.log2_max_poc_lsb).unwrap(), self.poc_lsb);
                match &self.rps {
                    SliceRps::Sps(index) => {
                        bits.bit(true);
                        let count = u32::try_from(config.short_term_sets.len()).unwrap();
                        if count > 1 {
                            bits.u(ceil_log2(count), *index);
                        }
                        if let Some(RpsCoding::Explicit(set)) =
                            config.short_term_sets.get(*index as usize)
                        {
                            total_curr += set.used_count();
                        }
                    }
                    SliceRps::Inline(coding) => {
                        bits.bit(false);
                        write_rps(&mut bits, config.short_term_sets.len(), coding, true);
                        if let RpsCoding::Explicit(set) = coding {
                            total_curr += set.used_count();
                        }
                    }
                }
                if let Some(table) = &config.long_term_sps {
                    let in_sps = self
                        .long_term
                        .iter()
                        .filter(|entry| matches!(entry, LongTermSpec::Sps { .. }))
                        .count();
                    if !table.is_empty() {
                        bits.ue(u32::try_from(in_sps).unwrap());
                    }
                    bits.ue(u32::try_from(self.long_term.len() - in_sps).unwrap());
                    for entry in &self.long_term {
                        let msb_cycle = match *entry {
                            LongTermSpec::Sps { index, msb_cycle } => {
                                if table.len() > 1 {
                                    bits.u(ceil_log2(u32::try_from(table.len()).unwrap()), index);
                                }
                                total_curr += usize::from(table[index as usize].1);
                                msb_cycle
                            }
                            LongTermSpec::Explicit {
                                poc_lsb,
                                used,
                                msb_cycle,
                            } => {
                                bits.u(u8::try_from(config.log2_max_poc_lsb).unwrap(), poc_lsb);
                                bits.bit(used);
                                total_curr += usize::from(used);
                                msb_cycle
                            }
                        };
                        bits.bit(msb_cycle.is_some());
                        if let Some(cycle) = msb_cycle {
                            bits.ue(cycle);
                        }
                    }
                }
                if config.temporal_mvp {
                    bits.bit(self.temporal_mvp);
                }
            }
            if config.sao {
                bits.bit(true);
                if config.chroma_format_idc != 0 {
                    bits.bit(true);
                }
            }
            let mut active = [0_u32; 2];
            if self.slice_type != 2 {
                let lists = if self.slice_type == 0 { 2 } else { 1 };
                bits.bit(self.active_override_minus1.is_some());
                for (count, default) in active
                    .iter_mut()
                    .zip(config.default_ref_idx_minus1)
                    .take(lists)
                {
                    *count = default + 1;
                }
                if let Some(minus1) = self.active_override_minus1 {
                    for (count, minus1) in active.iter_mut().zip(minus1).take(lists) {
                        bits.ue(minus1);
                        *count = minus1 + 1;
                    }
                }
                if config.lists_modification && total_curr > 1 {
                    for list in 0..lists {
                        bits.bit(self.list_entries[list].is_some());
                        if let Some(entries) = &self.list_entries[list] {
                            for &entry in entries {
                                bits.u(ceil_log2(u32::try_from(total_curr).unwrap()), entry);
                            }
                        }
                    }
                }
                if self.slice_type == 0 {
                    bits.bit(false);
                }
                if config.cabac_init_present {
                    bits.bit(false);
                }
                if self.temporal_mvp {
                    if self.slice_type == 0 {
                        bits.bit(self.collocated_from_l0);
                    }
                    let list = usize::from(!self.collocated_from_l0 && self.slice_type == 0);
                    if active[list] > 1 {
                        bits.ue(self.collocated_ref_idx);
                    }
                }
                if (config.weighted_pred && self.slice_type == 1)
                    || (config.weighted_bipred && self.slice_type == 0)
                {
                    let chroma = config.chroma_format_idc != 0;
                    bits.ue(2);
                    if chroma {
                        bits.se(0);
                    }
                    for &count in active.iter().take(lists) {
                        for index in 0..count {
                            bits.bit(index % 2 == 0);
                        }
                        if chroma {
                            for index in 0..count {
                                bits.bit(index == 0);
                            }
                        }
                        for index in 0..count {
                            if index % 2 == 0 {
                                bits.se(i32::try_from(index).unwrap() + 1);
                                bits.se(2 * i32::try_from(index).unwrap());
                            }
                            if chroma && index == 0 {
                                for _ in 0..2 {
                                    bits.se(1);
                                    bits.se(3);
                                }
                            }
                        }
                    }
                }
                bits.ue(1);
            }
            bits.se(self.qp_delta);
            if config.slice_chroma_qp_offsets {
                bits.se(0);
                bits.se(0);
            }
            if config.deblocking_override {
                bits.bit(self.deblocking_override.is_some());
                if let Some(disabled) = self.deblocking_override {
                    bits.bit(disabled);
                    if !disabled {
                        bits.se(1);
                        bits.se(-1);
                    }
                }
            }
            let deblocking_disabled = self.deblocking_override == Some(true);
            if config.sao || !deblocking_disabled {
                bits.bit(true);
            }
        }
        if config.tiles.is_some() || config.entropy_sync {
            bits.ue(u32::try_from(self.entry_points.len()).unwrap());
            if !self.entry_points.is_empty() {
                bits.ue(15);
                for &offset in &self.entry_points {
                    bits.u(16, offset);
                }
            }
        }
        if config.header_extension {
            bits.ue(3);
            bits.u(24, 0);
        }
        let mut rbsp = bits.into_rbsp();
        rbsp.extend_from_slice(&self.data);
        nal_unit(&nal_header(self.nal_unit_type, self.temporal_id), &rbsp)
    }
}
