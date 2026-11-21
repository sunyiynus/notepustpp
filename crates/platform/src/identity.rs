use std::fs::Metadata;
use std::path::Path;

use notepust_contracts::{FileIdentity, FileStamp};

/// Identity and stamp captured together from one metadata read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSnapshot {
    pub stamp: FileStamp,
    /// True when the filesystem exposed a usable volume/file id pair.
    pub identity_available: bool,
}

/// Reads the current stamp for a path without hashing content.
///
/// Callers that need to prove content equality must add a BLAKE3 hash of the
/// bytes they actually read; `mtime + size` alone is only a change hint.
pub fn read_file_stamp(path: &Path) -> std::io::Result<FileSnapshot> {
    let metadata = std::fs::metadata(path)?;
    let identity = identity_of_path(path)?;
    Ok(snapshot_from_metadata(&metadata, identity, None))
}

pub fn snapshot_of_path(
    path: &Path,
    content_hash: Option<[u8; 32]>,
) -> std::io::Result<FileSnapshot> {
    let metadata = std::fs::metadata(path)?;
    let identity = identity_of_path(path)?;
    Ok(snapshot_from_metadata(&metadata, identity, content_hash))
}

/// Identity of the file currently named by `path`.
///
/// On Windows this opens a `FILE_FLAG_OPEN_REPARSE_POINT` handle and asks for
/// `FILE_ID_INFO` (volume serial + 128-bit file id). This is a real filesystem
/// identity, unlike a lowercase path comparison. Filesystems that cannot
/// provide it yield `None` and callers must label identity as unavailable.
pub fn identity_of_path(path: &Path) -> std::io::Result<Option<FileIdentity>> {
    #[cfg(windows)]
    {
        win::file_identity(path)
    }
    #[cfg(not(windows))]
    {
        let metadata = std::fs::metadata(path)?;
        Ok(identity_from_metadata(&metadata))
    }
}

fn snapshot_from_metadata(
    metadata: &Metadata,
    identity: Option<FileIdentity>,
    content_hash: Option<[u8; 32]>,
) -> FileSnapshot {
    let byte_len = metadata.len();
    let last_write_time = modified_time(metadata);
    FileSnapshot {
        stamp: FileStamp::new(identity, byte_len, last_write_time, content_hash),
        identity_available: identity.is_some(),
    }
}

#[cfg(unix)]
fn identity_from_metadata(metadata: &Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;

    let mut file_id = [0u8; 16];
    file_id[..8].copy_from_slice(&metadata.ino().to_le_bytes());
    Some(FileIdentity::new(metadata.dev(), file_id))
}

#[cfg(not(any(windows, unix)))]
fn identity_from_metadata(_metadata: &Metadata) -> Option<FileIdentity> {
    None
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use notepust_contracts::FileIdentity;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_FLAGS_AND_ATTRIBUTES, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo, GetFileInformationByHandleEx, OPEN_EXISTING,
    };
    use windows::core::PCWSTR;

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            if self.0 != INVALID_HANDLE_VALUE && !self.0.is_invalid() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    pub(super) fn file_identity(path: &Path) -> std::io::Result<Option<FileIdentity>> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(
                    FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0,
                ),
                None,
            )
        }
        .map_err(windows_to_io)?;
        let handle = Handle(handle);

        let mut info = FILE_ID_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                handle.0,
                FileIdInfo,
                &mut info as *mut FILE_ID_INFO as *mut c_void,
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        }
        .map_err(windows_to_io)?;

        Ok(Some(FileIdentity::new(
            info.VolumeSerialNumber,
            info.FileId.Identifier,
        )))
    }

    fn windows_to_io(error: windows::core::Error) -> std::io::Error {
        let code = error.code().0 as u32;
        let kind = match code & 0xFFFF {
            2 | 3 => std::io::ErrorKind::NotFound,
            5 => std::io::ErrorKind::PermissionDenied,
            32 | 33 => std::io::ErrorKind::WouldBlock,
            _ => std::io::ErrorKind::Other,
        };
        std::io::Error::new(kind, error.message())
    }
}

#[cfg(windows)]
fn modified_time(metadata: &Metadata) -> u64 {
    use std::os::windows::fs::MetadataExt;
    metadata.last_write_time()
}

#[cfg(unix)]
fn modified_time(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let seconds = metadata.mtime().max(0) as u64;
    let nanos = metadata.mtime_nsec().max(0) as u64;
    seconds.saturating_mul(1_000_000_000).saturating_add(nanos)
}

#[cfg(not(any(windows, unix)))]
fn modified_time(metadata: &Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn identity_is_stable_for_one_file() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "hello").unwrap();
        let first = identity_of_path(file.path()).unwrap();
        let second = identity_of_path(file.path()).unwrap();
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    #[test]
    fn stamp_tracks_length_and_time() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"abc").unwrap();
        file.flush().unwrap();
        let before = read_file_stamp(file.path()).unwrap();
        assert_eq!(before.stamp.byte_len, 3);

        file.write_all(b"def").unwrap();
        file.flush().unwrap();
        let after = read_file_stamp(file.path()).unwrap();
        assert_eq!(after.stamp.byte_len, 6);
        assert!(!before.stamp.matches(&after.stamp));
    }
}
