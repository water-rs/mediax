//! Video, sequence and picture parameter sets (7.3.2), reduced to what a
//! hardware decoder of Main and Main 10 streams needs.
//!
//! No maintained crate parses H.265 parameter sets and slice headers
//! completely: the audit is in the pull request that introduced this module.
//! The bit reading is `h264-reader`'s codec independent RBSP layer; the
//! syntax is read here, with every count checked before it sizes anything
//! and every arithmetic step checked.

use super::{
    dpb::{RpsEntry, ShortTermRps},
    error::{H265Error, Unsupported},
    syntax::{Syntax, narrow},
};

/// The most short-term reference picture sets a sequence parameter set holds.
const MAX_SHORT_TERM_RPS: u32 = 64;
/// The most long-term reference pictures a sequence parameter set lists.
const MAX_LONG_TERM_SPS: u32 = 32;
/// The most pictures a reference picture set holds (`MaxDpbSize`).
const MAX_RPS_PICTURES: usize = 16;
/// The largest picture dimension: level 6.2 allows `sqrt(8 * MaxLumaPs)`.
const MAX_DIMENSION: u32 = 16_888;
/// `MaxLumaPs` of level 6.2.
const MAX_LUMA_PS: u64 = 35_651_584;

/// What a decoder reads of the profile, tier and level syntax (7.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileTierLevel {
    /// `general_profile_space`.
    pub profile_space: u8,
    /// `general_profile_idc`.
    pub profile_idc: u8,
    /// `general_profile_compatibility_flag[0..32]`, flag `j` at bit `j`.
    pub compatibility: u32,
    /// `general_progressive_source_flag`.
    pub progressive_source: bool,
    /// `general_interlaced_source_flag`.
    pub interlaced_source: bool,
    /// `general_level_idc`.
    pub level_idc: u8,
}

impl ProfileTierLevel {
    /// Whether the stream says it conforms to the profile `idc`.
    #[must_use]
    pub const fn conforms_to(&self, idc: u8) -> bool {
        self.profile_idc == idc || (self.compatibility >> idc) & 1 == 1
    }

    fn parse(syntax: &mut Syntax<'_>, max_sub_layers_minus1: u32) -> Result<Self, H265Error> {
        let profile_space = syntax.bits(2, "general_profile_space")?;
        syntax.skip(1, "general_tier_flag")?;
        let profile_idc = syntax.bits(5, "general_profile_idc")?;
        let compatibility = syntax
            .bits(32, "general_profile_compatibility_flag")?
            .reverse_bits();
        let progressive_source = syntax.flag("general_progressive_source_flag")?;
        let interlaced_source = syntax.flag("general_interlaced_source_flag")?;
        // non_packed_constraint, frame_only_constraint, 43 reserved bits and
        // the inbld or reserved bit.
        syntax.skip(2 + 43 + 1, "general_constraint_flags")?;
        let level_idc = syntax.bits(8, "general_level_idc")?;
        let mut profile_present = [false; 8];
        let mut level_present = [false; 8];
        let sub_layers: usize = narrow(max_sub_layers_minus1)?;
        for index in 0..sub_layers {
            profile_present[index] = syntax.flag("sub_layer_profile_present_flag")?;
            level_present[index] = syntax.flag("sub_layer_level_present_flag")?;
        }
        if max_sub_layers_minus1 > 0 {
            syntax.skip(2 * (8 - max_sub_layers_minus1), "reserved_zero_2bits")?;
        }
        for index in 0..sub_layers {
            if profile_present[index] {
                syntax.skip(88, "sub_layer_profile")?;
            }
            if level_present[index] {
                syntax.skip(8, "sub_layer_level_idc")?;
            }
        }
        Ok(Self {
            profile_space: narrow(profile_space)?,
            profile_idc: narrow(profile_idc)?,
            compatibility,
            progressive_source,
            interlaced_source,
            level_idc: narrow(level_idc)?,
        })
    }
}

/// A video parameter set. Nothing in it changes how a single layer is
/// decoded, so only its identifier is kept after the syntax up to the profile
/// has been checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vps {
    /// `vps_video_parameter_set_id`.
    pub id: u8,
}

impl Vps {
    /// Reads a VPS NAL unit.
    ///
    /// # Errors
    ///
    /// Returns [`H265Error::Malformed`] when it does not parse.
    pub fn parse(nal: &[u8]) -> Result<Self, H265Error> {
        let mut syntax = Syntax::new(nal, "video parameter set");
        let id = syntax.bits(4, "vps_video_parameter_set_id")?;
        syntax.skip(2, "vps_base_layer_flags")?;
        syntax.skip(6, "vps_max_layers_minus1")?;
        let max_sub_layers_minus1 = syntax.bits(3, "vps_max_sub_layers_minus1")?;
        if max_sub_layers_minus1 > 6 {
            return Err(H265Error::Invalid("vps_max_sub_layers_minus1 exceeds 6"));
        }
        syntax.skip(1, "vps_temporal_id_nesting_flag")?;
        if syntax.bits(16, "vps_reserved_0xffff_16bits")? != 0xffff {
            return Err(H265Error::Invalid(
                "vps_reserved_0xffff_16bits is not 0xffff",
            ));
        }
        ProfileTierLevel::parse(&mut syntax, max_sub_layers_minus1)?;
        Ok(Self { id: narrow(id)? })
    }
}

