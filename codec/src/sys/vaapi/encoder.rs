//! The realtime H.264 encoder over a VA-API render node.

use super::{
    VaapiError,
    api::Api,
    device::{
        Buffer, Config, Context, Device, Image, PictureBuffers, Surfaces, render_nodes, to_usize,
    },
    headers::{
        FrameKind, LOG2_MAX_FRAME_NUM, LOG2_MAX_POC_LSB, MACROBLOCK, MAX_NUM_REF_FRAMES,
        PIC_INIT_QP, PackedHeader, SliceHeader, StreamParameters,
    },
    plan::{FramePlan, Nv12Planes, RateControl, Sequencer, copy_nv12_padded, misc_frame_rate},
    types::{
        MiscParameter, VA_ATTRIB_NOT_SUPPORTED, VA_CODED_BUF_STATUS_BAD_BITSTREAM,
        VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK, VA_ENC_PACKED_HEADER_PICTURE,
        VA_ENC_PACKED_HEADER_SEQUENCE, VA_ENC_PACKED_HEADER_SLICE, VA_INVALID_ID,
        VA_PICTURE_H264_SHORT_TERM_REFERENCE, VA_RT_FORMAT_YUV420, VACodedBufferSegment,
        VAConfigAttrib, VAConfigAttribEncMaxRefFrames, VAConfigAttribEncPackedHeaders,
        VAConfigAttribRTFormat, VAConfigAttribRateControl, VAEncCodedBufferType,
        VAEncMiscParameterBuffer, VAEncMiscParameterBufferType, VAEncMiscParameterTypeFrameRate,
        VAEncMiscParameterTypeHRD, VAEncMiscParameterTypeRateControl,
        VAEncPackedHeaderDataBufferType, VAEncPackedHeaderParameterBuffer,
        VAEncPackedHeaderParameterBufferType, VAEncPackedHeaderPicture, VAEncPackedHeaderSequence,
        VAEncPackedHeaderSlice, VAEncPictureParameterBufferH264, VAEncPictureParameterBufferType,
        VAEncSequenceParameterBufferH264, VAEncSequenceParameterBufferType,
        VAEncSliceParameterBufferH264, VAEncSliceParameterBufferType, VAEntrypointEncSlice,
        VAEntrypointEncSliceLP, VAPictureH264, VAProfileH264ConstrainedBaseline, VAProfileH264High,
        VAProfileH264Main, VASurfaceID, picture_fields, sequence_fields, vui_fields,
    },
};
use crate::{
    Bitrate, CodecError, EncodedPacket, EncoderCodec, EncoderConfig, EncoderProfile, FrameOptions,
    H264Profile, H264Settings, annexb::AnnexBPackets, bitstream::build_h264_avcc_from_annex_b,
    encoder_config::signed_nanos,
};
use std::{ffi::c_int, path::Path, ptr, rc::Rc, slice, time::Duration};

/// The input surface plus the two reconstruction surfaces that alternate as
/// reference and target.
const SURFACE_COUNT: u8 = 3;
/// The coded buffer holds at least this many bytes.
const MIN_CODED_BUFFER_BYTES: usize = 1 << 16;
/// Upper bound on the segments of one coded buffer list, which a driver
/// fills with a handful of NAL units.
const MAX_CODED_SEGMENTS: usize = 256;

/// What an encoder is built from, independent of the render node.
#[derive(Debug, Clone, Copy)]
struct Stream {
    settings: H264Settings,
    parameters: StreamParameters,
    rate: RateControl,
    encoder_profile: EncoderProfile,
    idr_period: Option<u64>,
    coded_width: u32,
    coded_height: u32,
}

