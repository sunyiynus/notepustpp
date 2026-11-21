use std::path::Path;

use notepust_contracts::FileStamp;
use notepust_platform::snapshot_of_path;

use crate::open::hash_file;

/// Classification of the disk state relative to an expected stamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalChange {
    Unchanged,
    /// Same file identity, but length, mtime or content differ.
    Modified,
    /// The path now resolves to a different file.
    Replaced,
    Removed,
}

/// Cheap change check using identity and metadata only.
///
/// Use [`detect_external_change_deep`] when a same-size rewrite must be ruled
/// out (window activation, before a write, after a watcher overflow).
pub fn detect_external_change(
    path: &Path,
    expected: &FileStamp,
) -> std::io::Result<ExternalChange> {
    match snapshot_of_path(path, None) {
        Ok(snapshot) => {
            if let (Some(expected_id), Some(current_id)) =
                (expected.identity, snapshot.stamp.identity)
                && expected_id != current_id
            {
                return Ok(ExternalChange::Replaced);
            }
            if expected.byte_len == snapshot.stamp.byte_len
                && expected.last_write_time == snapshot.stamp.last_write_time
            {
                Ok(ExternalChange::Unchanged)
            } else {
                Ok(ExternalChange::Modified)
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ExternalChange::Removed),
        Err(error) => Err(error),
    }
}

/// Change check that reads and hashes the current bytes. Never call this on a
/// keystroke or timer path for large files.
pub fn detect_external_change_deep(
    path: &Path,
    expected: &FileStamp,
) -> std::io::Result<ExternalChange> {
    let change = detect_external_change(path, expected)?;
    if change != ExternalChange::Modified && change != ExternalChange::Unchanged {
        return Ok(change);
    }
    let current_hash = hash_file(path)?;
    match expected.content_blake3 {
        Some(expected_hash) if expected_hash == current_hash => Ok(ExternalChange::Unchanged),
        Some(_) => Ok(ExternalChange::Modified),
        None => Ok(change),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notepust_platform::read_file_stamp;

    #[test]
    fn detects_modified_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watched.txt");
        std::fs::write(&path, b"one").unwrap();
        let stamp = read_file_stamp(&path).unwrap().stamp;
        assert_eq!(
            detect_external_change(&path, &stamp).unwrap(),
            ExternalChange::Unchanged
        );

        // Keep the test independent of filesystem timestamp granularity.
        std::fs::write(&path, b"two-two").unwrap();
        assert_eq!(
            detect_external_change(&path, &stamp).unwrap(),
            ExternalChange::Modified
        );

        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            detect_external_change(&path, &stamp).unwrap(),
            ExternalChange::Removed
        );
    }
}
