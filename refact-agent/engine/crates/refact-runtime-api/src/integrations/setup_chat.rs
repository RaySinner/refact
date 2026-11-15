use std::sync::Arc;

use refact_chat_api::ChatMeta;
use refact_core::chat_types::{ChatContent, ChatMessage};
use refact_scratchpads::scratchpad_utils::HasRagResults;

use crate::global_context::{GlobalContext, GlobalContextCtx};
use crate::integrations::setting_up_integrations::integrations_all;

pub async fn mix_setup_messages(
    gcx: Arc<GlobalContext>,
    chat_meta: &ChatMeta,
    messages: &mut Vec<ChatMessage>,
    stream_back_to_user: &mut HasRagResults,
) {
    assert!(messages[0].role != "system");

    let mut sp_text = match crate::customization_registry_gcx::get_mode_config(
        gcx.clone(),
        "setup",
        None,
    )
    .await
    {
        Some(mode_config) => mode_config.prompt,
        None => {
            tracing::error!("Mode 'setup' not found");
            String::new()
        }
    };

    if sp_text.contains("%ALL_INTEGRATIONS%") {
        let allow_experimental = gcx.cmdline.experimental;
        let all_integrations = gcx
            .context_factory
            .get()
            .integration_provider()
            .integration_names(allow_experimental);
        sp_text = sp_text.replace("%ALL_INTEGRATIONS%", &all_integrations.join(", "));
    }

    if sp_text.contains("%AVAILABLE_INTEGRATIONS%") {
        let integrations_all = integrations_all(gcx.clone(), false).await.integrations;
        let integrations = integrations_all
            .iter()
            .filter(|x| x.integr_config_exists && x.project_path.is_empty())
            .collect::<Vec<_>>();
        sp_text = sp_text.replace(
            "%AVAILABLE_INTEGRATIONS%",
            &integrations
                .iter()
                .map(|x| x.integr_name.clone())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }

    let ctx = gcx.chat_ctx(gcx.clone());
    sp_text = ctx
        .prompts
        .system_prompt_add_extra_instructions(
            sp_text,
            ctx.tool_registry.get_tool_names_for_mode("setup", None).await,
            chat_meta,
            &None,
            "setup",
        )
        .await;

    let system_message = ChatMessage {
        role: "system".to_string(),
        content: ChatContent::SimpleText(sp_text),
        ..Default::default()
    };

    if messages.len() == 1 {
        stream_back_to_user.push_in_json(serde_json::json!(system_message));
    } else {
        tracing::error!(
            "more than 1 message when mixing setup chat context, bad things might happen!"
        );
    }

    messages.splice(0..0, vec![system_message]);
}
