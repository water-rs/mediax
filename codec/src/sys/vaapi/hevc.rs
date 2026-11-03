//! The H.265 decoder over a VA-API render node.
//!
//! The bitstream is parsed here and the picture buffer is managed by
//! [`crate::h265`]; the hardware only reconstructs pictures. Every packet is
//! one complete access unit, as the other decoder backends take them. A
//! picture's slice segments are collected and submitted when the next picture,
//! a parameter set or the end of the packet shows the picture is complete, and
//! each submission waits for the hardware, so frames are read back in the order
//! the picture buffer outputs them.

use super::{
    VaapiError,
    api::Api,
    device::{Device, PictureBuffers, SampleFormat, render_nodes},
    target::{DecodedPicture, Target, Visible, offers_decoding},
    types::{
        HevcLongSliceFlags, HevcPictureFields, HevcSliceParsingFields, VA_HEVC_MAX_REFS,
        VA_HEVC_REF_INVALID, VA_PICTURE_HEVC_LONG_TERM_REFERENCE, VA_PICTURE_HEVC_RPS_LT_CURR,
        VA_PICTURE_HEVC_RPS_ST_CURR_AFTER, VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE,
        VA_SLICE_DATA_FLAG_ALL, VAIQMatrixBufferHEVC, VAIQMatrixBufferType, VAPictureHEVC,
        VAPictureParameterBufferHEVC, VAPictureParameterBufferType, VAProfileHEVCMain,
        VAProfileHEVCMain10, VASliceDataBufferType, VASliceParameterBufferHEVC,
        VASliceParameterBufferType, VASurfaceID,
    },
};
use crate::{
    annexb::annex_b_nalus,
    bitstream::NalStreamConverter,
    h265::{
        H265Error,
        dpb::{Dpb, Event, FrameId},
        nal::{NalHeader, NalUnitType},
        sequence::{BitDepth, ScalingFactors, Sequence, Tiling},
        slice::{ParameterSets, Slice, SliceHeader, SliceType},
    },
};
#[cfg(any(test, feature = "fuzzing"))]
use std::path::Path;
use std::{rc::Rc, sync::Arc};

/// Whether a NAL unit type cannot continue a picture: the parameter sets,
/// access unit delimiter, end of sequence and of bitstream, prefix SEI and the
/// reserved types of those kinds (7.4.2.4.4).
const fn ends_picture(kind: u8) -> bool {
    matches!(kind, 32..=37 | 39 | 41..=44 | 48..=55)
}

/// Converts a value to the field type of the C structure, failing when it
/// does not fit.
fn narrow<T: TryFrom<U>, U>(value: U, name: &'static str) -> Result<T, H265Error> {
    T::try_from(value).map_err(|_| H265Error::Invalid(name))
}

/// A picture whose slice segments are being collected.
struct OpenPicture {
    id: FrameId,
    surface: VASurfaceID,
    buffers: PictureBuffers,
    /// The frames `ReferenceFrames` lists, in order.
    references: Vec<FrameId>,
    /// The header of the last slice segment, for the dependent ones.
    last: SliceHeader,
    /// The last slice segment, held until the next one shows it is not the
    /// last of the picture.
    pending: Option<(Box<VASliceParameterBufferHEVC>, Vec<u8>)>,
}

/// Everything that depends on the active sequence parameter set.
struct Session {
    open: Option<OpenPicture>,
    target: Target<FrameId>,
    sequence: Sequence,
    dpb: Dpb,
}

/// A VA-API H.265 decoder.
pub struct HevcDecoder {
    session: Option<Session>,
    converter: NalStreamConverter,
    parameters: ParameterSets,
    device: Rc<Device>,
    /// The slice segments of the current picture are ignored: the picture is
    /// skipped or its decoding failed.
    dropping: bool,
}

#[allow(
    clippy::non_send_fields_in_send_ty,
    reason = "the Rc handles are all owned by the decoder, so they move together"
)]
// SAFETY: the `Rc`s are all held by this value or by the session it owns, so
// moving it moves every handle to the display together, and libva allows a
// display to be used from any one thread at a time, which `&mut self`
// guarantees.
unsafe impl Send for HevcDecoder {}

