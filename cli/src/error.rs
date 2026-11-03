//! The CLI's error type.

use std::path::PathBuf;

use mediax_codec::{CodecType, DecodedPixelLayout};

/// Everything a `mediax` subcommand can fail with.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Reading or writing a container failed.
    #[error(transparent)]
    Container(#[from] mediax_container::Error),

    /// A codec failed.
    #[error(transparent)]
    Codec(#[from] mediax_codec::CodecError),

    /// An I/O operation on a file or on standard output failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// The probe report could not be serialized.
    #[error("cannot serialize the probe report: {0}")]
    Json(#[from] serde_json::Error),

    /// The probe report could not be rendered.
    #[error("cannot render the probe report: {0}")]
    Render(#[from] askama::Error),

    /// The output file name names no container `mediax` writes.
    #[error("{}: cannot choose a container from the extension; use .mp4 or .mov", .0.display())]
    UnknownOutputExtension(PathBuf),

    /// The container is one this CLI does not know how to handle.
    #[error("unsupported container {0:?}")]
    UnsupportedFormat(mediax_container::MediaFormat),

    /// The source has no video track to transcode.
    #[error("{}: the file has no video track", .0.display())]
    NoVideoTrack(PathBuf),

    /// The source has several video tracks, and transcoding handles one.
    #[error("{}: the file has {count} video tracks; transcoding takes exactly one", .path.display())]
    SeveralVideoTracks {
        /// The source file.
        path: PathBuf,
        /// How many video tracks it has.
        count: usize,
    },

    /// The source video codec is not one the decoder reads.
    #[error("cannot decode {0:?} video")]
    UndecodableSource(mediax_container::Codec),

    /// The source declares no constant frame rate, which the encoder needs.
    #[error("the video track declares no constant frame rate, which the encoder needs")]
    NoFrameRate,

    /// This host has no encoder for the requested codec.
    #[error("this host cannot encode {codec:?}: {reason}")]
    EncoderUnavailable {
        /// The requested codec.
        codec: CodecType,
        /// Why the codec library refused it.
        reason: String,
    },

    /// This host has no decoder for the source codec.
    #[error("this host cannot decode {codec:?}: {reason}")]
    DecoderUnavailable {
        /// The source codec.
        codec: CodecType,
        /// Why the codec library refused it.
        reason: String,
    },

    /// The decoder emitted a pixel layout the encoder cannot take.
    #[error(
        "the decoder produced {layout:?} frames, but the encoder takes only NV12; \
         converting between pixel layouts is not supported"
    )]
    UnsupportedPixelLayout {
        /// The layout the decoder produced.
        layout: DecodedPixelLayout,
    },

    /// The decoder emitted a frame whose size differs from the track's.
    #[error("the decoder produced a {actual} frame in a {expected} track")]
    FrameSizeChanged {
        /// The track's size, as `WxH`.
        expected: String,
        /// The frame's size, as `WxH`.
        actual: String,
    },

    /// The decoder produced no frame at all.
    #[error("the video track decoded to no frames")]
    NoFrames,

    /// The encoder reported no codec configuration after its first packet.
    #[error("the encoder reported no codec configuration")]
    NoCodecConfiguration,

    /// A command-line value is out of range.
    #[error("{0}")]
    InvalidArgument(String),
}
