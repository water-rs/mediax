//! H.265 (HEVC) stream handling for hardware decoders: parameter sets, slice
//! headers and the decoded picture buffer, for progressive 4:2:0 Main and
//! Main 10 streams.

pub mod dpb;
pub mod error;
pub mod nal;
pub mod params;
pub mod sequence;
pub mod slice;
pub mod syntax;
#[cfg(any(test, feature = "fuzzing"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use error::H265Error;
