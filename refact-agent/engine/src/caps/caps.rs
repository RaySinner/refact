use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use crate::caps::providers::{
    CapsProvider, add_models_to_caps, post_process_provider, read_providers_d,
    resolve_provider_api_key,
};
use crate::caps::model_caps::get_model_caps;
use crate::global_context::GlobalContext;
use refact_core::provider_types::{
    ProjectModelDefaults, ProviderDefaults, project_model_defaults_path,
};

use refact_caps_core::caps_build::{
    apply_model_caps_to_all_chat_models, apply_user_default_chat_model,
    apply_user_default_completion_model, apply_user_default_embedding_model,
    build_chat_model_record, remove_legacy_refact_models_from_caps, validate_default_models,
};
pub use refact_caps_core::caps_build::resolve_completion_model;
pub(crate) use refact_caps_core::caps_build::MODEL_BASE_PROVIDER_HEADER;

const PROVIDER_MODEL_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);

pub use refact_core::llm_types::{
    BaseModelRecord, EmbeddingModelRecord, HasBaseModelRecord, WireFormat, default_embedding_batch,
    default_rejection_threshold, default_true,
};

pub use refact_caps_core::model_records::{
    CapsMetadata, ChatModelRecord, CompletionModelFamily, CompletionModelRecord, DefaultModels,
    default_chat_scratchpad, default_completion_scratchpad, default_completion_scratchpad_patch,
    default_hf_tokenizer_template, normalize_string,
};

pub use refact_caps_core::code_assistant_caps::CodeAssistantCaps;

pub async fn populate_chat_models_from_providers(
    caps: &mut CodeAssistantCaps,
    gcx: Arc<GlobalContext>,
) {
    let model_caps = &*caps.model_caps;

    let (http_client, providers_snapshot) = {
        let registry = gcx.providers.read().await;
        let snapshot: Vec<Box<dyn crate::providers::traits::ProviderTrait>> =
            registry.iter().map(|(_, p)| p.clone_box()).collect();
        (gcx.http_client.clone(), snapshot)
    };

    for provider in &providers_snapshot {
        let runtime = match provider.build_runtime() {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    "Failed to build runtime for provider '{}': {}",
                    provider.name(),
                    e
                );
                continue;
            }
        };

        if !runtime.enabled {
            continue;
        }

        let mut base_provider_names = vec![provider.base_provider_name().to_string()];
        if runtime.name != provider.name() {
            base_provider_names.push(provider.name().to_string());
        }
        base_provider_names.sort();
        base_provider_names.dedup();

        let available_models = match tokio::time::timeout(
            PROVIDER_MODEL_DISCOVERY_TIMEOUT,
            provider.fetch_available_models(&http_client, model_caps),
        )
        .await
        {
            Ok(models) => models,
            Err(_) => {
                warn!(
                    "Timed out fetching available models for provider '{}'; using model_caps fallback",
                    provider.name()
                );
                provider.get_available_models_from_caps(model_caps)
            }
        };

        let runtime_credential = provider.credential();

        for model in available_models {
            if !model.enabled {
                continue;
            }

            let chat_record = build_chat_model_record(
                &runtime.name,
                &base_provider_names,
                &model,
                model_caps,
                runtime.wire_format,
                &runtime.chat_endpoint,
                &runtime.api_key,
                &runtime.auth_token,
                &runtime.tokenizer_api_key,
                &runtime.extra_headers,
                runtime_credential,
                runtime.supports_cache_control,
            );

            let model_id = chat_record.base.id.clone();

            let map = caps
                .provider_base_names
                .entry(runtime.name.clone())
                .or_default();
            map.extend(base_provider_names.iter().cloned());
            map.sort();
            map.dedup();

            caps.chat_models.insert(model_id, Arc::new(chat_record));
        }
    }

    // Chat default model slots are intentionally not auto-filled. The Default Models UI can
    // leave each slot unset, and tools that require a model type report a setup error instead
    // of silently falling back to another model type.

    if !caps.completion_models.is_empty() {
        let need_new_default = caps.defaults.completion_default_model.is_empty()
            || !caps
                .completion_models
                .contains_key(&caps.defaults.completion_default_model);

        if need_new_default {
            let mut candidates: Vec<&String> = caps.completion_models.keys().collect();
            candidates.sort();
            if let Some(first_model_id) = candidates.first() {
                info!(
                    "Auto-selecting default completion model: {}",
                    first_model_id
                );
                caps.defaults.completion_default_model = (*first_model_id).clone();
            }
        }
    }
}

async fn take_models_dev_startup_refresh_flag(gcx: Arc<GlobalContext>) -> bool {
    let caps_state = gcx.caps_state.clone();
    let mut caps_state = caps_state.write().await;
    if caps_state.models_dev_startup_refresh_attempted {
        false
    } else {
        caps_state.models_dev_startup_refresh_attempted = true;
        true
    }
}

pub async fn project_model_defaults_state(
    gcx: Arc<GlobalContext>,
) -> (Option<std::path::PathBuf>, u64) {
    let project_root = crate::files_correction::get_project_dirs(gcx)
        .await
        .into_iter()
        .next();
    let Some(root) = project_root.as_deref() else {
        return (None, 0);
    };
    let mtime = tokio::fs::metadata(project_model_defaults_path(root))
        .await
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    (project_root, mtime)
}

