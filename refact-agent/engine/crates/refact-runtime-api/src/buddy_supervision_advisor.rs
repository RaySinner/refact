use serde_json::json;
use uuid::Uuid;

use refact_buddy_core::settings::{BuddySettings, SupervisionMode};
use refact_buddy_core::supervision::{
    InterventionCandidate, InterventionDelivery, InterventionRecord, InterventionVerdict,
    SupervisionPolicyKey, SuppressionReason,
};
use refact_buddy_core::supervision_journal::SupervisionJournalBegin;
use refact_buddy_core::supervision_policy::{
    delivery_budget_allows, format_advice, off_goal_needs_confirmation, validate_arbiter_reply,
    SupervisionArbiterReply,
};
use refact_chat_api::delivery::deliver_to_chat;
use refact_chat_api::internal_roles::{event, EventSubkind};
use refact_chat_api::pursuit::{
    arbitrate_pursuit, pursuit_evidence_seq, stamp_pursuit_message, PursuitArbitrationContext,
    PursuitDecision, PursuitRequest, PursuitSource, PursuitSuppressReason,
};
use refact_chat_api::{ChatMessage, PendingDelivery, PushMode, SubchatParameters, ToolsPolicy};
use refact_core::chat_types::{CacheControl, ChatModelType};

use crate::app_state::AppState;
use crate::buddy_supervision::epoch_secs;

pub const BUDDY_SUPERVISION_SOURCE: &str = "buddy.supervision";
const MAX_NEW_TOKENS: usize = 300;
const SUPERVISION_ARBITER_PROMPT: &str = "You are Buddy's automated supervisor. Review only the supplied redacted supervision projection and candidate. Return exactly one JSON object with keys verdict, evidence_event_ids, confidence, suggested_action. verdict is confirmed, dismissed, or inconclusive. evidence_event_ids is a non-empty array of {chat_id,seq} objects copied only from the candidate evidence. confidence is an integer from 0 to 100. suggested_action is one short concrete next step. Confirm only when the evidence supports that trigger and the action is useful. Otherwise dismiss or return inconclusive. Never include reasoning, tool arguments, tool results, secrets, markdown, or extra keys. Do not propose edits, approvals, goal changes, provider retries, or process restarts.";
const MISSING_GOAL_ARBITER_PROMPT: &str = " For this missing_goal candidate only, a confirmed suggested_action must explicitly recommend calling set_goal with a short concrete goal inferred from the recent user request and substantive tool. This is advice only: never mutate goal state, tell the model to stop or abort, or request preemption or tool denial. This trigger-specific rule overrides the general goal-change prohibition above.";

