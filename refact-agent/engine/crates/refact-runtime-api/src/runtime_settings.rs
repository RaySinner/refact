use std::time::Duration;

pub use refact_core::trajectory_settings::*;

/// Push the live crate-backed limits into every consuming crate.
///
/// Called from both `install_startup` and `install_live`: none of these limits
/// size a boot-time allocation, so all of them apply immediately.
fn install_crate_limits(settings: &TrajectoryRuntimeSettings) {
    refact_postprocessing::config::install_postprocessing_limits(
        refact_postprocessing::config::PostprocessingLimits {
            max_tool_budget_tokens: settings.pp_max_tool_budget_tokens,
            max_per_file_budget_tokens: settings.pp_max_per_file_budget_tokens,
            max_line_length_chars: settings.pp_max_line_length_chars,
            tokens_for_text_percent: settings.pp_tokens_for_text_percent,
        },
    );
    refact_git_intel::config::install_git_intel_limits(refact_git_intel::config::GitIntelLimits {
        max_commits: settings.git_intel_max_commits,
        deep_walk_limit: settings.git_intel_deep_walk_limit,
        max_files_per_commit_for_cochange: settings.git_intel_max_files_per_commit_cochange,
        max_files_per_commit_for_entropy: settings.git_intel_max_files_per_commit_entropy,
    });
    refact_codegraph::config::install_codegraph_limits(refact_codegraph::config::CodegraphLimits {
        dead_code_max_results: settings.codegraph_dead_code_max_results,
        exec_flow_max_nodes: settings.codegraph_exec_flow_max_nodes,
    });
    refact_vecdb::vdb_thread::install_vecdb_trajectory_split_bytes(
        settings.vecdb_trajectory_split_bytes,
    );
}

/// Applies every crate-backed limit, then stores the snapshot itself.
pub fn install_startup(settings: TrajectoryRuntimeSettings) {
    install_crate_limits(&settings);
    refact_vecdb::vdb_thread::install_vecdb_path_coalescing_setting(
        settings.vecdb_path_coalescing_enabled,
    );
    refact_chat_history::config::install_runtime_config(refact_chat_history::config::ChatConfig {
        limits: refact_chat_history::config::ChatLimits {
            max_queue_size: settings.max_queue_size,
            event_channel_capacity: settings.event_channel_capacity,
            recent_request_ids_capacity: settings.recent_request_ids_capacity,
            max_images_per_message: settings.max_images_per_message,
            max_parallel_tools: settings.max_parallel_tools.unwrap_or(usize::MAX),
            max_file_size: settings.max_file_size,
        },
        ..Default::default()
    });
    refact_chat_api::install_runtime_timeouts(refact_chat_api::RuntimeChatTimeouts {
        max_queue_size: settings.max_queue_size,
        session_idle: Duration::from_secs(settings.session_idle_timeout_secs),
        session_cleanup_interval: Duration::from_secs(settings.session_cleanup_interval_secs),
        stream_idle: Duration::from_secs(settings.stream_idle_timeout_secs),
        stream_total: Duration::from_secs(settings.stream_total_timeout_secs),
    });
    set(settings);
}

/// Applies live limits without a restart, keeping the switches that only take
/// effect on boot frozen at their active values.
pub fn install_live(settings: &TrajectoryRuntimeSettings) {
    let active_before = current();
    install_crate_limits(settings);
    refact_chat_history::config::apply_live_limits(refact_chat_history::config::ChatLimits {
        max_queue_size: settings.max_queue_size,
        event_channel_capacity: active_before.event_channel_capacity,
        recent_request_ids_capacity: settings.recent_request_ids_capacity,
        max_images_per_message: settings.max_images_per_message,
        max_parallel_tools: settings.max_parallel_tools.unwrap_or(usize::MAX),
        max_file_size: settings.max_file_size,
    });
    refact_chat_api::install_runtime_timeouts(refact_chat_api::RuntimeChatTimeouts {
        max_queue_size: settings.max_queue_size,
        session_idle: Duration::from_secs(settings.session_idle_timeout_secs),
        session_cleanup_interval: Duration::from_secs(settings.session_cleanup_interval_secs),
        stream_idle: Duration::from_secs(settings.stream_idle_timeout_secs),
        stream_total: Duration::from_secs(settings.stream_total_timeout_secs),
    });
    update(|active| {
        let restart_required = (
            active.event_channel_capacity,
            active.trajectory_writer_enabled,
            active.trajectory_index_coordinator_enabled,
            active.tool_catalog_snapshots_enabled,
            active.vecdb_path_coalescing_enabled,
        );
        *active = settings.clone();
        active.event_channel_capacity = restart_required.0;
        active.trajectory_writer_enabled = restart_required.1;
        active.trajectory_index_coordinator_enabled = restart_required.2;
        active.tool_catalog_snapshots_enabled = restart_required.3;
        active.vecdb_path_coalescing_enabled = restart_required.4;
    });
}

