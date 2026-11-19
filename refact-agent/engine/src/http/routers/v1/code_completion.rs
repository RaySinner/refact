pub use refact_http::routers::v1::code_completion::*;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use hyper::StatusCode;

    use refact_core::call_validation::CodeCompletionPost;
    use refact_core::chat_types::{CodeCompletionInputs, CursorPosition, SamplingParameters};
    use refact_runtime_api::app_state::AppState;
    use refact_runtime_api::global_context::GlobalContext;
    use refact_scratchpads::completion_cache;

    use super::*;
    use crate::caps::{BaseModelRecord, CodeAssistantCaps, CompletionModelRecord};

    async fn completion_context(
        file_name: &str,
        restricted_destinations: &[&str],
        normal_destinations: &[&str],
    ) -> (Arc<GlobalContext>, AppState, CodeCompletionPost) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let source_path = gcx.config_dir.join(file_name);
        tokio::fs::write(&source_path, "fn main() {}\n")
            .await
            .unwrap();
        let restricted_destinations = restricted_destinations
            .iter()
            .map(|destination| format!("        - {destination}"))
            .collect::<Vec<_>>()
            .join("\n");
        let normal_destinations = normal_destinations
            .iter()
            .map(|destination| format!("        - {destination}"))
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(
            gcx.config_dir.join("privacy.yaml"),
            format!(
                "privacy_rules:\n  zones:\n    - name: excluded\n      patterns:\n        - \"*denied.rs\"\n      send_to:\n{restricted_destinations}\n    - name: normal\n      patterns:\n        - \"*\"\n      send_to:\n{normal_destinations}\n"
            ),
        )
        .await
        .unwrap();

        let model_id = "trusted/completion-model";
        let mut caps = CodeAssistantCaps::default();
        caps.defaults.completion_default_model = model_id.to_string();
        caps.completion_models.insert(
            model_id.to_string(),
            Arc::new(CompletionModelRecord {
                base: BaseModelRecord {
                    id: model_id.to_string(),
                    name: "completion-model".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        );
        let mut caps_state = gcx.caps_state.write().await;
        caps_state.caps = Some(Arc::new(caps));
        caps_state.last_attempted_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_add(60);
        drop(caps_state);

        let path = source_path.to_string_lossy().into_owned();
        let post = CodeCompletionPost {
            inputs: CodeCompletionInputs {
                sources: HashMap::from([(path.clone(), "fn main() {}\n".to_string())]),
                cursor: CursorPosition {
                    file: path,
                    line: 0,
                    character: 0,
                },
                multiline: false,
            },
            parameters: SamplingParameters::default(),
            model: model_id.to_string(),
            stream: false,
            no_cache: false,
            use_ast: false,
            use_vecdb: false,
            rag_tokens_n: 0,
        };
        let app = crate::app_state::app_state_from_gcx(gcx.clone()).await;
        (gcx, app, post)
    }

    #[tokio::test]
    async fn completion_for_excluded_zone_returns_422() {
        let (gcx, app, mut post) = completion_context("denied.rs", &["other"], &["trusted"]).await;
        let cache = gcx.completions_cache.clone();
        let _ = std::thread::spawn(move || {
            let _guard = cache.write().unwrap();
            panic!("poison completion cache");
        })
        .join();

        let error = handle_v1_code_completion(app, &mut post).await.unwrap_err();

        assert_eq!(error.status_code, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(error.message.contains("trusted"));
    }

    #[tokio::test]
    async fn completion_for_allowed_normal_zone_uses_normal_path() {
        let (gcx, app, mut post) = completion_context("allowed.rs", &["other"], &["trusted"]).await;
        let cached = serde_json::json!({
            "choices": [{
                "index": 0,
                "code_completion": "cached completion",
                "finish_reason": "stop"
            }],
            "cached": true
        });
        completion_cache::cache_put(
            gcx.completions_cache.clone(),
            completion_cache::cache_key_from_post(&post),
            cached,
        );

        let response = handle_v1_code_completion(app, &mut post).await.unwrap();
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["cached"], true);
        assert_eq!(value["choices"][0]["code_completion"], "cached completion");
    }
}
