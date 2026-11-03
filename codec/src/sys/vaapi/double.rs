//! A test double of the libva decode path, for the decoder tests and the fuzz
//! targets.
//!
//! It validates what the decoder sends the way a driver would (ids, sizes,
//! call order), records every submitted picture, and "decodes" by giving the
//! target surface a fill value that identifies the picture: the n-th picture
//! submitted is `fill_of(n)`. Reading a surface back yields a luma plane
//! of [`luma`] and a chroma plane of [`chroma`] values, so tests can tell
//! which decode produced a frame and where in it a sample came from.

#![allow(
    non_upper_case_globals,
    reason = "libva's constants are matched as patterns"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "sample values wrap on purpose, and libva sizes are positive"
)]

use super::{
    api::Api,
    types::{
        VA_FOURCC_NV12, VA_FOURCC_P010, VA_HEVC_REF_INVALID, VA_PICTURE_HEVC_INVALID,
        VA_PICTURE_HEVC_LONG_TERM_REFERENCE, VA_PICTURE_HEVC_RPS_LT_CURR,
        VA_PICTURE_HEVC_RPS_ST_CURR_AFTER, VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE, VA_RT_FORMAT_YUV420,
        VA_RT_FORMAT_YUV420_10, VA_SLICE_DATA_FLAG_ALL, VA_STATUS_ERROR_UNSUPPORTED_PROFILE,
        VA_STATUS_SUCCESS, VABufferID, VAConfigAttrib, VAConfigAttribRTFormat, VAConfigID,
        VAContextID, VADisplay, VAEntrypointVLD, VAIQMatrixBufferH264, VAIQMatrixBufferHEVC,
        VAIQMatrixBufferType, VAImage, VAImageFormat, VAImageID, VAPictureParameterBufferH264,
        VAPictureParameterBufferHEVC, VAPictureParameterBufferType,
        VAProfileH264ConstrainedBaseline, VAProfileH264High, VAProfileH264Main, VAProfileHEVCMain,
        VAProfileHEVCMain10, VASliceDataBufferType, VASliceParameterBufferH264,
        VASliceParameterBufferHEVC, VASliceParameterBufferType, VAStatus, VASurfaceAttrib,
        VASurfaceID,
    },
};
use std::{
    cell::Cell,
    collections::HashMap,
    ffi::{c_char, c_int, c_uint, c_void},
    mem::size_of,
    ptr,
};

/// The bytes a row of the double's images is wider than the picture.
const PITCH_PADDING: usize = 16;
/// `VA_STATUS_ERROR_OPERATION_FAILED`.
const STATUS_FAILED: VAStatus = 0x01;
/// `VA_STATUS_ERROR_RESOLUTION_NOT_SUPPORTED`.
const STATUS_RESOLUTION_NOT_SUPPORTED: VAStatus = 0x13;
/// The double decodes up to 512x512 samples, which keeps fuzzing quick.
const MAX_SURFACE_SAMPLES: u64 = 512 * 512;
const SURFACE_BASE: u32 = 1000;

/// The fill value of the `n`-th submitted picture, counting from 1.
#[must_use]
pub const fn fill_of(n: usize) -> u8 {
    // Wraps for very long streams, which only the fuzz targets reach.
    (n % 251) as u8 + 1
}

/// The luma sample at (`x`, `y`) of a surface with `fill`.
#[must_use]
pub const fn luma(fill: u8, x: usize, y: usize) -> u8 {
    fill.wrapping_add((x + 2 * y) as u8)
}

/// The chroma byte at (`x`, `y`) of the interleaved plane of a surface with
/// `fill`.
#[must_use]
pub const fn chroma(fill: u8, x: usize, y: usize) -> u8 {
    fill.wrapping_add(100).wrapping_add((x + 3 * y) as u8)
}

