//! The H.264 decoder over a VA-API render node.
//!
//! The bitstream is parsed here and the picture buffer is managed by
//! [`crate::h264`]; the hardware only reconstructs pictures. Every packet is
//! one complete access unit, as the other decoder backends take them. A
//! picture's slices are collected and submitted when the next picture, a
//! parameter set or the end of the packet shows the picture is complete, and
//! each submission waits for the hardware, so frames are read back in the order
//! the picture buffer outputs them.

use super::{
    VaapiError,
    api::Api,
    device::{Config, Context, Device, Image, PictureBuffers, Surfaces, render_nodes, to_usize},
    plan::Nv12Planes,
    types::{
        DecodePictureFields, DecodeSequenceFields, VA_ATTRIB_NOT_SUPPORTED, VA_INVALID_ID,
        VA_PICTURE_H264_LONG_TERM_REFERENCE, VA_PICTURE_H264_NON_EXISTING,
        VA_PICTURE_H264_SHORT_TERM_REFERENCE, VA_RT_FORMAT_YUV420, VA_SLICE_DATA_FLAG_ALL,
        VAConfigAttrib, VAConfigAttribRTFormat, VAEntrypointVLD, VAIQMatrixBufferH264,
        VAIQMatrixBufferType, VAPictureH264, VAPictureParameterBufferH264,
        VAPictureParameterBufferType, VAProfileH264ConstrainedBaseline, VAProfileH264High,
        VAProfileH264Main, VASliceDataBufferType, VASliceParameterBufferH264,
        VASliceParameterBufferType, VASurfaceID,
    },
};
use crate::{
    annexb::annex_b_nalus,
    bitstream::NalStreamConverter,
    h264::{
        H264Error,
        dpb::{Dpb, Event, FrameId, FrameView, ListKind},
        sequence::{Crop, ScalingLists, Sequence},
        slice::{ParameterSets, PredWeights, Slice, SliceHeader, SliceKind},
    },
};
use h264_reader::nal::{
    pps::PicParameterSet,
    sps::{PicOrderCntType, Profile, SeqParameterSet},
};
#[cfg(any(test, feature = "fuzzing"))]
use std::path::Path;
use std::{collections::HashMap, ffi::c_int, rc::Rc};

/// `nal_unit_type` of a coded slice of a non-IDR picture.
const NAL_SLICE: u8 = 1;
/// `nal_unit_type` of a coded slice of an IDR picture.
const NAL_SLICE_IDR: u8 = 5;
/// The NAL unit types that cannot continue a picture: SEI, SPS, PPS, access
/// unit delimiter, end of sequence and end of stream (7.4.1.2.3).
const NAL_AU_BOUNDARY: std::ops::RangeInclusive<u8> = 6..=11;
/// The longest reference list libva carries.
const MAX_LIST_LEN: usize = 32;

/// One decoded frame: tightly packed 8-bit NV12 of the cropped size.
#[derive(Debug)]
pub struct DecodedPicture {
    /// The planes, luma first, then interleaved chroma.
    pub data: Vec<u8>,
    /// The frame width in samples.
    pub width: u32,
    /// The frame height in samples.
    pub height: u32,
    /// The timestamp of the packet the picture came in.
    pub timestamp_ns: u64,
}

/// A picture whose slices are being collected.
struct OpenPicture {
    id: FrameId,
    surface: VASurfaceID,
    buffers: PictureBuffers,
    /// The header of the last slice, to recognise the next picture.
    last: SliceHeader,
}

/// Everything that depends on the active sequence parameter set.
struct Session {
    // Declaration order is teardown order; the device goes last.
    open: Option<OpenPicture>,
    image: Image,
    context: Context,
    surfaces: Surfaces,
    _config: Config,
    device: Rc<Device>,
    sequence: Sequence,
    dpb: Dpb,
    /// The surfaces not holding a frame.
    free: Vec<usize>,
    /// The surface index of each frame in the buffer.
    assigned: HashMap<FrameId, usize>,
    timestamps: HashMap<FrameId, u64>,
    coded_width: u32,
    coded_height: u32,
}

