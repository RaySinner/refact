#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::Value;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use crate::app_state::AppState;
#[cfg(test)]
#[allow(unused_imports)]
use crate::chat::types::{ChatSession, TaskMeta};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tokens::{cached_tokenizer, count_text_tokens_with_fallback};

pub use refact_runtime_api::chat_cache_guard::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::ChatModelRecord;
    use serde_json::json;

    #[tokio::test]
    async fn cache_guard_allows_anthropic_suffix_changes_after_explicit_cache_marker() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session = session_with_task_role(None);
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));
        let prev_body = json!({
            "model": "test",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hello", "cache_control": {"type": "ephemeral"}}]
            }]
        });
        {
            let mut session = session_arc.lock().await;
            session.cache_guard_snapshot = Some(body_for_cache_guard(&prev_body));
        }

        let next_body = json!({
            "model": "test",
            "messages": [
                {
                    "role": "user",
                    "content": [{"type": "text", "text": "hello"}]
                },
                {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "changed suffix"}]
                },
                {
                    "role": "user",
                    "content": [{"type": "text", "text": "continue", "cache_control": {"type": "ephemeral"}}]
                }
            ]
        });
        let outcome = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, CacheGuardOutcome::Pass(Some(_))));
        let session = session_arc.lock().await;
        assert!(session.runtime.pause_reasons.is_empty());
    }

    #[tokio::test]
    async fn cache_guard_skips_requests_without_explicit_cache_markers() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session = session_with_task_role(None);
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));
        let prev_body = json!({
            "model": "test",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hello", "cache_control": {"type": "ephemeral"}}]
            }]
        });
        let previous_snapshot = body_for_cache_guard(&prev_body);
        {
            let mut session = session_arc.lock().await;
            session.cache_guard_snapshot = Some(previous_snapshot.clone());
        }

        let next_body = json!({
            "model": "test",
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call_function_test_1",
                    "name": "cat",
                    "input": {"paths": "refact-agent/gui/src/components/Buttons/index.tsx"}
                }]
            }]
        });
        let outcome = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, CacheGuardOutcome::Pass(None)));
        let session = session_arc.lock().await;
        assert!(session.runtime.pause_reasons.is_empty());
        assert_eq!(
            session.cache_guard_snapshot.as_ref(),
            Some(&previous_snapshot)
        );
    }

    #[tokio::test]
    async fn cache_guard_pauses_when_anthropic_explicit_cache_prefix_changes() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session = session_with_task_role(None);
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));
        let prev_body = json!({
            "model": "test",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hello", "cache_control": {"type": "ephemeral"}}]
            }]
        });
        {
            let mut session = session_arc.lock().await;
            session.cache_guard_snapshot = Some(body_for_cache_guard(&prev_body));
        }

        let next_body = json!({
            "model": "test",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "changed", "cache_control": {"type": "ephemeral"}}]
            }]
        });
        let outcome = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, CacheGuardOutcome::Paused { .. }));
        let session = session_arc.lock().await;
        assert_eq!(session.runtime.pause_reasons.len(), 1);
    }

    #[tokio::test]
    async fn cache_guard_allows_tool_call_timing_metadata_changes() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session_arc = Arc::new(AMutex::new(session_with_task_role(None)));
        let prev_body = json!({
            "model": "test",
            "messages": [{
                "role": "assistant",
                "content": [{"type": "text", "text": "stable", "cache_control": {"type": "ephemeral"}}],
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "cat", "arguments": "{}"}
                }]
            }]
        });
        {
            let mut session = session_arc.lock().await;
            session.cache_guard_snapshot = Some(body_for_cache_guard(&prev_body));
        }
        let mut next_body = prev_body.clone();
        next_body["messages"][0]["tool_calls"][0]["started_at_ms"] = json!(1_700_000_000_000u64);
        next_body["messages"][0]["tool_calls"][0]["completed_at_ms"] = json!(1_700_000_000_123u64);

        let outcome = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, CacheGuardOutcome::Pass(Some(_))));
        assert!(session_arc.lock().await.runtime.pause_reasons.is_empty());
    }

    #[test]
    fn cache_guard_stale_snapshot_recheck_passes_without_pause() {
        let captured_previous = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "user", "content": "old"}],
            "model": "test"
        }));
        let current = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "user", "content": "new"}],
            "model": "test"
        }));
        let sanitized = sanitize_body_for_cache_guard(&json!({
            "messages": [
                {"role": "user", "content": "new"},
                {"role": "assistant", "content": "hello"}
            ],
            "model": "test"
        }));

        let outcome = cache_guard_outcome_if_snapshot_changed(
            Some(&current),
            &captured_previous,
            &sanitized,
            false,
            1,
            0,
        );

        assert!(matches!(outcome, Some(CacheGuardOutcome::Pass(Some(_)))));
    }

    #[test]
    fn cache_guard_stale_snapshot_recheck_still_pauses_on_new_violation() {
        let captured_previous = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "user", "content": "old"}],
            "model": "test"
        }));
        let current = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "user", "content": "new"}],
            "model": "test"
        }));
        let sanitized = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "assistant", "content": "different"}],
            "model": "test"
        }));

        let outcome = cache_guard_outcome_if_snapshot_changed(
            Some(&current),
            &captured_previous,
            &sanitized,
            false,
            1,
            0,
        );

        assert!(outcome.is_none());
    }

    #[test]
    fn cache_guard_stale_snapshot_recheck_noops_when_snapshot_unchanged() {
        let captured_previous = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "user", "content": "old"}],
            "model": "test"
        }));
        let sanitized = sanitize_body_for_cache_guard(&json!({
            "messages": [{"role": "assistant", "content": "different"}],
            "model": "test"
        }));

        let outcome = cache_guard_outcome_if_snapshot_changed(
            Some(&captured_previous),
            &captured_previous,
            &sanitized,
            false,
            1,
            0,
        );

        assert!(outcome.is_none());
    }

    #[tokio::test]
    async fn cache_guard_enabled_for_normal_cache_supported_chat() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session = session_with_task_role(None);

        assert!(is_guard_enabled(app, "test/model-with-cache", &session).await);
    }

    #[tokio::test]
    async fn cache_guard_enablement_ignores_cache_pricing_without_model_cache_support() {
        let app =
            app_with_cache_priced_model_without_cache_support("test/model-with-cache-price").await;
        let session = session_with_task_role(None);

        assert!(
            !is_guard_enabled(app, "test/model-with-cache-price", &session).await,
            "cache pricing metadata must not enable cache guard without model cache support"
        );
    }

    #[tokio::test]
    async fn cache_guard_disabled_for_task_management_roles() {
        for role in ["planner", "agents", "agent", "task_agent"] {
            let app = app_with_cache_priced_model("test/model-with-cache").await;
            let session = session_with_task_role(Some(role));

            assert!(
                !is_guard_enabled(app, "test/model-with-cache", &session).await,
                "cache guard should be disabled for task role {role}"
            );
        }
    }

    fn session_with_link_type(link_type: &str) -> crate::chat::types::ChatSession {
        let mut session =
            crate::chat::types::ChatSession::new("test-cache-guard-child".to_string());
        session.thread.parent_id = Some("parent-chat".to_string());
        session.thread.link_type = Some(link_type.to_string());
        session
    }

    #[tokio::test]
    async fn cache_guard_disabled_for_subagentic_child_threads() {
        for link_type in ["subagent", "buddy", "task_agent"] {
            let app = app_with_cache_priced_model("test/model-with-cache").await;
            let session = session_with_link_type(link_type);

            assert!(
                !is_guard_enabled(app, "test/model-with-cache", &session).await,
                "cache guard must be disabled for non-interactive child link type {link_type}"
            );
        }
    }

    #[tokio::test]
    async fn cache_guard_stays_enabled_for_user_facing_child_threads() {
        for link_type in ["handoff", "mode_transition", "branch"] {
            let app = app_with_cache_priced_model("test/model-with-cache").await;
            let session = session_with_link_type(link_type);

            assert!(
                is_guard_enabled(app, "test/model-with-cache", &session).await,
                "cache guard must stay enabled for user-facing link type {link_type}"
            );
        }
    }

    #[tokio::test]
    async fn cache_guard_never_pauses_subagent_child_on_prefix_break() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let mut session = session_with_link_type("subagent");
        let prev_body = json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "seed", "cache_control": {"type": "ephemeral"}}]
            }],
            "model": "test"
        });
        session.cache_guard_snapshot = Some(body_for_cache_guard(&prev_body));
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));

        let next_body = json!({
            "messages": [{
                "role": "assistant",
                "content": [{"type": "text", "text": "rewritten prefix", "cache_control": {"type": "ephemeral"}}]
            }],
            "model": "test"
        });

        let outcome = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();

        assert!(
            matches!(outcome, CacheGuardOutcome::Pass(None)),
            "a subagent child has no user to force-continue, so it must never pause"
        );
        let session = session_arc.lock().await;
        assert!(session.runtime.pause_reasons.is_empty());
        assert!(!session.abort_flag.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cache_guard_pass_returns_pass_outcome() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let session = crate::chat::types::ChatSession::new("test-pass".to_string());
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));
        let body = json!({"messages": [{"role": "user", "content": "hi"}]});
        let outcome = check_or_pause_cache_guard(app, session_arc, "some/model", &body)
            .await
            .unwrap();
        assert!(matches!(outcome, CacheGuardOutcome::Pass(_)));
    }

    #[tokio::test]
    async fn cache_guard_violation_in_normal_chat_pauses() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session = session_with_task_role(None);
        let prev_body = json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hello", "cache_control": {"type": "ephemeral"}}]
            }],
            "model": "test"
        });
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));

        {
            let mut s = session_arc.lock().await;
            s.cache_guard_snapshot = Some(sanitize_body_for_cache_guard(&prev_body));
        }

        let next_body = json!({
            "messages": [{
                "role": "assistant",
                "content": [{"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}]
            }],
            "model": "test"
        });
        let outcome = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, CacheGuardOutcome::Paused { .. }));
        let session = session_arc.lock().await;
        assert_eq!(session.runtime.pause_reasons.len(), 1);
        assert!(is_cache_guard_pause_reason(
            &session.runtime.pause_reasons[0]
        ));
    }

    #[tokio::test]
    async fn cache_guard_violation_pause_reason_redacts_secret_content() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session = session_with_task_role(None);
        let prev_body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "Bearer sk-secret-123 api_key=abcd",
                    "cache_control": {"type": "ephemeral"}
                }]
            }],
            "model": "test"
        });
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));

        {
            let mut s = session_arc.lock().await;
            s.cache_guard_snapshot = Some(sanitize_body_for_cache_guard(&prev_body));
        }

        let next_body = json!({
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "text",
                    "text": "changed Bearer sk-secret-123 api_key=abcd",
                    "cache_control": {"type": "ephemeral"}
                }]
            }],
            "model": "test"
        });
        let outcome = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();

        let CacheGuardOutcome::Paused { reason } = outcome else {
            panic!("cache guard should pause on changed prefix");
        };
        assert!(reason.contains("Prompt cache append-only prefix check failed"));
        assert!(!reason.contains("sk-secret-123"));
        assert!(!reason.contains("api_key=abcd"));
        assert!(reason.contains("[redacted text chars="));
        assert!(reason.contains("sha256="));
        let session = session_arc.lock().await;
        assert_eq!(session.runtime.pause_reasons[0].command, reason);
    }

    #[tokio::test]
    async fn cache_guard_violation_pause_reason_caps_huge_redacted_diff() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session = session_with_task_role(None);
        let prev_body = json!({
            "messages": (0..900).map(|i| json!({
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": format!("safe-prefix-{i}"),
                    "cache_control": {"type": "ephemeral"}
                }]
            })).collect::<Vec<_>>(),
            "metadata": {"auth_token": "raw-token-value"},
            "model": "test"
        });
        let mut next_body = prev_body.clone();
        for item in next_body["messages"].as_array_mut().unwrap() {
            item["extra"] = json!("Bearer sk-secret-123 api_key=abcd");
        }
        next_body["metadata"]["auth_token"] = json!("changed-raw-token-value");
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));

        {
            let mut s = session_arc.lock().await;
            s.cache_guard_snapshot = Some(sanitize_body_for_cache_guard(&prev_body));
        }

        let outcome = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();

        let CacheGuardOutcome::Paused { reason } = outcome else {
            panic!("cache guard should pause on changed prefix");
        };
        assert!(reason.chars().count() < CACHE_GUARD_DIFF_MAX_CHARS / 2);
        assert!(reason.contains("bounded structural preview"));
        assert!(reason.contains("cache guard preview truncated"));
        assert!(!reason.contains("sk-secret-123"));
        assert!(!reason.contains("api_key=abcd"));
        assert!(!reason.contains("raw-token-value"));
        assert!(reason.contains("[REDACTED]"));
        let session = session_arc.lock().await;
        assert_eq!(session.runtime.pause_reasons[0].command, reason);
    }

    #[tokio::test]
    async fn cache_guard_violation_in_task_agent_chat_passes_without_pausing() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session = session_with_task_role(Some("agents"));
        let prev_body =
            json!({"messages": [{"role": "user", "content": "hello"}], "model": "test"});
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));

        {
            let mut s = session_arc.lock().await;
            s.cache_guard_snapshot = Some(sanitize_body_for_cache_guard(&prev_body));
        }

        let next_body =
            json!({"messages": [{"role": "assistant", "content": "hi"}], "model": "test"});
        let outcome = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, CacheGuardOutcome::Pass(None)));
        let session = session_arc.lock().await;
        assert!(session.runtime.pause_reasons.is_empty());
    }

    #[tokio::test]
    async fn cache_guard_force_next_allows_one_intentional_reset() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session = session_with_task_role(None);
        let prev_body = json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hello", "cache_control": {"type": "ephemeral"}}]
            }],
            "model": "test"
        });
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));

        {
            let mut s = session_arc.lock().await;
            s.cache_guard_snapshot = Some(body_for_cache_guard(&prev_body));
            s.reset_cache_guard_snapshot();
        }

        let next_body = json!({
            "messages": [{
                "role": "assistant",
                "content": [{"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}]
            }],
            "model": "test"
        });
        let outcome = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();

        let CacheGuardOutcome::Pass(Some(snapshot)) = outcome else {
            panic!("cache guard should force a snapshot commit")
        };
        {
            let session = session_arc.lock().await;
            assert!(
                session.cache_guard_force_next,
                "force-next must stay armed until the snapshot commit succeeds"
            );
            assert!(session.runtime.pause_reasons.is_empty());
        }

        let expected = snapshot.body.clone();
        commit_cache_guard_snapshot(session_arc.clone(), snapshot).await;
        let session = session_arc.lock().await;
        assert!(!session.cache_guard_force_next);
        assert_eq!(session.cache_guard_snapshot.as_ref(), Some(&expected));
    }

    #[tokio::test]
    async fn cache_guard_concurrent_commits_keep_newest_snapshot() {
        let session_arc = Arc::new(AMutex::new(session_with_task_role(None)));
        let older = CacheGuardSnapshot {
            body: json!({"messages": [{"role": "user", "content": "older"}]}),
            request_generation: 10,
            reset_generation: 0,
        };
        let newer = CacheGuardSnapshot {
            body: json!({"messages": [{"role": "user", "content": "newer"}]}),
            request_generation: 11,
            reset_generation: 0,
        };

        commit_cache_guard_snapshot(session_arc.clone(), newer.clone()).await;
        commit_cache_guard_snapshot(session_arc.clone(), older).await;

        let session = session_arc.lock().await;
        assert_eq!(session.cache_guard_snapshot.as_ref(), Some(&newer.body));
        assert_eq!(
            session.cache_guard_snapshot_generation,
            newer.request_generation
        );
    }

    #[tokio::test]
    async fn cache_guard_reset_rejects_in_flight_pre_reset_snapshot() {
        let session_arc = Arc::new(AMutex::new(session_with_task_role(None)));
        let snapshot = CacheGuardSnapshot {
            body: json!({"messages": [{"role": "user", "content": "stale"}]}),
            request_generation: 1,
            reset_generation: 0,
        };
        {
            let mut session = session_arc.lock().await;
            session.reset_cache_guard_snapshot();
        }

        commit_cache_guard_snapshot(session_arc.clone(), snapshot).await;

        let session = session_arc.lock().await;
        assert!(session.cache_guard_snapshot.is_none());
        assert!(session.cache_guard_force_next);
    }

    #[tokio::test]
    async fn cache_guard_force_next_survives_pass_without_commit() {
        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session = session_with_task_role(None);
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));
        let prev_body = json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "pre-compaction", "cache_control": {"type": "ephemeral"}}]
            }],
            "model": "test"
        });
        let next_body = json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "post-compaction", "cache_control": {"type": "ephemeral"}}]
            }],
            "model": "test"
        });
        {
            let mut session = session_arc.lock().await;
            session.cache_guard_snapshot = Some(body_for_cache_guard(&prev_body));
            session.reset_cache_guard_snapshot();
        }

        let first = check_or_pause_cache_guard(
            app.clone(),
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();
        assert!(matches!(first, CacheGuardOutcome::Pass(Some(_))));
        {
            let session = session_arc.lock().await;
            assert!(session.cache_guard_force_next);
            assert!(session.runtime.pause_reasons.is_empty());
        }

        let retry = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_body,
        )
        .await
        .unwrap();
        assert!(matches!(retry, CacheGuardOutcome::Pass(Some(_))));
        let session = session_arc.lock().await;
        assert!(session.cache_guard_force_next);
        assert!(session.runtime.pause_reasons.is_empty());
    }

    #[tokio::test]
    async fn cache_guard_skips_openai_previous_response_id() {
        // When previous_response_id is present, the request body uses tail-only mode
        // (only new messages after last assistant), so the full-body comparison is invalid.
        // The cache guard should skip validation in this case.
        let prev = json!({
            "model": "gpt-4.1",
            "instructions": "You are helpful",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hello"}]}
            ],
            "store": true
        });
        let next_stateful = json!({
            "model": "gpt-4.1",
            "instructions": "You are helpful",
            "input": [
                {"type": "function_call_output", "call_id": "call_1", "output": "result"}
            ],
            "store": true,
            "previous_response_id": "resp_abc123"
        });
        // This SHOULD fail the append-only check (different input, extra key)
        assert!(!is_append_only_prefix(
            &sanitize_body_for_cache_guard(&prev),
            &sanitize_body_for_cache_guard(&next_stateful),
        ));

        let app = app_with_cache_priced_model("test/model-with-cache").await;
        let session = session_with_task_role(None);
        let session_arc = Arc::new(tokio::sync::Mutex::new(session));
        {
            let mut session = session_arc.lock().await;
            session.cache_guard_snapshot = Some(sanitize_body_for_cache_guard(&prev));
        }

        let outcome = check_or_pause_cache_guard(
            app,
            session_arc.clone(),
            "test/model-with-cache",
            &next_stateful,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, CacheGuardOutcome::Pass(None)));
        let session = session_arc.lock().await;
        assert!(session.runtime.pause_reasons.is_empty());
    }

    async fn app_with_cache_priced_model(model_id: &str) -> AppState {
        app_with_cache_priced_model_support(model_id, true).await
    }

    async fn app_with_cache_priced_model_without_cache_support(model_id: &str) -> AppState {
        app_with_cache_priced_model_support(model_id, false).await
    }

    async fn app_with_cache_priced_model_support(
        model_id: &str,
        supports_cache_control: bool,
    ) -> AppState {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let app = crate::app_state::app_state_from_gcx(gcx).await;
        let pricing = crate::providers::traits::ModelPricing {
            prompt: 2.5,
            generated: 10.0,
            cache_read: Some(1.25),
            cache_creation: None,
            context_over_200k: None,
        };
        let mut model_caps = std::collections::HashMap::new();
        model_caps.insert(
            model_id.to_string(),
            crate::caps::model_caps::ModelCapabilities {
                n_ctx: 128_000,
                max_output_tokens: 16_384,
                supports_cache_control: true,
                pricing: Some(pricing),
                ..Default::default()
            },
        );
        let mut code_caps = crate::caps::CodeAssistantCaps {
            model_caps: std::sync::Arc::new(model_caps),
            ..Default::default()
        };
        code_caps.chat_models.insert(
            model_id.to_string(),
            std::sync::Arc::new(ChatModelRecord {
                base: crate::caps::BaseModelRecord {
                    id: model_id.to_string(),
                    name: model_id.to_string(),
                    n_ctx: 128_000,
                    supports_cache_control,
                    ..Default::default()
                },
                ..Default::default()
            }),
        );
        {
            let mut caps = app.model.caps.write().await;
            caps.caps = Some(std::sync::Arc::new(code_caps));
            caps.last_attempted_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
        }
        app
    }

    fn task_meta(role: &str) -> TaskMeta {
        TaskMeta {
            task_id: "task-1".to_string(),
            role: role.to_string(),
            agent_id: None,
            card_id: None,
            planner_chat_id: None,
        }
    }

    fn session_with_task_role(role: Option<&str>) -> crate::chat::types::ChatSession {
        let mut session = crate::chat::types::ChatSession::new("test-cache-guard".to_string());
        session.thread.task_meta = role.map(task_meta);
        session
    }
}