/// A picture submitted between `vaBeginPicture` and `vaEndPicture`.
#[derive(Clone)]
pub struct Picture {
    /// The surface decoded into.
    pub target: VASurfaceID,
    /// The picture parameters.
    pub parameters: VAPictureParameterBufferH264,
    /// The scaling lists.
    #[cfg(test)]
    pub scaling: VAIQMatrixBufferH264,
    /// Each slice's parameters and data.
    pub slices: Vec<(VASliceParameterBufferH264, Vec<u8>)>,
    /// The fill value the surface got.
    pub fill: u8,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Picture")
            .field("target", &self.target)
            .field("slices", &self.slices.len())
            .field("fill", &self.fill)
            .finish_non_exhaustive()
    }
}

/// A picture submitted by the H.265 decoder.
#[derive(Clone)]
pub struct HevcPicture {
    /// The surface decoded into.
    pub target: VASurfaceID,
    /// The picture parameters.
    pub parameters: VAPictureParameterBufferHEVC,
    /// The scaling lists, when the picture sent them.
    #[cfg(test)]
    pub scaling: Option<VAIQMatrixBufferHEVC>,
    /// Each slice segment's parameters and data.
    pub slices: Vec<(VASliceParameterBufferHEVC, Vec<u8>)>,
    /// The fill value the surface got.
    pub fill: u8,
}

impl std::fmt::Debug for HevcPicture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HevcPicture")
            .field("target", &self.target)
            .field("slices", &self.slices.len())
            .field("fill", &self.fill)
            .finish_non_exhaustive()
    }
}

/// The codec of the config last created.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Codec {
    #[default]
    H264,
    Hevc,
}

#[derive(Default)]
struct State {
    log: Vec<&'static str>,
    fail_on: Option<(&'static str, VAStatus)>,
    profiles: Vec<c_int>,
    config_profiles: Vec<c_int>,
    codec: Codec,
    /// The render target format the config last created asked for.
    config_rt_format: u32,
    rt_format: u32,
    /// The bytes of a sample of the surfaces last created.
    sample_bytes: usize,
    hevc_pictures: Vec<HevcPicture>,
    submitted_hevc: Option<HevcPicture>,
    next_id: u32,
    live_surfaces: HashMap<VASurfaceID, u8>,
    surface_size: (u32, u32),
    surface_count: usize,
    context_size: (c_int, c_int),
    buffers: HashMap<VABufferID, (u32, Vec<u8>)>,
    image_buffer: VABufferID,
    image_data: Vec<u8>,
    image_size: (c_int, c_int),
    begun: Option<VASurfaceID>,
    submitted: Option<Picture>,
    pictures: Vec<Picture>,
}

thread_local! {
    /// The display `vaGetDisplayDRM` hands out; tests run on their own
    /// threads.
    static DISPLAY: Cell<*mut State> = const { Cell::new(ptr::null_mut()) };
}

/// The driver double: owns the state the [`Api`] it hands out works on.
pub struct Driver(*mut State);

impl std::fmt::Debug for Driver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Driver").finish_non_exhaustive()
    }
}

impl Default for Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver {
    /// A driver that decodes H.264 Constrained Baseline, Main and High and
    /// H.265 Main and Main 10.
    #[must_use]
    pub fn new() -> Self {
        let pointer = Box::into_raw(Box::new(State {
            profiles: vec![
                VAProfileH264ConstrainedBaseline,
                VAProfileH264Main,
                VAProfileH264High,
                VAProfileHEVCMain,
                VAProfileHEVCMain10,
            ],
            rt_format: VA_RT_FORMAT_YUV420 | VA_RT_FORMAT_YUV420_10,
            sample_bytes: 1,
            ..State::default()
        }));
        DISPLAY.with(|display| display.set(pointer));
        Self(pointer)
    }

    /// The libva entry points, backed by this double.
    #[must_use]
    pub const fn api() -> Api {
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
}

/// What the decoder tests inspect and script; the fuzz targets only run the
/// double.
#[cfg(test)]
impl Driver {
    fn state(&self) -> &State {
        // SAFETY: the pointer is live until `drop`, and the decoder is not
        // inside a call while a test inspects the state.
        unsafe { &*self.0 }
    }

