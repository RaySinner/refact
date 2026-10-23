mod load;
mod model;
mod paths;
mod queries;
mod reload;
mod store;
mod watcher;
mod writer;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests_commit;
#[cfg(test)]
mod tests_external_reload;
#[cfg(test)]
mod tests_frozen_prefix;
#[cfg(test)]
mod tests_http_save;
#[cfg(test)]
mod tests_index_list;
#[cfg(test)]
mod tests_load_delete;
#[cfg(test)]
mod tests_misc_handlers;
#[cfg(test)]
mod tests_prune_paths;
#[cfg(test)]
mod tests_snapshot;
#[cfg(test)]
mod tests_titles_stats;
#[cfg(test)]
mod tests_watcher_pending;

#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
#[cfg(test)]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(test)]
use axum::extract::Path as AxumPath;
#[cfg(test)]
use axum::extract::State;
#[cfg(test)]
use axum::http::{Response, StatusCode};
#[cfg(test)]
use hyper::Body;
#[cfg(test)]
use serde::{Deserialize, Serialize};
#[cfg(test)]
use crate::chat::perf_diagnostics::{PerfComponent, PerfOutcome};
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use tokio::fs;
#[cfg(test)]
use tokio::sync::{broadcast, Mutex as AMutex, Notify};
#[cfg(test)]
use tracing::{debug, info, warn};
#[cfg(test)]
use uuid::Uuid;
#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use crate::call_validation::{ChatContent, ChatMessage};
#[cfg(test)]
use crate::chat::types::{
    ChatSession, ExternalReloadPending, GoalSnapshotBudgetExt, SessionState, TaskMeta, ThreadParams,
    TrajectoryCommitIntent, TrajectorySourceIdentity, TrajectorySourceIdentitySessionExt,
};
#[cfg(test)]
use crate::chat::trajectory_index;
#[cfg(test)]
use crate::global_context::GlobalContext;
#[cfg(test)]
use crate::worktrees::service::WorktreeService;
#[cfg(test)]
use crate::worktrees::types::WorktreeMeta;
#[cfg(test)]
use refact_chat_api::{FrozenRequestPrefix, GoalLedgerEntry, GoalSnapshot, GoalStatus};

#[cfg(test)]
use load::*;
#[cfg(test)]
use model::*;
#[cfg(test)]
use paths::*;
#[cfg(test)]
use queries::*;
#[cfg(test)]
use reload::*;
#[cfg(test)]
use store::*;
#[cfg(test)]
use watcher::*;
#[cfg(test)]
use writer::*;

pub use refact_chat_history::trajectory_event::TrajectoryEvent;
pub use refact_chat_history::trajectory_snapshot::TrajectorySnapshot;

// Re-exports from writer
pub use writer::{
    atomic_write_file, trajectory_watcher_self_write_rollout_enabled,
    trajectory_writer_rollout_enabled, TRAJECTORY_WATCHER_SELF_WRITE_ENV, TRAJECTORY_WRITER_ENV,
};
pub(crate) use writer::{
    trajectory_watcher_self_write_rollout_enabled_for, trajectory_writer_rollout_enabled_for,
};
#[cfg(any(test, feature = "bench"))]
pub(crate) use writer::TrajectoryWriterRolloutGuard;
use writer::{
    atomic_write_json, atomic_write_json_with_tmp_path, consume_trajectory_self_write,
    unique_trajectory_tmp_path, TrajectorySelfWriteScope,
};
#[cfg(test)]
pub(crate) use writer::clear_all_trajectory_self_writes;
#[cfg(test)]
use writer::{
    clear_trajectory_self_writes, complete_trajectory_self_write, register_trajectory_self_write,
    trajectory_self_writes, TRAJECTORY_SELF_WRITE_TTL,
};
#[cfg(test)]
use writer::rollout_switch_enabled;

