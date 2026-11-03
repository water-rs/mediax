//! Hardware-accelerated, permissively licensed codecs, containers and
//! streaming for Rust.
//!
//! `mediax` is a facade: applications enable only the layers they need.
//! Containers, network delivery, frame processing and the codecs stay
//! separate crates so tools and libraries do not inherit an unrelated
//! graphics or networking dependency graph.

#![warn(missing_docs)]

pub use mediax_core as core;
pub use mediax_core::{
    ColorPrimaries, ColorRange, CommonEncryptionScheme, ContentLightLevel, EncryptionSubsample,
    Error as VideoError, FrameRate, FrameSize, FrameTiming, MatrixCoefficients, ProtectionInitData,
    SampleEncryption, TrackProtection, TransferFunction, VideoColorInfo,
};

#[cfg(feature = "codec")]
pub use mediax_codec as codec;

#[cfg(feature = "container")]
pub use mediax_container as container;
#[cfg(feature = "container")]
pub use mediax_container::{
    ChromaFormat, ContainerFormat, EmbeddedSubtitleCodec, EmbeddedSubtitleCue,
    EmbeddedSubtitleTrack, MediaFile, MediaFormat, MediaTrack, Muxer, SubtitleCue, TimedMetadata,
    VideoLevel, VideoParameters, VideoReader, active_subtitle_text, decode_cmaf_subtitle_sample,
    embedded_subtitle_tracks, parse_hls_webvtt_segment, parse_subrip_document,
    parse_subtitles_from_path, parse_ttml_document, parse_webvtt_document, probe_mp4_color_info,
    read_embedded_subtitle_cues,
};

#[cfg(feature = "streaming")]
pub use mediax_streaming as streaming;

#[cfg(feature = "processing")]
pub use mediax_processing as processing;
