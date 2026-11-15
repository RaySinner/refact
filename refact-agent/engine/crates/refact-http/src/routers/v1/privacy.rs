use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use hyper::StatusCode;
use refact_privacy::{
    Destination, DestinationId, DestinationKind, FileRecord, PrivacyAuditError, PrivacyAudited,
    PrivacyPolicy,
};
use serde::{Deserialize, Serialize};

use refact_runtime_api::app_state::AppState;
use refact_core::call_validation::ChatMessage;
use refact_core::custom_error::ScratchError;
use refact_worktrees::path_mappings::registered_worktree_path_mappings;
use refact_runtime_api::files_in_workspace::strictest_zone_for_path;
use refact_privacy::DerivedPrivacyZones;

#[derive(Debug, Serialize)]
pub struct PrivacyPolicyResponse {
    pub policy: PrivacyPolicy,
    pub destinations: Vec<Destination>,
    pub match_counts: BTreeMap<String, usize>,
    pub discovery_deferred: bool,
    pub error: Option<String>,
    pub source_paths: Vec<String>,
    pub has_project_overrides: bool,
}

#[derive(Debug, Serialize)]
pub struct PrivacyObservationCapability {
    pub platform_supported: bool,
    pub runtime_available: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PrivacyStatusResponse {
    pub platform: String,
    pub observation: PrivacyObservationCapability,
    pub config_error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PrivacyInspectRequest {
    pub chat_id: String,
    pub destination: Destination,
}

#[derive(Debug, Serialize)]
pub struct PrivacyBlockedRecord {
    pub record_index: usize,
    pub record: FileRecord,
}

#[derive(Debug, Serialize)]
pub struct PrivacyInspectResponse {
    pub chat_id: String,
    pub destination: Destination,
    pub sendable: bool,
    pub would_send: Vec<ChatMessage>,
    pub records: Vec<FileRecord>,
    pub blocked: Vec<PrivacyBlockedRecord>,
    pub refusal: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PrivacyDerivedRequest {
    pub chat_id: String,
}

#[derive(Debug, Serialize)]
pub struct PrivacyDerivedEntry {
    pub path: String,
    pub zone: String,
    pub origin: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PrivacyDerivedResponse {
    pub chat_id: String,
    pub entries: Vec<PrivacyDerivedEntry>,
}

#[derive(Debug, Deserialize)]
pub struct PrivacyDerivedClearRequest {
    pub chat_id: String,
    #[serde(default)]
    pub paths: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct PrivacyDerivedClearResponse {
    pub cleared: usize,
    pub remaining: usize,
}

struct AuditedMessages {
    records: Result<Vec<(usize, FileRecord)>, PrivacyAuditError>,
}

impl PrivacyAudited for AuditedMessages {
    fn privacy_records(&self) -> Result<Vec<(usize, FileRecord)>, PrivacyAuditError> {
        self.records.clone()
    }
}

pub async fn handle_v1_privacy_policy_get(
    State(app): State<AppState>,
) -> Result<Json<PrivacyPolicyResponse>, ScratchError> {
    refact_runtime_api::privacy::load_privacy_if_needed(app.gcx.clone()).await;
    Ok(Json(build_policy_response(&app).await?))
}

pub async fn handle_v1_privacy_policy_post(
    State(app): State<AppState>,
    Json(policy): Json<PrivacyPolicy>,
) -> Result<Json<PrivacyPolicyResponse>, ScratchError> {
    policy.compile().map_err(|error| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Invalid privacy policy: {error}"),
        )
    })?;
    refact_runtime_api::privacy::save_privacy_policy(app.gcx.clone(), policy)
        .await
        .map_err(|error| ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(build_saved_policy_response(&app).await))
}

pub async fn handle_v1_privacy_status(State(app): State<AppState>) -> Json<PrivacyStatusResponse> {
    refact_runtime_api::privacy::load_privacy_if_needed(app.gcx.clone()).await;
    let config_error = app.gcx.privacy_policy_load.read().unwrap().error.clone();
    let runtime = app.gcx.privacy_observation_runtime.read().unwrap().clone();
    Json(PrivacyStatusResponse {
        platform: std::env::consts::OS.to_string(),
        observation: observation_capability(runtime),
        config_error,
    })
}

pub async fn handle_v1_privacy_inspect(
    State(app): State<AppState>,
    Json(request): Json<PrivacyInspectRequest>,
) -> Result<Json<PrivacyInspectResponse>, ScratchError> {
    refact_runtime_api::privacy::load_privacy_if_needed(app.gcx.clone()).await;
    let session = {
        let sessions = app.gcx.chat_sessions.read().await;
        sessions.get(&request.chat_id).cloned()
    }
    .ok_or_else(|| {
        ScratchError::new(StatusCode::NOT_FOUND, "Chat session not found".to_string())
    })?;
    let messages = Arc::new(session.lock().await.messages.clone());
    let indexed_records = refact_privacy::records_from_messages(&messages);
    let records = indexed_records
        .as_ref()
        .map(|records| records.iter().map(|(_, record)| record.clone()).collect())
        .unwrap_or_default();
    let policy = app.gcx.privacy_policy_load.read().unwrap().policy.clone();
    let compiled = policy.compile().map_err(|error| {
        ScratchError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to compile privacy policy: {error}"),
        )
    })?;
    let clearance = refact_privacy::clear(
        AuditedMessages {
            records: indexed_records,
        },
        &request.destination,
        &compiled,
    );
    let sendable_messages = messages
        .iter()
        .cloned()
        .map(|mut message| {
            message.extra.remove("privacy");
            message
        })
        .collect::<Vec<_>>();
    let (sendable, would_send, blocked, refusal) = match clearance {
        Ok(_) => (true, sendable_messages, Vec::new(), None),
        Err(refusal) => (
            false,
            Vec::new(),
            refusal
                .offending
                .into_iter()
                .map(|(record_index, record)| PrivacyBlockedRecord {
                    record_index,
                    record,
                })
                .collect(),
            Some(refusal.message),
        ),
    };

    Ok(Json(PrivacyInspectResponse {
        chat_id: request.chat_id,
        destination: request.destination,
        sendable,
        would_send,
        records,
        blocked,
        refusal,
    }))
}

pub async fn handle_v1_privacy_derived(
    State(app): State<AppState>,
    Json(request): Json<PrivacyDerivedRequest>,
) -> Result<Json<PrivacyDerivedResponse>, ScratchError> {
    let derived_zones = session_derived_zones(&app, &request.chat_id).await?;
    let mut entries = {
        let derived = derived_zones
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        derived
            .iter()
            .map(|(path, derived)| PrivacyDerivedEntry {
                path: path.to_string_lossy().into_owned(),
                zone: derived.zone.clone(),
                origin: derived.origin.clone(),
            })
            .collect::<Vec<_>>()
    };
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Json(PrivacyDerivedResponse {
        chat_id: request.chat_id,
        entries,
    }))
}

pub async fn handle_v1_privacy_derived_clear(
    State(app): State<AppState>,
    Json(request): Json<PrivacyDerivedClearRequest>,
) -> Result<Json<PrivacyDerivedClearResponse>, ScratchError> {
    let derived_zones = session_derived_zones(&app, &request.chat_id).await?;
    let requested = request.paths.unwrap_or_default();
    let mut derived = derived_zones
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = derived.len();
    if requested.is_empty() {
        derived.clear();
    } else {
        for path in requested {
            derived.remove(std::path::Path::new(path.as_str()));
        }
    }
    let remaining = derived.len();
    Ok(Json(PrivacyDerivedClearResponse {
        cleared: before.saturating_sub(remaining),
        remaining,
    }))
}

async fn session_derived_zones(
    app: &AppState,
    chat_id: &str,
) -> Result<DerivedPrivacyZones, ScratchError> {
    let session = {
        let sessions = app.gcx.chat_sessions.read().await;
        sessions.get(chat_id).cloned()
    }
    .ok_or_else(|| {
        ScratchError::new(StatusCode::NOT_FOUND, "Chat session not found".to_string())
    })?;
    let derived_zones = session.lock().await.derived_privacy_zones.clone();
    Ok(derived_zones)
}

async fn build_policy_response(app: &AppState) -> Result<PrivacyPolicyResponse, ScratchError> {
    let load = app.gcx.privacy_policy_load.read().unwrap().clone();
    let effective = load.policy.as_ref().clone();
    let policy = refact_runtime_api::privacy::global_only_policy(app.gcx.clone())
        .await
        .unwrap_or_else(|| effective.clone());
    let destinations = collect_destinations(app, &effective).await;
    let match_counts = live_match_counts(app, &effective)?;
    Ok(policy_response(
        policy,
        effective,
        load,
        destinations,
        match_counts,
        false,
    ))
}

async fn build_saved_policy_response(app: &AppState) -> PrivacyPolicyResponse {
    let load = app.gcx.privacy_policy_load.read().unwrap().clone();
    let effective = load.policy.as_ref().clone();
    let policy = refact_runtime_api::privacy::global_only_policy(app.gcx.clone())
        .await
        .unwrap_or_else(|| effective.clone());
    policy_response(policy, effective, load, Vec::new(), BTreeMap::new(), true)
}

fn policy_response(
    policy: PrivacyPolicy,
    effective: PrivacyPolicy,
    load: refact_privacy::PolicyLoad,
    destinations: Vec<Destination>,
    match_counts: BTreeMap<String, usize>,
    discovery_deferred: bool,
) -> PrivacyPolicyResponse {
    let has_project_overrides = policy != effective;
    PrivacyPolicyResponse {
        policy,
        destinations,
        match_counts,
        discovery_deferred,
        error: load.error,
        source_paths: load
            .source_paths
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        has_project_overrides,
    }
}

async fn collect_destinations(app: &AppState, policy: &PrivacyPolicy) -> Vec<Destination> {
    let mut destinations = Vec::new();
    let caps = refact_runtime_api::caps_loader::try_load_caps_quickly_if_not_present(app.gcx.clone(), 0)
        .await
        .ok();
    if let Some(caps) = caps {
        for model in caps.chat_models.values() {
            let id = provider_id(&model.base.id);
            destinations.push(Destination {
                id: DestinationId(id.clone()),
                kind: DestinationKind::Provider,
                display_name: id,
            });
        }
        for model in caps.completion_models.values() {
            let id = provider_id(&model.base.id);
            destinations.push(Destination {
                id: DestinationId(id.clone()),
                kind: DestinationKind::Completion,
                display_name: id,
            });
        }
        if !caps.embedding_model.base.id.is_empty() {
            let id = provider_id(&caps.embedding_model.base.id);
            destinations.push(Destination {
                id: DestinationId(id.clone()),
                kind: DestinationKind::Provider,
                display_name: id,
            });
        }
    }
    let mut mcp_paths =
        refact_runtime_api::integrations::setting_up_integrations::integrations_all(app.gcx.clone(), false)
            .await
            .integrations
            .into_iter()
            .filter(|record| record.integr_name.starts_with("mcp_") && record.integr_config_exists)
            .map(|record| record.integr_config_path)
            .collect::<Vec<_>>();
    {
        let sessions = app.gcx.integration_sessions.lock().await;
        mcp_paths.extend(sessions.keys().cloned());
    }
    for path in mcp_paths {
        let id = refact_integrations::mcp::mcp_naming::server_name_from_config_path(&path);
        destinations.push(Destination {
            id: DestinationId(id.clone()),
            kind: DestinationKind::Mcp,
            display_name: id,
        });
    }
    let known_ids = destinations
        .iter()
        .map(|destination| destination.id.0.clone())
        .collect::<HashSet<_>>();
    for id in policy
        .zones
        .iter()
        .flat_map(|zone| zone.send_to.iter())
        .filter(|id| id.as_str() != "*" && !known_ids.contains(id.as_str()))
    {
        destinations.push(Destination {
            id: DestinationId(id.clone()),
            kind: DestinationKind::Provider,
            display_name: id.clone(),
        });
    }
    destinations.sort_by(|left, right| destination_key(left).cmp(&destination_key(right)));
    destinations.dedup_by(|left, right| left.id == right.id && left.kind == right.kind);
    destinations
}

const MATCH_COUNTS_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60);

