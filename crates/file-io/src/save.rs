use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use notepust_contracts::{
    DocRevision, DocumentId, EncodingSpec, FileStamp, NativePath, SaveOutcome,
};
use notepust_platform::snapshot_of_path;
use notepust_text_codec::encode_text;

use crate::error::SaveError;
use crate::open::hash_file;

/// What should be written.
#[derive(Clone, Copy, Debug)]
pub enum SaveContent<'a> {
    /// Byte-faithful output: retained source bytes, or a prepared artifact.
    Bytes(&'a [u8]),
    /// Re-encode this authoritative text with `SaveRequest::encoding`.
    Text(&'a str),
}

/// Optional versioned backup of the pre-save file.
#[derive(Clone, Debug)]
pub struct BackupPlan {
    pub directory: PathBuf,
    pub file_stem: String,
}

#[derive(Clone, Debug)]
pub struct SaveRequest<'a> {
    pub document_id: DocumentId,
    pub revision: DocRevision,
    pub target: &'a Path,
    pub encoding: &'a EncodingSpec,
    /// `None` means the target must not already exist.
    pub expected: Option<&'a FileStamp>,
    pub content: SaveContent<'a>,
    pub backup: Option<BackupPlan>,
}

#[derive(Clone, Debug)]
pub struct SaveResult {
    pub outcome: SaveOutcome,
    pub bytes_written: u64,
    pub output_digest: [u8; 32],
    /// The exact bytes written, retained as the new byte-faithful source
    /// baseline for future save-as operations.
    pub bytes: Vec<u8>,
}

/// Performs a reliable save.
///
/// The flow is: verify the observed disk version, optionally copy a backup,
/// write and fsync a same-directory temp file, then commit with
/// `ReplaceFileW` for existing files (or a no-clobber move for new files).
/// On any failure the original target is left untouched.
pub fn save_file(request: &SaveRequest<'_>) -> Result<SaveResult, SaveError> {
    request
        .encoding
        .validate()
        .map_err(|detail| SaveError::Unsupported {
            detail: format!("invalid encoding metadata: {detail}"),
        })?;

    let bytes: Vec<u8> = match request.content {
        SaveContent::Bytes(bytes) => bytes.to_vec(),
        SaveContent::Text(text) => encode_text(text, request.encoding)?,
    };
    let output_digest = *blake3::hash(&bytes).as_bytes();

    let target = request.target;
    let parent = target.parent().ok_or_else(|| SaveError::Unsupported {
        detail: "target has no parent directory".to_string(),
    })?;
    if !parent.exists() {
        return Err(SaveError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("parent directory {} does not exist", parent.display()),
        )));
    }

    let target_exists = target.exists();
    match (request.expected, target_exists) {
        (Some(_), false) => {
            return Err(SaveError::conflict(
                "the target disappeared since it was opened",
            ));
        }
        (None, true) => {
            return Err(SaveError::conflict(
                "a file already exists at the save-as target",
            ));
        }
        _ => {}
    }

    if let Some(expected) = request.expected {
        verify_expected(target, expected)?;
    }

    let backup_path = if let Some(plan) = &request.backup {
        if target_exists {
            Some(make_backup(target, plan)?)
        } else {
            None
        }
    } else {
        None
    };

    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(&bytes)?;
    temp.flush()?;
    temp.as_file().sync_all()?;

    if target_exists {
        let rollback = rollback_path(parent);
        let (_, temp_path) = temp.keep().map_err(|error| error.error)?;
        if let Err(error) = commit_replace(target, &temp_path, &rollback) {
            let preserved = temp_path.display().to_string();
            return Err(match error {
                SaveError::NeedsReconciliation { detail } => SaveError::NeedsReconciliation {
                    detail: format!("{detail}; prepared temp file kept at {preserved}"),
                },
                other => other.with_temp(Some(temp_path)),
            });
        }
        let mut warnings = Vec::new();
        if rollback.exists()
            && let Err(error) = std::fs::remove_file(&rollback)
        {
            warnings.push(format!(
                "could not remove rollback copy {}: {error}",
                rollback.display()
            ));
        }
        finish(request, target, bytes, output_digest, backup_path, warnings)
    } else {
        match temp.persist_noclobber(target) {
            Ok(_) => finish(
                request,
                target,
                bytes,
                output_digest,
                backup_path,
                Vec::new(),
            ),
            Err(error) => {
                let temp_path = error.file.path().to_path_buf();
                let _ = error.file.keep();
                Err(SaveError::Conflict {
                    detail: format!(
                        "the save-as target appeared before commit; prepared temp file kept at {}",
                        temp_path.display()
                    ),
                    preserved_temp: Some(temp_path),
                })
            }
        }
    }
}

