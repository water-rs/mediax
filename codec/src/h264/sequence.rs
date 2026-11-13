//! What the decoder takes from sequence and picture parameter sets.

use super::{
    dpb::{PocType, SequenceParams},
    error::{H264Error, Unsupported},
};
use h264_reader::nal::{
    pps::PicParameterSet,
    sps::{
        ChromaFormat, FrameMbsFlags, PicOrderCntType, Profile, ScalingLists8x8Resolved,
        SeqParameterSet,
    },
};

/// The largest `max_dec_frame_buffering` and `MaxDpbFrames`.
const MAX_DPB_FRAMES: usize = 16;
/// `MaxFS` of level 6.2, the largest frame in macroblocks.
const MAX_FRAME_MBS: u32 = 139_264;

/// The luma samples cropped from each edge of the decoded frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    /// Columns removed on the left.
    pub left: u32,
    /// Columns removed on the right.
    pub right: u32,
    /// Rows removed at the top.
    pub top: u32,
    /// Rows removed at the bottom.
    pub bottom: u32,
}

/// An activated sequence parameter set, checked against what the decoder
/// supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sequence {
    /// The parameter set, kept to recognise when it changes.
    pub sps: SeqParameterSet,
    /// `PicWidthInMbs`.
    pub width_mbs: u32,
    /// `FrameHeightInMbs`.
    pub height_mbs: u32,
    /// The cropping rectangle.
    pub crop: Crop,
    /// The width of the output frame in samples.
    pub width: u32,
    /// The height of the output frame in samples.
    pub height: u32,
    /// What the picture buffer needs.
    pub dpb: SequenceParams,
}

impl Sequence {
    /// Checks that `sps` is a progressive 8-bit 4:2:0 Baseline, Main or High
    /// stream and derives its geometry and picture buffer parameters.
    ///
    /// # Errors
    ///
    /// Returns [`H264Error::Unsupported`] for other streams and
    /// [`H264Error::Invalid`] for inconsistent values.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the SPS fields, each checked where it is read"
    )]
    pub fn new(sps: &SeqParameterSet) -> Result<Self, H264Error> {
        let profile_idc = sps.profile().profile_idc();
        if !matches!(
            sps.profile(),
            Profile::Baseline
                | Profile::ConstrainedBaseline
                | Profile::Main
                | Profile::High
                | Profile::ProgressiveHigh
                | Profile::ConstrainedHigh
        ) {
            return Err(Unsupported::Profile(profile_idc).into());
        }
        if !matches!(sps.frame_mbs_flags, FrameMbsFlags::Frames) {
            return Err(Unsupported::Interlaced.into());
        }
        let chroma = &sps.chroma_info;
        if chroma.chroma_format != ChromaFormat::YUV420 {
            return Err(Unsupported::ChromaFormat(chroma.chroma_format.to_u32()).into());
        }
        if chroma.bit_depth_luma_minus8 != 0 || chroma.bit_depth_chroma_minus8 != 0 {
            return Err(Unsupported::BitDepth {
                luma: chroma.bit_depth_luma_minus8 + 8,
                chroma: chroma.bit_depth_chroma_minus8 + 8,
            }
            .into());
        }
        let too_large = H264Error::Invalid("the frame is larger than level 6.2 allows");
        let width_mbs = sps
            .pic_width_in_mbs_minus1
            .checked_add(1)
            .ok_or_else(|| too_large.clone())?;
        let height_mbs = sps
            .pic_height_in_map_units_minus1
            .checked_add(1)
            .ok_or_else(|| too_large.clone())?;
        let frame_mbs = width_mbs
            .checked_mul(height_mbs)
            .filter(|frame_mbs| *frame_mbs <= MAX_FRAME_MBS)
            .ok_or(too_large)?;
        let (width, height) = sps
            .pixel_dimensions()
            .map_err(|error| H264Error::malformed("sequence parameter set", &error))?;
        if width == 0 || height == 0 {
            return Err(H264Error::Invalid("the cropped frame is empty"));
        }
        // CropUnitX and CropUnitY are 2 for 4:2:0 frames.
        let crop = sps.frame_cropping.as_ref().map_or(
            Crop {
                left: 0,
                right: 0,
                top: 0,
                bottom: 0,
            },
            |crop| Crop {
                left: crop.left_offset * 2,
                right: crop.right_offset * 2,
                top: crop.top_offset * 2,
                bottom: crop.bottom_offset * 2,
            },
        );
        let max_num_ref_frames = usize::try_from(sps.max_num_ref_frames).unwrap_or(usize::MAX);
        if max_num_ref_frames > MAX_DPB_FRAMES {
            return Err(H264Error::Invalid("max_num_ref_frames exceeds 16"));
        }
        let restrictions = sps
            .vui_parameters
            .as_ref()
            .and_then(|vui| vui.bitstream_restrictions.as_ref());
        let capacity = restrictions.map_or_else(
            || {
                sps.level().limits().map_or(MAX_DPB_FRAMES, |limits| {
                    usize::try_from(limits.max_dpb_mbs / frame_mbs)
                        .unwrap_or(MAX_DPB_FRAMES)
                        .min(MAX_DPB_FRAMES)
                })
            },
            |restrictions| {
                usize::try_from(restrictions.max_dec_frame_buffering)
                    .unwrap_or(usize::MAX)
                    .max(1)
            },
        );
        if capacity == 0 || capacity > MAX_DPB_FRAMES {
            return Err(H264Error::Invalid(
                "the picture buffer holds no frames or more than 16",
            ));
        }
        let poc = match &sps.pic_order_cnt {
            PicOrderCntType::TypeZero {
                log2_max_pic_order_cnt_lsb_minus4,
            } => PocType::Zero {
                log2_max_lsb: log2_max_pic_order_cnt_lsb_minus4 + 4,
            },
            PicOrderCntType::TypeOne {
                offset_for_non_ref_pic,
                offset_for_top_to_bottom_field,
                offsets_for_ref_frame,
                ..
            } => PocType::One {
                offset_for_non_ref_pic: *offset_for_non_ref_pic,
                offset_for_top_to_bottom_field: *offset_for_top_to_bottom_field,
                offsets_for_ref_frame: offsets_for_ref_frame.clone(),
            },
            PicOrderCntType::TypeTwo => PocType::Two,
        };
        Ok(Self {
            sps: sps.clone(),
            width_mbs,
            height_mbs,
            crop,
            width,
            height,
            dpb: SequenceParams {
                log2_max_frame_num: sps.log2_max_frame_num(),
                poc,
                max_num_ref_frames,
                gaps_allowed: sps.gaps_in_frame_num_value_allowed_flag,
                capacity,
                // Without the VUI's count, the pictures that cannot be
                // reordered are output at once: Baseline has no B slices and
                // type 2 orders pictures as they are decoded. Anything else
                // waits for the buffer to fill, as the bumping process says.
                num_reorder_frames: restrictions
                    .map(|restrictions| {
                        usize::try_from(restrictions.max_num_reorder_frames).unwrap_or(usize::MAX)
                    })
                    .or_else(|| {
                        (matches!(sps.pic_order_cnt, PicOrderCntType::TypeTwo)
                            || matches!(
                                sps.profile(),
                                Profile::Baseline | Profile::ConstrainedBaseline
                            ))
                        .then_some(0)
                    }),
            },
        })
    }
}