impl std::fmt::Debug for HevcDecoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HevcDecoder")
            .finish_non_exhaustive()
    }
}

impl HevcDecoder {
    /// Creates a decoder on the first render node that can decode H.265.
    ///
    /// `config` is the `hvcC` record of length-prefixed samples; without it
    /// samples are Annex B.
    ///
    /// # Errors
    ///
    /// Returns a [`VaapiError`] when libva is missing, no render node can
    /// decode H.265 or `config` is malformed.
    pub fn new(config: Option<&[u8]>) -> Result<Self, VaapiError> {
        let converter = converter(config)?;
        // Surface a missing libva once, before looking for devices.
        Api::load()?;
        let nodes = render_nodes()?;
        let mut attempts = Vec::with_capacity(nodes.len());
        for node in nodes {
            match Device::open(Api::load()?, &node).and_then(supports_hevc) {
                Ok(device) => return Ok(Self::with_device(device, converter)),
                Err(error) => attempts.push(format!("{}: {error}", node.display())),
            }
        }
        Err(VaapiError::NoUsableDevice {
            attempts: attempts.join("; "),
        })
    }

    /// Creates a decoder on the render node `path` through `api`, which tests
    /// and the fuzz targets replace with a double.
    ///
    /// # Errors
    ///
    /// See [`Self::new`].
    #[cfg(any(test, feature = "fuzzing"))]
    pub fn with_api(api: Api, path: &Path, config: Option<&[u8]>) -> Result<Self, VaapiError> {
        let converter = converter(config)?;
        let device = supports_hevc(Device::open(api, path)?)?;
        Ok(Self::with_device(device, converter))
    }

    fn with_device(device: Device, converter: NalStreamConverter) -> Self {
        Self {
            session: None,
            converter,
            parameters: ParameterSets::default(),
            device: Rc::new(device),
            dropping: false,
        }
    }

    /// Decodes one access unit and returns the frames the picture buffer
    /// outputs because of it, in output order.
    ///
    /// # Errors
    ///
    /// Returns a [`VaapiError`] when the stream is malformed or uses an
    /// unsupported feature, or the driver fails. The picture being decoded is
    /// dropped; decoding resumes at the next IRAP picture or whenever the
    /// following pictures only refer to intact ones.
    pub(super) fn decode(
        &mut self,
        sample: &[u8],
        timestamp_ns: u64,
    ) -> Result<Vec<DecodedPicture>, VaapiError> {
        let mut output = Vec::new();
        match self.decode_access_unit(sample, timestamp_ns, &mut output) {
            Ok(()) => Ok(output),
            Err(error) => {
                self.abandon_picture();
                Err(error)
            }
        }
    }

    /// Outputs every frame still waiting in the picture buffer, ending the
    /// stream: the next IRAP picture starts a new one.
    ///
    /// # Errors
    ///
    /// Returns a [`VaapiError`] when a picture cannot be completed or read.
    pub(super) fn drain(&mut self) -> Result<Vec<DecodedPicture>, VaapiError> {
        let mut output = Vec::new();
        let result = self.finish_picture(&mut output);
        let flushed = self.flush(&mut output);
        result?;
        flushed?;
        Ok(output)
    }

    fn decode_access_unit(
        &mut self,
        sample: &[u8],
        timestamp_ns: u64,
        output: &mut Vec<DecodedPicture>,
    ) -> Result<(), VaapiError> {
        let annex_b = self
            .converter
            .convert_sample_with_parameter_sets(sample)
            .map_err(|error| VaapiError::Framing(error.to_string()))?;
        for (start, end) in annex_b_nalus(&annex_b) {
            self.nal_unit(&annex_b[start..end], timestamp_ns, output)?;
        }
        // A packet is a whole access unit, so its last picture is complete.
        self.finish_picture(output)
    }

