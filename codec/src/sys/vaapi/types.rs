//! The libva types the H.264 encoder and decoder use.
//!
//! Layouts follow `va/va.h`, `va/va_enc_h264.h` and `va/drm/va_drm.h` of
//! libva 2.24.1 (<https://github.com/intel/libva/tree/2.24.1>), for the LP64
//! targets libva ships on. The unit tests pin every size, offset and bit-field
//! position to what a C compiler produces from those headers.

#![allow(
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case,
    clippy::struct_field_names,
    reason = "the names are libva's, kept so each field can be found in the header"
)]
use std::ffi::{c_int, c_uint, c_void};

/// `VADisplay`.
pub type VADisplay = *mut c_void;
/// `VAStatus`.
pub type VAStatus = c_int;
/// `VAGenericID`.
pub type VAGenericID = c_uint;
/// `VAConfigID`.
pub type VAConfigID = VAGenericID;
/// `VAContextID`.
pub type VAContextID = VAGenericID;
/// `VASurfaceID`.
pub type VASurfaceID = VAGenericID;
/// `VABufferID`.
pub type VABufferID = VAGenericID;
/// `VAImageID`.
pub type VAImageID = VAGenericID;

/// `VA_STATUS_SUCCESS`.
pub const VA_STATUS_SUCCESS: VAStatus = 0;
/// `VA_STATUS_ERROR_UNSUPPORTED_PROFILE`.
pub const VA_STATUS_ERROR_UNSUPPORTED_PROFILE: VAStatus = 0x0c;
/// `VA_INVALID_ID`.
pub const VA_INVALID_ID: VAGenericID = 0xffff_ffff;
/// `VA_INVALID_SURFACE`.
pub const VA_INVALID_SURFACE: VASurfaceID = VA_INVALID_ID;
/// `VA_PROGRESSIVE`.
pub const VA_PROGRESSIVE: c_int = 0x1;
/// `VA_ATTRIB_NOT_SUPPORTED`.
pub const VA_ATTRIB_NOT_SUPPORTED: u32 = 0x8000_0000;
/// `VA_RT_FORMAT_YUV420`.
pub const VA_RT_FORMAT_YUV420: u32 = 0x1;
/// `VA_FOURCC_NV12`.
pub const VA_FOURCC_NV12: u32 = 0x3231_564E;

/// `VAProfileH264Main`.
pub const VAProfileH264Main: c_int = 6;
/// `VAProfileH264High`.
pub const VAProfileH264High: c_int = 7;
/// `VAProfileH264ConstrainedBaseline`.
pub const VAProfileH264ConstrainedBaseline: c_int = 13;

/// `VAEntrypointVLD`.
pub const VAEntrypointVLD: c_int = 1;
/// `VAEntrypointEncSlice`.
pub const VAEntrypointEncSlice: c_int = 6;
/// `VAEntrypointEncSliceLP`.
pub const VAEntrypointEncSliceLP: c_int = 8;

/// `VAConfigAttribRTFormat`.
pub const VAConfigAttribRTFormat: u32 = 0;
/// `VAConfigAttribRateControl`.
pub const VAConfigAttribRateControl: u32 = 5;
/// `VAConfigAttribEncPackedHeaders`.
pub const VAConfigAttribEncPackedHeaders: u32 = 10;
/// `VAConfigAttribEncMaxRefFrames`.
pub const VAConfigAttribEncMaxRefFrames: u32 = 13;

/// `VA_RC_CBR`.
pub const VA_RC_CBR: u32 = 0x2;
/// `VA_RC_VBR`.
pub const VA_RC_VBR: u32 = 0x4;

/// `VA_ENC_PACKED_HEADER_SEQUENCE`.
pub const VA_ENC_PACKED_HEADER_SEQUENCE: u32 = 0x1;
/// `VA_ENC_PACKED_HEADER_PICTURE`.
pub const VA_ENC_PACKED_HEADER_PICTURE: u32 = 0x2;
/// `VA_ENC_PACKED_HEADER_SLICE`.
pub const VA_ENC_PACKED_HEADER_SLICE: u32 = 0x4;

/// `VAPictureParameterBufferType`.
pub const VAPictureParameterBufferType: u32 = 0;
/// `VAIQMatrixBufferType`.
pub const VAIQMatrixBufferType: u32 = 1;
/// `VASliceParameterBufferType`.
pub const VASliceParameterBufferType: u32 = 4;
/// `VASliceDataBufferType`.
pub const VASliceDataBufferType: u32 = 5;
/// `VA_SLICE_DATA_FLAG_ALL`: the buffer holds the whole slice.
pub const VA_SLICE_DATA_FLAG_ALL: u32 = 0;

