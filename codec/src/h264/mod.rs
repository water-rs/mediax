//! Parsing and picture management for progressive H.264 streams.
//!
//! The syntax is read by `h264-reader`; this module reduces it to what a
//! hardware decoder needs and implements the decoding process around the
//! hardware: picture order, the decoded picture buffer, reference marking,
//! reference lists and output order. Nothing here depends on the decoder
//! API that does the sample reconstruction.

pub mod dpb;
pub mod error;
pub mod sequence;
pub mod slice;
#[cfg(any(test, feature = "fuzzing"))]
pub mod testing;

pub use error::H264Error;
