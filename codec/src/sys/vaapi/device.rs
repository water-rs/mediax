//! A VA display on a render node and the libva objects created on it, shared
//! by the encoder and the decoder.

use super::{
    VaapiError,
    api::Api,
    types::{
        VA_FOURCC_NV12, VA_INVALID_ID, VA_PROGRESSIVE, VA_RT_FORMAT_YUV420,
        VA_STATUS_ERROR_UNSUPPORTED_PROFILE, VA_STATUS_SUCCESS, VA_SURFACE_ATTRIB_SETTABLE,
        VABufferID, VAConfigID, VAContextID, VADisplay, VAGenericValue, VAGenericValueTypeInteger,
        VAGenericValueUnion, VAImage, VAImageFormat, VAStatus, VASurfaceAttrib,
        VASurfaceAttribPixelFormat, VASurfaceID,
    },
};
use std::{
    ffi::{CStr, c_int, c_void},
    fs::File,
    mem::size_of,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    ptr,
    rc::Rc,
    slice,
};

/// The directory render nodes live in.
const DRI_DIRECTORY: &str = "/dev/dri";
/// The prefix of render node names.
const RENDER_NODE_PREFIX: &str = "renderD";

/// A VA display on a render node, terminated when the last user is gone.
pub struct Device {
    pub api: Api,
    pub display: VADisplay,
    initialized: bool,
    _node: File,
}

impl Device {
    pub fn open(api: Api, path: &Path) -> Result<Self, VaapiError> {
        let node = File::options()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| VaapiError::DeviceOpen {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        // SAFETY: `vaGetDisplayDRM` only reads the descriptor, which `node`
        // keeps open for as long as the display lives.
        let display = unsafe { (api.get_display_drm)(node.as_raw_fd()) };
        if display.is_null() {
            return Err(VaapiError::NoDisplay);
        }
        let mut device = Self {
            api,
            display,
            initialized: false,
            _node: node,
        };
        let (mut major, mut minor) = (0, 0);
        // SAFETY: `display` came from `vaGetDisplayDRM` and the out
        // parameters are live locals.
        let status = unsafe { (device.api.initialize)(display, &raw mut major, &raw mut minor) };
        device.check("vaInitialize", status)?;
        device.initialized = true;
        tracing::debug!(major, minor, node = %path.display(), "VA-API display initialised");
        Ok(device)
    }

    pub fn check(&self, call: &'static str, status: VAStatus) -> Result<(), VaapiError> {
        if status == VA_STATUS_SUCCESS {
            return Ok(());
        }
        // SAFETY: `vaErrorStr` takes any status and returns a static string.
        let text = unsafe { (self.api.error_str)(status) };
        let message = if text.is_null() {
            String::from("unknown error")
        } else {
            // SAFETY: a non-null result is a NUL-terminated static string.
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned()
        };
        Err(VaapiError::Driver {
            call,
            status,
            message,
        })
    }

    /// Reports a failed teardown call; nothing more can be done with it.
    pub fn log_teardown(&self, call: &'static str, status: VAStatus) {
        if let Err(error) = self.check(call, status) {
            tracing::warn!(%error, "VA-API teardown failed");
        }
    }

    /// The entry points the driver offers for `profile`, or `None` when it
    /// does not support the profile.
    pub fn entrypoints(&self, profile: c_int) -> Result<Option<Vec<c_int>>, VaapiError> {
        // SAFETY: the display is initialised.
        let capacity = unsafe { (self.api.max_num_entrypoints)(self.display) };
        let mut entrypoints = vec![0; usize::try_from(capacity).unwrap_or_default()];
        let mut count = 0;
        // SAFETY: `entrypoints` holds `vaMaxNumEntrypoints` elements.
        let status = unsafe {
            (self.api.query_config_entrypoints)(
                self.display,
                profile,
                entrypoints.as_mut_ptr(),
                &raw mut count,
            )
        };
        if status == VA_STATUS_ERROR_UNSUPPORTED_PROFILE {
            return Ok(None);
        }
        self.check("vaQueryConfigEntrypoints", status)?;
        entrypoints.truncate(usize::try_from(count).unwrap_or_default());
        Ok(Some(entrypoints))
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: the display was initialised and every object created
            // on it has been destroyed, as the owners hold an `Rc<Device>`.
            let status = unsafe { (self.api.terminate)(self.display) };
            self.log_teardown("vaTerminate", status);
        }
    }
}

