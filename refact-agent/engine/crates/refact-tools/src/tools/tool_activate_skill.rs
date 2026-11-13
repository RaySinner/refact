use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex as AMutex;

use refact_runtime_api::AtCommandsContext;
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
use refact_ext::config_dirs::get_ext_dirs;
use refact_ext::skills::load_skill_full;
use refact_ext::skills_context::expand_skill_includes;
use crate::tools::tools_description::{
    Tool, ToolDesc, ToolSource, ToolSourceType, json_schema_from_params,
};
use refact_chat_api::{ActiveCommandContext, PendingSkillDeactivation};

pub struct ToolActivateSkill {
    pub config_path: String,
}

#[derive(Debug)]
pub struct ActivatedSkill {
    pub body: String,
    pub allowed_tools: Vec<String>,
    pub model_override: Option<String>,
    pub skill_dir: String,
}

pub fn build_cd_instruction_content(name: &str, activated: &ActivatedSkill) -> String {
    let header_json = serde_json::json!({
        "name": name,
        "allowed_tools": &activated.allowed_tools,
        "model_override": &activated.model_override,
        "skill_dir": &activated.skill_dir,
    });
    format!(
        "💿 SKILL_ACTIVATED {}\n\nSkill directory: `{}`\n\nUse absolute paths under this directory when reading skill companion files (docs, scripts, assets). Do not use workspace-relative paths like `skills/{}/...`.\n\n{}",
        header_json,
        activated.skill_dir,
        name,
        activated.body
    )
}

pub async fn activate_skill_inner(
    ext_dirs: &refact_ext::config_dirs::ExtDirs,
    name: &str,
) -> Result<ActivatedSkill, String> {
    if let Err(e) = refact_ext::skills::validate_skill_id(name) {
        return Err(format!("Invalid skill name '{}': {}", name, e));
    }
    let skill = load_skill_full(ext_dirs, name)
        .await
        .ok_or_else(|| format!("Skill '{}' not found", name))?;
    if !skill.index.user_invocable {
        return Err(format!("Skill '{}' is not available for activation", name));
    }
    if skill.index.disable_model_invocation {
        return Err(format!("Skill '{}' cannot be activated by the model", name));
    }
    let body = expand_skill_includes(&skill.body, &skill.skill_dir).await;
    Ok(ActivatedSkill {
        body,
        allowed_tools: skill.allowed_tools,
        model_override: skill.model,
        skill_dir: skill.skill_dir.display().to_string(),
    })
}

#[async_trait]
impl Tool for ToolActivateSkill {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "activate_skill".to_string(),
            display_name: "Activate Skill".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Load a skill's full instructions into the current context. Use when you determine a skill from the available index is relevant to the user's request. Once activated, the skill's instructions guide your approach. When you're done with the skill's task, you MUST call deactivate_skill with a thorough report.".to_string(),
            input_schema: json_schema_from_params(
                &[("name", "string", "Name of the skill to activate")],
                &["name"],
            ),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let name = match args.get("name") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => return Err(format!("argument `name` is not a string: {:?}", v)),
            None => return Err("argument `name` is missing".to_string()),
        };
        if let Err(e) = refact_ext::skills::validate_skill_id(&name) {
            return Err(format!("Invalid skill name '{}': {}", name, e));
        }

        let (sessions_map, chat_id) = {
            let cgcx = ccx.lock().await;
            (
                refact_runtime_api::session_handles::sessions_map_from_handle(&cgcx.tool.chat.sessions),
                cgcx.chat_id.clone(),
            )
        };

        {
            let session_arc_opt = {
                let sessions = sessions_map.read().await;
                sessions.get(&chat_id).cloned()
            };
            if let Some(session_arc) = session_arc_opt {
                let mut session = session_arc.lock().await;
                if session.thread.active_skill.as_deref() == Some(name.as_str()) {
                    if session.active_command.started_at_index.is_none() {
                        session.active_command.started_at_index = Some(session.messages.len());
                        session.active_command.name = name.clone();
                    }
                    return Ok((false, vec![
                        ContextEnum::ChatMessage(ChatMessage {
                            role: "tool".to_string(),
                            content: ChatContent::SimpleText(format!("Skill '{}' is already active. Continue following its instructions.", name)),
                            tool_call_id: tool_call_id.clone(),
                            ..Default::default()
                        }),
                    ]));
                }
                if let Some(ref current) = session.thread.active_skill {
                    return Err(format!(
                        "Skill '{}' is currently active. Call deactivate_skill first before activating a different skill.",
                        current
                    ));
                }
            }
        }

        let (config_dir, workspace_dirs) = {
            let app = ccx.lock().await.app.clone();
            let workspace_dirs = refact_runtime_api::files_correction::get_project_dirs(
                app.gcx.clone(),
            )
            .await;
            (app.paths.config_dir.clone(), workspace_dirs)
        };
        let ext_dirs = get_ext_dirs(&config_dir, &workspace_dirs).await;
        let activated = activate_skill_inner(&ext_dirs, &name).await?;

        {
            let session_arc_opt = {
                let sessions = sessions_map.read().await;
                sessions.get(&chat_id).cloned()
            };
            if let Some(session_arc) = session_arc_opt {
                let mut session = session_arc.lock().await;
                session.active_command.name = name.clone();
                session.active_command.allowed_tools = activated.allowed_tools.clone();
                session.active_command.model_override = activated.model_override.clone();
                session.active_command.started_at_index = Some(session.messages.len());
                session.active_command.activation_tool_call_id = Some(tool_call_id.clone());
                session.set_active_skill(name.clone());
            }
        }

        let cd_instruction_content = build_cd_instruction_content(&name, &activated);

        Ok((
            false,
            vec![
                ContextEnum::ChatMessage(ChatMessage {
                    role: "tool".to_string(),
                    content: ChatContent::SimpleText(format!("Skill '{}' activated.", name)),
                    tool_call_id: tool_call_id.clone(),
                    ..Default::default()
                }),
                ContextEnum::ChatMessage(ChatMessage {
                    role: "cd_instruction".to_string(),
                    content: ChatContent::SimpleText(cd_instruction_content),
                    ..Default::default()
                }),
            ],
        ))
    }
}

