use axum::http::{Response, StatusCode};
use axum::extract::State;
use hyper::Body;
use serde::{Deserialize, Serialize};

use refact_runtime_api::app_state::AppState;
use refact_runtime_api::GlobalContextCtx;
use refact_runtime_api::ToolRegistry as _;
use refact_core::call_validation::{ChatContent, ChatMessage};
use refact_chat_api::ChatMeta;
use refact_runtime_api::call_validation::validate_mode_for_request;
use refact_runtime_api::session_runtime::get_or_create_session_with_trajectory;
use refact_runtime_api::trajectories::store::maybe_save_trajectory_with_intent;
use refact_runtime_api::trajectory_prefix::{ensure_frozen_prefix, new_frozen_request_prefix};
use refact_core::custom_error::ScratchError;
use refact_scratchpads::scratchpad_utils::HasRagResults;
use refact_tools::chat_prepare::build_canonical_openai_tools;
use refact_tools::chat_prompts::prepend_the_right_system_prompt_and_maybe_more_initial_messages;

use crate::indexing_utils::wait_for_indexing_if_needed;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PrependSystemPromptPost {
    pub messages: Vec<ChatMessage>,
    pub chat_meta: ChatMeta,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PrependSystemPromptResponse {
    pub messages: Vec<ChatMessage>,
    pub messages_to_stream_back: Vec<serde_json::Value>,
}

pub async fn persist_init_frozen_prefix(
    app: AppState,
    chat_id: &str,
    frozen_prefix: refact_chat_api::FrozenRequestPrefix,
) {
    let session_arc =
        get_or_create_session_with_trajectory(app.chat_ctx(), &app.chat.sessions, chat_id).await;
    let installed = {
        let mut session = session_arc.lock().await;
        ensure_frozen_prefix(
            &mut session,
            frozen_prefix.system_prompt.clone(),
            frozen_prefix.tools_canonical.clone(),
        )
        .is_some()
    };
    if installed {
        maybe_save_trajectory_with_intent(
            app,
            session_arc,
            refact_chat_api::types::TrajectoryCommitIntent::Required,
        )
        .await;
    }
}

pub async fn handle_v1_prepend_system_prompt_and_maybe_more_initial_messages(
    State(app): State<AppState>,
    body_bytes: hyper::body::Bytes,
) -> Result<Response<Body>, ScratchError> {
    let gcx = app.gcx.clone();
    wait_for_indexing_if_needed(gcx.clone()).await;

    let post = serde_json::from_slice::<PrependSystemPromptPost>(&body_bytes).map_err(|e| {
        ScratchError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("JSON problem: {}", e),
        )
    })?;
    let mut has_rag_results = HasRagResults::new();

    let mode_id = validate_mode_for_request(gcx.clone(), &post.chat_meta.chat_mode)
        .await
        .map_err(|e| {
            ScratchError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Invalid chat mode: {}", e),
            )
        })?;

    let tool_descs = app.tool_registry.get_tools_for_mode(&mode_id, None).await;
    let prompt_tool_names = tool_descs.iter().map(|t| t.name.clone()).collect();

    let (messages, _) = prepend_the_right_system_prompt_and_maybe_more_initial_messages(
        refact_runtime_api::app_state_from_gcx(gcx.clone()).await,
        post.messages,
        &post.chat_meta,
        &None,
        &mut has_rag_results,
        prompt_tool_names,
        &mode_id,
        "",
    )
    .await;

    let system_prompt = messages.iter().find_map(|message| {
        if message.role == "system" {
            match &message.content {
                ChatContent::SimpleText(text) => Some(text.clone()),
                _ => None,
            }
        } else {
            None
        }
    });
    let canonical_tools = build_canonical_openai_tools(gcx.chat_ctx(gcx.clone()), &tool_descs, false, true).await;
    let frozen_prefix = new_frozen_request_prefix(
        system_prompt,
        serde_json::Value::Array(canonical_tools.tools),
    );
    persist_init_frozen_prefix(app.clone(), &post.chat_meta.chat_id, frozen_prefix).await;
    let messages_to_stream_back = has_rag_results.in_json;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Body::from(
            serde_json::to_string(&PrependSystemPromptResponse {
                messages,
                messages_to_stream_back,
            })
            .unwrap(),
        ))
        .unwrap())
}
