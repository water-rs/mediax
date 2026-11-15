//! Platform-specific hardware codec implementations.

#[cfg(mediax_hw_codec_apple)]
pub mod apple;

#[cfg(mediax_hw_codec_android)]
pub mod android;

#[cfg(mediax_hw_codec_windows)]
pub mod windows;

// The driver-double tests need unix file descriptors and a dynamic loader.
#[cfg(any(all(test, unix), mediax_vaapi_fuzzing, mediax_hw_codec_vaapi))]
#[cfg_attr(
    all(feature = "fuzzing", not(test), not(mediax_hw_codec_vaapi)),
    expect(
        dead_code,
        reason = "fuzz targets use the driver double rather than the native backend"
    )
)]
pub mod vaapi;
