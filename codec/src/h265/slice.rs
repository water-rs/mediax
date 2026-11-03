//! Slice segment headers (7.3.6) and the parameter sets they refer to.

use super::{
    dpb::{ListKind, LongTermRef, ReferencePictureSet},
    error::H265Error,
    nal::{NalHeader, NalUnitType},
    params::{Pps, Sps, Vps, parse_short_term_rps},
    sequence::check_pps,
    syntax::{Syntax, escaped_prefix, narrow},
};
use std::{collections::HashMap, sync::Arc};

/// The most reference pictures a list holds (`num_ref_idx_lX_active_minus1`
/// is at most 14).
pub const MAX_REFS: usize = 15;

/// The kind of a slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceType {
    /// A bi-predictive slice (`slice_type` 0).
    B,
    /// A predictive slice (1).
    P,
    /// An intra slice (2).
    I,
}

impl SliceType {
    /// The list kind of an inter slice.
    #[must_use]
    pub const fn list_kind(self) -> Option<ListKind> {
        match self {
            Self::B => Some(ListKind::B),
            Self::P => Some(ListKind::P),
            Self::I => None,
        }
    }
}

/// The weights of one reference list, as the hardware takes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ListWeights {
    /// `delta_luma_weight_lX`.
    pub delta_luma_weight: [i8; MAX_REFS],
    /// `luma_offset_lX`.
    pub luma_offset: [i8; MAX_REFS],
    /// `delta_chroma_weight_lX`.
    pub delta_chroma_weight: [[i8; 2]; MAX_REFS],
    /// `ChromaOffsetLX`.
    pub chroma_offset: [[i8; 2]; MAX_REFS],
}

/// The prediction weight table of a slice (7.3.6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredWeights {
    /// `luma_log2_weight_denom`.
    pub luma_log2_denom: u8,
    /// `delta_chroma_log2_weight_denom`.
    pub delta_chroma_log2_denom: i8,
    /// The weights of lists 0 and 1.
    pub lists: [ListWeights; 2],
}

/// A slice segment header.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the syntax elements one to one"
)]
pub struct SliceHeader {
    /// The NAL unit type.
    pub nal_unit_type: NalUnitType,
    /// `TemporalId`.
    pub temporal_id: u8,
    /// `first_slice_segment_in_pic_flag`.
    pub first_slice_segment_in_pic: bool,
    /// `no_output_of_prior_pics_flag`.
    pub no_output_of_prior_pics: bool,
    /// `slice_pic_parameter_set_id`.
    pub pps_id: u8,
    /// `dependent_slice_segment_flag`.
    pub dependent: bool,
    /// `slice_segment_address`.
    pub segment_address: u32,
    /// `slice_type`.
    pub slice_type: SliceType,
    /// `pic_output_flag`.
    pub pic_output: bool,
    /// `slice_pic_order_cnt_lsb`.
    pub poc_lsb: u32,
    /// The reference picture set.
    pub rps: ReferencePictureSet,
    /// The bits the inline short-term reference picture set takes, 0 when the
    /// slice refers to one of the sequence's.
    pub st_rps_bits: u32,
    /// `slice_temporal_mvp_enabled_flag`.
    pub temporal_mvp: bool,
    /// `slice_sao_luma_flag`.
    pub sao_luma: bool,
    /// `slice_sao_chroma_flag`.
    pub sao_chroma: bool,
    /// `num_ref_idx_l0_active_minus1 + 1` and the list 1 value, 0 for lists
    /// the slice does not have.
    pub num_ref_idx_active: [usize; 2],
    /// The `list_entry_lX` values, when the lists are modified.
    pub list_entries: [Option<Vec<u32>>; 2],
    /// `mvd_l1_zero_flag`.
    pub mvd_l1_zero: bool,
    /// `cabac_init_flag`.
    pub cabac_init: bool,
    /// `collocated_from_l0_flag`.
    pub collocated_from_l0: bool,
    /// `collocated_ref_idx`.
    pub collocated_ref_idx: u8,
    /// The prediction weights, when the slice has them.
    pub weights: Option<PredWeights>,
    /// `five_minus_max_num_merge_cand`.
    pub five_minus_max_num_merge_cand: u8,
    /// `slice_qp_delta`.
    pub qp_delta: i8,
    /// `slice_cb_qp_offset`.
    pub cb_qp_offset: i8,
    /// `slice_cr_qp_offset`.
    pub cr_qp_offset: i8,
    /// `slice_deblocking_filter_disabled_flag`.
    pub deblocking_disabled: bool,
    /// `slice_beta_offset_div2`.
    pub beta_offset_div2: i8,
    /// `slice_tc_offset_div2`.
    pub tc_offset_div2: i8,
    /// `slice_loop_filter_across_slices_enabled_flag`.
    pub loop_filter_across_slices: bool,
    /// `num_entry_point_offsets`.
    pub num_entry_points: u32,
}

