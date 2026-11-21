use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::encoding::EncodingSpec;
use crate::ids::{DocRevision, DocumentId, SourceSnapshotId};

/// Strong identity of a file as reported by the filesystem.
///
/// Two paths that resolve to the same identity refer to the same file. A
/// string comparison of paths is never sufficient on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileIdentity {
    pub volume_serial: u64,
    pub file_id_128: [u8; 16],
}

impl FileIdentity {
    pub const fn new(volume_serial: u64, file_id_128: [u8; 16]) -> Self {
        Self {
            volume_serial,
            file_id_128,
        }
    }

    pub fn to_hex(self) -> String {
        let mut out = format!("{:016x}", self.volume_serial);
        for byte in self.file_id_128 {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}

impl std::fmt::Display for FileIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Point-in-time description of the bytes on disk.
///
/// `mtime + size` is a cheap change hint, not proof of content identity.
/// A strong comparison includes [`FileIdentity`] and, when required, a content
/// hash.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    pub identity: Option<FileIdentity>,
    pub byte_len: u64,
    pub last_write_time: u64,
    pub content_blake3: Option<[u8; 32]>,
}

impl FileStamp {
    pub fn new(
        identity: Option<FileIdentity>,
        byte_len: u64,
        last_write_time: u64,
        content_blake3: Option<[u8; 32]>,
    ) -> Self {
        Self {
            identity,
            byte_len,
            last_write_time,
            content_blake3,
        }
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.identity.is_some() && self.identity == other.identity
    }

    /// Compare the fields that are certainly available. A missing content hash
    /// on either side makes the result conservative: a changed length or mtime
    /// is reported as a change.
    pub fn matches(&self, other: &Self) -> bool {
        if self.identity != other.identity || self.byte_len != other.byte_len {
            return false;
        }
        if self.last_write_time != other.last_write_time {
            return false;
        }
        match (self.content_blake3, other.content_blake3) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        }
    }
}

/// A path in its native representation.
///
/// Protocol layers must not convert a path to a lossy string before reopening
/// it. Display code may use [`NativePath::display_lossy`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NativePath {
    Windows(Vec<u16>),
    Unix(Vec<u8>),
}

impl NativePath {
    pub fn from_path(path: &Path) -> Self {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self::Windows(path.as_os_str().encode_wide().collect())
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::ffi::OsStrExt;
            Self::Unix(path.as_os_str().as_bytes().to_vec())
        }
    }

    pub fn to_path_buf(&self) -> PathBuf {
        match self {
            #[cfg(windows)]
            Self::Windows(units) => {
                use std::os::windows::ffi::OsStringExt;
                PathBuf::from(std::ffi::OsString::from_wide(units))
            }
            #[cfg(not(windows))]
            Self::Windows(units) => PathBuf::from(String::from_utf16_lossy(units)),
            Self::Unix(bytes) => {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStringExt;
                    PathBuf::from(std::ffi::OsString::from_vec(bytes.clone()))
                }
                #[cfg(not(unix))]
                {
                    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
                }
            }
        }
    }

    /// Display-only rendering. Never feed the result back to the filesystem.
    pub fn display_lossy(&self) -> String {
        self.to_path_buf().to_string_lossy().into_owned()
    }

    pub fn file_name(&self) -> Option<String> {
        self.to_path_buf()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }
}

impl From<&Path> for NativePath {
    fn from(path: &Path) -> Self {
        Self::from_path(path)
    }
}

impl From<PathBuf> for NativePath {
    fn from(path: PathBuf) -> Self {
        Self::from_path(&path)
    }
}

impl std::fmt::Display for NativePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display_lossy())
    }
}

/// The disk state a document compares against to decide dirty/clean.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveBaseline {
    pub text_fingerprint: [u8; 32],
    pub serialization: EncodingSpec,
    pub disk_stamp: FileStamp,
    pub source_snapshot: SourceSnapshotId,
}

/// Result of one committed save.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveOutcome {
    pub document_id: DocumentId,
    pub saved_revision: DocRevision,
    pub serialization: EncodingSpec,
    pub stamp: FileStamp,
    pub backup_path: Option<NativePath>,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_paths_round_trip_through_json() {
        let path = if cfg!(windows) {
            PathBuf::from(r"C:\中文 目录\notes.txt")
        } else {
            PathBuf::from("/tmp/中文 目录/notes.txt")
        };
        let native = NativePath::from(path.clone());
        let json = serde_json::to_string(&native).unwrap();
        let back: NativePath = serde_json::from_str(&json).unwrap();
        assert_eq!(native, back);
        assert_eq!(back.to_path_buf(), path);
    }

    #[test]
    fn stamp_comparison_is_conservative() {
        let id = FileIdentity::new(1, [2; 16]);
        let a = FileStamp::new(Some(id), 10, 100, None);
        let b = FileStamp::new(Some(id), 10, 100, None);
        assert!(a.matches(&b));

        let c = FileStamp::new(Some(id), 11, 100, None);
        assert!(!a.matches(&c));
    }
}
