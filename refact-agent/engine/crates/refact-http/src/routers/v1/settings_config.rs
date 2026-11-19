use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::Json;
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use refact_runtime_api::AppState;
use refact_core::custom_error::ScratchError;
use refact_files::indexing_settings::IndexingSettings;

#[derive(Debug, Deserialize)]
pub struct SettingsScopeQuery {
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IndexingSettingsConfig {
    #[serde(default)]
    pub blocklist: Vec<String>,
    #[serde(default)]
    pub additional_indexing_dirs: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct IndexingSettingsResponse {
    pub scope: String,
    pub path: String,
    pub config: IndexingSettingsConfig,
    pub project_available: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum IndexingSettingsPost {
    Direct(IndexingSettingsConfig),
    Wrapped { config: IndexingSettingsConfig },
}

impl IndexingSettingsPost {
    fn into_config(self) -> IndexingSettingsConfig {
        match self {
            Self::Direct(config) | Self::Wrapped { config } => config,
        }
    }
}

#[derive(Serialize)]
struct PersistedIndexingSettings {
    blocklist: Vec<String>,
    additional_indexing_dirs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillsAutoTriggerSetting {
    IndexOnly,
    InjectFull,
    Off,
}

impl Default for SkillsAutoTriggerSetting {
    fn default() -> Self {
        Self::IndexOnly
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SkillsSettings {
    #[serde(default)]
    pub auto_trigger: SkillsAutoTriggerSetting,
}

fn bad_request(message: impl Into<String>) -> ScratchError {
    ScratchError::new(StatusCode::BAD_REQUEST, message.into())
}

fn server_error(message: impl Into<String>) -> ScratchError {
    ScratchError::new(StatusCode::INTERNAL_SERVER_ERROR, message.into())
}

fn requested_scope(query: &SettingsScopeQuery) -> Result<&str, ScratchError> {
    match query.scope.as_deref() {
        Some("global") => Ok("global"),
        Some("project") => Ok("project"),
        Some(other) => Err(bad_request(format!(
            "invalid scope '{}'; expected 'global' or 'project'",
            other
        ))),
        None => Err(bad_request("missing scope; expected 'global' or 'project'")),
    }
}

async fn first_project_root(app: &AppState) -> Result<PathBuf, ScratchError> {
    refact_runtime_api::files_correction::get_project_dirs(app.gcx.clone())
        .await
        .into_iter()
        .next()
        .ok_or_else(|| bad_request("no project root is available"))
}

fn global_indexing_path(app: &AppState) -> PathBuf {
    let configured = app.gcx.cmdline.indexing_yaml.trim();
    if configured.is_empty() {
        app.gcx.config_dir.join("indexing.yaml")
    } else {
        refact_files::path_utils::canonical_path(configured)
    }
}

async fn indexing_path(
    app: &AppState,
    scope: &str,
) -> Result<(PathBuf, Option<PathBuf>, bool), ScratchError> {
    let project_root = refact_runtime_api::files_correction::get_project_dirs(app.gcx.clone())
        .await
        .into_iter()
        .next();
    if scope == "global" {
        Ok((global_indexing_path(app), None, project_root.is_some()))
    } else {
        let root = project_root.ok_or_else(|| bad_request("no project root is available"))?;
        Ok((root.join(".refact").join("indexing.yaml"), Some(root), true))
    }
}

async fn ensure_default_global(path: &Path, app: &AppState) -> Result<(), ScratchError> {
    if !app.gcx.cmdline.indexing_yaml.trim().is_empty()
        || tokio::fs::try_exists(path)
            .await
            .map_err(|e| server_error(format!("cannot inspect {}: {}", path.display(), e)))?
    {
        return Ok(());
    }
    atomic_write(
        path,
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../src/yaml_configs/default_indexing.yaml")),
    )
    .await
}

pub async fn load_indexing_config(
    path: &Path,
    absent_is_empty: bool,
) -> Result<IndexingSettingsConfig, ScratchError> {
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(raw) => raw,
        Err(e) if absent_is_empty && e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(server_error(format!(
                "cannot read {}: {}",
                path.display(),
                e
            )))
        }
    };
    let settings = if raw.trim().is_empty() {
        IndexingSettings {
            blocklist: Vec::new(),
            additional_indexing_dirs: Vec::new(),
        }
    } else {
        serde_yaml::from_str::<IndexingSettings>(&raw).map_err(|e| {
            bad_request(format!(
                "invalid indexing YAML in {}: {}",
                path.display(),
                e
            ))
        })?
    };
    Ok(IndexingSettingsConfig {
        blocklist: settings.blocklist,
        additional_indexing_dirs: settings.additional_indexing_dirs,
    })
}

fn normalize_entries(entries: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    entries
        .into_iter()
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty() && seen.insert(entry.clone()))
        .collect()
}

pub fn validate_additional_dirs(scope: &str, entries: &[String]) -> Result<(), ScratchError> {
    for entry in entries {
        let uses_home_directory = entry == "~" || entry.starts_with("~/");
        if scope == "project" && uses_home_directory {
            return Err(bad_request(format!(
                "project additional indexing directory cannot use ~/ paths: {}",
                entry
            )));
        }
        let path = if uses_home_directory {
            let home = home::home_dir().ok_or_else(|| {
                server_error("cannot resolve home directory for indexing settings")
            })?;
            home.join(entry.trim_start_matches('~').trim_start_matches('/'))
        } else {
            PathBuf::from(entry)
        };
        if scope == "global" && !path.is_absolute() {
            return Err(bad_request(format!(
                "global additional indexing directory must be absolute or start with ~/: {}",
                entry
            )));
        }
        if path.is_absolute() && !path.is_dir() {
            return Err(bad_request(format!(
                "additional indexing directory is not an existing directory: {}",
                entry
            )));
        }
        if scope == "project"
            && !path.is_absolute()
            && path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
        {
            return Err(bad_request(format!(
                "project-relative additional indexing directory cannot escape the project: {}",
                entry
            )));
        }
    }
    Ok(())
}

fn normalize_config(
    scope: &str,
    mut config: IndexingSettingsConfig,
) -> Result<IndexingSettingsConfig, ScratchError> {
    config.blocklist = normalize_entries(config.blocklist);
    config.additional_indexing_dirs = normalize_entries(config.additional_indexing_dirs);
    validate_additional_dirs(scope, &config.additional_indexing_dirs)?;
    Ok(config)
}

async fn atomic_write(path: &Path, content: &str) -> Result<(), ScratchError> {
    let parent = path
        .parent()
        .ok_or_else(|| server_error(format!("{} has no parent directory", path.display())))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| server_error(format!("cannot create {}: {}", parent.display(), e)))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.yaml");
    let tmp = parent.join(format!(
        ".{}.{}.{}.tmp",
        file_name,
        std::process::id(),
        nonce
    ));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .await?;
        file.write_all(content.as_bytes()).await?;
        file.flush().await?;
        file.sync_all().await?;
        tokio::fs::rename(&tmp, path).await
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(server_error(format!(
            "cannot atomically write {}: {}",
            path.display(),
            error
        )));
    }
    Ok(())
}

fn indexing_response(
    scope: &str,
    path: &Path,
    config: IndexingSettingsConfig,
    project_available: bool,
) -> Json<IndexingSettingsResponse> {
    Json(IndexingSettingsResponse {
        scope: scope.to_string(),
        path: path.to_string_lossy().to_string(),
        config,
        project_available,
    })
}

pub async fn handle_v1_indexing_settings_get(
    State(app): State<AppState>,
    Query(query): Query<SettingsScopeQuery>,
) -> Result<Json<IndexingSettingsResponse>, ScratchError> {
    let scope = requested_scope(&query)?;
    let (path, _, project_available) = indexing_path(&app, scope).await?;
    if scope == "global" {
        ensure_default_global(&path, &app).await?;
    }
    let config = load_indexing_config(&path, scope == "project").await?;
    Ok(indexing_response(scope, &path, config, project_available))
}

pub async fn handle_v1_indexing_settings_post(
    State(app): State<AppState>,
    Query(query): Query<SettingsScopeQuery>,
    body: hyper::body::Bytes,
) -> Result<Json<IndexingSettingsResponse>, ScratchError> {
    let scope = requested_scope(&query)?;
    let post = serde_json::from_slice::<IndexingSettingsPost>(&body).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid indexing settings payload: {}", e),
        )
    })?;
    let (path, _, project_available) = indexing_path(&app, scope).await?;
    let config = normalize_config(scope, post.into_config())?;
    let persisted = PersistedIndexingSettings {
        blocklist: config.blocklist.clone(),
        additional_indexing_dirs: config.additional_indexing_dirs.clone(),
    };
    let yaml = serde_yaml::to_string(&persisted)
        .map_err(|e| server_error(format!("cannot serialize indexing settings: {}", e)))?;
    atomic_write(&path, &yaml).await?;

    refact_runtime_api::gcx_helpers::reload_indexing_everywhere_for_gcx(&app.gcx, true)
        .await;
    refact_runtime_api::files_in_workspace::enqueue_all_files_from_workspace_folders(
        app.gcx.clone(),
        true,
        false,
    )
    .await;
    Ok(indexing_response(scope, &path, config, project_available))
}

