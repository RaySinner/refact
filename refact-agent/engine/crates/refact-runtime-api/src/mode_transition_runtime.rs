use std::collections::HashMap;
use std::path::PathBuf;

use refact_agentic::mode_transition::{
    self, ParsedDecisions, ReconstructionOutcome, ReconstructionRequest, TransitionContextBudget,
};
use refact_chat_api::{ChatMessage, ExplicitSubchatSpec, TraceParent};
use refact_context_api::PathsAccess;

use crate::global_context::{GlobalContextCtx, SharedGlobalContext};

const SUBAGENT_ID: &str = "mode_transition";

struct MessageAudit<'a>(&'a [ChatMessage]);

impl refact_privacy::PrivacyAudited for MessageAudit<'_> {
    fn privacy_records(
        &self,
    ) -> Result<Vec<(usize, refact_privacy::FileRecord)>, refact_privacy::PrivacyAuditError> {
        refact_privacy::records_from_messages(self.0)
    }
}

async fn analyze_reconstruction(
    gcx: SharedGlobalContext,
    request: &ReconstructionRequest<'_>,
) -> Result<(ParsedDecisions, String, TransitionContextBudget), String> {
    mode_transition::check_cancelled(request)?;
    if request.messages.is_empty() {
        return Err("The provided chat is empty".into());
    }
    let ctx = gcx.chat_ctx(gcx.clone());
    let subagent = ctx
        .mode_config
        .get_subagent_config(SUBAGENT_ID, None)
        .await
        .ok_or("mode_transition config not found")?;
    let template = subagent
        .messages
        .user_template
        .as_ref()
        .ok_or("Missing reconstruction prompt")?;
    let mut params = crate::subchat_runtime::resolve_subchat_params(&ctx, SUBAGENT_ID).await?;
    if let Some(model) = &request.model_override {
        params.subchat_model = model.clone();
    }
    let model = crate::subchat_runtime::resolve_subchat_model(&ctx, &params).await?;
    let spec = ExplicitSubchatSpec {
        params,
        model: model.clone(),
        autonomous_no_confirm: false,
    };
    let mut config = ctx
        .subchat
        .resolve_subchat_config_with_explicit_params(
            SUBAGENT_ID,
            &spec,
            false,
            None,
            None,
            None,
            None,
            None,
            Some(vec![]),
            1,
            false,
            "agent".into(),
            None,
            None,
            None,
            None,
            request.abort_flag.clone(),
            0,
        )
        .await?;
    config.trace_parent = TraceParent::from_parts(request.parent_chat_id, None);
    let caps = ctx
        .caps
        .try_load_caps_quickly(0)
        .await
        .map_err(|e| e.to_string())?;
    let model_rec = refact_caps_core::model_resolve::resolve_chat_model(caps, &model)?;
    let tokenizer = crate::chat_tokens::cached_tokenizer_for_ctx(&ctx, &model_rec.base).await?;
    let mut budget = mode_transition::calculate_transition_context_budget(request.messages);
    if let Some(total) = request.target_budget_symbols {
        budget.total_symbols = total;
        budget.files_symbols = total * 70 / 100;
        budget.messages_symbols = total - budget.files_symbols;
    }
    let metadata = mode_transition::extract_conversation_metadata(request.messages);
    let hints = request
        .hints
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let prompt = mode_transition::ReconstructionPrompt {
        template,
        target_mode: request.target_mode,
        target_mode_description: request.target_mode_description,
        metadata: &metadata,
        messages: request.messages,
        budget,
        hints: &hints,
    };
    let input_limit =
        mode_transition::reconstruction_input_limit(config.n_ctx, config.max_new_tokens);
    let prompt = mode_transition::fit_reconstruction_prompt(
        |cap| mode_transition::render_reconstruction_prompt(&prompt, cap),
        |text| {
            // Without a local tokenizer use a conservative byte bound rather than
            // chars/4, which can undercount dense Unicode or source text.
            if tokenizer.is_some() {
                refact_core::tokens_runtime::count_text_tokens(tokenizer.clone(), text)
            } else {
                Ok(text.len())
            }
        },
        input_limit,
    )?;
    let mut analysis = ChatMessage::new("user".into(), prompt);
    refact_privacy::carry_records_into(&mut analysis, request.messages)
        .map_err(|e| e.to_string())?;
    refact_privacy::destinations::clear_for_model(
        &gcx.privacy_policy_load,
        MessageAudit(std::slice::from_ref(&analysis)),
        &model_rec.base,
    )
    .map_err(|e| e.to_string())?;
    mode_transition::check_cancelled(request)?;
    let result = ctx.subchat.run_subchat(vec![analysis], config).await?;
    mode_transition::check_cancelled(request)?;
    let response = result
        .messages
        .last()
        .filter(|m| m.role == "assistant" && m.tool_calls.as_ref().is_none_or(|c| c.is_empty()))
        .ok_or("No reconstruction analysis answer generated")?;
    let decisions = mode_transition::parse_llm_response(&response.content.content_text_only());
    mode_transition::validate_decisions(&decisions, request.messages)?;
    Ok((decisions, model, budget))
}