    fn nal_unit(
        &mut self,
        nal: &[u8],
        timestamp_ns: u64,
        output: &mut Vec<DecodedPicture>,
    ) -> Result<(), VaapiError> {
        let header = NalHeader::parse(nal)?;
        // Only the base layer is decoded.
        if header.layer_id > 0 {
            return Ok(());
        }
        let kind = header.kind;
        if ends_picture(kind) {
            self.finish_picture(output)?;
        }
        match kind {
            NalUnitType::VPS => self.parameters.put_vps(nal)?,
            NalUnitType::SPS => {
                self.parameters.put_sps(nal)?;
            }
            NalUnitType::PPS => self.parameters.put_pps(nal)?,
            NalUnitType::EOS | NalUnitType::EOB => self.flush(output)?,
            _ if header.unit_type().is_decodable_slice() => {
                self.slice(nal, header, timestamp_ns, output)?;
            }
            // Delimiters, SEI, reserved types and the rest carry nothing the
            // base layer needs.
            _ => {}
        }
        Ok(())
    }

    fn slice(
        &mut self,
        nal: &[u8],
        nal_header: NalHeader,
        timestamp_ns: u64,
        output: &mut Vec<DecodedPicture>,
    ) -> Result<(), VaapiError> {
        // `first_slice_segment_in_pic_flag` is the first bit after the header.
        let first = nal.get(NalHeader::LEN).is_some_and(|byte| byte & 0x80 != 0);
        if first {
            self.finish_picture(output)?;
            self.dropping = false;
        }
        let open = self
            .session
            .as_ref()
            .and_then(|session| session.open.as_ref());
        if !first && open.is_none() {
            return if self.dropping {
                Ok(())
            } else {
                Err(
                    H265Error::Invalid("a slice segment before the first one of its picture")
                        .into(),
                )
            };
        }
        // The stream's format is checked before the header is read, so an
        // unsupported one is not reported as malformed.
        let slice = self
            .parameters
            .slice_if(nal, open.map(|open| &open.last), |sps| {
                Sequence::supported(sps).map(drop)
            })?;
        if first && !self.begin_picture(&slice, nal_header, timestamp_ns, output)? {
            self.dropping = true;
            return Ok(());
        }
        self.session
            .as_mut()
            .map_or(Ok(()), |session| session.add_slice(&slice, nal))
    }

    /// Starts the picture of `slice`, activating its sequence when it is an
    /// IRAP picture that starts one. Returns `false` when the picture is
    /// skipped.
    fn begin_picture(
        &mut self,
        slice: &Slice,
        nal_header: NalHeader,
        timestamp_ns: u64,
        output: &mut Vec<DecodedPicture>,
    ) -> Result<bool, VaapiError> {
        let kind = nal_header.unit_type();
        let active = self
            .session
            .as_ref()
            .is_some_and(|session| *session.sequence.sps == *slice.sps);
        if !active {
            if !kind.is_irap() {
                if self.session.is_some() {
                    return Err(H265Error::SequenceChange.into());
                }
                tracing::debug!("skipping a picture before the first IRAP picture");
                return Ok(false);
            }
            if let Some(old) = &mut self.session {
                if !old.dpb.starts_sequence(kind) {
                    return Err(H265Error::SequenceChange.into());
                }
                let discard = old
                    .dpb
                    .discards_prior_pictures(&slice.header.picture_header());
                let events = old.dpb.end_sequence(discard);
                old.apply(&events, output)?;
            }
            self.session = None;
            self.session = Some(Session::new(
                &self.device,
                Sequence::new(Arc::clone(&slice.sps))?,
            )?);
        }
        let Some(session) = &mut self.session else {
            return Ok(false);
        };
        match session.begin_picture(slice, timestamp_ns, output) {
            Ok(began) => Ok(began),
            Err(error) => {
                session.abandon_picture();
                Err(error)
            }
        }
    }

    fn finish_picture(&mut self, output: &mut Vec<DecodedPicture>) -> Result<(), VaapiError> {
        self.session
            .as_mut()
            .map_or(Ok(()), |session| session.finish_picture(output))
    }

    fn abandon_picture(&mut self) {
        if let Some(session) = &mut self.session {
            session.abandon_picture();
        }
        self.dropping = true;
    }

