use crate::{RequestFingerprint, UhpErrorObject};

#[derive(Debug, thiserror::Error)]
pub enum UhpError {
    #[error("invalid UHP base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("Idempotency-Key must be non-empty ASCII without control characters")]
    InvalidIdempotencyKey,
    #[error("UHP transport failed during {operation}: {source}")]
    Transport {
        operation: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("UHP schema validation failed for {operation}: {source}")]
    Schema {
        operation: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid UHP response for {operation}: {message}")]
    InvalidResponse {
        operation: &'static str,
        message: String,
    },
    #[error("UHP version mismatch: expected {expected}, response carried {actual:?}")]
    VersionMismatch {
        expected: &'static str,
        actual: Option<String>,
    },
    #[error("UHP server does not support {expected}; advertised versions: {supported:?}")]
    UnsupportedProtocolVersion {
        expected: &'static str,
        supported: Vec<String>,
    },
    #[error("UHP server returned HTTP {status}: {error}")]
    Server { status: u16, error: UhpErrorObject },
    #[error("Idempotency-Key fingerprint conflict: stored {expected}, requested {actual}")]
    FingerprintConflict {
        expected: RequestFingerprint,
        actual: RequestFingerprint,
    },
}
