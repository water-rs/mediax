//! Output files: the container an extension names, and cleanup on failure.

use std::path::Path;

use mediax_container::ContainerFormat;

use crate::error::CliError;

/// The container `path`'s extension names.
///
/// # Errors
///
/// Returns [`CliError::UnknownOutputExtension`] for any extension but `mp4`
/// and `mov`, compared case-insensitively.
pub fn container_format(path: &Path) -> Result<ContainerFormat, CliError> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("mp4") => Ok(ContainerFormat::Mp4),
        Some("mov") => Ok(ContainerFormat::Mov),
        _ => Err(CliError::UnknownOutputExtension(path.to_path_buf())),
    }
}

/// Runs `write`, which creates `path`, and removes the unfinished file when
/// it fails.
///
/// # Errors
///
/// Returns whatever `write` returns.
pub fn remove_on_failure<T>(
    path: &Path,
    write: impl FnOnce() -> Result<T, CliError>,
) -> Result<T, CliError> {
    let result = write();
    if result.is_err()
        && let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "could not remove the unfinished output");
    }
    result
}