/// Checks the picture parameter set against what the decoder supports.
///
/// # Errors
///
/// Returns [`H264Error::Unsupported`] when the set uses slice groups.
pub fn check_picture_parameters(pps: &PicParameterSet) -> Result<(), H264Error> {
    if pps.slice_groups.is_some() {
        return Err(Unsupported::SliceGroups.into());
    }
    Ok(())
}

/// The scaling lists a picture is decoded with, in raster order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalingLists {
    /// The six 4x4 lists: intra Y, Cb, Cr, then inter Y, Cb, Cr.
    pub lists_4x4: [[u8; 16]; 6],
    /// The two 8x8 lists: intra Y, then inter Y.
    pub lists_8x8: [[u8; 64]; 2],
}

/// `result[i]` is the raster position of the `i`-th coefficient of the
/// zig-zag scan of a `size` by `size` block (8.5.6).
const fn zigzag<const N: usize, const SIZE: usize>() -> [usize; N] {
    let mut scan = [0; N];
    let mut index = 0;
    let mut diagonal = 0;
    while diagonal < 2 * SIZE - 1 {
        // Odd diagonals run from the top right down to the left, even ones
        // the other way.
        let mut step = 0;
        while step <= diagonal {
            let x = if diagonal % 2 == 1 {
                diagonal - step
            } else {
                step
            };
            let y = diagonal - x;
            if x < SIZE && y < SIZE {
                scan[index] = y * SIZE + x;
                index += 1;
            }
            step += 1;
        }
        diagonal += 1;
    }
    scan
}

const ZIGZAG_4X4: [usize; 16] = zigzag::<16, 4>();
const ZIGZAG_8X8: [usize; 64] = zigzag::<64, 8>();

impl ScalingLists {
    /// Resolves the lists of a picture: the picture parameter set's, falling
    /// back to the sequence's, which fall back to flat or default lists.
    #[must_use]
    pub fn new(sps: &SeqParameterSet, pps: &PicParameterSet) -> Self {
        let picture_matrix = pps
            .extension
            .as_ref()
            .and_then(|extension| extension.pic_scaling_matrix.as_deref());
        let lists_4x4 = picture_matrix.map_or_else(
            || sps.chroma_info.scaling_lists_4x4(),
            |matrix| matrix.scaling_lists_4x4(&sps.chroma_info),
        );
        let lists_8x8 = picture_matrix
            .and_then(|matrix| matrix.scaling_lists_8x8(&sps.chroma_info))
            .unwrap_or_else(|| sps.chroma_info.scaling_lists_8x8());
        let mut raster_4x4 = [[0; 16]; 6];
        for (raster, list) in raster_4x4.iter_mut().zip(&lists_4x4) {
            for (index, &position) in ZIGZAG_4X4.iter().enumerate() {
                raster[position] = list[index];
            }
        }
        let mut raster_8x8 = [[0; 64]; 2];
        let lists = match &lists_8x8 {
            ScalingLists8x8Resolved::Y(lists) => lists.as_slice(),
            ScalingLists8x8Resolved::YCbCr(lists) => &lists[..2],
        };
        for (raster, list) in raster_8x8.iter_mut().zip(lists) {
            for (index, &position) in ZIGZAG_8X8.iter().enumerate() {
                raster[position] = list[index];
            }
        }
        Self {
            lists_4x4: raster_4x4,
            lists_8x8: raster_8x8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_scans_match_the_standard() {
        assert_eq!(
            ZIGZAG_4X4,
            [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15]
        );
        assert_eq!(
            ZIGZAG_8X8,
            [
                0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41,
                34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23,
                30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63
            ]
        );
    }
}
