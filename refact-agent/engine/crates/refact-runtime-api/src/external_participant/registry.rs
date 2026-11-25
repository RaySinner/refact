use std::sync::Arc;

use refact_uhp::{
    CreateResponse, Discovery, ExternalEvidenceRecord, HarnessConfig, IdempotencyKey,
    RecoveryBinding, RequestScope, ResponseStatus, UhpDeliveryRequestState, UhpSessionBinding,
};

use super::*;

pub struct ExternalParticipantRegistry {
    store: ExternalParticipantStore,
    transport: Arc<dyn ExternalParticipantTransport>,
    operations: tokio::sync::Mutex<()>,
}

impl ExternalParticipantRegistry {
    pub fn new(config_dir: impl Into<std::path::PathBuf>) -> Self {
        Self::with_transport(config_dir, Arc::new(UhpParticipantTransport))
    }

    pub fn with_transport(
        config_dir: impl Into<std::path::PathBuf>,
        transport: Arc<dyn ExternalParticipantTransport>,
    ) -> Self {
        Self {
            store: ExternalParticipantStore::new(config_dir),
            transport,
            operations: tokio::sync::Mutex::new(()),
        }
    }

    pub fn snapshot(&self) -> Result<ExternalParticipantRegistryRead, ExternalParticipantError> {
        let state = self.store.load()?;
        let participants = state
            .participants
            .values()
            .map(ExternalParticipantRead::from_record)
            .collect::<Vec<_>>();
        Ok(ExternalParticipantRegistryRead {
            mode: if participants.is_empty() {
                ExternalRegistryMode::InternalAgents
            } else {
                ExternalRegistryMode::ExternalParticipants
            },
            participants,
        })
    }

    pub fn participant(
        &self,
        id: &str,
    ) -> Result<ExternalParticipantRead, ExternalParticipantError> {
        let state = self.store.load()?;
        let record = state.participants.get(id).ok_or_else(|| not_found(id))?;
        Ok(ExternalParticipantRead::from_record(record))
    }

    pub fn details(
        &self,
        id: &str,
    ) -> Result<ExternalParticipantDetails, ExternalParticipantError> {
        let state = self.store.load()?;
        let participant = state.participants.get(id).ok_or_else(|| not_found(id))?;
        let deliveries = state
            .deliveries
            .values()
            .filter(|record| record.delivery.recipient_id == id)
            .map(ExternalDeliveryRead::from_record)
            .collect::<Vec<_>>();
        let sessions = state
            .sessions
            .values()
            .filter(|session| session.participant_id == id)
            .map(|session| ExternalSessionRead {
                participant_id: session.participant_id.clone(),
                session_id: session.session_id.clone(),
                last_response_id: session.last_response_id.clone(),
                last_terminal_status: session.last_terminal_status.clone(),
                busy: deliveries.iter().any(|delivery| {
                    delivery.session_id.as_deref() == Some(session.session_id.as_str())
                        && matches!(
                            &delivery.status,
                            ExternalDeliveryStatus::Busy | ExternalDeliveryStatus::Retrying
                        )
                }),
                queued: deliveries.iter().any(|delivery| {
                    delivery.session_id.as_deref() == Some(session.session_id.as_str())
                        && delivery.status == ExternalDeliveryStatus::Queued
                }),
                created_at: session.created_at,
                updated_at: session.updated_at,
                replay_cursor: session.replay_cursor,
            })
            .collect();
        let evidence = state
            .evidence
            .values()
            .filter(|evidence| evidence.participant_id == id)
            .map(|evidence| evidence.record.clone())
            .collect();
        Ok(ExternalParticipantDetails {
            participant: ExternalParticipantRead::from_record(participant),
            sessions,
            deliveries,
            evidence,
        })
    }

