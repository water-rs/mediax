//! Linux hardware H.264 encoding and H.264 and H.265 decoding through VA-API.
//!
//! libva is loaded with `dlopen` when a codec is created, so a host without
//! it builds and runs, and the constructor reports why there is no codec. The
//! module also compiles in tests on other hosts, where the codecs run against
//! a test double of the driver.

mod api;
mod bits;
mod decoder;
#[cfg(test)]
mod decoder_tests;
mod device;
#[cfg(any(test, feature = "fuzzing"))]
pub mod double;
#[cfg(any(test, mediax_hw_codec_vaapi))]
mod encoder;
mod headers;
mod hevc;
#[cfg(test)]
mod hevc_tests;
mod plan;
mod target;
#[cfg(test)]
mod tests;
mod types;

pub use decoder::H264Decoder;
#[cfg(mediax_hw_codec_vaapi)]
pub use encoder::VaapiEncoder;
pub use hevc::HevcDecoder;
pub use target::DecodedPicture;

#[cfg(mediax_hw_codec_vaapi)]
use crate::CodecType;
use crate::{CodecError, FrameRate, H264Level, H264Profile, h264::H264Error, h265::H265Error};
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
    #[error("the driver's H.264 codec does not support the YUV 4:2:0 render target format")]
    UnsupportedRtFormat,
    /// The driver offers no decode entry point for the stream's codec and
    /// profile.
    #[error("the driver has no decode entry point (VAEntrypointVLD) for the stream's profile")]
    NoDecodeEntrypoint,
    /// Every surface holds a frame, which a conforming stream cannot cause.
    #[error("every decoding surface holds a frame")]
    NoFreeSurface,
    /// The picture buffer output a frame that was never decoded, a bug.
    #[error("output of a frame that was never decoded")]
    NeverDecoded,
    /// A sample cannot be split into NAL units.
    #[error("malformed sample framing: {0}")]
    Framing(String),
    /// The H.264 stream is malformed or uses a feature the decoder lacks.
    #[error(transparent)]
    Stream(#[from] H264Error),
    /// The H.265 stream is malformed or uses a feature the decoder lacks.
    #[error(transparent)]
    Hevc(#[from] H265Error),
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
    /// The driver has no image format to move frames through.
    #[error("the driver has no {0} image format")]
    NoImageFormat(&'static str),
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
    /// The codec is not implemented in this direction.
    #[error("{0} over VA-API is not implemented")]
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
                | Self::NoDecodeEntrypoint
                | Self::UnsupportedRtFormat
                | Self::UnsupportedRateControl(_)
                | Self::UnsupportedPackedHeaders { .. }
                | Self::NoReferenceFrames
                | Self::NoImageFormat(_)
                | Self::LevelExceeded { .. }
                | Self::FrameRateOutOfRange(_)
                | Self::UnsupportedCodec(_)
        ) || matches!(self, Self::Stream(error) if error.is_unsupported())
            || matches!(self, Self::Hevc(error) if error.is_unsupported())
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

    /// Converts the error of decoding a packet.
    #[cfg(any(test, mediax_hw_codec_vaapi))]
    pub(crate) fn at_decode(self) -> CodecError {
        if self.is_unsupported() {
            CodecError::Unsupported(self.to_string())
        } else {
            CodecError::DecodingFailed(self.to_string())
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

/// What the H.264 and H.265 decoders share: access units in, frames out in
/// output order.
pub trait VaapiDecoder {
    /// Decodes one access unit and returns the frames the picture buffer
    /// outputs because of it, in output order.
    ///
    /// # Errors
    ///
    /// Returns a [`VaapiError`] when the stream is malformed or uses an
    /// unsupported feature, or the driver fails.
    fn decode(
        &mut self,
        sample: &[u8],
        timestamp_ns: u64,
    ) -> Result<Vec<DecodedPicture>, VaapiError>;

    /// Outputs every frame still waiting in the picture buffer.
    ///
    /// # Errors
    ///
    /// Returns a [`VaapiError`] when a picture cannot be completed or read.
    fn drain(&mut self) -> Result<Vec<DecodedPicture>, VaapiError>;
}

impl VaapiDecoder for H264Decoder {
    fn decode(
        &mut self,
        sample: &[u8],
        timestamp_ns: u64,
    ) -> Result<Vec<DecodedPicture>, VaapiError> {
        Self::decode(self, sample, timestamp_ns)
    }

    fn drain(&mut self) -> Result<Vec<DecodedPicture>, VaapiError> {
        Self::drain(self)
    }
}

impl VaapiDecoder for HevcDecoder {
    fn decode(
        &mut self,
        sample: &[u8],
        timestamp_ns: u64,
    ) -> Result<Vec<DecodedPicture>, VaapiError> {
        Self::decode(self, sample, timestamp_ns)
    }

    fn drain(&mut self) -> Result<Vec<DecodedPicture>, VaapiError> {
        Self::drain(self)
    }
}

/// Opens a decoder of `codec` on the first render node that can decode it.
///
/// `config` is the `avcC` or `hvcC` record of length-prefixed samples.
#[cfg(mediax_hw_codec_vaapi)]
pub fn open_decoder(
    codec: CodecType,
    config: Option<&[u8]>,
) -> Result<crate::DecoderInner, CodecError> {
    match codec {
        CodecType::H264 => H264Decoder::new(config)
            .map(|decoder| crate::DecoderInner::VaapiH264(Box::new(decoder))),
        CodecType::H265 => HevcDecoder::new(config)
            .map(|decoder| crate::DecoderInner::VaapiH265(Box::new(decoder))),
        CodecType::Av1 => Err(VaapiError::UnsupportedCodec("AV1 decoding")),
    }
    .map_err(VaapiError::at_open)
}
