use serde::{Deserialize, Serialize};

/// Stable error categories used by the UI and the worker protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Cancelled,
    Conflict,
    EncodingLoss,
    InvalidEncoding,
    Unsupported,
    ResourceLimit,
    NotFound,
    Permission,
    Io,
    InvalidRequest,
    Protocol,
    Internal,
}

impl ErrorCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Conflict => "conflict",
            Self::EncodingLoss => "encoding_loss",
            Self::InvalidEncoding => "invalid_encoding",
            Self::Unsupported => "unsupported",
            Self::ResourceLimit => "resource_limit",
            Self::NotFound => "not_found",
            Self::Permission => "permission",
            Self::Io => "io",
            Self::InvalidRequest => "invalid_request",
            Self::Protocol => "protocol",
            Self::Internal => "internal",
        }
    }
}

/// Matchable domain errors shared across modules.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OperationError {
    #[error("operation cancelled")]
    Cancelled,

    #[error("conflict: {reason}")]
    Conflict { reason: String },

    #[error("text cannot be represented exactly in the target encoding: {detail}")]
    EncodingLoss { detail: String },

    #[error("invalid encoding: {detail}")]
    InvalidEncoding { detail: String },

    #[error("unsupported: {detail}")]
    Unsupported { detail: String },

    #[error("resource limit reached: {detail}")]
    ResourceLimit { detail: String },

    #[error("not found: {path}")]
    NotFound { path: String },

    #[error("permission denied: {path}")]
    PermissionDenied { path: String },

    #[error("I/O error ({kind:?}): {message}")]
    Io {
        kind: std::io::ErrorKind,
        message: String,
    },

    #[error("invalid request: {detail}")]
    InvalidRequest { detail: String },

    #[error("protocol error: {detail}")]
    Protocol { detail: String },

    #[error("internal error: {detail}")]
    Internal { detail: String },
}

impl OperationError {
    pub fn conflict(reason: impl Into<String>) -> Self {
        Self::Conflict {
            reason: reason.into(),
        }
    }

    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::InvalidRequest {
            detail: detail.into(),
        }
    }

    pub fn resource_limit(detail: impl Into<String>) -> Self {
        Self::ResourceLimit {
            detail: detail.into(),
        }
    }

    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::Cancelled => ErrorCategory::Cancelled,
            Self::Conflict { .. } => ErrorCategory::Conflict,
            Self::EncodingLoss { .. } => ErrorCategory::EncodingLoss,
            Self::InvalidEncoding { .. } => ErrorCategory::InvalidEncoding,
            Self::Unsupported { .. } => ErrorCategory::Unsupported,
            Self::ResourceLimit { .. } => ErrorCategory::ResourceLimit,
            Self::NotFound { .. } => ErrorCategory::NotFound,
            Self::PermissionDenied { .. } => ErrorCategory::Permission,
            Self::Io { .. } => ErrorCategory::Io,
            Self::InvalidRequest { .. } => ErrorCategory::InvalidRequest,
            Self::Protocol { .. } => ErrorCategory::Protocol,
            Self::Internal { .. } => ErrorCategory::Internal,
        }
    }
}

impl From<std::io::Error> for OperationError {
    fn from(error: std::io::Error) -> Self {
        let kind = error.kind();
        let message = error.to_string();
        if kind == std::io::ErrorKind::NotFound {
            Self::NotFound { path: message }
        } else if kind == std::io::ErrorKind::PermissionDenied {
            Self::PermissionDenied { path: message }
        } else {
            Self::Io { kind, message }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_are_matchable() {
        assert_eq!(
            OperationError::Cancelled.category(),
            ErrorCategory::Cancelled
        );
        assert_eq!(
            OperationError::conflict("changed on disk").category(),
            ErrorCategory::Conflict
        );
    }
}