/// `ScalingList` and `ScalingFactor` source data (7.3.4), in the order the
/// coefficients are coded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScalingLists {
    /// `ScalingList[sizeId][matrixId]`: the 16 coefficients of a 4x4 list or
    /// the 64 of an 8x8 list that stands for larger blocks. Size 3 has the
    /// two matrices 0 (intra) and 1 (inter).
    pub lists: [[[u8; 64]; 6]; 4],
    /// `scaling_list_dc_coef_minus8 + 8` of the sizes 2 and 3.
    pub dc: [[u8; 6]; 4],
}

/// Table 7-6, the default intra 8x8 list.
const DEFAULT_INTRA: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18, 17, 18, 18, 17, 18, 21, 19, 20,
    21, 20, 19, 21, 24, 22, 22, 24, 24, 22, 22, 24, 25, 25, 27, 30, 27, 25, 25, 29, 31, 35, 35, 31,
    29, 36, 41, 44, 41, 36, 47, 54, 54, 47, 65, 70, 65, 88, 88, 115,
];
/// Table 7-6, the default inter 8x8 list.
const DEFAULT_INTER: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18, 18, 18, 18, 18, 18, 20, 20, 20,
    20, 20, 20, 20, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 28, 28, 28, 28, 28,
    28, 33, 33, 33, 33, 33, 41, 41, 41, 41, 54, 54, 54, 71, 71, 91,
];

impl ScalingLists {
    /// The default lists (Tables 7-5 and 7-6).
    #[must_use]
    pub fn default_lists() -> Self {
        let mut lists = [[[16; 64]; 6]; 4];
        for (size_id, size) in lists.iter_mut().enumerate().skip(1) {
            for (matrix_id, list) in size.iter_mut().enumerate() {
                *list = Self::default_list(size_id, matrix_id);
            }
        }
        Self {
            lists,
            dc: [[16; 6]; 4],
        }
    }

    const fn default_list(size_id: usize, matrix_id: usize) -> [u8; 64] {
        let inter = if size_id == 3 {
            matrix_id >= 1
        } else {
            matrix_id >= 3
        };
        match (size_id, inter) {
            (0, _) => [16; 64],
            (_, false) => DEFAULT_INTRA,
            (_, true) => DEFAULT_INTER,
        }
    }

    /// Reads `scaling_list_data()`.
    fn parse(syntax: &mut Syntax<'_>) -> Result<Self, H265Error> {
        let mut result = Self::default_lists();
        for size_id in 0..4_usize {
            let matrices = if size_id == 3 { 2 } else { 6 };
            for matrix_id in 0..matrices {
                if syntax.flag("scaling_list_pred_mode_flag")? {
                    let count = 64.min(1_usize << (4 + (size_id << 1)));
                    let mut next = 8_i32;
                    if size_id > 1 {
                        next = syntax.se_range(-7, 247, "scaling_list_dc_coef_minus8")? + 8;
                        result.dc[size_id][matrix_id] = narrow(next)?;
                    }
                    for coefficient in 0..count {
                        let delta = syntax.se_range(-128, 127, "scaling_list_delta_coef")?;
                        next = (next + delta + 256) % 256;
                        if next == 0 {
                            return Err(H265Error::Invalid("a scaling list coefficient is 0"));
                        }
                        result.lists[size_id][matrix_id][coefficient] = narrow(next)?;
                    }
                } else {
                    let delta: usize = narrow(
                        syntax.ue_max(narrow(matrix_id)?, "scaling_list_pred_matrix_id_delta")?,
                    )?;
                    if delta == 0 {
                        result.lists[size_id][matrix_id] = Self::default_list(size_id, matrix_id);
                        result.dc[size_id][matrix_id] = 16;
                    } else {
                        let reference = matrix_id - delta;
                        result.lists[size_id][matrix_id] = result.lists[size_id][reference];
                        result.dc[size_id][matrix_id] = result.dc[size_id][reference];
                    }
                }
            }
        }
        Ok(result)
    }
}

/// The PCM coding parameters of a sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pcm {
    /// `pcm_sample_bit_depth_luma_minus1`.
    pub bit_depth_luma_minus1: u8,
    /// `pcm_sample_bit_depth_chroma_minus1`.
    pub bit_depth_chroma_minus1: u8,
    /// `log2_min_pcm_luma_coding_block_size_minus3`.
    pub log2_min_cb_minus3: u8,
    /// `log2_diff_max_min_pcm_luma_coding_block_size`.
    pub log2_diff_max_min_cb: u8,
    /// `pcm_loop_filter_disabled_flag`.
    pub loop_filter_disabled: bool,
}