impl SliceHeader {
    /// The part of the header that picture management reads.
    #[must_use]
    pub fn picture_header(&self) -> super::dpb::PictureHeader {
        super::dpb::PictureHeader {
            nal_unit_type: self.nal_unit_type,
            temporal_id: self.temporal_id,
            poc_lsb: self.poc_lsb,
            no_output_of_prior_pics: self.no_output_of_prior_pics,
            pic_output: self.pic_output,
            rps: self.rps.clone(),
        }
    }
}

/// A parsed slice segment.
#[derive(Debug, Clone)]
pub struct Slice {
    /// The header.
    pub header: SliceHeader,
    /// The picture parameter set the slice refers to.
    pub pps: Arc<Pps>,
    /// The sequence parameter set of the picture parameter set.
    pub sps: Arc<Sps>,
    /// Where the slice segment data starts in the NAL unit with its emulation
    /// prevention bytes removed, counting the NAL unit header.
    pub data_byte_offset: usize,
    /// The emulation prevention bytes in the header, the NAL unit header
    /// included.
    pub emulation_prevention_bytes: usize,
}

/// The parameter sets received so far.
#[derive(Debug, Default)]
pub struct ParameterSets {
    vps: HashMap<u8, Vps>,
    sps: HashMap<u8, Arc<Sps>>,
    pps: HashMap<u8, Arc<Pps>>,
}

impl ParameterSets {
    /// Stores a video parameter set.
    ///
    /// # Errors
    ///
    /// Returns an [`H265Error`] when it does not parse.
    pub fn put_vps(&mut self, nal: &[u8]) -> Result<(), H265Error> {
        let vps = Vps::parse(nal)?;
        self.vps.insert(vps.id, vps);
        Ok(())
    }

    /// Stores a sequence parameter set.
    ///
    /// # Errors
    ///
    /// Returns an [`H265Error`] when it does not parse.
    pub fn put_sps(&mut self, nal: &[u8]) -> Result<Arc<Sps>, H265Error> {
        let sps = Arc::new(Sps::parse(nal)?);
        self.sps.insert(sps.id, Arc::clone(&sps));
        Ok(sps)
    }

    /// Stores a picture parameter set.
    ///
    /// # Errors
    ///
    /// Returns an [`H265Error`] when it does not parse.
    pub fn put_pps(&mut self, nal: &[u8]) -> Result<(), H265Error> {
        let pps = Pps::parse(nal)?;
        self.pps.insert(pps.id, Arc::new(pps));
        Ok(())
    }

    /// Reads the slice segment header of `nal`. A dependent slice segment
    /// takes the fields it does not code from `previous`, the header of the
    /// slice segment before it in the picture.
    ///
    /// # Errors
    ///
    /// Returns an [`H265Error`] when the header does not parse or refers to a
    /// parameter set that was not received.
    #[cfg(any(test, feature = "fuzzing"))]
    pub fn slice(&self, nal: &[u8], previous: Option<&SliceHeader>) -> Result<Slice, H265Error> {
        self.slice_if(nal, previous, |_| Ok(()))
    }

