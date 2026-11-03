//! Slice headers, read with `h264-reader` and reduced to what decoding needs.

use super::{
    dpb::{Marking, Mmco, Modification, PictureHeader},
    error::{H264Error, Unsupported},
};
use h264_reader::{
    Context,
    nal::{
        NalHeader, UnitType,
        pps::{PicParamSetId, PicParameterSet},
        slice::{
            DecRefPicMarking, MemoryManagementControlOperation, ModificationOfPicNums,
            NumRefIdxActive, PicOrderCountLsb, PredWeight, RefPicListModifications, SliceFamily,
            SliceHeader as ParsedHeader,
        },
        sps::SeqParameterSet,
    },
    rbsp::{BitRead, BitReader, BitReaderError, ByteReader, Integer, Primitive},
};

/// The type of a slice, with SP and SI folded away because they are rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceKind {
    /// A P slice.
    P,
    /// A B slice.
    B,
    /// An I slice.
    I,
}

impl SliceKind {
    /// The value of `slice_type % 5` that VA-API takes.
    #[must_use]
    pub const fn va_slice_type(self) -> u8 {
        match self {
            Self::P => 0,
            Self::B => 1,
            Self::I => 2,
        }
    }
}

/// The explicit weights of one reference list entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryWeights {
    /// The luma weight and offset, when the entry carries them.
    pub luma: Option<(i16, i16)>,
    /// The Cb and Cr weights and offsets, when the entry carries them.
    pub chroma: Option<[(i16, i16); 2]>,
}

/// The prediction weight table of a slice (7.3.3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredWeights {
    /// `luma_log2_weight_denom`.
    pub luma_log2_denom: u8,
    /// `chroma_log2_weight_denom`.
    pub chroma_log2_denom: u8,
    /// The entries of the two lists.
    pub lists: [Vec<EntryWeights>; 2],
}

/// A slice header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceHeader {
    /// `first_mb_in_slice`.
    pub first_mb: u32,
    /// The slice type.
    pub kind: SliceKind,
    /// `pic_parameter_set_id`.
    pub pps_id: u8,
    /// `idr_pic_id`.
    pub idr_pic_id: Option<u32>,
    /// `redundant_pic_cnt`.
    pub redundant_pic_cnt: u32,
    /// `direct_spatial_mv_pred_flag`.
    pub direct_spatial_mv_pred: bool,
    /// The number of active entries of each list, defaults applied.
    pub num_ref_idx_active: [usize; 2],
    /// The modification operations of each list.
    pub modifications: [Vec<Modification>; 2],
    /// The explicit prediction weights.
    pub weights: Option<PredWeights>,
    /// `cabac_init_idc`.
    pub cabac_init_idc: u8,
    /// `slice_qp_delta`.
    pub slice_qp_delta: i8,
    /// `disable_deblocking_filter_idc`.
    pub disable_deblocking_filter_idc: u8,
    /// `slice_alpha_c0_offset_div2`.
    pub slice_alpha_c0_offset_div2: i8,
    /// `slice_beta_offset_div2`.
    pub slice_beta_offset_div2: i8,
    /// The fields picture management reads.
    pub picture: PictureHeader,
}

/// A parsed slice NAL unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slice {
    /// The header.
    pub header: SliceHeader,
    /// Bits from the start of the NAL unit, its header byte included, to
    /// `slice_data()`, not counting emulation prevention bytes.
    pub data_bit_offset: u32,
}

/// Counts the bits a parser reads.
struct Counting<R> {
    inner: R,
    bits: u64,
}

impl<R: BitRead> BitRead for Counting<R> {
    fn read_ue(&mut self, name: &'static str) -> Result<u32, BitReaderError> {
        let value = self.inner.read_ue(name)?;
        // `value + 1` has `n + 1` bits and the code is `n` zeros then those.
        self.bits += u64::from(2 * (u64::from(value) + 1).ilog2() + 1);
        Ok(value)
    }

