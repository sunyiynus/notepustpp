use std::path::PathBuf;

use notepust_contracts::{EncodingId, OperationError};
use notepust_text_codec::CodecError;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("file is {byte_len} bytes, above the {limit} byte open limit")]
    TooLarge {
        path: PathBuf,
        byte_len: u64,
        limit: u64,
    },

    #[error("file changed while it was being read")]
    ChangedDuringRead { path: PathBuf },

    #[error("an encoding must be selected; best candidate is {candidate}")]
    EncodingChoiceRequired { candidate: EncodingId },

    #[error("the file appears to be binary: {path}")]
    Binary { path: PathBuf },

    #[error("a line is {byte_len} bytes, above the {limit} byte normal-edit limit")]
    LongLine { byte_len: u64, limit: u64 },

    #[error(transparent)]
    Codec(#[from] CodecError),

    #[error("I/O error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl From<OpenError> for OperationError {
    fn from(error: OpenError) -> Self {
        match error {
            OpenError::TooLarge {
                byte_len, limit, ..
            } => OperationError::ResourceLimit {
                detail: format!("file is {byte_len} bytes; open limit is {limit}"),
            },
            OpenError::ChangedDuringRead { path } => OperationError::conflict(format!(
                "{} changed while it was being read",
                path.display()
            )),
            OpenError::EncodingChoiceRequired { candidate } => OperationError::InvalidEncoding {
                detail: format!("encoding choice required; best candidate is {candidate}"),
            },
            OpenError::Binary { path } => OperationError::Unsupported {
                detail: format!("{} appears to be binary", path.display()),
            },
            OpenError::LongLine { byte_len, limit } => OperationError::ResourceLimit {
                detail: format!(
                    "a line is {byte_len} bytes; normal editing is limited to {limit} bytes per line; use the log view (not yet wired)"
                ),
            },
            OpenError::Codec(error) => error.into(),
            OpenError::Io { path, source } => {
                let mut converted = OperationError::from(source);
                if let OperationError::NotFound { path: message }
                | OperationError::PermissionDenied { path: message }
                | OperationError::Io { message, .. } = &mut converted
                {
                    *message = path.display().to_string();
                }
                converted
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("conflict: {detail}")]
    Conflict {
        detail: String,
        /// A prepared temp file that can still be saved elsewhere or inspected.
        preserved_temp: Option<PathBuf>,
    },

    #[error("the target changed in a way that needs reconciliation: {detail}")]
    NeedsReconciliation { detail: String },

    #[error("save target is not supported: {detail}")]
    Unsupported { detail: String },

    #[error(transparent)]
    Codec(#[from] CodecError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl SaveError {
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::Conflict {
            detail: detail.into(),
            preserved_temp: None,
        }
    }

    pub fn with_temp(mut self, temp: Option<PathBuf>) -> Self {
        if let Self::Conflict { preserved_temp, .. } = &mut self {
            *preserved_temp = temp;
        }
        self
    }
}

impl From<SaveError> for OperationError {
    fn from(error: SaveError) -> Self {
        match error {
            SaveError::Conflict { detail, .. } => OperationError::conflict(detail),
            SaveError::NeedsReconciliation { detail } => {
                OperationError::conflict(format!("needs reconciliation: {detail}"))
            }
            SaveError::Unsupported { detail } => OperationError::Unsupported { detail },
            SaveError::Codec(error) => error.into(),
            SaveError::Io(error) => error.into(),
        }
    }
}
