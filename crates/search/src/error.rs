#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("invalid search pattern: {detail}")]
    InvalidPattern { detail: String },

    #[error("this pattern needs advanced regex mode: {detail}")]
    AdvancedRequired { detail: String },

    #[error("advanced regex mode is not available in this build path")]
    AdvancedUnavailable,

    #[error("search resource limit reached: {detail}")]
    ResourceLimit { detail: String },

    #[error("search cancelled")]
    Cancelled,

    #[error("I/O error for {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("file {path} could not be decoded: {detail}")]
    Encoding { path: String, detail: String },

    #[error("search worker protocol error: {detail}")]
    Protocol { detail: String },
}

impl SearchError {
    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::InvalidPattern {
            detail: detail.into(),
        }
    }

    pub fn resource(detail: impl Into<String>) -> Self {
        Self::ResourceLimit {
            detail: detail.into(),
        }
    }
}

impl From<notepust_contracts::OperationError> for SearchError {
    fn from(error: notepust_contracts::OperationError) -> Self {
        match error {
            notepust_contracts::OperationError::Cancelled => Self::Cancelled,
            notepust_contracts::OperationError::ResourceLimit { detail } => {
                Self::ResourceLimit { detail }
            }
            notepust_contracts::OperationError::InvalidEncoding { detail } => {
                Self::InvalidPattern { detail }
            }
            other => Self::Protocol {
                detail: other.to_string(),
            },
        }
    }
}
