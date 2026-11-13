//! What a coded video sequence and its pictures derive from the parameter
//! sets: the supported feature check, picture geometry, tiles and scaling
//! factors.

use super::{
    dpb::SequenceParams,
    error::{H265Error, Unsupported},
    params::{Pps, ScalingLists, Sps},
};
use std::sync::Arc;

/// The sample bit depth of a sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    /// 8 bits per sample: the Main profile.
    Eight,
    /// 10 bits per sample: the Main 10 profile.
    Ten,
}

/// A sequence parameter set the decoder accepts.
#[derive(Debug, Clone)]
pub struct Sequence {
    /// The parameter set.
    pub sps: Arc<Sps>,
    /// The sample bit depth.
    pub bit_depth: BitDepth,
    /// What picture management needs.
    pub params: SequenceParams,
}

impl Sequence {
    /// Checks that the decoder supports the sequence.
    ///
    /// # Errors
    ///
    /// Returns [`H265Error::Unsupported`] for streams other than progressive
    /// 4:2:0 Main and Main 10 ones.
    pub fn new(sps: Arc<Sps>) -> Result<Self, H265Error> {
        let bit_depth = Self::supported(&sps)?;
        let params = SequenceParams {
            log2_max_poc_lsb: sps.log2_max_poc_lsb,
            max_dec_pic_buffering: usize::from(sps.max_dec_pic_buffering_minus1) + 1,
            max_num_reorder: usize::from(sps.max_num_reorder_pics),
            max_latency_increase_plus1: sps.max_latency_increase_plus1,
        };
        Ok(Self {
            sps,
            bit_depth,
            params,
        })
    }

    /// Checks that the decoder supports the sequence, and returns its bit
    /// depth. This is the check that runs before a slice header is parsed, so
    /// an unsupported stream is reported as such rather than as malformed.
    ///
    /// # Errors
    ///
    /// Returns [`H265Error::Unsupported`] for streams other than progressive
    /// 4:2:0 Main and Main 10 ones.
    pub fn supported(sps: &Sps) -> Result<BitDepth, H265Error> {
        let profile = &sps.profile;
        if profile.profile_space != 0 || ![1, 2, 3].iter().any(|&idc| profile.conforms_to(idc)) {
            return Err(Unsupported::Profile(profile.profile_idc).into());
        }
        if profile.interlaced_source && !profile.progressive_source {
            return Err(Unsupported::Interlaced.into());
        }
        if sps.chroma_format_idc != 1 {
            return Err(Unsupported::ChromaFormat(sps.chroma_format_idc).into());
        }
        match (sps.bit_depth_luma, sps.bit_depth_chroma) {
            (8, 8) => Ok(BitDepth::Eight),
            (10, 10) => Ok(BitDepth::Ten),
            (luma, chroma) => Err(Unsupported::BitDepth { luma, chroma }.into()),
        }
    }

    /// The width and height of the output pictures, the conformance window
    /// applied.
    #[must_use]
    pub fn output_size(&self) -> (u32, u32) {
        let window = self.sps.window;
        (
            self.sps.width - window.left - window.right,
            self.sps.height - window.top - window.bottom,
        )
    }
}

/// Checks a picture parameter set against the sequence it refers to.
///
/// # Errors
///
/// Returns [`H265Error::Invalid`] when a value is out of its range.
pub fn check_pps(sps: &Sps, pps: &Pps) -> Result<(), H265Error> {
    let qp_bd_offset = 6 * super::syntax::narrow::<i32, _>(sps.bit_depth_luma - 8)?;
    if i32::from(pps.init_qp_minus26) < -(26 + qp_bd_offset) {
        return Err(H265Error::Invalid("init_qp_minus26 is out of range"));
    }
    if pps.diff_cu_qp_delta_depth > sps.log2_diff_max_min_cb {
        return Err(H265Error::Invalid(
            "diff_cu_qp_delta_depth exceeds log2_diff_max_min_luma_coding_block_size",
        ));
    }
    if u32::from(pps.log2_parallel_merge_level_minus2) + 2 > sps.log2_ctb_size() {
        return Err(H265Error::Invalid("Log2ParMrgLevel exceeds CtbLog2SizeY"));
    }
    if let Some(tiles) = &pps.tiles {
        let (width, height) = (sps.width_in_ctbs(), sps.height_in_ctbs());
        if tiles.columns_minus1 >= width || tiles.rows_minus1 >= height {
            return Err(H265Error::Invalid("more tiles than coding tree blocks"));
        }
        if let Some((widths, heights)) = &tiles.explicit_sizes {
            let columns: u64 = widths.iter().map(|&value| u64::from(value) + 1).sum();
            let rows: u64 = heights.iter().map(|&value| u64::from(value) + 1).sum();
            if columns >= u64::from(width) || rows >= u64::from(height) {
                return Err(H265Error::Invalid("the tiles are larger than the picture"));
            }
        }
    }
    Ok(())
}