/// The conformance window, in luma samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Columns removed on the left.
    pub left: u32,
    /// Columns removed on the right.
    pub right: u32,
    /// Rows removed at the top.
    pub top: u32,
    /// Rows removed at the bottom.
    pub bottom: u32,
}

/// A sequence parameter set, up to `strong_intra_smoothing_enabled_flag`.
///
/// What follows (the VUI and the extensions) does not change how pictures of
/// Main and Main 10 streams are decoded, so it is not read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the syntax elements one to one"
)]
pub struct Sps {
    /// `sps_seq_parameter_set_id`.
    pub id: u8,
    /// `sps_max_sub_layers_minus1`.
    pub max_sub_layers_minus1: u8,
    /// The profile, tier and level.
    pub profile: ProfileTierLevel,
    /// `chroma_format_idc`.
    pub chroma_format_idc: u32,
    /// `pic_width_in_luma_samples`.
    pub width: u32,
    /// `pic_height_in_luma_samples`.
    pub height: u32,
    /// The conformance window, in luma samples.
    pub window: Window,
    /// `bit_depth_luma_minus8 + 8`.
    pub bit_depth_luma: u32,
    /// `bit_depth_chroma_minus8 + 8`.
    pub bit_depth_chroma: u32,
    /// `log2_max_pic_order_cnt_lsb_minus4 + 4`.
    pub log2_max_poc_lsb: u8,
    /// `sps_max_dec_pic_buffering_minus1` of the highest sub-layer.
    pub max_dec_pic_buffering_minus1: u8,
    /// `sps_max_num_reorder_pics` of the highest sub-layer.
    pub max_num_reorder_pics: u8,
    /// `sps_max_latency_increase_plus1` of the highest sub-layer.
    pub max_latency_increase_plus1: u32,
    /// `log2_min_luma_coding_block_size_minus3`.
    pub log2_min_cb_minus3: u8,
    /// `log2_diff_max_min_luma_coding_block_size`.
    pub log2_diff_max_min_cb: u8,
    /// `log2_min_luma_transform_block_size_minus2`.
    pub log2_min_tb_minus2: u8,
    /// `log2_diff_max_min_luma_transform_block_size`.
    pub log2_diff_max_min_tb: u8,
    /// `max_transform_hierarchy_depth_inter`.
    pub max_transform_hierarchy_depth_inter: u8,
    /// `max_transform_hierarchy_depth_intra`.
    pub max_transform_hierarchy_depth_intra: u8,
    /// `scaling_list_enabled_flag`.
    pub scaling_list_enabled: bool,
    /// The lists of `sps_scaling_list_data_present_flag`.
    pub scaling_lists: Option<ScalingLists>,
    /// `amp_enabled_flag`.
    pub amp_enabled: bool,
    /// `sample_adaptive_offset_enabled_flag`.
    pub sao_enabled: bool,
    /// The PCM parameters, when `pcm_enabled_flag` is set.
    pub pcm: Option<Pcm>,
    /// The short-term reference picture sets.
    pub short_term_rps: Vec<ShortTermRps>,
    /// `lt_ref_pic_poc_lsb_sps` and `used_by_curr_pic_lt_sps_flag`, when
    /// `long_term_ref_pics_present_flag` is set.
    pub long_term_refs: Option<Vec<(u32, bool)>>,
    /// `sps_temporal_mvp_enabled_flag`.
    pub temporal_mvp_enabled: bool,
    /// `strong_intra_smoothing_enabled_flag`.
    pub strong_intra_smoothing_enabled: bool,
}

impl Sps {
    /// `CtbLog2SizeY`.
    #[must_use]
    pub const fn log2_ctb_size(&self) -> u32 {
        self.log2_min_cb_minus3 as u32 + 3 + self.log2_diff_max_min_cb as u32
    }

    /// `PicWidthInCtbsY`.
    #[must_use]
    pub const fn width_in_ctbs(&self) -> u32 {
        self.width.div_ceil(1 << self.log2_ctb_size())
    }

    /// `PicHeightInCtbsY`.
    #[must_use]
    pub const fn height_in_ctbs(&self) -> u32 {
        self.height.div_ceil(1 << self.log2_ctb_size())
    }

    /// `PicSizeInCtbsY`.
    #[must_use]
    pub const fn size_in_ctbs(&self) -> u32 {
        self.width_in_ctbs() * self.height_in_ctbs()
    }