    /// Reads the slice segment header of `nal`, first asking `supported` about
    /// the sequence parameter set the slice resolves to: an error from it is
    /// returned before the rest of the header is read. A dependent slice
    /// segment takes the fields it does not code from `previous`.
    ///
    /// # Errors
    ///
    /// Returns an [`H265Error`] when the header does not parse or refers to a
    /// parameter set that was not received, and whatever `supported` returns.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the syntax, each element checked where it is read"
    )]
    pub fn slice_if(
        &self,
        nal: &[u8],
        previous: Option<&SliceHeader>,
        supported: impl FnOnce(&Sps) -> Result<(), H265Error>,
    ) -> Result<Slice, H265Error> {
        let nal_header = NalHeader::parse(nal)?;
        let nal_unit_type = nal_header.unit_type();
        if !nal_unit_type.is_decodable_slice() {
            return Err(H265Error::Invalid("not a slice segment NAL unit"));
        }
        let mut syntax = Syntax::new(nal, "slice segment header");
        let first_slice_segment_in_pic = syntax.flag("first_slice_segment_in_pic_flag")?;
        let no_output_of_prior_pics = if nal_unit_type.is_irap() {
            syntax.flag("no_output_of_prior_pics_flag")?
        } else {
            false
        };
        let pps_id: u8 = narrow(syntax.ue_max(63, "slice_pic_parameter_set_id")?)?;
        let pps = self
            .pps
            .get(&pps_id)
            .ok_or(H265Error::MissingParameterSet("picture parameter set"))?;
        let sps = self
            .sps
            .get(&pps.sps_id)
            .ok_or(H265Error::MissingParameterSet("sequence parameter set"))?;
        supported(sps)?;
        check_pps(sps, pps)?;
        let size_in_ctbs = sps.size_in_ctbs();
        let (dependent, segment_address) = if first_slice_segment_in_pic {
            (false, 0)
        } else {
            let dependent = pps.dependent_slice_segments_enabled
                && syntax.flag("dependent_slice_segment_flag")?;
            let address = syntax.bits(ceil_log2(size_in_ctbs), "slice_segment_address")?;
            if address >= size_in_ctbs {
                return Err(H265Error::Invalid(
                    "slice_segment_address is outside the picture",
                ));
            }
            (dependent, address)
        };
        let mut header = if dependent {
            let mut header = previous
                .ok_or(H265Error::Invalid(
                    "a dependent slice segment without a slice segment before it",
                ))?
                .clone();
            header.nal_unit_type = nal_unit_type;
            header.temporal_id = nal_header.temporal_id;
            header
        } else {
            read_independent(
                &mut syntax,
                sps,
                pps,
                nal_header,
                first_slice_segment_in_pic,
            )?
        };
        header.first_slice_segment_in_pic = first_slice_segment_in_pic;
        header.no_output_of_prior_pics = no_output_of_prior_pics;
        header.pps_id = pps_id;
        header.dependent = dependent;
        header.segment_address = segment_address;
        header.num_entry_points = 0;
        if pps.tiles.is_some() || pps.entropy_coding_sync_enabled {
            let count = syntax.ue_max(
                // The most substreams: a column of CTBs per tile row, or a
                // row of CTBs per tile.
                size_in_ctbs,
                "num_entry_point_offsets",
            )?;
            if count > 0 {
                let length = syntax.ue_max(31, "offset_len_minus1")? + 1;
                for _ in 0..count {
                    syntax.skip(length, "entry_point_offset_minus1")?;
                }
            }
            header.num_entry_points = count;
        }
        if pps.slice_segment_header_extension_present {
            let length = syntax.ue_max(256, "slice_segment_header_extension_length")?;
            syntax.skip(length * 8, "slice_segment_header_extension_data_byte")?;
        }
        syntax.byte_alignment()?;
        let unescaped = NalHeader::LEN
            + usize::try_from(syntax.bits_read() / 8)
                .map_err(|_| H265Error::Invalid("a slice header longer than memory"))?;
        let (_, emulation_prevention_bytes) = escaped_prefix(nal, unescaped);
        Ok(Slice {
            header,
            pps: Arc::clone(pps),
            sps: Arc::clone(sps),
            data_byte_offset: unescaped,
            emulation_prevention_bytes,
        })
    }
}

/// `Ceil(Log2(value))`.
const fn ceil_log2(value: u32) -> u32 {
    value.saturating_sub(1).bit_width()
}

