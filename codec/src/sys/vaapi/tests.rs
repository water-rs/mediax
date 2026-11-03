//! The encoder against a test double of libva, and the opt-in hardware test.

#![allow(
    non_upper_case_globals,
    reason = "libva's constants are matched as patterns"
)]

use super::{VaapiError, api::Api, encoder::VaapiEncoder, types::*};
use crate::{
    Bitrate, BitstreamFormat, CodecError, EncoderCodec, EncoderConfig, EncoderProfile,
    EvenDimension, FrameOptions, FrameRate, H264Level, H264Profile, H264Settings,
};
use mediax_core::VideoColorInfo;
use std::{
    cell::Cell,
    collections::HashMap,
    ffi::{c_char, c_int, c_uint, c_void},
    mem::size_of,
    num::NonZeroU32,
    path::Path,
    ptr,
    time::Duration,
};

const WIDTH: u32 = 50;
const HEIGHT: u32 = 34;
const CODED_WIDTH: usize = 64;
const CODED_HEIGHT: usize = 48;
/// The mock image's pitch, wider than the picture to exercise the pitch.
const PITCH: usize = 80;
const PITCH_U32: u32 = 80;
const SURFACE_BASE: u32 = 100;

/// A test double of the libva driver: it records every call and buffer and
/// answers with a scripted capability set and a fake coded buffer.
#[derive(Default)]
struct Mock {
    log: Vec<String>,
    fail_on: Option<(&'static str, VAStatus)>,
    coded_status: u32,
    entrypoints: Vec<c_int>,
    rt_format: u32,
    rate_control: u32,
    packed_headers: u32,
    max_ref_frames: u32,
    next_id: u32,
    buffers: HashMap<u32, (u32, Vec<u8>)>,
    image_buffer: u32,
    image_data: Vec<u8>,
    uploads: Vec<Vec<u8>>,
    configs: Vec<Vec<(u32, u32)>>,
    surfaces: Vec<VASurfaceID>,
    begun: Vec<VASurfaceID>,
    frames: Vec<Vec<(u32, Vec<u8>)>>,
    segments: Vec<VACodedBufferSegment>,
    segment_data: Vec<Vec<u8>>,
}

thread_local! {
    /// The display `vaGetDisplayDRM` hands out; tests run on their own threads.
    static DISPLAY: Cell<*mut Mock> = const { Cell::new(ptr::null_mut()) };
}

/// Owns a [`Mock`] and frees it after the encoder under test is gone.
struct Handle(*mut Mock);

impl Handle {
    fn new(mock: Mock) -> Self {
        let pointer = Box::into_raw(Box::new(mock));
        DISPLAY.with(|display| display.set(pointer));
        Self(pointer)
    }

    fn mock(&self) -> &Mock {
        // SAFETY: the pointer is live until `drop`, and the encoder is not
        // inside a call while a test inspects the mock.
        unsafe { &*self.0 }
    }

    fn mock_mut(&mut self) -> &mut Mock {
        // SAFETY: as in `mock`; tests change the script between calls.
        unsafe { &mut *self.0 }
    }

    fn api() -> Api {
        Api {
            initialize,
            terminate,
            error_str,
            max_num_entrypoints,
            query_config_entrypoints,
            get_config_attributes,
            create_config,
            destroy_config,
            create_surfaces,
            destroy_surfaces,
            create_context,
            destroy_context,
            create_buffer,
            destroy_buffer,
            map_buffer,
            unmap_buffer,
            begin_picture,
            render_picture,
            end_picture,
            sync_surface,
            max_num_image_formats,
            query_image_formats,
            create_image,
            destroy_image,
            put_image,
            get_image,
            get_display_drm,
            libraries: None,
        }
    }

    #[allow(
        clippy::unused_self,
        reason = "opening only makes sense while the mock is the live display"
    )]
    fn open(&self, config: &EncoderConfig) -> Result<VaapiEncoder, VaapiError> {
        VaapiEncoder::with_api(Self::api(), Path::new("/dev/null"), config)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: allocated by `Box::into_raw` in `new` and freed once.
        drop(unsafe { Box::from_raw(self.0) });
    }
}

fn working_mock() -> Mock {
    Mock {
        entrypoints: vec![VAEntrypointEncSlice],
        rt_format: VA_RT_FORMAT_YUV420,
        rate_control: VA_RC_CBR | VA_RC_VBR,
        packed_headers: 0x1f,
        max_ref_frames: 1,
        ..Mock::default()
    }
}

/// Runs `body` on the mock behind `display`, logging `call` first and
/// failing it when the script says so.
fn with_mock(
    display: VADisplay,
    call: &'static str,
    body: impl FnOnce(&mut Mock) -> VAStatus,
) -> VAStatus {
    // SAFETY: tests pass the pointer `Handle::new` made as the display.
    let mock = unsafe { &mut *display.cast::<Mock>() };
    mock.log.push(call.to_owned());
    if let Some((failing, status)) = mock.fail_on
        && failing == call
    {
        return status;
    }
    body(mock)
}

fn fresh_id(mock: &mut Mock) -> u32 {
    mock.next_id += 1;
    mock.next_id
}

unsafe extern "C" fn get_display_drm(_fd: c_int) -> VADisplay {
    DISPLAY.with(|display| display.get().cast())
}

