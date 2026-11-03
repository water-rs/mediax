//! Linux hardware H.264 encoding through VA-API.
//!
//! libva is loaded with `dlopen` when an encoder is created, so a host
//! without it builds and runs, and the constructor reports why there is no
//! encoder. The module also compiles in tests on other hosts, where the
//! encoder runs against a test double of the driver.

mod api;
mod bits;
mod encoder;
mod headers;
mod plan;
#[cfg(test)]
mod tests;
mod types;

#[cfg(mediax_hw_codec_vaapi)]
pub use encoder::VaapiEncoder;

use crate::{CodecError, FrameRate, H264Level, H264Profile};
use plan::RateControlMode;
use thiserror::Error;

/// Why a VA-API operation failed.
#[derive(Debug, Clone, Error)]
pub enum VaapiError {
    /// A libva shared library could not be loaded.
    #[error("{library} is not available on this host: {reason}")]
    LibraryUnavailable {
        /// The soname that failed to load.
        library: &'static str,
        /// The loader's message.
        reason: String,
    },
    /// A loaded libva library lacks an entry point.
    #[error("libva has no {name}: {reason}")]
    MissingSymbol {
        /// The symbol.
        name: &'static str,
        /// The loader's message.
        reason: String,
    },
    /// No DRM render node exists.
    #[error("no DRM render node found under {0}")]
    NoRenderNode(&'static str),
    /// No render node produced a working encoder.
    #[error("no render node could encode this stream: {attempts}")]
    NoUsableDevice {
        /// Each node tried with the reason it was rejected.
        attempts: String,
    },
    /// A render node could not be opened.
    #[error("cannot open the render node {path}: {reason}")]
    DeviceOpen {
        /// The device path.
        path: String,
        /// The operating system's message.
        reason: String,
    },
    /// libva returned an error status.
    #[error("{call} failed: {message} ({status:#x})")]
    Driver {
        /// The libva function.
        call: &'static str,
        /// The `VAStatus`.
        status: i32,
        /// `vaErrorStr` for the status.
        message: String,
    },
    /// `vaGetDisplayDRM` returned no display.
    #[error("libva could not create a display for the render node")]
    NoDisplay,
    /// The driver offers no slice encode entry point for the profile.
    #[error(
        "the driver has no H.264 {profile:?} encode entry point (VAEntrypointEncSlice or VAEntrypointEncSliceLP)"
    )]
    NoEncodeEntrypoint {
        /// The requested profile.
        profile: H264Profile,
    },
    /// The encode entry point does not take 4:2:0 8-bit surfaces.
    #[error("the driver's H.264 encoder does not support the YUV 4:2:0 render target format")]
    UnsupportedRtFormat,
    /// The encode entry point lacks the rate control mode.
    #[error("the driver's H.264 encoder does not support {0} rate control")]
    UnsupportedRateControl(RateControlMode),
    /// The encode entry point cannot take packed sequence, picture and slice
    /// headers.
    #[error(
        "the driver's H.264 encoder takes packed headers {supported:#x}; sequence, picture and slice (0x7) are required"
    )]
    UnsupportedPackedHeaders {
        /// The `VAConfigAttribEncPackedHeaders` value.
        supported: u32,
    },
    /// The encode entry point cannot hold a reference frame.
    #[error("the driver's H.264 encoder keeps no reference frame")]
    NoReferenceFrames,
    /// The driver has no NV12 image format to upload frames through.
    #[error("the driver has no NV12 image format")]
    NoNv12ImageFormat,
    /// The stream does not fit the requested level.
    #[error("{width}x{height} at this frame rate exceeds the limits of H.264 level {level:?}")]
    LevelExceeded {
        /// The requested level.
        level: H264Level,
        /// The frame width.
        width: u32,
        /// The frame height.
        height: u32,
    },
    /// The frame rate does not fit a libva or VUI field.
    #[error("frame rate {}/{} does not fit the VA-API frame rate fields", .0.numerator(), .0.denominator())]
    FrameRateOutOfRange(FrameRate),
    /// A bitrate does not fit libva's 32-bit rate control fields.
    #[error("bitrate {0} bit/s does not fit the VA-API rate control fields")]
    BitrateOutOfRange(u64),
    /// A size does not fit a 32-bit libva field.
    #[error("{0} does not fit a VA-API size field")]
    SizeOutOfRange(&'static str),
    /// A mapped image or coded buffer is smaller than its layout needs.
    #[error("the driver's {0} is smaller than its layout requires")]
    BufferTooSmall(&'static str),
    /// The coded buffer cannot be an H.264 access unit.
    #[error("the driver produced an invalid coded buffer: {0}")]
    CodedBuffer(&'static str),
    /// Only H.264 is implemented.
    #[error("{0} encoding over VA-API is not implemented")]
    UnsupportedCodec(&'static str),
    /// A P picture was planned with no reference picture, a bug.
    #[error("a P picture was planned before any picture was coded")]
    MissingReference,
    /// The NV12 input has the wrong size.
    #[error("NV12 data is {actual} bytes, expected {expected} for {width}x{height}")]
    InputSize {
        /// The bytes received.
        actual: usize,
        /// The bytes a tightly packed frame takes.
        expected: usize,
        /// The configured width.
        width: u32,
        /// The configured height.
        height: u32,
    },
}

impl VaapiError {
    /// Whether the host or configuration, rather than a failing call, is the
    /// reason: nothing is wrong with the driver, it just cannot do this.
    const fn is_unsupported(&self) -> bool {
        matches!(
            self,
            Self::LibraryUnavailable { .. }
                | Self::MissingSymbol { .. }
                | Self::NoRenderNode(_)
                | Self::NoUsableDevice { .. }
                | Self::NoEncodeEntrypoint { .. }
                | Self::UnsupportedRtFormat
                | Self::UnsupportedRateControl(_)
                | Self::UnsupportedPackedHeaders { .. }
                | Self::NoReferenceFrames
                | Self::NoNv12ImageFormat
                | Self::LevelExceeded { .. }
                | Self::FrameRateOutOfRange(_)
                | Self::UnsupportedCodec(_)
        )
    }

    /// Converts the error of encoder construction.
    fn at_open(self) -> CodecError {
        if self.is_unsupported() {
            CodecError::Unsupported(self.to_string())
        } else {
            CodecError::InitializationFailed(self.to_string())
        }
    }

    /// Converts the error of encoding a frame or changing the rate.
    fn at_encode(self) -> CodecError {
        if self.is_unsupported() {
            CodecError::Unsupported(self.to_string())
        } else {
            CodecError::EncodingFailed(self.to_string())
        }
    }
}

/// Opens an encoder on the configured render node, or on the first node
/// that can encode the stream.
#[cfg(mediax_hw_codec_vaapi)]
pub fn open_encoder(config: &crate::EncoderConfig) -> Result<crate::EncoderInner, CodecError> {
    VaapiEncoder::new(config)
        .map(|encoder| crate::EncoderInner::Vaapi(Box::new(encoder)))
        .map_err(VaapiError::at_open)
}