impl Stream {
    fn from_config(config: &EncoderConfig) -> Result<Self, VaapiError> {
        let settings = match config.codec {
            EncoderCodec::H264(settings) => settings,
            EncoderCodec::H265(_) => return Err(VaapiError::UnsupportedCodec("H.265")),
            EncoderCodec::Av1 => return Err(VaapiError::UnsupportedCodec("AV1")),
        };
        let parameters = StreamParameters {
            profile: settings.profile,
            level: settings.level,
            width: config.width.get(),
            height: config.height.get(),
            color: config.color.cicp(),
            frame_rate: config.frame_rate,
        };
        parameters.check_level()?;
        // Fail on a frame rate libva cannot carry before any device opens.
        misc_frame_rate(config.frame_rate)?;
        parameters.vui_timing()?;
        Ok(Self {
            settings,
            parameters,
            rate: RateControl::new(config.profile, config.bitrate)?,
            encoder_profile: config.profile,
            idr_period: config
                .keyframe_interval
                .map(|interval| config.frame_rate.frames_in(interval)),
            coded_width: parameters.width_mbs() * MACROBLOCK,
            coded_height: parameters.height_mbs() * MACROBLOCK,
        })
    }

    const fn va_profile(&self) -> c_int {
        match self.settings.profile {
            H264Profile::ConstrainedBaseline => VAProfileH264ConstrainedBaseline,
            H264Profile::Main => VAProfileH264Main,
            H264Profile::High => VAProfileH264High,
        }
    }

    /// The bytes of a tightly packed NV12 frame.
    const fn frame_bytes(&self) -> usize {
        self.parameters.width as usize * self.parameters.height as usize * 3 / 2
    }
}

/// The picture the next P picture predicts from.
#[derive(Debug, Clone, Copy)]
struct Reference {
    surface: VASurfaceID,
    frame_num: u32,
    poc: u32,
}

/// A realtime H.264 encoder on a VA-API render node.
///
/// Frames are encoded synchronously: [`Self::encode_nv12`] returns the access
/// unit of the frame it was given.
pub struct VaapiEncoder {
    // Declaration order is teardown order; the device goes last.
    image: Image,
    coded_buffer: Buffer,
    context: Context,
    surfaces: Surfaces,
    _config: Config,
    device: Rc<Device>,
    stream: Stream,
    packets: AnnexBPackets,
    codec_config: Option<Vec<u8>>,
    sequencer: Sequencer,
    rate: RateControl,
    rate_changed: bool,
    reference: Option<Reference>,
    reconstruction_slot: usize,
}

#[allow(
    clippy::non_send_fields_in_send_ty,
    reason = "the Rc handles are all owned by the encoder, so they move together"
)]
// SAFETY: the `Rc`s are all held by this value, so moving it moves every
// handle to the display together, and libva allows a display to be used from
// any one thread at a time, which `&mut self` guarantees.
unsafe impl Send for VaapiEncoder {}

impl std::fmt::Debug for VaapiEncoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VaapiEncoder")
            .field("stream", &self.stream)
            .finish_non_exhaustive()
    }
}

impl VaapiEncoder {
    /// Creates an encoder on the first render node that can encode `config`.
    ///
    /// # Errors
    ///
    /// Returns a [`VaapiError`] when libva is missing, no render node can
    /// encode the stream or the configuration is unsupported.
    pub fn new(config: &EncoderConfig) -> Result<Self, VaapiError> {
        let stream = Stream::from_config(config)?;
        // Surface a missing libva once, before looking for devices.
        Api::load()?;
        let nodes = render_nodes()?;
        let mut attempts = Vec::with_capacity(nodes.len());
        for node in nodes {
            match Self::open(Api::load()?, &node, &stream) {
                Ok(encoder) => return Ok(encoder),
                Err(error) => attempts.push(format!("{}: {error}", node.display())),
            }
        }
        Err(VaapiError::NoUsableDevice {
            attempts: attempts.join("; "),
        })
    }

    /// Creates an encoder for `config` on the render node `path` through
    /// `api`, which tests replace with a double.
    ///
    /// # Errors
    ///
    /// See [`Self::new`].
    #[cfg(test)]
    pub fn with_api(api: Api, path: &Path, config: &EncoderConfig) -> Result<Self, VaapiError> {
        Self::open(api, path, &Stream::from_config(config)?)
    }