fn finish(
    request: &SaveRequest<'_>,
    target: &Path,
    bytes: Vec<u8>,
    digest: [u8; 32],
    backup_path: Option<PathBuf>,
    warnings: Vec<String>,
) -> Result<SaveResult, SaveError> {
    let snapshot = snapshot_of_path(target, Some(digest))?;
    if snapshot.stamp.byte_len != bytes.len() as u64 {
        return Err(SaveError::NeedsReconciliation {
            detail: format!(
                "after commit the target reports {} bytes but {} were written",
                snapshot.stamp.byte_len,
                bytes.len()
            ),
        });
    }
    Ok(SaveResult {
        outcome: SaveOutcome {
            document_id: request.document_id,
            saved_revision: request.revision,
            serialization: request.encoding.clone(),
            stamp: snapshot.stamp,
            backup_path: backup_path.map(|path| NativePath::from_path(&path)),
            warnings,
        },
        bytes_written: bytes.len() as u64,
        output_digest: digest,
        bytes,
    })
}

fn verify_expected(target: &Path, expected: &FileStamp) -> Result<(), SaveError> {
    let current = snapshot_of_path(target, None)?;
    let current_identity = current.stamp.identity;
    let identity_conflict = match (expected.identity, current_identity) {
        (Some(expected_id), Some(current_id)) => expected_id != current_id,
        _ => false,
    };
    if identity_conflict {
        return Err(SaveError::conflict(
            "the path now refers to a different file",
        ));
    }

    let metadata_matches = current.stamp.byte_len == expected.byte_len
        && current.stamp.last_write_time == expected.last_write_time;

    match expected.content_blake3 {
        Some(expected_hash) => {
            let current_hash = hash_file(target)?;
            if current_hash != expected_hash {
                return Err(SaveError::conflict(
                    "the file content changed on disk after it was opened",
                ));
            }
            Ok(())
        }
        None if metadata_matches => Ok(()),
        None => Err(SaveError::conflict(
            "the file length or modification time changed on disk",
        )),
    }
}

fn make_backup(target: &Path, plan: &BackupPlan) -> Result<PathBuf, SaveError> {
    std::fs::create_dir_all(&plan.directory)?;
    let backup = unique_path(&plan.directory, &plan.file_stem, "bak");
    std::fs::copy(target, &backup)?;
    // FlushFileBuffers needs a write-capable handle on Windows.
    let file = std::fs::OpenOptions::new().write(true).open(&backup)?;
    file.sync_all()?;
    Ok(backup)
}

