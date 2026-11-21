use std::io::Read;
use std::path::{Path, PathBuf};

use notepust_contracts::{
    EncodingId, EncodingSpec, FileIdentity, FileStamp, NativePath, SnapshotId,
};
use notepust_platform::{read_file_stamp, snapshot_of_path};
use notepust_text_codec::{
    CodecError, DecodedFile, DetectionSource, RoundTripClass, decode_detected, decode_explicit,
};

use crate::error::OpenError;

/// Default ceiling for the editable-file path. Larger files open through the
/// on-demand log reader instead.
pub const DEFAULT_OPEN_LIMIT: u64 = 100 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct OpenOptions {
    pub explicit_encoding: Option<EncodingSpec>,
    pub allow_guess: bool,
    pub max_bytes: u64,
    /// Refuse the normal editor when one logical line is longer than this.
    /// Kit shapes whole lines; a 10 MB line freezes its layout. `None` is for
    /// callers that provide their own horizontal segmentation.
    pub max_line_bytes: Option<u64>,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            explicit_encoding: None,
            allow_guess: false,
            max_bytes: DEFAULT_OPEN_LIMIT,
            max_line_bytes: Some(2 * 1024 * 1024),
        }
    }
}

/// A fully read and decoded editable file.
#[derive(Clone, Debug)]
pub struct OpenedFile {
    pub path: PathBuf,
    pub native_path: NativePath,
    pub raw_bytes: Vec<u8>,
    pub raw_snapshot: SnapshotId,
    pub text: String,
    pub encoding: EncodingSpec,
    pub detection: DetectionSource,
    pub round_trip: RoundTripClass,
    pub stamp_before: FileStamp,
    pub stamp_after: FileStamp,
    pub identity: Option<FileIdentity>,
    pub warnings: Vec<String>,
}

impl OpenedFile {
    pub fn byte_len(&self) -> u64 {
        self.raw_bytes.len() as u64
    }

    pub fn has_bom(&self) -> bool {
        self.encoding.bom.is_bom()
    }
}