    fn read_se(&mut self, name: &'static str) -> Result<i32, BitReaderError> {
        let value = self.inner.read_se(name)?;
        let code = if value > 0 {
            i64::from(value) * 2 - 1
        } else {
            -i64::from(value) * 2
        };
        self.bits += u64::from(2 * (code.unsigned_abs() + 1).ilog2() + 1);
        Ok(value)
    }

    fn read_bit(&mut self, name: &'static str) -> Result<bool, BitReaderError> {
        let bit = self.inner.read_bit(name)?;
        self.bits += 1;
        Ok(bit)
    }

    fn read<const BITS: u32, I: Integer>(
        &mut self,
        name: &'static str,
    ) -> Result<I, BitReaderError> {
        let value = self.inner.read::<BITS, I>(name)?;
        self.bits += u64::from(BITS);
        Ok(value)
    }

    fn read_var<I: Integer>(
        &mut self,
        bit_count: u32,
        name: &'static str,
    ) -> Result<I, BitReaderError> {
        let value = self.inner.read_var::<I>(bit_count, name)?;
        self.bits += u64::from(bit_count);
        Ok(value)
    }

    fn read_to<V: Primitive>(&mut self, name: &'static str) -> Result<V, BitReaderError> {
        let value = self.inner.read_to::<V>(name)?;
        self.bits += 8 * u64::try_from(size_of::<V>()).unwrap_or(0);
        Ok(value)
    }

    fn skip(&mut self, bit_count: u32, name: &'static str) -> Result<(), BitReaderError> {
        self.inner.skip(bit_count, name)?;
        self.bits += u64::from(bit_count);
        Ok(())
    }

    fn byte_aligned(&self) -> bool {
        self.inner.byte_aligned()
    }

    fn has_more_rbsp_data(&mut self, name: &'static str) -> Result<bool, BitReaderError> {
        self.inner.has_more_rbsp_data(name)
    }

    fn finish_rbsp(self) -> Result<(), BitReaderError> {
        self.inner.finish_rbsp()
    }

    fn finish_sei_payload(self) -> Result<(), BitReaderError> {
        self.inner.finish_sei_payload()
    }
}

impl SliceHeader {
    /// Whether this slice belongs to a different picture than `previous`
    /// (7.4.1.2.4), or restarts the picture at its first macroblock.
    #[must_use]
    pub fn starts_new_picture(&self, previous: &Self) -> bool {
        let (current, last) = (&self.picture, &previous.picture);
        self.first_mb == 0
            || self.pps_id != previous.pps_id
            || current.frame_num != last.frame_num
            || (current.nal_ref_idc == 0) != (last.nal_ref_idc == 0)
            || current.poc_lsb != last.poc_lsb
            || current.delta_poc_bottom != last.delta_poc_bottom
            || current.delta_poc != last.delta_poc
            || current.idr != last.idr
            || (current.idr && self.idr_pic_id != previous.idr_pic_id)
    }
}

/// The parameter sets received so far.
#[derive(Debug, Default)]
pub struct ParameterSets {
    context: Context,
}

impl ParameterSets {
    /// Parses and stores a sequence parameter set NAL unit.
    ///
    /// # Errors
    ///
    /// Returns [`H264Error::Malformed`] when it does not parse.
    pub fn put_sps(&mut self, nal: &[u8]) -> Result<(), H264Error> {
        let sps = SeqParameterSet::from_bits(rbsp_bits(nal))
            .map_err(|error| H264Error::malformed("sequence parameter set", &error))?;
        self.context.put_seq_param_set(sps);
        Ok(())
    }

    /// Parses and stores a picture parameter set NAL unit.
    ///
    /// # Errors
    ///
    /// Returns [`H264Error::Malformed`] when it does not parse, for instance
    /// because its sequence parameter set has not arrived.
    pub fn put_pps(&mut self, nal: &[u8]) -> Result<(), H264Error> {
        let pps = PicParameterSet::from_bits(&self.context, rbsp_bits(nal))
            .map_err(|error| H264Error::malformed("picture parameter set", &error))?;
        self.context.put_pic_param_set(pps);
        Ok(())
    }