    /// Reads an SPS NAL unit.
    ///
    /// # Errors
    ///
    /// Returns [`H265Error::Malformed`] when it does not parse and
    /// [`H265Error::Invalid`] when a value is out of its range.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the syntax, each element checked where it is read"
    )]
    pub fn parse(nal: &[u8]) -> Result<Self, H265Error> {
        let mut syntax = Syntax::new(nal, "sequence parameter set");
        syntax.skip(4, "sps_video_parameter_set_id")?;
        let max_sub_layers_minus1 = syntax.bits(3, "sps_max_sub_layers_minus1")?;
        if max_sub_layers_minus1 > 6 {
            return Err(H265Error::Invalid("sps_max_sub_layers_minus1 exceeds 6"));
        }
        syntax.skip(1, "sps_temporal_id_nesting_flag")?;
        let profile = ProfileTierLevel::parse(&mut syntax, max_sub_layers_minus1)?;
        let id = syntax.ue_max(15, "sps_seq_parameter_set_id")?;
        let chroma_format_idc = syntax.ue_max(3, "chroma_format_idc")?;
        if chroma_format_idc == 3 && syntax.flag("separate_colour_plane_flag")? {
            return Err(Unsupported::ChromaFormat(3).into());
        }
        let width = syntax.ue("pic_width_in_luma_samples")?;
        let height = syntax.ue("pic_height_in_luma_samples")?;
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(H265Error::Invalid("the picture size is outside level 6.2"));
        }
        if u64::from(width) * u64::from(height) > MAX_LUMA_PS {
            return Err(H265Error::Invalid("the picture is larger than level 6.2"));
        }
        let window = if syntax.flag("conformance_window_flag")? {
            // The offsets are in chroma units, which 4:2:0 and 4:2:2 make two
            // luma samples wide, and 4:2:0 two high.
            let (unit_x, unit_y) = match chroma_format_idc {
                1 => (2, 2),
                2 => (2, 1),
                _ => (1, 1),
            };
            let mut offsets = [0_u32; 4];
            for (offset, unit) in offsets.iter_mut().zip([unit_x, unit_x, unit_y, unit_y]) {
                *offset = syntax
                    .ue_max(MAX_DIMENSION, "conf_win_offset")?
                    .checked_mul(unit)
                    .ok_or(H265Error::Invalid("the conformance window"))?;
            }
            let [left, right, top, bottom] = offsets;
            Window {
                left,
                right,
                top,
                bottom,
            }
        } else {
            Window {
                left: 0,
                right: 0,
                top: 0,
                bottom: 0,
            }
        };
        if u64::from(window.left) + u64::from(window.right) >= u64::from(width)
            || u64::from(window.top) + u64::from(window.bottom) >= u64::from(height)
        {
            return Err(H265Error::Invalid("the conformance window is empty"));
        }
        let bit_depth_luma = syntax.ue_max(8, "bit_depth_luma_minus8")? + 8;
        let bit_depth_chroma = syntax.ue_max(8, "bit_depth_chroma_minus8")? + 8;
        let log2_max_poc_lsb = syntax.ue_max(12, "log2_max_pic_order_cnt_lsb_minus4")? + 4;
        let sub_layer_ordering_info_present =
            syntax.flag("sps_sub_layer_ordering_info_present_flag")?;
        // Without the info of every sub-layer, the last one's applies to all.
        let first = if sub_layer_ordering_info_present {
            0
        } else {
            max_sub_layers_minus1
        };
        let mut ordering = (0, 0, 0);
        for _ in first..=max_sub_layers_minus1 {
            ordering = (
                syntax.ue_max(15, "sps_max_dec_pic_buffering_minus1")?,
                syntax.ue("sps_max_num_reorder_pics")?,
                syntax.ue("sps_max_latency_increase_plus1")?,
            );
        }
        let (max_dec_pic_buffering_minus1, max_num_reorder_pics, max_latency_increase_plus1) =
            ordering;
        if max_num_reorder_pics > max_dec_pic_buffering_minus1 {
            return Err(H265Error::Invalid(
                "sps_max_num_reorder_pics exceeds sps_max_dec_pic_buffering_minus1",
            ));
        }
        let log2_min_cb_minus3 = syntax.ue_max(3, "log2_min_luma_coding_block_size_minus3")?;
        let log2_diff_max_min_cb = syntax.ue_max(3, "log2_diff_max_min_luma_coding_block_size")?;
        let log2_min_cb = log2_min_cb_minus3 + 3;
        let log2_ctb = log2_min_cb + log2_diff_max_min_cb;
        if !(4..=6).contains(&log2_ctb) {
            return Err(H265Error::Invalid("CtbLog2SizeY is outside 4..=6"));
        }
        if width % (1 << log2_min_cb) != 0 || height % (1 << log2_min_cb) != 0 {
            return Err(H265Error::Invalid(
                "the picture size is not a multiple of MinCbSizeY",
            ));
        }
        let transform_min_minus2 = syntax.ue_max(3, "log2_min_luma_transform_block_size_minus2")?;
        let transform_diff = syntax.ue_max(3, "log2_diff_max_min_luma_transform_block_size")?;
        let transform_min = transform_min_minus2 + 2;
        let transform_max = transform_min + transform_diff;
        if transform_min >= log2_min_cb || transform_max > log2_ctb.min(5) {
            return Err(H265Error::Invalid("the transform block sizes"));
        }
        let max_depth = log2_ctb - transform_min;
        let max_transform_hierarchy_depth_inter =
            syntax.ue_max(max_depth, "max_transform_hierarchy_depth_inter")?;
        let max_transform_hierarchy_depth_intra =
            syntax.ue_max(max_depth, "max_transform_hierarchy_depth_intra")?;
        let scaling_list_enabled = syntax.flag("scaling_list_enabled_flag")?;
        let scaling_lists =
            if scaling_list_enabled && syntax.flag("sps_scaling_list_data_present_flag")? {
                Some(ScalingLists::parse(&mut syntax)?)
            } else {
                None
            };
        let amp_enabled = syntax.flag("amp_enabled_flag")?;
        let sao_enabled = syntax.flag("sample_adaptive_offset_enabled_flag")?;
        let pcm = if syntax.flag("pcm_enabled_flag")? {
            let bit_depth_luma_minus1 = syntax.bits(4, "pcm_sample_bit_depth_luma_minus1")?;
            let bit_depth_chroma_minus1 = syntax.bits(4, "pcm_sample_bit_depth_chroma_minus1")?;
            if bit_depth_luma_minus1 >= bit_depth_luma
                || bit_depth_chroma_minus1 >= bit_depth_chroma
            {
                return Err(H265Error::Invalid(
                    "a PCM bit depth exceeds the sample bit depth",
                ));
            }
            let pcm_log2_min_cb_minus3 =
                syntax.ue_max(2, "log2_min_pcm_luma_coding_block_size_minus3")?;
            let pcm_log2_min = pcm_log2_min_cb_minus3 + 3;
            let pcm_log2_diff = syntax.ue_max(2, "log2_diff_max_min_pcm_luma_coding_block_size")?;
            if pcm_log2_min < log2_min_cb.min(5) || pcm_log2_min + pcm_log2_diff > log2_ctb.min(5) {
                return Err(H265Error::Invalid("the PCM coding block sizes"));
            }
            Some(Pcm {
                bit_depth_luma_minus1: narrow(bit_depth_luma_minus1)?,
                bit_depth_chroma_minus1: narrow(bit_depth_chroma_minus1)?,
                log2_min_cb_minus3: narrow(pcm_log2_min_cb_minus3)?,
                log2_diff_max_min_cb: narrow(pcm_log2_diff)?,
                loop_filter_disabled: syntax.flag("pcm_loop_filter_disabled_flag")?,
            })
        } else {
            None
        };
        let num_short_term_rps =
            syntax.ue_max(MAX_SHORT_TERM_RPS, "num_short_term_ref_pic_sets")?;
        let mut short_term_rps = Vec::new();
        for _ in 0..num_short_term_rps {
            let set = parse_short_term_rps(&mut syntax, &short_term_rps, false)?;
            short_term_rps.push(set);
        }
        let long_term_refs = if syntax.flag("long_term_ref_pics_present_flag")? {
            let count = syntax.ue_max(MAX_LONG_TERM_SPS, "num_long_term_ref_pics_sps")?;
            let mut entries = Vec::new();
            for _ in 0..count {
                entries.push((
                    syntax.bits(log2_max_poc_lsb, "lt_ref_pic_poc_lsb_sps")?,
                    syntax.flag("used_by_curr_pic_lt_sps_flag")?,
                ));
            }
            Some(entries)
        } else {
            None
        };
        let temporal_mvp_enabled = syntax.flag("sps_temporal_mvp_enabled_flag")?;
        let strong_intra_smoothing_enabled = syntax.flag("strong_intra_smoothing_enabled_flag")?;
        Ok(Self {
            id: narrow(id)?,
            max_sub_layers_minus1: narrow(max_sub_layers_minus1)?,
            profile,
            chroma_format_idc,
            width,
            height,
            window,
            bit_depth_luma,
            bit_depth_chroma,
            log2_max_poc_lsb: narrow(log2_max_poc_lsb)?,
            max_dec_pic_buffering_minus1: narrow(max_dec_pic_buffering_minus1)?,
            max_num_reorder_pics: narrow(max_num_reorder_pics)?,
            max_latency_increase_plus1,
            log2_min_cb_minus3: narrow(log2_min_cb_minus3)?,
            log2_diff_max_min_cb: narrow(log2_diff_max_min_cb)?,
            log2_min_tb_minus2: narrow(transform_min_minus2)?,
            log2_diff_max_min_tb: narrow(transform_diff)?,
            max_transform_hierarchy_depth_inter: narrow(max_transform_hierarchy_depth_inter)?,
            max_transform_hierarchy_depth_intra: narrow(max_transform_hierarchy_depth_intra)?,
            scaling_list_enabled,
            scaling_lists,
            amp_enabled,
            sao_enabled,
            pcm,
            short_term_rps,
            long_term_refs,
            temporal_mvp_enabled,
            strong_intra_smoothing_enabled,
        })
    }
}