/// A VA-API H.264 decoder.
pub struct VaapiDecoder {
    session: Option<Session>,
    converter: NalStreamConverter,
    parameters: ParameterSets,
    device: Rc<Device>,
}

#[allow(
    clippy::non_send_fields_in_send_ty,
    reason = "the Rc handles are all owned by the decoder, so they move together"
)]
// SAFETY: the `Rc`s are all held by this value or by the session it owns, so
// moving it moves every handle to the display together, and libva allows a
// display to be used from any one thread at a time, which `&mut self`
// guarantees.
unsafe impl Send for VaapiDecoder {}

impl std::fmt::Debug for VaapiDecoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VaapiDecoder")
            .finish_non_exhaustive()
    }
}

impl VaapiDecoder {
    /// Creates a decoder on the first render node that can decode H.264.
    ///
    /// `config` is the `avcC` record of length-prefixed samples; without it
    /// samples are Annex B.
    ///
    /// # Errors
    ///
    /// Returns a [`VaapiError`] when libva is missing, no render node can
    /// decode H.264 or `config` is malformed.
    pub fn new(config: Option<&[u8]>) -> Result<Self, VaapiError> {
        let converter = converter(config)?;
        // Surface a missing libva once, before looking for devices.
        Api::load()?;
        let nodes = render_nodes()?;
        let mut attempts = Vec::with_capacity(nodes.len());
        for node in nodes {
            match Device::open(Api::load()?, &node).and_then(supports_h264) {
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
        let device = supports_h264(Device::open(api, path)?)?;
        Ok(Self::with_device(device, converter))
    }

    fn with_device(device: Device, converter: NalStreamConverter) -> Self {
        Self {
            session: None,
            converter,
            parameters: ParameterSets::default(),
            device: Rc::new(device),
        }
    }

    /// Decodes one access unit and returns the frames the picture buffer
    /// outputs because of it, in output order.
    ///
    /// # Errors
    ///
    /// Returns a [`VaapiError`] when the stream is malformed or uses an
    /// unsupported feature, or the driver fails. The picture being decoded is
    /// dropped; decoding resumes at the next IDR picture or whenever the
    /// following pictures only refer to intact ones.
    pub fn decode(
        &mut self,
        sample: &[u8],
        timestamp_ns: u64,
    ) -> Result<Vec<DecodedPicture>, VaapiError> {
        let mut output = Vec::new();
        match self.decode_access_unit(sample, timestamp_ns, &mut output) {
            Ok(()) => Ok(output),
            Err(error) => {
                if let Some(session) = &mut self.session {
                    session.abandon_picture();
                }
                Err(error)
            }
        }
    }

    /// Outputs every frame still waiting in the picture buffer, ending the
    /// stream: the next IDR picture starts a new one.
    ///
    /// # Errors
    ///
    /// Returns a [`VaapiError`] when a picture cannot be completed or read.
    pub fn drain(&mut self) -> Result<Vec<DecodedPicture>, VaapiError> {
        let mut output = Vec::new();
        let result = self.finish_picture(&mut output);
        let drained = end_sequence(&mut self.session, &mut output);
        result?;
        drained?;
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
        let header = nal[0];
        if header & 0x80 != 0 {
            return Err(H264Error::Invalid("forbidden_zero_bit is set").into());
        }
        let kind = header & 0x1f;
        if NAL_AU_BOUNDARY.contains(&kind) || (14..=18).contains(&kind) {
            self.finish_picture(output)?;
        }
        match kind {
            7 => self.parameters.put_sps(nal)?,
            8 => self.parameters.put_pps(nal)?,
            // Data partitions are rejected by the slice parser.
            NAL_SLICE | NAL_SLICE_IDR | 2..=4 => self.slice(nal, timestamp_ns, output)?,
            // Delimiters, SEI, filler, extensions and the rest carry nothing
            // the base view needs.
            _ => {}
        }
        Ok(())
    }

    fn slice(
        &mut self,
        nal: &[u8],
        timestamp_ns: u64,
        output: &mut Vec<DecodedPicture>,
    ) -> Result<(), VaapiError> {
        let slice = self.parameters.slice(nal)?;
        if slice.header.redundant_pic_cnt > 0 {
            return Ok(());
        }
        let continues = self
            .session
            .as_ref()
            .and_then(|session| session.open.as_ref())
            .is_some_and(|open| !slice.header.starts_new_picture(&open.last));
        if !continues {
            self.finish_picture(output)?;
            if !self.begin_picture(&slice, timestamp_ns, output)? {
                return Ok(());
            }
        }
        self.session
            .as_mut()
            .map_or(Ok(()), |session| session.add_slice(&slice, nal))
    }

    /// Starts the picture of `slice`, activating its sequence when it is an
    /// IDR picture. Returns `false` when the picture is skipped because the
    /// stream has not reached an IDR picture yet.
    fn begin_picture(
        &mut self,
        slice: &Slice,
        timestamp_ns: u64,
        output: &mut Vec<DecodedPicture>,
    ) -> Result<bool, VaapiError> {
        let (sps, pps) = self.parameters.for_picture(slice.header.pps_id)?;
        let active = self
            .session
            .as_ref()
            .is_some_and(|session| session.sequence.sps == *sps);
        if !active {
            if !slice.header.picture.idr {
                if self.session.is_some() {
                    return Err(H264Error::SequenceChange.into());
                }
                tracing::debug!("skipping a picture before the first IDR picture");
                return Ok(false);
            }
            end_sequence(&mut self.session, output)?;
            self.session = Some(Session::new(&self.device, Sequence::new(sps)?)?);
        }
        let Some(session) = &mut self.session else {
            return Ok(false);
        };
        if let Err(error) = session.begin_picture(slice, sps, pps, timestamp_ns, output) {
            session.abandon_picture();
            return Err(error);
        }
        Ok(true)
    }

    fn finish_picture(&mut self, output: &mut Vec<DecodedPicture>) -> Result<(), VaapiError> {
        self.session
            .as_mut()
            .map_or(Ok(()), |session| session.finish_picture(output))
    }
}

fn converter(config: Option<&[u8]>) -> Result<NalStreamConverter, VaapiError> {
    NalStreamConverter::new(false, config).map_err(|error| VaapiError::Framing(error.to_string()))
}

/// The profiles to try for a stream, best fit first: the decoders of the
/// larger profiles decode the smaller ones.
const fn profile_candidates(profile: &Profile) -> &'static [c_int] {
    match profile {
        Profile::Baseline | Profile::ConstrainedBaseline => &[
            VAProfileH264ConstrainedBaseline,
            VAProfileH264Main,
            VAProfileH264High,
        ],
        Profile::Main => &[VAProfileH264Main, VAProfileH264High],
        _ => &[VAProfileH264High],
    }
}

fn offers_decoding(device: &Device, profile: c_int) -> Result<bool, VaapiError> {
    Ok(device
        .entrypoints(profile)?
        .is_some_and(|entrypoints| entrypoints.contains(&VAEntrypointVLD)))
}

/// Checks the device decodes H.264 at all.
fn supports_h264(device: Device) -> Result<Device, VaapiError> {
    for profile in [
        VAProfileH264ConstrainedBaseline,
        VAProfileH264Main,
        VAProfileH264High,
    ] {
        if offers_decoding(&device, profile)? {
            return Ok(device);
        }
    }
    Err(VaapiError::NoDecodeEntrypoint)
}

/// Outputs the frames left in the session's picture buffer and ends it.
fn end_sequence(
    session: &mut Option<Session>,
    output: &mut Vec<DecodedPicture>,
) -> Result<(), VaapiError> {
    let Some(mut ended) = session.take() else {
        return Ok(());
    };
    let events = ended.dpb.drain();
    ended.apply(&events, output)
}

impl Session {
    fn new(device: &Rc<Device>, sequence: Sequence) -> Result<Self, VaapiError> {
        let mut profile = None;
        for &candidate in profile_candidates(&sequence.sps.profile()) {
            if offers_decoding(device, candidate)? {
                profile = Some(candidate);
                break;
            }
        }
        let profile = profile.ok_or(VaapiError::NoDecodeEntrypoint)?;
        let config = create_config(device, profile)?;
        let coded_width = sequence.width_mbs * 16;
        let coded_height = sequence.height_mbs * 16;
        // The frames of the buffer plus the one being decoded.
        let count = sequence.dpb.capacity + 1;
        let surfaces = Surfaces::create(device, coded_width, coded_height, count)?;
        let context = Context::create(device, &config, coded_width, coded_height, &surfaces)?;
        let image = Image::create(device, coded_width, coded_height)?;
        let dpb = Dpb::new(sequence.dpb.clone()).map_err(H264Error::from)?;
        Ok(Self {
            open: None,
            image,
            context,
            surfaces,
            _config: config,
            device: Rc::clone(device),
            sequence,
            dpb,
            free: (0..count).rev().collect(),
            assigned: HashMap::new(),
            timestamps: HashMap::new(),
            coded_width,
            coded_height,
        })
    }