/// `VAEncCodedBufferType`.
pub const VAEncCodedBufferType: u32 = 21;
/// `VAEncSequenceParameterBufferType`.
pub const VAEncSequenceParameterBufferType: u32 = 22;
/// `VAEncPictureParameterBufferType`.
pub const VAEncPictureParameterBufferType: u32 = 23;
/// `VAEncSliceParameterBufferType`.
pub const VAEncSliceParameterBufferType: u32 = 24;
/// `VAEncPackedHeaderParameterBufferType`.
pub const VAEncPackedHeaderParameterBufferType: u32 = 25;
/// `VAEncPackedHeaderDataBufferType`.
pub const VAEncPackedHeaderDataBufferType: u32 = 26;
/// `VAEncMiscParameterBufferType`.
pub const VAEncMiscParameterBufferType: u32 = 27;

/// `VAEncPackedHeaderSequence`.
pub const VAEncPackedHeaderSequence: u32 = 1;
/// `VAEncPackedHeaderPicture`.
pub const VAEncPackedHeaderPicture: u32 = 2;
/// `VAEncPackedHeaderSlice`.
pub const VAEncPackedHeaderSlice: u32 = 3;

/// `VAEncMiscParameterTypeFrameRate`.
pub const VAEncMiscParameterTypeFrameRate: u32 = 0;
/// `VAEncMiscParameterTypeRateControl`.
pub const VAEncMiscParameterTypeRateControl: u32 = 1;
/// `VAEncMiscParameterTypeHRD`.
pub const VAEncMiscParameterTypeHRD: u32 = 5;

/// `VASurfaceAttribPixelFormat`.
pub const VASurfaceAttribPixelFormat: u32 = 1;
/// `VA_SURFACE_ATTRIB_SETTABLE`.
pub const VA_SURFACE_ATTRIB_SETTABLE: u32 = 0x2;
/// `VAGenericValueTypeInteger`.
pub const VAGenericValueTypeInteger: u32 = 1;

/// `VA_PICTURE_H264_INVALID`.
pub const VA_PICTURE_H264_INVALID: u32 = 0x1;
/// `VA_PICTURE_H264_SHORT_TERM_REFERENCE`.
pub const VA_PICTURE_H264_SHORT_TERM_REFERENCE: u32 = 0x8;
/// `VA_PICTURE_H264_LONG_TERM_REFERENCE`.
pub const VA_PICTURE_H264_LONG_TERM_REFERENCE: u32 = 0x10;
/// `VA_PICTURE_H264_NON_EXISTING`.
pub const VA_PICTURE_H264_NON_EXISTING: u32 = 0x20;

/// `VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK`.
pub const VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK: u32 = 0x200;
/// `VA_CODED_BUF_STATUS_BAD_BITSTREAM`.
pub const VA_CODED_BUF_STATUS_BAD_BITSTREAM: u32 = 0x8000;

/// `VA_PADDING_LOW`.
const VA_PADDING_LOW: usize = 4;
/// `VA_PADDING_MEDIUM`.
const VA_PADDING_MEDIUM: usize = 8;

/// `VAConfigAttrib`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VAConfigAttrib {
    pub type_: u32,
    pub value: u32,
}

/// The union inside `VAGenericValue`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union VAGenericValueUnion {
    pub i: i32,
    pub f: f32,
    pub p: *mut c_void,
    pub func: Option<unsafe extern "C" fn()>,
}

/// `VAGenericValue`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VAGenericValue {
    pub type_: u32,
    pub value: VAGenericValueUnion,
}

/// `VASurfaceAttrib`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VASurfaceAttrib {
    pub type_: u32,
    pub flags: u32,
    pub value: VAGenericValue,
}