pub async fn run_supervision_advisor(app: AppState) {
    let mut receiver = app.buddy.supervision.subscribe();
    loop {
        tokio::select! {
            received = receiver.recv() => {
                match received {
                    Ok(candidate) => process_candidate(&app, candidate).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                if app.chat_ctx().runtime.shutdown_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
            }
        }
    }
}

async fn finish(
    app: &AppState,
    key: SupervisionPolicyKey,
    mut record: InterventionRecord,
    verdict: InterventionVerdict,
    delivery: InterventionDelivery,
) {
    record.policy_key = Some(key.clone());
    record.verdict = verdict;
    record.delivery = delivery;
    record.decided_at = epoch_secs();
    if let Err(error) = app
        .buddy
        .supervision
        .journal()
        .finalize_candidate(&key, record)
        .await
    {
        tracing::warn!("buddy supervision could not finalize candidate: {error}");
    }
}

async fn process_candidate(app: &AppState, candidate: InterventionRecord) {
    let settings = {
        let buddy = app.buddy.buddy.lock().await;
        buddy.as_ref().map(|service| service.settings.clone())
    };
    let Some(settings) = settings else {
        return;
    };
    if settings.supervision_mode != SupervisionMode::Advise {
        return;
    }
    let Some(chat_id) = candidate
        .evidence
        .ids()
        .first()
        .map(|id| id.chat_id.as_str().to_string())
    else {
        return;
    };
    let Some(projection) = app.buddy.supervision.projection(&chat_id).await else {
        return;
    };
    let goal_revision = projection
        .detection_events()
        .filter(|event| {
            candidate.evidence.ids().contains(&event.evidence_id())
                && matches!(
                    &event.data,
                    refact_buddy_core::supervision::SupervisionKind::Goal { .. }
                )
        })
        .filter_map(|event| match &event.data {
            refact_buddy_core::supervision::SupervisionKind::Goal { facts: Some(facts) } => {
                Some(facts.revision)
            }
            _ => None,
        })
        .last()
        .or_else(|| projection.goal().map(|goal| goal.revision))
        .unwrap_or(0);
    let key = SupervisionPolicyKey::new(&chat_id, goal_revision, &candidate);
    match app
        .buddy
        .supervision
        .journal()
        .begin_candidate(&candidate, key.clone())
        .await
    {
        Ok(SupervisionJournalBegin::Reserved) => {}
        Ok(SupervisionJournalBegin::Duplicate) => return,
        Err(error) => {
            tracing::warn!("buddy supervision could not reserve candidate: {error}");
            return;
        }
    }
    let snapshot = match app.buddy.supervision.journal().load().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!("buddy supervision could not read policy history: {error}");
            return;
        }
    };
    let now = epoch_secs();
    if !delivery_budget_allows(
        snapshot.records(),
        &chat_id,
        now,
        settings.stalled_turn_min_interval_secs,
        settings.stalled_turn_max_per_hour,
    ) {
        finish(
            app,
            key,
            candidate.clone(),
            candidate.verdict,
            InterventionDelivery::Suppressed {
                reason: SuppressionReason::Cooldown,
            },
        )
        .await;
        return;
    }
    let reply = match ask_buddy_arbiter(app, &projection, &candidate).await {
        Ok(reply) => reply,
        Err(_) => {
            tracing::warn!("buddy supervision arbiter failed; advice suppressed");
            finish(
                app,
                key,
                candidate,
                InterventionVerdict::Inconclusive,
                InterventionDelivery::Suppressed {
                    reason: SuppressionReason::ArbiterFailed,
                },
            )
            .await;
            return;
        }
    };
    let advice = match validate_arbiter_reply(
        &candidate,
        &projection,
        reply,
        settings.supervision_min_confidence.min(100),
    ) {
        Ok(advice) => advice,
        Err(_) => {
            finish(
                app,
                key,
                candidate,
                InterventionVerdict::Inconclusive,
                InterventionDelivery::Suppressed {
                    reason: SuppressionReason::NotConfirmed,
                },
            )
            .await;
            return;
        }
    };
    if advice.verdict != InterventionVerdict::Confirmed {
        finish(
            app,
            key,
            candidate,
            advice.verdict,
            InterventionDelivery::Suppressed {
                reason: SuppressionReason::NotConfirmed,
            },
        )
        .await;
        return;
    }
    let mut reserved = candidate.clone();
    reserved.policy_key = Some(key.clone());
    if off_goal_needs_confirmation(snapshot.records(), &reserved) {
        finish(
            app,
            key,
            candidate,
            InterventionVerdict::Confirmed,
            InterventionDelivery::Suppressed {
                reason: SuppressionReason::NeedsConfirmation,
            },
        )
        .await;
        return;
    }
    let Some(session_arc) = app.chat.sessions.read().await.get(&chat_id).cloned() else {
        finish(
            app,
            key,
            candidate,
            InterventionVerdict::Confirmed,
            InterventionDelivery::Suppressed {
                reason: SuppressionReason::UnsafeBoundary,
            },
        )
        .await;
        return;
    };
    let text = format_advice(&candidate, &advice);
    let delivery = if candidate.candidate == InterventionCandidate::MissingGoal {
        let stale = {
            let session = session_arc.lock().await;
            missing_goal_is_stale(&session)
        };
        if stale {
            finish(
                app,
                key,
                candidate,
                InterventionVerdict::Confirmed,
                InterventionDelivery::Suppressed {
                    reason: SuppressionReason::StaleEvidence,
                },
            )
            .await;
            return;
        }
        missing_goal_delivery(
            &key,
            &candidate,
            &advice.evidence_event_ids,
            advice.confidence,
            text,
        )
    } else {
        let (decision, request, session_goal_revision) = {
            let session = session_arc.lock().await;
            let request = PursuitRequest::new(
                PursuitSource::Buddy,
                format!("supervision:{:?}", candidate.candidate),
                pursuit_evidence_seq(&session),
                candidate.candidate != InterventionCandidate::ProviderDisconnect,
            );
            let decision = arbitrate_pursuit(
                &session,
                &request,
                PursuitArbitrationContext::new(now.saturating_mul(1000)),
            );
            let revision = session.goal.as_ref().map(|goal| goal.revision).unwrap_or(0);
            (decision, request, revision)
        };
        if revision_mismatch(goal_revision, session_goal_revision) {
            finish(
                app,
                key,
                candidate,
                InterventionVerdict::Confirmed,
                InterventionDelivery::Suppressed {
                    reason: SuppressionReason::StaleEvidence,
                },
            )
            .await;
            return;
        }
        let wake = match decision {
            PursuitDecision::StartTurn => true,
            PursuitDecision::VisibleOnly => false,
            PursuitDecision::Suppress(reason) => {
                let suppression = match reason {
                    PursuitSuppressReason::DuplicateEvidence => SuppressionReason::Duplicate,
                    PursuitSuppressReason::AutonomyOff => SuppressionReason::AutonomyOff,
                    _ => SuppressionReason::UnsafeBoundary,
                };
                finish(
                    app,
                    key,
                    candidate,
                    InterventionVerdict::Confirmed,
                    InterventionDelivery::Suppressed {
                        reason: suppression,
                    },
                )
                .await;
                return;
            }
        };
        supervision_delivery(
            &key,
            &candidate,
            &advice.evidence_event_ids,
            advice.confidence,
            text,
            wake,
            &request,
            session_goal_revision,
        )
    };
    let delivery_reference = delivery.id.clone();
    let message_id = delivery
        .messages
        .first()
        .map(|message| message.message_id.clone());
    match deliver_to_chat(app.clone(), &chat_id, delivery).await {
        Ok(_) => {
            crate::buddy_supervision::collect_intervention(
                app,
                &chat_id,
                &delivery_reference,
                message_id.as_deref(),
            )
            .await;
            finish(
                app,
                key,
                candidate,
                InterventionVerdict::Confirmed,
                InterventionDelivery::Sent {
                    delivery_id: refact_buddy_core::supervision::SupervisionReference::new(
                        &delivery_reference,
                    ),
                    message_id: refact_buddy_core::supervision::SupervisionReference::new(
                        message_id.as_deref().unwrap_or_default(),
                    ),
                    sent_at: now,
                },
            )
            .await;
        }
        Err(_) => {
            finish(
                app,
                key,
                candidate,
                InterventionVerdict::Confirmed,
                InterventionDelivery::Suppressed {
                    reason: SuppressionReason::DeliveryFailed,
                },
            )
            .await;
        }
    }
}