    /// Drops the picture being collected, as when decoding it failed.
    fn abandon_picture(&mut self) {
        self.open = None;
        if let Some(id) = self.dpb.abort_picture() {
            self.release(id);
        }
    }

    fn release(&mut self, id: FrameId) {
        if let Some(index) = self.assigned.remove(&id) {
            self.free.push(index);
        }
        self.timestamps.remove(&id);
    }

    /// The surface of a frame, taking a free one for a frame that has none:
    /// the frames a `frame_num` gap inferred are never decoded.
    fn surface_of(&mut self, id: FrameId) -> Result<VASurfaceID, VaapiError> {
        let index = if let Some(&index) = self.assigned.get(&id) {
            index
        } else {
            let index = self.free.pop().ok_or(VaapiError::NoFreeSurface)?;
            self.assigned.insert(id, index);
            index
        };
        Ok(self.surfaces.ids[index])
    }

    fn apply(
        &mut self,
        events: &[Event],
        output: &mut Vec<DecodedPicture>,
    ) -> Result<(), VaapiError> {
        for event in events {
            match *event {
                Event::Output(id) => output.push(self.read(id)?),
                Event::Release(id) => self.release(id),
            }
        }
        Ok(())
    }

    fn begin_picture(
        &mut self,
        slice: &Slice,
        sps: &SeqParameterSet,
        pps: &PicParameterSet,
        timestamp_ns: u64,
        output: &mut Vec<DecodedPicture>,
    ) -> Result<(), VaapiError> {
        let began = self
            .dpb
            .begin_picture(slice.header.picture.clone())
            .map_err(H264Error::from)?;
        self.apply(&began.events, output)?;
        self.timestamps.insert(began.id, timestamp_ns);
        let surface = self.surface_of(began.id)?;
        let mut buffers = PictureBuffers::new(&self.device, self.context.id);
        buffers.add(
            VAPictureParameterBufferType,
            &self.picture_parameters(slice, sps, pps, surface)?,
        )?;
        let scaling = ScalingLists::new(sps, pps);
        buffers.add(
            VAIQMatrixBufferType,
            &VAIQMatrixBufferH264 {
                ScalingList4x4: scaling.lists_4x4,
                ScalingList8x8: scaling.lists_8x8,
                va_reserved: [0; 4],
            },
        )?;
        self.open = Some(OpenPicture {
            id: began.id,
            surface,
            buffers,
            last: slice.header.clone(),
        });
        Ok(())
    }