/// Wrapper over `refact_agentic::mode_transition::assemble_reconstruction`: the analysis,
/// privacy clearing and subchat run need the live context, while `ReconstructionRequest`
/// lives in the crate.
pub async fn reconstruct_context(
    gcx: SharedGlobalContext,
    request: ReconstructionRequest<'_>,
) -> Result<ReconstructionOutcome, String> {
    let ctx = gcx.chat_ctx(gcx.clone());
    let (mut decisions, model, budget) = analyze_reconstruction(gcx.clone(), &request).await?;
    let workspace_dirs = gcx.workspace_folders();
    let metadata = mode_transition::extract_conversation_metadata(request.messages);
    let allowed = mode_transition::allowed_reconstruction_paths(&metadata);
    let caps = ctx
        .caps
        .try_load_caps_quickly(0)
        .await
        .map_err(|e| e.to_string())?;
    let model_rec = refact_caps_core::model_resolve::resolve_chat_model(caps, &model)?;
    let mut reread_records = Vec::new();
    let mut preloaded = HashMap::new();
    // Only canonical, conversation-allowlisted, runtime-cleared paths may be read by assembly.
    for paths in [
        &mut decisions.files_to_open,
        &mut decisions.memories_to_include,
    ] {
        let mut safe = Vec::new();
        for path in paths.iter() {
            mode_transition::check_cancelled(&request)?;
            if !allowed.contains(path) {
                continue;
            }
            let full = PathBuf::from(path);
            let full = if full.is_absolute() {
                full
            } else if let Some(root) = workspace_dirs.first() {
                root.join(full)
            } else {
                continue;
            };
            let Ok(canonical) = full.canonicalize() else {
                continue;
            };
            if !mode_transition::canonical_reread_path_allowed(&canonical, &workspace_dirs) {
                continue;
            }
            crate::files_in_workspace::check_file_privacy_for_model_context(
                gcx.clone(),
                &canonical.to_path_buf(),
            )
            .await?;
            let record = refact_privacy::model_context::declared_file_record(
                &crate::gcx_helpers::privacy_record_scope(&gcx),
                &canonical,
            )?;
            let mut audit = ChatMessage::default();
            refact_privacy::attach_record(&mut audit, record.clone());
            refact_privacy::destinations::clear_for_model(
                &gcx.privacy_policy_load,
                MessageAudit(std::slice::from_ref(&audit)),
                &model_rec.base,
            )
            .map_err(|e| e.to_string())?;
            // Read the same canonical path that was cleared, once; pure assembly must
            // never resolve the original alias again after the privacy check.
            if tokio::fs::metadata(&canonical)
                .await
                .map_err(|e| e.to_string())?
                .len()
                > 1024 * 1024
            {
                continue;
            }
            let content = tokio::fs::read_to_string(&canonical)
                .await
                .map_err(|e| e.to_string())?;
            preloaded.insert(path.clone(), content);
            reread_records.push(record);
            safe.push(path.clone());
        }
        *paths = safe;
    }
    let mut messages = mode_transition::assemble_reconstruction(
        request.messages,
        &decisions,
        &workspace_dirs,
        budget,
        request.preserve_goal_messages,
        &preloaded,
    )
    .await?;
    let reread_paths = mode_transition::reread_paths(&messages);
    mode_transition::finalize_reconstructed_messages(
        &mut messages,
        request.messages,
        reread_records,
    )?;
    mode_transition::check_cancelled(&request)?;
    Ok(ReconstructionOutcome {
        messages,
        decisions,
        model,
        budget,
        reread_paths,
    })
}

pub async fn analyze_mode_transition(
    gcx: SharedGlobalContext,
    messages: &[ChatMessage],
    target_mode: &str,
    target_mode_description: &str,
    parent_chat_id: Option<&str>,
) -> Result<ParsedDecisions, String> {
    analyze_reconstruction(
        gcx,
        &ReconstructionRequest {
            messages,
            target_mode,
            target_mode_description,
            parent_chat_id,
            model_override: None,
            abort_flag: None,
            hints: None,
            target_budget_symbols: None,
            preserve_goal_messages: false,
        },
    )
    .await
    .map(|(decisions, _, _)| decisions)
}

pub async fn assemble_new_chat<T: refact_context_api::PathsAccess + ?Sized>(
    context: &T,
    original_messages: &[ChatMessage],
    decisions: &ParsedDecisions,
) -> Result<Vec<ChatMessage>, String> {
    mode_transition::assemble_new_chat_for_context(context, original_messages, decisions).await
}