fn revision_mismatch(candidate_revision: u64, session_revision: u64) -> bool {
    candidate_revision != 0 && candidate_revision != session_revision
}

fn missing_goal_is_stale(session: &refact_chat_api::ChatSession) -> bool {
    session.goal.is_some()
}

pub fn missing_goal_delivery(
    key: &SupervisionPolicyKey,
    candidate: &InterventionRecord,
    evidence_event_ids: &[refact_buddy_core::supervision::EvidenceId],
    confidence: u8,
    text: String,
) -> PendingDelivery {
    let notice = event(
        EventSubkind::SystemNotice,
        BUDDY_SUPERVISION_SOURCE,
        json!({
            "kind": "buddy_supervision_advice",
            "trigger": candidate.candidate,
            "evidence_event_ids": evidence_event_ids,
            "confidence": confidence,
            "attribution": "Buddy automated supervisor",
        }),
        text,
    );
    PendingDelivery::with_id(
        key.stable_id().as_str().to_string(),
        vec![notice],
        PushMode::Append,
        BUDDY_SUPERVISION_SOURCE,
        false,
    )
}

pub fn supervision_delivery(
    key: &SupervisionPolicyKey,
    candidate: &InterventionRecord,
    evidence_event_ids: &[refact_buddy_core::supervision::EvidenceId],
    confidence: u8,
    text: String,
    wake: bool,
    request: &PursuitRequest,
    goal_revision: u64,
) -> PendingDelivery {
    let mut notice = event(
        EventSubkind::SystemNotice,
        BUDDY_SUPERVISION_SOURCE,
        json!({
            "kind": "buddy_supervision_advice",
            "trigger": candidate.candidate,
            "evidence_event_ids": evidence_event_ids,
            "confidence": confidence,
            "attribution": "Buddy automated supervisor",
        }),
        text,
    );
    stamp_pursuit_message(&mut notice, request, goal_revision);
    PendingDelivery::with_id(
        key.stable_id().as_str().to_string(),
        vec![notice],
        if wake {
            PushMode::Append
        } else {
            PushMode::WhenIdle
        },
        BUDDY_SUPERVISION_SOURCE,
        wake,
    )
}