// Re-exports from model
pub use model::{
    apply_mode_defaults_to_thread, get_session_state_for_chat, trajectory_event_is_displayable_chat,
    LoadedTrajectory, TrajectoryData, TrajectoryMeta, TrajectoryRepairPatch,
};
pub(crate) use model::{
    trajectory_list_data_is_displayable_chat, trajectory_snapshot_from_session,
    TrajectoryListCandidate, TrajectoryListData,
};
use model::{
    clamp_goal_snapshot_for_load, epoch_ms_now, get_session_runtime_for_trajectory_source,
};

// Re-exports from paths
pub use paths::{
    chat_folder_name, find_trajectory_or_buddy_path, find_trajectory_path,
    find_trajectory_path_for_active_chat, get_all_trajectories_dirs, get_buddy_conversations_dir,
    get_global_trajectories_dir, get_trajectories_dir, internal_trace_link_type,
    is_internal_trace_link_type, validate_trajectory_id, TrajectoryVectorizationQueue,
    INTERNAL_TRACE_LINK_PREFIX, UNATTRIBUTED_TRACES_DIR,
};
pub(crate) use paths::{
    find_trajectory_parent_id, index_dir_for_trajectory_file, is_trajectory_id_mismatch_error,
    list_chat_folder_dirs, list_task_trajectory_dirs, list_trajectory_dirs,
    trajectory_id_mismatch_error,
};
use paths::{
    enqueue_trajectory_vectorization, ensure_existing_trajectory_file_matches,
    find_normal_trajectory_file, find_normal_trajectory_path, find_trajectory_file,
    find_trajectory_or_buddy_file, fix_tool_call_indexes, get_all_task_roots,
    get_all_task_roots_from_weak, get_all_trajectories_dirs_from_weak,
    get_or_create_buddy_conversations_dir, normal_trajectory_candidate_paths,
    parse_stored_messages, prune_buddy_conversations_throttled, prune_internal_traces_throttled,
    read_existing_trajectory_object, read_valid_trajectory_candidate, should_vectorize_trajectory,
    trajectory_candidate_paths, PruneThrottle, ValidTrajectoryCandidate,
    PRUNE_THROTTLE_MAX_ENTRIES,
};
#[cfg(test)]
use paths::{
    prune_buddy_conversations_in_dir, record_trajectory_metadata_disk_read,
    trajectory_metadata_disk_reads_by_chat, trajectory_metadata_disk_reads_for_chat,
};

// Re-exports from load
pub use load::{
    load_generic_trajectory_for_chat, load_trajectory_for_chat, save_initial_task_chat_trajectory,
    save_trajectory_as, save_trajectory_as_with_intent, save_trajectory_as_with_intent_checked,
    set_trajectory_label, validate_trajectory_label,
};
pub(crate) use load::{
    chat_id_is_planner_for_task, resolve_task_planner_controller_chat_id,
    trajectory_path_stem_matches_id, verified_planner_linked_root_chat_id,
};
use load::{
    effective_trajectory_source_for_path, is_known_trajectory_top_level_key,
    load_generic_trajectory_for_chat_matching_source, load_trajectory_candidate,
    preserve_existing_trajectory_metadata, safe_new_buddy_trajectory_file,
    safe_new_nested_trajectory_file, safe_new_task_trajectory_file, sanitize_worktree_extra,
    title_generation_backing_file_matches, trajectory_candidate_has_minimum_schema,
    trajectory_root_id_matches, trajectory_source_matches_hint, trajectory_source_path_is_allowed,
    trajectory_worktree_from_extra, validate_loaded_worktree_strict,
};