    fn open(api: Api, path: &Path, stream: &Stream) -> Result<Self, VaapiError> {
        let device = Rc::new(Device::open(api, path)?);
        let entrypoint = device.encode_entrypoint(stream.va_profile())?.ok_or(
            VaapiError::NoEncodeEntrypoint {
                profile: stream.settings.profile,
            },
        )?;
        device.check_capabilities(stream, entrypoint)?;
        let config = create_config(&device, stream, entrypoint)?;
        let surfaces = Surfaces::create(
            &device,
            stream.coded_width,
            stream.coded_height,
            usize::from(SURFACE_COUNT),
        )?;
        let context = Context::create(
            &device,
            &config,
            stream.coded_width,
            stream.coded_height,
            &surfaces,
        )?;
        let coded_buffer = create_coded_buffer(&device, &context, stream)?;
        let image = Image::create(&device, stream.coded_width, stream.coded_height)?;

        let parameter_sets = stream.parameters.annex_b_parameter_sets()?;
        let codec_config = build_h264_avcc_from_annex_b(&parameter_sets);
        Ok(Self {
            image,
            coded_buffer,
            context,
            surfaces,
            _config: config,
            device,
            stream: *stream,
            packets: AnnexBPackets::new(false, stream.settings.bitstream, parameter_sets),
            codec_config,
            sequencer: Sequencer::new(stream.idr_period),
            rate: stream.rate,
            rate_changed: false,
            reference: None,
            reconstruction_slot: 0,
        })
    }

    /// Returns the `avcC` record of the stream, which does not change.
    pub fn codec_config(&self) -> Option<Vec<u8>> {
        self.codec_config.clone()
    }

    /// Changes the target bitrate from the next frame on.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::Unsupported`] when the rate does not fit
    /// libva's rate control fields.
    pub fn set_bitrate(&mut self, bitrate: Bitrate) -> Result<(), CodecError> {
        self.rate = RateControl::new(self.stream.encoder_profile, bitrate)
            .map_err(VaapiError::at_encode)?;
        self.rate_changed = true;
        Ok(())
    }

    /// Encodes one tightly packed NV12 frame of the configured size.
    ///
    /// The packet reports `presentation_time` and a zero decode delay: the
    /// encoder is low-delay I/P, so frames are coded in presentation order and
    /// none is dropped. `options.keyframe` makes it an IDR picture. A failed call makes the
    /// next frame an IDR, as the reference is then unknown.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::EncodingFailed`] when the driver fails and
    /// [`CodecError::Unsupported`] when `data` has the wrong size.
    pub fn encode_nv12(
        &mut self,
        data: &[u8],
        presentation_time: Duration,
        options: FrameOptions,
    ) -> Result<Vec<EncodedPacket>, CodecError> {
        let expected = self.stream.frame_bytes();
        if data.len() != expected {
            return Err(CodecError::EncodingFailed(
                VaapiError::InputSize {
                    actual: data.len(),
                    expected,
                    width: self.stream.parameters.width,
                    height: self.stream.parameters.height,
                }
                .to_string(),
            ));
        }
        match self.encode_access_unit(data, options) {
            Ok(access_unit) => Ok(vec![self.packets.packet(
                access_unit,
                presentation_time,
                signed_nanos(presentation_time)?,
            )?]),
            Err(error) => {
                self.sequencer.restart();
                Err(error.at_encode())
            }
        }
    }

    fn encode_access_unit(
        &mut self,
        data: &[u8],
        options: FrameOptions,
    ) -> Result<Vec<u8>, VaapiError> {
        self.upload(data)?;
        let plan = self.sequencer.plan(options.keyframe);
        let reconstruction = self.surfaces.ids[1 + self.reconstruction_slot];
        self.submit(&plan, reconstruction)?;
        self.reference = Some(Reference {
            surface: reconstruction,
            frame_num: plan.frame_num,
            poc: plan.poc,
        });
        self.reconstruction_slot ^= 1;
        self.rate_changed = false;
        self.read_coded_buffer()
    }

