//! Track descriptions as the file signals them.

use std::path::Path;

use mediax_container::{MediaFile, MediaFormat, TrackInfo, TrackKind, probe_mp4_color_info};

use crate::error::CliError;

/// The tracks of `file`, opened from `path`, with each video track's colour
/// description read from the file.
///
/// A demuxed track carries only the conventional default colour for its
/// height; an ISO BMFF file signals the real one, which a copy must keep. A
/// transport stream signals none, so its default stays.
///
/// # Errors
///
/// Returns an error when the file's colour boxes cannot be read.
pub fn tracks_with_signaled_color(
    path: &Path,
    file: &MediaFile,
) -> Result<Vec<TrackInfo>, CliError> {
    file.tracks()
        .iter()
        .map(|track| {
            let info = track.info();
            let height = info.video_dimensions().map(|size| size.height.get());
            match (file.format(), info.kind()) {
                (
                    MediaFormat::Mp4 | MediaFormat::Mov | MediaFormat::FragmentedMp4,
                    TrackKind::Video,
                ) => Ok(info
                    .clone()
                    .with_video_color_info(probe_mp4_color_info(path, height)?)),
                _ => Ok(info.clone()),
            }
        })
        .collect()
}