/// `VAImageFormat`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VAImageFormat {
    pub fourcc: u32,
    pub byte_order: u32,
    pub bits_per_pixel: u32,
    pub depth: u32,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub alpha_mask: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// `VAImage`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VAImage {
    pub image_id: VAImageID,
    pub format: VAImageFormat,
    pub buf: VABufferID,
    pub width: u16,
    pub height: u16,
    pub data_size: u32,
    pub num_planes: u32,
    pub pitches: [u32; 3],
    pub offsets: [u32; 3],
    pub num_palette_entries: i32,
    pub entry_bytes: i32,
    pub component_order: [i8; 4],
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// `VAEncPackedHeaderParameterBuffer`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VAEncPackedHeaderParameterBuffer {
    pub type_: u32,
    pub bit_length: u32,
    pub has_emulation_bytes: u8,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// `VAEncMiscParameterBuffer` without its flexible array member.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VAEncMiscParameterBuffer {
    pub type_: u32,
}

/// `VAEncMiscParameterRateControl`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VAEncMiscParameterRateControl {
    pub bits_per_second: u32,
    pub target_percentage: u32,
    pub window_size: u32,
    pub initial_qp: u32,
    pub min_qp: u32,
    pub basic_unit_size: u32,
    pub rc_flags: u32,
    pub ICQ_quality_factor: u32,
    pub max_qp: u32,
    pub quality_factor: u32,
    pub target_frame_size: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// `VAEncMiscParameterFrameRate`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VAEncMiscParameterFrameRate {
    pub framerate: u32,
    pub framerate_flags: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// `VAEncMiscParameterHRD`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VAEncMiscParameterHRD {
    pub initial_buffer_fullness: u32,
    pub buffer_size: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// A `VAEncMiscParameterBuffer` followed by its payload, the layout
/// `vaCreateBuffer` takes for a misc parameter of type `T`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MiscParameter<T> {
    pub header: VAEncMiscParameterBuffer,
    pub payload: T,
}

/// `VACodedBufferSegment`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VACodedBufferSegment {
    pub size: u32,
    pub bit_offset: u32,
    pub status: u32,
    pub reserved: u32,
    pub buf: *mut c_void,
    pub next: *mut c_void,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// `VAPictureH264`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VAPictureH264 {
    pub picture_id: VASurfaceID,
    pub frame_idx: u32,
    pub flags: u32,
    pub TopFieldOrderCnt: i32,
    pub BottomFieldOrderCnt: i32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

impl VAPictureH264 {
    /// An entry marked `VA_PICTURE_H264_INVALID`, as unused list slots are.
    pub const INVALID: Self = Self {
        picture_id: VA_INVALID_SURFACE,
        frame_idx: 0,
        flags: VA_PICTURE_H264_INVALID,
        TopFieldOrderCnt: 0,
        BottomFieldOrderCnt: 0,
        va_reserved: [0; VA_PADDING_LOW],
    };
}

/// `VAPictureParameterBufferH264`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VAPictureParameterBufferH264 {
    pub CurrPic: VAPictureH264,
    pub ReferenceFrames: [VAPictureH264; 16],
    pub picture_width_in_mbs_minus1: u16,
    pub picture_height_in_mbs_minus1: u16,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub num_ref_frames: u8,
    pub seq_fields: u32,
    pub num_slice_groups_minus1: u8,
    pub slice_group_map_type: u8,
    pub slice_group_change_rate_minus1: u16,
    pub pic_init_qp_minus26: i8,
    pub pic_init_qs_minus26: i8,
    pub chroma_qp_index_offset: i8,
    pub second_chroma_qp_index_offset: i8,
    pub pic_fields: u32,
    pub frame_num: u16,
    pub va_reserved: [u32; VA_PADDING_MEDIUM],
}