fn policy_fingerprint(policy: &PrivacyPolicy) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(policy)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

fn live_match_counts(
    app: &AppState,
    policy: &PrivacyPolicy,
) -> Result<BTreeMap<String, usize>, ScratchError> {
    let fingerprint = policy_fingerprint(policy);
    let (cached, cold, needs_refresh) = {
        let cache = app
            .gcx
            .privacy_match_counts
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fresh = cache.fingerprint == fingerprint
            && cache
                .computed_at
                .is_some_and(|at| at.elapsed() < MATCH_COUNTS_MAX_AGE);
        (
            cache.counts.clone(),
            cache.computed_at.is_none(),
            !fresh && !cache.computing,
        )
    };
    if cold {
        let counts = compute_match_counts(&app.gcx, policy).unwrap_or_default();
        let mut cache = app
            .gcx
            .privacy_match_counts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.counts = counts.clone();
        cache.fingerprint = fingerprint;
        cache.computed_at = Some(std::time::Instant::now());
        return Ok(counts);
    }
    if needs_refresh {
        spawn_match_counts_refresh(app.gcx.clone(), policy.clone(), fingerprint);
    }
    Ok(cached)
}

fn spawn_match_counts_refresh(
    gcx: Arc<refact_runtime_api::global_context::GlobalContext>,
    policy: PrivacyPolicy,
    fingerprint: u64,
) {
    {
        let mut cache = gcx
            .privacy_match_counts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.computing {
            return;
        }
        cache.computing = true;
    }
    tokio::task::spawn_blocking(move || {
        let computed = compute_match_counts(&gcx, &policy);
        let mut cache = gcx
            .privacy_match_counts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.computing = false;
        if let Some(counts) = computed {
            cache.counts = counts;
            cache.fingerprint = fingerprint;
            cache.computed_at = Some(std::time::Instant::now());
        }
    });
}

