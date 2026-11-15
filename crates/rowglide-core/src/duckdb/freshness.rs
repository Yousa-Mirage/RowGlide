//! Lightweight file fingerprints; no hashing, watchers or retained file handles.

use std::{fs, io, path::Path, time::SystemTime};

/// Portable size/time fingerprint, with file identity on Unix to detect replacements.
/// This cannot detect edits that preserve all observed metadata; it is not snapshot isolation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileStamp {
    /// Logical file length in bytes.
    size: u64,
    /// Last modification timestamp, at the filesystem's available precision.
    modified: SystemTime,
    /// Device and inode identify atomic replacements even when size/time are preserved.
    #[cfg(unix)]
    identity: (u64, u64),
}

impl FileStamp {
    /// Read one regular file's metadata, propagating missing files and unsupported timestamps.
    pub(super) fn read(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(io::Error::other("Source is no longer a regular file"));
        }
        Ok(Self {
            size: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            },
        })
    }
}