/// `VAIQMatrixBufferH264`: the scaling lists in raster order.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VAIQMatrixBufferH264 {
    pub ScalingList4x4: [[u8; 16]; 6],
    pub ScalingList8x8: [[u8; 64]; 2],
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// `VASliceParameterBufferH264`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VASliceParameterBufferH264 {
    pub slice_data_size: u32,
    pub slice_data_offset: u32,
    pub slice_data_flag: u32,
    pub slice_data_bit_offset: u16,
    pub first_mb_in_slice: u16,
    pub slice_type: u8,
    pub direct_spatial_mv_pred_flag: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub cabac_init_idc: u8,
    pub slice_qp_delta: i8,
    pub disable_deblocking_filter_idc: u8,
    pub slice_alpha_c0_offset_div2: i8,
    pub slice_beta_offset_div2: i8,
    pub RefPicList0: [VAPictureH264; 32],
    pub RefPicList1: [VAPictureH264; 32],
    pub luma_log2_weight_denom: u8,
    pub chroma_log2_weight_denom: u8,
    pub luma_weight_l0_flag: u8,
    pub luma_weight_l0: [i16; 32],
    pub luma_offset_l0: [i16; 32],
    pub chroma_weight_l0_flag: u8,
    pub chroma_weight_l0: [[i16; 2]; 32],
    pub chroma_offset_l0: [[i16; 2]; 32],
    pub luma_weight_l1_flag: u8,
    pub luma_weight_l1: [i16; 32],
    pub luma_offset_l1: [i16; 32],
    pub chroma_weight_l1_flag: u8,
    pub chroma_weight_l1: [[i16; 2]; 32],
    pub chroma_offset_l1: [[i16; 2]; 32],
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// The `seq_fields` bit-fields of `VAPictureParameterBufferH264` for a
/// progressive 4:2:0 stream.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the C bit-fields one to one"
)]
pub struct DecodeSequenceFields {
    pub gaps_in_frame_num_allowed: bool,
    pub direct_8x8_inference: bool,
    /// Whether the level is 3.1 or higher.
    pub min_luma_bi_pred_size_8x8: bool,
    pub log2_max_frame_num: u32,
    pub pic_order_cnt_type: u32,
    pub log2_max_poc_lsb: u32,
    pub delta_pic_order_always_zero: bool,
}

impl DecodeSequenceFields {
    /// The packed union value.
    pub const fn pack(self) -> u32 {
        const CHROMA_FORMAT_IDC_420: u32 = 1;
        const FRAME_MBS_ONLY: u32 = 1 << 4;
        CHROMA_FORMAT_IDC_420
            | FRAME_MBS_ONLY
            | ((self.gaps_in_frame_num_allowed as u32) << 3)
            | ((self.direct_8x8_inference as u32) << 6)
            | ((self.min_luma_bi_pred_size_8x8 as u32) << 7)
            | ((self.log2_max_frame_num - 4) << 8)
            | (self.pic_order_cnt_type << 12)
            | (self.log2_max_poc_lsb.saturating_sub(4) << 14)
            | ((self.delta_pic_order_always_zero as u32) << 18)
    }
}

/// The `pic_fields` bit-fields of `VAPictureParameterBufferH264` for a frame
/// picture.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the C bit-fields one to one"
)]
pub struct DecodePictureFields {
    pub entropy_coding_mode: bool,
    pub weighted_pred: bool,
    pub weighted_bipred_idc: u32,
    pub transform_8x8_mode: bool,
    pub constrained_intra_pred: bool,
    pub pic_order_present: bool,
    pub deblocking_filter_control_present: bool,
    pub redundant_pic_cnt_present: bool,
    pub reference_pic: bool,
}

impl DecodePictureFields {
    /// The packed union value.
    pub const fn pack(self) -> u32 {
        (self.entropy_coding_mode as u32)
            | ((self.weighted_pred as u32) << 1)
            | (self.weighted_bipred_idc << 2)
            | ((self.transform_8x8_mode as u32) << 4)
            | ((self.constrained_intra_pred as u32) << 6)
            | ((self.pic_order_present as u32) << 7)
            | ((self.deblocking_filter_control_present as u32) << 8)
            | ((self.redundant_pic_cnt_present as u32) << 9)
            | ((self.reference_pic as u32) << 10)
    }
}

