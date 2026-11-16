pub use refact_http::routers::v1::project_information::*;
#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;

    #[tokio::test]
    async fn preview_includes_models_info_block() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let workspace = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() =
            vec![workspace.path().to_path_buf()];
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let mut config = ProjectInformationConfig::default();
        config.sections.system_info.enabled = false;
        config.sections.environment_instructions.enabled = false;
        config.sections.detected_environments.enabled = false;
        config.sections.git_info.enabled = false;
        config.sections.project_tree.enabled = false;
        config.sections.instruction_files.enabled = false;
        config.sections.project_configs.enabled = false;
        config.sections.memories.enabled = false;

        let response = handle_v1_project_information_preview(State(app), axum::Json(config))
            .await
            .unwrap()
            .0;

        assert_eq!(response.blocks.len(), 1);
        let block = &response.blocks[0];
        assert_eq!(block.id, "models_info");
        assert_eq!(block.section, "models_info");
        assert_eq!(block.title, "Models & Access");
        assert!(block.content.contains("## Models & Access Snapshot"));
    }
}