    pub async fn create(
        &self,
        config: ExternalParticipantConfig,
    ) -> Result<ExternalParticipantRead, ExternalParticipantError> {
        config.validate()?;
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        if state.participants.contains_key(&config.id) {
            return Err(ExternalParticipantError::AlreadyExists(config.id));
        }
        let record = ExternalParticipantRecord::new(config);
        let read = ExternalParticipantRead::from_record(&record);
        state.participants.insert(read.config.id.clone(), record);
        self.store.save(&state)?;
        Ok(read)
    }

    pub async fn update_configuration(
        &self,
        id: &str,
        config: ExternalParticipantConfig,
    ) -> Result<ExternalParticipantRead, ExternalParticipantError> {
        config.validate()?;
        if config.id != id {
            return Err(ExternalParticipantError::InvalidConfiguration(
                "participant id is immutable",
            ));
        }
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        let current = state
            .participants
            .get(id)
            .cloned()
            .ok_or_else(|| not_found(id))?;
        if current.config.harness_id.is_some()
            && (current.config.base != config.base
                || current.config.harness_id != config.harness_id)
        {
            return Err(ExternalParticipantError::InvalidConfiguration(
                "base and harness_id are immutable after provisioning; create a replacement participant",
            ));
        }
        let mut record = current.clone();
        record.config = config;
        record.config_revision = current.config_revision.saturating_add(1);
        record.state = if record.discovery.is_some() {
            ExternalParticipantState::Unverified
        } else {
            ExternalParticipantState::Staged
        };
        record.last_error = None;
        if record.config.harness_id.is_none() {
            record.provision_key = None;
            record.provision_fingerprint = None;
        }
        let read = ExternalParticipantRead::from_record(&record);
        state.participants.insert(id.to_string(), record);
        self.store.save(&state)?;
        Ok(read)
    }