    /// Writes `frame` into the input surface through the staging image.
    fn upload(&self, frame: &[u8]) -> Result<(), VaapiError> {
        let device = &*self.device;
        let image = &self.image.image;
        let layout = Nv12Planes {
            y_offset: to_usize(image.offsets[0])?,
            y_pitch: to_usize(image.pitches[0])?,
            uv_offset: to_usize(image.offsets[1])?,
            uv_pitch: to_usize(image.pitches[1])?,
        };
        if image.num_planes != 2 {
            return Err(VaapiError::BufferTooSmall("image planes"));
        }
        let parameters = &self.stream.parameters;
        device.with_mapped_bytes(image.buf, to_usize(image.data_size)?, "image", |bytes| {
            copy_nv12_padded(
                frame,
                (parameters.width as usize, parameters.height as usize),
                bytes,
                (
                    self.stream.coded_width as usize,
                    self.stream.coded_height as usize,
                ),
                layout,
            )
        })?;
        // SAFETY: the surface and image belong to this display; the previous
        // frame was synchronised, so the surface is idle.
        let status = unsafe {
            (device.api.put_image)(
                device.display,
                self.surfaces.ids[0],
                image.image_id,
                0,
                0,
                self.stream.coded_width,
                self.stream.coded_height,
                0,
                0,
                self.stream.coded_width,
                self.stream.coded_height,
            )
        };
        device.check("vaPutImage", status)
    }

    /// Records and submits the picture, then waits for the hardware.
    fn submit(&self, plan: &FramePlan, reconstruction: VASurfaceID) -> Result<(), VaapiError> {
        let reference = match plan.kind {
            FrameKind::Idr => None,
            FrameKind::P => Some(self.reference.ok_or(VaapiError::MissingReference)?),
        };
        let mut buffers = PictureBuffers::new(&self.device, self.context.id);
        let idr = plan.kind == FrameKind::Idr;
        let parameters = &self.stream.parameters;
        if idr {
            buffers.add(VAEncSequenceParameterBufferType, &self.sequence_parameter())?;
        }
        if idr || self.rate_changed {
            add_misc(
                &mut buffers,
                VAEncMiscParameterTypeRateControl,
                self.rate.misc_rate_control(),
            )?;
        }
        if idr {
            add_misc(
                &mut buffers,
                VAEncMiscParameterTypeFrameRate,
                misc_frame_rate(parameters.frame_rate)?,
            )?;
        }
        if idr || self.rate_changed {
            add_misc(
                &mut buffers,
                VAEncMiscParameterTypeHRD,
                self.rate.misc_hrd(),
            )?;
        }
        buffers.add(
            VAEncPictureParameterBufferType,
            &self.picture_parameter(plan, reconstruction, reference),
        )?;
        if idr {
            add_packed(
                &mut buffers,
                VAEncPackedHeaderSequence,
                &parameters.packed_sps()?,
            )?;
            add_packed(
                &mut buffers,
                VAEncPackedHeaderPicture,
                &parameters.packed_pps(),
            )?;
        }
        add_packed(
            &mut buffers,
            VAEncPackedHeaderSlice,
            &parameters.packed_slice_header(&SliceHeader {
                kind: plan.kind,
                frame_num: plan.frame_num,
                idr_pic_id: plan.idr_pic_id,
                poc_lsb: plan.poc_lsb(),
            }),
        )?;
        buffers.add(
            VAEncSliceParameterBufferType,
            &self.slice_parameter(plan, reference),
        )?;

        buffers.render(self.surfaces.ids[0])
    }