#[allow(
    clippy::too_many_lines,
    clippy::useless_let_if_seq,
    reason = "one pass over the syntax, each element checked where it is read"
)]
fn read_independent(
    syntax: &mut Syntax<'_>,
    sps: &Sps,
    pps: &Pps,
    nal_header: NalHeader,
    first_slice_segment_in_pic: bool,
) -> Result<SliceHeader, H265Error> {
    let nal_unit_type = nal_header.unit_type();
    syntax.skip(
        u32::from(pps.num_extra_slice_header_bits),
        "slice_reserved_flag",
    )?;
    let slice_type = match syntax.ue_max(2, "slice_type")? {
        0 => SliceType::B,
        1 => SliceType::P,
        _ => SliceType::I,
    };
    if nal_unit_type.is_irap() && slice_type != SliceType::I {
        return Err(H265Error::Invalid("an IRAP picture has an inter slice"));
    }
    let pic_output = if pps.output_flag_present {
        syntax.flag("pic_output_flag")?
    } else {
        true
    };
    let mut poc_lsb = 0;
    let mut rps = ReferencePictureSet::default();
    let mut st_rps_bits = 0;
    let mut temporal_mvp = false;
    if !nal_unit_type.is_idr() {
        poc_lsb = syntax.bits(u32::from(sps.log2_max_poc_lsb), "slice_pic_order_cnt_lsb")?;
        let from_sps = syntax.flag("short_term_ref_pic_set_sps_flag")?;
        let count = sps.short_term_rps.len();
        rps.short_term = if from_sps {
            if count == 0 {
                return Err(H265Error::Invalid(
                    "a slice uses a sequence reference picture set but there is none",
                ));
            }
            let index = if count > 1 {
                syntax.bits(ceil_log2(narrow(count)?), "short_term_ref_pic_set_idx")?
            } else {
                0
            };
            sps.short_term_rps
                .get(narrow::<usize, _>(index)?)
                .ok_or(H265Error::Invalid(
                    "short_term_ref_pic_set_idx is out of range",
                ))?
                .clone()
        } else {
            let start = syntax.bits_read();
            let set = parse_short_term_rps(syntax, &sps.short_term_rps, true)?;
            st_rps_bits = narrow(syntax.bits_read() - start)?;
            set
        };
        if let Some(long_term_refs) = &sps.long_term_refs {
            rps.long_term = read_long_term(syntax, sps, long_term_refs)?;
        }
        if sps.temporal_mvp_enabled {
            temporal_mvp = syntax.flag("slice_temporal_mvp_enabled_flag")?;
        }
    }
    // ChromaArrayType is not 0: the SPS has chroma planes (separate colour
    // planes are not decoded).
    let chroma_present = sps.chroma_format_idc != 0;
    let (mut sao_luma, mut sao_chroma) = (false, false);
    if sps.sao_enabled {
        sao_luma = syntax.flag("slice_sao_luma_flag")?;
        if chroma_present {
            sao_chroma = syntax.flag("slice_sao_chroma_flag")?;
        }
    }
    let mut num_ref_idx_active = [0_usize; 2];
    let mut list_entries = [None, None];
    let (mut mvd_l1_zero, mut cabac_init) = (false, false);
    let mut collocated_from_l0 = true;
    let mut collocated_ref_idx = 0;
    let mut weights = None;
    let mut five_minus_max_num_merge_cand = 0;
    if slice_type != SliceType::I {
        let lists = if slice_type == SliceType::B { 2 } else { 1 };
        for (list, count) in num_ref_idx_active.iter_mut().enumerate().take(lists) {
            *count = usize::from(pps.num_ref_idx_default_active_minus1[list]) + 1;
        }
        if syntax.flag("num_ref_idx_active_override_flag")? {
            for count in num_ref_idx_active.iter_mut().take(lists) {
                *count = narrow::<usize, _>(syntax.ue_max(14, "num_ref_idx_active_minus1")?)? + 1;
            }
        }
        let total = rps.num_pic_total_curr();
        if total == 0 {
            return Err(H265Error::Invalid(
                "an inter slice whose reference picture set has no picture to refer to",
            ));
        }
        if pps.lists_modification_present && total > 1 {
            let bits = ceil_log2(narrow(total)?);
            for (list, entries) in list_entries.iter_mut().enumerate().take(lists) {
                if syntax.flag("ref_pic_list_modification_flag")? {
                    let mut values = Vec::new();
                    for _ in 0..num_ref_idx_active[list] {
                        values.push(syntax.bits(bits, "list_entry")?);
                    }
                    *entries = Some(values);
                }
            }
        }
        if slice_type == SliceType::B {
            mvd_l1_zero = syntax.flag("mvd_l1_zero_flag")?;
        }
        if pps.cabac_init_present {
            cabac_init = syntax.flag("cabac_init_flag")?;
        }
        if temporal_mvp {
            if slice_type == SliceType::B {
                collocated_from_l0 = syntax.flag("collocated_from_l0_flag")?;
            }
            let list = usize::from(!collocated_from_l0);
            if num_ref_idx_active[list] > 1 {
                let max = narrow(num_ref_idx_active[list] - 1)?;
                collocated_ref_idx = narrow(syntax.ue_max(max, "collocated_ref_idx")?)?;
            }
        }
        if (pps.weighted_pred && slice_type == SliceType::P)
            || (pps.weighted_bipred && slice_type == SliceType::B)
        {
            weights = Some(read_weights(
                syntax,
                num_ref_idx_active,
                lists,
                chroma_present,
            )?);
        }
        five_minus_max_num_merge_cand = narrow(syntax.ue_max(4, "five_minus_max_num_merge_cand")?)?;
    }
    let qp_delta = syntax.se_range(-128, 127, "slice_qp_delta")?;
    let qp_bd_offset = 6 * narrow::<i32, _>(sps.bit_depth_luma - 8)?;
    let slice_qp = 26 + i32::from(pps.init_qp_minus26) + qp_delta;
    if !(-qp_bd_offset..=51).contains(&slice_qp) {
        return Err(H265Error::Invalid("SliceQpY is out of range"));
    }
    let (mut cb_qp_offset, mut cr_qp_offset) = (0, 0);
    if pps.slice_chroma_qp_offsets_present {
        cb_qp_offset = syntax.se_range(-12, 12, "slice_cb_qp_offset")?;
        cr_qp_offset = syntax.se_range(-12, 12, "slice_cr_qp_offset")?;
        if !(-12..=12).contains(&(i32::from(pps.cb_qp_offset) + cb_qp_offset))
            || !(-12..=12).contains(&(i32::from(pps.cr_qp_offset) + cr_qp_offset))
        {
            return Err(H265Error::Invalid("a chroma QP offset is out of range"));
        }
    }
    let mut deblocking_disabled = pps.deblocking_disabled;
    let (mut beta_offset_div2, mut tc_offset_div2) = (
        i32::from(pps.beta_offset_div2),
        i32::from(pps.tc_offset_div2),
    );
    if pps.deblocking_override_enabled && syntax.flag("deblocking_filter_override_flag")? {
        deblocking_disabled = syntax.flag("slice_deblocking_filter_disabled_flag")?;
        if !deblocking_disabled {
            beta_offset_div2 = syntax.se_range(-6, 6, "slice_beta_offset_div2")?;
            tc_offset_div2 = syntax.se_range(-6, 6, "slice_tc_offset_div2")?;
        }
    }
    let mut loop_filter_across_slices = pps.loop_filter_across_slices;
    if pps.loop_filter_across_slices && (sao_luma || sao_chroma || !deblocking_disabled) {
        loop_filter_across_slices = syntax.flag("slice_loop_filter_across_slices_enabled_flag")?;
    }
    Ok(SliceHeader {
        nal_unit_type,
        temporal_id: nal_header.temporal_id,
        first_slice_segment_in_pic,
        no_output_of_prior_pics: false,
        pps_id: pps.id,
        dependent: false,
        segment_address: 0,
        slice_type,
        pic_output,
        poc_lsb,
        rps,
        st_rps_bits,
        temporal_mvp,
        sao_luma,
        sao_chroma,
        num_ref_idx_active,
        list_entries,
        mvd_l1_zero,
        cabac_init,
        collocated_from_l0,
        collocated_ref_idx,
        weights,
        five_minus_max_num_merge_cand,
        qp_delta: narrow(qp_delta)?,
        cb_qp_offset: narrow(cb_qp_offset)?,
        cr_qp_offset: narrow(cr_qp_offset)?,
        deblocking_disabled,
        beta_offset_div2: narrow(beta_offset_div2)?,
        tc_offset_div2: narrow(tc_offset_div2)?,
        loop_filter_across_slices,
        num_entry_points: 0,
    })
}