    pub async fn remove(&self, id: &str) -> Result<(), ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        let record = state.participants.get(id).ok_or_else(|| not_found(id))?;
        if record.config.harness_id.is_some()
            || state
                .sessions
                .values()
                .any(|session| session.participant_id == id)
            || state
                .deliveries
                .values()
                .any(|delivery| delivery.delivery.recipient_id == id)
            || state
                .evidence
                .values()
                .any(|evidence| evidence.participant_id == id)
        {
            return Err(ExternalParticipantError::InvalidOperation(
                "retire provisioned participants and preserve sessions, deliveries, and evidence",
            ));
        }
        state.participants.remove(id);
        self.store.save(&state)
    }

    pub async fn write_credential(
        &self,
        id: &str,
        bearer: &str,
    ) -> Result<(), ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let state = self.store.load()?;
        let record = state.participants.get(id).ok_or_else(|| not_found(id))?;
        self.store
            .write_bearer(&record.config.credential_profile_ref, bearer)
    }

    pub async fn discover(
        &self,
        id: &str,
    ) -> Result<ExternalParticipantRead, ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        let record = state
            .participants
            .get(id)
            .cloned()
            .ok_or_else(|| not_found(id))?;
        let connection = self.connection(&record.config)?;
        match self.transport.discover(&connection).await {
            Ok(discovery) => {
                let mut updated = record;
                updated.discovery = Some(discovery);
                updated.state = ExternalParticipantState::Unverified;
                updated.last_error = None;
                let read = ExternalParticipantRead::from_record(&updated);
                state.participants.insert(id.to_string(), updated);
                self.store.save(&state)?;
                Ok(read)
            }
            Err(error) => Err(self.transport_failure(&mut state, id, error)?),
        }
    }

    pub async fn test_model(
        &self,
        id: &str,
    ) -> Result<ExternalParticipantRead, ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        let record = state
            .participants
            .get(id)
            .cloned()
            .ok_or_else(|| not_found(id))?;
        let harness_id =
            record
                .config
                .harness_id
                .clone()
                .ok_or(ExternalParticipantError::InvalidOperation(
                    "configure or provision a harness before testing its model",
                ))?;
        let connection = self.connection(&record.config)?;
        match self
            .transport
            .test_model(&connection, &harness_id, &record.config.model)
            .await
        {
            Ok(model) if model.available && model.id == record.config.model => {
                let mut updated = record;
                if updated.state != ExternalParticipantState::Verified {
                    updated.state = ExternalParticipantState::Unverified;
                }
                updated.last_error = None;
                let read = ExternalParticipantRead::from_record(&updated);
                state.participants.insert(id.to_string(), updated);
                self.store.save(&state)?;
                Ok(read)
            }
            Ok(_) => {
                let error = ExternalParticipantError::InvalidOperation(
                    "the selected model is unavailable or the server returned a different model",
                );
                self.record_failure(&mut state, id, &error)?;
                Err(error)
            }
            Err(error) => Err(self.transport_failure(&mut state, id, error)?),
        }
    }

    pub async fn provision(
        &self,
        id: &str,
        idempotency_key: &str,
    ) -> Result<ExternalParticipantRead, ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        let record = state
            .participants
            .get(id)
            .cloned()
            .ok_or_else(|| not_found(id))?;
        let key = IdempotencyKey::new(idempotency_key)
            .map_err(|error| ExternalParticipantError::Transport(sanitize_uhp_error(&error)))?;
        let scope = request_scope(&record.config);
        let harness_config = harness_config(&record.config);
        let fingerprint = harness_config
            .fingerprint(&scope)
            .map_err(|error| ExternalParticipantError::Transport(sanitize_uhp_error(&error)))?;
        if record.provision_key.as_deref() == Some(idempotency_key)
            && record.provision_fingerprint.as_deref() != Some(fingerprint.as_str())
        {
            return Err(ExternalParticipantError::ProvisionConflict);
        }
        if record.config.harness_id.is_some() {
            if record.provision_key.as_deref() == Some(idempotency_key) {
                return Ok(ExternalParticipantRead::from_record(&record));
            }
            return Err(ExternalParticipantError::InvalidOperation(
                "use remote update for an already provisioned harness",
            ));
        }
        let discovery = require_discovery(&record)?;
        require_capability(&record, "harness_management", "provision")?;
        require_capability(&record, "harness_idempotency", "provision")?;
        let connection = self.connection(&record.config)?;
        match self
            .transport
            .provision(&connection, discovery, &harness_config, &key, &scope)
            .await
        {
            Ok(harness) if !harness.id.is_empty() && harness.base == record.config.base => {
                let mut updated = record;
                updated.config.harness_id = Some(harness.id);
                updated.config_revision = updated.config_revision.saturating_add(1);
                updated.state = ExternalParticipantState::Unverified;
                updated.last_error = None;
                updated.provision_key = Some(idempotency_key.to_string());
                updated.provision_fingerprint = Some(fingerprint.to_string());
                let read = ExternalParticipantRead::from_record(&updated);
                state.participants.insert(id.to_string(), updated);
                self.store.save(&state)?;
                Ok(read)
            }
            Ok(_) => {
                let error = ExternalParticipantError::InvalidOperation(
                    "server returned an invalid harness identity or changed the opaque base",
                );
                self.record_failure(&mut state, id, &error)?;
                Err(error)
            }
            Err(error) => Err(self.transport_failure(&mut state, id, error)?),
        }
    }

    pub async fn update_remote(
        &self,
        id: &str,
    ) -> Result<ExternalParticipantRead, ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        let record = state
            .participants
            .get(id)
            .cloned()
            .ok_or_else(|| not_found(id))?;
        let discovery = require_discovery(&record)?;
        require_capability(&record, "harness_management", "update")?;
        let harness_id =
            record
                .config
                .harness_id
                .clone()
                .ok_or(ExternalParticipantError::InvalidOperation(
                    "provision a harness before remote update",
                ))?;
        let connection = self.connection(&record.config)?;
        let config = harness_config(&record.config);
        match self
            .transport
            .update(
                &connection,
                discovery,
                &harness_id,
                &config,
                record.etag.as_deref(),
            )
            .await
        {
            Ok(versioned)
                if versioned.harness.id == harness_id
                    && versioned.harness.base == record.config.base =>
            {
                let mut updated = record;
                updated.etag = versioned.etag;
                updated.config_revision = updated.config_revision.saturating_add(1);
                updated.state = ExternalParticipantState::Unverified;
                updated.last_error = None;
                let read = ExternalParticipantRead::from_record(&updated);
                state.participants.insert(id.to_string(), updated);
                self.store.save(&state)?;
                Ok(read)
            }
            Ok(_) => {
                let error = ExternalParticipantError::InvalidOperation(
                    "server returned a mismatched harness update",
                );
                self.record_failure(&mut state, id, &error)?;
                Err(error)
            }
            Err(error) => Err(self.transport_failure(&mut state, id, error)?),
        }
    }

    pub async fn upsert_session(
        &self,
        session: UhpSessionBinding,
    ) -> Result<(), ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        if !state.participants.contains_key(&session.participant_id) {
            return Err(not_found(&session.participant_id));
        }
        state.sessions.insert(session.session_id.clone(), session);
        self.store.save(&state)
    }

    pub async fn upsert_delivery(
        &self,
        record: ExternalDeliveryRecord,
    ) -> Result<(), ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        if !state
            .participants
            .contains_key(&record.delivery.recipient_id)
        {
            return Err(not_found(&record.delivery.recipient_id));
        }
        state
            .deliveries
            .insert(record.delivery.delivery_id.clone(), record);
        self.store.save(&state)
    }

    pub async fn set_environment_version(
        &self,
        id: &str,
        version: u64,
    ) -> Result<(), ExternalParticipantError> {
        if version == 0 {
            return Err(ExternalParticipantError::InvalidConfiguration(
                "environment version must be positive",
            ));
        }
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        let record = state
            .participants
            .get_mut(id)
            .ok_or_else(|| not_found(id))?;
        if record.config.environment_id.is_none() {
            return Err(ExternalParticipantError::InvalidOperation(
                "configure an environment before setting its version",
            ));
        }
        record.environment_version = Some(version);
        self.store.save(&state)
    }

    pub async fn record_live_evidence(
        &self,
        id: &str,
        evidence: ExternalEvidenceRecord,
    ) -> Result<ExternalParticipantRead, ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        let mut record = state
            .participants
            .get(id)
            .cloned()
            .ok_or_else(|| not_found(id))?;
        evidence.validate().map_err(|_| {
            ExternalParticipantError::InvalidConfiguration("invalid live evidence record")
        })?;
        let discovered_version = record
            .discovery
            .as_ref()
            .and_then(|discovery| discovery.implementation.as_ref())
            .and_then(|implementation| implementation.version.as_deref());
        if !evidence_is_all_pass(&evidence)
            || discovered_version != Some(evidence.server_version.as_str())
            || evidence.cli_version != record.config.cli_pin
            || evidence.model_version != record.config.model
        {
            return Err(ExternalParticipantError::InvalidOperation(
                "live evidence must be all-pass and match the discovered server, CLI pin, and selected model",
            ));
        }
        if state.evidence.contains_key(&evidence.smoke_suite_id) {
            return Err(ExternalParticipantError::AlreadyExists(
                evidence.smoke_suite_id,
            ));
        }
        record.state = ExternalParticipantState::Verified;
        record.last_error = None;
        record.evidence_ids.push(evidence.smoke_suite_id.clone());
        state.evidence.insert(
            evidence.smoke_suite_id.clone(),
            ParticipantEvidence {
                participant_id: id.to_string(),
                record: evidence,
            },
        );
        let read = ExternalParticipantRead::from_record(&record);
        state.participants.insert(id.to_string(), record);
        self.store.save(&state)?;
        Ok(read)
    }

    pub fn authorize_delivery(
        &self,
        id: &str,
    ) -> Result<ExternalDeliveryAuthorization, ExternalParticipantError> {
        let state = self.store.load()?;
        let record = state.participants.get(id).ok_or_else(|| not_found(id))?;
        if !record.config.enabled
            || record.retired
            || record.state != ExternalParticipantState::Verified
        {
            return Err(ExternalParticipantError::NotVerified);
        }
        require_capability(record, "idempotency", "delivery")?;
        require_capability(record, "session_listing", "delivery")?;
        require_capability(record, "environments", "delivery")?;
        require_capability(record, "files_output", "delivery")?;
        Ok(ExternalDeliveryAuthorization {
            participant_id: id.to_string(),
            harness_id: record
                .config
                .harness_id
                .clone()
                .ok_or(ExternalParticipantError::NotVerified)?,
            environment_id: record
                .config
                .environment_id
                .clone()
                .ok_or(ExternalParticipantError::NotVerified)?,
            environment_version: record
                .environment_version
                .ok_or(ExternalParticipantError::NotVerified)?,
            writes: false,
            agent_chat_id: None,
        })
    }

    pub async fn cancel(
        &self,
        id: &str,
        target: ExternalCancelTarget,
    ) -> Result<(), ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        let record = state
            .participants
            .get(id)
            .cloned()
            .ok_or_else(|| not_found(id))?;
        require_capability(&record, "cancellation", "cancel")?;
        match &target {
            ExternalCancelTarget::Response(response_id) => {
                if !state.deliveries.values().any(|delivery| {
                    delivery.delivery.recipient_id == id
                        && delivery.delivery.response_id.as_deref() == Some(response_id.as_str())
                }) {
                    return Err(not_found(response_id));
                }
            }
            ExternalCancelTarget::Session(session_id) => {
                if state
                    .sessions
                    .get(session_id)
                    .is_none_or(|session| session.participant_id != id)
                {
                    return Err(not_found(session_id));
                }
            }
        }
        let connection = self.connection(&record.config)?;
        match self.transport.cancel(&connection, &target).await {
            Ok(response) => {
                if let Some(response) = response {
                    apply_response(&mut state, id, None, &response);
                } else if let ExternalCancelTarget::Session(session_id) = target {
                    for record in state.deliveries.values_mut() {
                        let record: &mut ExternalDeliveryRecord = record;
                        if record.delivery.recipient_id != id
                            || record.delivery.session_id.as_deref() != Some(session_id.as_str())
                        {
                            continue;
                        }
                        if matches!(
                            &record.delivery.request_state,
                            UhpDeliveryRequestState::Pending | UhpDeliveryRequestState::InProgress
                        ) {
                            record.delivery.request_state = UhpDeliveryRequestState::Cancelled;
                        }
                        record.retrying = false;
                    }
                    if let Some(session) = state.sessions.get_mut(&session_id) {
                        session.last_terminal_status = Some(ResponseStatus::Cancelled);
                    }
                }
                self.store.save(&state)
            }
            Err(error) => Err(ExternalParticipantError::Transport(sanitize_uhp_error(
                &error,
            ))),
        }
    }

    pub async fn retry_delivery(
        &self,
        id: &str,
        delivery_id: &str,
        request: &CreateResponse,
    ) -> Result<ExternalDeliveryRead, ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        let participant = state
            .participants
            .get(id)
            .cloned()
            .ok_or_else(|| not_found(id))?;
        require_capability(&participant, "idempotency", "retry")?;
        let record = state
            .deliveries
            .get(delivery_id)
            .cloned()
            .ok_or_else(|| not_found(delivery_id))?;
        if record.delivery.recipient_id != id
            || !matches!(
                &record.delivery.request_state,
                UhpDeliveryRequestState::Failed | UhpDeliveryRequestState::Cancelled
            )
        {
            return Err(ExternalParticipantError::InvalidOperation(
                "only a failed or cancelled delivery owned by the participant can be retried",
            ));
        }
        let scope = request_scope(&participant.config);
        let actual = request
            .fingerprint(&scope)
            .map_err(|error| ExternalParticipantError::Transport(sanitize_uhp_error(&error)))?;
        if actual != record.delivery.request_fingerprint {
            return Err(ExternalParticipantError::InvalidOperation(
                "retry request does not match the persisted delivery fingerprint",
            ));
        }
        state
            .deliveries
            .get_mut(delivery_id)
            .expect("delivery was loaded")
            .retrying = true;
        self.store.save(&state)?;
        let connection = self.connection(&participant.config)?;
        let binding = RecoveryBinding {
            request_fingerprint: record.delivery.request_fingerprint.clone(),
            response_id: record.delivery.response_id.clone(),
        };
        match self
            .transport
            .retry(
                &connection,
                request,
                &record.delivery.idempotency_key,
                &scope,
                &binding,
            )
            .await
        {
            Ok(response) => {
                apply_response(&mut state, id, Some(delivery_id), &response);
                self.store.save(&state)?;
                let record = state
                    .deliveries
                    .get(delivery_id)
                    .expect("retried delivery remains stored");
                Ok(ExternalDeliveryRead::from_record(record))
            }
            Err(error) => {
                let sanitized = sanitize_uhp_error(&error);
                let record = state
                    .deliveries
                    .get_mut(delivery_id)
                    .expect("delivery was loaded");
                record.retrying = false;
                record.delivery.request_state = UhpDeliveryRequestState::Failed;
                record.delivery.error_class = Some(sanitized.code.clone());
                self.store.save(&state)?;
                Err(ExternalParticipantError::Transport(sanitized))
            }
        }
    }

    pub async fn retire(
        &self,
        id: &str,
    ) -> Result<ExternalRetirementRead, ExternalParticipantError> {
        let _operation = self.operations.lock().await;
        let mut state = self.store.load()?;
        let record = state
            .participants
            .get(id)
            .cloned()
            .ok_or_else(|| not_found(id))?;
        let discovery = require_discovery(&record)?;
        require_capability(&record, "harness_management", "retire")?;
        if record.retired {
            return Err(ExternalParticipantError::InvalidOperation(
                "participant is already retired",
            ));
        }
        let harness_id =
            record
                .config
                .harness_id
                .clone()
                .ok_or(ExternalParticipantError::InvalidOperation(
                    "participant has no provisioned harness",
                ))?;
        let connection = self.connection(&record.config)?;
        match self
            .transport
            .retire(&connection, discovery, &harness_id)
            .await
        {
            Ok(retirement) => {
                let updated = state
                    .participants
                    .get_mut(id)
                    .expect("participant was loaded");
                updated.config.enabled = false;
                updated.retired = true;
                updated.state = ExternalParticipantState::Unverified;
                updated.last_error = None;
                self.store.save(&state)?;
                Ok(ExternalRetirementRead {
                    participant_id: id.to_string(),
                    deleted: retirement.deleted,
                    sessions: retirement.sessions,
                    restorable_until: retirement.restorable_until,
                })
            }
            Err(error) => Err(self.transport_failure(&mut state, id, error)?),
        }
    }

    fn connection(
        &self,
        config: &ExternalParticipantConfig,
    ) -> Result<ExternalConnection, ExternalParticipantError> {
        let bearer = self.store.bearer(&config.credential_profile_ref)?;
        Ok(ExternalConnection::new(config.server_root.clone(), bearer))
    }

    fn transport_failure(
        &self,
        state: &mut ExternalParticipantStateData,
        id: &str,
        error: refact_uhp::UhpError,
    ) -> Result<ExternalParticipantError, ExternalParticipantError> {
        let error = ExternalParticipantError::Transport(sanitize_uhp_error(&error));
        self.record_failure(state, id, &error)?;
        Ok(error)
    }

    fn record_failure(
        &self,
        state: &mut ExternalParticipantStateData,
        id: &str,
        error: &ExternalParticipantError,
    ) -> Result<(), ExternalParticipantError> {
        let record = state
            .participants
            .get_mut(id)
            .ok_or_else(|| not_found(id))?;
        record.state = ExternalParticipantState::Failed;
        record.last_error = Some(error.sanitized());
        self.store.save(state)
    }
}