pub struct ToolDeactivateSkill {
    pub config_path: String,
}

#[async_trait]
impl Tool for ToolDeactivateSkill {
    fn tool_description(&self) -> ToolDesc {
        ToolDesc {
            name: "deactivate_skill".to_string(),
            display_name: "Deactivate Skill".to_string(),
            source: ToolSource {
                source_type: ToolSourceType::Builtin,
                config_path: self.config_path.clone(),
            },
            experimental: false,
            allow_parallel: false,
            description: "Deactivate the currently active skill with a completion report. The report should be a thorough overview of what was done, what happened, and what was changed. After deactivation, the skill execution messages are compacted into the report, keeping chat history clean while preserving knowledge of what occurred.".to_string(),
            input_schema: json_schema_from_params(
                &[("report", "string", "A thorough overview of what was done, what happened, what was changed during the skill execution. Use clear markdown formatting.")],
                &["report"],
            ),
            output_schema: None,
            annotations: None,
        }
    }

    async fn tool_execute(
        &mut self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let report =
            match args.get("report") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => return Err(format!("argument `report` is not a string: {:?}", v)),
                None => return Err(
                    "argument `report` is missing. Provide a thorough overview of what was done."
                        .to_string(),
                ),
            };

        let (sessions_map, chat_id) = {
            let cgcx = ccx.lock().await;
            (
                refact_runtime_api::session_handles::sessions_map_from_handle(&cgcx.tool.chat.sessions),
                cgcx.chat_id.clone(),
            )
        };

        {
            let session_arc_opt = {
                let sessions = sessions_map.read().await;
                sessions.get(&chat_id).cloned()
            };
            if let Some(session_arc) = session_arc_opt {
                let mut session = session_arc.lock().await;
                let skill_name = match session.thread.active_skill.clone() {
                    Some(name) => name,
                    None => return Err("No active skill to deactivate".to_string()),
                };
                let start_index = session
                    .active_command
                    .started_at_index
                    .unwrap_or(session.messages.len());
                session.pending_skill_deactivation = Some(PendingSkillDeactivation {
                    start_index,
                    report: report.clone(),
                    skill_name: skill_name.clone(),
                    activation_tool_call_id: session.active_command.activation_tool_call_id.clone(),
                });
                let compaction_note = if session.active_command.started_at_index.is_some() {
                    String::new()
                } else {
                    tracing::warn!("deactivate_skill: no started_at_index for skill '{}', reporting without compaction", skill_name);
                    " (Note: skill history compaction was skipped — activation anchor was not set.)"
                        .to_string()
                };
                session.active_command = ActiveCommandContext::default();
                session.clear_active_skill();
                return Ok((
                    false,
                    vec![ContextEnum::ChatMessage(ChatMessage {
                        role: "tool".to_string(),
                        content: ChatContent::SimpleText(format!(
                            "✅ Skill '{}' deactivated. Report has been recorded.{}",
                            skill_name, compaction_note
                        )),
                        tool_call_id: tool_call_id.clone(),
                        ..Default::default()
                    })],
                ));
            }
        }

        Ok((
            false,
            vec![ContextEnum::ChatMessage(ChatMessage {
                role: "tool".to_string(),
                content: ChatContent::SimpleText(
                    "✅ Skill deactivated. Report has been recorded.".to_string(),
                ),
                tool_call_id: tool_call_id.clone(),
                ..Default::default()
            })],
        ))
    }
}