    fn state_mut(&mut self) -> &mut State {
        // SAFETY: as in `state`; tests change the script between calls.
        unsafe { &mut *self.0 }
    }

    /// Every libva call made so far, in order.
    #[must_use]
    pub fn log(&self) -> &[&'static str] {
        &self.state().log
    }

    /// The pictures submitted so far.
    #[must_use]
    pub fn pictures(&self) -> &[Picture] {
        &self.state().pictures
    }

    /// The H.265 pictures submitted so far.
    #[must_use]
    pub fn hevc_pictures(&self) -> &[HevcPicture] {
        &self.state().hevc_pictures
    }

    /// The surfaces created and not yet destroyed, with their fill values.
    #[must_use]
    pub fn surfaces(&self) -> usize {
        self.state().live_surfaces.len()
    }

    /// The size of the surfaces last created.
    #[must_use]
    pub fn surface_size(&self) -> (u32, u32) {
        self.state().surface_size
    }

    /// Makes `call` return `status` from now on.
    pub fn fail(&mut self, call: &'static str, status: VAStatus) {
        self.state_mut().fail_on = Some((call, status));
    }

    /// Lets every call succeed again.
    pub fn clear_failure(&mut self) {
        self.state_mut().fail_on = None;
    }

    /// The profile of each config created so far.
    #[must_use]
    pub fn config_profiles(&self) -> &[c_int] {
        &self.state().config_profiles
    }

    /// Restricts the profiles the driver decodes.
    pub fn set_profiles(&mut self, profiles: &[c_int]) {
        self.state_mut().profiles = profiles.to_vec();
    }