async fn ask_buddy_arbiter(
    app: &AppState,
    projection: &refact_buddy_core::supervision::SupervisionProjection,
    candidate: &InterventionRecord,
) -> Result<SupervisionArbiterReply, String> {
    let mut config = app
        .chat_ctx()
        .subchat
        .resolve_subchat_config(
            "follow_up",
            false,
            Some(format!("buddy-supervision-{}", Uuid::new_v4())),
            Some("Buddy Supervision".to_string()),
            Some(vec![]),
            1,
            false,
            "buddy".to_string(),
        )
        .await
        .map_err(|error| format!("subchat config: {error}"))?;
    let params = SubchatParameters {
        subchat_model_type: ChatModelType::Buddy,
        subchat_model: String::new(),
        subchat_n_ctx: config.n_ctx,
        subchat_max_new_tokens: MAX_NEW_TOKENS,
        subchat_temperature: Some(0.0),
        subchat_tokens_for_rag: 0,
        subchat_reasoning_effort: None,
        subchat_cache_control: CacheControl::Ephemeral,
    };
    let model = crate::subchat_runtime::resolve_subchat_model(&app.chat_ctx(), &params)
        .await
        .map_err(|error| format!("buddy model: {error}"))?;
    config.tool_name = "buddy_supervision_arbiter".to_string();
    config.tools = ToolsPolicy::None;
    config.max_steps = 1;
    config.model = model;
    config.max_new_tokens = MAX_NEW_TOKENS;
    config.temperature = Some(0.0);
    config.buddy_meta = Some(refact_core::buddy_meta::BuddyThreadMeta {
        is_buddy_chat: true,
        buddy_chat_kind: "system".to_string(),
        workflow_id: Some("buddy_supervision_arbiter".to_string()),
    });
    let input = json!({
        "projection": serde_json::from_str::<serde_json::Value>(&projection.snapshot().map_err(|error| error.to_string())?).map_err(|error| error.to_string())?,
        "candidate": candidate,
    });
    let messages = vec![
        ChatMessage::new("system".to_string(), supervision_arbiter_prompt(candidate)),
        ChatMessage::new("user".to_string(), input.to_string()),
    ];
    let result = app
        .chat_ctx()
        .subchat
        .run_subchat(messages, config)
        .await
        .map_err(|error| format!("subchat: {error}"))?;
    let reply = result
        .messages
        .last()
        .filter(|message| message.role == "assistant")
        .map(|message| message.content.content_text_only())
        .ok_or_else(|| "buddy returned no assistant message".to_string())?;
    serde_json::from_str(reply.trim()).map_err(|error| format!("invalid arbiter JSON: {error}"))
}

