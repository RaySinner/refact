use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    Capabilities, ExternalEvidenceRecord, UhpDeliveryRequestState, UhpDeliveryState, UhpError,
    UhpSessionBinding, UHP_VERSION,
};

pub const UHP_BINDINGS_SCHEMA_VERSION: u32 = 1;
pub const MAX_UHP_REPLAY_CURSOR: u64 = 100;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExternalParticipantBinding {
    pub participant_id: String,
    pub server_root: String,
    pub server_implementation: String,
    pub server_version: String,
    pub image_digest: String,
    pub harness_id: String,
    pub base: String,
    pub cli_pin: String,
    pub model: String,
    pub credential_profile_ref: String,
    pub environment_id: String,
    pub environment_version: Option<u64>,
    pub protocol_version: String,
    pub capability_snapshot: Capabilities,
}

impl ExternalParticipantBinding {
    pub fn validate(&self) -> Result<(), UhpError> {
        for (field, value) in [
            ("participant_id", &self.participant_id),
            ("server_root", &self.server_root),
            ("server_implementation", &self.server_implementation),
            ("server_version", &self.server_version),
            ("image_digest", &self.image_digest),
            ("harness_id", &self.harness_id),
            ("base", &self.base),
            ("cli_pin", &self.cli_pin),
            ("model", &self.model),
            ("credential_profile_ref", &self.credential_profile_ref),
            ("environment_id", &self.environment_id),
        ] {
            require_nonempty(field, value)?;
        }
        if self.protocol_version != UHP_VERSION {
            return Err(UhpError::InvalidBinding(format!(
                "unsupported participant protocol version '{}'",
                self.protocol_version
            )));
        }
        let server_root = reqwest::Url::parse(&self.server_root)
            .map_err(|_| UhpError::InvalidBinding("invalid server_root URL".to_string()))?;
        if !server_root.username().is_empty()
            || server_root.password().is_some()
            || server_root.query().is_some()
            || server_root.fragment().is_some()
        {
            return Err(UhpError::InvalidBinding(
                "server_root must not contain credentials, query, or fragment".to_string(),
            ));
        }
        if !self.capability_snapshot.get("environments") {
            return Err(UhpError::CapabilityUnsupported {
                capability: "environments",
                operation: "bind external participant snapshot",
            });
        }
        if self.environment_version == Some(0) {
            return Err(UhpError::InvalidBinding(
                "environment_version must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct UhpBindingRecords {
    pub schema_version: u32,
    pub participants: BTreeMap<String, ExternalParticipantBinding>,
    pub sessions: BTreeMap<String, UhpSessionBinding>,
    pub deliveries: BTreeMap<String, UhpDeliveryState>,
    pub evidence: BTreeMap<String, ExternalEvidenceRecord>,
}

impl UhpBindingRecords {
    pub fn new() -> Self {
        Self {
            schema_version: UHP_BINDINGS_SCHEMA_VERSION,
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), UhpError> {
        if self.schema_version != UHP_BINDINGS_SCHEMA_VERSION {
            return Err(UhpError::InvalidBinding(format!(
                "unsupported bindings schema version {}",
                self.schema_version
            )));
        }
        for (id, participant) in &self.participants {
            if id != &participant.participant_id {
                return Err(UhpError::InvalidBinding(
                    "participant key does not match participant_id".to_string(),
                ));
            }
            participant.validate()?;
        }
        for (id, session) in &self.sessions {
            if id != &session.session_id {
                return Err(UhpError::InvalidBinding(
                    "session key does not match session_id".to_string(),
                ));
            }
            session.validate()?;
            if !self.participants.contains_key(&session.participant_id) {
                return Err(UhpError::InvalidBinding(
                    "session references an unknown participant".to_string(),
                ));
            }
        }
        for (id, delivery) in &self.deliveries {
            if id != &delivery.delivery_id {
                return Err(UhpError::InvalidBinding(
                    "delivery key does not match delivery_id".to_string(),
                ));
            }
            if delivery.previous_response_id.is_some()
                && delivery.previous_response_id == delivery.response_id
            {
                return Err(UhpError::InvalidBinding(
                    "a delivery cannot continue from its own response".to_string(),
                ));
            }
            delivery.validate()?;
            if !self.participants.contains_key(&delivery.recipient_id) {
                return Err(UhpError::InvalidBinding(
                    "delivery references an unknown participant".to_string(),
                ));
            }
            if let Some(session_id) = &delivery.session_id {
                let session = self.sessions.get(session_id).ok_or_else(|| {
                    UhpError::InvalidBinding("delivery references an unknown session".to_string())
                })?;
                if session.participant_id != delivery.recipient_id {
                    return Err(UhpError::InvalidBinding(
                        "delivery and session participants differ".to_string(),
                    ));
                }
            }
        }
        for (id, evidence) in &self.evidence {
            if id != &evidence.smoke_suite_id {
                return Err(UhpError::InvalidBinding(
                    "evidence key does not match smoke_suite_id".to_string(),
                ));
            }
            evidence.validate()?;
        }
        let mut delivery_versions = BTreeSet::new();
        for delivery in self.deliveries.values() {
            let version = delivery.environment_version.ok_or_else(|| {
                UhpError::InvalidBinding("delivery has no environment version".to_string())
            })?;
            if !delivery_versions.insert((delivery.recipient_id.as_str(), version)) {
                return Err(UhpError::InvalidBinding(
                    "separate deliveries cannot reuse an environment version".to_string(),
                ));
            }
        }
        Ok(())
    }
}

impl UhpSessionBinding {
    pub fn validate(&self) -> Result<(), UhpError> {
        require_nonempty("participant_id", &self.participant_id)?;
        require_nonempty("session_id", &self.session_id)?;
        if self
            .replay_cursor
            .is_some_and(|cursor| cursor > MAX_UHP_REPLAY_CURSOR)
        {
            return Err(UhpError::InvalidBinding(format!(
                "replay cursor exceeds {MAX_UHP_REPLAY_CURSOR}"
            )));
        }
        if self.updated_at < self.created_at {
            return Err(UhpError::InvalidBinding(
                "session updated_at precedes created_at".to_string(),
            ));
        }
        Ok(())
    }
}

impl UhpDeliveryState {
    pub fn validate(&self) -> Result<(), UhpError> {
        for (field, value) in [
            ("delivery_id", self.delivery_id.as_str()),
            ("room_id", self.room_id.as_str()),
            ("event_id", self.event_id.as_str()),
            ("recipient_id", self.recipient_id.as_str()),
            ("request_fingerprint", self.request_fingerprint.as_str()),
            ("idempotency_key", self.idempotency_key.as_str()),
        ] {
            require_nonempty(field, value)?;
        }
        let source_revision = self.source_revision.as_deref().ok_or_else(|| {
            UhpError::InvalidBinding("snapshot delivery requires source_revision".to_string())
        })?;
        let source_digest = self.source_digest.as_deref().ok_or_else(|| {
            UhpError::InvalidBinding("snapshot delivery requires source_digest".to_string())
        })?;
        require_nonempty("source_revision", source_revision)?;
        if self.environment_version.is_none_or(|version| version == 0) {
            return Err(UhpError::InvalidBinding(
                "snapshot delivery requires a positive environment_version".to_string(),
            ));
        }
        if source_digest.len() != 64
            || !source_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(UhpError::InvalidBinding(
                "source_digest must be lowercase sha256 hex".to_string(),
            ));
        }
        if self.request_state == UhpDeliveryRequestState::Completed
            && self.response_id.as_deref().is_none_or(str::is_empty)
        {
            return Err(UhpError::InvalidBinding(
                "completed delivery requires response_id".to_string(),
            ));
        }
        if self.updated_at < self.created_at {
            return Err(UhpError::InvalidBinding(
                "delivery updated_at precedes created_at".to_string(),
            ));
        }
        if self.error_class.as_deref().is_some_and(|class| {
            class.is_empty()
                || class.len() > 128
                || !class
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        }) {
            return Err(UhpError::InvalidBinding(
                "error_class must be a short sanitized identifier".to_string(),
            ));
        }
        Ok(())
    }
}

impl ExternalEvidenceRecord {
    pub fn validate(&self) -> Result<(), UhpError> {
        for (field, value) in [
            ("smoke_suite_id", &self.smoke_suite_id),
            ("server_version", &self.server_version),
            ("cli_version", &self.cli_version),
            ("model_version", &self.model_version),
            ("sanitized_report_location", &self.sanitized_report_location),
        ] {
            require_nonempty(field, value)?;
        }
        if self.checks.is_empty()
            || self.checks.iter().any(|check| {
                check.name.trim().is_empty()
                    || check.name.len() > 128
                    || check.name.chars().any(char::is_control)
            })
            || !valid_report_location(&self.sanitized_report_location)
        {
            return Err(UhpError::InvalidBinding(
                "evidence requires named checks and a sanitized report location".to_string(),
            ));
        }
        Ok(())
    }
}

fn valid_report_location(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['\n', '\r', '\0', '\\', ':', '?', '#'])
        && !value.starts_with('/')
        && value
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn require_nonempty(field: &str, value: &str) -> Result<(), UhpError> {
    if value.trim().is_empty() {
        return Err(UhpError::InvalidBinding(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}