/// Reads `st_ref_pic_set(stRpsIdx)` (7.3.7) for the set after `previous`, a
/// slice header's own set when `in_slice`.
///
/// # Errors
///
/// Returns an [`H265Error`] when the syntax is malformed or out of range.
pub fn parse_short_term_rps(
    syntax: &mut Syntax<'_>,
    previous: &[ShortTermRps],
    in_slice: bool,
) -> Result<ShortTermRps, H265Error> {
    let index = previous.len();
    let inter = index != 0 && syntax.flag("inter_ref_pic_set_prediction_flag")?;
    if !inter {
        return parse_explicit_rps(syntax);
    }
    let delta_idx = if in_slice {
        narrow(syntax.ue_max(narrow(index - 1)?, "delta_idx_minus1")?)?
    } else {
        0
    };
    let reference = &previous[index - 1 - delta_idx];
    let delta_rps_sign = syntax.flag("delta_rps_sign")?;
    let magnitude = narrow::<i32, _>(syntax.ue_max(32_767, "abs_delta_rps_minus1")?)? + 1;
    let delta_rps = if delta_rps_sign {
        -magnitude
    } else {
        magnitude
    };
    // Entry `j` of the reference set is its j-th negative picture, then its
    // positive ones; entry `len` stands for the reference set's own picture.
    let reference_entries: Vec<i32> = reference
        .negative
        .iter()
        .chain(&reference.positive)
        .map(|entry| entry.delta_poc)
        .collect();
    let negatives = reference.negative.len();
    let mut used = Vec::with_capacity(reference_entries.len() + 1);
    let mut use_delta = Vec::with_capacity(reference_entries.len() + 1);
    for _ in 0..=reference_entries.len() {
        let used_by_curr = syntax.flag("used_by_curr_pic_flag")?;
        used.push(used_by_curr);
        use_delta.push(used_by_curr || syntax.flag("use_delta_flag")?);
    }
    let candidate = |entry: usize| -> Result<(i32, bool, bool), H265Error> {
        let base = reference_entries.get(entry).copied().unwrap_or(0);
        let delta_poc = base
            .checked_add(delta_rps)
            .ok_or(H265Error::Invalid("delta_rps"))?;
        Ok((delta_poc, use_delta[entry], used[entry]))
    };
    let own = reference_entries.len();
    let mut negative = Vec::new();
    let mut positive = Vec::new();
    // (7-61): negative pictures, nearest first.
    for entry in (negatives..own)
        .rev()
        .chain(std::iter::once(own))
        .chain(0..negatives)
    {
        let (delta_poc, keep, used_by_curr) = if entry == own {
            (delta_rps, use_delta[own], used[own])
        } else {
            candidate(entry)?
        };
        if delta_poc < 0 && keep {
            negative.push(RpsEntry {
                delta_poc,
                used_by_curr,
            });
        }
    }
    // (7-62): positive pictures, nearest first.
    for entry in (0..negatives)
        .rev()
        .chain(std::iter::once(own))
        .chain(negatives..own)
    {
        let (delta_poc, keep, used_by_curr) = if entry == own {
            (delta_rps, use_delta[own], used[own])
        } else {
            candidate(entry)?
        };
        if delta_poc > 0 && keep {
            positive.push(RpsEntry {
                delta_poc,
                used_by_curr,
            });
        }
    }
    if negative.len() + positive.len() > MAX_RPS_PICTURES {
        return Err(H265Error::Invalid(
            "a reference picture set holds more than 16 pictures",
        ));
    }
    Ok(ShortTermRps { negative, positive })
}