    /// Parses a slice NAL unit.
    ///
    /// # Errors
    ///
    /// See [`parse_slice`].
    pub fn slice(&self, nal: &[u8]) -> Result<Slice, H264Error> {
        parse_slice(&self.context, nal)
    }

    /// The parameter sets a slice with `pps_id` is decoded with.
    ///
    /// # Errors
    ///
    /// Returns [`H264Error::MissingParameterSet`] when either is unknown.
    pub fn for_picture(
        &self,
        pps_id: u8,
    ) -> Result<(&SeqParameterSet, &PicParameterSet), H264Error> {
        let pps = PicParamSetId::from_u32(u32::from(pps_id))
            .ok()
            .and_then(|id| self.context.pps_by_id(id))
            .ok_or(H264Error::MissingParameterSet("picture parameter set"))?;
        let sps = self
            .context
            .sps_by_id(pps.seq_parameter_set_id)
            .ok_or(H264Error::MissingParameterSet("sequence parameter set"))?;
        Ok((sps, pps))
    }
}

/// The bit reader of a NAL unit's RBSP.
pub fn rbsp_bits(nal: &[u8]) -> BitReader<ByteReader<&[u8]>> {
    BitReader::new(ByteReader::skipping_h264_header(nal))
}

/// Parses a slice NAL unit of type 1 or 5 against the parameter sets in
/// `context`.
///
/// # Errors
///
/// Returns an [`H264Error`] when the NAL unit is malformed or uses a feature
/// the decoder does not implement.
pub fn parse_slice(context: &Context, nal: &[u8]) -> Result<Slice, H264Error> {
    let header_byte = *nal.first().ok_or(H264Error::Invalid("an empty NAL unit"))?;
    let nal_header = NalHeader::new(header_byte)
        .map_err(|error| H264Error::malformed("NAL unit header", &error))?;
    let idr = match nal_header.nal_unit_type() {
        UnitType::SliceLayerWithoutPartitioningNonIdr => false,
        UnitType::SliceLayerWithoutPartitioningIdr => true,
        UnitType::SliceDataPartitionALayer
        | UnitType::SliceDataPartitionBLayer
        | UnitType::SliceDataPartitionCLayer => return Err(Unsupported::DataPartitioning.into()),
        _ => return Err(H264Error::Invalid("not a slice NAL unit")),
    };
    let nal_ref_idc = nal_header.nal_ref_idc();
    let mut bits = Counting {
        inner: rbsp_bits(nal),
        bits: 0,
    };
    let (parsed, sps, pps) = ParsedHeader::from_bits(context, &mut bits, nal_header, None)
        .map_err(|error| H264Error::malformed("slice header", &error))?;
    super::sequence::check_picture_parameters(pps)?;
    let data_bit_offset = u32::try_from(8 + bits.bits).map_err(|_| H264Error::Invalid("header"))?;
    let kind = match parsed.slice_type.family {
        SliceFamily::P => SliceKind::P,
        SliceFamily::B => SliceKind::B,
        SliceFamily::I => SliceKind::I,
        SliceFamily::SP | SliceFamily::SI => return Err(Unsupported::SwitchingSlices.into()),
    };
    let num_ref_idx_active = match (&parsed.num_ref_idx_active, kind) {
        (
            Some(NumRefIdxActive::P {
                num_ref_idx_l0_active_minus1,
            }),
            _,
        ) => [*num_ref_idx_l0_active_minus1, 0],
        (
            Some(NumRefIdxActive::B {
                num_ref_idx_l0_active_minus1,
                num_ref_idx_l1_active_minus1,
            }),
            _,
        ) => [*num_ref_idx_l0_active_minus1, *num_ref_idx_l1_active_minus1],
        (None, _) => [
            pps.num_ref_idx_l0_default_active_minus1,
            pps.num_ref_idx_l1_default_active_minus1,
        ],
    }
    .map(|minus1| usize::try_from(minus1).unwrap_or(usize::MAX) + 1);
    let num_ref_idx_active = match kind {
        SliceKind::I => [0, 0],
        SliceKind::P => [num_ref_idx_active[0], 0],
        SliceKind::B => num_ref_idx_active,
    };
    let (poc_lsb, delta_poc_bottom, delta_poc) = match &parsed.pic_order_cnt_lsb {
        Some(PicOrderCountLsb::Frame(lsb)) => (*lsb, 0, [0; 2]),
        Some(PicOrderCountLsb::FieldsAbsolute {
            pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom,
        }) => (*pic_order_cnt_lsb, *delta_pic_order_cnt_bottom, [0; 2]),
        Some(PicOrderCountLsb::FieldsDelta(delta)) => (0, 0, *delta),
        None => (0, 0, [0; 2]),
    };
    let marking = parsed.dec_ref_pic_marking.as_ref().map(marking);
    let picture = PictureHeader {
        idr,
        nal_ref_idc,
        frame_num: u32::from(parsed.frame_num),
        poc_lsb,
        delta_poc_bottom,
        delta_poc,
        marking,
    };
    Ok(Slice {
        header: SliceHeader {
            first_mb: parsed.first_mb_in_slice,
            kind,
            pps_id: pps.pic_parameter_set_id.id(),
            idr_pic_id: parsed.idr_pic_id,
            redundant_pic_cnt: parsed.redundant_pic_cnt.unwrap_or(0),
            direct_spatial_mv_pred: parsed.direct_spatial_mv_pred_flag.unwrap_or(false),
            num_ref_idx_active,
            modifications: modifications(&parsed),
            weights: weights(&parsed, sps.chroma_info.chroma_array_type() != 0)?,
            cabac_init_idc: u8::try_from(parsed.cabac_init_idc.unwrap_or(0))
                .map_err(|_| H264Error::Invalid("cabac_init_idc"))?,
            slice_qp_delta: i8::try_from(parsed.slice_qp_delta)
                .map_err(|_| H264Error::Invalid("slice_qp_delta"))?,
            disable_deblocking_filter_idc: parsed.disable_deblocking_filter_idc,
            slice_alpha_c0_offset_div2: i8::try_from(
                parsed.slice_alpha_c0_offset_div2.unwrap_or(0),
            )
            .map_err(|_| H264Error::Invalid("slice_alpha_c0_offset_div2"))?,
            slice_beta_offset_div2: i8::try_from(parsed.slice_beta_offset_div2.unwrap_or(0))
                .map_err(|_| H264Error::Invalid("slice_beta_offset_div2"))?,
            picture,
        },
        data_bit_offset,
    })
}