async fn skills_path(app: &AppState) -> Result<PathBuf, ScratchError> {
    Ok(first_project_root(app)
        .await?
        .join(".refact")
        .join("skills.yaml"))
}

async fn load_skills(path: &Path) -> Result<SkillsSettings, ScratchError> {
    match tokio::fs::read_to_string(path).await {
        Ok(raw) => serde_yaml::from_str(&raw)
            .map_err(|e| bad_request(format!("invalid skills YAML in {}: {}", path.display(), e))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SkillsSettings::default()),
        Err(e) => Err(server_error(format!(
            "cannot read {}: {}",
            path.display(),
            e
        ))),
    }
}

pub async fn handle_v1_skills_settings_get(
    State(app): State<AppState>,
) -> Result<Json<SkillsSettings>, ScratchError> {
    let path = skills_path(&app).await?;
    Ok(Json(load_skills(&path).await?))
}

pub async fn handle_v1_skills_settings_post(
    State(app): State<AppState>,
    body: hyper::body::Bytes,
) -> Result<Json<SkillsSettings>, ScratchError> {
    let settings = serde_json::from_slice::<SkillsSettings>(&body).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid skills settings payload: {}", e),
        )
    })?;
    let path = skills_path(&app).await?;
    let yaml = serde_yaml::to_string(&settings)
        .map_err(|e| server_error(format!("cannot serialize skills settings: {}", e)))?;
    atomic_write(&path, &yaml).await?;
    Ok(Json(settings))
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum TrajectorySettingsPost {
    Direct(refact_runtime_api::runtime_settings::TrajectoryRuntimeSettings),
    Wrapped {
        config: refact_runtime_api::runtime_settings::TrajectoryRuntimeSettings,
    },
}