fn supervision_arbiter_prompt(candidate: &InterventionRecord) -> String {
    let mut prompt = SUPERVISION_ARBITER_PROMPT.to_string();
    if candidate.candidate == InterventionCandidate::MissingGoal {
        prompt.push_str(MISSING_GOAL_ARBITER_PROMPT);
    }
    prompt
}

pub fn stalled_turn_enabled(settings: &BuddySettings) -> bool {
    settings.stalled_turn_intervention && settings.supervision_mode == SupervisionMode::Advise
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_buddy_core::supervision::{
        EvidenceId, GoalFacts, MessageBody, MessageRole, SupervisionEvidence, SupervisionEvent,
        SupervisionHash, SupervisionKind, SupervisionReference, SupervisionVersion,
    };
    use refact_chat_api::{ChatSession, GoalAutonomy, GoalBudget};

    fn candidate() -> InterventionRecord {
        let event = SupervisionEvent::new(
            7,
            "chat-1",
            100,
            SupervisionKind::Message {
                role: MessageRole::Assistant,
                message_id: SupervisionReference::new("m7"),
                body: Some(MessageBody::new("safe")),
            },
        );
        InterventionRecord {
            schema_version: SupervisionVersion,
            candidate: InterventionCandidate::Loop,
            evidence: SupervisionEvidence::from_events(&[event]).unwrap(),
            detector_version: SupervisionReference::new("test.v1"),
            policy_key: None,
            verdict: InterventionVerdict::Confirmed,
            delivery: InterventionDelivery::Suppressed {
                reason: SuppressionReason::ShadowMode,
            },
            detected_at: 100,
            decided_at: 100,
        }
    }

    #[test]
    fn visible_delivery_is_attributed_stable_and_never_a_user_message() {
        let candidate = candidate();
        let key = SupervisionPolicyKey::new("chat-1", 3, &candidate);
        let request = PursuitRequest::new(PursuitSource::Buddy, "loop", 8, false);
        let evidence = vec![EvidenceId {
            chat_id: SupervisionReference::new("chat-1"),
            seq: 7,
        }];
        let first = supervision_delivery(
            &key,
            &candidate,
            &evidence,
            90,
            "Buddy supervision: loop. Next step: change the call.".to_string(),
            false,
            &request,
            3,
        );
        let second = supervision_delivery(
            &key,
            &candidate,
            &evidence,
            90,
            "Buddy supervision: loop. Next step: change the call.".to_string(),
            false,
            &request,
            3,
        );
        assert_eq!(first.id, second.id);
        assert!(!first.wake);
        assert_eq!(first.push, PushMode::WhenIdle);
        assert_eq!(first.messages[0].role, "event");
        assert_eq!(first.messages[0].extra["event"]["subkind"], "systemNotice");
        assert_eq!(first.messages[0].extra["pursuit"]["wake_requested"], false);
        assert_eq!(
            first.messages[0].content.content_text_only(),
            second.messages[0].content.content_text_only()
        );
    }

    #[test]
    fn missing_goal_delivery_appends_without_wake_and_suppresses_stale_goals() {
        let mut candidate = candidate();
        candidate.candidate = InterventionCandidate::MissingGoal;
        let key = SupervisionPolicyKey::new("chat-1", 0, &candidate);
        let evidence = candidate.evidence.ids().to_vec();
        let session = ChatSession::new("chat-1".to_string());
        let request = PursuitRequest::new(PursuitSource::Buddy, "missing_goal", 8, false);
        assert_eq!(
            arbitrate_pursuit(&session, &request, PursuitArbitrationContext::new(1)),
            PursuitDecision::Suppress(PursuitSuppressReason::NoGoal)
        );
        assert!(!missing_goal_is_stale(&session));

        let first = missing_goal_delivery(
            &key,
            &candidate,
            &evidence,
            90,
            "Buddy supervision: work without a goal. Next step: call set_goal.".to_string(),
        );
        let second = missing_goal_delivery(
            &key,
            &candidate,
            &evidence,
            90,
            "Buddy supervision: work without a goal. Next step: call set_goal.".to_string(),
        );
        assert_eq!(first.id, second.id);
        assert_eq!(first.push, PushMode::Append);
        assert!(!first.wake);
        assert_eq!(first.source, BUDDY_SUPERVISION_SOURCE);
        assert_eq!(first.messages[0].role, "event");
        assert_eq!(first.messages[0].extra["event"]["subkind"], "systemNotice");
        assert_eq!(
            first.messages[0].extra["event"]["source"],
            BUDDY_SUPERVISION_SOURCE
        );
        assert!(first.messages[0].extra.get("pursuit").is_none());

        let mut installed = session;
        installed.install_goal("agent", "finish", true, GoalBudget::default());
        assert!(missing_goal_is_stale(&installed));
        installed.goal.as_mut().unwrap().active = false;
        assert!(missing_goal_is_stale(&installed));
    }

    #[test]
    fn missing_goal_arbiter_prompt_is_trigger_specific() {
        let mut missing = candidate();
        missing.candidate = InterventionCandidate::MissingGoal;
        let prompt = supervision_arbiter_prompt(&missing);
        assert!(prompt.starts_with(SUPERVISION_ARBITER_PROMPT));
        assert!(prompt.contains("must explicitly recommend calling set_goal"));
        assert!(prompt.contains("never mutate goal state"));

        let off_goal = candidate();
        assert_eq!(
            supervision_arbiter_prompt(&off_goal),
            SUPERVISION_ARBITER_PROMPT
        );
    }

    #[test]
    fn autonomy_and_provider_never_start_an_unsafe_retry() {
        let mut session = ChatSession::new("chat-1".to_string());
        session.install_goal("agent", "finish", true, GoalBudget::default());
        session.goal.as_mut().unwrap().autonomy = GoalAutonomy::Off;
        let request = PursuitRequest::new(PursuitSource::Buddy, "loop", 1, true);
        assert_eq!(
            arbitrate_pursuit(&session, &request, PursuitArbitrationContext::new(1)),
            PursuitDecision::VisibleOnly
        );
        let provider = PursuitRequest::new(PursuitSource::Buddy, "provider", 1, false);
        assert_eq!(
            arbitrate_pursuit(&session, &provider, PursuitArbitrationContext::new(1)),
            PursuitDecision::VisibleOnly
        );
    }

    #[test]
    fn policy_key_changes_with_goal_trigger_and_evidence() {
        let first = candidate();
        let key = SupervisionPolicyKey::new("chat-1", 3, &first);
        let mut other = first.clone();
        other.candidate = InterventionCandidate::Stagnation;
        assert_ne!(
            key.stable_id(),
            SupervisionPolicyKey::new("chat-1", 4, &first).stable_id()
        );
        assert_ne!(
            key.stable_id(),
            SupervisionPolicyKey::new("chat-1", 3, &other).stable_id()
        );
        assert_ne!(
            key.evidence_hash,
            SupervisionHash::normalized_arguments(&json!("other"))
        );
        let _ = GoalFacts {
            revision: 3,
            active: true,
            autonomy: true,
            prompt_enabled: true,
            can_pursue: true,
        };
    }

    #[test]
    fn shadow_and_off_disable_legacy_stalled_turn_delivery() {
        let mut settings = BuddySettings::default();
        assert!(stalled_turn_enabled(&settings));
        settings.supervision_mode = SupervisionMode::Shadow;
        assert!(!stalled_turn_enabled(&settings));
        settings.supervision_mode = SupervisionMode::Off;
        assert!(!stalled_turn_enabled(&settings));
    }
}