/// Opens and decodes a file for full editing.
///
/// The file is read sequentially, then its identity and stamp are checked
/// again. A mismatch is reported instead of treating a torn read as a stable
/// editing baseline.
pub fn open_file(path: impl AsRef<Path>, options: &OpenOptions) -> Result<OpenedFile, OpenError> {
    let path = path.as_ref();
    let before = read_file_stamp(path).map_err(|source| OpenError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    if options.max_bytes != 0 && before.stamp.byte_len > options.max_bytes {
        return Err(OpenError::TooLarge {
            path: path.to_path_buf(),
            byte_len: before.stamp.byte_len,
            limit: options.max_bytes,
        });
    }

    let mut file = std::fs::File::open(path).map_err(|source| OpenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut raw_bytes = Vec::with_capacity(before.stamp.byte_len.min(16 * 1024 * 1024) as usize);
    file.read_to_end(&mut raw_bytes)
        .map_err(|source| OpenError::Io {
            path: path.to_path_buf(),
            source,
        })?;

    let after = read_file_stamp(path).map_err(|source| OpenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if before.stamp.identity != after.stamp.identity
        || before.stamp.byte_len != after.stamp.byte_len
        || before.stamp.last_write_time != after.stamp.last_write_time
        || raw_bytes.len() as u64 != after.stamp.byte_len
    {
        return Err(OpenError::ChangedDuringRead {
            path: path.to_path_buf(),
        });
    }

    let hash = *blake3::hash(&raw_bytes).as_bytes();
    let mut stamp_after = after.stamp.clone();
    stamp_after.content_blake3 = Some(hash);

    let decoded: DecodedFile = if let Some(spec) = &options.explicit_encoding {
        decode_explicit(&raw_bytes, spec)?
    } else {
        match decode_detected(&raw_bytes, options.allow_guess) {
            Ok(decoded) => decoded,
            Err(CodecError::EncodingChoiceRequired { candidate }) => {
                return Err(OpenError::EncodingChoiceRequired { candidate });
            }
            Err(error) => return Err(OpenError::Codec(error)),
        }
    };

    if !matches!(
        decoded.spec.encoding,
        EncodingId::Utf16Le | EncodingId::Utf16Be
    ) && raw_bytes[..raw_bytes.len().min(8192)].contains(&0)
    {
        return Err(OpenError::Binary {
            path: path.to_path_buf(),
        });
    }

    if let Some(limit) = options.max_line_bytes {
        let longest = longest_logical_line(&decoded.text);
        if longest > limit {
            return Err(OpenError::LongLine {
                byte_len: longest,
                limit,
            });
        }
    }

    let mut warnings = Vec::new();
    if decoded.round_trip == RoundTripClass::NonCanonical {
        warnings.push(
            "This file does not re-encode to the same bytes; editing it will rewrite the encoding."
                .to_string(),
        );
    }
    if decoded.source == DetectionSource::Guessed {
        warnings.push(format!(
            "Encoding {} was guessed from the content.",
            decoded.spec.encoding.label()
        ));
    }

    Ok(OpenedFile {
        path: path.to_path_buf(),
        native_path: NativePath::from_path(path),
        raw_snapshot: SnapshotId::new(),
        text: decoded.text,
        encoding: decoded.spec,
        detection: decoded.source,
        round_trip: decoded.round_trip,
        stamp_before: before.stamp,
        stamp_after,
        identity: after.stamp.identity,
        raw_bytes,
        warnings,
    })
}

fn longest_logical_line(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut longest = 0u64;
    let mut start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                longest = longest.max((index - start) as u64);
                start = index + 1;
            }
            b'\r' => {
                longest = longest.max((index - start) as u64);
                if bytes.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    longest.max((bytes.len() - start) as u64)
}

/// Re-decodes the retained raw bytes with an explicit encoding, without
/// touching the original file.
pub fn reinterpret(raw_bytes: &[u8], spec: &EncodingSpec) -> Result<DecodedFile, CodecError> {
    decode_explicit(raw_bytes, spec)
}

/// Reads a small prefix for the encoding-choice preview.
pub fn encoding_preview(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; limit];
    let mut filled = 0usize;
    while filled < limit {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    buffer.truncate(filled);
    Ok(buffer)
}

/// Best candidate for an undecodable file, used by the encoding-choice UI.
pub fn encoding_candidate(raw_bytes: &[u8]) -> EncodingId {
    notepust_text_codec::detect_candidate(raw_bytes)
}

/// Computes a fresh content hash for a file, for external-change checks that
/// need more than `mtime + size`.
pub fn hash_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        match file.read(&mut buffer)? {
            0 => break,
            read => {
                hasher.update(&buffer[..read]);
            }
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

/// Captures a stamp including a content hash. Used by save and external-change
/// workflows, not by the per-keystroke path.
pub fn content_stamp(path: &Path) -> std::io::Result<FileStamp> {
    let hash = hash_file(path)?;
    let snapshot = snapshot_of_path(path, Some(hash))?;
    Ok(snapshot.stamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn opens_utf8_and_keeps_crlf() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all("a\r\n中\n".as_bytes()).unwrap();
        file.flush().unwrap();
        let opened = open_file(file.path(), &OpenOptions::default()).unwrap();
        assert_eq!(opened.text, "a\r\n中\n");
        assert_eq!(opened.detection, DetectionSource::StrictUtf8);
        assert!(opened.warnings.is_empty());
    }

    #[test]
    fn refuses_oversized_files() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&[b'a'; 32]).unwrap();
        let options = OpenOptions {
            max_bytes: 16,
            ..Default::default()
        };
        assert!(matches!(
            open_file(file.path(), &options),
            Err(OpenError::TooLarge { .. })
        ));
    }

    #[test]
    fn refuses_long_single_lines_in_normal_edit_mode() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&vec![b'a'; 3 * 1024 * 1024]).unwrap();
        file.flush().unwrap();
        let error = open_file(file.path(), &OpenOptions::default()).unwrap_err();
        assert!(matches!(error, OpenError::LongLine { .. }));
    }

    #[test]
    fn requires_confirmation_for_guessed_encodings() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&[0xD6, 0xD0, 0xCE, 0xC4]).unwrap();
        let options = OpenOptions::default();
        let error = open_file(file.path(), &options).unwrap_err();
        assert!(matches!(error, OpenError::EncodingChoiceRequired { .. }));
    }
}
