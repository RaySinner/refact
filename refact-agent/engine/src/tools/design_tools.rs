pub use refact_tools::tools::design_tools::*;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use refact_tools::tools::tools_description::Tool as _;
    use serde_json::json;
    use tokio::sync::Mutex as AMutex;

    use refact_tools::tools::design_tools::{
        artifact_reference_name, design_artifacts_dir, resolve_image_source, ToolVisualDiff,
        ARTIFACT_PATH_PREFIX,
    };

    #[tokio::test]
    async fn image_region_accepts_artifact_paths_and_bare_artifact_names() {
        let app = refact_runtime_api::app_state_from_gcx(
            crate::global_context::tests::make_test_gcx().await,
        )
        .await;
        let artifacts = design_artifacts_dir(&app.tool.paths.cache_dir);
        std::fs::create_dir_all(&artifacts).unwrap();
        let written = artifacts.join("chrome-screenshot-1.png");
        std::fs::write(&written, b"png-bytes").unwrap();

        for requested in [
            written.to_string_lossy().into_owned(),
            "chrome-screenshot-1.png".to_string(),
            format!("{ARTIFACT_PATH_PREFIX}chrome-screenshot-1.png"),
        ] {
            assert_eq!(
                resolve_image_source(&app, None, &requested).await.unwrap(),
                written,
                "{requested}"
            );
        }

        let error = resolve_image_source(&app, None, "nope.png")
            .await
            .unwrap_err();
        assert!(error.contains("artifact.path"), "{error}");
        assert_eq!(
            artifact_reference_name(&format!("{ARTIFACT_PATH_PREFIX}dir/shot.png")).unwrap(),
            "shot.png"
        );
        assert!(artifact_reference_name("   ").is_none());
    }

    #[tokio::test]
    async fn visual_diff_confirms_only_explicit_baseline_updates() {
        let app = refact_runtime_api::app_state_from_gcx(
            crate::global_context::tests::make_test_gcx().await,
        )
        .await;
        let ccx = Arc::new(AMutex::new(
            crate::at_commands::at_commands::at_commands_new_from_app(
                app,
                4096,
                10,
                false,
                Vec::new(),
                "chat".to_string(),
                None,
                "model".to_string(),
                None,
                None,
            )
            .await,
        ));
        let tool = ToolVisualDiff {
            config_path: "builtin".to_string(),
        };
        let confirm = tool
            .match_against_confirm_deny(
                ccx.clone(),
                &HashMap::from([("update_baseline".to_string(), json!(true))]),
            )
            .await
            .unwrap();
        assert_eq!(
            confirm.result,
            crate::tools::tools_description::MatchConfirmDenyResult::CONFIRMATION
        );
        let pass = tool
            .match_against_confirm_deny(ccx, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(
            pass.result,
            crate::tools::tools_description::MatchConfirmDenyResult::PASS
        );
    }
}