/// `VAEncSequenceParameterBufferH264`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VAEncSequenceParameterBufferH264 {
    pub seq_parameter_set_id: u8,
    pub level_idc: u8,
    pub intra_period: u32,
    pub intra_idr_period: u32,
    pub ip_period: u32,
    pub bits_per_second: u32,
    pub max_num_ref_frames: u32,
    pub picture_width_in_mbs: u16,
    pub picture_height_in_mbs: u16,
    pub seq_fields: u32,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub num_ref_frames_in_pic_order_cnt_cycle: u8,
    pub offset_for_non_ref_pic: i32,
    pub offset_for_top_to_bottom_field: i32,
    pub offset_for_ref_frame: [i32; 256],
    pub frame_cropping_flag: u8,
    pub frame_crop_left_offset: u32,
    pub frame_crop_right_offset: u32,
    pub frame_crop_top_offset: u32,
    pub frame_crop_bottom_offset: u32,
    pub vui_parameters_present_flag: u8,
    pub vui_fields: u32,
    pub aspect_ratio_idc: u8,
    pub sar_width: u32,
    pub sar_height: u32,
    pub num_units_in_tick: u32,
    pub time_scale: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// `VAEncPictureParameterBufferH264`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VAEncPictureParameterBufferH264 {
    pub CurrPic: VAPictureH264,
    pub ReferenceFrames: [VAPictureH264; 16],
    pub coded_buf: VABufferID,
    pub pic_parameter_set_id: u8,
    pub seq_parameter_set_id: u8,
    pub last_picture: u8,
    pub frame_num: u16,
    pub pic_init_qp: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub chroma_qp_index_offset: i8,
    pub second_chroma_qp_index_offset: i8,
    pub pic_fields: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// `VAEncSliceParameterBufferH264`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VAEncSliceParameterBufferH264 {
    pub macroblock_address: u32,
    pub num_macroblocks: u32,
    pub macroblock_info: VABufferID,
    pub slice_type: u8,
    pub pic_parameter_set_id: u8,
    pub idr_pic_id: u16,
    pub pic_order_cnt_lsb: u16,
    pub delta_pic_order_cnt_bottom: i32,
    pub delta_pic_order_cnt: [i32; 2],
    pub direct_spatial_mv_pred_flag: u8,
    pub num_ref_idx_active_override_flag: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub RefPicList0: [VAPictureH264; 32],
    pub RefPicList1: [VAPictureH264; 32],
    pub luma_log2_weight_denom: u8,
    pub chroma_log2_weight_denom: u8,
    pub luma_weight_l0_flag: u8,
    pub luma_weight_l0: [i16; 32],
    pub luma_offset_l0: [i16; 32],
    pub chroma_weight_l0_flag: u8,
    pub chroma_weight_l0: [[i16; 2]; 32],
    pub chroma_offset_l0: [[i16; 2]; 32],
    pub luma_weight_l1_flag: u8,
    pub luma_weight_l1: [i16; 32],
    pub luma_offset_l1: [i16; 32],
    pub chroma_weight_l1_flag: u8,
    pub chroma_weight_l1: [[i16; 2]; 32],
    pub chroma_offset_l1: [[i16; 2]; 32],
    pub cabac_init_idc: u8,
    pub slice_qp_delta: i8,
    pub disable_deblocking_filter_idc: u8,
    pub slice_alpha_c0_offset_div2: i8,
    pub slice_beta_offset_div2: i8,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// Packs `seq_fields` of `VAEncSequenceParameterBufferH264` for the
/// encoder's fixed choices: 4:2:0, frame coding, `direct_8x8_inference`,
/// POC type 0 and the given `log2_max_*` values.
pub const fn sequence_fields(log2_max_frame_num: u32, log2_max_poc_lsb: u32) -> u32 {
    const CHROMA_FORMAT_IDC_420: u32 = 1;
    const FRAME_MBS_ONLY: u32 = 1 << 2;
    const DIRECT_8X8_INFERENCE: u32 = 1 << 5;
    CHROMA_FORMAT_IDC_420
        | FRAME_MBS_ONLY
        | DIRECT_8X8_INFERENCE
        | ((log2_max_frame_num - 4) << 6)
        | ((log2_max_poc_lsb - 4) << 12)
}

/// Packs `vui_fields` of `VAEncSequenceParameterBufferH264` for the SPS the
/// encoder writes: timing information, a bitstream restriction with MVs over
/// picture boundaries, and a fixed frame rate.
pub const fn vui_fields(log2_max_mv_length: u32) -> u32 {
    const TIMING_INFO_PRESENT: u32 = 1 << 1;
    const BITSTREAM_RESTRICTION: u32 = 1 << 2;
    const FIXED_FRAME_RATE: u32 = 1 << 13;
    const MOTION_VECTORS_OVER_PIC_BOUNDARIES: u32 = 1 << 15;
    TIMING_INFO_PRESENT
        | BITSTREAM_RESTRICTION
        | (log2_max_mv_length << 3)
        | (log2_max_mv_length << 8)
        | FIXED_FRAME_RATE
        | MOTION_VECTORS_OVER_PIC_BOUNDARIES
}