    fn picture_parameters(
        &mut self,
        slice: &Slice,
        sps: &SeqParameterSet,
        pps: &PicParameterSet,
        surface: VASurfaceID,
    ) -> Result<VAPictureParameterBufferH264, VaapiError> {
        let current = self
            .dpb
            .current()
            .ok_or(H264Error::Invalid("no picture is being decoded"))?;
        let mut reference_frames = [VAPictureH264::INVALID; 16];
        let references: Vec<FrameView> = self.dpb.reference_frames().collect();
        for (slot, view) in reference_frames.iter_mut().zip(references) {
            *slot = self.picture(view)?;
        }
        let narrow = |value: i32, name: &'static str| {
            i8::try_from(value).map_err(|_| H264Error::Invalid(name))
        };
        let extension = pps.extension.as_ref();
        let header = &slice.header;
        Ok(VAPictureParameterBufferH264 {
            CurrPic: VAPictureH264 {
                picture_id: surface,
                frame_idx: current.frame_num,
                flags: 0,
                TopFieldOrderCnt: current.top,
                BottomFieldOrderCnt: current.bottom,
                va_reserved: [0; 4],
            },
            ReferenceFrames: reference_frames,
            picture_width_in_mbs_minus1: u16::try_from(self.sequence.width_mbs - 1)
                .map_err(|_| H264Error::Invalid("picture width"))?,
            picture_height_in_mbs_minus1: u16::try_from(self.sequence.height_mbs - 1)
                .map_err(|_| H264Error::Invalid("picture height"))?,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            num_ref_frames: u8::try_from(self.sequence.dpb.max_num_ref_frames)
                .map_err(|_| H264Error::Invalid("max_num_ref_frames"))?,
            seq_fields: {
                // The least significant bits only exist for type 0.
                let (pic_order_cnt_type, log2_max_poc_lsb, delta_pic_order_always_zero) =
                    match &sps.pic_order_cnt {
                        PicOrderCntType::TypeZero {
                            log2_max_pic_order_cnt_lsb_minus4,
                        } => (0, u32::from(*log2_max_pic_order_cnt_lsb_minus4) + 4, false),
                        PicOrderCntType::TypeOne {
                            delta_pic_order_always_zero_flag,
                            ..
                        } => (1, 4, *delta_pic_order_always_zero_flag),
                        PicOrderCntType::TypeTwo => (2, 4, false),
                    };
                DecodeSequenceFields {
                    gaps_in_frame_num_allowed: sps.gaps_in_frame_num_value_allowed_flag,
                    direct_8x8_inference: sps.direct_8x8_inference_flag,
                    min_luma_bi_pred_size_8x8: sps.level_idc >= 31,
                    log2_max_frame_num: u32::from(sps.log2_max_frame_num()),
                    pic_order_cnt_type,
                    log2_max_poc_lsb,
                    delta_pic_order_always_zero,
                }
                .pack()
            },
            num_slice_groups_minus1: 0,
            slice_group_map_type: 0,
            slice_group_change_rate_minus1: 0,
            pic_init_qp_minus26: narrow(pps.pic_init_qp_minus26, "pic_init_qp_minus26")?,
            pic_init_qs_minus26: narrow(pps.pic_init_qs_minus26, "pic_init_qs_minus26")?,
            chroma_qp_index_offset: narrow(pps.chroma_qp_index_offset, "chroma_qp_index_offset")?,
            second_chroma_qp_index_offset: narrow(
                extension.map_or(pps.chroma_qp_index_offset, |extension| {
                    extension.second_chroma_qp_index_offset
                }),
                "second_chroma_qp_index_offset",
            )?,
            pic_fields: DecodePictureFields {
                entropy_coding_mode: pps.entropy_coding_mode_flag,
                weighted_pred: pps.weighted_pred_flag,
                weighted_bipred_idc: u32::from(pps.weighted_bipred_idc),
                transform_8x8_mode: extension
                    .is_some_and(|extension| extension.transform_8x8_mode_flag),
                constrained_intra_pred: pps.constrained_intra_pred_flag,
                pic_order_present: pps.bottom_field_pic_order_in_frame_present_flag,
                deblocking_filter_control_present: pps.deblocking_filter_control_present_flag,
                redundant_pic_cnt_present: pps.redundant_pic_cnt_present_flag,
                reference_pic: header.picture.nal_ref_idc != 0,
            }
            .pack(),
            frame_num: u16::try_from(header.picture.frame_num)
                .map_err(|_| H264Error::Invalid("frame_num"))?,
            va_reserved: [0; 8],
        })
    }