    fn sequence_parameter(&self) -> VAEncSequenceParameterBufferH264 {
        let parameters = &self.stream.parameters;
        let (crop_right, crop_bottom) = parameters.crop();
        let cropped = crop_right != 0 || crop_bottom != 0;
        // The frame rate was validated when the stream was configured.
        let (time_scale, num_units_in_tick) = parameters.vui_timing().unwrap_or_default();
        let idr_period = self
            .stream
            .idr_period
            .and_then(|period| u32::try_from(period).ok())
            .unwrap_or(0);
        VAEncSequenceParameterBufferH264 {
            seq_parameter_set_id: 0,
            level_idc: parameters.profile_level()[2],
            intra_period: idr_period,
            intra_idr_period: idr_period,
            ip_period: 1,
            bits_per_second: self.rate.bits_per_second,
            max_num_ref_frames: MAX_NUM_REF_FRAMES,
            picture_width_in_mbs: u16::try_from(parameters.width_mbs()).unwrap_or(u16::MAX),
            picture_height_in_mbs: u16::try_from(parameters.height_mbs()).unwrap_or(u16::MAX),
            seq_fields: sequence_fields(LOG2_MAX_FRAME_NUM, LOG2_MAX_POC_LSB),
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            num_ref_frames_in_pic_order_cnt_cycle: 0,
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            offset_for_ref_frame: [0; 256],
            frame_cropping_flag: u8::from(cropped),
            frame_crop_left_offset: 0,
            frame_crop_right_offset: crop_right / 2,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: crop_bottom / 2,
            vui_parameters_present_flag: 1,
            vui_fields: vui_fields(15),
            aspect_ratio_idc: 0,
            sar_width: 0,
            sar_height: 0,
            num_units_in_tick,
            time_scale,
            va_reserved: [0; 4],
        }
    }

    fn picture_parameter(
        &self,
        plan: &FramePlan,
        reconstruction: VASurfaceID,
        reference: Option<Reference>,
    ) -> VAEncPictureParameterBufferH264 {
        let mut reference_frames = [VAPictureH264::INVALID; 16];
        if let Some(reference) = reference {
            reference_frames[0] = reference_picture(reference);
        }
        let high = self.stream.settings.profile == H264Profile::High;
        VAEncPictureParameterBufferH264 {
            CurrPic: VAPictureH264 {
                picture_id: reconstruction,
                frame_idx: plan.frame_num,
                flags: 0,
                TopFieldOrderCnt: poc_value(plan.poc),
                BottomFieldOrderCnt: poc_value(plan.poc),
                va_reserved: [0; 4],
            },
            ReferenceFrames: reference_frames,
            coded_buf: self.coded_buffer.id,
            pic_parameter_set_id: 0,
            seq_parameter_set_id: 0,
            last_picture: 0,
            frame_num: u16::try_from(plan.frame_num).unwrap_or_default(),
            pic_init_qp: PIC_INIT_QP,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            chroma_qp_index_offset: 0,
            second_chroma_qp_index_offset: 0,
            pic_fields: picture_fields(
                plan.kind == FrameKind::Idr,
                self.stream.parameters.cabac(),
                high,
            ),
            va_reserved: [0; 4],
        }
    }

    fn slice_parameter(
        &self,
        plan: &FramePlan,
        reference: Option<Reference>,
    ) -> VAEncSliceParameterBufferH264 {
        let mut list0 = [VAPictureH264::INVALID; 32];
        if let Some(reference) = reference {
            list0[0] = reference_picture(reference);
        }
        let parameters = &self.stream.parameters;
        VAEncSliceParameterBufferH264 {
            macroblock_address: 0,
            num_macroblocks: parameters.width_mbs() * parameters.height_mbs(),
            macroblock_info: VA_INVALID_ID,
            slice_type: plan.kind.va_slice_type(),
            pic_parameter_set_id: 0,
            idr_pic_id: u16::try_from(plan.idr_pic_id).unwrap_or_default(),
            pic_order_cnt_lsb: u16::try_from(plan.poc_lsb()).unwrap_or_default(),
            delta_pic_order_cnt_bottom: 0,
            delta_pic_order_cnt: [0; 2],
            direct_spatial_mv_pred_flag: 0,
            num_ref_idx_active_override_flag: 0,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            RefPicList0: list0,
            RefPicList1: [VAPictureH264::INVALID; 32],
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
            cabac_init_idc: 0,
            slice_qp_delta: 0,
            disable_deblocking_filter_idc: 0,
            slice_alpha_c0_offset_div2: 0,
            slice_beta_offset_div2: 0,
            va_reserved: [0; 4],
        }
    }

    /// Copies the coded buffer's segments into one Annex B byte string.
    fn read_coded_buffer(&self) -> Result<Vec<u8>, VaapiError> {
        self.device.with_mapping(self.coded_buffer.id, |mapped| {
            collect_segments(mapped.cast::<VACodedBufferSegment>())
        })
    }
}