/// Packs `pic_fields` of `VAEncPictureParameterBufferH264` for a reference
/// picture with deblocking control in the slice header.
pub const fn picture_fields(idr: bool, cabac: bool, transform_8x8: bool) -> u32 {
    const REFERENCE_PIC: u32 = 1 << 1;
    const DEBLOCKING_FILTER_CONTROL_PRESENT: u32 = 1 << 9;
    (idr as u32)
        | REFERENCE_PIC
        | ((cabac as u32) << 3)
        | ((transform_8x8 as u32) << 8)
        | DEBLOCKING_FILTER_CONTROL_PRESENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    // Every constant below was printed by a C program compiled against the
    // libva 2.24.1 headers (clang, LP64).
    #[test]
    fn sizes_and_alignments_match_the_c_headers() {
        assert_eq!(
            (size_of::<VAConfigAttrib>(), align_of::<VAConfigAttrib>()),
            (8, 4)
        );
        assert_eq!(
            (size_of::<VAGenericValue>(), align_of::<VAGenericValue>()),
            (16, 8)
        );
        assert_eq!(
            (size_of::<VASurfaceAttrib>(), align_of::<VASurfaceAttrib>()),
            (24, 8)
        );
        assert_eq!(size_of::<VAImageFormat>(), 48);
        assert_eq!(size_of::<VAImage>(), 120);
        assert_eq!(size_of::<VAEncPackedHeaderParameterBuffer>(), 28);
        assert_eq!(size_of::<VAEncMiscParameterBuffer>(), 4);
        assert_eq!(size_of::<VAEncMiscParameterRateControl>(), 60);
        assert_eq!(size_of::<VAEncMiscParameterFrameRate>(), 24);
        assert_eq!(size_of::<VAEncMiscParameterHRD>(), 24);
        assert_eq!(
            (
                size_of::<VACodedBufferSegment>(),
                align_of::<VACodedBufferSegment>()
            ),
            (48, 8)
        );
        assert_eq!(size_of::<VAPictureH264>(), 36);
        assert_eq!(size_of::<VAEncSequenceParameterBufferH264>(), 1132);
        assert_eq!(size_of::<VAEncPictureParameterBufferH264>(), 648);
        assert_eq!(size_of::<VAEncSliceParameterBufferH264>(), 3140);
        assert_eq!(
            (
                size_of::<VAPictureParameterBufferH264>(),
                align_of::<VAPictureParameterBufferH264>()
            ),
            (672, 4)
        );
        assert_eq!(size_of::<VAIQMatrixBufferH264>(), 240);
        assert_eq!(
            (
                size_of::<VASliceParameterBufferH264>(),
                align_of::<VASliceParameterBufferH264>()
            ),
            (3128, 4)
        );
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one assertion per field")]
    fn decode_field_offsets_match_the_c_headers() {
        type Picture = VAPictureParameterBufferH264;
        type Slice = VASliceParameterBufferH264;
        assert_eq!(offset_of!(Picture, ReferenceFrames), 36);
        assert_eq!(offset_of!(Picture, picture_width_in_mbs_minus1), 612);
        assert_eq!(offset_of!(Picture, picture_height_in_mbs_minus1), 614);
        assert_eq!(offset_of!(Picture, bit_depth_luma_minus8), 616);
        assert_eq!(offset_of!(Picture, bit_depth_chroma_minus8), 617);
        assert_eq!(offset_of!(Picture, num_ref_frames), 618);
        assert_eq!(offset_of!(Picture, seq_fields), 620);
        assert_eq!(offset_of!(Picture, num_slice_groups_minus1), 624);
        assert_eq!(offset_of!(Picture, slice_group_map_type), 625);
        assert_eq!(offset_of!(Picture, slice_group_change_rate_minus1), 626);
        assert_eq!(offset_of!(Picture, pic_init_qp_minus26), 628);
        assert_eq!(offset_of!(Picture, pic_init_qs_minus26), 629);
        assert_eq!(offset_of!(Picture, chroma_qp_index_offset), 630);
        assert_eq!(offset_of!(Picture, second_chroma_qp_index_offset), 631);
        assert_eq!(offset_of!(Picture, pic_fields), 632);
        assert_eq!(offset_of!(Picture, frame_num), 636);
        assert_eq!(offset_of!(Picture, va_reserved), 640);

        assert_eq!(offset_of!(VAIQMatrixBufferH264, ScalingList8x8), 96);
        assert_eq!(offset_of!(VAIQMatrixBufferH264, va_reserved), 224);

        assert_eq!(offset_of!(Slice, slice_data_offset), 4);
        assert_eq!(offset_of!(Slice, slice_data_flag), 8);
        assert_eq!(offset_of!(Slice, slice_data_bit_offset), 12);
        assert_eq!(offset_of!(Slice, first_mb_in_slice), 14);
        assert_eq!(offset_of!(Slice, slice_type), 16);
        assert_eq!(offset_of!(Slice, direct_spatial_mv_pred_flag), 17);
        assert_eq!(offset_of!(Slice, num_ref_idx_l0_active_minus1), 18);
        assert_eq!(offset_of!(Slice, num_ref_idx_l1_active_minus1), 19);
        assert_eq!(offset_of!(Slice, cabac_init_idc), 20);
        assert_eq!(offset_of!(Slice, slice_qp_delta), 21);
        assert_eq!(offset_of!(Slice, disable_deblocking_filter_idc), 22);
        assert_eq!(offset_of!(Slice, slice_alpha_c0_offset_div2), 23);
        assert_eq!(offset_of!(Slice, slice_beta_offset_div2), 24);
        assert_eq!(offset_of!(Slice, RefPicList0), 28);
        assert_eq!(offset_of!(Slice, RefPicList1), 1180);
        assert_eq!(offset_of!(Slice, luma_log2_weight_denom), 2332);
        assert_eq!(offset_of!(Slice, chroma_log2_weight_denom), 2333);
        assert_eq!(offset_of!(Slice, luma_weight_l0_flag), 2334);
        assert_eq!(offset_of!(Slice, luma_weight_l0), 2336);
        assert_eq!(offset_of!(Slice, luma_offset_l0), 2400);
        assert_eq!(offset_of!(Slice, chroma_weight_l0_flag), 2464);
        assert_eq!(offset_of!(Slice, chroma_weight_l0), 2466);
        assert_eq!(offset_of!(Slice, chroma_offset_l0), 2594);
        assert_eq!(offset_of!(Slice, luma_weight_l1_flag), 2722);
        assert_eq!(offset_of!(Slice, luma_weight_l1), 2724);
        assert_eq!(offset_of!(Slice, luma_offset_l1), 2788);
        assert_eq!(offset_of!(Slice, chroma_weight_l1_flag), 2852);
        assert_eq!(offset_of!(Slice, chroma_weight_l1), 2854);
        assert_eq!(offset_of!(Slice, chroma_offset_l1), 2982);
        assert_eq!(offset_of!(Slice, va_reserved), 3112);
    }

    // A C program set every bit-field of the headers' unions to the values
    // below and printed 0x665d9 and 0x7fb; the latter also set
    // `field_pic_flag` (bit 5), which frame pictures leave clear.
    #[test]
    fn decode_bit_fields_match_the_c_compiler() {
        let sequence = DecodeSequenceFields {
            gaps_in_frame_num_allowed: true,
            direct_8x8_inference: true,
            min_luma_bi_pred_size_8x8: true,
            log2_max_frame_num: 9,
            pic_order_cnt_type: 2,
            log2_max_poc_lsb: 13,
            delta_pic_order_always_zero: true,
        };
        assert_eq!(sequence.pack(), 0x665d9);
        let picture = DecodePictureFields {
            entropy_coding_mode: true,
            weighted_pred: true,
            weighted_bipred_idc: 2,
            transform_8x8_mode: true,
            constrained_intra_pred: true,
            pic_order_present: true,
            deblocking_filter_control_present: true,
            redundant_pic_cnt_present: true,
            reference_pic: true,
        };
        assert_eq!(picture.pack(), 0x7fb & !(1 << 5));
    }

    #[test]
    fn misc_parameter_payloads_start_after_the_four_byte_header() {
        assert_eq!(
            offset_of!(MiscParameter<VAEncMiscParameterRateControl>, payload),
            4
        );
        assert_eq!(
            size_of::<MiscParameter<VAEncMiscParameterRateControl>>(),
            64
        );
        assert_eq!(size_of::<MiscParameter<VAEncMiscParameterFrameRate>>(), 28);
        assert_eq!(size_of::<MiscParameter<VAEncMiscParameterHRD>>(), 28);
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one assertion per field")]
    fn field_offsets_match_the_c_headers() {
        assert_eq!(offset_of!(VASurfaceAttrib, value), 8);
        assert_eq!(offset_of!(VAGenericValue, value), 8);
        assert_eq!(offset_of!(VAImage, format), 4);
        assert_eq!(offset_of!(VAImage, buf), 52);
        assert_eq!(offset_of!(VAImage, width), 56);
        assert_eq!(offset_of!(VAImage, data_size), 60);
        assert_eq!(offset_of!(VAImage, pitches), 68);
        assert_eq!(offset_of!(VAImage, offsets), 80);
        assert_eq!(offset_of!(VAImage, num_palette_entries), 92);
        assert_eq!(offset_of!(VACodedBufferSegment, buf), 16);
        assert_eq!(offset_of!(VACodedBufferSegment, next), 24);

        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, intra_period),
            4
        );
        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, max_num_ref_frames),
            20
        );
        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, picture_width_in_mbs),
            24
        );
        assert_eq!(offset_of!(VAEncSequenceParameterBufferH264, seq_fields), 28);
        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, bit_depth_luma_minus8),
            32
        );
        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, offset_for_non_ref_pic),
            36
        );
        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, offset_for_ref_frame),
            44
        );
        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, frame_cropping_flag),
            1068
        );
        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, frame_crop_left_offset),
            1072
        );
        assert_eq!(
            offset_of!(
                VAEncSequenceParameterBufferH264,
                vui_parameters_present_flag
            ),
            1088
        );
        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, vui_fields),
            1092
        );
        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, aspect_ratio_idc),
            1096
        );
        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, num_units_in_tick),
            1108
        );
        assert_eq!(
            offset_of!(VAEncSequenceParameterBufferH264, va_reserved),
            1116
        );

        assert_eq!(
            offset_of!(VAEncPictureParameterBufferH264, ReferenceFrames),
            36
        );
        assert_eq!(offset_of!(VAEncPictureParameterBufferH264, coded_buf), 612);
        assert_eq!(
            offset_of!(VAEncPictureParameterBufferH264, pic_parameter_set_id),
            616
        );
        assert_eq!(offset_of!(VAEncPictureParameterBufferH264, frame_num), 620);
        assert_eq!(
            offset_of!(VAEncPictureParameterBufferH264, pic_init_qp),
            622
        );
        assert_eq!(
            offset_of!(VAEncPictureParameterBufferH264, chroma_qp_index_offset),
            625
        );
        assert_eq!(offset_of!(VAEncPictureParameterBufferH264, pic_fields), 628);

        assert_eq!(
            offset_of!(VAEncSliceParameterBufferH264, macroblock_info),
            8
        );
        assert_eq!(offset_of!(VAEncSliceParameterBufferH264, slice_type), 12);
        assert_eq!(offset_of!(VAEncSliceParameterBufferH264, idr_pic_id), 14);
        assert_eq!(
            offset_of!(VAEncSliceParameterBufferH264, pic_order_cnt_lsb),
            16
        );
        assert_eq!(
            offset_of!(VAEncSliceParameterBufferH264, delta_pic_order_cnt_bottom),
            20
        );
        assert_eq!(
            offset_of!(VAEncSliceParameterBufferH264, direct_spatial_mv_pred_flag),
            32
        );
        assert_eq!(offset_of!(VAEncSliceParameterBufferH264, RefPicList0), 36);
        assert_eq!(offset_of!(VAEncSliceParameterBufferH264, RefPicList1), 1188);
        assert_eq!(
            offset_of!(VAEncSliceParameterBufferH264, luma_log2_weight_denom),
            2340
        );
        assert_eq!(
            offset_of!(VAEncSliceParameterBufferH264, luma_weight_l0),
            2344
        );
        assert_eq!(
            offset_of!(VAEncSliceParameterBufferH264, chroma_weight_l0_flag),
            2472
        );
        assert_eq!(
            offset_of!(VAEncSliceParameterBufferH264, cabac_init_idc),
            3118
        );
        assert_eq!(offset_of!(VAEncSliceParameterBufferH264, va_reserved), 3124);
    }

    // A C program set the same bit-fields through the headers' structs and
    // printed the unions: seq_fields 0x4125, vui_fields 0xaf7e and the
    // pic_fields below.
    #[test]
    fn bit_field_packing_matches_the_c_compiler() {
        assert_eq!(sequence_fields(8, 8), 0x4125);
        assert_eq!(vui_fields(15), 0xaf7e);
        for (idr, cabac, transform_8x8, value) in [
            (false, false, false, 0x202),
            (true, false, false, 0x203),
            (false, true, false, 0x20a),
            (true, true, false, 0x20b),
            (false, false, true, 0x302),
            (true, false, true, 0x303),
            (false, true, true, 0x30a),
            (true, true, true, 0x30b),
        ] {
            assert_eq!(picture_fields(idr, cabac, transform_8x8), value);
        }
    }
}