/// The tile partitioning of a picture, in coding tree blocks (6.5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tiling {
    /// `colWidth`.
    pub columns: Vec<u32>,
    /// `rowHeight`.
    pub rows: Vec<u32>,
}

impl Tiling {
    /// Derives the partitioning of a picture. The parameter sets have passed
    /// [`check_pps`].
    #[must_use]
    pub fn new(sps: &Sps, pps: &Pps) -> Self {
        let (width, height) = (sps.width_in_ctbs(), sps.height_in_ctbs());
        let Some(tiles) = &pps.tiles else {
            return Self {
                columns: vec![width],
                rows: vec![height],
            };
        };
        let split = |total: u32, count: u32, explicit: Option<&Vec<u32>>| -> Vec<u32> {
            explicit.map_or_else(
                || {
                    (0..count)
                        .map(|index| {
                            // At most 20 tiles over a picture a few hundred
                            // coding tree blocks wide: no overflow.
                            let edge = |position: u32| position * total / count;
                            edge(index + 1) - edge(index)
                        })
                        .collect()
                },
                |sizes| {
                    let mut parts: Vec<u32> = sizes.iter().map(|&value| value + 1).collect();
                    parts.push(total - parts.iter().sum::<u32>());
                    parts
                },
            )
        };
        let explicit = tiles.explicit_sizes.as_ref();
        Self {
            columns: split(
                width,
                tiles.columns_minus1 + 1,
                explicit.map(|sizes| &sizes.0),
            ),
            rows: split(
                height,
                tiles.rows_minus1 + 1,
                explicit.map(|sizes| &sizes.1),
            ),
        }
    }
}

/// The scaling factors of a picture in the order the hardware takes them:
/// the coefficients of each list in raster order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScalingFactors {
    /// 4x4 lists.
    pub list_4x4: [[u8; 16]; 6],
    /// 8x8 lists.
    pub list_8x8: [[u8; 64]; 6],
    /// The 8x8 lists that stand for 16x16 blocks.
    pub list_16x16: [[u8; 64]; 6],
    /// The 8x8 lists that stand for 32x32 blocks, intra and inter.
    pub list_32x32: [[u8; 64]; 2],
    /// The DC coefficients of 16x16 blocks.
    pub dc_16x16: [u8; 6],
    /// The DC coefficients of 32x32 blocks.
    pub dc_32x32: [u8; 2],
}

/// The up-right diagonal scan of a `size` by `size` block (6.5.3) as raster
/// positions.
fn diagonal_scan(size: usize) -> Vec<usize> {
    let mut positions = Vec::with_capacity(size * size);
    let (mut x, mut y) = (0_usize, 0_usize);
    while positions.len() < size * size {
        let mut row = y;
        let mut column = x;
        loop {
            if column < size && row < size {
                positions.push(size * row + column);
            }
            if row == 0 {
                break;
            }
            row -= 1;
            column += 1;
        }
        x = 0;
        y += 1;
    }
    positions
}

impl ScalingFactors {
    /// The factors a picture uses, or `None` when the sequence does not
    /// enable scaling lists: the picture's lists, else the sequence's, else
    /// the defaults.
    #[must_use]
    pub fn new(sps: &Sps, pps: &Pps) -> Option<Self> {
        if !sps.scaling_list_enabled {
            return None;
        }
        let lists = pps
            .scaling_lists
            .or(sps.scaling_lists)
            .unwrap_or_else(ScalingLists::default_lists);
        let scan_4x4 = diagonal_scan(4);
        let scan_8x8 = diagonal_scan(8);
        let mut result = Self {
            list_4x4: [[0; 16]; 6],
            list_8x8: [[0; 64]; 6],
            list_16x16: [[0; 64]; 6],
            list_32x32: [[0; 64]; 2],
            dc_16x16: lists.dc[2],
            dc_32x32: [lists.dc[3][0], lists.dc[3][1]],
        };
        for matrix in 0..6 {
            for (coded, &position) in scan_4x4.iter().enumerate() {
                result.list_4x4[matrix][position] = lists.lists[0][matrix][coded];
            }
            for (coded, &position) in scan_8x8.iter().enumerate() {
                result.list_8x8[matrix][position] = lists.lists[1][matrix][coded];
                result.list_16x16[matrix][position] = lists.lists[2][matrix][coded];
                if matrix < 2 {
                    result.list_32x32[matrix][position] = lists.lists[3][matrix][coded];
                }
            }
        }
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_diagonal_scan_goes_up_and_right() {
        // Figure 6-11: (x, y) = (0,0), (0,1), (1,0), (0,2), (1,1), (2,0)...
        assert_eq!(&diagonal_scan(4)[..6], &[0, 4, 1, 8, 5, 2]);
        let scan = diagonal_scan(8);
        assert_eq!(scan.len(), 64);
        let mut sorted = scan.clone();
        sorted.sort_unstable();
        assert!(
            sorted
                .iter()
                .enumerate()
                .all(|(index, &value)| index == value)
        );
        assert_eq!(scan[63], 63);
    }
}