fn compute_match_counts(
    gcx: &Arc<refact_runtime_api::global_context::GlobalContext>,
    policy: &PrivacyPolicy,
) -> Option<BTreeMap<String, usize>> {
    let compiled = policy.compile().ok()?;
    let mappings = registered_worktree_path_mappings(gcx.cache_dir.as_path());
    let workspace_roots = gcx
        .documents_state
        .workspace_folders
        .lock()
        .unwrap()
        .clone();
    let files = gcx.documents_state.workspace_files.lock().unwrap().clone();
    let mut counts = policy
        .zones
        .iter()
        .map(|zone| (zone.name.clone(), 0))
        .collect::<BTreeMap<_, _>>();
    counts.entry("blocked".to_string()).or_default();
    for path in files {
        let zone = strictest_zone_for_path(&compiled, &path, &workspace_roots, &mappings);
        *counts.entry(zone.name.clone()).or_default() += 1;
    }
    Some(counts)
}

fn provider_id(model_id: &str) -> String {
    model_id
        .split_once('/')
        .map_or(model_id, |(provider, _)| provider)
        .to_string()
}

fn destination_key(destination: &Destination) -> (u8, &str) {
    let kind = match destination.kind {
        DestinationKind::Provider => 0,
        DestinationKind::Mcp => 1,
        DestinationKind::SubagentModel => 2,
        DestinationKind::Completion => 3,
    };
    (kind, destination.id.0.as_str())
}

fn observation_capability(
    runtime: refact_privacy::PrivacyObservationRuntimeState,
) -> PrivacyObservationCapability {
    PrivacyObservationCapability {
        platform_supported: cfg!(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )),
        runtime_available: runtime.runtime_available,
        last_error: runtime.last_error,
    }
}
