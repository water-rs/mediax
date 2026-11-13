//! The libva entry points the encoder and decoder call, loaded at run time.

use super::{
    VaapiError,
    types::{
        VABufferID, VAConfigAttrib, VAConfigID, VAContextID, VADisplay, VAImage, VAImageFormat,
        VAImageID, VAStatus, VASurfaceAttrib, VASurfaceID,
    },
};
use libloading::Library;
use std::ffi::{c_char, c_int, c_uint, c_void};

/// The soname of libva.
pub const LIBVA: &str = "libva.so.2";
/// The soname of libva's DRM backend.
pub const LIBVA_DRM: &str = "libva-drm.so.2";

/// Declares [`Api`] with one function pointer per libva symbol.
macro_rules! libva_functions {
    ($($field:ident: $signature:ty = $symbol:literal;)*) => {
        /// The libva entry points, resolved once.
        ///
        /// The function pointers stay valid for as long as `libraries` is
        /// alive, which is as long as the `Api` is.
        pub struct Api {
            $(
                #[doc = concat!("`", $symbol, "`.")]
                pub $field: $signature,
            )*
            /// `vaGetDisplayDRM`, from libva-drm.
            pub get_display_drm: unsafe extern "C" fn(c_int) -> VADisplay,
            /// The mapped libraries; `None` for a test double.
            pub libraries: Option<Libraries>,
        }

        impl Api {
            fn resolve(libva: &Library, libva_drm: &Library) -> Result<Self, VaapiError> {
                Ok(Self {
                    $($field: symbol(libva, $symbol)?,)*
                    get_display_drm: symbol(libva_drm, "vaGetDisplayDRM")?,
                    libraries: None,
                })
            }
        }
    };
}

libva_functions! {
    initialize: unsafe extern "C" fn(VADisplay, *mut c_int, *mut c_int) -> VAStatus = "vaInitialize";
    terminate: unsafe extern "C" fn(VADisplay) -> VAStatus = "vaTerminate";
    error_str: unsafe extern "C" fn(VAStatus) -> *const c_char = "vaErrorStr";
    max_num_entrypoints: unsafe extern "C" fn(VADisplay) -> c_int = "vaMaxNumEntrypoints";
    query_config_entrypoints:
        unsafe extern "C" fn(VADisplay, c_int, *mut c_int, *mut c_int) -> VAStatus
        = "vaQueryConfigEntrypoints";
    get_config_attributes:
        unsafe extern "C" fn(VADisplay, c_int, c_int, *mut VAConfigAttrib, c_int) -> VAStatus
        = "vaGetConfigAttributes";
    create_config:
        unsafe extern "C" fn(VADisplay, c_int, c_int, *mut VAConfigAttrib, c_int, *mut VAConfigID)
            -> VAStatus
        = "vaCreateConfig";
    destroy_config: unsafe extern "C" fn(VADisplay, VAConfigID) -> VAStatus = "vaDestroyConfig";
    create_surfaces:
        unsafe extern "C" fn(
            VADisplay,
            c_uint,
            c_uint,
            c_uint,
            *mut VASurfaceID,
            c_uint,
            *mut VASurfaceAttrib,
            c_uint,
        ) -> VAStatus
        = "vaCreateSurfaces";
    destroy_surfaces:
        unsafe extern "C" fn(VADisplay, *mut VASurfaceID, c_int) -> VAStatus
        = "vaDestroySurfaces";
    create_context:
        unsafe extern "C" fn(
            VADisplay,
            VAConfigID,
            c_int,
            c_int,
            c_int,
            *mut VASurfaceID,
            c_int,
            *mut VAContextID,
        ) -> VAStatus
        = "vaCreateContext";
    destroy_context: unsafe extern "C" fn(VADisplay, VAContextID) -> VAStatus = "vaDestroyContext";
    create_buffer:
        unsafe extern "C" fn(
            VADisplay,
            VAContextID,
            c_uint,
            c_uint,
            c_uint,
            *mut c_void,
            *mut VABufferID,
        ) -> VAStatus
        = "vaCreateBuffer";
    destroy_buffer: unsafe extern "C" fn(VADisplay, VABufferID) -> VAStatus = "vaDestroyBuffer";
    map_buffer:
        unsafe extern "C" fn(VADisplay, VABufferID, *mut *mut c_void) -> VAStatus = "vaMapBuffer";
    unmap_buffer: unsafe extern "C" fn(VADisplay, VABufferID) -> VAStatus = "vaUnmapBuffer";
    begin_picture:
        unsafe extern "C" fn(VADisplay, VAContextID, VASurfaceID) -> VAStatus = "vaBeginPicture";
    render_picture:
        unsafe extern "C" fn(VADisplay, VAContextID, *mut VABufferID, c_int) -> VAStatus
        = "vaRenderPicture";
    end_picture: unsafe extern "C" fn(VADisplay, VAContextID) -> VAStatus = "vaEndPicture";
    sync_surface: unsafe extern "C" fn(VADisplay, VASurfaceID) -> VAStatus = "vaSyncSurface";
    max_num_image_formats: unsafe extern "C" fn(VADisplay) -> c_int = "vaMaxNumImageFormats";
    query_image_formats:
        unsafe extern "C" fn(VADisplay, *mut VAImageFormat, *mut c_int) -> VAStatus
        = "vaQueryImageFormats";
    create_image:
        unsafe extern "C" fn(VADisplay, *mut VAImageFormat, c_int, c_int, *mut VAImage) -> VAStatus
        = "vaCreateImage";
    destroy_image: unsafe extern "C" fn(VADisplay, VAImageID) -> VAStatus = "vaDestroyImage";
    put_image:
        unsafe extern "C" fn(
            VADisplay,
            VASurfaceID,
            VAImageID,
            c_int,
            c_int,
            c_uint,
            c_uint,
            c_int,
            c_int,
            c_uint,
            c_uint,
        ) -> VAStatus
        = "vaPutImage";
    get_image:
        unsafe extern "C" fn(VADisplay, VASurfaceID, c_int, c_int, c_uint, c_uint, VAImageID)
            -> VAStatus
        = "vaGetImage";
}

