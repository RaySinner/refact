use crate::{EnvironmentStatus, RequestFingerprint, StreamFailure, UhpErrorObject};

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
    #[error("UHP capability {capability} is required for {operation}")]
    CapabilityUnsupported {
        capability: &'static str,
        operation: &'static str,
    },
    #[error("UHP harness base is unsupported: {base}")]
    UnsupportedBase { base: String },
    #[error("UHP model {model} is unavailable for harness {harness_id:?}")]
    ModelUnavailable {
        harness_id: Option<String>,
        model: String,
    },
    #[error("UHP harness precondition failed for {harness_id}")]
    HarnessPreconditionFailed { harness_id: String },
    #[error("UHP environment {environment_id} was not found")]
    EnvironmentNotFound { environment_id: String },
    #[error(
        "UHP environment {environment_id} is not ready (status {status:?}, version {version:?})"
    )]
    EnvironmentNotReady {
        environment_id: String,
        status: Option<EnvironmentStatus>,
        version: Option<u64>,
    },
    #[error("UHP environment {environment_id} is busy")]
    EnvironmentBusy { environment_id: String },
    #[error("UHP environment {environment_id} is unavailable (HTTP {status})")]
    EnvironmentUnavailable { environment_id: String, status: u16 },
    #[error("UHP server returned HTTP {status}: {error}")]
    Server { status: u16, error: UhpErrorObject },
    #[error("Idempotency-Key fingerprint conflict: stored {expected}, requested {actual}")]
    FingerprintConflict {
        expected: RequestFingerprint,
        actual: RequestFingerprint,
    },
    #[error(transparent)]
    Stream(#[from] StreamFailure),
}
