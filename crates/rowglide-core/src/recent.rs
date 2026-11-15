//! Small persistent MRU history, independent of the query backend and GUI.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

/// Maximum serialized history size, including long Windows paths.
const MAX_HISTORY_BYTES: u64 = 1024 * 1024;

/// Failure reading or persisting optional recent-file preferences.
#[derive(Debug, thiserror::Error)]
pub enum RecentError {
    /// User data directory is unavailable on this platform or account.
    #[error("Cannot locate the user data directory")]
    DirectoryUnavailable,
    /// File access, atomic replacement or synchronization failed.
    #[error("Recent files: {0}")]
    Io(#[from] io::Error),
    /// Stored paths are malformed or cannot be represented in JSON.
    #[error("Invalid recent-file history: {0}")]
    Json(#[from] serde_json::Error),
}

/// At most five canonical paths, newest first, shared by background opening tasks.
/// Only paths are persisted; no schemas, results or file content are retained.
/// A mutex serializes this instance's loads and writes. Separate application instances
/// use atomic replacement with last-writer-wins semantics, never a shared append log.
pub struct RecentFiles {
    /// Destination under the platform's local user data directory.
    path: PathBuf,
    /// Lazily loaded history; failed reads leave an empty, usable in-memory list.
    entries: Mutex<Option<Vec<PathBuf>>>,
}

impl RecentFiles {
    /// Resolve the platform data location without accessing the filesystem.
    pub fn for_user() -> Result<Self, RecentError> {
        let root = dirs::data_local_dir().ok_or(RecentError::DirectoryUnavailable)?;
        Ok(Self::at(root.join("rowglide").join("recent-files.json")))
    }

    /// Construct a lazy store at an explicit path, useful for isolated preferences and tests.
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            entries: Mutex::new(None),
        }
    }

    /// Lock cached entries, preserving synchronization failure as a typed I/O error.
    fn lock(&self) -> Result<MutexGuard<'_, Option<Vec<PathBuf>>>, RecentError> {
        Ok(self
            .entries
            .lock()
            .map_err(|_| io::Error::other("Recent-file history lock poisoned"))?)
    }

    /// Return a snapshot, reading at most 1 MiB once; call off the GUI thread.
    /// Missing history is normal. Corrupt/unreadable history reports an error once and
    /// leaves an empty cache, allowing subsequent successful opens to repair it.
    pub fn entries(&self) -> Result<Vec<PathBuf>, RecentError> {
        let mut entries = self.lock()?;
        if entries.is_none() {
            *entries = Some(Vec::new());
            let file = match fs::File::open(&self.path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error.into()),
            };
            let mut bytes = Vec::new();
            file.take(MAX_HISTORY_BYTES + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_HISTORY_BYTES {
                return Err(io::Error::other("Recent-file history exceeds 1 MiB").into());
            }
            let paths: Vec<PathBuf> = serde_json::from_slice(&bytes)?;
            let mut unique = Vec::new();
            for path in paths {
                if path.is_absolute() && !unique.contains(&path) {
                    unique.push(path);
                    if unique.len() == 5 {
                        break;
                    }
                }
            }
            *entries = Some(unique);
        }
        Ok(entries.clone().unwrap_or_default())
    }

    /// Record a successfully opened file and atomically persist the newest five paths.
    /// Canonicalization deduplicates relative paths and symlinks. Call off the GUI thread.
    /// History-load errors are logged here and repaired; save errors propagate while
    /// preserving the updated in-memory list. No source file is modified.
    pub fn record(&self, path: &Path) -> Result<(), RecentError> {
        let path = path.canonicalize()?;
        if let Err(error) = self.entries() {
            eprintln!("{error}");
        }
        let mut guard = self.lock()?;
        let entries = guard.get_or_insert_default();
        entries.retain(|entry| entry != &path);
        entries.insert(0, path);
        entries.truncate(5);
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("History has no parent directory"))?;
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec(entries)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Successful opens persist only five unique files and move reopened entries to the front.
    #[test]
    fn bounded_history_survives_restart() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let destination = directory.path().join("settings/recent.json");
        let history = RecentFiles::at(destination.clone());
        assert!(history.entries()?.is_empty());
        for index in 0..7 {
            let path = directory.path().join(format!("{index}.parquet"));
            fs::write(&path, "fixture")?;
            history.record(&path)?;
        }
        history.record(&directory.path().join("3.parquet"))?;
        let names = history
            .entries()?
            .iter()
            .map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "3.parquet",
                "6.parquet",
                "5.parquet",
                "4.parquet",
                "2.parquet"
            ]
            .map(|name| Some(name.to_owned()))
        );
        assert_eq!(history.entries()?, RecentFiles::at(destination).entries()?);
        Ok(())
    }

    /// Invalid history is recoverable; missing files cannot be recorded and do not erase entries.
    #[test]
    fn invalid_history_and_missing_files() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let destination = directory.path().join("recent.json");
        fs::write(&destination, "broken json")?;
        let history = RecentFiles::at(destination.clone());
        assert!(history.entries().is_err());
        let path = directory.path().join("有效.parquet");
        fs::write(&path, "fixture")?;
        history.record(&path)?;
        assert!(
            history
                .record(&directory.path().join("missing.parquet"))
                .is_err()
        );
        assert_eq!(
            RecentFiles::at(destination).entries()?,
            vec![path.canonicalize()?]
        );
        Ok(())
    }
}