/// Reads a short-term reference picture set coded without prediction.
fn parse_explicit_rps(syntax: &mut Syntax<'_>) -> Result<ShortTermRps, H265Error> {
    let max: u32 = narrow(MAX_RPS_PICTURES)?;
    let negatives = syntax.ue_max(max, "num_negative_pics")?;
    let positives = syntax.ue_max(max - negatives.min(max), "num_positive_pics")?;
    let mut read = |count: u32, sign: i32| -> Result<Vec<RpsEntry>, H265Error> {
        let mut entries = Vec::new();
        let mut delta_poc = 0_i32;
        for _ in 0..count {
            let step = narrow::<i32, _>(syntax.ue_max(32_767, "delta_poc_minus1")?)? + 1;
            delta_poc += sign * step;
            entries.push(RpsEntry {
                delta_poc,
                used_by_curr: syntax.flag("used_by_curr_pic_flag")?,
            });
        }
        Ok(entries)
    };
    Ok(ShortTermRps {
        negative: read(negatives, -1)?,
        positive: read(positives, 1)?,
    })
}

/// The tile partitioning of a picture parameter set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tiles {
    /// `num_tile_columns_minus1`.
    pub columns_minus1: u32,
    /// `num_tile_rows_minus1`.
    pub rows_minus1: u32,
    /// `column_width_minus1` and `row_height_minus1`, unless the spacing is
    /// uniform.
    pub explicit_sizes: Option<(Vec<u32>, Vec<u32>)>,
    /// `loop_filter_across_tiles_enabled_flag`.
    pub loop_filter_across: bool,
}