fn not_found(id: &str) -> ExternalParticipantError {
    ExternalParticipantError::NotFound(id.to_string())
}

fn request_scope(config: &ExternalParticipantConfig) -> RequestScope {
    RequestScope::new(config.id.clone(), config.credential_profile_ref.clone())
}

fn harness_config(config: &ExternalParticipantConfig) -> HarnessConfig {
    HarnessConfig {
        name: Some(config.name.clone()),
        base: config.base.clone(),
        default_model: Some(config.model.clone()),
        system_prompt: Some(config.prompt.clone()),
        environment: config.environment_id.clone(),
        disabled_tools: config.disabled_tools.clone(),
        max_step: config.max_step,
        timeout_seconds: config.timeout_seconds,
        ..HarnessConfig::default()
    }
}

fn require_discovery(
    record: &ExternalParticipantRecord,
) -> Result<&Discovery, ExternalParticipantError> {
    record
        .discovery
        .as_ref()
        .ok_or(ExternalParticipantError::InvalidOperation(
            "run discovery before this action",
        ))
}

fn require_capability(
    record: &ExternalParticipantRecord,
    capability: &'static str,
    action: &'static str,
) -> Result<(), ExternalParticipantError> {
    if require_discovery(record)?.capability(capability) {
        Ok(())
    } else {
        Err(ExternalParticipantError::CapabilityUnsupported { capability, action })
    }
}

