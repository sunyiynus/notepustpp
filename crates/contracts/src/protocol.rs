use std::io::{Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::search::{FinishReason, RegexEngine, SearchMatch, SearchRequest};

/// Version of the length-prefixed worker protocol.
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum serialized frame size (1 MiB).
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;

/// Messages sent from the application to `search-worker`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case")]
pub enum WorkerRequest {
    Hello {
        protocol_version: u32,
        build_id: String,
    },
    Search(SearchRequest),
    Shutdown,
}

/// Messages sent from `search-worker` to the application.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case")]
pub enum WorkerResponse {
    Ready {
        protocol_version: u32,
        build_id: String,
        engines: Vec<RegexEngine>,
    },
    MatchBatch(MatchBatch),
    Progress(Progress),
    Finished {
        job_id: crate::ids::JobId,
        reason: FinishReason,
        stats: SearchStats,
    },
    ReplacementReady(ReplacementReady),
    Failed {
        job_id: Option<crate::ids::JobId>,
        category: crate::errors::ErrorCategory,
        detail: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchBatch {
    pub job_id: crate::ids::JobId,
    pub snapshot_id: crate::ids::SnapshotId,
    pub sequence: u32,
    pub matches: Vec<SearchMatch>,
    /// Bytes scanned when the batch was emitted (progress, not a total).
    pub bytes_scanned: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Progress {
    pub job_id: crate::ids::JobId,
    pub bytes_scanned: u64,
    pub matches_found: u64,
    /// `None` when the worker cannot know a total.
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchStats {
    pub bytes_scanned: u64,
    pub matches_found: u64,
    pub elapsed_ms: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplacementReady {
    pub job_id: crate::ids::JobId,
    pub snapshot_id: crate::ids::SnapshotId,
    pub artifact_path: crate::files::NativePath,
    pub utf8_length: u64,
    pub digest: [u8; 32],
    pub match_count: u64,
    pub complete: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame is larger than the {MAX_FRAME_LEN} byte protocol limit")]
    TooLarge,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON frame: {0}")]
    Json(#[from] serde_json::Error),
}

/// Encodes one JSON payload as a little-endian `u32` length prefix + payload.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let payload = serde_json::to_vec(value)?;
    let len = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge)?;
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge);
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Decodes one complete frame (length prefix included).
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, FrameError> {
    if frame.len() < 4 {
        return Err(FrameError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "frame header is shorter than 4 bytes",
        )));
    }
    let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge);
    }
    let payload = &frame[4..];
    if payload.len() != len as usize {
        return Err(FrameError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame declared {len} bytes but carried {}", payload.len()),
        )));
    }
    Ok(serde_json::from_slice(payload)?)
}

/// Reads one frame from a stream. Returns `None` on a clean EOF at the frame
/// boundary; a partial frame is an error.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>, FrameError> {
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        match reader.read(&mut header[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(None);
                }
                return Err(FrameError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "stream ended inside a frame header",
                )));
            }
            Ok(n) => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(FrameError::Io(error)),
        }
    }

    let len = u32::from_le_bytes(header);
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge);
    }
    let mut payload = vec![0u8; len as usize];
    let mut filled = 0;
    while filled < payload.len() {
        match reader.read(&mut payload[filled..]) {
            Ok(0) => {
                return Err(FrameError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "stream ended inside a frame payload",
                )));
            }
            Ok(n) => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(FrameError::Io(error)),
        }
    }

    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&payload);
    Ok(Some(frame))
}

/// Writes one encoded frame and flushes it.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), FrameError> {
    let frame = encode_frame(value)?;
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::JobId;

    #[test]
    fn frame_roundtrip() {
        let request = WorkerRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
            build_id: "test".to_string(),
        };
        let frame = encode_frame(&request).unwrap();
        let back: WorkerRequest = decode_frame(&frame).unwrap();
        match back {
            WorkerRequest::Hello { build_id, .. } => assert_eq!(build_id, "test"),
            _ => panic!("wrong message"),
        }

        let mut cursor = std::io::Cursor::new(frame);
        let raw = read_frame(&mut cursor).unwrap().unwrap();
        let back: WorkerRequest = decode_frame(&raw).unwrap();
        assert!(matches!(back, WorkerRequest::Hello { .. }));
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn oversized_frames_are_rejected_before_allocation() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&(MAX_FRAME_LEN + 1).to_le_bytes());
        let mut cursor = std::io::Cursor::new(frame);
        assert!(matches!(read_frame(&mut cursor), Err(FrameError::TooLarge)));
    }

    #[test]
    fn failed_response_carries_stable_category() {
        let response = WorkerResponse::Failed {
            job_id: Some(JobId::new()),
            category: crate::errors::ErrorCategory::ResourceLimit,
            detail: "memory".to_string(),
        };
        let frame = encode_frame(&response).unwrap();
        let back: WorkerResponse = decode_frame(&frame).unwrap();
        assert!(matches!(
            back,
            WorkerResponse::Failed {
                category: crate::errors::ErrorCategory::ResourceLimit,
                ..
            }
        ));
    }
}