fn unique_path(directory: &Path, stem: &str, extension: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let pid = std::process::id();
    for attempt in 0..100 {
        let candidate = directory.join(format!("{stem}.{timestamp}.{pid}.{attempt}.{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    directory.join(format!("{stem}.{timestamp}.{pid}.{extension}"))
}

fn rollback_path(parent: &Path) -> PathBuf {
    unique_path(parent, ".notepust-rollback", "tmp")
}

#[cfg(windows)]
fn commit_replace(target: &Path, temp: &Path, rollback: &Path) -> Result<(), SaveError> {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Storage::FileSystem::{REPLACE_FILE_FLAGS, ReplaceFileW};
    use windows::core::PCWSTR;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let target_w = wide(target);
    let temp_w = wide(temp);
    let rollback_w = wide(rollback);

    let result = unsafe {
        ReplaceFileW(
            PCWSTR(target_w.as_ptr()),
            PCWSTR(temp_w.as_ptr()),
            PCWSTR(rollback_w.as_ptr()),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
    };

    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            let win32 = error.code().0 as u32;
            let detail = format!("ReplaceFileW failed ({win32}): {}", error.message());
            match win32 {
                1175..=1177 => Err(SaveError::NeedsReconciliation { detail }),
                _ => Err(SaveError::Io(std::io::Error::other(detail))),
            }
        }
    }
}

#[cfg(not(windows))]
fn commit_replace(target: &Path, temp: &Path, _rollback: &Path) -> Result<(), SaveError> {
    std::fs::rename(temp, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use notepust_contracts::{BomPolicy, EncodingId, FileStamp};
    use notepust_platform::read_file_stamp;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn expected_for(path: &Path) -> FileStamp {
        let mut stamp = read_file_stamp(path).unwrap().stamp;
        stamp.content_blake3 = Some(hash_file(path).unwrap());
        stamp
    }

    #[test]
    fn writes_new_file_and_refuses_to_clobber() {
        let dir = temp_dir();
        let target = dir.path().join("new.txt");
        let spec = EncodingSpec::utf8();
        let request = SaveRequest {
            document_id: DocumentId::new(),
            revision: DocRevision::new(1),
            target: &target,
            encoding: &spec,
            expected: None,
            content: SaveContent::Text("hello\r\n"),
            backup: None,
        };
        let result = save_file(&request).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello\r\n");
        assert_eq!(result.outcome.stamp.byte_len, 7);

        let second = save_file(&request);
        assert!(matches!(second, Err(SaveError::Conflict { .. })));
    }

    #[test]
    fn detects_external_change_before_overwriting() {
        let dir = temp_dir();
        let target = dir.path().join("doc.txt");
        std::fs::write(&target, b"first").unwrap();
        let expected = expected_for(&target);
        std::fs::write(&target, b"second").unwrap();

        let spec = EncodingSpec::utf8();
        let request = SaveRequest {
            document_id: DocumentId::new(),
            revision: DocRevision::new(1),
            target: &target,
            encoding: &spec,
            expected: Some(&expected),
            content: SaveContent::Text("third"),
            backup: None,
        };
        let error = save_file(&request).unwrap_err();
        assert!(matches!(error, SaveError::Conflict { .. }));
        assert_eq!(std::fs::read(&target).unwrap(), b"second");
    }

    #[test]
    fn byte_faithful_save_keeps_exact_bytes() {
        let dir = temp_dir();
        let target = dir.path().join("bom.txt");
        let original = b"\xEF\xBB\xBFa\r\nb";
        std::fs::write(&target, original).unwrap();
        let expected = expected_for(&target);
        let spec = EncodingSpec::new(EncodingId::Utf8, BomPolicy::Utf8);
        let request = SaveRequest {
            document_id: DocumentId::new(),
            revision: DocRevision::new(2),
            target: &target,
            encoding: &spec,
            expected: Some(&expected),
            content: SaveContent::Bytes(original),
            backup: None,
        };
        save_file(&request).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), original);
    }

    #[test]
    fn backup_copies_previous_content() {
        let dir = temp_dir();
        let target = dir.path().join("backup-me.txt");
        std::fs::write(&target, b"old").unwrap();
        let expected = expected_for(&target);
        let backup_dir = dir.path().join("backups");
        let spec = EncodingSpec::utf8();
        let request = SaveRequest {
            document_id: DocumentId::new(),
            revision: DocRevision::new(2),
            target: &target,
            encoding: &spec,
            expected: Some(&expected),
            content: SaveContent::Text("new"),
            backup: Some(BackupPlan {
                directory: backup_dir,
                file_stem: "backup-me".to_string(),
            }),
        };
        let result = save_file(&request).unwrap();
        let backup = result.outcome.backup_path.unwrap().to_path_buf();
        assert_eq!(std::fs::read(backup).unwrap(), b"old");
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn encoding_failure_leaves_target_untouched() {
        let dir = temp_dir();
        let target = dir.path().join("gbk.txt");
        std::fs::write(&target, b"old").unwrap();
        let expected = expected_for(&target);
        let spec = EncodingSpec::new(EncodingId::Gbk, BomPolicy::None);
        let request = SaveRequest {
            document_id: DocumentId::new(),
            revision: DocRevision::new(2),
            target: &target,
            encoding: &spec,
            expected: Some(&expected),
            content: SaveContent::Text("emoji 😀"),
            backup: None,
        };
        let error = save_file(&request).unwrap_err();
        assert!(matches!(error, SaveError::Codec(_)));
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
    }
}
