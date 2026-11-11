pub use refact_agentic::mode_transition::{
    assemble_new_chat_for_context, calculate_transition_context_budget, carried_goal_messages,
    carried_plan_messages, check_cancelled, context_file_rendered_symbols,
    count_images_in_messages, current_base_goal_message, extract_conversation_metadata,
    extract_initial_plan_text, format_annotated_messages, format_budget_summary, format_file_list,
    format_memory_list, insert_goal_messages_before_plan, is_goal_delta_event,
    make_pinned_plan_message, message_symbols, normalize_goal_message_content, parse_llm_response,
    push_context_file_with_budget, text_symbols, transfer_goal_ownership, truncate_utf8,
    AgenticPathContext, ConversationMetadata, FileReference, GoalTransferResult, ParsedDecisions,
    ReconstructionHints, ReconstructionOutcome, ReconstructionRequest, TransitionContextBudget,
};

use std::path::PathBuf;
use std::sync::Arc;

use refact_context_api::PathsAccess;

use crate::call_validation::{ChatContent, ChatMessage};
use crate::global_context::GlobalContextCtx;
use crate::global_context::GlobalContext;
use crate::subchat::{
    run_subchat, resolve_subchat_config_with_explicit_params, resolve_subchat_params,
    resolve_subchat_model, ExplicitSubchatSpec, TraceParent,
};
use crate::yaml_configs::customization_registry::get_subagent_config;

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
    gcx: Arc<GlobalContext>,
    request: &ReconstructionRequest<'_>,
) -> Result<(ParsedDecisions, String, TransitionContextBudget), String> {
    check_cancelled(request)?;
    if request.messages.is_empty() {
        return Err("The provided chat is empty".into());
    }
    let subagent = get_subagent_config(gcx.clone(), SUBAGENT_ID, None)
        .await
        .ok_or("mode_transition config not found")?;
    let template = subagent
        .messages
        .user_template
        .as_ref()
        .ok_or("Missing reconstruction prompt")?;
    let ctx = gcx.chat_ctx(gcx.clone());
    let mut params = resolve_subchat_params(&ctx, SUBAGENT_ID).await?;
    if let Some(model) = &request.model_override {
        params.subchat_model = model.clone();
    }
    let model = resolve_subchat_model(&ctx, &params).await?;
    let spec = ExplicitSubchatSpec {
        params,
        model: model.clone(),
        autonomous_no_confirm: false,
    };
    let mut config = resolve_subchat_config_with_explicit_params(
        gcx.clone(),
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
    let caps = crate::global_context::try_load_caps_quickly_if_not_present(gcx.clone(), 0)
        .await
        .map_err(|e| e.to_string())?;
    let model_rec = crate::caps::resolve_chat_model(caps, &model)?;
    let tokenizer = crate::tokens::cached_tokenizer(gcx.clone(), &model_rec.base).await?;
    let mut budget = calculate_transition_context_budget(request.messages);
    if let Some(total) = request.target_budget_symbols {
        budget.total_symbols = total;
        budget.files_symbols = total * 70 / 100;
        budget.messages_symbols = total - budget.files_symbols;
    }
    let metadata = extract_conversation_metadata(request.messages);
    // Full current request/control artifacts are mandatory; older entries have bounded previews.
    let latest_user = request.messages.iter().rposition(|m| m.role == "user");
    let hints = request
        .hints
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let render = |cap: usize| {
        let mut annotated = String::new();
        for (index, (id, message)) in metadata.annotated_messages.iter().enumerate() {
            let mandatory = Some(index) == latest_user
                || matches!(message.role.as_str(), "plan" | "goal")
                || message
                    .extra
                    .get("event")
                    .and_then(|e| e.get("subkind"))
                    .and_then(|s| s.as_str())
                    .is_some_and(|s| matches!(s, "plan_delta" | "goal_delta" | "goal_pursuit"));
            let text = message.content.content_text_only();
            let text = if mandatory {
                text
            } else {
                truncate_utf8(&text, cap)
            };
            annotated.push_str(&format!("[{id}] [{}]\n{text}\n", message.role));
            if let Some(calls) = &message.tool_calls {
                for call in calls {
                    annotated.push_str(&format!(
                        "tool: {} {}\n",
                        call.function.name,
                        truncate_utf8(&call.function.arguments, cap)
                    ));
                }
            }
        }
        format!(
            "{}\nCaller preservation hints (source-grounded only): {}",
            template
                .replace("{target_mode}", request.target_mode)
                .replace("{target_mode_description}", request.target_mode_description)
                .replace("{annotated_message_list}", &annotated)
                .replace("{file_list}", &format_file_list(&metadata))
                .replace("{memory_list}", &format_memory_list(&metadata))
                .replace(
                    "{budget_summary}",
                    &format_budget_summary(budget, request.messages)
                ),
            hints
        )
    };
    let input_limit = config
        .n_ctx
        .saturating_sub(config.max_new_tokens)
        .saturating_sub(1024);
    let mut cap = 16000;
    let prompt = loop {
        let prompt = render(cap);
        // Without a local tokenizer use a conservative byte bound rather than
        // chars/4, which can undercount dense Unicode or source text.
        let tokens = if tokenizer.is_some() {
            crate::tokens::count_text_tokens(tokenizer.clone(), &prompt)?
        } else {
            prompt.len()
        };
        if tokens <= input_limit {
            break prompt;
        }
        if cap == 0 {
            return Err(
                "Mandatory reconstruction input and instructions exceed resolved analyzer context"
                    .into(),
            );
        }
        cap /= 2;
    };
    let mut analysis = ChatMessage::new("user".into(), prompt);
    refact_privacy::carry_records_into(&mut analysis, request.messages)
        .map_err(|e| e.to_string())?;
    crate::privacy::destinations::clear_for_model(
        &gcx.privacy_policy_load,
        MessageAudit(std::slice::from_ref(&analysis)),
        &model_rec.base,
    )
    .map_err(|e| e.to_string())?;
    check_cancelled(request)?;
    let result = run_subchat(gcx, vec![analysis], config).await?;
    check_cancelled(request)?;
    let response = result
        .messages
        .last()
        .filter(|m| m.role == "assistant" && m.tool_calls.as_ref().is_none_or(|c| c.is_empty()))
        .ok_or("No reconstruction analysis answer generated")?;
    let decisions = parse_llm_response(&response.content.content_text_only());
    refact_agentic::mode_transition::validate_decisions(&decisions, request.messages)?;
    Ok((decisions, model, budget))
}

/// Engine wrapper over `refact_agentic::mode_transition::assemble_reconstruction`: the analysis,
/// privacy clearing and subchat run need `GlobalContext`, while `ReconstructionRequest` lives in the crate.
pub async fn reconstruct_context(
    gcx: Arc<GlobalContext>,
    request: ReconstructionRequest<'_>,
) -> Result<ReconstructionOutcome, String> {
    let (mut decisions, model, budget) = analyze_reconstruction(gcx.clone(), &request).await?;
    let workspace_dirs = gcx.workspace_folders();
    let metadata = extract_conversation_metadata(request.messages);
    let allowed: std::collections::HashSet<_> = metadata
        .context_files
        .iter()
        .chain(&metadata.edited_files)
        .map(|f| f.path.clone())
        .chain(metadata.memory_paths.iter().cloned())
        .collect();
    let caps = crate::global_context::try_load_caps_quickly_if_not_present(gcx.clone(), 0)
        .await
        .map_err(|e| e.to_string())?;
    let model_rec = crate::caps::resolve_chat_model(caps, &model)?;
    let mut reread_records = Vec::new();
    let mut preloaded = std::collections::HashMap::new();
    // Only canonical, conversation-allowlisted, runtime-cleared paths may be read by assembly.
    for paths in [
        &mut decisions.files_to_open,
        &mut decisions.memories_to_include,
    ] {
        let mut safe = Vec::new();
        for path in paths.iter() {
            check_cancelled(&request)?;
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
            let in_workspace = workspace_dirs.iter().any(|root| {
                root.canonicalize()
                    .is_ok_and(|root| canonical.starts_with(root))
            });
            let in_refact = canonical
                .components()
                .any(|c| matches!(c, std::path::Component::Normal(name) if name == ".refact"));
            if !in_workspace && !in_refact {
                continue;
            }
            crate::files_in_workspace::check_file_privacy_for_model_context(
                gcx.clone(),
                &canonical,
            )
            .await?;
            let record = crate::privacy::records::declared_file_record(
                &crate::privacy::records::privacy_record_scope(&gcx),
                &canonical,
            )?;
            let mut audit = ChatMessage::default();
            refact_privacy::attach_record(&mut audit, record.clone());
            crate::privacy::destinations::clear_for_model(
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
    let mut messages = refact_agentic::mode_transition::assemble_reconstruction(
        request.messages,
        &decisions,
        &workspace_dirs,
        budget,
        request.preserve_goal_messages,
        &preloaded,
    )
    .await?;
    let reread_paths = messages
        .iter()
        .filter_map(|m| match &m.content {
            ChatContent::ContextFiles(f) => Some(f.iter().map(|f| f.file_name.clone())),
            _ => None,
        })
        .flatten()
        .collect();
    for message in &mut messages {
        if message.message_id.is_empty() {
            message.message_id = uuid::Uuid::new_v4().to_string();
        }
        refact_privacy::carry_records_into(message, request.messages)
            .map_err(|e| e.to_string())?;
        refact_privacy::merge_records(message, reread_records.clone());
    }
    check_cancelled(&request)?;
    Ok(ReconstructionOutcome {
        messages,
        decisions,
        model,
        budget,
        reread_paths,
    })
}

pub async fn analyze_mode_transition(
    gcx: Arc<GlobalContext>,
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

pub async fn assemble_new_chat<T: PathsAccess + ?Sized>(
    context: &T,
    original_messages: &[ChatMessage],
    decisions: &ParsedDecisions,
) -> Result<Vec<ChatMessage>, String> {
    assemble_new_chat_for_context(context, original_messages, decisions).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use crate::call_validation::{ChatContent, ChatMessage, ContextFile};

    #[test]
    fn reconstruction_cancellation_is_checked_before_work() {
        let flag = Arc::new(AtomicBool::new(true));
        let request = ReconstructionRequest {
            messages: &[],
            target_mode: "agent",
            target_mode_description: "",
            parent_chat_id: None,
            model_override: None,
            abort_flag: Some(flag.clone()),
            hints: None,
            target_budget_symbols: None,
            preserve_goal_messages: true,
        };
        assert!(check_cancelled(&request).is_err());
        flag.store(false, Ordering::Release);
        assert!(check_cancelled(&request).is_ok());
    }

    #[test]
    fn reconstruction_audit_keeps_source_records() {
        let mut source = ChatMessage::new("user".into(), "source".into());
        refact_privacy::attach_record(
            &mut source,
            refact_privacy::FileRecord {
                path: "secret.rs".into(),
                zone: "private".into(),
                attribution: refact_privacy::Attribution::Declared,
            },
        );
        let mut narrative = ChatMessage::new("user".into(), "summary".into());
        refact_privacy::carry_records_into(&mut narrative, &[source]).unwrap();
        let records = refact_privacy::records_from_messages(&[narrative]).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].1.zone, "private");
    }

    struct TestPaths {
        workspace_folders: Vec<PathBuf>,
    }

    impl TestPaths {
        fn new(workspace_folders: Vec<PathBuf>) -> Self {
            Self { workspace_folders }
        }
    }

    impl PathsAccess for TestPaths {
        fn cache_dir(&self) -> PathBuf {
            PathBuf::new()
        }

        fn config_dir(&self) -> PathBuf {
            PathBuf::new()
        }

        fn workspace_folders(&self) -> Vec<PathBuf> {
            self.workspace_folders.clone()
        }
    }

    #[tokio::test]
    async fn test_assemble_new_chat_limits_message_budget_and_images() {
        use crate::scratchpads::multimodality::MultimodalElement;

        let paths = TestPaths::new(vec![]);
        let original_messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: ChatContent::SimpleText("baseline ".repeat(400)),
                ..Default::default()
            },
            ChatMessage {
                role: "user".to_string(),
                content: ChatContent::Multimodal(vec![
                    MultimodalElement {
                        m_type: "text".to_string(),
                        m_content: "first image context ".repeat(100),
                    },
                    MultimodalElement {
                        m_type: "image/png".to_string(),
                        m_content: "base64data-one".to_string(),
                    },
                ]),
                ..Default::default()
            },
            ChatMessage {
                role: "user".to_string(),
                content: ChatContent::Multimodal(vec![
                    MultimodalElement {
                        m_type: "text".to_string(),
                        m_content: "second image context ".repeat(100),
                    },
                    MultimodalElement {
                        m_type: "image/png".to_string(),
                        m_content: "base64data-two".to_string(),
                    },
                ]),
                ..Default::default()
            },
        ];
        let budget = calculate_transition_context_budget(&original_messages);
        let decisions = ParsedDecisions {
            summary: "summary ".repeat(200),
            messages_to_preserve: vec!["MSG_ID:1".to_string(), "MSG_ID:2".to_string()],
            handoff_message: "continue ".repeat(200),
            ..Default::default()
        };

        let new_messages = assemble_new_chat(&paths, &original_messages, &decisions)
            .await
            .unwrap();
        let message_symbols = new_messages
            .iter()
            .filter(|msg| msg.role != "context_file")
            .map(|msg| text_symbols(&msg.content.content_text_only()))
            .sum::<usize>();
        let image_count = count_images_in_messages(&new_messages);

        assert!(message_symbols <= budget.messages_symbols);
        assert_eq!(image_count, 2);
    }

    #[tokio::test]
    async fn test_assemble_new_chat_limits_file_budget() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("src/main.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(
            &file_path,
            "fn main() { println!(\"hello\"); }\n".repeat(300),
        )
        .unwrap();

        let paths = TestPaths::new(vec![dir.path().to_path_buf()]);

        let original_messages = vec![
            ChatMessage {
                role: "context_file".to_string(),
                content: ChatContent::ContextFiles(vec![ContextFile {
                    file_name: "src/main.rs".to_string(),
                    file_content: "old content\n".repeat(300),
                    line1: 1,
                    line2: 300,
                    ..Default::default()
                }]),
                ..Default::default()
            },
            ChatMessage {
                role: "user".to_string(),
                content: ChatContent::SimpleText("requirements ".repeat(500)),
                ..Default::default()
            },
        ];
        let budget = calculate_transition_context_budget(&original_messages);
        let decisions = ParsedDecisions {
            files_to_open: vec!["src/main.rs".to_string()],
            ..Default::default()
        };

        let new_messages = assemble_new_chat(&paths, &original_messages, &decisions)
            .await
            .unwrap();
        let file_symbols = new_messages
            .iter()
            .filter(|msg| msg.role == "context_file")
            .map(|msg| text_symbols(&msg.content.content_text_only()))
            .sum::<usize>();

        assert!(file_symbols <= budget.files_symbols);
        assert!(file_symbols > 0);
    }
}
