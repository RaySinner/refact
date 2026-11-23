#[cfg(test)]
#[allow(unused_imports)]
use std::collections::{HashMap, HashSet};
#[cfg(test)]
#[allow(unused_imports)]
use std::path::PathBuf;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::{Arc, OnceLock};
#[cfg(test)]
#[allow(unused_imports)]
use sha2::{Digest, Sha256};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use crate::app_state::AppState;
#[cfg(test)]
#[allow(unused_imports)]
use crate::call_validation;
#[cfg(test)]
#[allow(unused_imports)]
use crate::files_correction::{
    get_unscoped_project_dirs, normalize_path_for_unscoped_root_selection,
    registered_worktree_path_mappings,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::scratchpads::scratchpad_utils::HasRagResults;
#[cfg(test)]
#[allow(unused_imports)]
use super::system_context::{
    self, create_instruction_files_message, create_memories_message, gather_system_context,
    generate_git_info_prompt, gather_git_info, PROJECT_CONTEXT_MARKER,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::ext::skills_context::{
    build_skills_context_messages_tracked, build_skills_prompt_text, SkillsTrackingInfo,
    SKILLS_CONTEXT_MARKER,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tool_task_memory::{MemoryKind, MemoryNamespace, MemoryStatus, TaskMemoryFrontmatter};
#[cfg(test)]
#[allow(unused_imports)]
use crate::yaml_configs::project_information::load_project_information_config;
#[cfg(test)]
#[allow(unused_imports)]
use crate::call_validation::{ChatMessage, ChatContent, ContextFile, canonical_mode_id};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tasks::storage::infer_task_id_from_chat_id;
#[cfg(test)]
#[allow(unused_imports)]
use crate::yaml_configs::customization_registry::{get_mode_config, map_legacy_mode_to_id};

pub use refact_tools::chat_prompts::*;

#[cfg(test)]
mod tests {
    use super::*;
    use refact_buddy_core::runtime_queue::RuntimeQueue;
    use refact_buddy_core::settings::BuddySettings;
    use refact_privacy::{
        PolicyLoad, PrivacyPolicy, ProviderToolAccess, SubagentPolicy, ToolAccess, Zone,
    };
    use crate::call_validation::{ChatContent, ChatMeta};
    use crate::caps::{BaseModelRecord, ChatModelRecord, CodeAssistantCaps};
    use crate::tasks::types::{BoardCard, TaskBoard};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use tokio::sync::broadcast;

    struct ModePromptCase {
        label: &'static str,
        lookup_mode: &'static str,
        model_id: Option<&'static str>,
        cache_mode: &'static str,
    }

    const SPECIFIED_MODES: &[ModePromptCase] = &[
        ModePromptCase {
            label: "agent",
            lookup_mode: "agent",
            model_id: None,
            cache_mode: "agent",
        },
        ModePromptCase {
            label: "explore",
            lookup_mode: "explore",
            model_id: None,
            cache_mode: "explore",
        },
        ModePromptCase {
            label: "buddy",
            lookup_mode: "buddy",
            model_id: None,
            cache_mode: "buddy",
        },
        ModePromptCase {
            label: "task_planner",
            lookup_mode: "task_planner",
            model_id: None,
            cache_mode: "task_planner",
        },
        ModePromptCase {
            label: "task_agent",
            lookup_mode: "task_agent",
            model_id: None,
            cache_mode: "task_agent",
        },
        ModePromptCase {
            label: "setup",
            lookup_mode: "setup",
            model_id: None,
            cache_mode: "setup",
        },
        ModePromptCase {
            label: "learn",
            lookup_mode: "learn",
            model_id: None,
            cache_mode: "learn",
        },
        ModePromptCase {
            label: "plan",
            lookup_mode: "plan",
            model_id: None,
            cache_mode: "plan",
        },
    ];

    fn prompt_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    async fn make_gcx_with_buddy() -> AppState {
        clear_buddy_persona_cache_for_tests();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let (tx, _) = broadcast::channel(16);
        let mut state = refact_buddy_core::state::default_buddy_state();
        state.identity.name = "Pixel".to_string();
        state.personality.archetype_id = "helper_sprite".to_string();
        state.personality.archetype_label = "Helper Sprite".to_string();
        state.personality.vibe = "Playful, quirky, helpful".to_string();
        state.personality.summary = "An energetic helper.".to_string();
        state.personality.prompt = "Use warm humor.".to_string();
        let service = crate::buddy::actor::BuddyService::new(
            std::env::temp_dir().join(format!("buddy-persona-test-{}", uuid::Uuid::new_v4())),
            state,
            BuddySettings::default(),
            Vec::new(),
            RuntimeQueue::new(),
            tx,
            None,
        );
        let buddy_arc = app.buddy.buddy.clone();
        *buddy_arc.lock().await = Some(service);
        app
    }

    async fn set_buddy_name(app: &AppState, name: &str) {
        let buddy_arc = app.buddy.buddy.clone();
        let mut lock = buddy_arc.lock().await;
        let service = lock.as_mut().unwrap();
        service.state.identity.name = name.to_string();
    }

    fn system_text(messages: &[ChatMessage]) -> &str {
        let Some(message) = messages.iter().find(|m| m.role == "system") else {
            panic!("system message not found");
        };
        match &message.content {
            ChatContent::SimpleText(text) => text,
            _ => panic!("system content must be simple text"),
        }
    }

    async fn task_memory_test_app(task_id: &str) -> (tempfile::TempDir, AppState, PathBuf) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let temp = tempfile::tempdir().unwrap();
        {
            *gcx.documents_state.workspace_folders.lock().unwrap() =
                vec![temp.path().to_path_buf()];
        }
        let task_dir = temp.path().join(".refact/tasks").join(task_id);
        tokio::fs::create_dir_all(task_dir.join("memories"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(task_dir.join("documents"))
            .await
            .unwrap();
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        (temp, app, task_dir)
    }

    async fn write_task_memory(task_dir: &Path, name: &str, frontmatter: &str, body: &str) {
        tokio::fs::write(
            task_dir.join("memories").join(name),
            format!("---\n{}\n---\n\n{}", frontmatter.trim(), body),
        )
        .await
        .unwrap();
    }

    async fn write_task_document(task_dir: &Path, name: &str, frontmatter: &str, body: &str) {
        tokio::fs::write(
            task_dir.join("documents").join(name),
            format!("---\n{}\n---\n\n{}", frontmatter.trim(), body),
        )
        .await
        .unwrap();
    }

    async fn reset_task_briefing_tests() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = prompt_test_lock().lock().await;
        clear_task_briefing_cache_for_tests().await;
        clear_task_briefing_test_state().await;
        guard
    }

    async fn wait_for_task_briefing(expected_calls: usize) {
        for _ in 0..128 {
            if task_briefing_test_calls().await == expected_calls
                && task_briefing_test_in_flight().await == 0
            {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!(
            "task briefing did not finish: calls={}, in_flight={}",
            task_briefing_test_calls().await,
            task_briefing_test_in_flight().await
        );
    }

    fn task_meta(task_id: &str, role: &str, card_id: Option<&str>) -> crate::chat::types::TaskMeta {
        crate::chat::types::TaskMeta {
            task_id: task_id.to_string(),
            role: role.to_string(),
            agent_id: None,
            card_id: card_id.map(|id| id.to_string()),
            planner_chat_id: None,
        }
    }

    fn board_card(id: &str, column: &str, depends_on: Vec<&str>) -> BoardCard {
        let now = chrono::Utc::now().to_rfc3339();
        BoardCard {
            id: id.to_string(),
            title: id.to_string(),
            column: column.to_string(),
            priority: "P1".to_string(),
            depends_on: depends_on.into_iter().map(str::to_string).collect(),
            instructions: String::new(),
            assignee: None,
            agent_chat_id: None,
            retry_count: 0,
            status_updates: Vec::new(),
            comments: vec![],
            final_report: None,
            final_report_structured: None,
            verifier_report: None,
            created_at: now,
            started_at: None,
            last_heartbeat_at: None,
            completed_at: None,
            agent_branch: None,
            agent_worktree: None,
            agent_worktree_name: None,
            base_branch: None,
            base_commit: None,
            ab_variants: None,
            team_members: vec![],
            target_files: Vec::new(),
            scope_guard_mode: Default::default(),
        }
    }

    async fn injected_task_memory_files(
        app: &AppState,
        task_id: &str,
        task_meta: Option<&crate::chat::types::TaskMeta>,
    ) -> Vec<ContextFile> {
        injected_task_context_files(app, task_id, task_meta, TASK_MEMORIES_CONTEXT_MARKER).await
    }

    async fn injected_task_context_files(
        app: &AppState,
        task_id: &str,
        task_meta: Option<&crate::chat::types::TaskMeta>,
        marker: &str,
    ) -> Vec<ContextFile> {
        let mut messages = vec![ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText("hello".to_string()),
            ..Default::default()
        }];
        let mut stream_back_to_user = HasRagResults::new();
        inject_task_memories(
            app,
            &mut messages,
            &mut stream_back_to_user,
            Some(task_id.to_string()),
            task_meta,
        )
        .await
        .unwrap();
        let message = messages
            .iter()
            .find(|message| message.tool_call_id == marker)
            .unwrap();
        match &message.content {
            ChatContent::ContextFiles(files) => files.clone(),
            _ => panic!("task context must be context files"),
        }
    }

    fn combined_task_memory_content(files: &[ContextFile]) -> String {
        files
            .iter()
            .map(|file| file.file_content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn models_info_test_model(
        id: &str,
        n_ctx: usize,
        supports_tools: bool,
        supports_agent: bool,
    ) -> Arc<ChatModelRecord> {
        Arc::new(ChatModelRecord {
            base: BaseModelRecord {
                id: id.to_string(),
                name: id.to_string(),
                n_ctx,
                ..Default::default()
            },
            supports_tools,
            supports_agent,
            reasoning_effort_options: Some(vec!["high".to_string()]),
            ..Default::default()
        })
    }

    async fn models_info_test_app() -> (tempfile::TempDir, AppState) {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let temp = tempfile::tempdir().unwrap();
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![temp.path().to_path_buf()];
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        (temp, app)
    }

    async fn set_models_info_config(root: &Path, enabled: bool, max_chars: Option<usize>) {
        let max_chars = max_chars
            .map(|max_chars| format!("\n    max_chars: {max_chars}"))
            .unwrap_or_default();
        let config = format!(
            "enabled: true\nsections:\n  models_info:\n    enabled: {enabled}{max_chars}\n"
        );
        let config_dir = root.join(".refact");
        tokio::fs::create_dir_all(&config_dir).await.unwrap();
        tokio::fs::write(config_dir.join("project_information.yaml"), config)
            .await
            .unwrap();
    }

    async fn install_models_info_caps(app: &AppState) {
        let mut caps = CodeAssistantCaps::default();
        let default = "anthropic/claude-sonnet-4.5";
        let light = "openai/gpt-5-mini";
        caps.chat_models.insert(
            default.to_string(),
            models_info_test_model(default, 200_000, true, true),
        );
        caps.chat_models.insert(
            light.to_string(),
            models_info_test_model(light, 128_000, false, true),
        );
        caps.defaults.chat_default_model = default.to_string();
        caps.defaults.chat_light_model = light.to_string();
        caps.defaults.chat_thinking_model = default.to_string();
        caps.defaults.chat_buddy_model = light.to_string();
        caps.defaults.chat_model_2 = default.to_string();
        caps.defaults.task_planner_agent_model = light.to_string();
        app.gcx.caps_state.write().await.caps = Some(Arc::new(caps));
    }

    fn install_models_info_policy(app: &AppState) {
        *app.gcx.privacy_policy_load.write().unwrap() = PolicyLoad {
            policy: Arc::new(PrivacyPolicy {
                zones: vec![
                    Zone {
                        name: "secrets".to_string(),
                        send_to: vec!["anthropic".to_string(), "openai".to_string()],
                        ..Default::default()
                    },
                    Zone {
                        name: "normal".to_string(),
                        send_to: vec!["*".to_string()],
                        ..Default::default()
                    },
                ],
                subagents: SubagentPolicy {
                    report_declassifies: false,
                },
                tool_access: ToolAccess {
                    providers: BTreeMap::from([
                        (
                            "openai".to_string(),
                            ProviderToolAccess {
                                mcp: vec!["github".to_string(), "postgres".to_string()],
                            },
                        ),
                        (
                            "anthropic".to_string(),
                            ProviderToolAccess {
                                mcp: vec!["*".to_string()],
                            },
                        ),
                    ]),
                },
                ..Default::default()
            }),
            error: None,
            source_paths: Vec::new(),
        };
    }

    #[tokio::test]
    async fn models_info_snapshot_renders_slots_models_and_privacy_access() {
        let (_temp, app) = models_info_test_app().await;
        install_models_info_caps(&app).await;
        install_models_info_policy(&app);

        let snapshot = models_info_snapshot(&app).await;

        assert!(snapshot.contains("## Models & Access Snapshot"));
        assert!(snapshot.contains("### Model types (slots)"));
        assert!(snapshot.contains("- default → anthropic/claude-sonnet-4.5 (ctx 200k, tools ✓, agent ✓, reasoning effort)"));
        assert!(snapshot.contains(
            "- light → openai/gpt-5-mini (ctx 128k, tools ✗, agent ✓, reasoning effort)"
        ));
        assert!(snapshot.contains("- model_2 → anthropic/claude-sonnet-4.5"));
        assert!(snapshot.contains("- task_planner → openai/gpt-5-mini"));
        assert!(snapshot.contains("### Available chat models"));
        assert!(snapshot.contains(
            "- anthropic/claude-sonnet-4.5 — ctx 200k, tools ✓, agent ✓, reasoning effort"
        ));
        assert!(snapshot.contains("### Privacy & access"));
        assert!(snapshot.contains("- Privacy zones: secrets → anthropic, openai"));
        assert!(snapshot.contains(
            "MCP access per provider: anthropic → all servers; openai → github, postgres"
        ));
        assert!(snapshot.contains("- Subagent reports declassify: no"));
    }

    #[tokio::test]
    async fn models_info_snapshot_renders_privacy_and_catalog_gaps() {
        let (_temp, app) = models_info_test_app().await;
        install_models_info_caps(&app).await;
        {
            let mut caps = app.gcx.caps_state.write().await;
            let mut updated = caps.caps.as_deref().unwrap().clone();
            updated.defaults.chat_buddy_model = "missing-model".to_string();
            updated.chat_models.insert(
                "small-context".to_string(),
                models_info_test_model("small-context", 9_999, true, true),
            );
            updated.chat_models.insert(
                "rounded-context".to_string(),
                models_info_test_model("rounded-context", 131_072, true, true),
            );
            caps.caps = Some(Arc::new(updated));
        }
        let mut policy = PrivacyPolicy::default();
        policy.blocked = vec!["secret".to_string(), "token".to_string()];
        *app.gcx.privacy_policy_load.write().unwrap() = PolicyLoad {
            policy: Arc::new(policy),
            error: Some("bad config".to_string()),
            source_paths: Vec::new(),
        };

        let snapshot = models_info_snapshot(&app).await;

        assert!(snapshot.contains("- buddy → missing-model (not in catalog)"));
        assert!(snapshot.contains("ctx 9999"));
        assert!(snapshot.contains("ctx 131k"));
        assert!(snapshot.contains("- Blocked patterns: 2 configured (never sent anywhere)"));
        assert!(snapshot
            .contains("- Privacy policy: (load failed — treating as restrictive; check config)"));
    }

    #[tokio::test]
    async fn models_info_snapshot_handles_missing_and_empty_caps() {
        let (_temp, app) = models_info_test_app().await;

        let missing = models_info_snapshot(&app).await;
        assert_eq!(
            missing
                .matches("(models unavailable — caps not loaded)")
                .count(),
            1
        );
        assert!(missing.contains("no restricted zones configured"));
        assert!(missing.contains("no MCP restrictions"));
        assert!(missing.contains("Subagent reports declassify: yes"));

        app.gcx.caps_state.write().await.caps = Some(Arc::new(CodeAssistantCaps::default()));
        let empty = models_info_snapshot(&app).await;
        assert!(empty.contains("(no model type slots configured)"));
        assert!(empty.contains("(no chat models configured)"));
    }

    #[tokio::test]
    async fn models_info_macro_is_removed_when_section_is_disabled() {
        let (temp, app) = models_info_test_app().await;
        install_models_info_caps(&app).await;
        set_models_info_config(temp.path(), false, None).await;

        let rendered = system_prompt_add_extra_instructions(
            app,
            "Before\n%MODELS_INFO%\nAfter".to_string(),
            HashSet::new(),
            &ChatMeta {
                include_project_info: true,
                ..Default::default()
            },
            &None,
            "agent",
        )
        .await;

        assert_eq!(rendered, "Before\n\nAfter");
    }

    #[tokio::test]
    async fn models_info_macro_obeys_max_chars() {
        let (temp, app) = models_info_test_app().await;
        install_models_info_caps(&app).await;
        set_models_info_config(temp.path(), true, Some(25)).await;
        let expected = truncate_to_chars(&models_info_snapshot(&app).await, 25);

        let rendered = system_prompt_add_extra_instructions(
            app,
            "%MODELS_INFO%".to_string(),
            HashSet::new(),
            &ChatMeta {
                include_project_info: true,
                ..Default::default()
            },
            &None,
            "agent",
        )
        .await;

        assert_eq!(rendered, expected);
        assert!(rendered.ends_with("[TRUNCATED]"));
    }

    #[tokio::test]
    async fn task_memory_injection_skips_archived_and_superseded() {
        let task_id = "task-memory-archived";
        let (_temp, app, task_dir) = task_memory_test_app(task_id).await;
        write_task_memory(
            &task_dir,
            "active.md",
            "created_at: 2026-05-22T00:00:03Z\nnamespace: task",
            "ACTIVE_MEMORY",
        )
        .await;
        write_task_memory(
            &task_dir,
            "archived.md",
            "created_at: 2026-05-22T00:00:02Z\nnamespace: task\nstatus: archived",
            "ARCHIVED_MEMORY",
        )
        .await;
        write_task_memory(
            &task_dir,
            "superseded.md",
            "created_at: 2026-05-22T00:00:01Z\nnamespace: task\nstatus: superseded",
            "SUPERSEDED_MEMORY",
        )
        .await;

        let meta = task_meta(task_id, "planner", None);
        let files = injected_task_memory_files(&app, task_id, Some(&meta)).await;
        let content = combined_task_memory_content(&files);

        assert!(content.contains("ACTIVE_MEMORY"));
        assert!(!content.contains("ARCHIVED_MEMORY"));
        assert!(!content.contains("SUPERSEDED_MEMORY"));
        assert!(content.contains(
            "Showing 1 of 3 memories (2 archived/superseded skipped, 0 dropped over budget)."
        ));
    }

    #[tokio::test]
    async fn task_memory_injection_keeps_pinned_memories_over_budget() {
        let task_id = "task-memory-pinned";
        let (_temp, app, task_dir) = task_memory_test_app(task_id).await;
        let large_body = "x".repeat(MAX_TASK_MEMORY_CONTENT_SIZE + 200);
        for idx in 0..10 {
            write_task_memory(
                &task_dir,
                &format!("bulk-{}.md", idx),
                &format!("created_at: 2026-05-22T00:00:{:02}Z\nnamespace: task", idx),
                &large_body,
            )
            .await;
        }
        write_task_memory(
            &task_dir,
            "pinned.md",
            "created_at: 2026-05-22T00:01:00Z\nnamespace: card:other\npinned: true",
            "PINNED_MEMORY",
        )
        .await;

        let meta = task_meta(task_id, "agents", Some("T-1"));
        let files = injected_task_memory_files(&app, task_id, Some(&meta)).await;
        let content = combined_task_memory_content(&files);

        assert!(content.contains("PINNED_MEMORY"));
        assert!(content.contains("dropped over budget"));
    }

    #[tokio::test]
    async fn task_memory_injection_agent_uses_card_global_task_and_dependencies() {
        let task_id = "task-memory-agent-scope";
        let (_temp, app, task_dir) = task_memory_test_app(task_id).await;
        crate::tasks::storage::save_board(
            app.gcx.clone(),
            task_id,
            &TaskBoard {
                cards: vec![
                    board_card("T-1", "done", vec![]),
                    board_card("T-2", "planned", vec![]),
                    board_card("T-3", "doing", vec!["T-1"]),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        write_task_memory(&task_dir, "global.md", "namespace: global", "GLOBAL_MEMORY").await;
        write_task_memory(&task_dir, "task.md", "namespace: task", "TASK_MEMORY").await;
        write_task_memory(
            &task_dir,
            "current.md",
            "namespace: card:T-3",
            "CURRENT_CARD_MEMORY",
        )
        .await;
        write_task_memory(
            &task_dir,
            "dependency.md",
            "namespace: card:T-1",
            "DEPENDENCY_MEMORY",
        )
        .await;
        write_task_memory(
            &task_dir,
            "other.md",
            "namespace: card:T-2",
            "OTHER_CARD_MEMORY",
        )
        .await;

        let meta = task_meta(task_id, "agents", Some("T-3"));
        let files = injected_task_memory_files(&app, task_id, Some(&meta)).await;
        let content = combined_task_memory_content(&files);

        assert!(content.contains("GLOBAL_MEMORY"));
        assert!(content.contains("TASK_MEMORY"));
        assert!(content.contains("CURRENT_CARD_MEMORY"));
        assert!(content.contains("DEPENDENCY_MEMORY"));
        assert!(!content.contains("OTHER_CARD_MEMORY"));
    }

    #[tokio::test]
    async fn task_memory_injection_planner_uses_doing_card_global_and_task() {
        let task_id = "task-memory-planner-scope";
        let (_temp, app, task_dir) = task_memory_test_app(task_id).await;
        crate::tasks::storage::save_board(
            app.gcx.clone(),
            task_id,
            &TaskBoard {
                cards: vec![
                    board_card("T-1", "doing", vec![]),
                    board_card("T-2", "planned", vec![]),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        write_task_memory(&task_dir, "global.md", "namespace: global", "GLOBAL_MEMORY").await;
        write_task_memory(&task_dir, "task.md", "namespace: task", "TASK_MEMORY").await;
        write_task_memory(
            &task_dir,
            "doing.md",
            "namespace: card:T-1",
            "DOING_CARD_MEMORY",
        )
        .await;
        write_task_memory(
            &task_dir,
            "planned.md",
            "namespace: card:T-2",
            "PLANNED_CARD_MEMORY",
        )
        .await;

        let meta = task_meta(task_id, "planner", None);
        let files = injected_task_memory_files(&app, task_id, Some(&meta)).await;
        let content = combined_task_memory_content(&files);

        assert!(content.contains("GLOBAL_MEMORY"));
        assert!(content.contains("TASK_MEMORY"));
        assert!(content.contains("DOING_CARD_MEMORY"));
        assert!(!content.contains("PLANNED_CARD_MEMORY"));
    }

    #[tokio::test]
    async fn task_memory_injection_budget_footer_counts_drops() {
        let task_id = "task-memory-budget";
        let (_temp, app, task_dir) = task_memory_test_app(task_id).await;
        let large_body = "y".repeat(MAX_TASK_MEMORY_CONTENT_SIZE + 200);
        for idx in 0..10 {
            write_task_memory(
                &task_dir,
                &format!("memory-{}.md", idx),
                &format!("created_at: 2026-05-22T00:00:{:02}Z\nnamespace: task", idx),
                &large_body,
            )
            .await;
        }

        let meta = task_meta(task_id, "planner", None);
        let files = injected_task_memory_files(&app, task_id, Some(&meta)).await;
        let content = combined_task_memory_content(&files);

        assert!(content.contains(
            "Showing 8 of 10 memories (0 archived/superseded skipped, 2 dropped over budget)."
        ));
    }

    #[tokio::test]
    async fn task_briefing_small_content_skips_briefing() {
        let _guard = reset_task_briefing_tests().await;
        let task_id = "task-briefing-small";
        let (_temp, app, task_dir) = task_memory_test_app(task_id).await;
        set_task_briefing_test_responses(
            task_id,
            vec![Ok("## Active Plans\n- should not be used".to_string())],
        )
        .await;
        write_task_memory(
            &task_dir,
            "small.md",
            "created_at: 2026-05-22T00:00:00Z\nnamespace: task",
            "SMALL",
        )
        .await;

        let meta = task_meta(task_id, "planner", None);
        let files = injected_task_memory_files(&app, task_id, Some(&meta)).await;
        let content = combined_task_memory_content(&files);

        assert!(content.contains("SMALL"));
        assert_eq!(task_briefing_test_calls().await, 0);
    }

    #[tokio::test]
    async fn task_briefing_cached_without_second_subchat_call() {
        let _guard = reset_task_briefing_tests().await;
        let task_id = "task-briefing-cache";
        let (_temp, app, task_dir) = task_memory_test_app(task_id).await;
        set_task_briefing_test_responses(
            task_id,
            vec![Ok("## Active Plans\n- Use cached plan [cache.md]\n\n## Key Decisions\n- Keep cache [cache.md]\n\n## Gotchas / Risks\n- None\n\n## Current State\n- Ready".to_string())],
        )
        .await;
        write_task_memory(
            &task_dir,
            "large.md",
            "created_at: 2026-05-22T00:00:00Z\nnamespace: task",
            &"CACHE_MEMORY ".repeat(300),
        )
        .await;

        let meta = task_meta(task_id, "planner", None);
        let first = injected_task_memory_files(&app, task_id, Some(&meta)).await;
        assert!(combined_task_memory_content(&first).contains("CACHE_MEMORY"));
        wait_for_task_briefing(1).await;
        let second =
            injected_task_context_files(&app, task_id, Some(&meta), TASK_BRIEFING_CONTEXT_MARKER)
                .await;
        let third =
            injected_task_context_files(&app, task_id, Some(&meta), TASK_BRIEFING_CONTEXT_MARKER)
                .await;

        assert_eq!(task_briefing_test_calls().await, 1);
        assert!(combined_task_memory_content(&second).contains("Use cached plan"));
        assert_eq!(
            combined_task_memory_content(&second),
            combined_task_memory_content(&third)
        );
    }

    #[tokio::test]
    async fn task_briefing_failure_preserves_raw_and_retries() {
        let _guard = reset_task_briefing_tests().await;
        let task_id = "task-briefing-fallback";
        let (_temp, app, task_dir) = task_memory_test_app(task_id).await;
        set_task_briefing_test_responses(
            task_id,
            vec![
                Err("briefing unavailable".to_string()),
                Ok("## Active Plans\n- Retry succeeded".to_string()),
            ],
        )
        .await;
        write_task_memory(
            &task_dir,
            "large.md",
            "created_at: 2026-05-22T00:00:00Z\nnamespace: task",
            &"FALLBACK_MEMORY ".repeat(250),
        )
        .await;

        let meta = task_meta(task_id, "planner", None);
        let files = injected_task_memory_files(&app, task_id, Some(&meta)).await;
        let content = combined_task_memory_content(&files);

        assert!(content.contains("FALLBACK_MEMORY"));
        assert!(content.contains("Showing 1 of 1 memories"));
        wait_for_task_briefing(1).await;

        let retry = injected_task_memory_files(&app, task_id, Some(&meta)).await;
        assert!(combined_task_memory_content(&retry).contains("FALLBACK_MEMORY"));
        wait_for_task_briefing(2).await;

        let briefing =
            injected_task_context_files(&app, task_id, Some(&meta), TASK_BRIEFING_CONTEXT_MARKER)
                .await;
        assert!(combined_task_memory_content(&briefing).contains("Retry succeeded"));
        assert_eq!(task_briefing_test_calls().await, 2);
    }

    #[tokio::test]
    async fn task_briefing_concurrent_misses_share_subchat() {
        let _guard = reset_task_briefing_tests().await;
        let task_id = "task-briefing-concurrent";
        let (_temp, app, task_dir) = task_memory_test_app(task_id).await;
        set_task_briefing_test_responses(
            task_id,
            vec![Ok("## Active Plans\n- Shared result".to_string())],
        )
        .await;
        write_task_memory(
            &task_dir,
            "large.md",
            "created_at: 2026-05-22T00:00:00Z\nnamespace: task",
            &"CONCURRENT_MEMORY ".repeat(250),
        )
        .await;

        let meta = task_meta(task_id, "planner", None);
        let (first, second) = tokio::join!(
            injected_task_memory_files(&app, task_id, Some(&meta)),
            injected_task_memory_files(&app, task_id, Some(&meta))
        );
        assert!(combined_task_memory_content(&first).contains("CONCURRENT_MEMORY"));
        assert!(combined_task_memory_content(&second).contains("CONCURRENT_MEMORY"));
        wait_for_task_briefing(1).await;

        let briefing =
            injected_task_context_files(&app, task_id, Some(&meta), TASK_BRIEFING_CONTEXT_MARKER)
                .await;
        assert!(combined_task_memory_content(&briefing).contains("Shared result"));
        assert_eq!(task_briefing_test_calls().await, 1);
    }

    #[tokio::test]
    async fn task_briefing_renders_expected_sections_and_pinned_documents() {
        let _guard = reset_task_briefing_tests().await;
        let task_id = "task-briefing-render";
        let prompt = task_briefing_prompt("agents", "SOURCE");
        assert!(prompt.contains("## Active Plans"));
        assert!(prompt.contains("## Key Decisions"));
        assert!(prompt.contains("## Gotchas / Risks"));
        assert!(prompt.contains("## Current State"));
        let (_temp, app, task_dir) = task_memory_test_app(task_id).await;
        set_task_briefing_test_responses(
            task_id,
            vec![Ok("## Active Plans\n- Follow the rollout plan [plan.md]\n\n## Key Decisions\n- Use task documents [brief.md]\n\n## Gotchas / Risks\n- Watch cache invalidation [risk.md]\n\n## Current State\n- Implementation is in progress".to_string())],
        )
        .await;
        write_task_memory(
            &task_dir,
            "risk.md",
            "created_at: 2026-05-22T00:00:01Z\nnamespace: task\nkind: risk",
            &"Risk details ".repeat(180),
        )
        .await;
        write_task_document(
            &task_dir,
            "brief.md",
            "name: Brief\nslug: brief\nkind: brief\ncreated_at: 2026-05-22T00:00:02Z\nupdated_at: 2026-05-22T00:00:02Z\nauthor_role: planner\npinned: true\nversion: 1",
            &"Pinned task document ".repeat(80),
        )
        .await;

        let meta = task_meta(task_id, "agents", Some("T-1"));
        let raw = injected_task_memory_files(&app, task_id, Some(&meta)).await;
        let raw_content = combined_task_memory_content(&raw);
        assert!(raw_content.contains("Risk details"));
        assert!(raw_content.contains("Pinned task document"));
        wait_for_task_briefing(1).await;

        let files =
            injected_task_context_files(&app, task_id, Some(&meta), TASK_BRIEFING_CONTEXT_MARKER)
                .await;
        let content = combined_task_memory_content(&files);

        assert!(content.contains("## Active Plans"));
        assert!(content.contains("## Key Decisions"));
        assert!(content.contains("## Gotchas / Risks"));
        assert!(content.contains("## Current State"));
        assert!(content.contains("[brief.md]"));
    }

    #[tokio::test]
    async fn every_specified_mode_substitutes_buddy_personality_marker() {
        let _guard = prompt_test_lock().lock().await;
        let app = make_gcx_with_buddy().await;

        for case in SPECIFIED_MODES {
            let prompt = get_mode_system_prompt(app.clone(), case.lookup_mode, case.model_id).await;
            assert!(
                prompt.contains(BUDDY_PERSONALITY_MARKER),
                "mode missing marker: {}",
                case.label
            );
            let rendered = system_prompt_add_extra_instructions(
                app.clone(),
                prompt,
                HashSet::new(),
                &ChatMeta {
                    chat_mode: case.lookup_mode.to_string(),
                    include_project_info: false,
                    ..Default::default()
                },
                &None,
                case.cache_mode,
            )
            .await;

            assert!(rendered.contains("You are Pixel, a Helper Sprite"));
            assert!(!rendered.contains(BUDDY_PERSONALITY_MARKER));
        }
    }

    #[tokio::test]
    async fn codegraph_instructions_expand_only_when_codegraph_is_available() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let prompt = "%CODEGRAPH_INSTRUCTIONS%".to_string();
        let rendered = system_prompt_add_extra_instructions(
            app.clone(),
            prompt.clone(),
            HashSet::from(["codegraph_overview".to_string()]),
            &ChatMeta::default(),
            &None,
            "explore",
        )
        .await;
        assert!(rendered.contains("## CodeGraph Tools"));
        assert!(rendered.contains("same workspace-relative, absolute, or active-worktree forms"));
        assert!(!rendered.contains("%CODEGRAPH_INSTRUCTIONS%"));

        let rendered_subset = system_prompt_add_extra_instructions(
            app.clone(),
            prompt.clone(),
            HashSet::from(["code_health".to_string()]),
            &ChatMeta::default(),
            &None,
            "explore",
        )
        .await;
        assert!(rendered_subset.contains("## CodeGraph Tools"));

        let without_codegraph = system_prompt_add_extra_instructions(
            app,
            prompt,
            HashSet::new(),
            &ChatMeta::default(),
            &None,
            "explore",
        )
        .await;
        assert!(without_codegraph.is_empty());
    }

    #[tokio::test]
    async fn trajectory_label_instructions_expand_only_when_tool_is_available() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let prompt = "%TRAJECTORY_LABEL_INSTRUCTIONS%".to_string();
        let rendered = system_prompt_add_extra_instructions(
            app.clone(),
            prompt.clone(),
            HashSet::from(["set_trajectory_label".to_string()]),
            &ChatMeta::default(),
            &None,
            "ask",
        )
        .await;
        assert!(rendered.contains("Use `set_trajectory_label`"));
        assert!(!rendered.contains("%TRAJECTORY_LABEL_INSTRUCTIONS%"));

        let without_tool = system_prompt_add_extra_instructions(
            app,
            prompt,
            HashSet::new(),
            &ChatMeta::default(),
            &None,
            "ask",
        )
        .await;
        assert!(without_tool.is_empty());
    }

    #[tokio::test]
    async fn personality_block_caches_per_archetype_and_mode() {
        let _guard = prompt_test_lock().lock().await;
        let app = make_gcx_with_buddy().await;
        let prompt = format!("before\n{}\nafter", BUDDY_PERSONALITY_MARKER);

        let first = system_prompt_add_extra_instructions(
            app.clone(),
            prompt.clone(),
            HashSet::new(),
            &ChatMeta::default(),
            &None,
            "agent",
        )
        .await;
        set_buddy_name(&app, "Nova").await;
        let cached_same_mode = system_prompt_add_extra_instructions(
            app.clone(),
            prompt.clone(),
            HashSet::new(),
            &ChatMeta::default(),
            &None,
            "agent",
        )
        .await;
        let uncached_other_mode = system_prompt_add_extra_instructions(
            app.clone(),
            prompt,
            HashSet::new(),
            &ChatMeta::default(),
            &None,
            "explore",
        )
        .await;

        assert!(first.contains("You are Pixel"));
        assert!(cached_same_mode.contains("You are Nova"));
        assert!(uncached_other_mode.contains("You are Nova"));
    }

    #[tokio::test]
    async fn persona_cache_invalidates_on_identity_name_change() {
        let _guard = prompt_test_lock().lock().await;
        let app = make_gcx_with_buddy().await;
        let prompt = format!("{}", BUDDY_PERSONALITY_MARKER);
        let first = system_prompt_add_extra_instructions(
            app.clone(),
            prompt.clone(),
            HashSet::new(),
            &ChatMeta::default(),
            &None,
            "agent",
        )
        .await;

        set_buddy_name(&app, "Nova").await;
        let second = system_prompt_add_extra_instructions(
            app.clone(),
            prompt,
            HashSet::new(),
            &ChatMeta::default(),
            &None,
            "agent",
        )
        .await;

        assert!(first.contains("You are Pixel"));
        assert!(second.contains("You are Nova"));
    }

    #[tokio::test]
    async fn personality_cache_invalidates_on_reroll() {
        let _guard = prompt_test_lock().lock().await;
        let app = make_gcx_with_buddy().await;
        let prompt = format!("{}", BUDDY_PERSONALITY_MARKER);
        let first = system_prompt_add_extra_instructions(
            app.clone(),
            prompt.clone(),
            HashSet::new(),
            &ChatMeta::default(),
            &None,
            "agent",
        )
        .await;

        set_buddy_name(&app, "Nova").await;
        refact_buddy_core::state::mark_persona_cache_dirty();
        let second = system_prompt_add_extra_instructions(
            app.clone(),
            prompt,
            HashSet::new(),
            &ChatMeta::default(),
            &None,
            "agent",
        )
        .await;

        assert!(first.contains("You are Pixel"));
        assert!(second.contains("You are Nova"));
    }

    #[tokio::test]
    async fn missing_buddy_state_renders_empty_marker_replacement() {
        let _guard = prompt_test_lock().lock().await;
        clear_buddy_persona_cache_for_tests();
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let rendered = system_prompt_add_extra_instructions(
            app,
            format!("Alpha\n{}\nOmega", BUDDY_PERSONALITY_MARKER),
            HashSet::new(),
            &ChatMeta::default(),
            &None,
            "agent",
        )
        .await;

        assert!(!rendered.contains(BUDDY_PERSONALITY_MARKER));
        assert!(rendered.contains("Alpha\n\nOmega"));
    }

    #[tokio::test]
    async fn gather_and_inject_includes_personality_in_system_prompt() {
        let _guard = prompt_test_lock().lock().await;
        let app = make_gcx_with_buddy().await;
        let mut stream_back_to_user = HasRagResults::new();
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: ChatContent::SimpleText("hello".to_string()),
            ..Default::default()
        }];

        let (messages, _) = prepend_the_right_system_prompt_and_maybe_more_initial_messages(
            app,
            messages,
            &ChatMeta {
                chat_mode: "agent".to_string(),
                include_project_info: false,
                ..Default::default()
            },
            &None,
            &mut stream_back_to_user,
            HashSet::new(),
            "agent",
            "",
        )
        .await;

        let content = system_text(&messages);
        assert!(content.contains("You are Pixel, a Helper Sprite"));
        assert!(!content.contains(BUDDY_PERSONALITY_MARKER));
    }
}