/// A `VAConfigID`.
pub struct Config {
    device: Rc<Device>,
    pub id: VAConfigID,
}

impl Config {
    /// Wraps a config id created on `device`.
    pub const fn new(device: Rc<Device>, id: VAConfigID) -> Self {
        Self { device, id }
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        // SAFETY: `id` came from `vaCreateConfig` on this display.
        let status = unsafe { (self.device.api.destroy_config)(self.device.display, self.id) };
        self.device.log_teardown("vaDestroyConfig", status);
    }
}

/// NV12 surfaces of one size.
pub struct Surfaces {
    device: Rc<Device>,
    pub ids: Vec<VASurfaceID>,
}

impl Surfaces {
    /// Creates `count` NV12 surfaces of `width` by `height` samples.
    pub fn create(
        device: &Rc<Device>,
        width: u32,
        height: u32,
        count: usize,
    ) -> Result<Self, VaapiError> {
        let mut attribute = VASurfaceAttrib {
            type_: VASurfaceAttribPixelFormat,
            flags: VA_SURFACE_ATTRIB_SETTABLE,
            value: VAGenericValue {
                type_: VAGenericValueTypeInteger,
                value: VAGenericValueUnion {
                    i: i32::from_ne_bytes(VA_FOURCC_NV12.to_ne_bytes()),
                },
            },
        };
        let mut ids = vec![VA_INVALID_ID; count];
        let count = u32::try_from(count).map_err(|_| VaapiError::SizeOutOfRange("surfaces"))?;
        // SAFETY: the pointers and counts describe live locals.
        let status = unsafe {
            (device.api.create_surfaces)(
                device.display,
                VA_RT_FORMAT_YUV420,
                width,
                height,
                ids.as_mut_ptr(),
                count,
                &raw mut attribute,
                1,
            )
        };
        device.check("vaCreateSurfaces", status)?;
        Ok(Self {
            device: Rc::clone(device),
            ids,
        })
    }
}

impl Drop for Surfaces {
    fn drop(&mut self) {
        let Ok(count) = c_int::try_from(self.ids.len()) else {
            return;
        };
        // SAFETY: the ids came from `vaCreateSurfaces` on this display.
        let status = unsafe {
            (self.device.api.destroy_surfaces)(self.device.display, self.ids.as_mut_ptr(), count)
        };
        self.device.log_teardown("vaDestroySurfaces", status);
    }
}

/// A `VAContextID`.
pub struct Context {
    device: Rc<Device>,
    pub id: VAContextID,
}

impl Context {
    /// Creates a progressive context rendering into `surfaces`.
    pub fn create(
        device: &Rc<Device>,
        config: &Config,
        width: u32,
        height: u32,
        surfaces: &Surfaces,
    ) -> Result<Self, VaapiError> {
        let width =
            c_int::try_from(width).map_err(|_| VaapiError::SizeOutOfRange("frame width"))?;
        let height =
            c_int::try_from(height).map_err(|_| VaapiError::SizeOutOfRange("frame height"))?;
        let count = c_int::try_from(surfaces.ids.len())
            .map_err(|_| VaapiError::SizeOutOfRange("surface count"))?;
        let mut targets = surfaces.ids.clone();
        let mut id = VA_INVALID_ID;
        // SAFETY: the pointers and the count describe live locals.
        let status = unsafe {
            (device.api.create_context)(
                device.display,
                config.id,
                width,
                height,
                VA_PROGRESSIVE,
                targets.as_mut_ptr(),
                count,
                &raw mut id,
            )
        };
        device.check("vaCreateContext", status)?;
        Ok(Self {
            device: Rc::clone(device),
            id,
        })
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: `id` came from `vaCreateContext` on this display.
        let status = unsafe { (self.device.api.destroy_context)(self.device.display, self.id) };
        self.device.log_teardown("vaDestroyContext", status);
    }
}