#[allow(deprecated)]
pub async fn load_caps(
    _cmdline: crate::global_context::CommandLine,
    gcx: Arc<GlobalContext>,
) -> Result<Arc<CodeAssistantCaps>, String> {
    crate::providers::oauth_refresh::refresh_expiring_claude_code_tokens(&gcx).await;
    let (config_dir, cmdline_api_key, experimental) = {
        (
            gcx.config_dir.clone(),
            String::new(),
            gcx.cmdline.experimental,
        )
    };

    let mut caps = CodeAssistantCaps::default();
    let server_providers = Vec::new();

    let (mut providers, error_log): (Vec<CapsProvider>, Vec<_>) =
        read_providers_d(server_providers, &config_dir, experimental).await;
    providers.retain(|p| p.enabled);
    for e in error_log {
        tracing::error!("{e}");
    }
    for provider in &mut providers {
        post_process_provider(provider, false, experimental);
        provider.api_key = resolve_provider_api_key(&provider, &cmdline_api_key);
        if !provider.base_provider.is_empty() && provider.base_provider != provider.name {
            caps.provider_base_names
                .entry(provider.name.clone())
                .or_default()
                .push(provider.base_provider.clone());
        }
    }

    let force_models_dev_refresh = take_models_dev_startup_refresh_flag(gcx.clone()).await;
    if force_models_dev_refresh {
        info!("Refreshing models.dev catalog on engine startup");
    }
    let model_caps_map = get_model_caps(gcx.clone(), force_models_dev_refresh).await.map_err(|e| {
        format!("Failed to load models.dev capabilities. Check the bundled snapshot or runtime cache: {e}")
    })?;
    caps.model_caps = Arc::new(model_caps_map);

    // Clear chat models from legacy CapsProviders that have a new ProviderTrait implementation.
    // The new system (populate_chat_models_from_providers) is the sole source of truth for
    // chat models — it respects enabled_models selection. Legacy running_models from YAML
    // templates would otherwise bypass model selection, showing all template models.
    // Only chat_models are cleared; completion_models and embedding_model are preserved
    // since the new system doesn't handle those yet.
    {
        let registry = gcx.providers.read().await;
        for p in &mut providers {
            if registry.get(&p.name).is_some() {
                p.chat_models.clear();
            }
        }
    }

    let embedding_models = add_models_to_caps(&mut caps, providers);
    populate_chat_models_from_providers(&mut caps, gcx.clone()).await;
    apply_model_caps_to_all_chat_models(&mut caps);
    remove_legacy_refact_models_from_caps(&mut caps);

    let project_root = crate::files_correction::get_project_dirs(gcx.clone())
        .await
        .into_iter()
        .next();

    match ProviderDefaults::load(&config_dir).await {
        Ok(mut user_defaults) => {
            if let Some(root) = project_root.as_deref() {
                match ProjectModelDefaults::load(root).await {
                    Ok(project_defaults) => {
                        user_defaults.apply_project_overrides(&project_defaults);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to load project model defaults from {}: {}",
                            project_model_defaults_path(root).display(),
                            e
                        );
                    }
                }
            }
            apply_user_default_chat_model(
                &mut caps.defaults.chat_default_model,
                &user_defaults.chat,
                "chat",
                &caps.chat_models,
            );
            apply_user_default_chat_model(
                &mut caps.defaults.chat_model_2,
                &user_defaults.chat_model_2,
                "chat 2",
                &caps.chat_models,
            );
            apply_user_default_chat_model(
                &mut caps.defaults.task_planner_agent_model,
                &user_defaults.task_planner_agent_model,
                "task planner agent",
                &caps.chat_models,
            );
            apply_user_default_chat_model(
                &mut caps.defaults.chat_light_model,
                &user_defaults.chat_light,
                "light",
                &caps.chat_models,
            );
            apply_user_default_chat_model(
                &mut caps.defaults.chat_buddy_model,
                &user_defaults.chat_buddy,
                "buddy",
                &caps.chat_models,
            );
            apply_user_default_chat_model(
                &mut caps.defaults.chat_thinking_model,
                &user_defaults.chat_thinking,
                "thinking",
                &caps.chat_models,
            );
            apply_user_default_completion_model(
                &mut caps.defaults.completion_default_model,
                user_defaults.completion_model.as_deref(),
                &caps.completion_models,
            );
            apply_user_default_embedding_model(
                &mut caps.embedding_model,
                user_defaults.embedding_model.as_deref(),
                &embedding_models,
            );
            caps.user_defaults = user_defaults;
        }
        Err(e) => {
            warn!(
                "Failed to load user defaults from providers.d/defaults.yaml: {}",
                e
            );
        }
    }

    validate_default_models(&caps)?;

    Ok(Arc::new(caps))
}

pub use refact_caps_core::model_records::strip_model_from_finetune;

pub use refact_caps_core::model_resolve::{resolve_chat_model, resolve_model};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_models_dev_startup_refresh_flag_is_consumed_once() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let caps_state = gcx.caps_state.clone();
        caps_state
            .write()
            .await
            .models_dev_startup_refresh_attempted = false;

        assert!(take_models_dev_startup_refresh_flag(gcx.clone()).await);
        assert!(!take_models_dev_startup_refresh_flag(gcx).await);
    }
}