    /// Outputs the frames waiting in the picture buffer; the next picture
    /// starts a new sequence.
    fn flush(&mut self, output: &mut Vec<DecodedPicture>) -> Result<(), VaapiError> {
        self.finish_picture(output)?;
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        let events = session.dpb.end_sequence(false);
        session.apply(&events, output)
    }
}

fn converter(config: Option<&[u8]>) -> Result<NalStreamConverter, VaapiError> {
    NalStreamConverter::new(true, config).map_err(|error| VaapiError::Framing(error.to_string()))
}

/// Checks the device decodes H.265 at all.
fn supports_hevc(device: Device) -> Result<Device, VaapiError> {
    for profile in [VAProfileHEVCMain, VAProfileHEVCMain10] {
        if offers_decoding(&device, profile)? {
            return Ok(device);
        }
    }
    Err(VaapiError::NoDecodeEntrypoint)
}

impl Session {
    fn new(device: &Rc<Device>, sequence: Sequence) -> Result<Self, VaapiError> {
        let (profile, format) = match sequence.bit_depth {
            BitDepth::Eight => (VAProfileHEVCMain, SampleFormat::NV12),
            BitDepth::Ten => (VAProfileHEVCMain10, SampleFormat::P010),
        };
        if !offers_decoding(device, profile)? {
            return Err(VaapiError::NoDecodeEntrypoint);
        }
        let sps = &sequence.sps;
        let (width, height) = sequence.output_size();
        let visible = Visible {
            left: sps.window.left,
            top: sps.window.top,
            width,
            height,
        };
        // The frames of the buffer plus the one being decoded.
        let count = sequence.params.max_dec_pic_buffering + 1;
        let target = Target::new(
            device,
            profile,
            format,
            (sps.width, sps.height),
            visible,
            count,
        )?;
        let dpb = Dpb::new(sequence.params.clone()).map_err(H265Error::from)?;
        Ok(Self {
            open: None,
            target,
            sequence,
            dpb,
        })
    }

    /// Drops the picture being collected, as when decoding it failed.
    fn abandon_picture(&mut self) {
        self.open = None;
        if let Some(id) = self.dpb.abort_picture() {
            self.target.release(id);
        }
    }

    fn apply(
        &mut self,
        events: &[Event],
        output: &mut Vec<DecodedPicture>,
    ) -> Result<(), VaapiError> {
        for event in events {
            match *event {
                Event::Output(id) => output.push(self.target.read(id)?),
                Event::Release(id) => self.target.release(id),
            }
        }
        Ok(())
    }

    /// Starts the picture of `slice`. Returns `false` when it is not decoded:
    /// a random access skipped leading picture.
    fn begin_picture(
        &mut self,
        slice: &Slice,
        timestamp_ns: u64,
        output: &mut Vec<DecodedPicture>,
    ) -> Result<bool, VaapiError> {
        let Some(began) = self
            .dpb
            .begin_picture(slice.header.picture_header())
            .map_err(H265Error::from)?
        else {
            return Ok(false);
        };
        self.apply(&began.events, output)?;
        self.target.stamp(began.id, timestamp_ns);
        let surface = self.target.surface_of(began.id)?;
        let (picture, references) = self.picture_parameters(slice, surface)?;
        let mut buffers = self.target.picture_buffers();
        buffers.add(VAPictureParameterBufferType, &*picture)?;
        if let Some(factors) = ScalingFactors::new(&slice.sps, &slice.pps) {
            buffers.add(
                VAIQMatrixBufferType,
                &VAIQMatrixBufferHEVC {
                    ScalingList4x4: factors.list_4x4,
                    ScalingList8x8: factors.list_8x8,
                    ScalingList16x16: factors.list_16x16,
                    ScalingList32x32: factors.list_32x32,
                    ScalingListDC16x16: factors.dc_16x16,
                    ScalingListDC32x32: factors.dc_32x32,
                    va_reserved: [0; 4],
                },
            )?;
        }
        self.open = Some(OpenPicture {
            id: began.id,
            surface,
            buffers,
            references,
            last: slice.header.clone(),
            pending: None,
        });
        Ok(true)
    }