/// Reads the long-term part of the reference picture set (7.3.6.1).
fn read_long_term(
    syntax: &mut Syntax<'_>,
    sps: &Sps,
    from_sps: &[(u32, bool)],
) -> Result<Vec<LongTermRef>, H265Error> {
    let candidates: u32 = narrow(from_sps.len())?;
    let in_sps = if candidates > 0 {
        syntax.ue_max(candidates, "num_long_term_sps")?
    } else {
        0
    };
    let explicit = syntax.ue_max(32, "num_long_term_pics")?;
    if in_sps + explicit > 32 {
        return Err(H265Error::Invalid("more than 32 long-term pictures"));
    }
    let mut entries: Vec<LongTermRef> = Vec::new();
    for index in 0..in_sps + explicit {
        let (poc_lsb, used_by_curr) = if index < in_sps {
            let chosen = if candidates > 1 {
                syntax.bits(ceil_log2(candidates), "lt_idx_sps")?
            } else {
                0
            };
            *from_sps
                .get(narrow::<usize, _>(chosen)?)
                .ok_or(H265Error::Invalid("lt_idx_sps is out of range"))?
        } else {
            (
                syntax.bits(u32::from(sps.log2_max_poc_lsb), "poc_lsb_lt")?,
                syntax.flag("used_by_curr_pic_lt_flag")?,
            )
        };
        let msb_present = syntax.flag("delta_poc_msb_present_flag")?;
        // An absent delta_poc_msb_cycle_lt is 0, so the running total of
        // (7-52) carries through an entry without the flag.
        let delta = if msb_present {
            syntax.ue_max(1 << 20, "delta_poc_msb_cycle_lt")?
        } else {
            0
        };
        let before = match entries.last() {
            Some(previous) if index != in_sps => previous.msb_cycle,
            _ => 0,
        };
        entries.push(LongTermRef {
            poc_lsb,
            msb_cycle: before + delta,
            msb_present,
            used_by_curr,
        });
    }
    Ok(entries)
}

