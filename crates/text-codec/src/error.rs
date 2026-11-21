use notepust_contracts::{EncodingId, OperationError};

/// Errors produced by strict decoding and encoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("malformed {encoding} at raw byte {raw_offset}: {detail}")]
    Malformed {
        encoding: EncodingId,
        raw_offset: u64,
        detail: String,
    },

    #[error("BOM mismatch: file carries {actual:?} but {expected} was selected")]
    BomMismatch {
        expected: EncodingId,
        actual: notepust_contracts::BomPolicy,
    },

    #[error("encoding {encoding} is not supported for this operation: {detail}")]
    Unsupported {
        encoding: EncodingId,
        detail: String,
    },

    #[error(
        "character {character:?} at UTF-8 offset {utf8_offset} cannot be encoded as {encoding}"
    )]
    Unmappable {
        encoding: EncodingId,
        character: char,
        utf8_offset: u64,
    },

    #[error("an encoding choice is required; best candidate is {candidate}")]
    EncodingChoiceRequired { candidate: EncodingId },

    #[error("input is not valid UTF-8 at byte {offset}")]
    InvalidUtf8 { offset: u64 },
}

impl CodecError {
    pub fn malformed(encoding: EncodingId, raw_offset: u64, detail: impl Into<String>) -> Self {
        Self::Malformed {
            encoding,
            raw_offset,
            detail: detail.into(),
        }
    }
}

impl From<CodecError> for OperationError {
    fn from(error: CodecError) -> Self {
        match error {
            CodecError::Unmappable {
                character,
                utf8_offset,
                encoding,
            } => OperationError::EncodingLoss {
                detail: format!(
                    "{character:?} at UTF-8 offset {utf8_offset} cannot be encoded as {encoding}"
                ),
            },
            CodecError::Unsupported {
                encoding, detail, ..
            } => OperationError::Unsupported {
                detail: format!("{encoding}: {detail}"),
            },
            CodecError::InvalidUtf8 { offset } => OperationError::InvalidEncoding {
                detail: format!("invalid UTF-8 at byte {offset}"),
            },
            CodecError::BomMismatch { .. }
            | CodecError::Malformed { .. }
            | CodecError::EncodingChoiceRequired { .. } => OperationError::InvalidEncoding {
                detail: error.to_string(),
            },
        }
    }
}
