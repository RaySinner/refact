use std::sync::{LazyLock, RwLock};

use serde::{Deserialize, Serialize};

const INTERNAL_TRACES_KEEP_PER_FOLDER_DEFAULT: usize = 200;
const INTERNAL_TRACE_PRUNE_INTERVAL_SECS_DEFAULT: u64 = 3600;
const BUDDY_CONVERSATIONS_KEEP_DEFAULT: usize = 500;
const BUDDY_CONVERSATIONS_PRUNE_INTERVAL_SECS_DEFAULT: u64 = 3600;
const BUDDY_CONVERSATIONS_PRUNE_MIN_AGE_SECS_DEFAULT: u64 = 86_400;
const SESSION_IDLE_TIMEOUT_SECS_DEFAULT: u64 = 30 * 60;
const SESSION_CLEANUP_INTERVAL_SECS_DEFAULT: u64 = 5 * 60;
const STREAM_IDLE_TIMEOUT_SECS_DEFAULT: u64 = 5 * 60;
const STREAM_TOTAL_TIMEOUT_SECS_DEFAULT: u64 = 30 * 60;
const MAX_QUEUE_SIZE_DEFAULT: usize = 100;
const EVENT_CHANNEL_CAPACITY_DEFAULT: usize = 4096;
const RECENT_REQUEST_IDS_CAPACITY_DEFAULT: usize = 100;
const MAX_IMAGES_PER_MESSAGE_DEFAULT: usize = 50;
const MAX_FILE_SIZE_DEFAULT: usize = 40_000;
const AUTO_ENRICHMENT_TOTAL_TOKEN_CAP_DEFAULT: usize = 1600;
const AUTO_ENRICHMENT_CARD_TOKEN_CAP_DEFAULT: usize = 480;
const KNOWLEDGE_TOP_N_DEFAULT: usize = 3;
const TRAJECTORY_TOP_N_DEFAULT: usize = 2;
const PP_MAX_TOOL_BUDGET_TOKENS_DEFAULT: usize = 131_072;
const PP_MAX_PER_FILE_BUDGET_TOKENS_DEFAULT: usize = 131_072;
const PP_MAX_LINE_LENGTH_CHARS_DEFAULT: usize = 10_000;
const PP_TOKENS_FOR_TEXT_PERCENT_DEFAULT: usize = 30;
const GIT_INTEL_MAX_COMMITS_DEFAULT: usize = 5_000;
const GIT_INTEL_DEEP_WALK_LIMIT_DEFAULT: usize = 50_000;
const GIT_INTEL_MAX_FILES_PER_COMMIT_COCHANGE_DEFAULT: usize = 1_000;
const GIT_INTEL_MAX_FILES_PER_COMMIT_ENTROPY_DEFAULT: usize = 200;
const CODEGRAPH_DEAD_CODE_MAX_RESULTS_DEFAULT: usize = 5_000;
const CODEGRAPH_EXEC_FLOW_MAX_NODES_DEFAULT: usize = 5_000;
const VECDB_TRAJECTORY_SPLIT_BYTES_DEFAULT: usize = 16_384;
const CAT_MAX_INPUT_PATHS_DEFAULT: usize = 512;
const CAT_MAX_LINES_DEFAULT: usize = 5_000;
const CAT_MAX_FILE_BYTES_DEFAULT: usize = 8_388_608;
const CAT_MAX_EXPANDED_FILES_DEFAULT: usize = 2_048;
const CAT_LINE_RANGES_ENABLED_DEFAULT: bool = true;
const GET_LOGS_MAX_TAIL_BYTES_DEFAULT: usize = 1_048_576;
const PLANNER_QNA_QUESTION_LIMIT_DEFAULT: usize = 8_000;
const PLANNER_QNA_ANSWER_LIMIT_DEFAULT: usize = 8_000;
const HIST_SEARCH_PREVIEW_CHARS_DEFAULT: usize = 2_000;
const AGENT_DIFF_MAX_OUTPUT_BYTES_DEFAULT: usize = 4_194_304;
const PROCESS_SUBSCRIBE_PREVIEW_BYTES_DEFAULT: usize = 2_000;
const REVIEW_DIFF_CHAR_CAP_DEFAULT: usize = 400_000;
const REVIEW_MAX_DIFF_PATCH_BYTES_DEFAULT: usize = 4_194_304;
const TASK_AGENT_MAX_RETRIES_DEFAULT: usize = 3;
const TOOL_LOOP_GUARD_ENABLED_DEFAULT: bool = true;
const TOOL_LOOP_GUARD_THRESHOLD_DEFAULT: usize = 3;
const MAX_INVESTIGATION_TOOL_CALLS_PER_TURN_DEFAULT: usize = 12;
const TOOL_LOOP_HARD_STOP_ON_BLOCK_DEFAULT: bool = true;
const TOOL_LOOP_STAGNATION_DETECTOR_ENABLED_DEFAULT: bool = true;
const TOOL_LOOP_STAGNATION_THRESHOLD_DEFAULT: usize = 8;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct TrajectoryRuntimeSettings {
    pub internal_traces_keep_per_folder: usize,
    pub internal_trace_prune_interval_secs: u64,
    pub buddy_conversations_keep: usize,
    pub buddy_conversations_prune_interval_secs: u64,
    pub buddy_conversations_prune_min_age_secs: u64,
    pub session_idle_timeout_secs: u64,
    pub session_cleanup_interval_secs: u64,
    pub stream_idle_timeout_secs: u64,
    pub stream_total_timeout_secs: u64,
    pub max_queue_size: usize,
    pub event_channel_capacity: usize,
    pub recent_request_ids_capacity: usize,
    pub max_parallel_tools: Option<usize>,
    pub max_images_per_message: usize,
    pub max_file_size: usize,
    pub auto_enrichment_total_token_cap: usize,
    pub auto_enrichment_card_token_cap: usize,
    pub auto_enrichment_knowledge_top_n: usize,
    pub auto_enrichment_trajectory_top_n: usize,
    pub trajectory_writer_enabled: bool,
    pub trajectory_index_coordinator_enabled: bool,
    pub trajectory_watcher_self_write_enabled: bool,
    pub tool_catalog_snapshots_enabled: bool,
    pub vecdb_path_coalescing_enabled: bool,
    pub pp_max_tool_budget_tokens: usize,
    pub pp_max_per_file_budget_tokens: usize,
    pub pp_max_line_length_chars: usize,
    pub pp_tokens_for_text_percent: usize,
    pub git_intel_max_commits: usize,
    pub git_intel_deep_walk_limit: usize,
    pub git_intel_max_files_per_commit_cochange: usize,
    pub git_intel_max_files_per_commit_entropy: usize,
    pub codegraph_dead_code_max_results: usize,
    pub codegraph_exec_flow_max_nodes: usize,
    pub vecdb_trajectory_split_bytes: usize,
    pub cat_max_input_paths: usize,
    pub cat_max_lines: usize,
    pub cat_max_file_bytes: usize,
    pub cat_max_expanded_files: usize,
    pub cat_line_ranges_enabled: bool,
    pub get_logs_max_tail_bytes: usize,
    pub planner_qna_question_limit: usize,
    pub planner_qna_answer_limit: usize,
    pub hist_search_preview_chars: usize,
    pub agent_diff_max_output_bytes: usize,
    pub process_subscribe_preview_bytes: usize,
    pub review_diff_char_cap: usize,
    pub review_max_diff_patch_bytes: usize,
    pub task_agent_max_retries: usize,
    pub tool_loop_guard_enabled: bool,
    pub tool_loop_guard_threshold: usize,
    pub max_investigation_tool_calls_per_turn: usize,
    pub tool_loop_hard_stop_on_block: bool,
    pub tool_loop_stagnation_detector_enabled: bool,
    pub tool_loop_stagnation_threshold: usize,
}