/// Reads `pred_weight_table()`; the chroma elements are present when the
/// stream has chroma planes.
fn read_weights(
    syntax: &mut Syntax<'_>,
    num_ref_idx_active: [usize; 2],
    lists: usize,
    chroma_present: bool,
) -> Result<PredWeights, H265Error> {
    let luma_log2_denom = syntax.ue_max(7, "luma_log2_weight_denom")?;
    let delta_chroma = if chroma_present {
        syntax.se_range(-7, 7, "delta_chroma_log2_weight_denom")?
    } else {
        0
    };
    let chroma_denom = narrow::<i32, _>(luma_log2_denom)? + delta_chroma;
    if !(0..=7).contains(&chroma_denom) {
        return Err(H265Error::Invalid("ChromaLog2WeightDenom is out of range"));
    }
    let mut result = PredWeights {
        luma_log2_denom: narrow(luma_log2_denom)?,
        delta_chroma_log2_denom: narrow(delta_chroma)?,
        lists: [ListWeights::default(); 2],
    };
    for (list, weights) in result.lists.iter_mut().enumerate().take(lists) {
        let count = num_ref_idx_active[list];
        let mut luma_flags = [false; MAX_REFS];
        let mut chroma_flags = [false; MAX_REFS];
        for flag in luma_flags.iter_mut().take(count) {
            *flag = syntax.flag("luma_weight_flag")?;
        }
        if chroma_present {
            for flag in chroma_flags.iter_mut().take(count) {
                *flag = syntax.flag("chroma_weight_flag")?;
            }
        }
        for index in 0..count {
            if luma_flags[index] {
                weights.delta_luma_weight[index] =
                    narrow(syntax.se_range(-128, 127, "delta_luma_weight")?)?;
                weights.luma_offset[index] = narrow(syntax.se_range(-128, 127, "luma_offset")?)?;
            }
            if chroma_flags[index] {
                for component in 0..2 {
                    let delta_weight = syntax.se_range(-128, 127, "delta_chroma_weight")?;
                    let delta_offset = syntax.se_range(-512, 511, "delta_chroma_offset")?;
                    let weight = (1_i32 << chroma_denom) + delta_weight;
                    let offset =
                        (128 + delta_offset - ((128 * weight) >> chroma_denom)).clamp(-128, 127);
                    weights.delta_chroma_weight[index][component] = narrow(delta_weight)?;
                    weights.chroma_offset[index][component] = narrow(offset)?;
                }
            }
        }
    }
    Ok(result)
}