#[cfg(any(test, feature = "test-util"))]
pub fn reset_for_test() {
    install_startup(TrajectoryRuntimeSettings::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Every crate-backed limit, tweaked away from its default so a stale
    /// installer cannot pass by accident.
    fn tweaked() -> TrajectoryRuntimeSettings {
        TrajectoryRuntimeSettings {
            pp_max_tool_budget_tokens: 262_144,
            pp_max_per_file_budget_tokens: 65_536,
            pp_max_line_length_chars: 4_096,
            pp_tokens_for_text_percent: 42,
            git_intel_max_commits: 1_234,
            git_intel_deep_walk_limit: 23_456,
            git_intel_max_files_per_commit_cochange: 321,
            git_intel_max_files_per_commit_entropy: 77,
            codegraph_dead_code_max_results: 654,
            codegraph_exec_flow_max_nodes: 987,
            vecdb_trajectory_split_bytes: 4_096,
            ..TrajectoryRuntimeSettings::default()
        }
    }

    fn assert_crate_limits_match(settings: &TrajectoryRuntimeSettings) {
        let pp = refact_postprocessing::config::limits();
        assert_eq!(
            pp.max_tool_budget_tokens,
            settings.pp_max_tool_budget_tokens
        );
        assert_eq!(
            pp.max_per_file_budget_tokens,
            settings.pp_max_per_file_budget_tokens
        );
        assert_eq!(pp.max_line_length_chars, settings.pp_max_line_length_chars);
        assert_eq!(
            pp.tokens_for_text_percent,
            settings.pp_tokens_for_text_percent
        );

        let git = refact_git_intel::config::limits();
        assert_eq!(git.max_commits, settings.git_intel_max_commits);
        assert_eq!(git.deep_walk_limit, settings.git_intel_deep_walk_limit);
        assert_eq!(
            git.max_files_per_commit_for_cochange,
            settings.git_intel_max_files_per_commit_cochange
        );
        assert_eq!(
            git.max_files_per_commit_for_entropy,
            settings.git_intel_max_files_per_commit_entropy
        );

        let cg = refact_codegraph::config::limits();
        assert_eq!(
            cg.dead_code_max_results,
            settings.codegraph_dead_code_max_results
        );
        assert_eq!(
            cg.exec_flow_max_nodes,
            settings.codegraph_exec_flow_max_nodes
        );
    }

    #[test]
    #[serial(runtime_settings)]
    fn install_startup_propagates_every_crate_backed_limit() {
        let settings = tweaked();
        assert!(validate(&settings).is_ok());
        install_startup(settings.clone());
        assert_crate_limits_match(&settings);
        assert_eq!(
            refact_vecdb::vdb_thread::vecdb_trajectory_split_bytes(),
            settings.vecdb_trajectory_split_bytes
        );
        assert_eq!(
            current().pp_max_tool_budget_tokens,
            settings.pp_max_tool_budget_tokens
        );
        reset_for_test();
    }

    #[test]
    #[serial(runtime_settings)]
    fn install_live_propagates_every_crate_backed_limit_without_restart() {
        reset_for_test();
        let settings = tweaked();
        install_live(&settings);
        assert_crate_limits_match(&settings);
        assert_eq!(
            refact_vecdb::vdb_thread::vecdb_trajectory_split_bytes(),
            settings.vecdb_trajectory_split_bytes
        );
        // None of the new limits are carved out as restart-required, so the
        // active snapshot must reflect them immediately.
        assert_eq!(current(), settings);
        reset_for_test();
    }
}
