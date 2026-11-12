//! Free space and filesystem identity, which `std` doesn't report.

use std::path::Path;

/// The bytes a non-privileged writer can still use on the filesystem holding `path`.
#[cfg(unix)]
pub(crate) fn free_bytes(path: &Path) -> std::io::Result<u64> {
    let s = rustix::fs::statvfs(path)?;
    Ok(s.f_bavail.saturating_mul(s.f_frsize))
}

/// Not measured on Windows: the bulk fetchers that need it run on the project's Linux nodes.
#[cfg(not(unix))]
pub(crate) fn free_bytes(_path: &Path) -> std::io::Result<u64> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "free space isn't measured on this platform",
    ))
}

/// A value that is equal for two paths on one filesystem, to check before a hard link.
#[cfg(unix)]
pub(crate) fn filesystem_id(path: &Path) -> std::io::Result<String> {
    use std::os::unix::fs::MetadataExt as _;
    Ok(std::fs::metadata(path)?.dev().to_string())
}

/// On Windows a hard link needs one volume: the drive or share is the best `std` reports. The
/// path must exist, as on Unix.
#[cfg(not(unix))]
pub(crate) fn filesystem_id(path: &Path) -> std::io::Result<String> {
    use std::path::Component;
    let full = std::fs::canonicalize(path)?;
    Ok(match full.components().next() {
        Some(Component::Prefix(prefix)) => {
            prefix.as_os_str().to_string_lossy().to_ascii_lowercase()
        }
        _ => String::new(),
    })
}