fn marking(parsed: &DecRefPicMarking) -> Marking {
    match parsed {
        DecRefPicMarking::Idr {
            no_output_of_prior_pics_flag,
            long_term_reference_flag,
        } => Marking::Idr {
            no_output_of_prior_pics: *no_output_of_prior_pics_flag,
            long_term_reference: *long_term_reference_flag,
        },
        DecRefPicMarking::SlidingWindow => Marking::SlidingWindow,
        DecRefPicMarking::Adaptive(operations) => Marking::Adaptive(
            operations
                .iter()
                .map(|operation| match *operation {
                    MemoryManagementControlOperation::ShortTermUnusedForRef {
                        difference_of_pic_nums_minus1,
                    } => Mmco::ShortTermUnused {
                        difference_of_pic_nums_minus1,
                    },
                    MemoryManagementControlOperation::LongTermUnusedForRef {
                        long_term_pic_num,
                    } => Mmco::LongTermUnused { long_term_pic_num },
                    MemoryManagementControlOperation::ShortTermUsedForLongTerm {
                        difference_of_pic_nums_minus1,
                        long_term_frame_idx,
                    } => Mmco::ShortToLong {
                        difference_of_pic_nums_minus1,
                        long_term_frame_idx,
                    },
                    MemoryManagementControlOperation::MaxUsedLongTermFrameRef {
                        max_long_term_frame_idx_plus1,
                    } => Mmco::MaxLongTermIdx {
                        max_long_term_frame_idx_plus1,
                    },
                    MemoryManagementControlOperation::AllRefPicturesUnused => Mmco::AllUnused,
                    MemoryManagementControlOperation::CurrentUsedForLongTerm {
                        long_term_frame_idx,
                    } => Mmco::CurrentToLong {
                        long_term_frame_idx,
                    },
                })
                .collect(),
        ),
    }
}