    /// The `VAPictureH264` of a reference frame.
    fn picture(&mut self, view: FrameView) -> Result<VAPictureH264, VaapiError> {
        let mut flags = if view.long_term_idx.is_some() {
            VA_PICTURE_H264_LONG_TERM_REFERENCE
        } else {
            VA_PICTURE_H264_SHORT_TERM_REFERENCE
        };
        if view.non_existing {
            flags |= VA_PICTURE_H264_NON_EXISTING;
        }
        Ok(VAPictureH264 {
            picture_id: self.surface_of(view.id)?,
            frame_idx: view.long_term_idx.unwrap_or(view.frame_num),
            flags,
            TopFieldOrderCnt: view.top,
            BottomFieldOrderCnt: view.bottom,
            va_reserved: [0; 4],
        })
    }

    fn add_slice(&mut self, slice: &Slice, nal: &[u8]) -> Result<(), VaapiError> {
        let parameter = self.slice_parameters(slice, nal.len())?;
        let open = self
            .open
            .as_mut()
            .ok_or(H264Error::Invalid("a slice without a picture"))?;
        open.buffers.add(VASliceParameterBufferType, &*parameter)?;
        open.buffers.add_bytes(VASliceDataBufferType, nal)?;
        open.last = slice.header.clone();
        Ok(())
    }