unsafe extern "C" fn initialize(
    display: VADisplay,
    major: *mut c_int,
    minor: *mut c_int,
) -> VAStatus {
    with_mock(display, "vaInitialize", |_| {
        // SAFETY: the encoder passes live locals.
        unsafe {
            *major = 1;
            *minor = 24;
        }
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn terminate(display: VADisplay) -> VAStatus {
    with_mock(display, "vaTerminate", |_| VA_STATUS_SUCCESS)
}

unsafe extern "C" fn error_str(status: VAStatus) -> *const c_char {
    match status {
        0x0c => c"the requested profile is not supported".as_ptr(),
        _ => c"scripted failure".as_ptr(),
    }
}

unsafe extern "C" fn max_num_entrypoints(_display: VADisplay) -> c_int {
    8
}

unsafe extern "C" fn query_config_entrypoints(
    display: VADisplay,
    _profile: c_int,
    entrypoints: *mut c_int,
    count: *mut c_int,
) -> VAStatus {
    with_mock(display, "vaQueryConfigEntrypoints", |mock| {
        if mock.entrypoints.is_empty() {
            return VA_STATUS_ERROR_UNSUPPORTED_PROFILE;
        }
        for (index, entrypoint) in mock.entrypoints.iter().enumerate() {
            // SAFETY: the buffer holds `vaMaxNumEntrypoints` (8) elements.
            unsafe { *entrypoints.add(index) = *entrypoint };
        }
        // SAFETY: a live local.
        unsafe { *count = c_int::try_from(mock.entrypoints.len()).unwrap() };
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn get_config_attributes(
    display: VADisplay,
    _profile: c_int,
    _entrypoint: c_int,
    attributes: *mut VAConfigAttrib,
    count: c_int,
) -> VAStatus {
    with_mock(display, "vaGetConfigAttributes", |mock| {
        for index in 0..usize::try_from(count).unwrap() {
            // SAFETY: the encoder passes `count` attributes.
            let attribute = unsafe { &mut *attributes.add(index) };
            attribute.value = match attribute.type_ {
                VAConfigAttribRTFormat => mock.rt_format,
                VAConfigAttribRateControl => mock.rate_control,
                VAConfigAttribEncPackedHeaders => mock.packed_headers,
                VAConfigAttribEncMaxRefFrames => mock.max_ref_frames,
                _ => VA_ATTRIB_NOT_SUPPORTED,
            };
        }
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn create_config(
    display: VADisplay,
    _profile: c_int,
    _entrypoint: c_int,
    attributes: *mut VAConfigAttrib,
    count: c_int,
    id: *mut VAConfigID,
) -> VAStatus {
    with_mock(display, "vaCreateConfig", |mock| {
        let attributes = (0..usize::try_from(count).unwrap())
            .map(|index| {
                // SAFETY: the encoder passes `count` attributes.
                let attribute = unsafe { *attributes.add(index) };
                (attribute.type_, attribute.value)
            })
            .collect();
        mock.configs.push(attributes);
        // SAFETY: a live local.
        unsafe { *id = fresh_id(mock) };
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn destroy_config(display: VADisplay, _id: VAConfigID) -> VAStatus {
    with_mock(display, "vaDestroyConfig", |_| VA_STATUS_SUCCESS)
}

unsafe extern "C" fn create_surfaces(
    display: VADisplay,
    format: c_uint,
    width: c_uint,
    height: c_uint,
    ids: *mut VASurfaceID,
    count: c_uint,
    attributes: *mut VASurfaceAttrib,
    attribute_count: c_uint,
) -> VAStatus {
    with_mock(display, "vaCreateSurfaces", |mock| {
        assert_eq!(format, VA_RT_FORMAT_YUV420);
        assert_eq!((width, height), (64, 48), "surfaces are macroblock aligned");
        assert_eq!(attribute_count, 1);
        // SAFETY: one attribute, as asserted.
        let attribute = unsafe { *attributes };
        assert_eq!(attribute.type_, VASurfaceAttribPixelFormat);
        // SAFETY: the encoder stored the integer member.
        assert_eq!(unsafe { attribute.value.value.i }, 0x3231_564E);
        for index in 0..count {
            let id = SURFACE_BASE + index;
            mock.surfaces.push(id);
            // SAFETY: the encoder passes `count` slots.
            unsafe { *ids.add(index as usize) = id };
        }
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn destroy_surfaces(
    display: VADisplay,
    _ids: *mut VASurfaceID,
    _count: c_int,
) -> VAStatus {
    with_mock(display, "vaDestroySurfaces", |_| VA_STATUS_SUCCESS)
}

unsafe extern "C" fn create_context(
    display: VADisplay,
    _config: VAConfigID,
    width: c_int,
    height: c_int,
    flag: c_int,
    _targets: *mut VASurfaceID,
    count: c_int,
    id: *mut VAContextID,
) -> VAStatus {
    with_mock(display, "vaCreateContext", |mock| {
        assert_eq!((width, height, flag, count), (64, 48, VA_PROGRESSIVE, 3));
        // SAFETY: a live local.
        unsafe { *id = fresh_id(mock) };
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn destroy_context(display: VADisplay, _id: VAContextID) -> VAStatus {
    with_mock(display, "vaDestroyContext", |_| VA_STATUS_SUCCESS)
}

unsafe extern "C" fn create_buffer(
    display: VADisplay,
    _context: VAContextID,
    kind: c_uint,
    size: c_uint,
    count: c_uint,
    data: *mut c_void,
    id: *mut VABufferID,
) -> VAStatus {
    with_mock(display, "vaCreateBuffer", |mock| {
        assert_eq!(count, 1);
        let bytes = if data.is_null() {
            vec![0; size as usize]
        } else {
            // SAFETY: libva reads `size` bytes from a non-null `data`.
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size as usize) }.to_vec()
        };
        let buffer = fresh_id(mock);
        mock.buffers.insert(buffer, (kind, bytes));
        // SAFETY: a live local.
        unsafe { *id = buffer };
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn destroy_buffer(display: VADisplay, id: VABufferID) -> VAStatus {
    with_mock(display, "vaDestroyBuffer", |mock| {
        mock.buffers.remove(&id);
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn map_buffer(
    display: VADisplay,
    id: VABufferID,
    mapped: *mut *mut c_void,
) -> VAStatus {
    with_mock(display, "vaMapBuffer", |mock| {
        let pointer = if id == mock.image_buffer {
            mock.image_data.as_mut_ptr().cast()
        } else {
            let (kind, _) = mock.buffers[&id];
            assert_eq!(
                kind, VAEncCodedBufferType,
                "only the image and coded buffers are mapped"
            );
            mock.segments.as_mut_ptr().cast()
        };
        // SAFETY: a live local.
        unsafe { *mapped = pointer };
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn unmap_buffer(display: VADisplay, _id: VABufferID) -> VAStatus {
    with_mock(display, "vaUnmapBuffer", |_| VA_STATUS_SUCCESS)
}

unsafe extern "C" fn begin_picture(
    display: VADisplay,
    _context: VAContextID,
    target: VASurfaceID,
) -> VAStatus {
    with_mock(display, "vaBeginPicture", |mock| {
        mock.begun.push(target);
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn render_picture(
    display: VADisplay,
    _context: VAContextID,
    ids: *mut VABufferID,
    count: c_int,
) -> VAStatus {
    with_mock(display, "vaRenderPicture", |mock| {
        let frame = (0..usize::try_from(count).unwrap())
            .map(|index| {
                // SAFETY: the encoder passes `count` ids.
                mock.buffers[&unsafe { *ids.add(index) }].clone()
            })
            .collect();
        mock.frames.push(frame);
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn end_picture(display: VADisplay, _context: VAContextID) -> VAStatus {
    with_mock(display, "vaEndPicture", |mock| {
        // A driver emits the packed headers it was given, then the slice.
        let frame = mock.frames.last().unwrap().clone();
        let mut access_unit = Vec::new();
        let mut idr = false;
        for (kind, bytes) in &frame {
            idr |= *kind == VAEncSequenceParameterBufferType;
            if *kind == VAEncPackedHeaderDataBufferType
                && bytes
                    .get(4)
                    .is_some_and(|nal| nal & 0x1f != 1 && nal & 0x1f != 5)
            {
                access_unit.extend_from_slice(bytes);
            }
        }
        access_unit.extend_from_slice(if idr {
            &[0, 0, 0, 1, 0x65, 0x88, 0x84]
        } else {
            &[0, 0, 0, 1, 0x41, 0x9A, 0x00]
        });
        // Two segments, to exercise the list walk.
        let split = access_unit.len() / 2;
        mock.segment_data = vec![access_unit[..split].to_vec(), access_unit[split..].to_vec()];
        mock.segments = mock
            .segment_data
            .iter()
            .map(|data| VACodedBufferSegment {
                size: u32::try_from(data.len()).unwrap(),
                bit_offset: 0,
                status: mock.coded_status,
                reserved: 0,
                buf: data.as_ptr().cast_mut().cast(),
                next: ptr::null_mut(),
                va_reserved: [0; 4],
            })
            .collect();
        let second = ptr::from_mut(&mut mock.segments[1]);
        mock.segments[0].next = second.cast();
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn sync_surface(display: VADisplay, _surface: VASurfaceID) -> VAStatus {
    with_mock(display, "vaSyncSurface", |_| VA_STATUS_SUCCESS)
}

unsafe extern "C" fn max_num_image_formats(_display: VADisplay) -> c_int {
    4
}

unsafe extern "C" fn query_image_formats(
    display: VADisplay,
    formats: *mut VAImageFormat,
    count: *mut c_int,
) -> VAStatus {
    with_mock(display, "vaQueryImageFormats", |_| {
        // SAFETY: room for `vaMaxNumImageFormats` (4) formats.
        unsafe {
            *formats = VAImageFormat {
                fourcc: 0x5659_5559,
                ..VAImageFormat::default()
            };
            *formats.add(1) = VAImageFormat {
                fourcc: VA_FOURCC_NV12,
                bits_per_pixel: 12,
                ..VAImageFormat::default()
            };
            *count = 2;
        }
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn create_image(
    display: VADisplay,
    format: *mut VAImageFormat,
    width: c_int,
    height: c_int,
    image: *mut VAImage,
) -> VAStatus {
    with_mock(display, "vaCreateImage", |mock| {
        assert_eq!((width, height), (64, 48));
        // SAFETY: a live local.
        assert_eq!(unsafe { (*format).fourcc }, VA_FOURCC_NV12);
        let size = PITCH * CODED_HEIGHT * 3 / 2;
        mock.image_data = vec![0xEE; size];
        let buffer = fresh_id(mock);
        mock.image_buffer = buffer;
        // SAFETY: a live local.
        unsafe {
            *image = VAImage {
                image_id: fresh_id(mock),
                format: *format,
                buf: buffer,
                width: 64,
                height: 48,
                data_size: u32::try_from(size).unwrap(),
                num_planes: 2,
                pitches: [PITCH_U32, PITCH_U32, 0],
                offsets: [0, PITCH_U32 * 48, 0],
                ..VAImage::default()
            };
        }
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn destroy_image(display: VADisplay, _id: VAImageID) -> VAStatus {
    with_mock(display, "vaDestroyImage", |_| VA_STATUS_SUCCESS)
}

unsafe extern "C" fn put_image(
    display: VADisplay,
    _surface: VASurfaceID,
    _image: VAImageID,
    src_x: c_int,
    src_y: c_int,
    src_width: c_uint,
    src_height: c_uint,
    dest_x: c_int,
    dest_y: c_int,
    dest_width: c_uint,
    dest_height: c_uint,
) -> VAStatus {
    with_mock(display, "vaPutImage", |mock| {
        assert_eq!((src_x, src_y, dest_x, dest_y), (0, 0, 0, 0));
        assert_eq!((src_width, src_height), (64, 48));
        assert_eq!((dest_width, dest_height), (64, 48));
        mock.uploads.push(mock.image_data.clone());
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn get_image(
    display: VADisplay,
    _surface: VASurfaceID,
    _x: c_int,
    _y: c_int,
    _width: c_uint,
    _height: c_uint,
    _image: VAImageID,
) -> VAStatus {
    with_mock(display, "vaGetImage", |_| {
        panic!("the encoder reads no surface back")
    })
}

fn config(bitstream: BitstreamFormat, keyframe_interval: Option<Duration>) -> EncoderConfig {
    EncoderConfig {
        codec: EncoderCodec::H264(H264Settings {
            profile: H264Profile::Main,
            level: H264Level::L3_1,
            bitstream,
        }),
        width: EvenDimension::new(WIDTH).unwrap(),
        height: EvenDimension::new(HEIGHT).unwrap(),
        color: VideoColorInfo::default(),
        profile: EncoderProfile::Realtime,
        bitrate: Bitrate::new(NonZeroU32::new(2_000_000).unwrap()),
        frame_rate: FrameRate::new(NonZeroU32::new(30).unwrap(), NonZeroU32::new(1).unwrap()),
        keyframe_interval,
    }
}

fn annex_b_config() -> EncoderConfig {
    config(BitstreamFormat::AnnexB, None)
}

fn nv12_frame(seed: u8) -> Vec<u8> {
    let luma = (WIDTH * HEIGHT) as usize;
    (0..luma * 3 / 2)
        .map(|index| u8::try_from(index % 251).unwrap().wrapping_add(seed))
        .collect()
}

fn read<T: Copy>(bytes: &[u8]) -> T {
    assert_eq!(bytes.len(), size_of::<T>());
    // SAFETY: the length matches and `T` is plain old data.
    unsafe { ptr::read_unaligned(bytes.as_ptr().cast()) }
}

fn kinds(frame: &[(u32, Vec<u8>)]) -> Vec<u32> {
    frame.iter().map(|(kind, _)| *kind).collect()
}

fn buffer_of(frame: &[(u32, Vec<u8>)], kind: u32) -> &[u8] {
    &frame.iter().find(|(found, _)| *found == kind).unwrap().1
}

fn misc_kinds(frame: &[(u32, Vec<u8>)]) -> Vec<u32> {
    frame
        .iter()
        .filter(|(kind, _)| *kind == VAEncMiscParameterBufferType)
        .map(|(_, bytes)| u32::from_ne_bytes(bytes[..4].try_into().unwrap()))
        .collect()
}

fn encode(encoder: &mut VaapiEncoder, seed: u8, keyframe: bool) -> crate::EncodedPacket {
    let mut packets = encoder
        .encode_nv12(
            &nv12_frame(seed),
            Duration::from_millis(u64::from(seed)),
            FrameOptions { keyframe },
        )
        .unwrap();
    assert_eq!(packets.len(), 1);
    let packet = packets.remove(0);
    assert_eq!(
        packet.presentation_time,
        Duration::from_millis(u64::from(seed))
    );
    assert_eq!(
        packet.decode_time_nanos,
        i64::from(seed) * 1_000_000,
        "low-delay I/P decodes each frame at its presentation time"
    );
    packet
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one test walks the whole call sequence"
)]
fn first_frame_is_an_idr_with_every_header_then_p_frames_follow() {
    let handle = Handle::new(working_mock());
    let mut encoder = handle.open(&annex_b_config()).unwrap();

    let idr = encode(&mut encoder, 0, false);
    assert!(idr.keyframe);
    let p = encode(&mut encoder, 1, false);
    assert!(!p.keyframe);
    let p2 = encode(&mut encoder, 2, false);
    assert!(!p2.keyframe);

    // The IDR carries the parameter sets the driver emitted; the P frames none.
    assert_eq!(&idr.data[..5], [0, 0, 0, 1, 0x67]);
    assert!(
        idr.data
            .windows(5)
            .any(|window| window == [0, 0, 0, 1, 0x68])
    );
    assert_eq!(p.data, [0, 0, 0, 1, 0x41, 0x9A, 0x00]);

    let mock = handle.mock();
    assert_eq!(
        kinds(&mock.frames[0]),
        [
            VAEncSequenceParameterBufferType,
            VAEncMiscParameterBufferType,
            VAEncMiscParameterBufferType,
            VAEncMiscParameterBufferType,
            VAEncPictureParameterBufferType,
            VAEncPackedHeaderParameterBufferType,
            VAEncPackedHeaderDataBufferType,
            VAEncPackedHeaderParameterBufferType,
            VAEncPackedHeaderDataBufferType,
            VAEncPackedHeaderParameterBufferType,
            VAEncPackedHeaderDataBufferType,
            VAEncSliceParameterBufferType,
        ]
    );
    assert_eq!(
        misc_kinds(&mock.frames[0]),
        [
            VAEncMiscParameterTypeRateControl,
            VAEncMiscParameterTypeFrameRate,
            VAEncMiscParameterTypeHRD
        ]
    );
    assert_eq!(
        kinds(&mock.frames[1]),
        [
            VAEncPictureParameterBufferType,
            VAEncPackedHeaderParameterBufferType,
            VAEncPackedHeaderDataBufferType,
            VAEncSliceParameterBufferType,
        ]
    );
    // The reconstruction surfaces alternate; the input surface is first.
    assert_eq!(mock.begun, [SURFACE_BASE; 3]);

    let picture: Vec<VAEncPictureParameterBufferH264> = mock
        .frames
        .iter()
        .map(|frame| read(buffer_of(frame, VAEncPictureParameterBufferType)))
        .collect();
    assert_eq!(picture[0].CurrPic.picture_id, SURFACE_BASE + 1);
    assert_eq!(picture[0].pic_fields & 1, 1, "IDR flag");
    assert!(
        picture[0]
            .ReferenceFrames
            .iter()
            .all(|entry| entry.flags == VA_PICTURE_H264_INVALID)
    );
    assert_eq!(picture[1].CurrPic.picture_id, SURFACE_BASE + 2);
    assert_eq!(picture[1].ReferenceFrames[0].picture_id, SURFACE_BASE + 1);
    assert_eq!(
        picture[1].ReferenceFrames[0].flags,
        VA_PICTURE_H264_SHORT_TERM_REFERENCE
    );
    assert_eq!(picture[1].ReferenceFrames[1].flags, VA_PICTURE_H264_INVALID);
    assert_eq!(
        (picture[1].frame_num, picture[1].CurrPic.TopFieldOrderCnt),
        (1, 2)
    );
    assert_eq!(picture[1].pic_fields & 1, 0);
    assert_eq!(picture[2].CurrPic.picture_id, SURFACE_BASE + 1);
    assert_eq!(picture[2].ReferenceFrames[0].picture_id, SURFACE_BASE + 2);
    assert_eq!(
        (picture[2].frame_num, picture[2].CurrPic.TopFieldOrderCnt),
        (2, 4)
    );
    assert_eq!(picture[0].coded_buf, picture[1].coded_buf);

    let slices: Vec<VAEncSliceParameterBufferH264> = mock
        .frames
        .iter()
        .map(|frame| read(buffer_of(frame, VAEncSliceParameterBufferType)))
        .collect();
    assert_eq!((slices[0].slice_type, slices[1].slice_type), (2, 0));
    assert_eq!(slices[0].num_macroblocks, 4 * 3);
    assert_eq!(slices[1].pic_order_cnt_lsb, 2);
    assert_eq!(slices[1].RefPicList0[0].picture_id, SURFACE_BASE + 1);
    assert_eq!(slices[0].RefPicList0[0].flags, VA_PICTURE_H264_INVALID);

    let sequence: VAEncSequenceParameterBufferH264 =
        read(buffer_of(&mock.frames[0], VAEncSequenceParameterBufferType));
    assert_eq!(
        (
            sequence.picture_width_in_mbs,
            sequence.picture_height_in_mbs
        ),
        (4, 3)
    );
    assert_eq!(sequence.level_idc, 31);
    assert_eq!(sequence.max_num_ref_frames, 1);
    assert_eq!(sequence.bits_per_second, 2_000_000);
    assert_eq!(
        (
            sequence.frame_crop_right_offset,
            sequence.frame_crop_bottom_offset
        ),
        (7, 7)
    );
    assert_eq!((sequence.num_units_in_tick, sequence.time_scale), (1, 60));

    // The configuration asked for NV12 4:2:0, CBR and all three headers.
    assert_eq!(
        mock.configs[0],
        [
            (VAConfigAttribRTFormat, VA_RT_FORMAT_YUV420),
            (VAConfigAttribRateControl, VA_RC_CBR),
            (VAConfigAttribEncPackedHeaders, 0x7),
        ]
    );
}

#[test]
fn frames_are_uploaded_padded_to_whole_macroblocks_through_the_image_pitch() {
    let handle = Handle::new(working_mock());
    let mut encoder = handle.open(&annex_b_config()).unwrap();
    let frame = nv12_frame(3);
    encoder
        .encode_nv12(&frame, Duration::ZERO, FrameOptions::default())
        .unwrap();

    let image = &handle.mock().uploads[0];
    let (width, height) = (WIDTH as usize, HEIGHT as usize);
    for row in 0..CODED_HEIGHT {
        let source = &frame[row.min(height - 1) * width..][..width];
        let target = &image[row * PITCH..][..CODED_WIDTH];
        assert_eq!(&target[..width], source, "luma row {row}");
        assert!(
            target[width..]
                .iter()
                .all(|&byte| byte == source[width - 1])
        );
        assert!(
            image[row * PITCH + CODED_WIDTH..(row + 1) * PITCH]
                .iter()
                .all(|&byte| byte == 0xEE)
        );
    }
    let chroma = &frame[width * height..];
    let chroma_offset = PITCH * CODED_HEIGHT;
    for row in 0..CODED_HEIGHT / 2 {
        let source = &chroma[row.min(height / 2 - 1) * width..][..width];
        let target = &image[chroma_offset + row * PITCH..][..CODED_WIDTH];
        assert_eq!(&target[..width], source, "chroma row {row}");
        for pair in target[width..].chunks(2) {
            assert_eq!(
                pair,
                &source[width - 2..],
                "chroma padding repeats the last pair"
            );
        }
    }
}

#[test]
fn a_keyframe_request_forces_an_idr_with_a_new_idr_pic_id() {
    let handle = Handle::new(working_mock());
    let mut encoder = handle.open(&annex_b_config()).unwrap();
    encode(&mut encoder, 0, false);
    encode(&mut encoder, 1, false);
    let idr = encode(&mut encoder, 2, true);
    let p = encode(&mut encoder, 3, false);
    assert!(idr.keyframe && !p.keyframe);

    let mock = handle.mock();
    let slice = |frame: usize| -> VAEncSliceParameterBufferH264 {
        read(buffer_of(
            &mock.frames[frame],
            VAEncSliceParameterBufferType,
        ))
    };
    assert_eq!((slice(2).slice_type, slice(2).idr_pic_id), (2, 1));
    assert_eq!(slice(3).slice_type, 0);
    let picture: VAEncPictureParameterBufferH264 =
        read(buffer_of(&mock.frames[2], VAEncPictureParameterBufferType));
    assert_eq!(
        (picture.frame_num, picture.CurrPic.TopFieldOrderCnt),
        (0, 0)
    );
    assert!(
        picture
            .ReferenceFrames
            .iter()
            .all(|entry| entry.flags == VA_PICTURE_H264_INVALID)
    );
    // The second IDR re-sends the sequence level buffers.
    assert_eq!(kinds(&mock.frames[2]).len(), 12);
}

#[test]
fn the_keyframe_interval_inserts_idrs() {
    let handle = Handle::new(working_mock());
    // 100 ms at 30 fps: an IDR every three frames.
    let mut encoder = handle
        .open(&config(
            BitstreamFormat::AnnexB,
            Some(Duration::from_millis(100)),
        ))
        .unwrap();
    let keyframes: Vec<bool> = (0..7)
        .map(|seed| encode(&mut encoder, seed, false).keyframe)
        .collect();
    assert_eq!(keyframes, [true, false, false, true, false, false, true]);
}

#[test]
fn a_bitrate_change_applies_to_the_next_frame_only() {
    let handle = Handle::new(working_mock());
    let mut encoder = handle.open(&annex_b_config()).unwrap();
    encode(&mut encoder, 0, false);
    encoder
        .set_bitrate(Bitrate::new(NonZeroU32::new(500_000).unwrap()))
        .unwrap();
    encode(&mut encoder, 1, false);
    encode(&mut encoder, 2, false);

    let mock = handle.mock();
    assert_eq!(
        misc_kinds(&mock.frames[1]),
        [VAEncMiscParameterTypeRateControl, VAEncMiscParameterTypeHRD]
    );
    assert_eq!(misc_kinds(&mock.frames[2]), Vec::<u32>::new());
    let (_, bytes) = mock.frames[1]
        .iter()
        .find(|(kind, _)| *kind == VAEncMiscParameterBufferType)
        .unwrap();
    let rate: MiscParameter<VAEncMiscParameterRateControl> = read(bytes);
    assert_eq!(rate.payload.bits_per_second, 500_000);
    assert_eq!(rate.payload.target_percentage, 100);
    assert_eq!(rate.payload.window_size, 500);
}

#[test]
fn length_prefixed_output_is_framed_with_an_avcc_record() {
    let handle = Handle::new(working_mock());
    let mut encoder = handle
        .open(&config(BitstreamFormat::LengthPrefixed, None))
        .unwrap();
    let idr = encode(&mut encoder, 0, false);
    let p = encode(&mut encoder, 1, false);
    assert!(idr.keyframe);
    assert_eq!(p.data, [0, 0, 0, 3, 0x41, 0x9A, 0x00]);
    // SPS, PPS and the IDR slice, each behind its length.
    let mut offset = 0;
    let mut types = Vec::new();
    while offset < idr.data.len() {
        let length = u32::from_be_bytes(idr.data[offset..offset + 4].try_into().unwrap()) as usize;
        types.push(idr.data[offset + 4] & 0x1f);
        offset += 4 + length;
    }
    assert_eq!((types, offset), (vec![7, 8, 5], idr.data.len()));

    let avcc = encoder.codec_config().unwrap();
    assert_eq!(avcc[0], 1);
    assert_eq!(avcc[1..4], [0x4D, 0x00, 31], "Main profile, level 3.1");
}

#[test]
fn a_failed_frame_restarts_with_an_idr() {
    let mut handle = Handle::new(working_mock());
    let mut encoder = handle.open(&annex_b_config()).unwrap();
    encode(&mut encoder, 0, false);
    handle.mock_mut().fail_on = Some(("vaRenderPicture", 0x1));
    let error = encoder
        .encode_nv12(&nv12_frame(1), Duration::ZERO, FrameOptions::default())
        .unwrap_err();
    assert!(
        matches!(&error, CodecError::EncodingFailed(message) if message.contains("vaRenderPicture")),
        "{error}"
    );
    // The picture that was begun is still ended.
    let calls: Vec<&str> = handle
        .mock()
        .log
        .iter()
        .map(String::as_str)
        .filter(|call| *call != "vaDestroyBuffer")
        .collect();
    assert!(calls.ends_with(&["vaBeginPicture", "vaRenderPicture", "vaEndPicture"]));
    handle.mock_mut().fail_on = None;
    assert!(encode(&mut encoder, 2, false).keyframe);
}

#[test]
fn wrong_sized_input_is_rejected_before_the_driver_is_touched() {
    let handle = Handle::new(working_mock());
    let mut encoder = handle.open(&annex_b_config()).unwrap();
    let calls = handle.mock().log.len();
    let error = encoder
        .encode_nv12(&[0; 10], Duration::ZERO, FrameOptions::default())
        .unwrap_err();
    assert!(matches!(error, CodecError::EncodingFailed(_)));
    assert_eq!(handle.mock().log.len(), calls);
    assert!(encode(&mut encoder, 0, false).keyframe);
    assert_eq!(handle.mock().frames.len(), 1);
}

#[test]
fn dropping_the_encoder_destroys_everything_before_terminating() {
    let handle = Handle::new(working_mock());
    let encoder = handle.open(&annex_b_config()).unwrap();
    let opened = handle.mock().log.len();
    drop(encoder);
    assert_eq!(
        handle.mock().log[opened..],
        [
            "vaDestroyImage",
            "vaDestroyBuffer",
            "vaDestroyContext",
            "vaDestroySurfaces",
            "vaDestroyConfig",
            "vaTerminate"
        ]
    );
}

#[test]
fn a_failure_while_opening_releases_what_was_created() {
    let handle = Handle::new(Mock {
        fail_on: Some(("vaCreateContext", 0x2)),
        ..working_mock()
    });
    let error = handle.open(&annex_b_config()).unwrap_err();
    assert!(matches!(
        error,
        VaapiError::Driver {
            call: "vaCreateContext",
            status: 2,
            ..
        }
    ));
    assert!(matches!(
        error.at_open(),
        CodecError::InitializationFailed(_)
    ));
    let log = &handle.mock().log;
    assert_eq!(
        log[log.len() - 3..],
        ["vaDestroySurfaces", "vaDestroyConfig", "vaTerminate"]
    );
}

#[test]
fn unsupported_drivers_are_typed_errors() {
    let open = |mock: Mock| Handle::new(mock).open(&annex_b_config()).unwrap_err();

    assert!(matches!(
        open(Mock {
            entrypoints: vec![],
            ..working_mock()
        }),
        VaapiError::NoEncodeEntrypoint {
            profile: H264Profile::Main
        }
    ));
    assert!(matches!(
        open(Mock {
            entrypoints: vec![1, 2],
            ..working_mock()
        }),
        VaapiError::NoEncodeEntrypoint { .. }
    ));
    assert!(matches!(
        open(Mock {
            rt_format: 0,
            ..working_mock()
        }),
        VaapiError::UnsupportedRtFormat
    ));
    assert!(matches!(
        open(Mock {
            rate_control: VA_RC_VBR,
            ..working_mock()
        }),
        VaapiError::UnsupportedRateControl(_)
    ));
    assert!(matches!(
        open(Mock {
            packed_headers: 0x3,
            ..working_mock()
        }),
        VaapiError::UnsupportedPackedHeaders { supported: 3 }
    ));
    assert!(matches!(
        open(Mock {
            packed_headers: VA_ATTRIB_NOT_SUPPORTED,
            ..working_mock()
        }),
        VaapiError::UnsupportedPackedHeaders { .. }
    ));
    assert!(matches!(
        open(Mock {
            max_ref_frames: 0,
            ..working_mock()
        }),
        VaapiError::NoReferenceFrames
    ));
    assert!(matches!(
        open(Mock {
            fail_on: Some(("vaInitialize", 0x4)),
            ..working_mock()
        }),
        VaapiError::Driver {
            call: "vaInitialize",
            ..
        }
    ));
    let error = open(Mock {
        entrypoints: vec![],
        ..working_mock()
    });
    assert!(matches!(error.at_open(), CodecError::Unsupported(_)));
}

#[test]
fn the_low_power_entry_point_is_used_when_it_is_the_only_one() {
    let handle = Handle::new(Mock {
        entrypoints: vec![VAEntrypointEncSliceLP],
        ..working_mock()
    });
    handle.open(&annex_b_config()).unwrap();
}

#[test]
fn unsupported_configurations_fail_before_any_device_is_opened() {
    let handle = Handle::new(working_mock());
    let mut too_large = annex_b_config();
    too_large.width = EvenDimension::new(4096).unwrap();
    too_large.height = EvenDimension::new(2160).unwrap();
    assert!(matches!(
        handle.open(&too_large).unwrap_err(),
        VaapiError::LevelExceeded {
            level: H264Level::L3_1,
            ..
        }
    ));
    let mut h265 = annex_b_config();
    h265.codec = EncoderCodec::H265(crate::H265Settings {
        bitstream: BitstreamFormat::AnnexB,
    });
    assert!(matches!(
        handle.open(&h265).unwrap_err(),
        VaapiError::UnsupportedCodec("H.265")
    ));
    let mut av1 = annex_b_config();
    av1.codec = EncoderCodec::Av1;
    assert!(matches!(
        handle.open(&av1).unwrap_err(),
        VaapiError::UnsupportedCodec("AV1")
    ));
    let mut odd_rate = annex_b_config();
    odd_rate.codec = EncoderCodec::H264(H264Settings {
        profile: H264Profile::Main,
        level: H264Level::L5_2,
        bitstream: BitstreamFormat::AnnexB,
    });
    odd_rate.frame_rate = FrameRate::new(
        NonZeroU32::new(70_001).unwrap(),
        NonZeroU32::new(1).unwrap(),
    );
    assert!(matches!(
        handle.open(&odd_rate).unwrap_err(),
        VaapiError::FrameRateOutOfRange(_)
    ));
    assert_eq!(handle.mock().log, Vec::<String>::new());
}

#[test]
fn a_corrupt_coded_buffer_is_reported_and_the_next_frame_is_an_idr() {
    let mut handle = Handle::new(working_mock());
    let mut encoder = handle.open(&annex_b_config()).unwrap();
    encode(&mut encoder, 0, false);
    handle.mock_mut().coded_status = VA_CODED_BUF_STATUS_BAD_BITSTREAM;
    let error = encoder
        .encode_nv12(&nv12_frame(1), Duration::ZERO, FrameOptions::default())
        .unwrap_err();
    assert!(
        matches!(&error, CodecError::EncodingFailed(message) if message.contains("corrupt")),
        "{error}"
    );
    handle.mock_mut().coded_status = VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK;
    assert!(
        encoder
            .encode_nv12(&nv12_frame(2), Duration::ZERO, FrameOptions::default())
            .is_err()
    );
    handle.mock_mut().coded_status = 0;
    assert!(encode(&mut encoder, 3, false).keyframe);
}

#[test]
fn a_failed_sync_is_reported() {
    let mut handle = Handle::new(working_mock());
    let mut encoder = handle.open(&annex_b_config()).unwrap();
    handle.mock_mut().fail_on = Some(("vaSyncSurface", 0x22));
    let error = encoder
        .encode_nv12(&nv12_frame(1), Duration::ZERO, FrameOptions::default())
        .unwrap_err();
    assert!(
        matches!(&error, CodecError::EncodingFailed(message) if message.contains("vaSyncSurface"))
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn a_host_without_libva_gets_a_clear_error_from_the_constructor() {
    let error = VaapiEncoder::new(&annex_b_config()).unwrap_err();
    assert!(
        matches!(
            &error,
            VaapiError::LibraryUnavailable {
                library: "libva.so.2",
                ..
            }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("libva.so.2 is not available"));
    assert!(matches!(error.at_open(), CodecError::Unsupported(_)));
}

/// Encodes on the first real VA-API render node; ignored where there is none.
#[test]
#[ignore = "needs a VA-API render node; run with --ignored"]
fn hardware_encode_produces_decodable_annex_b() {
    let mut encoder = VaapiEncoder::new(&config(BitstreamFormat::AnnexB, None))
        .expect("the render node must support H.264 encoding");
    let mut keyframes = 0;
    for index in 0..30_u8 {
        let packet = encoder
            .encode_nv12(
                &nv12_frame(index.wrapping_mul(5)),
                Duration::ZERO,
                FrameOptions::default(),
            )
            .unwrap()
            .remove(0);
        assert!(packet.data.starts_with(&[0, 0, 0, 1]) || packet.data.starts_with(&[0, 0, 1]));
        keyframes += usize::from(packet.keyframe);
        if packet.keyframe {
            assert_eq!(index, 0);
            assert_eq!(packet.data[4] & 0x1f, 7, "the IDR starts with the SPS");
        }
    }
    assert_eq!(keyframes, 1);
    let idr = encoder
        .encode_nv12(
            &nv12_frame(9),
            Duration::ZERO,
            FrameOptions { keyframe: true },
        )
        .unwrap()
        .remove(0);
    assert!(idr.keyframe);
    encoder
        .set_bitrate(Bitrate::new(NonZeroU32::new(1_000_000).unwrap()))
        .unwrap();
    encoder
        .encode_nv12(&nv12_frame(10), Duration::ZERO, FrameOptions::default())
        .unwrap();
}