    /// Sets the render target formats the driver reports.
    pub fn set_rt_format(&mut self, rt_format: u32) {
        self.state_mut().rt_format = rt_format;
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // SAFETY: allocated by `Box::into_raw` in `new` and freed once.
        drop(unsafe { Box::from_raw(self.0) });
        DISPLAY.with(|display| display.set(ptr::null_mut()));
    }
}

/// Runs `body` on the state behind `display`, logging `call` first and
/// failing it when the script says so.
fn with_state(
    display: VADisplay,
    call: &'static str,
    body: impl FnOnce(&mut State) -> VAStatus,
) -> VAStatus {
    // SAFETY: the decoder passes the pointer `Driver::new` made as display.
    let state = unsafe { &mut *display.cast::<State>() };
    state.log.push(call);
    if let Some((failing, status)) = state.fail_on
        && failing == call
    {
        return status;
    }
    body(state)
}

const fn fresh_id(state: &mut State) -> u32 {
    state.next_id += 1;
    state.next_id
}

fn read<T: Copy>(bytes: &[u8]) -> T {
    assert_eq!(
        bytes.len(),
        size_of::<T>(),
        "a parameter buffer has its struct's size"
    );
    // SAFETY: the length was checked and the type is plain old data.
    unsafe { ptr::read_unaligned(bytes.as_ptr().cast()) }
}

unsafe extern "C" fn get_display_drm(_fd: c_int) -> VADisplay {
    DISPLAY.with(|display| display.get().cast())
}

unsafe extern "C" fn initialize(
    display: VADisplay,
    major: *mut c_int,
    minor: *mut c_int,
) -> VAStatus {
    with_state(display, "vaInitialize", |_| {
        // SAFETY: the decoder passes live locals.
        unsafe {
            *major = 1;
            *minor = 24;
        }
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn terminate(display: VADisplay) -> VAStatus {
    with_state(display, "vaTerminate", |_| VA_STATUS_SUCCESS)
}

const unsafe extern "C" fn error_str(status: VAStatus) -> *const c_char {
    match status {
        VA_STATUS_ERROR_UNSUPPORTED_PROFILE => c"the requested profile is not supported".as_ptr(),
        _ => c"scripted failure".as_ptr(),
    }
}

const unsafe extern "C" fn max_num_entrypoints(_display: VADisplay) -> c_int {
    8
}

unsafe extern "C" fn query_config_entrypoints(
    display: VADisplay,
    profile: c_int,
    entrypoints: *mut c_int,
    count: *mut c_int,
) -> VAStatus {
    with_state(display, "vaQueryConfigEntrypoints", |state| {
        if !state.profiles.contains(&profile) {
            return VA_STATUS_ERROR_UNSUPPORTED_PROFILE;
        }
        // SAFETY: the buffer holds `vaMaxNumEntrypoints` (8) elements.
        unsafe {
            *entrypoints = VAEntrypointVLD;
            *count = 1;
        }
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn get_config_attributes(
    display: VADisplay,
    profile: c_int,
    entrypoint: c_int,
    attributes: *mut VAConfigAttrib,
    count: c_int,
) -> VAStatus {
    with_state(display, "vaGetConfigAttributes", |state| {
        assert!(state.profiles.contains(&profile));
        assert_eq!(entrypoint, VAEntrypointVLD);
        for index in 0..usize::try_from(count).unwrap() {
            // SAFETY: the decoder passes `count` attributes.
            let attribute = unsafe { &mut *attributes.add(index) };
            assert_eq!(attribute.type_, VAConfigAttribRTFormat);
            attribute.value = state.rt_format;
        }
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn create_config(
    display: VADisplay,
    profile: c_int,
    entrypoint: c_int,
    attributes: *mut VAConfigAttrib,
    count: c_int,
    id: *mut VAConfigID,
) -> VAStatus {
    with_state(display, "vaCreateConfig", |state| {
        assert!(state.profiles.contains(&profile));
        assert_eq!(entrypoint, VAEntrypointVLD);
        assert_eq!(count, 1);
        state.config_profiles.push(profile);
        state.codec = if matches!(profile, VAProfileHEVCMain | VAProfileHEVCMain10) {
            Codec::Hevc
        } else {
            Codec::H264
        };
        state.config_rt_format = if profile == VAProfileHEVCMain10 {
            VA_RT_FORMAT_YUV420_10
        } else {
            VA_RT_FORMAT_YUV420
        };
        // SAFETY: one attribute, as asserted.
        let attribute = unsafe { *attributes };
        assert_eq!(
            (attribute.type_, attribute.value),
            (VAConfigAttribRTFormat, state.config_rt_format)
        );
        // SAFETY: a live local.
        unsafe { *id = fresh_id(state) };
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn destroy_config(display: VADisplay, _id: VAConfigID) -> VAStatus {
    with_state(display, "vaDestroyConfig", |_| VA_STATUS_SUCCESS)
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
    with_state(display, "vaCreateSurfaces", |state| {
        if u64::from(width) * u64::from(height) > MAX_SURFACE_SAMPLES {
            return STATUS_RESOLUTION_NOT_SUPPORTED;
        }
        assert_eq!(attribute_count, 1);
        // SAFETY: one attribute, as asserted.
        let attribute = unsafe { *attributes };
        // SAFETY: the decoder stored the integer member.
        let fourcc = unsafe { attribute.value.value.i }.cast_unsigned();
        // The surfaces are of the format the config was created for.
        assert_eq!(format, state.config_rt_format);
        state.sample_bytes = match (format, fourcc) {
            (VA_RT_FORMAT_YUV420, VA_FOURCC_NV12) => 1,
            (VA_RT_FORMAT_YUV420_10, VA_FOURCC_P010) => 2,
            other => panic!("surfaces of {other:x?}"),
        };
        state.surface_size = (width, height);
        state.surface_count = count as usize;
        for index in 0..count {
            let id = SURFACE_BASE + state.next_id + index;
            state.live_surfaces.insert(id, 0);
            // SAFETY: the decoder passes `count` slots.
            unsafe { *ids.add(index as usize) = id };
        }
        state.next_id += count;
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn destroy_surfaces(
    display: VADisplay,
    ids: *mut VASurfaceID,
    count: c_int,
) -> VAStatus {
    with_state(display, "vaDestroySurfaces", |state| {
        for index in 0..usize::try_from(count).unwrap() {
            // SAFETY: the decoder passes the `count` ids it created.
            let id = unsafe { *ids.add(index) };
            assert!(
                state.live_surfaces.remove(&id).is_some(),
                "a surface is destroyed once"
            );
        }
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn create_context(
    display: VADisplay,
    _config: VAConfigID,
    width: c_int,
    height: c_int,
    flag: c_int,
    targets: *mut VASurfaceID,
    count: c_int,
    id: *mut VAContextID,
) -> VAStatus {
    with_state(display, "vaCreateContext", |state| {
        assert_eq!(flag, 1, "VA_PROGRESSIVE");
        assert_eq!(
            (
                u32::try_from(width).unwrap(),
                u32::try_from(height).unwrap()
            ),
            state.surface_size
        );
        assert_eq!(usize::try_from(count).unwrap(), state.surface_count);
        for index in 0..usize::try_from(count).unwrap() {
            // SAFETY: the decoder passes `count` surfaces.
            let target = unsafe { *targets.add(index) };
            assert!(state.live_surfaces.contains_key(&target));
        }
        state.context_size = (width, height);
        // SAFETY: a live local.
        unsafe { *id = fresh_id(state) };
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn destroy_context(display: VADisplay, _id: VAContextID) -> VAStatus {
    with_state(display, "vaDestroyContext", |_| VA_STATUS_SUCCESS)
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
    with_state(display, "vaCreateBuffer", |state| {
        assert_eq!(count, 1);
        assert!(!data.is_null(), "decode buffers carry their data");
        // SAFETY: libva reads `size` bytes from a non-null `data`.
        let bytes =
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size as usize) }.to_vec();
        let buffer = fresh_id(state);
        state.buffers.insert(buffer, (kind, bytes));
        // SAFETY: a live local.
        unsafe { *id = buffer };
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn destroy_buffer(display: VADisplay, id: VABufferID) -> VAStatus {
    with_state(display, "vaDestroyBuffer", |state| {
        assert!(
            state.buffers.remove(&id).is_some() || id == state.image_buffer,
            "a buffer is destroyed once"
        );
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn map_buffer(
    display: VADisplay,
    id: VABufferID,
    mapped: *mut *mut c_void,
) -> VAStatus {
    with_state(display, "vaMapBuffer", |state| {
        assert_eq!(id, state.image_buffer, "only the image is mapped");
        // SAFETY: a live local.
        unsafe { *mapped = state.image_data.as_mut_ptr().cast() };
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn unmap_buffer(display: VADisplay, _id: VABufferID) -> VAStatus {
    with_state(display, "vaUnmapBuffer", |_| VA_STATUS_SUCCESS)
}

unsafe extern "C" fn begin_picture(
    display: VADisplay,
    _context: VAContextID,
    target: VASurfaceID,
) -> VAStatus {
    with_state(display, "vaBeginPicture", |state| {
        assert!(state.begun.is_none(), "pictures do not nest");
        assert!(state.live_surfaces.contains_key(&target));
        state.begun = Some(target);
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn render_picture(
    display: VADisplay,
    _context: VAContextID,
    ids: *mut VABufferID,
    count: c_int,
) -> VAStatus {
    with_state(display, "vaRenderPicture", |state| {
        let target = state.begun.expect("rendering happens inside a picture");
        let buffers: Vec<(u32, Vec<u8>)> = (0..usize::try_from(count).unwrap())
            .map(|index| {
                // SAFETY: the decoder passes `count` ids.
                let id = unsafe { *ids.add(index) };
                state.buffers[&id].clone()
            })
            .collect();
        match state.codec {
            Codec::H264 => render_h264(state, target, &buffers),
            Codec::Hevc => render_hevc(state, target, &buffers),
        }
        VA_STATUS_SUCCESS
    })
}

fn render_h264(state: &mut State, target: VASurfaceID, buffers: &[(u32, Vec<u8>)]) {
    let mut parameters = None;
    let mut scaling: Option<VAIQMatrixBufferH264> = None;
    let mut slices: Vec<(VASliceParameterBufferH264, Vec<u8>)> = Vec::new();
    let mut pending: Option<VASliceParameterBufferH264> = None;
    for (kind, bytes) in buffers {
        match *kind {
            VAPictureParameterBufferType => parameters = Some(read(bytes)),
            VAIQMatrixBufferType => scaling = Some(read(bytes)),
            VASliceParameterBufferType => {
                assert!(pending.is_none(), "slice data follows its parameters");
                pending = Some(read(bytes));
            }
            VASliceDataBufferType => {
                let parameter = pending.take().expect("slice data has parameters");
                assert_eq!(parameter.slice_data_size as usize, bytes.len());
                assert_eq!(parameter.slice_data_offset, 0);
                slices.push((parameter, bytes.clone()));
            }
            other => panic!("unexpected buffer type {other}"),
        }
    }
    assert!(pending.is_none(), "every slice has data");
    assert!(scaling.is_some(), "a picture has scaling lists");
    state.submitted = Some(Picture {
        target,
        parameters: parameters.expect("a picture has parameters"),
        #[cfg(test)]
        scaling: scaling.expect("checked above"),
        slices,
        fill: 0,
    });
}

/// The `VAPictureHEVC` flags a driver accepts.
const HEVC_FLAGS: u32 = VA_PICTURE_HEVC_INVALID
    | VA_PICTURE_HEVC_LONG_TERM_REFERENCE
    | VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE
    | VA_PICTURE_HEVC_RPS_ST_CURR_AFTER
    | VA_PICTURE_HEVC_RPS_LT_CURR;

/// Validates the buffers of an H.265 picture the way a driver would.
#[allow(clippy::too_many_lines, reason = "one check after another")]
fn render_hevc(state: &mut State, target: VASurfaceID, buffers: &[(u32, Vec<u8>)]) {
    let mut parameters: Option<VAPictureParameterBufferHEVC> = None;
    let mut scaling: Option<VAIQMatrixBufferHEVC> = None;
    let mut slices: Vec<(VASliceParameterBufferHEVC, Vec<u8>)> = Vec::new();
    let mut pending: Option<VASliceParameterBufferHEVC> = None;
    for (kind, bytes) in buffers {
        match *kind {
            VAPictureParameterBufferType => {
                assert!(parameters.is_none() && slices.is_empty());
                parameters = Some(read(bytes));
            }
            VAIQMatrixBufferType => {
                assert!(scaling.is_none() && slices.is_empty());
                scaling = Some(read(bytes));
            }
            VASliceParameterBufferType => {
                assert!(pending.is_none(), "slice data follows its parameters");
                pending = Some(read(bytes));
            }
            VASliceDataBufferType => {
                let parameter = pending.take().expect("slice data has parameters");
                assert_eq!(parameter.slice_data_size as usize, bytes.len());
                assert_eq!(parameter.slice_data_offset, 0);
                assert_eq!(parameter.slice_data_flag, VA_SLICE_DATA_FLAG_ALL);
                assert!(
                    (2..parameter.slice_data_size).contains(&parameter.slice_data_byte_offset),
                    "the slice data starts after the NAL unit header and inside the unit"
                );
                slices.push((parameter, bytes.clone()));
            }
            other => panic!("unexpected buffer type {other}"),
        }
    }
    assert!(pending.is_none(), "every slice has data");
    let picture = parameters.expect("a picture has parameters");
    assert!(!slices.is_empty(), "a picture has slices");
    assert_eq!(picture.CurrPic.picture_id, target);
    assert_eq!(picture.CurrPic.flags, 0);
    assert_eq!(
        (
            u32::from(picture.pic_width_in_luma_samples),
            u32::from(picture.pic_height_in_luma_samples)
        ),
        state.surface_size,
        "the picture is the size of the surfaces"
    );
    assert_eq!(
        usize::from(picture.bit_depth_luma_minus8) / 2 + 1,
        state.sample_bytes,
        "the bit depth is the surfaces'"
    );
    assert_eq!(
        picture.bit_depth_luma_minus8,
        picture.bit_depth_chroma_minus8
    );
    assert_eq!(
        picture.pic_fields & 0b111,
        1,
        "4:2:0 without separate planes"
    );
    // The scaling lists go with the flag that enables them.
    assert_eq!(
        scaling.is_some(),
        picture.pic_fields & (1 << 4) != 0,
        "scaling lists are sent exactly when they are enabled"
    );
    let mut valid = Vec::new();
    for (index, reference) in picture.ReferenceFrames.iter().enumerate() {
        assert_eq!(reference.flags & !HEVC_FLAGS, 0);
        if reference.flags & VA_PICTURE_HEVC_INVALID != 0 {
            continue;
        }
        assert_ne!(
            state.live_surfaces.get(&reference.picture_id).copied(),
            Some(0),
            "a reference is a decoded surface"
        );
        assert_ne!(reference.picture_id, target);
        valid.push(index);
    }
    // The valid entries come first.
    assert!(
        valid
            .iter()
            .enumerate()
            .all(|(position, &index)| position == index)
    );
    let last = slices.len() - 1;
    for (position, (slice, _)) in slices.iter().enumerate() {
        assert_eq!(
            slice.LongSliceFlags & 1 != 0,
            position == last,
            "exactly the last slice segment ends the picture"
        );
        let slice_type = (slice.LongSliceFlags >> 2) & 3;
        assert!(slice_type <= 2);
        let lists = match slice_type {
            0 => 2,
            1 => 1,
            _ => 0,
        };
        for (list, entries) in slice.RefPicList.iter().enumerate() {
            let active = if list == 0 {
                slice.num_ref_idx_l0_active_minus1
            } else {
                slice.num_ref_idx_l1_active_minus1
            };
            for (entry_index, &entry) in entries.iter().enumerate() {
                if list < lists && entry_index <= usize::from(active) {
                    assert!(
                        usize::from(entry) < valid.len(),
                        "an active reference names a reference frame"
                    );
                } else {
                    assert_eq!(entry, VA_HEVC_REF_INVALID);
                }
            }
        }
        if slice.collocated_ref_idx != VA_HEVC_REF_INVALID {
            assert!(slice_type != 2 && slice.LongSliceFlags & (1 << 10) != 0);
        }
    }
    state.submitted_hevc = Some(HevcPicture {
        target,
        parameters: picture,
        #[cfg(test)]
        scaling,
        slices,
        fill: 0,
    });
}

unsafe extern "C" fn end_picture(display: VADisplay, _context: VAContextID) -> VAStatus {
    // A picture ends whether or not the call fails.
    // SAFETY: the decoder passes the pointer `Driver::new` made as display.
    let (begun, submitted, submitted_hevc) = {
        let state = unsafe { &mut *display.cast::<State>() };
        (
            state.begun.take(),
            state.submitted.take(),
            state.submitted_hevc.take(),
        )
    };
    with_state(display, "vaEndPicture", |state| {
        let target = begun.expect("a picture was begun");
        match state.codec {
            Codec::H264 => {
                let mut picture = submitted.expect("a picture was rendered");
                assert_eq!(picture.target, target);
                assert_eq!(picture.parameters.CurrPic.picture_id, target);
                picture.fill = fill_of(state.pictures.len() + 1);
                state.live_surfaces.insert(target, picture.fill);
                state.pictures.push(picture);
            }
            Codec::Hevc => {
                let mut picture = submitted_hevc.expect("a picture was rendered");
                assert_eq!(picture.target, target);
                picture.fill = fill_of(state.hevc_pictures.len() + 1);
                state.live_surfaces.insert(target, picture.fill);
                state.hevc_pictures.push(picture);
            }
        }
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn sync_surface(display: VADisplay, surface: VASurfaceID) -> VAStatus {
    with_state(display, "vaSyncSurface", |state| {
        assert!(state.live_surfaces.contains_key(&surface));
        VA_STATUS_SUCCESS
    })
}

const unsafe extern "C" fn max_num_image_formats(_display: VADisplay) -> c_int {
    4
}

unsafe extern "C" fn query_image_formats(
    display: VADisplay,
    formats: *mut VAImageFormat,
    count: *mut c_int,
) -> VAStatus {
    with_state(display, "vaQueryImageFormats", |_| {
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
            *formats.add(2) = VAImageFormat {
                fourcc: VA_FOURCC_P010,
                bits_per_pixel: 24,
                ..VAImageFormat::default()
            };
            *count = 3;
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
    with_state(display, "vaCreateImage", |state| {
        assert_eq!(
            (width, height),
            state.context_size,
            "the image is the coded size"
        );
        // SAFETY: a live local.
        let fourcc = unsafe { (*format).fourcc };
        assert_eq!(
            fourcc,
            if state.sample_bytes == 2 {
                VA_FOURCC_P010
            } else {
                VA_FOURCC_NV12
            },
            "the image is in the format of the surfaces"
        );
        let (width_samples, height_samples) = (width as usize, height as usize);
        let pitch = width_samples * state.sample_bytes + PITCH_PADDING;
        let size = pitch * height_samples * 3 / 2;
        state.image_data = vec![0xEE; size];
        state.image_size = (width, height);
        let buffer = fresh_id(state);
        state.image_buffer = buffer;
        let pitch_u32 = u32::try_from(pitch).unwrap();
        // SAFETY: a live local.
        unsafe {
            *image = VAImage {
                image_id: fresh_id(state),
                format: *format,
                buf: buffer,
                width: u16::try_from(width).unwrap(),
                height: u16::try_from(height).unwrap(),
                data_size: u32::try_from(size).unwrap(),
                num_planes: 2,
                pitches: [pitch_u32, pitch_u32, 0],
                offsets: [0, pitch_u32 * u32::try_from(height).unwrap(), 0],
                ..VAImage::default()
            };
        }
        VA_STATUS_SUCCESS
    })
}

unsafe extern "C" fn destroy_image(display: VADisplay, _id: VAImageID) -> VAStatus {
    with_state(display, "vaDestroyImage", |_| VA_STATUS_SUCCESS)
}

unsafe extern "C" fn put_image(
    display: VADisplay,
    _surface: VASurfaceID,
    _image: VAImageID,
    _src_x: c_int,
    _src_y: c_int,
    _src_width: c_uint,
    _src_height: c_uint,
    _dest_x: c_int,
    _dest_y: c_int,
    _dest_width: c_uint,
    _dest_height: c_uint,
) -> VAStatus {
    with_state(display, "vaPutImage", |_| STATUS_FAILED)
}

unsafe extern "C" fn get_image(
    display: VADisplay,
    surface: VASurfaceID,
    x: c_int,
    y: c_int,
    width: c_uint,
    height: c_uint,
    _image: VAImageID,
) -> VAStatus {
    with_state(display, "vaGetImage", |state| {
        assert_eq!((x, y), (0, 0));
        assert_eq!(
            (i64::from(width), i64::from(height)),
            (i64::from(state.image_size.0), i64::from(state.image_size.1))
        );
        let fill = state.live_surfaces[&surface];
        assert_ne!(fill, 0, "only decoded surfaces are read");
        let (width, height) = (width as usize, height as usize);
        let bytes = state.sample_bytes;
        let pitch = width * bytes + PITCH_PADDING;
        // A 10-bit sample is the 8-bit one in the top bits of a
        // little-endian 16-bit word.
        let mut put = |offset: usize, value: u8| {
            if bytes == 2 {
                state.image_data[offset] = 0;
                state.image_data[offset + 1] = value;
            } else {
                state.image_data[offset] = value;
            }
        };
        for row in 0..height {
            for column in 0..width {
                put(row * pitch + column * bytes, luma(fill, column, row));
            }
        }
        let chroma_base = pitch * height;
        for row in 0..height / 2 {
            for column in 0..width {
                put(
                    chroma_base + row * pitch + column * bytes,
                    chroma(fill, column, row),
                );
            }
        }
        VA_STATUS_SUCCESS
    })
}
