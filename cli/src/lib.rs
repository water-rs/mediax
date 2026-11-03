//! The `mediax` command line: probe, transcode and remux media files.
//!
//! Everything media-specific comes from `mediax-container` and
//! `mediax-codec`; this crate parses arguments, wires the two together and
//! reports.

pub mod args;
pub mod error;
pub mod output;
pub mod probe;
pub mod remux;
pub mod samples;
pub mod tracks;
pub mod transcode;