/// A `VABufferID`, destroyed on drop.
pub struct Buffer {
    device: Rc<Device>,
    pub id: VABufferID,
}

impl Buffer {
    /// Wraps a buffer id created on `device`.
    pub const fn new(device: Rc<Device>, id: VABufferID) -> Self {
        Self { device, id }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: `id` came from `vaCreateBuffer` on this display.
        let status = unsafe { (self.device.api.destroy_buffer)(self.device.display, self.id) };
        self.device.log_teardown("vaDestroyBuffer", status);
    }
}

/// The staging `VAImage` frames are moved through.
pub struct Image {
    device: Rc<Device>,
    pub image: VAImage,
}

impl Image {
    /// Creates an NV12 image of `width` by `height` samples.
    pub fn create(device: &Rc<Device>, width: u32, height: u32) -> Result<Self, VaapiError> {
        // SAFETY: the display is initialised.
        let capacity = unsafe { (device.api.max_num_image_formats)(device.display) };
        let mut formats =
            vec![VAImageFormat::default(); usize::try_from(capacity).unwrap_or_default()];
        let mut count = 0;
        // SAFETY: `formats` holds `vaMaxNumImageFormats` elements.
        let status = unsafe {
            (device.api.query_image_formats)(device.display, formats.as_mut_ptr(), &raw mut count)
        };
        device.check("vaQueryImageFormats", status)?;
        let mut format = formats
            .get(..usize::try_from(count).unwrap_or_default())
            .unwrap_or_default()
            .iter()
            .find(|format| format.fourcc == VA_FOURCC_NV12)
            .copied()
            .ok_or(VaapiError::NoNv12ImageFormat)?;
        let width =
            c_int::try_from(width).map_err(|_| VaapiError::SizeOutOfRange("frame width"))?;
        let height =
            c_int::try_from(height).map_err(|_| VaapiError::SizeOutOfRange("frame height"))?;
        let mut image = VAImage::default();
        // SAFETY: the pointers refer to live locals.
        let status = unsafe {
            (device.api.create_image)(
                device.display,
                &raw mut format,
                width,
                height,
                &raw mut image,
            )
        };
        device.check("vaCreateImage", status)?;
        Ok(Self {
            device: Rc::clone(device),
            image,
        })
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        // SAFETY: the image came from `vaCreateImage` on this display.
        let status =
            unsafe { (self.device.api.destroy_image)(self.device.display, self.image.image_id) };
        self.device.log_teardown("vaDestroyImage", status);
    }
}

pub fn to_usize(value: u32) -> Result<usize, VaapiError> {
    usize::try_from(value).map_err(|_| VaapiError::SizeOutOfRange("size"))
}

impl Device {
    /// Maps `buffer`, runs `read` on the mapping and unmaps it. The mapping
    /// is null only when the driver misbehaves, which `read` must reject.
    pub fn with_mapping<T>(
        &self,
        buffer: VABufferID,
        read: impl FnOnce(*mut c_void) -> Result<T, VaapiError>,
    ) -> Result<T, VaapiError> {
        let mut mapped: *mut c_void = ptr::null_mut();
        // SAFETY: `buffer` belongs to this display and `mapped` is a live local.
        let status = unsafe { (self.api.map_buffer)(self.display, buffer, &raw mut mapped) };
        self.check("vaMapBuffer", status)?;
        let result = read(mapped);
        // SAFETY: the buffer was mapped above.
        let status = unsafe { (self.api.unmap_buffer)(self.display, buffer) };
        self.check("vaUnmapBuffer", status)?;
        result
    }

    /// Maps `buffer` and runs `read` on its first `size` bytes.
    pub fn with_mapped_bytes<T>(
        &self,
        buffer: VABufferID,
        size: usize,
        what: &'static str,
        read: impl FnOnce(&mut [u8]) -> Result<T, VaapiError>,
    ) -> Result<T, VaapiError> {
        self.with_mapping(buffer, |mapped| {
            if mapped.is_null() {
                return Err(VaapiError::BufferTooSmall(what));
            }
            // SAFETY: the driver maps the buffer's `size` bytes at `mapped`
            // until it is unmapped, and nothing else touches them.
            read(unsafe { slice::from_raw_parts_mut(mapped.cast::<u8>(), size) })
        })
    }
}

