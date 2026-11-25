use std::collections::BTreeMap;
use std::fmt;

use refact_uhp::{
    Capabilities, Discovery, EvidenceCheckStatus, ExternalEvidenceRecord, ResponseStatus,
    RetiredSessions, UhpDeliveryRequestState, UhpDeliveryState, UhpError, UhpSessionBinding,
};
use serde::{Deserialize, Serialize};
use refact_room::SourceDeliveryVersion;

pub const EXTERNAL_PARTICIPANT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalParticipantState {
    Staged,
    Unverified,
    Verified,
    Failed,
}

impl Default for ExternalParticipantState {
    fn default() -> Self {
        Self::Staged
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalRegistryMode {
    InternalAgents,
    ExternalParticipants,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalParticipantKind {
    External,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ExternalParticipantConfig {
    pub id: String,
    pub name: String,
    pub base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_id: Option<String>,
    pub server_root: String,
    pub model: String,
    pub credential_profile_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_step: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub writes: bool,
    #[serde(default)]
    pub cli_pin: String,
    #[serde(default)]
    pub server_image_digest: String,
}

impl ExternalParticipantConfig {
    pub fn validate(&self) -> Result<(), ExternalParticipantError> {
        for value in [
            &self.id,
            &self.name,
            &self.base,
            &self.server_root,
            &self.model,
            &self.credential_profile_ref,
        ] {
            if value.trim().is_empty() {
                return Err(ExternalParticipantError::InvalidConfiguration(
                    "required participant fields must not be empty",
                ));
            }
        }
        if self.writes {
            return Err(ExternalParticipantError::ExternalWriterForbidden);
        }
        if self.max_step == Some(0) || self.timeout_seconds == Some(0) {
            return Err(ExternalParticipantError::InvalidConfiguration(
                "participant budgets must be greater than zero",
            ));
        }
        if self.environment_id.as_deref() == Some("") {
            return Err(ExternalParticipantError::InvalidConfiguration(
                "environment_id must not be empty",
            ));
        }
        let server = reqwest::Url::parse(&self.server_root).map_err(|_| {
            ExternalParticipantError::InvalidConfiguration("invalid UHP server root")
        })?;
        if !matches!(server.scheme(), "http" | "https")
            || !server.username().is_empty()
            || server.password().is_some()
            || server.query().is_some()
            || server.fragment().is_some()
        {
            return Err(ExternalParticipantError::InvalidConfiguration(
                "UHP server root must be an HTTP(S) URL without credentials, query, or fragment",
            ));
        }
        if self
            .disabled_tools
            .iter()
            .any(|name| name.trim().is_empty() || name.chars().any(char::is_control))
        {
            return Err(ExternalParticipantError::InvalidConfiguration(
                "disabled tool names must be non-empty and printable",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SanitizedExternalError {
    pub code: String,
    pub message: String,
}

impl SanitizedExternalError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalParticipantError {
    NotFound(String),
    AlreadyExists(String),
    InvalidConfiguration(&'static str),
    InvalidOperation(&'static str),
    ExternalWriterForbidden,
    CapabilityUnsupported {
        capability: &'static str,
        action: &'static str,
    },
    NotVerified,
    SessionExpired,
    ProvisionConflict,
    Storage(String),
    Transport(SanitizedExternalError),
}

impl ExternalParticipantError {
    pub fn sanitized(&self) -> SanitizedExternalError {
        match self {
            Self::NotFound(_) => {
                SanitizedExternalError::new("not_found", "participant or record was not found")
            }
            Self::AlreadyExists(_) => {
                SanitizedExternalError::new("already_exists", "participant already exists")
            }
            Self::InvalidConfiguration(message) => {
                SanitizedExternalError::new("invalid_configuration", *message)
            }
            Self::InvalidOperation(message) => {
                SanitizedExternalError::new("invalid_operation", *message)
            }
            Self::ExternalWriterForbidden => SanitizedExternalError::new(
                "external_writer_forbidden",
                "external participants are always read-only",
            ),
            Self::CapabilityUnsupported { capability, .. } => SanitizedExternalError::new(
                "capability_unsupported",
                format!("required capability '{capability}' is unavailable"),
            ),
            Self::NotVerified => SanitizedExternalError::new(
                "live_evidence_required",
                "matching all-pass live evidence is required",
            ),
            Self::SessionExpired => SanitizedExternalError::new(
                "session_expired",
                "the external session expired and requires a linked attempt",
            ),
            Self::ProvisionConflict => SanitizedExternalError::new(
                "provision_conflict",
                "the idempotency key was already used for different configuration",
            ),
            Self::Storage(_) => SanitizedExternalError::new(
                "storage_error",
                "participant registry persistence failed",
            ),
            Self::Transport(error) => error.clone(),
        }
    }
}

impl fmt::Display for ExternalParticipantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.sanitized().message)
    }
}

impl std::error::Error for ExternalParticipantError {}

pub fn sanitize_uhp_error(error: &UhpError) -> SanitizedExternalError {
    let code = match error {
        UhpError::InvalidBinding(_) => "invalid_binding",
        UhpError::InvalidBaseUrl(_) => "invalid_server_root",
        UhpError::InvalidIdempotencyKey => "invalid_idempotency_key",
        UhpError::Transport { .. } => "transport",
        UhpError::Schema { .. } => "protocol_schema",
        UhpError::InvalidResponse { .. } => "protocol_response",
        UhpError::VersionMismatch { .. } => "version_mismatch",
        UhpError::UnsupportedProtocolVersion { .. } => "unsupported_protocol_version",
        UhpError::CapabilityUnsupported { .. } => "capability_unsupported",
        UhpError::UnsupportedBase { .. } => "unsupported_base",
        UhpError::ModelUnavailable { .. } => "model_unavailable",
        UhpError::HarnessPreconditionFailed { .. } => "harness_precondition_failed",
        UhpError::EnvironmentNotFound { .. } => "environment_not_found",
        UhpError::EnvironmentNotReady { .. } => "environment_not_ready",
        UhpError::EnvironmentBusy { .. } => "environment_busy",
        UhpError::EnvironmentUnavailable { .. } => "environment_unavailable",
        UhpError::Server { error, .. } => error.code.as_str(),
        UhpError::FingerprintConflict { .. } => "fingerprint_conflict",
        UhpError::Stream(_) => "stream_interrupted",
    };
    let code = if code.len() <= 128
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        code.to_string()
    } else {
        "server_error".to_string()
    };
    SanitizedExternalError::new(code, "external UHP operation failed")
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ExternalParticipantRecord {
    pub config: ExternalParticipantConfig,
    #[serde(default)]
    pub config_revision: u64,
    #[serde(default)]
    pub state: ExternalParticipantState,
    #[serde(default)]
    pub discovery: Option<Discovery>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub environment_version: Option<u64>,
    #[serde(default)]
    pub retired: bool,
    #[serde(default)]
    pub last_error: Option<SanitizedExternalError>,
    #[serde(default)]
    pub provision_key: Option<String>,
    #[serde(default)]
    pub provision_fingerprint: Option<String>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}

impl ExternalParticipantRecord {
    pub fn new(config: ExternalParticipantConfig) -> Self {
        Self {
            config,
            config_revision: 0,
            state: ExternalParticipantState::Staged,
            discovery: None,
            etag: None,
            environment_version: None,
            retired: false,
            last_error: None,
            provision_key: None,
            provision_fingerprint: None,
            evidence_ids: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSourceFreshness {
    Unknown,
    Fresh,
    Stale,
}

impl Default for ExternalSourceFreshness {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExternalDeliveryRecord {
    pub delivery: UhpDeliveryState,
    #[serde(default)]
    pub retrying: bool,
    #[serde(default)]
    pub source_freshness: ExternalSourceFreshness,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ParticipantEvidence {
    pub participant_id: String,
    pub record: ExternalEvidenceRecord,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ExternalParticipantStateData {
    pub schema_version: u32,
    #[serde(default)]
    pub participants: BTreeMap<String, ExternalParticipantRecord>,
    #[serde(default)]
    pub sessions: BTreeMap<String, UhpSessionBinding>,
    #[serde(default)]
    pub deliveries: BTreeMap<String, ExternalDeliveryRecord>,
    #[serde(default)]
    pub evidence: BTreeMap<String, ParticipantEvidence>,
}

impl Default for ExternalParticipantStateData {
    fn default() -> Self {
        Self {
            schema_version: EXTERNAL_PARTICIPANT_SCHEMA_VERSION,
            participants: BTreeMap::new(),
            sessions: BTreeMap::new(),
            deliveries: BTreeMap::new(),
            evidence: BTreeMap::new(),
        }
    }
}

impl ExternalParticipantStateData {
    pub fn validate(&self) -> Result<(), ExternalParticipantError> {
        if self.schema_version != EXTERNAL_PARTICIPANT_SCHEMA_VERSION {
            return Err(ExternalParticipantError::Storage(
                "unsupported external participant registry schema".to_string(),
            ));
        }
        for (id, record) in &self.participants {
            if id != &record.config.id {
                return Err(ExternalParticipantError::Storage(
                    "participant key does not match its id".to_string(),
                ));
            }
            record.config.validate()?;
            if record.environment_version == Some(0) {
                return Err(ExternalParticipantError::Storage(
                    "environment version must be positive".to_string(),
                ));
            }
        }
        for (id, session) in &self.sessions {
            if (id != &session.session_id
                && id != &external_session_key(&session.participant_id, &session.session_id))
                || !self.participants.contains_key(&session.participant_id)
            {
                return Err(ExternalParticipantError::Storage(
                    "session has an invalid identity or participant".to_string(),
                ));
            }
            session
                .validate()
                .map_err(|error| ExternalParticipantError::Storage(error.to_string()))?;
        }
        for (id, record) in &self.deliveries {
            let delivery = &record.delivery;
            if id != &delivery.delivery_id
                || !self.participants.contains_key(&delivery.recipient_id)
            {
                return Err(ExternalParticipantError::Storage(
                    "delivery has an invalid identity or participant".to_string(),
                ));
            }
            delivery
                .validate()
                .map_err(|error| ExternalParticipantError::Storage(error.to_string()))?;
            if let Some(session_id) = &delivery.session_id {
                let key = external_session_lookup_key(self, &delivery.recipient_id, session_id);
                let session = self.sessions.get(&key).ok_or_else(|| {
                    ExternalParticipantError::Storage(
                        "delivery references an unknown session".to_string(),
                    )
                })?;
                if session.participant_id != delivery.recipient_id {
                    return Err(ExternalParticipantError::Storage(
                        "delivery and session participants differ".to_string(),
                    ));
                }
            }
        }
        for (id, evidence) in &self.evidence {
            if id != &evidence.record.smoke_suite_id
                || !self.participants.contains_key(&evidence.participant_id)
            {
                return Err(ExternalParticipantError::Storage(
                    "evidence has an invalid identity or participant".to_string(),
                ));
            }
            evidence
                .record
                .validate()
                .map_err(|error| ExternalParticipantError::Storage(error.to_string()))?;
        }
        Ok(())
    }
}

pub(crate) fn external_session_key(participant_id: &str, session_id: &str) -> String {
    format!("{participant_id}\u{1f}{session_id}")
}

pub(crate) fn external_session_lookup_key(
    state: &ExternalParticipantStateData,
    participant_id: &str,
    session_id: &str,
) -> String {
    let scoped = external_session_key(participant_id, session_id);
    if state.sessions.contains_key(&scoped) {
        scoped
    } else if state
        .sessions
        .get(session_id)
        .is_some_and(|session| session.participant_id == participant_id)
    {
        session_id.to_string()
    } else {
        scoped
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExternalActionAvailability {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ExternalActionAvailability {
    fn new(enabled: bool, reason: Option<&str>) -> Self {
        Self {
            enabled,
            reason: reason.map(ToString::to_string),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExternalParticipantActions {
    pub discover: ExternalActionAvailability,
    pub test: ExternalActionAvailability,
    pub provision: ExternalActionAvailability,
    pub update: ExternalActionAvailability,
    pub cancel: ExternalActionAvailability,
    pub retry: ExternalActionAvailability,
    pub retire: ExternalActionAvailability,
}

impl ExternalParticipantActions {
    pub fn for_record(record: &ExternalParticipantRecord) -> Self {
        let capabilities = record.discovery.as_ref().map(|value| &value.capabilities);
        let supported = |name| capabilities.is_some_and(|value| value.get(name));
        let has_harness = record.config.harness_id.is_some();
        Self {
            discover: ExternalActionAvailability::new(true, None),
            test: ExternalActionAvailability::new(has_harness, (!has_harness).then_some("configure or provision a harness first")),
            provision: ExternalActionAvailability::new(
                !has_harness && supported("harness_management") && supported("harness_idempotency"),
                (has_harness || !supported("harness_management") || !supported("harness_idempotency"))
                    .then_some("provisioning requires an unprovisioned participant and harness management with idempotency"),
            ),
            update: ExternalActionAvailability::new(
                has_harness && supported("harness_management"),
                (!has_harness || !supported("harness_management")).then_some("remote update requires a harness and harness_management capability"),
            ),
            cancel: ExternalActionAvailability::new(
                supported("cancellation"),
                (!supported("cancellation")).then_some("server does not advertise cancellation"),
            ),
            retry: ExternalActionAvailability::new(
                supported("idempotency"),
                (!supported("idempotency")).then_some("safe retry requires idempotency"),
            ),
            retire: ExternalActionAvailability::new(
                has_harness && supported("harness_management") && !record.retired,
                (!has_harness || !supported("harness_management") || record.retired)
                    .then_some("retirement requires an active harness and harness_management capability"),
            ),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ExternalParticipantRead {
    #[serde(flatten)]
    pub config: ExternalParticipantConfig,
    pub kind: ExternalParticipantKind,
    pub agent_chat_id: Option<String>,
    pub config_revision: u64,
    pub state: ExternalParticipantState,
    pub capabilities: Capabilities,
    pub server_implementation: Option<String>,
    pub server_version: Option<String>,
    pub protocol_version: Option<String>,
    pub environment_version: Option<u64>,
    pub retired: bool,
    pub last_error: Option<SanitizedExternalError>,
    pub evidence_ids: Vec<String>,
    pub actions: ExternalParticipantActions,
}

impl ExternalParticipantRead {
    pub fn from_record(record: &ExternalParticipantRecord) -> Self {
        let mut config = record.config.clone();
        config.writes = false;
        let discovery = record.discovery.as_ref();
        Self {
            config,
            kind: ExternalParticipantKind::External,
            agent_chat_id: None,
            config_revision: record.config_revision,
            state: record.state.clone(),
            capabilities: discovery
                .map(|value| value.capabilities.clone())
                .unwrap_or_default(),
            server_implementation: discovery
                .and_then(|value| value.implementation.as_ref())
                .and_then(|value| value.name.clone()),
            server_version: discovery
                .and_then(|value| value.implementation.as_ref())
                .and_then(|value| value.version.clone()),
            protocol_version: discovery.map(|value| value.default_version.clone()),
            environment_version: record.environment_version,
            retired: record.retired,
            last_error: record.last_error.clone(),
            evidence_ids: record.evidence_ids.clone(),
            actions: ExternalParticipantActions::for_record(record),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ExternalParticipantRegistryRead {
    pub mode: ExternalRegistryMode,
    pub participants: Vec<ExternalParticipantRead>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalDeliveryStatus {
    Queued,
    Busy,
    Retrying,
    Terminal,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExternalDeliveryRead {
    pub delivery_id: String,
    pub room_id: String,
    pub event_id: String,
    pub participant_id: String,
    pub status: ExternalDeliveryStatus,
    pub request_state: UhpDeliveryRequestState,
    pub logical_attempt: u64,
    pub response_id: Option<String>,
    pub session_id: Option<String>,
    pub source_revision: Option<String>,
    pub source_digest: Option<String>,
    pub source_freshness: ExternalSourceFreshness,
    pub environment_version: Option<u64>,
    pub error: Option<SanitizedExternalError>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl ExternalDeliveryRead {
    pub fn from_record(record: &ExternalDeliveryRecord) -> Self {
        let delivery = &record.delivery;
        let status = if record.retrying {
            ExternalDeliveryStatus::Retrying
        } else {
            match &delivery.request_state {
                UhpDeliveryRequestState::Pending => ExternalDeliveryStatus::Queued,
                UhpDeliveryRequestState::InProgress => ExternalDeliveryStatus::Busy,
                UhpDeliveryRequestState::Completed
                | UhpDeliveryRequestState::Failed
                | UhpDeliveryRequestState::Cancelled => ExternalDeliveryStatus::Terminal,
            }
        };
        Self {
            delivery_id: delivery.delivery_id.clone(),
            room_id: delivery.room_id.clone(),
            event_id: delivery.event_id.clone(),
            participant_id: delivery.recipient_id.clone(),
            status,
            request_state: delivery.request_state.clone(),
            logical_attempt: delivery.logical_attempt,
            response_id: delivery.response_id.clone(),
            session_id: delivery.session_id.clone(),
            source_revision: delivery.source_revision.clone(),
            source_digest: delivery.source_digest.clone(),
            source_freshness: record.source_freshness.clone(),
            environment_version: delivery.environment_version,
            error: delivery
                .error_class
                .as_ref()
                .map(|code| SanitizedExternalError::new(code.clone(), "external delivery failed")),
            created_at: delivery.created_at,
            updated_at: delivery.updated_at,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExternalSessionRead {
    pub participant_id: String,
    pub session_id: String,
    pub last_response_id: Option<String>,
    pub last_terminal_status: Option<ResponseStatus>,
    pub busy: bool,
    pub queued: bool,
    pub created_at: u64,
    pub updated_at: u64,
    pub replay_cursor: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ExternalParticipantDetails {
    pub participant: ExternalParticipantRead,
    pub sessions: Vec<ExternalSessionRead>,
    pub deliveries: Vec<ExternalDeliveryRead>,
    pub evidence: Vec<ExternalEvidenceRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExternalDeliveryAuthorization {
    pub participant_id: String,
    pub harness_id: String,
    pub environment_id: String,
    pub environment_version: u64,
    pub writes: bool,
    pub agent_chat_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalRoomDelivery {
    pub delivery: ExternalDeliveryRead,
    pub source: SourceDeliveryVersion,
    pub response_id: Option<String>,
    pub completed_bodies: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExternalRetirementRead {
    pub participant_id: String,
    pub deleted: bool,
    pub sessions: RetiredSessions,
    pub restorable_until: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ExternalCancelTarget {
    Response(String),
    Session(String),
}

pub fn evidence_is_all_pass(record: &ExternalEvidenceRecord) -> bool {
    !record.checks.is_empty()
        && record
            .checks
            .iter()
            .all(|check| check.status == EvidenceCheckStatus::Pass)
}