// Re-exports from store
pub use store::{
    flush_all_trajectories, flush_trajectory_for_session, maybe_save_trajectory,
    maybe_save_trajectory_background, maybe_save_trajectory_background_with_intent,
    maybe_save_trajectory_with_intent, save_trajectory_snapshot, try_save_trajectory,
    try_save_trajectory_with_intent,
};
pub(crate) use store::persist_trajectory_snapshot_with_intent;
#[cfg(test)]
use store::{
    set_test_detached_trajectory_writer_failure_hook, set_test_message_serialization_failure,
    test_message_serialization_should_fail, wait_for_test_detached_trajectory_writer_failure,
    TEST_DETACHED_TRAJECTORY_WRITER_FAILURE_HOOK, TEST_MESSAGE_SERIALIZATION_FAILURE_ID,
};
#[cfg(test)]
use store::{
    commit_trajectory_snapshot_for_session, detached_trajectory_writer_key,
    detached_trajectory_writers, flush_all_trajectories_with_timeout,
    prune_detached_trajectory_writers, run_detached_trajectory_writer,
    save_trajectory_snapshot_inner, store_trajectory_metadata_cache_entry,
    trajectory_metadata_cache, trajectory_metadata_cache_key, wait_for_detached_trajectory_commit,
    wait_for_trajectory_commit, DetachedTrajectoryWriteCache, DetachedTrajectoryWriterState,
    SessionTrajectoryCommitOutcome, TrajectoryFileStamp, TrajectoryMetadataCacheEntry,
    MAX_CACHED_TRAJECTORY_METADATA,
};

// Re-exports from reload
pub use reload::{
    check_external_reload_pending, ensure_frozen_prefix, first_system_prompt,
    frozen_prefix_is_complete, new_frozen_request_prefix, refresh_session_from_trajectory_if_stale,
};
pub(crate) use reload::{
    new_legacy_incomplete_frozen_request_prefix, persist_loaded_trajectory_repair_raw,
};
use reload::{
    apply_external_delete_with_revalidation, apply_loaded_external_update_with_repair,
    can_apply_external_reload, loaded_trajectory_source, parsed_rfc3339_utc,
    persist_loaded_trajectory_repair, raw_frozen_prefix_has_tools, repair_transition_frozen_prefix,
    session_is_stale_refresh_candidate, ExternalDeleteRevalidationOutcome,
};
#[cfg(test)]
use reload::{persist_frozen_prefix, trajectory_commit_contract};
#[cfg(test)]
pub(crate) use reload::trajectory_commit_contract_for_test;

// Re-exports from watcher
pub use watcher::start_trajectory_watcher;
use watcher::{
    is_trajectory_temporary_file, is_under_task_root, should_dispatch_trajectory_path,
    task_trajectory_context_from_path, trajectory_source_identity_from_path,
    trajectory_watcher_message_for_path,
};
#[cfg(test)]
pub(crate) use watcher::{process_trajectory_change, reset_test_trajectory_watcher_task};
#[cfg(test)]
use watcher::{
    process_trajectory_change_for_source, process_trajectory_watcher_change,
    remove_stale_trajectory_index_entries,
};
#[cfg(test)]
use watcher::TRAJECTORY_WATCHER_TASK;

// Re-exports from queries
pub use queries::{
    extract_text_with_image_placeholders_from_json, handle_v1_trajectories_all,
    handle_v1_trajectories_delete, handle_v1_trajectories_get, handle_v1_trajectories_list,
    handle_v1_trajectories_save, handle_v1_trajectories_subscribe, handle_v1_trajectory_path,
    list_all_trajectories_meta, list_trajectories_page, list_trajectories_page_for,
    PaginatedTrajectories, TrajectoriesAllQuery, TrajectoriesListQuery, TRAJECTORY_LABEL_MAX_CHARS,
};
pub(crate) use queries::{
    calculate_line_changes_from_messages, calculate_task_progress_from_messages,
    calculate_token_totals_from_messages, task_context_from_task_meta, trajectory_meta_title,
    TokenTotals,
};
use queries::{
    allowed_real_dir_symlink_target, calculate_line_changes_from_chat_messages,
    calculate_task_progress_from_chat_messages, calculate_token_totals_from_chat_messages,
    canonical_child_path_under_root, count_user_messages, ensure_real_dir_tree, is_placeholder_title,
    is_real_dir, is_real_dir_sync, is_real_file, safe_new_trajectory_file_in_dir,
    safe_trajectory_file_in_dir, spawn_task_name_generation_task, spawn_title_generation_task,
    hydrate_trajectory_list_page, trajectory_list_candidate_matches_hydrated_data,
    TRAJECTORY_META_TITLE_MAX_CHARS,
};
#[cfg(test)]
pub(crate) use queries::TITLE_GENERATION_LLM_TIMEOUT;
