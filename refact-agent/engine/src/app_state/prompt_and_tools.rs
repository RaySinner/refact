#[allow(unused_imports)]
use super::*;

pub struct EnginePromptAssembler {
    gcx: SharedGlobalContext,
}

impl EnginePromptAssembler {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }
}

#[async_trait]
impl PromptAssembler for EnginePromptAssembler {
    async fn prepend_system_prompt(
        &self,
        messages: Vec<ChatMessage>,
        chat_meta: &crate::call_validation::ChatMeta,
        task_meta: &Option<crate::chat::types::TaskMeta>,
        stream_back_to_user: &mut crate::scratchpads::scratchpad_utils::HasRagResults,
        tool_names: HashSet<String>,
        mode_id: &str,
        model_id: &str,
    ) -> (
        Vec<ChatMessage>,
        crate::ext::skills_context::SkillsTrackingInfo,
    ) {
        crate::chat::prompts::prepend_the_right_system_prompt_and_maybe_more_initial_messages(
            app_state_from_gcx(self.gcx.clone()).await,
            messages,
            chat_meta,
            task_meta,
            stream_back_to_user,
            tool_names,
            mode_id,
            model_id,
        )
        .await
    }

    async fn system_prompt_add_extra_instructions(
        &self,
        system_prompt: String,
        tool_names: HashSet<String>,
        chat_meta: &crate::call_validation::ChatMeta,
        task_meta: &Option<crate::chat::types::TaskMeta>,
        mode_id: &str,
    ) -> String {
        crate::chat::prompts::system_prompt_add_extra_instructions(
            app_state_from_gcx(self.gcx.clone()).await,
            system_prompt,
            tool_names,
            chat_meta,
            task_meta,
            mode_id,
        )
        .await
    }
}

pub struct EngineChatToolExecutor {
    gcx: SharedGlobalContext,
}

impl EngineChatToolExecutor {
    pub fn new(gcx: SharedGlobalContext) -> Self {
        Self { gcx }
    }
}

#[async_trait]
impl ChatToolExecutor for EngineChatToolExecutor {
    async fn execute_tools(
        &self,
        tool_calls: &[crate::call_validation::ChatToolCall],
        messages: &[ChatMessage],
        thread: &ThreadParams,
        mode_id: &str,
        model_id: Option<&str>,
        options: crate::chat::tools::ExecuteToolsOptions,
    ) -> (Vec<ChatMessage>, bool) {
        crate::chat::tools::execute_tools(
            app_state_from_gcx(self.gcx.clone()).await,
            tool_calls,
            messages,
            thread,
            mode_id,
            model_id,
            options,
        )
        .await
    }
}