/// A picture parameter set, without the extensions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the syntax elements one to one"
)]
pub struct Pps {
    /// `pps_pic_parameter_set_id`.
    pub id: u8,
    /// `pps_seq_parameter_set_id`.
    pub sps_id: u8,
    /// `dependent_slice_segments_enabled_flag`.
    pub dependent_slice_segments_enabled: bool,
    /// `output_flag_present_flag`.
    pub output_flag_present: bool,
    /// `num_extra_slice_header_bits`.
    pub num_extra_slice_header_bits: u8,
    /// `sign_data_hiding_enabled_flag`.
    pub sign_data_hiding_enabled: bool,
    /// `cabac_init_present_flag`.
    pub cabac_init_present: bool,
    /// `num_ref_idx_l0_default_active_minus1` and the list 1 value.
    pub num_ref_idx_default_active_minus1: [u8; 2],
    /// `init_qp_minus26`.
    pub init_qp_minus26: i8,
    /// `constrained_intra_pred_flag`.
    pub constrained_intra_pred: bool,
    /// `transform_skip_enabled_flag`.
    pub transform_skip_enabled: bool,
    /// `cu_qp_delta_enabled_flag`.
    pub cu_qp_delta_enabled: bool,
    /// `diff_cu_qp_delta_depth`.
    pub diff_cu_qp_delta_depth: u8,
    /// `pps_cb_qp_offset`.
    pub cb_qp_offset: i8,
    /// `pps_cr_qp_offset`.
    pub cr_qp_offset: i8,
    /// `pps_slice_chroma_qp_offsets_present_flag`.
    pub slice_chroma_qp_offsets_present: bool,
    /// `weighted_pred_flag`.
    pub weighted_pred: bool,
    /// `weighted_bipred_flag`.
    pub weighted_bipred: bool,
    /// `transquant_bypass_enabled_flag`.
    pub transquant_bypass_enabled: bool,
    /// The tiles, when `tiles_enabled_flag` is set.
    pub tiles: Option<Tiles>,
    /// `entropy_coding_sync_enabled_flag`.
    pub entropy_coding_sync_enabled: bool,
    /// `pps_loop_filter_across_slices_enabled_flag`.
    pub loop_filter_across_slices: bool,
    /// `deblocking_filter_override_enabled_flag`.
    pub deblocking_override_enabled: bool,
    /// `pps_deblocking_filter_disabled_flag`.
    pub deblocking_disabled: bool,
    /// `pps_beta_offset_div2`.
    pub beta_offset_div2: i8,
    /// `pps_tc_offset_div2`.
    pub tc_offset_div2: i8,
    /// The lists of `pps_scaling_list_data_present_flag`.
    pub scaling_lists: Option<ScalingLists>,
    /// `lists_modification_present_flag`.
    pub lists_modification_present: bool,
    /// `log2_parallel_merge_level_minus2`.
    pub log2_parallel_merge_level_minus2: u8,
    /// `slice_segment_header_extension_present_flag`.
    pub slice_segment_header_extension_present: bool,
}