impl Device {
    /// Returns the slice encode entry point the driver offers for `profile`:
    /// the regular one when present, else the low-power one.
    fn encode_entrypoint(&self, profile: c_int) -> Result<Option<c_int>, VaapiError> {
        Ok(self.entrypoints(profile)?.and_then(|offered| {
            [VAEntrypointEncSlice, VAEntrypointEncSliceLP]
                .into_iter()
                .find(|entrypoint| offered.contains(entrypoint))
        }))
    }

    /// Checks the entry point takes the render target format, the rate
    /// control mode and the packed headers the encoder relies on.
    fn check_capabilities(&self, stream: &Stream, entrypoint: c_int) -> Result<(), VaapiError> {
        let mut attributes = [
            VAConfigAttrib {
                type_: VAConfigAttribRTFormat,
                value: 0,
            },
            VAConfigAttrib {
                type_: VAConfigAttribRateControl,
                value: 0,
            },
            VAConfigAttrib {
                type_: VAConfigAttribEncPackedHeaders,
                value: 0,
            },
            VAConfigAttrib {
                type_: VAConfigAttribEncMaxRefFrames,
                value: 0,
            },
        ];
        let count = c_int::try_from(attributes.len())
            .map_err(|_| VaapiError::SizeOutOfRange("attribute count"))?;
        // SAFETY: the pointer and count describe `attributes`.
        let status = unsafe {
            (self.api.get_config_attributes)(
                self.display,
                stream.va_profile(),
                entrypoint,
                attributes.as_mut_ptr(),
                count,
            )
        };
        self.check("vaGetConfigAttributes", status)?;
        let [
            render_target,
            rate_control,
            packed_headers,
            reference_frames,
        ] = attributes.map(|attribute| attribute.value);
        if render_target & VA_RT_FORMAT_YUV420 == 0 {
            return Err(VaapiError::UnsupportedRtFormat);
        }
        let mode = stream.rate.mode;
        if rate_control & mode.attribute_bit() == 0 {
            return Err(VaapiError::UnsupportedRateControl(mode));
        }
        let required = VA_ENC_PACKED_HEADER_SEQUENCE
            | VA_ENC_PACKED_HEADER_PICTURE
            | VA_ENC_PACKED_HEADER_SLICE;
        if packed_headers == VA_ATTRIB_NOT_SUPPORTED || packed_headers & required != required {
            return Err(VaapiError::UnsupportedPackedHeaders {
                supported: packed_headers,
            });
        }
        // The low 16 bits count the list 0 references.
        if reference_frames != VA_ATTRIB_NOT_SUPPORTED && reference_frames.trailing_zeros() >= 16 {
            return Err(VaapiError::NoReferenceFrames);
        }
        Ok(())
    }
}

fn create_config(
    device: &Rc<Device>,
    stream: &Stream,
    entrypoint: c_int,
) -> Result<Config, VaapiError> {
    let mut attributes = [
        VAConfigAttrib {
            type_: VAConfigAttribRTFormat,
            value: VA_RT_FORMAT_YUV420,
        },
        VAConfigAttrib {
            type_: VAConfigAttribRateControl,
            value: stream.rate.mode.attribute_bit(),
        },
        VAConfigAttrib {
            type_: VAConfigAttribEncPackedHeaders,
            value: VA_ENC_PACKED_HEADER_SEQUENCE
                | VA_ENC_PACKED_HEADER_PICTURE
                | VA_ENC_PACKED_HEADER_SLICE,
        },
    ];
    let count = c_int::try_from(attributes.len())
        .map_err(|_| VaapiError::SizeOutOfRange("attribute count"))?;
    let mut id = VA_INVALID_ID;
    // SAFETY: the pointers and the count describe live locals.
    let status = unsafe {
        (device.api.create_config)(
            device.display,
            stream.va_profile(),
            entrypoint,
            attributes.as_mut_ptr(),
            count,
            &raw mut id,
        )
    };
    device.check("vaCreateConfig", status)?;
    Ok(Config::new(Rc::clone(device), id))
}