fn apply_response(
    state: &mut ExternalParticipantStateData,
    participant_id: &str,
    delivery_id: Option<&str>,
    response: &refact_uhp::Response,
) {
    let request_state = match &response.status {
        ResponseStatus::InProgress => UhpDeliveryRequestState::InProgress,
        ResponseStatus::Completed => UhpDeliveryRequestState::Completed,
        ResponseStatus::Cancelled => UhpDeliveryRequestState::Cancelled,
        ResponseStatus::Failed | ResponseStatus::Incomplete | ResponseStatus::Unknown => {
            UhpDeliveryRequestState::Failed
        }
    };
    let owned_delivery_id = delivery_id.map(ToString::to_string).or_else(|| {
        state
            .deliveries
            .iter()
            .find(|(_, record)| {
                record.delivery.recipient_id == participant_id
                    && record.delivery.response_id.as_deref() == Some(response.id.as_str())
            })
            .map(|(id, _)| id.clone())
    });
    if let Some(delivery_id) = owned_delivery_id {
        if let Some(record) = state.deliveries.get_mut(&delivery_id) {
            record.retrying = false;
            record.delivery.request_state = request_state.clone();
            record.delivery.response_id = Some(response.id.clone());
            record.delivery.session_id = response
                .metadata
                .session_id
                .clone()
                .or_else(|| record.delivery.session_id.clone());
            record.delivery.updated_at = record.delivery.updated_at.max(response.created_at);
            record.delivery.error_class = match &response.status {
                ResponseStatus::Failed | ResponseStatus::Incomplete | ResponseStatus::Unknown => {
                    Some("response_failed".to_string())
                }
                _ => None,
            };
        }
    }
    if let Some(session_id) = &response.metadata.session_id {
        let terminal = matches!(
            &response.status,
            ResponseStatus::Completed
                | ResponseStatus::Failed
                | ResponseStatus::Incomplete
                | ResponseStatus::Cancelled
        )
        .then(|| response.status.clone());
        let session =
            state
                .sessions
                .entry(session_id.clone())
                .or_insert_with(|| UhpSessionBinding {
                    participant_id: participant_id.to_string(),
                    session_id: session_id.clone(),
                    last_response_id: None,
                    last_terminal_status: None,
                    created_at: response.created_at,
                    updated_at: response.created_at,
                    replay_cursor: None,
                });
        session.last_response_id = Some(response.id.clone());
        session.last_terminal_status = terminal.or_else(|| session.last_terminal_status.clone());
        session.updated_at = session.updated_at.max(response.created_at);
    }
}