    fn slice_parameters(
        &mut self,
        slice: &Slice,
        nal_len: usize,
    ) -> Result<Box<VASliceParameterBufferH264>, VaapiError> {
        let header = &slice.header;
        let macroblocks = self.sequence.width_mbs * self.sequence.height_mbs;
        if header.first_mb >= macroblocks {
            return Err(H264Error::Invalid("first_mb_in_slice is outside the picture").into());
        }
        let [list0, list1] = self.reference_pictures(slice)?;
        let active = |count: usize| u8::try_from(count.saturating_sub(1)).unwrap_or(u8::MAX);
        let mut parameter = Box::new(VASliceParameterBufferH264 {
            slice_data_size: u32::try_from(nal_len)
                .map_err(|_| VaapiError::SizeOutOfRange("slice"))?,
            slice_data_offset: 0,
            slice_data_flag: VA_SLICE_DATA_FLAG_ALL,
            slice_data_bit_offset: u16::try_from(slice.data_bit_offset)
                .map_err(|_| H264Error::Invalid("the slice header is longer than 65535 bits"))?,
            first_mb_in_slice: u16::try_from(header.first_mb)
                .map_err(|_| H264Error::Invalid("first_mb_in_slice"))?,
            slice_type: header.kind.va_slice_type(),
            direct_spatial_mv_pred_flag: u8::from(header.direct_spatial_mv_pred),
            num_ref_idx_l0_active_minus1: active(header.num_ref_idx_active[0]),
            num_ref_idx_l1_active_minus1: active(header.num_ref_idx_active[1]),
            cabac_init_idc: header.cabac_init_idc,
            slice_qp_delta: header.slice_qp_delta,
            disable_deblocking_filter_idc: header.disable_deblocking_filter_idc,
            slice_alpha_c0_offset_div2: header.slice_alpha_c0_offset_div2,
            slice_beta_offset_div2: header.slice_beta_offset_div2,
            RefPicList0: list0,
            RefPicList1: list1,
            luma_log2_weight_denom: 0,
            chroma_log2_weight_denom: 0,
            luma_weight_l0_flag: 0,
            luma_weight_l0: [0; 32],
            luma_offset_l0: [0; 32],
            chroma_weight_l0_flag: 0,
            chroma_weight_l0: [[0; 2]; 32],
            chroma_offset_l0: [[0; 2]; 32],
            luma_weight_l1_flag: 0,
            luma_weight_l1: [0; 32],
            luma_offset_l1: [0; 32],
            chroma_weight_l1_flag: 0,
            chroma_weight_l1: [[0; 2]; 32],
            chroma_offset_l1: [[0; 2]; 32],
            va_reserved: [0; 4],
        });
        if let Some(weights) = &header.weights {
            apply_weights(weights, &mut parameter);
        }
        Ok(parameter)
    }

