use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use refact_runtime_api::AppState;
use refact_perf::perf_telemetry::{PERFORMANCE_TELEMETRY_SCHEMA_VERSION, PerformanceTelemetrySnapshot};
use refact_core::custom_error::ScratchError;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceTelemetrySettingsRequest {
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct PerformanceTelemetrySettingsResponse {
    schema_version: u8,
    enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct PerformanceTelemetryResetResponse {
    schema_version: u8,
    reset: bool,
    enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct PerformanceTelemetryResponse {
    #[serde(flatten)]
    telemetry: PerformanceTelemetrySnapshot,
    rollout_switches: RolloutSwitches,
}

#[derive(Debug, Serialize)]
struct RolloutSwitches {
    trajectory_writer_enabled: bool,
    trajectory_index_coordinator_enabled: bool,
    trajectory_watcher_self_write_enabled: bool,
    tool_catalog_snapshots_enabled: bool,
    vecdb_path_coalescing_enabled: bool,
}

fn rollout_switches() -> RolloutSwitches {
    RolloutSwitches {
        trajectory_writer_enabled: refact_chat_history::trajectory_writer::trajectory_writer_rollout_enabled(),
        trajectory_index_coordinator_enabled:
            refact_chat_history::trajectory_index::trajectory_index_coordinator_rollout_enabled(),
        trajectory_watcher_self_write_enabled:
            refact_chat_history::trajectory_writer::trajectory_watcher_self_write_rollout_enabled(),
        tool_catalog_snapshots_enabled: refact_runtime_api::tool_catalog_snapshot_rollout_enabled(),
        vecdb_path_coalescing_enabled:
            refact_vecdb::vdb_thread::vecdb_path_coalescing_rollout_enabled(),
    }
}

pub async fn handle_v1_performance_telemetry_get(
    State(app): State<AppState>,
) -> Json<PerformanceTelemetryResponse> {
    Json(PerformanceTelemetryResponse {
        telemetry: app.gcx.performance_telemetry.snapshot(),
        rollout_switches: rollout_switches(),
    })
}

pub async fn handle_v1_performance_telemetry_post(
    State(app): State<AppState>,
    Json(request): Json<PerformanceTelemetrySettingsRequest>,
) -> Json<PerformanceTelemetrySettingsResponse> {
    app.gcx.performance_telemetry.set_enabled(request.enabled);
    Json(PerformanceTelemetrySettingsResponse {
        schema_version: PERFORMANCE_TELEMETRY_SCHEMA_VERSION,
        enabled: app.gcx.performance_telemetry.enabled(),
    })
}

pub async fn handle_v1_performance_telemetry_reset(
    State(app): State<AppState>,
) -> Result<Json<PerformanceTelemetryResetResponse>, ScratchError> {
    app.gcx.performance_telemetry.reset();
    Ok(Json(PerformanceTelemetryResetResponse {
        schema_version: PERFORMANCE_TELEMETRY_SCHEMA_VERSION,
        reset: true,
        enabled: app.gcx.performance_telemetry.enabled(),
    }))
}
