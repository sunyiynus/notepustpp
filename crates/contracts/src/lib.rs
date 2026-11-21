//! Pure data contracts shared by every Notepust module.
//!
//! This crate intentionally has no UI, filesystem, database or native
//! dependencies. Coordinates, identities, revisions, encoding descriptions and
//! the worker wire protocol live here so that the same types are used by the
//! application, the background services and the search worker process.

pub mod edits;
pub mod encoding;
pub mod errors;
pub mod files;
pub mod ids;
pub mod offsets;
pub mod protocol;
pub mod search;

pub use edits::{EditBatch, SelectionRange, TextEdit};
pub use encoding::{BomPolicy, ByteWritePolicy, EncodingId, EncodingSpec};
pub use errors::{ErrorCategory, OperationError};
pub use files::{FileIdentity, FileStamp, NativePath, SaveBaseline, SaveOutcome};
pub use ids::{
    DocRevision, DocumentId, JobId, QueryId, SnapshotId, SourceSnapshotId, TransactionId, ViewId,
};
pub use offsets::{
    DisplayColumn, LogicalPosition, RawByteOffset, RawByteRange, Utf8Offset, Utf8Range,
    Utf16Position,
};
pub use protocol::{
    FrameError, MAX_FRAME_LEN, MatchBatch, PROTOCOL_VERSION, Progress, ReplacementReady,
    SearchStats, WorkerRequest, WorkerResponse, decode_frame, encode_frame, read_frame,
    write_frame,
};
pub use search::{
    FinishReason, RegexEngine, ReplacementPiece, ReplacementProgram, SearchMatch, SearchOptions,
    SearchPurpose, SearchRequest,
};