    /// The reference lists of a slice as libva takes them.
    fn reference_pictures(
        &mut self,
        slice: &Slice,
    ) -> Result<[[VAPictureH264; 32]; 2], VaapiError> {
        let header = &slice.header;
        let mut lists = [[VAPictureH264::INVALID; MAX_LIST_LEN]; 2];
        let kind = match header.kind {
            SliceKind::I => return Ok(lists),
            SliceKind::P => ListKind::P,
            SliceKind::B => ListKind::B,
        };
        if header
            .num_ref_idx_active
            .iter()
            .any(|&count| count > MAX_LIST_LEN)
        {
            return Err(H264Error::Invalid("more than 32 active reference pictures").into());
        }
        let entries = self
            .dpb
            .reference_lists(
                kind,
                header.num_ref_idx_active,
                [&header.modifications[0], &header.modifications[1]],
            )
            .map_err(H264Error::from)?;
        for (list, entries) in lists.iter_mut().zip(&entries) {
            for (slot, entry) in list.iter_mut().zip(entries) {
                if let Some(id) = *entry {
                    let view = self
                        .dpb
                        .frame(id)
                        .ok_or(H264Error::Invalid("a reference list names a dropped frame"))?;
                    *slot = self.picture(view)?;
                }
            }
        }
        Ok(lists)
    }

    /// Decodes the collected picture and updates the picture buffer.
    fn finish_picture(&mut self, output: &mut Vec<DecodedPicture>) -> Result<(), VaapiError> {
        let Some(mut open) = self.open.take() else {
            return Ok(());
        };
        if let Err(error) = open.buffers.render(open.surface) {
            self.abandon_picture_of(open.id);
            return Err(error);
        }
        drop(open.buffers);
        match self.dpb.finish_picture() {
            Ok(events) => self.apply(&events, output),
            Err(error) => {
                self.release(open.id);
                Err(H264Error::from(error).into())
            }
        }
    }

    fn abandon_picture_of(&mut self, id: FrameId) {
        self.dpb.abort_picture();
        self.release(id);
    }

    /// Reads a decoded frame back, cropped and tightly packed.
    fn read(&self, id: FrameId) -> Result<DecodedPicture, VaapiError> {
        let (&index, &timestamp_ns) =
            self.assigned
                .get(&id)
                .zip(self.timestamps.get(&id))
                .ok_or(H264Error::Invalid(
                    "output of a frame that was never decoded",
                ))?;
        let device = &*self.device;
        let image = &self.image.image;
        // SAFETY: the surface and the image belong to this display, and the
        // surface was synchronised when its picture was rendered.
        let status = unsafe {
            (device.api.get_image)(
                device.display,
                self.surfaces.ids[index],
                0,
                0,
                self.coded_width,
                self.coded_height,
                image.image_id,
            )
        };
        device.check("vaGetImage", status)?;
        if image.num_planes != 2 {
            return Err(VaapiError::BufferTooSmall("image planes"));
        }
        let planes = Nv12Planes {
            y_offset: to_usize(image.offsets[0])?,
            y_pitch: to_usize(image.pitches[0])?,
            uv_offset: to_usize(image.offsets[1])?,
            uv_pitch: to_usize(image.pitches[1])?,
        };
        let crop = self.sequence.crop;
        let (width, height) = (self.sequence.width, self.sequence.height);
        let data =
            device.with_mapped_bytes(image.buf, to_usize(image.data_size)?, "image", |bytes| {
                copy_nv12_cropped(
                    bytes,
                    planes,
                    (self.coded_width, self.coded_height),
                    crop,
                    (width, height),
                )
            })?;
        Ok(DecodedPicture {
            data,
            width,
            height,
            timestamp_ns,
        })
    }
}

fn create_config(device: &Rc<Device>, profile: c_int) -> Result<Config, VaapiError> {
    let mut attribute = VAConfigAttrib {
        type_: VAConfigAttribRTFormat,
        value: 0,
    };
    // SAFETY: the pointer refers to the one attribute counted.
    let status = unsafe {
        (device.api.get_config_attributes)(
            device.display,
            profile,
            VAEntrypointVLD,
            &raw mut attribute,
            1,
        )
    };
    device.check("vaGetConfigAttributes", status)?;
    if attribute.value == VA_ATTRIB_NOT_SUPPORTED || attribute.value & VA_RT_FORMAT_YUV420 == 0 {
        return Err(VaapiError::UnsupportedRtFormat);
    }
    attribute.value = VA_RT_FORMAT_YUV420;
    let mut id = VA_INVALID_ID;
    // SAFETY: the pointers refer to live locals; one attribute is counted.
    let status = unsafe {
        (device.api.create_config)(
            device.display,
            profile,
            VAEntrypointVLD,
            &raw mut attribute,
            1,
            &raw mut id,
        )
    };
    device.check("vaCreateConfig", status)?;
    Ok(Config::new(Rc::clone(device), id))
}

