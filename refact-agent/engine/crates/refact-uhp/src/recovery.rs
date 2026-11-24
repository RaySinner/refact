use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{RequestFingerprint, Response, ResponseStatus, UhpError};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RecoveryBinding {
    pub request_fingerprint: RequestFingerprint,
    #[serde(default)]
    pub response_id: Option<String>,
}

impl RecoveryBinding {
    pub fn before_response(request_fingerprint: RequestFingerprint) -> Self {
        Self {
            request_fingerprint,
            response_id: None,
        }
    }

    pub fn after_created(request_fingerprint: RequestFingerprint, response_id: String) -> Self {
        Self {
            request_fingerprint,
            response_id: Some(response_id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetryAdvice {
    RecoverResponse {
        response_id: Option<String>,
    },
    WaitForTerminalAndRetry {
        retry_after_ms: Option<u64>,
        in_flight_response_id: Option<String>,
    },
    StartLinkedAttempt {
        reason: LinkedAttemptReason,
    },
    NoAutomaticRetry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkedAttemptReason {
    SessionExpired,
}

pub fn retry_advice(error: &UhpError) -> RetryAdvice {
    match error {
        UhpError::Stream(failure) => RetryAdvice::RecoverResponse {
            response_id: failure.response_id.clone(),
        },
        UhpError::Server { error, .. } if error.code == "session_busy" => {
            RetryAdvice::WaitForTerminalAndRetry {
                retry_after_ms: detail_field(error.detail.as_ref(), "retry_after_ms")
                    .and_then(Value::as_u64),
                in_flight_response_id: detail_field(error.detail.as_ref(), "response_id")
                    .or_else(|| detail_field(error.detail.as_ref(), "in_flight_response_id"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            }
        }
        UhpError::Server { error, .. } if error.code == "session_expired" => {
            RetryAdvice::StartLinkedAttempt {
                reason: LinkedAttemptReason::SessionExpired,
            }
        }
        _ => RetryAdvice::NoAutomaticRetry,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterruptedResumeAdvice {
    NotInterrupted,
    ExplicitResumeAllowed,
    ExplicitNewAttemptRequired(UnsafeResumeReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnsafeResumeReason {
    ResumeCapabilityMissing,
    EffectsMayRepeat,
    WorkspaceLost,
    WorkspaceUnknown,
}

pub fn interrupted_resume_advice(
    response: &Response,
    resume_capability: bool,
) -> InterruptedResumeAdvice {
    if response.status != ResponseStatus::Failed {
        return InterruptedResumeAdvice::NotInterrupted;
    }
    let Some(error) = &response.error else {
        return InterruptedResumeAdvice::NotInterrupted;
    };
    if detail_field(error.detail.as_ref(), "reason").and_then(Value::as_str) != Some("interrupted")
    {
        return InterruptedResumeAdvice::NotInterrupted;
    }
    if !resume_capability {
        return InterruptedResumeAdvice::ExplicitNewAttemptRequired(
            UnsafeResumeReason::ResumeCapabilityMissing,
        );
    }
    if detail_field(error.detail.as_ref(), "effects_may_repeat").and_then(Value::as_bool)
        != Some(false)
    {
        return InterruptedResumeAdvice::ExplicitNewAttemptRequired(
            UnsafeResumeReason::EffectsMayRepeat,
        );
    }
    match detail_field(error.detail.as_ref(), "workspace").and_then(Value::as_str) {
        Some("kept" | "checkpoint") => InterruptedResumeAdvice::ExplicitResumeAllowed,
        Some("lost") => {
            InterruptedResumeAdvice::ExplicitNewAttemptRequired(UnsafeResumeReason::WorkspaceLost)
        }
        _ => InterruptedResumeAdvice::ExplicitNewAttemptRequired(
            UnsafeResumeReason::WorkspaceUnknown,
        ),
    }
}

fn detail_field<'a>(detail: Option<&'a Value>, field: &str) -> Option<&'a Value> {
    detail
        .and_then(Value::as_object)
        .and_then(|detail| detail.get(field))
}
