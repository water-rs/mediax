//! Platform-specific hardware codec implementations.

#[cfg(mediax_hw_codec_apple)]
pub mod apple;

#[cfg(mediax_hw_codec_android)]
pub mod android;

#[cfg(mediax_hw_codec_windows)]
pub mod windows;

#[cfg(mediax_hw_codec_vaapi)]
pub mod linux;