fn modifications(parsed: &ParsedHeader) -> [Vec<Modification>; 2] {
    let convert = |operations: &[ModificationOfPicNums]| -> Vec<Modification> {
        operations
            .iter()
            .filter_map(|operation| match *operation {
                ModificationOfPicNums::Subtract(abs_diff_pic_num_minus1) => {
                    Some(Modification::Subtract {
                        abs_diff_pic_num_minus1,
                    })
                }
                ModificationOfPicNums::Add(abs_diff_pic_num_minus1) => Some(Modification::Add {
                    abs_diff_pic_num_minus1,
                }),
                ModificationOfPicNums::LongTermRef(long_term_pic_num) => {
                    Some(Modification::LongTerm { long_term_pic_num })
                }
                // Only multiview slices carry the view index operations,
                // which the parser reads only for them.
                ModificationOfPicNums::SubtractViewIdx(_)
                | ModificationOfPicNums::AddViewIdx(_) => None,
            })
            .collect()
    };
    match &parsed.ref_pic_list_modification {
        Some(RefPicListModifications::P {
            ref_pic_list_modification_l0,
        }) => [convert(ref_pic_list_modification_l0), Vec::new()],
        Some(RefPicListModifications::B {
            ref_pic_list_modification_l0,
            ref_pic_list_modification_l1,
        }) => [
            convert(ref_pic_list_modification_l0),
            convert(ref_pic_list_modification_l1),
        ],
        Some(RefPicListModifications::I) | None => [Vec::new(), Vec::new()],
    }
}

fn weight_pair(weight: &PredWeight) -> Result<(i16, i16), H264Error> {
    let weight_value =
        i16::try_from(weight.weight).map_err(|_| H264Error::Invalid("prediction weight"))?;
    let offset =
        i16::try_from(weight.offset).map_err(|_| H264Error::Invalid("prediction offset"))?;
    Ok((weight_value, offset))
}

fn weights(parsed: &ParsedHeader, has_chroma: bool) -> Result<Option<PredWeights>, H264Error> {
    let Some(table) = &parsed.pred_weight_table else {
        return Ok(None);
    };
    let entries = |luma: &[Option<PredWeight>],
                   chroma: &[Vec<PredWeight>]|
     -> Result<Vec<EntryWeights>, H264Error> {
        luma.iter()
            .enumerate()
            .map(|(index, luma)| {
                let chroma = match chroma.get(index).map(Vec::as_slice) {
                    Some([cb, cr]) => Some([weight_pair(cb)?, weight_pair(cr)?]),
                    _ => None,
                };
                Ok(EntryWeights {
                    luma: luma.as_ref().map(weight_pair).transpose()?,
                    chroma,
                })
            })
            .collect()
    };
    let denom = |value: u32| -> Result<u8, H264Error> {
        u8::try_from(value)
            .ok()
            .filter(|denom| *denom <= 7)
            .ok_or(H264Error::Invalid("weight denominator"))
    };
    Ok(Some(PredWeights {
        luma_log2_denom: denom(table.luma_log2_weight_denom)?,
        chroma_log2_denom: if has_chroma {
            denom(table.chroma_log2_weight_denom.unwrap_or(0))?
        } else {
            0
        },
        lists: [
            entries(&table.luma_weights, &table.chroma_weights)?,
            entries(&table.luma_weights_l1, &table.chroma_weights_l1)?,
        ],
    }))
}