/// Fills the explicit prediction weights of a slice. Entries without explicit
/// weights get the default ones, which weigh like no weighting at all.
pub(super) fn apply_weights(weights: &PredWeights, parameter: &mut VASliceParameterBufferH264) {
    parameter.luma_log2_weight_denom = weights.luma_log2_denom;
    parameter.chroma_log2_weight_denom = weights.chroma_log2_denom;
    let (luma, chroma) = fill_list(
        weights,
        0,
        &mut parameter.luma_weight_l0,
        &mut parameter.luma_offset_l0,
        &mut parameter.chroma_weight_l0,
        &mut parameter.chroma_offset_l0,
    );
    parameter.luma_weight_l0_flag = u8::from(luma);
    parameter.chroma_weight_l0_flag = u8::from(chroma);
    let (luma, chroma) = fill_list(
        weights,
        1,
        &mut parameter.luma_weight_l1,
        &mut parameter.luma_offset_l1,
        &mut parameter.chroma_weight_l1,
        &mut parameter.chroma_offset_l1,
    );
    parameter.luma_weight_l1_flag = u8::from(luma);
    parameter.chroma_weight_l1_flag = u8::from(chroma);
}

/// Fills one list's weight arrays and reports whether any entry carries
/// explicit luma and chroma weights.
fn fill_list(
    weights: &PredWeights,
    list: usize,
    luma_weight: &mut [i16; 32],
    luma_offset: &mut [i16; 32],
    chroma_weight: &mut [[i16; 2]; 32],
    chroma_offset: &mut [[i16; 2]; 32],
) -> (bool, bool) {
    // `luma_log2_denom` and `chroma_log2_denom` are at most 7.
    let luma_default = 1_i16 << weights.luma_log2_denom;
    let chroma_default = 1_i16 << weights.chroma_log2_denom;
    luma_weight.fill(luma_default);
    luma_offset.fill(0);
    chroma_weight.fill([chroma_default; 2]);
    chroma_offset.fill([0; 2]);
    let (mut any_luma, mut any_chroma) = (false, false);
    let entries = weights.lists[list].iter().take(MAX_LIST_LEN);
    for (index, entry) in entries.enumerate() {
        if let Some((weight, offset)) = entry.luma {
            luma_weight[index] = weight;
            luma_offset[index] = offset;
            any_luma = true;
        }
        if let Some([cb, cr]) = entry.chroma {
            chroma_weight[index] = [cb.0, cr.0];
            chroma_offset[index] = [cb.1, cr.1];
            any_chroma = true;
        }
    }
    (any_luma, any_chroma)
}

/// Copies the visible rectangle of a mapped NV12 image into a tightly packed
/// frame of `width` x `height`.
pub(super) fn copy_nv12_cropped(
    image: &[u8],
    planes: Nv12Planes,
    (coded_width, coded_height): (u32, u32),
    crop: Crop,
    (width, height): (u32, u32),
) -> Result<Vec<u8>, VaapiError> {
    let too_small = || VaapiError::BufferTooSmall("image");
    let (width, height) = (to_usize(width)?, to_usize(height)?);
    let (left, top) = (to_usize(crop.left)?, to_usize(crop.top)?);
    if width + left + to_usize(crop.right)? != to_usize(coded_width)?
        || height + top + to_usize(crop.bottom)? != to_usize(coded_height)?
    {
        return Err(too_small());
    }
    let mut frame = Vec::with_capacity(width * height * 3 / 2);
    for row in 0..height {
        let start = planes.y_offset + (top + row) * planes.y_pitch + left;
        frame.extend_from_slice(image.get(start..start + width).ok_or_else(too_small)?);
    }
    // A chroma row covers two luma rows, and a chroma sample two luma
    // columns in two bytes, so the byte offset of the column is `left`.
    for row in 0..height / 2 {
        let start = planes.uv_offset + (top / 2 + row) * planes.uv_pitch + left;
        frame.extend_from_slice(image.get(start..start + width).ok_or_else(too_small)?);
    }
    Ok(frame)
}
