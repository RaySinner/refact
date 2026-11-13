pub use refact_http::routers::v1::settings_config::*;

#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use axum::extract::{Query, State};
#[cfg(test)]
use hyper::StatusCode;
#[cfg(test)]
use crate::app_state::AppState;

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Arc;

    async fn trajectory_settings_app() -> AppState {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        crate::app_state::app_state_from_gcx(gcx).await
    }

    /// The field metadata table and `runtime_settings::validate` are two
    /// hand-maintained parallel lists. If they drift, the GUI either blocks
    /// values the engine accepts or offers values the engine rejects with a
    /// 400. This test is the only thing keeping them in agreement.
    #[test]
    fn trajectory_settings_fields_match_struct_and_validate_bounds() {
        let defaults = crate::runtime_settings::TrajectoryRuntimeSettings::default();
        let defaults_json = serde_json::to_value(&defaults).unwrap();
        let serialized_names: std::collections::BTreeSet<String> = defaults_json
            .as_object()
            .expect("settings serialize as a JSON object")
            .keys()
            .cloned()
            .collect();

        let fields = trajectory_settings_fields();
        let field_names: std::collections::BTreeSet<String> =
            fields.iter().map(|field| field.name.to_string()).collect();
        assert_eq!(
            field_names.len(),
            fields.len(),
            "trajectory_settings_fields() contains duplicate names"
        );
        assert_eq!(
            field_names, serialized_names,
            "trajectory_settings_fields() must list exactly the serialized settings fields"
        );

        // Every advertised min/max must be precisely what validate() enforces.
        for field in &fields {
            let (minimum, maximum) = match (field.minimum, field.maximum) {
                (Some(minimum), Some(maximum)) => (minimum, maximum),
                _ => {
                    assert_eq!(
                        field.value_type, "boolean",
                        "{} has no bounds but is not a boolean",
                        field.name
                    );
                    continue;
                }
            };
            assert_eq!(field.value_type, "integer");
            assert!(minimum < maximum, "{} has an empty range", field.name);

            let with_value = |value: u64| {
                let mut json = defaults_json.clone();
                json[field.name] = serde_json::json!(value);
                let settings: crate::runtime_settings::TrajectoryRuntimeSettings =
                    serde_json::from_value(json).unwrap_or_else(|e| {
                        panic!("{} is not settable to {value}: {e}", field.name)
                    });
                crate::runtime_settings::validate(&settings)
            };
            let range_error = format!("{} must be between {} and {}", field.name, minimum, maximum);

            // Boundaries are accepted (a cross-field rule may still complain,
            // but never about this field's range).
            for accepted in [minimum, maximum] {
                if let Err(error) = with_value(accepted) {
                    assert!(
                        !error.starts_with(&range_error),
                        "{} rejects in-range value {accepted}: {error}",
                        field.name
                    );
                }
            }
            // Just outside the boundaries is rejected with exactly these bounds.
            // A zero minimum has no representable value below it.
            let below = minimum.checked_sub(1);
            for rejected in below.into_iter().chain([maximum + 1]) {
                let error = with_value(rejected).unwrap_err();
                assert_eq!(
                    error,
                    format!("{range_error}; got {rejected}"),
                    "{} bounds disagree between fields() and validate()",
                    field.name
                );
            }
        }
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn trajectory_settings_defaults_round_trip_and_persist() {
        crate::runtime_settings::reset_for_test();
        let app = trajectory_settings_app().await;
        let initial = handle_v1_trajectory_settings_get(State(app.clone()))
            .await
            .unwrap()
            .0;
        assert_eq!(
            initial.config,
            crate::runtime_settings::TrajectoryRuntimeSettings::default()
        );
        assert_eq!(initial.fields.len(), 55);
        assert!(initial
            .fields
            .iter()
            .any(|field| field.name == "event_channel_capacity"
                && field.apply_mode == "restart_required"));
        for name in [
            "trajectory_writer_enabled",
            "trajectory_index_coordinator_enabled",
            "trajectory_watcher_self_write_enabled",
            "tool_catalog_snapshots_enabled",
            "vecdb_path_coalescing_enabled",
        ] {
            assert!(initial
                .fields
                .iter()
                .any(|field| field.name == name && field.value_type == "boolean"));
        }
        assert!(initial.fields.iter().any(|field| field.name
            == "trajectory_watcher_self_write_enabled"
            && field.apply_mode == "live"));
        assert!(initial.defaults.trajectory_writer_enabled);
        assert!(initial.defaults.trajectory_index_coordinator_enabled);
        assert!(initial.defaults.trajectory_watcher_self_write_enabled);
        assert!(initial.defaults.tool_catalog_snapshots_enabled);
        assert!(initial.defaults.vecdb_path_coalescing_enabled);

        let mut updated = initial.config.clone();
        updated.internal_traces_keep_per_folder = 25;
        updated.session_idle_timeout_secs = 120;
        updated.auto_enrichment_total_token_cap = 640;
        updated.auto_enrichment_card_token_cap = 320;
        let saved = handle_v1_trajectory_settings_post(
            State(app.clone()),
            hyper::body::Bytes::from(serde_json::to_vec(&updated).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(saved.config, updated);
        assert_eq!(
            crate::runtime_settings::current().session_idle_timeout_secs,
            120
        );
        let reloaded = handle_v1_trajectory_settings_get(State(app))
            .await
            .unwrap()
            .0;
        assert_eq!(reloaded.config, updated);
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn trajectory_settings_validation_preserves_previous_configuration() {
        crate::runtime_settings::reset_for_test();
        let app = trajectory_settings_app().await;
        let mut valid = crate::runtime_settings::TrajectoryRuntimeSettings::default();
        valid.max_queue_size = 7;
        let _ = handle_v1_trajectory_settings_post(
            State(app.clone()),
            hyper::body::Bytes::from(serde_json::to_vec(&valid).unwrap()),
        )
        .await
        .unwrap();
        let mut invalid = valid.clone();
        invalid.max_queue_size = 0;
        let error = handle_v1_trajectory_settings_post(
            State(app.clone()),
            hyper::body::Bytes::from(serde_json::to_vec(&invalid).unwrap()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("max_queue_size"));
        assert_eq!(
            handle_v1_trajectory_settings_get(State(app))
                .await
                .unwrap()
                .0
                .config,
            valid
        );
    }

    #[tokio::test]
    #[serial(runtime_settings)]
    async fn trajectory_settings_live_values_apply_and_restart_values_wait() {
        crate::runtime_settings::reset_for_test();
        let app = trajectory_settings_app().await;
        let initial_capacity = refact_chat_history::config::limits().event_channel_capacity;
        let mut updated = crate::runtime_settings::TrajectoryRuntimeSettings::default();
        updated.max_queue_size = 9;
        updated.event_channel_capacity = initial_capacity + 100;
        updated.auto_enrichment_total_token_cap = 800;
        updated.auto_enrichment_card_token_cap = 400;
        updated.trajectory_writer_enabled = false;
        updated.trajectory_index_coordinator_enabled = false;
        updated.trajectory_watcher_self_write_enabled = false;
        updated.tool_catalog_snapshots_enabled = false;
        updated.vecdb_path_coalescing_enabled = false;
        let saved = handle_v1_trajectory_settings_post(
            State(app.clone()),
            hyper::body::Bytes::from(serde_json::to_vec(&updated).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(saved.0.config, updated);
        assert_eq!(refact_chat_api::max_queue_size(), 9);
        assert_eq!(
            refact_chat_history::config::limits().event_channel_capacity,
            initial_capacity
        );
        assert_eq!(
            crate::runtime_settings::current().auto_enrichment_total_token_cap,
            800
        );
        assert!(crate::runtime_settings::current().trajectory_writer_enabled);
        assert!(crate::runtime_settings::current().trajectory_index_coordinator_enabled);
        assert!(!crate::runtime_settings::current().trajectory_watcher_self_write_enabled);
        assert!(crate::runtime_settings::current().tool_catalog_snapshots_enabled);
        assert!(crate::runtime_settings::current().vecdb_path_coalescing_enabled);

        let mut mixed = updated.clone();
        mixed.trajectory_writer_enabled = true;
        mixed.trajectory_index_coordinator_enabled = false;
        mixed.trajectory_watcher_self_write_enabled = true;
        mixed.tool_catalog_snapshots_enabled = false;
        mixed.vecdb_path_coalescing_enabled = true;
        let mixed_saved = handle_v1_trajectory_settings_post(
            State(app.clone()),
            hyper::body::Bytes::from(serde_json::to_vec(&mixed).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(mixed_saved.0.config, mixed);
        assert!(crate::runtime_settings::current().trajectory_writer_enabled);
        assert!(crate::runtime_settings::current().trajectory_index_coordinator_enabled);
        assert!(crate::runtime_settings::current().trajectory_watcher_self_write_enabled);
        assert!(crate::runtime_settings::current().tool_catalog_snapshots_enabled);
        assert!(crate::runtime_settings::current().vecdb_path_coalescing_enabled);
        assert_eq!(
            handle_v1_trajectory_settings_get(State(app))
                .await
                .unwrap()
                .0
                .config,
            mixed
        );
    }

    #[test]
    #[serial(runtime_settings)]
    fn trajectory_settings_environment_rollout_override_wins() {
        let cases = [
            (
                crate::chat::trajectories::TRAJECTORY_WRITER_ENV,
                crate::chat::trajectories::trajectory_writer_rollout_enabled_for
                    as fn(Option<&str>) -> bool,
            ),
            (
                crate::chat::trajectory_index::TRAJECTORY_INDEX_COORDINATOR_ENV,
                crate::chat::trajectory_index::trajectory_index_coordinator_rollout_enabled_for,
            ),
            (
                crate::chat::trajectories::TRAJECTORY_WATCHER_SELF_WRITE_ENV,
                crate::chat::trajectories::trajectory_watcher_self_write_rollout_enabled_for,
            ),
            (
                crate::app_state::TOOL_CATALOG_SNAPSHOTS_ENV,
                crate::app_state::tool_catalog_snapshot_rollout_enabled_for,
            ),
        ];
        for (key, enabled) in cases {
            let previous = std::env::var_os(key);
            std::env::set_var(key, "0");
            assert!(!enabled(std::env::var(key).ok().as_deref()));
            std::env::set_var(key, "on");
            assert!(enabled(std::env::var(key).ok().as_deref()));
            if let Some(previous) = previous {
                std::env::set_var(key, previous);
            } else {
                std::env::remove_var(key);
            }
        }
        let previous = std::env::var_os(refact_vecdb::vdb_thread::VECDB_PATH_COALESCING_ENV);
        std::env::set_var(refact_vecdb::vdb_thread::VECDB_PATH_COALESCING_ENV, "0");
        assert!(!refact_vecdb::vdb_thread::vecdb_path_coalescing_rollout_enabled());
        std::env::set_var(refact_vecdb::vdb_thread::VECDB_PATH_COALESCING_ENV, "on");
        assert!(refact_vecdb::vdb_thread::vecdb_path_coalescing_rollout_enabled());
        if let Some(previous) = previous {
            std::env::set_var(
                refact_vecdb::vdb_thread::VECDB_PATH_COALESCING_ENV,
                previous,
            );
        } else {
            std::env::remove_var(refact_vecdb::vdb_thread::VECDB_PATH_COALESCING_ENV);
        }
    }

    #[tokio::test]
    async fn global_and_project_indexing_load_save_and_absent_project() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut gcx = crate::global_context::tests::make_test_gcx().await;
        let mutable = Arc::get_mut(&mut gcx).expect("test owns context");
        mutable.config_dir = temp.path().join("config");
        *mutable.documents_state.workspace_folders.lock().unwrap() = vec![project.clone()];
        let app = crate::app_state::app_state_from_gcx(gcx).await;

        let absent = handle_v1_indexing_settings_get(
            State(app.clone()),
            Query(SettingsScopeQuery {
                scope: Some("project".into()),
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(absent.config.blocklist.is_empty());
        assert!(!project.join(".refact/indexing.yaml").exists());

        let body = serde_json::json!({"config": {
            "blocklist": [" target ", "target", ""],
            "additional_indexing_dirs": [" docs ", "docs"]
        }});
        let saved = handle_v1_indexing_settings_post(
            State(app.clone()),
            Query(SettingsScopeQuery {
                scope: Some("project".into()),
            }),
            hyper::body::Bytes::from(serde_json::to_vec(&body).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(saved.config.blocklist, vec!["target"]);
        assert_eq!(saved.config.additional_indexing_dirs, vec!["docs"]);
        let reloaded = load_indexing_config(&project.join(".refact/indexing.yaml"), false)
            .await
            .unwrap();
        assert_eq!(reloaded.blocklist, vec!["target"]);

        let global_body = serde_json::json!({
            "blocklist": [" generated ", "generated"],
            "additional_indexing_dirs": [temp.path().to_string_lossy()]
        });
        let global = handle_v1_indexing_settings_post(
            State(app.clone()),
            Query(SettingsScopeQuery {
                scope: Some("global".into()),
            }),
            hyper::body::Bytes::from(serde_json::to_vec(&global_body).unwrap()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(global.config.blocklist, vec!["generated"]);
        assert!(Path::new(&global.path).exists());
        let loaded_global = handle_v1_indexing_settings_get(
            State(app),
            Query(SettingsScopeQuery {
                scope: Some("global".into()),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(loaded_global.config.blocklist, vec!["generated"]);
    }

    #[tokio::test]
    async fn malformed_indexing_payload_and_unknown_skills_policy_are_rejected() {
        let malformed = serde_json::from_slice::<IndexingSettingsPost>(
            br#"{"config":{"blocklist":"bad","additional_indexing_dirs":[]}}"#,
        );
        assert!(malformed.is_err());
        let unknown = serde_json::from_slice::<SkillsSettings>(br#"{"auto_trigger":"sometimes"}"#);
        assert!(unknown.is_err());
    }

    #[test]
    fn project_scope_rejects_home_relative_indexing_directories() {
        let error = validate_additional_dirs("project", &["~/outside".to_string()]).unwrap_err();

        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("cannot use ~/"));
    }

    #[tokio::test]
    async fn skills_default_and_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut gcx = crate::global_context::tests::make_test_gcx().await;
        let mutable = Arc::get_mut(&mut gcx).expect("test owns context");
        *mutable.documents_state.workspace_folders.lock().unwrap() = vec![project.clone()];
        let app = crate::app_state::app_state_from_gcx(gcx).await;

        let default = handle_v1_skills_settings_get(State(app.clone()))
            .await
            .unwrap()
            .0;
        assert_eq!(default.auto_trigger, SkillsAutoTriggerSetting::IndexOnly);
        let saved = handle_v1_skills_settings_post(
            State(app.clone()),
            hyper::body::Bytes::from_static(br#"{"auto_trigger":"inject_full"}"#),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(saved.auto_trigger, SkillsAutoTriggerSetting::InjectFull);
        let loaded = handle_v1_skills_settings_get(State(app)).await.unwrap().0;
        assert_eq!(loaded, saved);
    }

    #[tokio::test]
    async fn skills_without_project_root_is_bad_request() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let error = handle_v1_skills_settings_get(State(app)).await.unwrap_err();
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("no project root"));
    }
}