    /// The picture parameters, and the frames listed in `ReferenceFrames`.
    #[allow(
        clippy::too_many_lines,
        reason = "one field of the C structure per line"
    )]
    fn picture_parameters(
        &mut self,
        slice: &Slice,
        surface: VASurfaceID,
    ) -> Result<(Box<VAPictureParameterBufferHEVC>, Vec<FrameId>), VaapiError> {
        let invalid = H265Error::Invalid;
        let current = self
            .dpb
            .current()
            .ok_or_else(|| invalid("no picture is being decoded"))?;
        let sets = self
            .dpb
            .current_sets()
            .ok_or_else(|| invalid("no picture is being decoded"))?
            .clone();
        let mut reference_frames = [VAPictureHEVC::INVALID; VA_HEVC_MAX_REFS];
        let mut references = Vec::new();
        let views: Vec<_> = self.dpb.reference_frames().collect();
        if views.len() > VA_HEVC_MAX_REFS {
            return Err(invalid("more than 15 reference pictures").into());
        }
        for (slot, view) in reference_frames.iter_mut().zip(views) {
            let mut flags = 0;
            if view.long_term {
                flags |= VA_PICTURE_HEVC_LONG_TERM_REFERENCE;
            }
            if sets.st_curr_before.contains(&view.id) {
                flags |= VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE;
            }
            if sets.st_curr_after.contains(&view.id) {
                flags |= VA_PICTURE_HEVC_RPS_ST_CURR_AFTER;
            }
            if sets.lt_curr.contains(&view.id) {
                flags |= VA_PICTURE_HEVC_RPS_LT_CURR;
            }
            *slot = VAPictureHEVC {
                picture_id: self.target.surface_of(view.id)?,
                pic_order_cnt: view.poc,
                flags,
                va_reserved: [0; 4],
            };
            references.push(view.id);
        }
        let sps = &slice.sps;
        let pps = &slice.pps;
        let header = &slice.header;
        let tiling = Tiling::new(sps, pps);
        let mut column_width_minus1 = [0_u16; 19];
        for (slot, width) in column_width_minus1.iter_mut().zip(&tiling.columns) {
            *slot = u16::try_from(width - 1).map_err(|_| invalid("a tile column width"))?;
        }
        let mut row_height_minus1 = [0_u16; 21];
        for (slot, height) in row_height_minus1.iter_mut().zip(&tiling.rows) {
            *slot = u16::try_from(height - 1).map_err(|_| invalid("a tile row height"))?;
        }
        let pcm = sps.pcm;
        let pcm_field =
            |select: fn(&crate::h265::params::Pcm) -> u8| pcm.as_ref().map_or(0, select);
        let kind = header.nal_unit_type;
        let picture = Box::new(VAPictureParameterBufferHEVC {
            CurrPic: VAPictureHEVC {
                picture_id: surface,
                pic_order_cnt: current.poc,
                flags: 0,
                va_reserved: [0; 4],
            },
            ReferenceFrames: reference_frames,
            pic_width_in_luma_samples: u16::try_from(sps.width)
                .map_err(|_| invalid("picture width"))?,
            pic_height_in_luma_samples: u16::try_from(sps.height)
                .map_err(|_| invalid("picture height"))?,
            pic_fields: HevcPictureFields {
                scaling_list_enabled: sps.scaling_list_enabled,
                amp_enabled: sps.amp_enabled,
                strong_intra_smoothing_enabled: sps.strong_intra_smoothing_enabled,
                pcm_enabled: pcm.is_some(),
                transform_skip_enabled: pps.transform_skip_enabled,
                sign_data_hiding_enabled: pps.sign_data_hiding_enabled,
                constrained_intra_pred: pps.constrained_intra_pred,
                cu_qp_delta_enabled: pps.cu_qp_delta_enabled,
                weighted_pred: pps.weighted_pred,
                weighted_bipred: pps.weighted_bipred,
                transquant_bypass_enabled: pps.transquant_bypass_enabled,
                tiles_enabled: pps.tiles.is_some(),
                entropy_coding_sync_enabled: pps.entropy_coding_sync_enabled,
                loop_filter_across_slices_enabled: pps.loop_filter_across_slices,
                loop_filter_across_tiles_enabled: pps
                    .tiles
                    .as_ref()
                    .is_some_and(|tiles| tiles.loop_filter_across),
                pcm_loop_filter_disabled: pcm.is_some_and(|pcm| pcm.loop_filter_disabled),
                no_pic_reordering: sps.max_num_reorder_pics == 0,
            }
            .pack(),
            sps_max_dec_pic_buffering_minus1: sps.max_dec_pic_buffering_minus1,
            bit_depth_luma_minus8: narrow(sps.bit_depth_luma - 8, "bit depth")?,
            bit_depth_chroma_minus8: narrow(sps.bit_depth_chroma - 8, "bit depth")?,
            pcm_sample_bit_depth_luma_minus1: pcm_field(|pcm| pcm.bit_depth_luma_minus1),
            pcm_sample_bit_depth_chroma_minus1: pcm_field(|pcm| pcm.bit_depth_chroma_minus1),
            log2_min_luma_coding_block_size_minus3: sps.log2_min_cb_minus3,
            log2_diff_max_min_luma_coding_block_size: sps.log2_diff_max_min_cb,
            log2_min_transform_block_size_minus2: sps.log2_min_tb_minus2,
            log2_diff_max_min_transform_block_size: sps.log2_diff_max_min_tb,
            log2_min_pcm_luma_coding_block_size_minus3: pcm_field(|pcm| pcm.log2_min_cb_minus3),
            log2_diff_max_min_pcm_luma_coding_block_size: pcm_field(|pcm| pcm.log2_diff_max_min_cb),
            max_transform_hierarchy_depth_intra: sps.max_transform_hierarchy_depth_intra,
            max_transform_hierarchy_depth_inter: sps.max_transform_hierarchy_depth_inter,
            init_qp_minus26: pps.init_qp_minus26,
            diff_cu_qp_delta_depth: pps.diff_cu_qp_delta_depth,
            pps_cb_qp_offset: pps.cb_qp_offset,
            pps_cr_qp_offset: pps.cr_qp_offset,
            log2_parallel_merge_level_minus2: pps.log2_parallel_merge_level_minus2,
            num_tile_columns_minus1: narrow(tiling.columns.len() - 1, "tile columns")?,
            num_tile_rows_minus1: narrow(tiling.rows.len() - 1, "tile rows")?,
            column_width_minus1,
            row_height_minus1,
            slice_parsing_fields: HevcSliceParsingFields {
                lists_modification_present: pps.lists_modification_present,
                long_term_ref_pics_present: sps.long_term_refs.is_some(),
                temporal_mvp_enabled: sps.temporal_mvp_enabled,
                cabac_init_present: pps.cabac_init_present,
                output_flag_present: pps.output_flag_present,
                dependent_slice_segments_enabled: pps.dependent_slice_segments_enabled,
                slice_chroma_qp_offsets_present: pps.slice_chroma_qp_offsets_present,
                sao_enabled: sps.sao_enabled,
                deblocking_filter_override_enabled: pps.deblocking_override_enabled,
                deblocking_disabled: pps.deblocking_disabled,
                slice_segment_header_extension_present: pps.slice_segment_header_extension_present,
                rap_pic: kind.is_irap(),
                idr_pic: kind.is_idr(),
                intra_pic: kind.is_irap(),
            }
            .pack(),
            log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_poc_lsb - 4,
            num_short_term_ref_pic_sets: narrow(
                sps.short_term_rps.len(),
                "num_short_term_ref_pic_sets",
            )?,
            num_long_term_ref_pic_sps: narrow(
                sps.long_term_refs.as_ref().map_or(0, Vec::len),
                "num_long_term_ref_pics_sps",
            )?,
            num_ref_idx_l0_default_active_minus1: pps.num_ref_idx_default_active_minus1[0],
            num_ref_idx_l1_default_active_minus1: pps.num_ref_idx_default_active_minus1[1],
            pps_beta_offset_div2: pps.beta_offset_div2,
            pps_tc_offset_div2: pps.tc_offset_div2,
            num_extra_slice_header_bits: pps.num_extra_slice_header_bits,
            st_rps_bits: header.st_rps_bits,
            va_reserved: [0; 8],
        });
        Ok((picture, references))
    }

    fn add_slice(&mut self, slice: &Slice, nal: &[u8]) -> Result<(), VaapiError> {
        let parameter = self.slice_parameters(slice, nal.len())?;
        let open = self
            .open
            .as_mut()
            .ok_or(H265Error::Invalid("a slice segment without a picture"))?;
        if let Some((previous, data)) = open.pending.take() {
            submit(&mut open.buffers, previous, &data, false)?;
        }
        open.pending = Some((parameter, nal.to_vec()));
        open.last = slice.header.clone();
        Ok(())
    }

    fn slice_parameters(
        &self,
        slice: &Slice,
        nal_len: usize,
    ) -> Result<Box<VASliceParameterBufferHEVC>, VaapiError> {
        let invalid = H265Error::Invalid;
        let header = &slice.header;
        let open = self
            .open
            .as_ref()
            .ok_or_else(|| invalid("a slice segment without a picture"))?;
        let ref_pic_list = self.reference_indices(header, &open.references)?;
        let active = |count: usize| narrow(count.saturating_sub(1), "num_ref_idx_active_minus1");
        let active_minus1 = [
            active(header.num_ref_idx_active[0])?,
            active(header.num_ref_idx_active[1])?,
        ];
        let mut parameter = Box::new(VASliceParameterBufferHEVC {
            slice_data_size: u32::try_from(nal_len)
                .map_err(|_| VaapiError::SizeOutOfRange("slice"))?,
            slice_data_offset: 0,
            slice_data_flag: VA_SLICE_DATA_FLAG_ALL,
            slice_data_byte_offset: u32::try_from(slice.data_byte_offset)
                .map_err(|_| VaapiError::SizeOutOfRange("slice header"))?,
            slice_segment_address: header.segment_address,
            RefPicList: ref_pic_list,
            LongSliceFlags: HevcLongSliceFlags {
                last_slice_of_pic: false,
                dependent_slice_segment: header.dependent,
                slice_type: match header.slice_type {
                    SliceType::B => 0,
                    SliceType::P => 1,
                    SliceType::I => 2,
                },
                sao_luma: header.sao_luma,
                sao_chroma: header.sao_chroma,
                mvd_l1_zero: header.mvd_l1_zero,
                cabac_init: header.cabac_init,
                temporal_mvp: header.temporal_mvp,
                deblocking_disabled: header.deblocking_disabled,
                collocated_from_l0: header.collocated_from_l0,
                loop_filter_across_slices: header.loop_filter_across_slices,
            }
            .pack(),
            collocated_ref_idx: if header.temporal_mvp && header.slice_type != SliceType::I {
                header.collocated_ref_idx
            } else {
                VA_HEVC_REF_INVALID
            },
            num_ref_idx_l0_active_minus1: active_minus1[0],
            num_ref_idx_l1_active_minus1: active_minus1[1],
            slice_qp_delta: header.qp_delta,
            slice_cb_qp_offset: header.cb_qp_offset,
            slice_cr_qp_offset: header.cr_qp_offset,
            slice_beta_offset_div2: header.beta_offset_div2,
            slice_tc_offset_div2: header.tc_offset_div2,
            luma_log2_weight_denom: 0,
            delta_chroma_log2_weight_denom: 0,
            delta_luma_weight_l0: [0; VA_HEVC_MAX_REFS],
            luma_offset_l0: [0; VA_HEVC_MAX_REFS],
            delta_chroma_weight_l0: [[0; 2]; VA_HEVC_MAX_REFS],
            ChromaOffsetL0: [[0; 2]; VA_HEVC_MAX_REFS],
            delta_luma_weight_l1: [0; VA_HEVC_MAX_REFS],
            luma_offset_l1: [0; VA_HEVC_MAX_REFS],
            delta_chroma_weight_l1: [[0; 2]; VA_HEVC_MAX_REFS],
            ChromaOffsetL1: [[0; 2]; VA_HEVC_MAX_REFS],
            five_minus_max_num_merge_cand: header.five_minus_max_num_merge_cand,
            num_entry_point_offsets: u16::try_from(header.num_entry_points)
                .map_err(|_| invalid("num_entry_point_offsets"))?,
            entry_offset_to_subset_array: 0,
            slice_data_num_emu_prevn_bytes: u16::try_from(slice.emulation_prevention_bytes)
                .map_err(|_| invalid("a slice header with more than 65535 escapes"))?,
            va_reserved: [0; 2],
        });
        if let Some(weights) = &header.weights {
            parameter.luma_log2_weight_denom = weights.luma_log2_denom;
            parameter.delta_chroma_log2_weight_denom = weights.delta_chroma_log2_denom;
            let [list0, list1] = &weights.lists;
            parameter.delta_luma_weight_l0 = list0.delta_luma_weight;
            parameter.luma_offset_l0 = list0.luma_offset;
            parameter.delta_chroma_weight_l0 = list0.delta_chroma_weight;
            parameter.ChromaOffsetL0 = list0.chroma_offset;
            parameter.delta_luma_weight_l1 = list1.delta_luma_weight;
            parameter.luma_offset_l1 = list1.luma_offset;
            parameter.delta_chroma_weight_l1 = list1.delta_chroma_weight;
            parameter.ChromaOffsetL1 = list1.chroma_offset;
        }
        Ok(parameter)
    }

    /// The reference lists of a slice as indices into `ReferenceFrames`.
    fn reference_indices(
        &self,
        header: &SliceHeader,
        references: &[FrameId],
    ) -> Result<[[u8; VA_HEVC_MAX_REFS]; 2], VaapiError> {
        let mut lists = [[VA_HEVC_REF_INVALID; VA_HEVC_MAX_REFS]; 2];
        let Some(kind) = header.slice_type.list_kind() else {
            return Ok(lists);
        };
        let entries = self
            .dpb
            .reference_lists(
                kind,
                header.num_ref_idx_active,
                [
                    header.list_entries[0].as_deref(),
                    header.list_entries[1].as_deref(),
                ],
            )
            .map_err(H265Error::from)?;
        for (list, entries) in lists.iter_mut().zip(&entries) {
            for (slot, id) in list.iter_mut().zip(entries) {
                let index = references
                    .iter()
                    .position(|reference| reference == id)
                    .ok_or(H265Error::Invalid(
                        "a reference list names a picture that is not a reference",
                    ))?;
                *slot = u8::try_from(index).map_err(|_| H265Error::Invalid("reference index"))?;
            }
        }
        Ok(lists)
    }

    /// Decodes the collected picture and updates the picture buffer.
    fn finish_picture(&mut self, output: &mut Vec<DecodedPicture>) -> Result<(), VaapiError> {
        let Some(mut open) = self.open.take() else {
            return Ok(());
        };
        let rendered = match open.pending.take() {
            Some((parameter, data)) => submit(&mut open.buffers, parameter, &data, true)
                .and_then(|()| open.buffers.render(open.surface)),
            None => Err(H265Error::Invalid("a picture without slice segments").into()),
        };
        if let Err(error) = rendered {
            self.dpb.abort_picture();
            self.target.release(open.id);
            return Err(error);
        }
        drop(open.buffers);
        match self.dpb.finish_picture() {
            Ok(events) => self.apply(&events, output),
            Err(error) => {
                self.target.release(open.id);
                Err(H265Error::from(error).into())
            }
        }
    }
}

/// Adds a slice segment to the buffers of its picture.
fn submit(
    buffers: &mut PictureBuffers,
    mut parameter: Box<VASliceParameterBufferHEVC>,
    data: &[u8],
    last: bool,
) -> Result<(), VaapiError> {
    if last {
        parameter.LongSliceFlags |= 1;
    }
    buffers.add(VASliceParameterBufferType, &*parameter)?;
    buffers.add_bytes(VASliceDataBufferType, data)
}