fn create_coded_buffer(
    device: &Rc<Device>,
    context: &Context,
    stream: &Stream,
) -> Result<Buffer, VaapiError> {
    // A coded picture is not larger than the raw one at any sane rate.
    let bytes = (stream.coded_width as usize * stream.coded_height as usize * 3 / 2)
        .max(MIN_CODED_BUFFER_BYTES);
    let size = u32::try_from(bytes).map_err(|_| VaapiError::SizeOutOfRange("coded buffer"))?;
    let mut id = VA_INVALID_ID;
    // SAFETY: the driver allocates the buffer; `id` is a live local.
    let status = unsafe {
        (device.api.create_buffer)(
            device.display,
            context.id,
            VAEncCodedBufferType,
            size,
            1,
            ptr::null_mut(),
            &raw mut id,
        )
    };
    device.check("vaCreateBuffer", status)?;
    Ok(Buffer::new(Rc::clone(device), id))
}

/// Adds a misc parameter buffer of `payload`.
fn add_misc<T>(buffers: &mut PictureBuffers, kind: u32, payload: T) -> Result<(), VaapiError> {
    buffers.add(
        VAEncMiscParameterBufferType,
        &MiscParameter {
            header: VAEncMiscParameterBuffer { type_: kind },
            payload,
        },
    )
}

/// Adds a packed header's parameter and data buffers.
fn add_packed(
    buffers: &mut PictureBuffers,
    kind: u32,
    header: &PackedHeader,
) -> Result<(), VaapiError> {
    buffers.add(
        VAEncPackedHeaderParameterBufferType,
        &VAEncPackedHeaderParameterBuffer {
            type_: kind,
            bit_length: header.bit_length,
            // The headers carry their emulation prevention bytes.
            has_emulation_bytes: 1,
            va_reserved: [0; 4],
        },
    )?;
    buffers.add_bytes(VAEncPackedHeaderDataBufferType, &header.data)
}

/// Concatenates the segments of a mapped coded buffer.
fn collect_segments(first: *const VACodedBufferSegment) -> Result<Vec<u8>, VaapiError> {
    let mut access_unit = Vec::new();
    let mut segment = first;
    for _ in 0..MAX_CODED_SEGMENTS {
        if segment.is_null() {
            return if access_unit.is_empty() {
                Err(VaapiError::CodedBuffer("it holds no data"))
            } else {
                Ok(access_unit)
            };
        }
        // SAFETY: a mapped coded buffer is a list of segments, each valid
        // until the buffer is unmapped; the list ends in a null `next`.
        let current = unsafe { &*segment };
        if current.status
            & (VA_CODED_BUF_STATUS_BAD_BITSTREAM | VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK)
            != 0
        {
            return Err(VaapiError::CodedBuffer(
                "the driver flagged a segment as corrupt or overflowing",
            ));
        }
        if current.size != 0 {
            if current.buf.is_null() {
                return Err(VaapiError::CodedBuffer("a segment has a size but no data"));
            }
            // SAFETY: the segment's `buf` holds `size` bytes while mapped.
            let bytes = unsafe {
                slice::from_raw_parts(
                    current.buf.cast::<u8>().cast_const(),
                    to_usize(current.size)?,
                )
            };
            access_unit.extend_from_slice(bytes);
        }
        segment = current.next.cast::<VACodedBufferSegment>().cast_const();
    }
    Err(VaapiError::CodedBuffer("its segment list does not end"))
}

fn reference_picture(reference: Reference) -> VAPictureH264 {
    VAPictureH264 {
        picture_id: reference.surface,
        frame_idx: reference.frame_num,
        flags: VA_PICTURE_H264_SHORT_TERM_REFERENCE,
        TopFieldOrderCnt: poc_value(reference.poc),
        BottomFieldOrderCnt: poc_value(reference.poc),
        va_reserved: [0; 4],
    }
}

/// A POC is below 2^16, so it always fits.
fn poc_value(poc: u32) -> i32 {
    i32::try_from(poc).unwrap_or(i32::MAX)
}
