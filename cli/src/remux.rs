//! `mediax remux`: copy every track into another container.

use std::path::Path;

use mediax_container::{MediaFile, Muxer};

use crate::error::CliError;
use crate::output::{container_format, remove_on_failure};
use crate::samples::Samples;
use crate::tracks::tracks_with_signaled_color;

/// Copies every track of `input` sample for sample into `output`, whose
/// extension picks the container. Timestamps are kept.
///
/// # Errors
///
/// Returns an error when the extension names no container, `input` cannot be
/// read, or the muxer rejects a track or sample (a codec it does not write,
/// for one). The unfinished `output` is removed.
pub fn remux(input: &Path, output: &Path) -> Result<(), CliError> {
    let format = container_format(output)?;
    let file = MediaFile::open(input)?;
    let tracks = tracks_with_signaled_color(input, &file)?;
    let samples = Samples::open(input, &file)?;
    remove_on_failure(output, || {
        let mut muxer = Muxer::create(output, format)?;
        for track in tracks {
            muxer.add_track(track)?;
        }
        let mut count = 0_u64;
        for sample in samples {
            muxer.write_sample(&sample?)?;
            count += 1;
        }
        muxer.finish()?;
        tracing::info!(samples = count, ?format, output = %output.display(), "remuxed");
        Ok(())
    })
}