impl Default for TrajectoryRuntimeSettings {
    fn default() -> Self {
        Self {
            internal_traces_keep_per_folder: INTERNAL_TRACES_KEEP_PER_FOLDER_DEFAULT,
            internal_trace_prune_interval_secs: INTERNAL_TRACE_PRUNE_INTERVAL_SECS_DEFAULT,
            buddy_conversations_keep: BUDDY_CONVERSATIONS_KEEP_DEFAULT,
            buddy_conversations_prune_interval_secs:
                BUDDY_CONVERSATIONS_PRUNE_INTERVAL_SECS_DEFAULT,
            buddy_conversations_prune_min_age_secs: BUDDY_CONVERSATIONS_PRUNE_MIN_AGE_SECS_DEFAULT,
            session_idle_timeout_secs: SESSION_IDLE_TIMEOUT_SECS_DEFAULT,
            session_cleanup_interval_secs: SESSION_CLEANUP_INTERVAL_SECS_DEFAULT,
            stream_idle_timeout_secs: STREAM_IDLE_TIMEOUT_SECS_DEFAULT,
            stream_total_timeout_secs: STREAM_TOTAL_TIMEOUT_SECS_DEFAULT,
            max_queue_size: MAX_QUEUE_SIZE_DEFAULT,
            event_channel_capacity: EVENT_CHANNEL_CAPACITY_DEFAULT,
            recent_request_ids_capacity: RECENT_REQUEST_IDS_CAPACITY_DEFAULT,
            max_parallel_tools: None,
            max_images_per_message: MAX_IMAGES_PER_MESSAGE_DEFAULT,
            max_file_size: MAX_FILE_SIZE_DEFAULT,
            auto_enrichment_total_token_cap: AUTO_ENRICHMENT_TOTAL_TOKEN_CAP_DEFAULT,
            auto_enrichment_card_token_cap: AUTO_ENRICHMENT_CARD_TOKEN_CAP_DEFAULT,
            auto_enrichment_knowledge_top_n: KNOWLEDGE_TOP_N_DEFAULT,
            auto_enrichment_trajectory_top_n: TRAJECTORY_TOP_N_DEFAULT,
            trajectory_writer_enabled: true,
            trajectory_index_coordinator_enabled: true,
            trajectory_watcher_self_write_enabled: true,
            tool_catalog_snapshots_enabled: true,
            vecdb_path_coalescing_enabled: true,
            pp_max_tool_budget_tokens: PP_MAX_TOOL_BUDGET_TOKENS_DEFAULT,
            pp_max_per_file_budget_tokens: PP_MAX_PER_FILE_BUDGET_TOKENS_DEFAULT,
            pp_max_line_length_chars: PP_MAX_LINE_LENGTH_CHARS_DEFAULT,
            pp_tokens_for_text_percent: PP_TOKENS_FOR_TEXT_PERCENT_DEFAULT,
            git_intel_max_commits: GIT_INTEL_MAX_COMMITS_DEFAULT,
            git_intel_deep_walk_limit: GIT_INTEL_DEEP_WALK_LIMIT_DEFAULT,
            git_intel_max_files_per_commit_cochange:
                GIT_INTEL_MAX_FILES_PER_COMMIT_COCHANGE_DEFAULT,
            git_intel_max_files_per_commit_entropy: GIT_INTEL_MAX_FILES_PER_COMMIT_ENTROPY_DEFAULT,
            codegraph_dead_code_max_results: CODEGRAPH_DEAD_CODE_MAX_RESULTS_DEFAULT,
            codegraph_exec_flow_max_nodes: CODEGRAPH_EXEC_FLOW_MAX_NODES_DEFAULT,
            vecdb_trajectory_split_bytes: VECDB_TRAJECTORY_SPLIT_BYTES_DEFAULT,
            cat_max_input_paths: CAT_MAX_INPUT_PATHS_DEFAULT,
            cat_max_lines: CAT_MAX_LINES_DEFAULT,
            cat_max_file_bytes: CAT_MAX_FILE_BYTES_DEFAULT,
            cat_max_expanded_files: CAT_MAX_EXPANDED_FILES_DEFAULT,
            cat_line_ranges_enabled: CAT_LINE_RANGES_ENABLED_DEFAULT,
            get_logs_max_tail_bytes: GET_LOGS_MAX_TAIL_BYTES_DEFAULT,
            planner_qna_question_limit: PLANNER_QNA_QUESTION_LIMIT_DEFAULT,
            planner_qna_answer_limit: PLANNER_QNA_ANSWER_LIMIT_DEFAULT,
            hist_search_preview_chars: HIST_SEARCH_PREVIEW_CHARS_DEFAULT,
            agent_diff_max_output_bytes: AGENT_DIFF_MAX_OUTPUT_BYTES_DEFAULT,
            process_subscribe_preview_bytes: PROCESS_SUBSCRIBE_PREVIEW_BYTES_DEFAULT,
            review_diff_char_cap: REVIEW_DIFF_CHAR_CAP_DEFAULT,
            review_max_diff_patch_bytes: REVIEW_MAX_DIFF_PATCH_BYTES_DEFAULT,
            task_agent_max_retries: TASK_AGENT_MAX_RETRIES_DEFAULT,
            tool_loop_guard_enabled: TOOL_LOOP_GUARD_ENABLED_DEFAULT,
            tool_loop_guard_threshold: TOOL_LOOP_GUARD_THRESHOLD_DEFAULT,
            max_investigation_tool_calls_per_turn: MAX_INVESTIGATION_TOOL_CALLS_PER_TURN_DEFAULT,
            tool_loop_hard_stop_on_block: TOOL_LOOP_HARD_STOP_ON_BLOCK_DEFAULT,
            tool_loop_stagnation_detector_enabled: TOOL_LOOP_STAGNATION_DETECTOR_ENABLED_DEFAULT,
            tool_loop_stagnation_threshold: TOOL_LOOP_STAGNATION_THRESHOLD_DEFAULT,
        }
    }
}

static ACTIVE_SETTINGS: LazyLock<RwLock<TrajectoryRuntimeSettings>> =
    LazyLock::new(|| RwLock::new(TrajectoryRuntimeSettings::default()));

pub fn current() -> TrajectoryRuntimeSettings {
    ACTIVE_SETTINGS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub fn set(settings: TrajectoryRuntimeSettings) {
    *ACTIVE_SETTINGS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings;
}

pub fn update(mutate: impl FnOnce(&mut TrajectoryRuntimeSettings)) {
    let mut active = ACTIVE_SETTINGS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    mutate(&mut active);
}
