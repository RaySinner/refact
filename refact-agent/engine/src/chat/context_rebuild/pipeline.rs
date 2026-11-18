//! Engine-side re-export of model-based context reconstruction.
//!
//! The body moved to `refact_runtime_api::chat_context_rebuild`, which reaches the
//! same behaviour through `ChatCtx` seams (`tool_registry`, `prompts`, `mode_config`,
//! `subchat`, `pending_handoff`) and `refact_chat_history::chat_compress`. Every
//! symbol below keeps its old name, so no call site changed.
pub use refact_runtime_api::chat_context_rebuild::*;

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use refact_chat_api::compression_gate::compression_attempt_active;
    use refact_chat_api::context_rebuild::{
        at_auto_cap, idle, status, AttemptGuard, CompactionOutcome,
    };
    use refact_chat_api::pending_context_rebuild::PendingContextRebuild;
    use refact_chat_api::{
        ChatMessage, ChatSession, CompressionPhase, SessionState, ThreadParams,
    };
    use refact_core::active_context::{
        active_context, make_reconstruction_report, ReconstructionMetadata,
    };
    use refact_core::chat_types::ChatUsage;

    use super::*;
    use refact_runtime_api::chat_context_rebuild::{
        budget_prefix_is_usable, commit_report, estimated_tokens, payload_request_cap,
        validate_output,
    };

    fn fixture() -> (ChatSession, ThreadParams, u64, ChatMessage) {
        let mut s = ChatSession::new("transaction-test".into());
        s.add_message(ChatMessage::new(
            "user".into(),
            "ARCHIVED_SENTINEL ".repeat(100),
        ));
        s.active_compression_attempt = Some(1);
        status(&mut s, CompressionPhase::Running, None);
        let mut context = ChatMessage::new("user".into(), "active request".into());
        context.message_id = "rebuilt-request".into();
        let report =
            make_reconstruction_report(vec![context], ReconstructionMetadata::default()).unwrap();
        let thread = s.thread.clone();
        let version = s.trajectory_version;
        (s, thread, version, report)
    }

    #[test]
    fn unavailable_legacy_estimate_is_not_zero_or_a_successful_fit() {
        let mut legacy = ChatMessage::new("assistant".into(), "legacy".into());
        legacy.summarized_range = Some((0, 0));
        assert_eq!(estimated_tokens(&[legacy.clone()], None, false), usize::MAX);
        assert!(validate_output(
            &[legacy],
            &[ChatMessage::new("user".into(), "x".into())],
            usize::MAX,
            None
        )
        .is_err());
    }

    #[test]
    fn smaller_output_cannot_hide_mandatory_prefix_or_missing_budget() {
        let s = ChatSession::new("budget".into());
        let mut thread = s.thread.clone();
        assert!(payload_request_cap(&thread, 4096, 512, None).is_err());
        thread.frozen_request_prefix = Some(refact_chat_api::FrozenRequestPrefix {
            schema_version: 1,
            created_at: String::new(),
            system_prompt: Some("system".repeat(2000)),
            tools_canonical: Some(serde_json::json!([])),
        });
        assert!(payload_request_cap(&thread, 4096, 512, None).is_err());
        thread.frozen_request_prefix.as_mut().unwrap().system_prompt = Some("system".into());
        let cap = payload_request_cap(&thread, 4096, 512, None).unwrap();
        let source = vec![ChatMessage::new("user".into(), "x".repeat(24000))];
        let output = vec![ChatMessage::new("user".into(), "x".repeat(9000))];
        assert!(validate_output(&source, &output, 4096, None).is_ok());
        assert!(validate_output(&source, &output, cap, None).is_err());
    }

    #[test]
    fn incomplete_frozen_prefix_is_an_unavailable_budget_not_a_missing_prompt() {
        let s = ChatSession::new("incomplete-prefix".into());
        let mut thread = s.thread.clone();
        thread.frozen_request_prefix = Some(refact_chat_api::FrozenRequestPrefix {
            schema_version: 1,
            created_at: String::new(),
            system_prompt: None,
            tools_canonical: Some(serde_json::json!([])),
        });
        let error = payload_request_cap(&thread, 4096, 512, None).unwrap_err();
        assert!(
            error.contains("incomplete frozen request prefix"),
            "{error}"
        );
        thread.frozen_request_prefix = Some(refact_chat_api::FrozenRequestPrefix {
            schema_version: 1,
            created_at: String::new(),
            system_prompt: Some("system".into()),
            tools_canonical: None,
        });
        let error = payload_request_cap(&thread, 4096, 512, None).unwrap_err();
        assert!(
            error.contains("incomplete frozen request prefix"),
            "{error}"
        );
        assert!(!crate::chat::trajectories::frozen_prefix_is_complete(
            thread.frozen_request_prefix.as_ref().unwrap()
        ));
        thread.frozen_request_prefix = Some(refact_chat_api::FrozenRequestPrefix {
            schema_version: 1,
            created_at: String::new(),
            system_prompt: Some("system".into()),
            tools_canonical: Some(serde_json::json!({})),
        });
        let error = payload_request_cap(&thread, 4096, 512, None).unwrap_err();
        assert!(
            error.contains("incomplete frozen request prefix"),
            "{error}"
        );
        assert!(!budget_prefix_is_usable(
            thread.frozen_request_prefix.as_ref().unwrap()
        ));
        thread
            .frozen_request_prefix
            .as_mut()
            .unwrap()
            .tools_canonical = Some(serde_json::json!([]));
        assert!(budget_prefix_is_usable(
            thread.frozen_request_prefix.as_ref().unwrap()
        ));
        assert!(payload_request_cap(&thread, 4096, 512, None).is_ok());
    }

    #[test]
    fn fallback_estimate_counts_tokens_not_bytes() {
        let text = "x".repeat(300_000);
        let messages = vec![ChatMessage::new("user".into(), text.clone())];
        let estimate = estimated_tokens(&messages, None, false);
        let fallback_tokens = crate::tokens::count_text_tokens(None, &text).unwrap();
        assert!(
            estimate >= fallback_tokens,
            "{estimate} < {fallback_tokens}"
        );
        assert!(estimate < text.len(), "{estimate} counted bytes as tokens");
        assert!(!at_auto_cap(estimate, 500_000));
    }

    #[test]
    fn fresh_provider_usage_anchors_below_cap_context_over_inflated_local_estimate() {
        let system = ChatMessage::new("system".into(), "s".repeat(120_000));
        let user = ChatMessage::new("user".into(), "u".repeat(400_000));
        let mut answer = ChatMessage::new("assistant".into(), "answer".into());
        answer.usage = Some(ChatUsage {
            prompt_tokens: 1_592,
            completion_tokens: 542,
            total_tokens: 134_486,
            cache_read_tokens: Some(132_352),
            ..Default::default()
        });
        let follow_up = ChatMessage::new("user".into(), "continue".into());
        let messages = vec![system, user, answer, follow_up];
        let fresh = estimated_tokens(&messages, None, true);
        let stale = estimated_tokens(&messages, None, false);
        assert!(fresh >= 133_944, "{fresh}");
        assert!(fresh < stale, "fresh={fresh} stale={stale}");
        assert!(!at_auto_cap(fresh, 500_000));
        assert!(!at_auto_cap(fresh, 872_000));
    }

    #[test]
    fn exact_auto_boundary_and_no_savings_percentage_gate() {
        assert!(!at_auto_cap(850, 1000));
        assert!(!at_auto_cap(999, 1000));
        assert!(at_auto_cap(1000, 1000));
        assert!(at_auto_cap(1001, 1000));
        let source = vec![ChatMessage::new("user".into(), "a".repeat(1000))];
        let smaller = vec![ChatMessage::new("user".into(), "a".repeat(950))];
        assert!(validate_output(&source, &smaller, usize::MAX, None).is_ok());
        assert!(validate_output(&source, &source, usize::MAX, None).is_err());
        assert!(validate_output(&source, &smaller, 10, None).is_err());
        assert!(validate_output(&source, &[], usize::MAX, None).is_err());
    }

    #[test]
    fn fresh_usage_includes_assistant_and_later_growth_but_stale_does_not() {
        let mut answer = ChatMessage::new("assistant".into(), "answer".into());
        answer.usage = Some(ChatUsage {
            prompt_tokens: 10000,
            ..Default::default()
        });
        let input = vec![answer, ChatMessage::new("user".into(), "new input".into())];
        assert!(estimated_tokens(&input, None, true) > 10000);
        assert!(estimated_tokens(&input, None, false) < 10000);
    }

    #[tokio::test]
    async fn success_appends_report_after_persist_and_repeat_uses_active_only() {
        let (mut s, thread, version, report) = fixture();
        let archived = serde_json::to_value(&s.messages).unwrap();
        commit_report(
            &mut s,
            report,
            version,
            1,
            &thread,
            &[],
            &AtomicBool::new(false),
            |snapshot| async move {
                assert_eq!(snapshot.version, version + 1);
                assert_eq!(snapshot.messages.len(), 2);
                assert!(snapshot.previous_response_id.is_none());
                assert!(snapshot.frozen_request_prefix.is_none());
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(serde_json::to_value(&s.messages[..1]).unwrap(), archived);
        assert_eq!(s.trajectory_committed_version, s.trajectory_version);
        assert!(s.provider_usage_stale);
        let active = active_context(&s.messages).unwrap().messages;
        assert_eq!(active.len(), 1);
        assert!(!serde_json::to_string(&active)
            .unwrap()
            .contains("ARCHIVED_SENTINEL"));
        s.add_message(ChatMessage::new("user".into(), "tail".into()));
        let second_input = active_context(&s.messages).unwrap().messages;
        assert_eq!(second_input.len(), 2);
        let report =
            make_reconstruction_report(second_input, ReconstructionMetadata::default()).unwrap();
        let version = s.trajectory_version;
        commit_report(
            &mut s,
            report,
            version,
            1,
            &thread,
            &[],
            &AtomicBool::new(false),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
        assert_eq!(s.messages.len(), 4);
        assert_eq!(active_context(&s.messages).unwrap().messages.len(), 2);
    }

    #[tokio::test]
    async fn failed_persistence_never_publishes_report() {
        let (mut s, thread, version, report) = fixture();
        let before = serde_json::to_value(&s.messages).unwrap();
        assert!(commit_report(
            &mut s,
            report,
            version,
            1,
            &thread,
            &[],
            &AtomicBool::new(false),
            |_| async { Err("disk failed".into()) }
        )
        .await
        .is_err());
        assert_eq!(serde_json::to_value(&s.messages).unwrap(), before);
        assert_eq!(s.trajectory_version, version);
        let session = Arc::new(Mutex::new(s));
        drop(AttemptGuard {
            session: session.clone(),
            attempt: 1,
            abort: Arc::new(AtomicBool::new(false)),
        });
        assert!(!compression_attempt_active(&*session.lock().await));
    }

    #[tokio::test]
    async fn canceled_version_settings_and_delivery_races_never_persist() {
        for race in 0..5 {
            let (mut s, thread, version, report) = fixture();
            let abort = AtomicBool::new(false);
            match race {
                0 => abort.store(true, Ordering::SeqCst),
                1 => s.add_message(ChatMessage::new("user".into(), "race".into())),
                2 => s.thread.model = "changed".into(),
                3 => s.pending_deliveries.push_back(
                    refact_core::chat_types::PendingDelivery::with_id(
                        "new-delivery",
                        vec![ChatMessage::new(
                            "user".into(),
                            "accepted but not model input".into(),
                        )],
                        refact_core::chat_types::PushMode::WhenIdle,
                        "test",
                        false,
                    ),
                ),
                _ => s.active_compression_attempt = Some(2),
            }
            let before = serde_json::to_value(&s.messages).unwrap();
            assert!(commit_report(
                &mut s,
                report,
                version,
                1,
                &thread,
                &[],
                &abort,
                |_| async { panic!("CAS must precede persistence") }
            )
            .await
            .is_err());
            assert_eq!(serde_json::to_value(&s.messages).unwrap(), before);
        }
    }

    #[tokio::test]
    async fn dropped_attempt_releases_after_contended_lock_without_clearing_new_owner() {
        let (s, _, _, _) = fixture();
        let session = Arc::new(Mutex::new(s));
        let abort = Arc::new(AtomicBool::new(false));
        let lock = session.lock().await;
        drop(AttemptGuard {
            session: session.clone(),
            attempt: 1,
            abort: abort.clone(),
        });
        assert!(abort.load(Ordering::SeqCst));
        drop(lock);
        tokio::task::yield_now().await;
        assert!(!compression_attempt_active(&*session.lock().await));
        session.lock().await.active_compression_attempt = Some(2);
        drop(AttemptGuard {
            session: session.clone(),
            attempt: 1,
            abort,
        });
        assert_eq!(session.lock().await.active_compression_attempt, Some(2));
    }

    #[test]
    fn overflow_finishes_stream_before_idle_and_preserves_text() {
        let mut s = ChatSession::new("overflow-test".into());
        let mut events = s.event_tx.subscribe();
        s.start_stream().unwrap();
        s.draft_message.as_mut().unwrap().content =
            crate::call_validation::ChatContent::SimpleText("accepted output".into());
        s.finish_stream_for_rebuild();
        assert!(idle(&s));
        assert_eq!(
            s.messages.last().unwrap().content.content_text_only(),
            "accepted output"
        );
        let mut recorded = Vec::new();
        while let Ok(event) = events.try_recv() {
            recorded.push(event.to_string());
        }
        assert!(recorded
            .iter()
            .any(|event| event.contains("stream_finished")));
    }

    #[test]
    fn pending_request_is_canceled_by_abort() {
        let mut s = ChatSession::new("abort-test".into());
        crate::chat::context_rebuild::request_rebuild(
            &mut s,
            PendingContextRebuild {
                model: None,
                trigger: "tool".into(),
            },
        )
        .unwrap();
        s.abort_stream();
        assert!(s.pending_context_rebuild.is_none());
        assert!(!idle(&s));
    }

    #[tokio::test]
    async fn stale_manual_version_never_starts_builder() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut s = ChatSession::new("stale-test".into());
        s.add_message(ChatMessage::new("user".into(), "keep unchanged".into()));
        let before = serde_json::to_value(&s.messages).unwrap();
        let session = Arc::new(Mutex::new(s));
        let result = rebuild_session(
            gcx,
            &session,
            PendingContextRebuild {
                model: None,
                trigger: "test".into(),
            },
            Some(999),
            true,
        )
        .await;
        assert!(result.unwrap_err().contains("changed since preview"));
        let s = session.lock().await;
        assert_eq!(serde_json::to_value(&s.messages).unwrap(), before);
        assert!(!compression_attempt_active(&s));
    }

    #[tokio::test]
    async fn direct_rebuild_rejects_pending_owners_before_model_resolution() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        for handoff in [false, true] {
            let mut s = ChatSession::new("reserved-owner".into());
            s.add_message(ChatMessage::new("user".into(), "keep".into()));
            s.runtime.state = SessionState::Completed;
            s.abort_flag.store(true, Ordering::SeqCst);
            let request = PendingContextRebuild {
                model: None,
                trigger: "manual".into(),
            };
            if handoff {
                s.pending_mode_handoff = Some(serde_json::json!({"target_mode":"agent"}));
            } else {
                s.pending_context_rebuild = Some(request.clone());
            }
            let before = serde_json::to_value(&s.messages).unwrap();
            let session = Arc::new(Mutex::new(s));
            let error = rebuild_session(gcx.clone(), &session, request, None, true)
                .await
                .unwrap_err();
            assert!(error.contains("operation is pending"));
            let s = session.lock().await;
            assert_eq!(serde_json::to_value(&s.messages).unwrap(), before);
            assert_eq!(s.runtime.state, SessionState::Completed);
            assert!(s.abort_flag.load(Ordering::SeqCst));
            assert!(s.active_compression_attempt.is_none());
        }
    }

    #[tokio::test]
    async fn disabled_automatic_compression_blocks_forced_rebuild_without_model_call() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut s = ChatSession::new("disabled-auto".into());
        s.thread.auto_compact_enabled = Some(false);
        s.add_message(ChatMessage::new("user".into(), "keep".into()));
        let thread = s.thread.clone();
        let before = serde_json::to_value(&s.messages).unwrap();
        let session = Arc::new(Mutex::new(s));
        for force in [false, true] {
            let outcome =
                apply_context_rebuild_with_reason(gcx.clone(), &session, &thread, force, None)
                    .await;
            assert_eq!(outcome, CompactionOutcome::NothingToCompact);
        }
        let s = session.lock().await;
        assert_eq!(serde_json::to_value(&s.messages).unwrap(), before);
        assert!(s.last_rebuild_attempt_version.is_none());
    }

    #[tokio::test]
    async fn deterministic_recovery_sweeps_high_context_without_model_call() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut s = ChatSession::new("det-recovery".into());
        s.add_message(ChatMessage::new("system".into(), "system prompt".into()));
        s.add_message(ChatMessage::new("user".into(), "big conversation ".repeat(2000)));
        let before = s.messages.len();
        let before_tokens = estimated_tokens(&s.messages, None, false);
        let session = Arc::new(Mutex::new(s));
        let applied =
            apply_deterministic_compaction_for_recovery(gcx, &session).await;
        assert!(applied, "deterministic sweep must apply");
        let s = &*session.lock().await;
        assert!(s.messages.len() >= before, "raw history stays append-only");
        let active = active_context(&s.messages).unwrap().messages;
        let after_tokens = estimated_tokens(&active, None, false);
        assert!(
            after_tokens < before_tokens,
            "active context must shrink: before={before_tokens} after={after_tokens}"
        );
        assert!(
            active.iter().any(|m| m.role == "compression_report"),
            "sweep must insert a compression_report"
        );
        assert!(s.thread.previous_response_id.is_none());
        assert!(!compression_attempt_active(s));
        assert_eq!(s.compression_phase, Some(CompressionPhase::Applied));
    }

    #[tokio::test]
    async fn deterministic_recovery_is_noop_when_nothing_is_compressible() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut s = ChatSession::new("det-noop".into());
        s.add_message(ChatMessage::new("user".into(), "tiny".into()));
        let session = Arc::new(Mutex::new(s));
        let applied =
            apply_deterministic_compaction_for_recovery(gcx, &session).await;
        assert!(!applied, "tiny chat has nothing to sweep");
        let s = &*session.lock().await;
        assert_eq!(s.messages.len(), 1);
        assert!(s.compression_phase.is_none());
    }

    #[tokio::test]
    async fn deterministic_recovery_runs_after_failed_rebuild_leaves_error_state() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let mut s = ChatSession::new("det-error-state".into());
        s.add_message(ChatMessage::new("system".into(), "system prompt".into()));
        s.add_message(ChatMessage::new("user".into(), "big conversation ".repeat(2000)));
        s.set_runtime_state(SessionState::Error, Some("Context rebuild failed".into()));
        let session = Arc::new(Mutex::new(s));
        let applied = apply_deterministic_compaction_for_recovery(gcx, &session).await;
        assert!(
            applied,
            "an Error state left by a failed model rebuild must not block the deterministic fallback"
        );
    }
}