impl Pps {
    /// Reads a PPS NAL unit.
    ///
    /// # Errors
    ///
    /// Returns [`H265Error::Malformed`] when it does not parse,
    /// [`H265Error::Invalid`] when a value is out of its range and
    /// [`H265Error::Unsupported`] for the extensions beyond version 1.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the syntax, each element checked where it is read"
    )]
    pub fn parse(nal: &[u8]) -> Result<Self, H265Error> {
        let mut syntax = Syntax::new(nal, "picture parameter set");
        let id = syntax.ue_max(63, "pps_pic_parameter_set_id")?;
        let sps_id = syntax.ue_max(15, "pps_seq_parameter_set_id")?;
        let dependent_slice_segments_enabled =
            syntax.flag("dependent_slice_segments_enabled_flag")?;
        let output_flag_present = syntax.flag("output_flag_present_flag")?;
        let num_extra_slice_header_bits = syntax.bits(3, "num_extra_slice_header_bits")?;
        let sign_data_hiding_enabled = syntax.flag("sign_data_hiding_enabled_flag")?;
        let cabac_init_present = syntax.flag("cabac_init_present_flag")?;
        let num_ref_idx_l0 = syntax.ue_max(14, "num_ref_idx_l0_default_active_minus1")?;
        let num_ref_idx_l1 = syntax.ue_max(14, "num_ref_idx_l1_default_active_minus1")?;
        let init_qp_minus26 = syntax.se_range(-38, 25, "init_qp_minus26")?;
        let constrained_intra_pred = syntax.flag("constrained_intra_pred_flag")?;
        let transform_skip_enabled = syntax.flag("transform_skip_enabled_flag")?;
        let cu_qp_delta_enabled = syntax.flag("cu_qp_delta_enabled_flag")?;
        let diff_cu_qp_delta_depth = if cu_qp_delta_enabled {
            syntax.ue_max(3, "diff_cu_qp_delta_depth")?
        } else {
            0
        };
        let cb_qp_offset = syntax.se_range(-12, 12, "pps_cb_qp_offset")?;
        let cr_qp_offset = syntax.se_range(-12, 12, "pps_cr_qp_offset")?;
        let slice_chroma_qp_offsets_present =
            syntax.flag("pps_slice_chroma_qp_offsets_present_flag")?;
        let weighted_pred = syntax.flag("weighted_pred_flag")?;
        let weighted_bipred = syntax.flag("weighted_bipred_flag")?;
        let transquant_bypass_enabled = syntax.flag("transquant_bypass_enabled_flag")?;
        let tiles_enabled = syntax.flag("tiles_enabled_flag")?;
        let entropy_coding_sync_enabled = syntax.flag("entropy_coding_sync_enabled_flag")?;
        let tiles = if tiles_enabled {
            let columns_minus1 = syntax.ue_max(19, "num_tile_columns_minus1")?;
            let rows_minus1 = syntax.ue_max(21, "num_tile_rows_minus1")?;
            let uniform = syntax.flag("uniform_spacing_flag")?;
            let explicit_sizes = if uniform {
                None
            } else {
                let mut widths = Vec::new();
                for _ in 0..columns_minus1 {
                    widths.push(syntax.ue_max(MAX_DIMENSION, "column_width_minus1")?);
                }
                let mut heights = Vec::new();
                for _ in 0..rows_minus1 {
                    heights.push(syntax.ue_max(MAX_DIMENSION, "row_height_minus1")?);
                }
                Some((widths, heights))
            };
            Some(Tiles {
                columns_minus1,
                rows_minus1,
                explicit_sizes,
                loop_filter_across: syntax.flag("loop_filter_across_tiles_enabled_flag")?,
            })
        } else {
            None
        };
        let loop_filter_across_slices =
            syntax.flag("pps_loop_filter_across_slices_enabled_flag")?;
        let (mut deblocking_override_enabled, mut deblocking_disabled) = (false, false);
        let (mut beta_offset_div2, mut tc_offset_div2) = (0, 0);
        if syntax.flag("deblocking_filter_control_present_flag")? {
            deblocking_override_enabled = syntax.flag("deblocking_filter_override_enabled_flag")?;
            deblocking_disabled = syntax.flag("pps_deblocking_filter_disabled_flag")?;
            if !deblocking_disabled {
                beta_offset_div2 = syntax.se_range(-6, 6, "pps_beta_offset_div2")?;
                tc_offset_div2 = syntax.se_range(-6, 6, "pps_tc_offset_div2")?;
            }
        }
        let scaling_lists = if syntax.flag("pps_scaling_list_data_present_flag")? {
            Some(ScalingLists::parse(&mut syntax)?)
        } else {
            None
        };
        let lists_modification_present = syntax.flag("lists_modification_present_flag")?;
        let log2_parallel_merge_level_minus2 =
            syntax.ue_max(4, "log2_parallel_merge_level_minus2")?;
        let slice_segment_header_extension_present =
            syntax.flag("slice_segment_header_extension_present_flag")?;
        if syntax.flag("pps_extension_present_flag")? {
            let range = syntax.flag("pps_range_extension_flag")?;
            let multilayer = syntax.flag("pps_multilayer_extension_flag")?;
            let three_d = syntax.flag("pps_3d_extension_flag")?;
            let screen_content = syntax.flag("pps_scc_extension_flag")?;
            // The remaining four bits are reserved for extension data that
            // decoders ignore.
            for (present, name) in [
                (range, "range"),
                (multilayer, "multilayer"),
                (three_d, "3D"),
                (screen_content, "screen content"),
            ] {
                if present {
                    return Err(Unsupported::Extension(name).into());
                }
            }
        }
        Ok(Self {
            id: narrow(id)?,
            sps_id: narrow(sps_id)?,
            dependent_slice_segments_enabled,
            output_flag_present,
            num_extra_slice_header_bits: narrow(num_extra_slice_header_bits)?,
            sign_data_hiding_enabled,
            cabac_init_present,
            num_ref_idx_default_active_minus1: [narrow(num_ref_idx_l0)?, narrow(num_ref_idx_l1)?],
            init_qp_minus26: narrow(init_qp_minus26)?,
            constrained_intra_pred,
            transform_skip_enabled,
            cu_qp_delta_enabled,
            diff_cu_qp_delta_depth: narrow(diff_cu_qp_delta_depth)?,
            cb_qp_offset: narrow(cb_qp_offset)?,
            cr_qp_offset: narrow(cr_qp_offset)?,
            slice_chroma_qp_offsets_present,
            weighted_pred,
            weighted_bipred,
            transquant_bypass_enabled,
            tiles,
            entropy_coding_sync_enabled,
            loop_filter_across_slices,
            deblocking_override_enabled,
            deblocking_disabled,
            beta_offset_div2: narrow(beta_offset_div2)?,
            tc_offset_div2: narrow(tc_offset_div2)?,
            scaling_lists,
            lists_modification_present,
            log2_parallel_merge_level_minus2: narrow(log2_parallel_merge_level_minus2)?,
            slice_segment_header_extension_present,
        })
    }
}