/// The two libraries the function pointers point into.
#[derive(Debug)]
pub struct Libraries {
    _libva: Library,
    _libva_drm: Library,
}

impl Api {
    /// Loads libva and libva-drm and resolves every entry point.
    ///
    /// # Errors
    ///
    /// Returns [`VaapiError::LibraryUnavailable`] when either library cannot
    /// be loaded and [`VaapiError::MissingSymbol`] when one lacks an entry
    /// point.
    pub fn load() -> Result<Self, VaapiError> {
        // SAFETY: libva and libva-drm run no unsound initialisation code on
        // load; they are the system's own VA-API libraries.
        let libva =
            unsafe { Library::new(LIBVA) }.map_err(|error| VaapiError::LibraryUnavailable {
                library: LIBVA,
                reason: error.to_string(),
            })?;
        // SAFETY: as above.
        let libva_drm =
            unsafe { Library::new(LIBVA_DRM) }.map_err(|error| VaapiError::LibraryUnavailable {
                library: LIBVA_DRM,
                reason: error.to_string(),
            })?;
        let mut api = Self::resolve(&libva, &libva_drm)?;
        api.libraries = Some(Libraries {
            _libva: libva,
            _libva_drm: libva_drm,
        });
        Ok(api)
    }
}

/// Resolves `name` in `library` as the function pointer type `T`.
fn symbol<T: Copy>(library: &Library, name: &'static str) -> Result<T, VaapiError> {
    // SAFETY: every call site names the symbol's signature from `va.h`; the
    // pointer is only used while the `Library` it came from is alive.
    let symbol = unsafe { library.get::<T>(name.as_bytes()) }.map_err(|error| {
        VaapiError::MissingSymbol {
            name,
            reason: error.to_string(),
        }
    })?;
    Ok(*symbol)
}