/// The buffers recorded for one picture, destroyed when it is done.
pub struct PictureBuffers {
    device: Rc<Device>,
    context: VAContextID,
    ids: Vec<VABufferID>,
}

impl PictureBuffers {
    pub fn new(device: &Rc<Device>, context: VAContextID) -> Self {
        Self {
            device: Rc::clone(device),
            context,
            ids: Vec::new(),
        }
    }

    /// Adds a buffer of `kind` holding `count` elements of `size` bytes read
    /// from `data`.
    pub fn add_raw(
        &mut self,
        kind: u32,
        size: usize,
        count: usize,
        data: *mut c_void,
    ) -> Result<(), VaapiError> {
        let size = u32::try_from(size).map_err(|_| VaapiError::SizeOutOfRange("buffer"))?;
        let count = u32::try_from(count).map_err(|_| VaapiError::SizeOutOfRange("buffer"))?;
        let mut id = VA_INVALID_ID;
        // SAFETY: libva copies `size * count` bytes from `data`, which points
        // at live memory of that size; `id` is a live local.
        let status = unsafe {
            (self.device.api.create_buffer)(
                self.device.display,
                self.context,
                kind,
                size,
                count,
                data,
                &raw mut id,
            )
        };
        self.device.check("vaCreateBuffer", status)?;
        self.ids.push(id);
        Ok(())
    }

    /// Adds a buffer holding a copy of `value`.
    pub fn add<T>(&mut self, kind: u32, value: &T) -> Result<(), VaapiError> {
        self.add_raw(
            kind,
            size_of::<T>(),
            1,
            ptr::from_ref(value).cast_mut().cast(),
        )
    }

    /// Adds a buffer holding a copy of `bytes`.
    pub fn add_bytes(&mut self, kind: u32, bytes: &[u8]) -> Result<(), VaapiError> {
        self.add_raw(kind, bytes.len(), 1, bytes.as_ptr().cast_mut().cast())
    }

    /// Renders the recorded buffers into `target` and waits for the hardware.
    pub fn render(&mut self, target: VASurfaceID) -> Result<(), VaapiError> {
        let device = &*self.device;
        // SAFETY: the context and the surface belong to this display.
        let status = unsafe { (device.api.begin_picture)(device.display, self.context, target) };
        device.check("vaBeginPicture", status)?;
        let count = c_int::try_from(self.ids.len())
            .map_err(|_| VaapiError::SizeOutOfRange("buffer count"))?;
        // SAFETY: `ids` are live buffers of this context.
        let rendered = device.check("vaRenderPicture", unsafe {
            (device.api.render_picture)(device.display, self.context, self.ids.as_mut_ptr(), count)
        });
        // SAFETY: a picture was begun on this context; it must be ended
        // whether or not rendering succeeded.
        let ended = device.check("vaEndPicture", unsafe {
            (device.api.end_picture)(device.display, self.context)
        });
        rendered?;
        ended?;
        // SAFETY: the surface belongs to this display.
        let status = unsafe { (device.api.sync_surface)(device.display, target) };
        device.check("vaSyncSurface", status)
    }
}

impl Drop for PictureBuffers {
    fn drop(&mut self) {
        for &id in &self.ids {
            // SAFETY: `id` came from `vaCreateBuffer` on this display.
            let status = unsafe { (self.device.api.destroy_buffer)(self.device.display, id) };
            self.device.log_teardown("vaDestroyBuffer", status);
        }
    }
}

/// Lists the DRM render nodes, in name order.
pub fn render_nodes() -> Result<Vec<PathBuf>, VaapiError> {
    let entries =
        std::fs::read_dir(DRI_DIRECTORY).map_err(|_| VaapiError::NoRenderNode(DRI_DIRECTORY))?;
    let mut nodes: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(RENDER_NODE_PREFIX)
        })
        .map(|entry| entry.path())
        .collect();
    nodes.sort();
    if nodes.is_empty() {
        Err(VaapiError::NoRenderNode(DRI_DIRECTORY))
    } else {
        Ok(nodes)
    }
}
