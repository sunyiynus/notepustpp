//! File opening, reliable saving and external-change detection.
//!
//! `open_file` retains the original source bytes so an unchanged save can be
//! byte-for-byte faithful. `save_file` verifies the observed disk version,
//! writes and fsyncs a same-directory temporary file, and commits with
//! `ReplaceFileW` on Windows.

pub mod error;
pub mod open;
pub mod save;
pub mod watch;

pub use error::{OpenError, SaveError};
pub use open::{
    DEFAULT_OPEN_LIMIT, OpenOptions, OpenedFile, content_stamp, encoding_candidate,
    encoding_preview, hash_file, open_file, reinterpret,
};
pub use save::{BackupPlan, SaveContent, SaveRequest, SaveResult, save_file};
pub use watch::{ExternalChange, detect_external_change, detect_external_change_deep};