impl TrajectorySettingsPost {
    fn into_config(self) -> refact_runtime_api::runtime_settings::TrajectoryRuntimeSettings {
        match self {
            Self::Direct(config) | Self::Wrapped { config } => config,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TrajectorySettingsResponse {
    pub path: String,
    pub config: refact_runtime_api::runtime_settings::TrajectoryRuntimeSettings,
    pub current: refact_runtime_api::runtime_settings::TrajectoryRuntimeSettings,
    pub defaults: refact_runtime_api::runtime_settings::TrajectoryRuntimeSettings,
    pub fields: Vec<TrajectorySettingField>,
    pub environment_precedence: &'static str,
}

#[derive(Debug, Serialize)]
pub struct TrajectorySettingField {
    pub name: &'static str,
    pub value_type: &'static str,
    pub minimum: Option<u64>,
    pub maximum: Option<u64>,
    pub apply_mode: &'static str,
}

pub fn trajectory_settings_fields() -> Vec<TrajectorySettingField> {
    let live_usize = |name, minimum, maximum| TrajectorySettingField {
        name,
        value_type: "integer",
        minimum: Some(minimum),
        maximum: Some(maximum),
        apply_mode: "live",
    };
    let restart_usize = |name, minimum, maximum| TrajectorySettingField {
        name,
        value_type: "integer",
        minimum: Some(minimum),
        maximum: Some(maximum),
        apply_mode: "restart_required",
    };
    let restart_bool = |name| TrajectorySettingField {
        name,
        value_type: "boolean",
        minimum: None,
        maximum: None,
        apply_mode: "restart_required",
    };
    let live_bool = |name| TrajectorySettingField {
        name,
        value_type: "boolean",
        minimum: None,
        maximum: None,
        apply_mode: "live",
    };
    vec![
        live_usize("internal_traces_keep_per_folder", 10, 10_000),
        live_usize("internal_trace_prune_interval_secs", 60, 86_400),
        live_usize("buddy_conversations_keep", 10, 100_000),
        live_usize("buddy_conversations_prune_interval_secs", 60, 86_400),
        live_usize("buddy_conversations_prune_min_age_secs", 60, 365 * 86_400),
        live_usize("session_idle_timeout_secs", 60, 86_400),
        live_usize("session_cleanup_interval_secs", 10, 86_400),
        live_usize("stream_idle_timeout_secs", 10, 86_400),
        live_usize("stream_total_timeout_secs", 60, 172_800),
        live_usize("max_queue_size", 1, 10_000),
        restart_usize("event_channel_capacity", 16, 1_000_000),
        live_usize("recent_request_ids_capacity", 1, 100_000),
        live_usize("max_parallel_tools", 1, 10_000),
        live_usize("max_images_per_message", 1, 1_000),
        live_usize("max_file_size", 1_024, 50_000_000),
        live_usize("auto_enrichment_total_token_cap", 64, 32_000),
        live_usize("auto_enrichment_card_token_cap", 32, 16_000),
        live_usize("auto_enrichment_knowledge_top_n", 1, 20),
        live_usize("auto_enrichment_trajectory_top_n", 1, 20),
        restart_bool("trajectory_writer_enabled"),
        restart_bool("trajectory_index_coordinator_enabled"),
        live_bool("trajectory_watcher_self_write_enabled"),
        restart_bool("tool_catalog_snapshots_enabled"),
        restart_bool("vecdb_path_coalescing_enabled"),
        live_usize("pp_max_tool_budget_tokens", 4_096, 2_000_000),
        live_usize("pp_max_per_file_budget_tokens", 1_024, 2_000_000),
        live_usize("pp_max_line_length_chars", 80, 1_000_000),
        live_usize("pp_tokens_for_text_percent", 1, 100),
        live_usize("git_intel_max_commits", 100, 1_000_000),
        live_usize("git_intel_deep_walk_limit", 100, 5_000_000),
        live_usize("git_intel_max_files_per_commit_cochange", 10, 100_000),
        live_usize("git_intel_max_files_per_commit_entropy", 5, 100_000),
        live_usize("codegraph_dead_code_max_results", 50, 1_000_000),
        live_usize("codegraph_exec_flow_max_nodes", 50, 1_000_000),
        live_usize("vecdb_trajectory_split_bytes", 256, 1_048_576),
        live_usize("cat_max_input_paths", 1, 100_000),
        live_usize("cat_max_lines", 100, 1_000_000),
        live_usize("cat_max_file_bytes", 4_096, 268_435_456),
        live_usize("cat_max_expanded_files", 1, 100_000),
        live_bool("cat_line_ranges_enabled"),
        live_usize("get_logs_max_tail_bytes", 4_096, 268_435_456),
        live_usize("planner_qna_question_limit", 200, 1_000_000),
        live_usize("planner_qna_answer_limit", 200, 1_000_000),
        live_usize("hist_search_preview_chars", 100, 100_000),
        live_usize("agent_diff_max_output_bytes", 4_096, 268_435_456),
        live_usize("process_subscribe_preview_bytes", 50, 100_000),
        live_usize("review_diff_char_cap", 4_096, 8_388_608),
        live_usize("review_max_diff_patch_bytes", 4_096, 268_435_456),
        live_usize("task_agent_max_retries", 0, 10),
        live_bool("tool_loop_guard_enabled"),
        live_usize("tool_loop_guard_threshold", 1, 100),
        live_usize("max_investigation_tool_calls_per_turn", 1, 100),
        live_bool("tool_loop_hard_stop_on_block"),
        live_bool("tool_loop_stagnation_detector_enabled"),
        live_usize("tool_loop_stagnation_threshold", 2, 50),
    ]
}

fn trajectory_settings_response(
    path: PathBuf,
    config: refact_runtime_api::runtime_settings::TrajectoryRuntimeSettings,
) -> Json<TrajectorySettingsResponse> {
    Json(TrajectorySettingsResponse {
        path: path.to_string_lossy().to_string(),
        config,
        current: refact_runtime_api::runtime_settings::current(),
        defaults: refact_runtime_api::runtime_settings::TrajectoryRuntimeSettings::default(),
        fields: trajectory_settings_fields(),
        environment_precedence: "REFACT_TRAJECTORY_WRITER, REFACT_TRAJECTORY_INDEX_COORDINATOR, REFACT_TRAJECTORY_WATCHER_SELF_WRITE, REFACT_TOOL_CATALOG_SNAPSHOTS, and REFACT_VECDB_PATH_COALESCING take precedence over persisted rollout switches. Persisted writer, index coordinator, tool catalog, and VecDB changes apply after restart; watcher self-write suppression applies live.",
    })
}

pub async fn handle_v1_trajectory_settings_get(
    State(app): State<AppState>,
) -> Result<Json<TrajectorySettingsResponse>, ScratchError> {
    let path = refact_runtime_api::runtime_settings::settings_path(&app.paths.config_dir);
    let config = refact_runtime_api::runtime_settings::load_from_path(&path)
        .await
        .map_err(server_error)?;
    Ok(trajectory_settings_response(path, config))
}

pub async fn handle_v1_trajectory_settings_post(
    State(app): State<AppState>,
    body: hyper::body::Bytes,
) -> Result<Json<TrajectorySettingsResponse>, ScratchError> {
    let settings = serde_json::from_slice::<TrajectorySettingsPost>(&body)
        .map_err(|error| {
            ScratchError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid trajectory settings payload: {error}"),
            )
        })?
        .into_config();
    refact_runtime_api::runtime_settings::validate(&settings).map_err(bad_request)?;
    let path = refact_runtime_api::runtime_settings::settings_path(&app.paths.config_dir);
    let yaml = serde_yaml::to_string(&settings)
        .map_err(|error| server_error(format!("cannot serialize trajectory settings: {error}")))?;
    atomic_write(&path, &yaml).await?;
    refact_runtime_api::runtime_settings::install_live(&settings);
    Ok(trajectory_settings_response(path, settings))
}
