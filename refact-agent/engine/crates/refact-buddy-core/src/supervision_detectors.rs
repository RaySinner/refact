use std::collections::BTreeMap;

use crate::supervision::{
    DiffStatus, ErrorCategory, ErrorFingerprint, InterventionCandidate, InterventionDelivery,
    InterventionRecord, InterventionVerdict, MessageRole, ProviderState, ResultStatus, StreamState,
    SupervisionEvidence, SupervisionEvent, SupervisionHash, SupervisionKind, SupervisionProjection,
    SupervisionReference, SupervisionVersion, SuppressionReason, TestStatus,
};

pub const DEFAULT_LOOP_THRESHOLD: usize = 3;
pub const DEFAULT_STAGNATION_AFTER_SECS: u64 = 300;
const OFF_GOAL_EVIDENCE_EVENTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisionDetectorConfig {
    pub loop_threshold: usize,
    pub stagnation_after_secs: u64,
}

impl Default for SupervisionDetectorConfig {
    fn default() -> Self {
        Self {
            loop_threshold: DEFAULT_LOOP_THRESHOLD,
            stagnation_after_secs: DEFAULT_STAGNATION_AFTER_SECS,
        }
    }
}

#[derive(Clone)]
struct PendingCall {
    args_hash: SupervisionHash,
    event: SupervisionEvent,
}

#[derive(Clone)]
struct CompletedCall {
    tool_name: SupervisionReference,
    args_hash: SupervisionHash,
    result_hash: SupervisionHash,
    status: ResultStatus,
    error: Option<ErrorFingerprint>,
    events: [SupervisionEvent; 2],
}

impl PartialEq for CompletedCall {
    fn eq(&self, other: &Self) -> bool {
        self.tool_name == other.tool_name
            && self.args_hash == other.args_hash
            && self.result_hash == other.result_hash
            && self.status == other.status
            && self.error == other.error
    }
}

fn shadow_record(
    candidate: InterventionCandidate,
    verdict: InterventionVerdict,
    detector_version: &str,
    evidence_events: &[SupervisionEvent],
    now: u64,
) -> Option<InterventionRecord> {
    Some(InterventionRecord {
        schema_version: SupervisionVersion,
        candidate,
        evidence: SupervisionEvidence::from_events(evidence_events).ok()?,
        detector_version: SupervisionReference::new(detector_version),
        verdict,
        delivery: InterventionDelivery::Suppressed {
            reason: SuppressionReason::ShadowMode,
        },
        detected_at: now,
        decided_at: now,
    })
}

fn detect_loop(
    projection: &SupervisionProjection,
    config: SupervisionDetectorConfig,
    now: u64,
) -> Option<InterventionRecord> {
    if config.loop_threshold < 2 {
        return None;
    }
    let mut pending = BTreeMap::<SupervisionReference, PendingCall>::new();
    let mut chain = Vec::<CompletedCall>::new();
    for event in projection.detection_events() {
        match &event.data {
            SupervisionKind::Message {
                role: MessageRole::User,
                ..
            }
            | SupervisionKind::Diff {
                status: DiffStatus::Changed,
            } => {
                pending.clear();
                chain.clear();
            }
            SupervisionKind::ToolCall {
                tool_name,
                normalized_args_hash,
            } => {
                pending.insert(
                    tool_name.clone(),
                    PendingCall {
                        args_hash: normalized_args_hash.clone(),
                        event: event.clone(),
                    },
                );
            }
            SupervisionKind::ToolResult {
                tool_name,
                result_hash,
                status,
                error,
            } if matches!(
                status,
                ResultStatus::Succeeded | ResultStatus::Failed | ResultStatus::Cancelled
            ) =>
            {
                let Some(call) = pending.remove(tool_name) else {
                    chain.clear();
                    continue;
                };
                let completed = CompletedCall {
                    tool_name: tool_name.clone(),
                    args_hash: call.args_hash,
                    result_hash: result_hash.clone(),
                    status: *status,
                    error: error.clone(),
                    events: [call.event, event.clone()],
                };
                if chain.last().is_some_and(|previous| previous != &completed) {
                    chain.clear();
                }
                chain.push(completed);
            }
            _ => {}
        }
    }
    if chain.len() < config.loop_threshold {
        return None;
    }
    let evidence_len = config
        .loop_threshold
        .min(crate::supervision::MAX_EVIDENCE_IDS / 2);
    let evidence = chain
        .iter()
        .skip(chain.len() - evidence_len)
        .flat_map(|completed| completed.events.clone())
        .collect::<Vec<_>>();
    shadow_record(
        InterventionCandidate::Loop,
        InterventionVerdict::Confirmed,
        "bd2.loop.v1",
        &evidence,
        now,
    )
}

fn meaningful_event(event: &SupervisionEvent) -> bool {
    match &event.data {
        SupervisionKind::Message { body, .. } => body.is_some(),
        SupervisionKind::ToolResult { status, .. } => matches!(
            status,
            ResultStatus::Succeeded | ResultStatus::Failed | ResultStatus::Cancelled
        ),
        SupervisionKind::Diff {
            status: DiffStatus::Changed,
        } => true,
        SupervisionKind::Test { status } => matches!(
            status,
            TestStatus::Passed | TestStatus::Failed | TestStatus::Cancelled
        ),
        SupervisionKind::Goal { .. } | SupervisionKind::Plan { .. } => true,
        _ => false,
    }
}

fn detect_stagnation(
    projection: &SupervisionProjection,
    config: SupervisionDetectorConfig,
    now: u64,
) -> Option<InterventionRecord> {
    let fresh_stream = projection
        .detection_events()
        .filter_map(|event| match &event.data {
            SupervisionKind::Stream { facts, .. } => Some((event.timestamp, facts)),
            _ => None,
        })
        .last()
        .is_some_and(|(timestamp, stream)| {
            (stream.state == StreamState::Streaming
                || matches!(
                    stream.provider,
                    ProviderState::Connecting | ProviderState::RetryScheduled
                ))
                && timestamp.saturating_add(config.stagnation_after_secs) >= now
        });
    if fresh_stream
        || projection.processes().any(|process| match process.state {
            crate::supervision::ProcessState::Running => {
                process
                    .heartbeat_at
                    .saturating_add(config.stagnation_after_secs)
                    >= now
            }
            crate::supervision::ProcessState::WaitingInput
            | crate::supervision::ProcessState::WaitingApproval => true,
            _ => false,
        })
    {
        return None;
    }
    let last = projection
        .detection_events()
        .filter(|event| meaningful_event(event))
        .last()?;
    if now.saturating_sub(last.timestamp) < config.stagnation_after_secs {
        return None;
    }
    shadow_record(
        InterventionCandidate::Stagnation,
        InterventionVerdict::Confirmed,
        "bd2.stagnation.v1",
        std::slice::from_ref(last),
        now,
    )
}

fn detect_off_goal(projection: &SupervisionProjection, now: u64) -> Option<InterventionRecord> {
    if !projection.goal().is_some_and(|goal| goal.active) {
        return None;
    }
    let latest_assistant = projection
        .detection_events()
        .filter(|event| {
            matches!(
                &event.data,
                SupervisionKind::Message {
                    role: MessageRole::Assistant,
                    body: Some(_),
                    ..
                }
            )
        })
        .last()?;
    let mut evidence = projection
        .detection_events()
        .filter(|event| {
            event.seq <= latest_assistant.seq
                && matches!(
                    event.data,
                    SupervisionKind::Goal { .. }
                        | SupervisionKind::Plan { .. }
                        | SupervisionKind::Task { .. }
                        | SupervisionKind::Message { .. }
                        | SupervisionKind::ToolCall { .. }
                        | SupervisionKind::ToolResult { .. }
                        | SupervisionKind::Diff { .. }
                        | SupervisionKind::Test { .. }
                )
        })
        .cloned()
        .collect::<Vec<_>>();
    if evidence.len() > OFF_GOAL_EVIDENCE_EVENTS {
        evidence.drain(..evidence.len() - OFF_GOAL_EVIDENCE_EVENTS);
    }
    if !evidence
        .iter()
        .any(|event| matches!(event.data, SupervisionKind::Goal { facts: Some(_) }))
    {
        if let Some(goal) = projection
            .detection_events()
            .filter(|event| matches!(event.data, SupervisionKind::Goal { facts: Some(_) }))
            .last()
        {
            evidence.insert(0, goal.clone());
        }
    }
    if !evidence
        .iter()
        .any(|event| matches!(event.data, SupervisionKind::Goal { facts: Some(_) }))
    {
        return None;
    }
    shadow_record(
        InterventionCandidate::OffGoal,
        InterventionVerdict::Inconclusive,
        "bd2.off_goal_evidence.v1",
        &evidence,
        now,
    )
}

fn detect_provider(projection: &SupervisionProjection, now: u64) -> Option<InterventionRecord> {
    let event = projection
        .detection_events()
        .filter(|event| {
            matches!(
                &event.data,
                SupervisionKind::Stream { facts, error: Some(error) }
                    if facts.state == StreamState::Failed
                        && matches!(facts.provider, ProviderState::Failed | ProviderState::Disconnected)
                        && matches!(
                            error.category,
                            ErrorCategory::Transport
                                | ErrorCategory::StreamCorrupted
                                | ErrorCategory::Timeout
                                | ErrorCategory::RateLimited
                                | ErrorCategory::Authentication
                                | ErrorCategory::Permission
                                | ErrorCategory::InvalidRequest
                                | ErrorCategory::Unavailable
                        )
            )
        })
        .last()?;
    shadow_record(
        InterventionCandidate::ProviderDisconnect,
        InterventionVerdict::Confirmed,
        "bd2.provider.v1",
        std::slice::from_ref(event),
        now,
    )
}

pub fn detect_supervision_candidates(
    projection: &SupervisionProjection,
    config: SupervisionDetectorConfig,
    now: u64,
) -> Vec<InterventionRecord> {
    [
        detect_loop(projection, config, now),
        detect_stagnation(projection, config, now),
        detect_off_goal(projection, now),
        detect_provider(projection, now),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervision::{
        GoalFacts, MessageBody, PlanFacts, ProcessFacts, ProcessState, StreamFacts,
    };
    use serde_json::json;

    fn event(seq: u64, data: SupervisionKind) -> SupervisionEvent {
        SupervisionEvent::new(seq, "chat-1", 1_000 + seq, data)
    }

    fn message(seq: u64, role: MessageRole, body: &str) -> SupervisionEvent {
        event(
            seq,
            SupervisionKind::Message {
                role,
                message_id: SupervisionReference::new(&format!("message-{seq}")),
                body: Some(MessageBody::new(body)),
            },
        )
    }

    fn call(seq: u64, args: &str) -> SupervisionEvent {
        event(
            seq,
            SupervisionKind::ToolCall {
                tool_name: SupervisionReference::new("search_pattern"),
                normalized_args_hash: SupervisionHash::normalized_arguments(&json!({"q": args})),
            },
        )
    }

    fn result(seq: u64, output: &str) -> SupervisionEvent {
        event(
            seq,
            SupervisionKind::ToolResult {
                tool_name: SupervisionReference::new("search_pattern"),
                result_hash: SupervisionHash::normalized_result(&json!({"output": output})),
                status: ResultStatus::Succeeded,
                error: None,
            },
        )
    }

    fn replay(events: Vec<SupervisionEvent>) -> SupervisionProjection {
        let mut projection = SupervisionProjection::new("chat-1");
        for event in events {
            projection.append(event).unwrap();
        }
        projection
    }

    fn config() -> SupervisionDetectorConfig {
        SupervisionDetectorConfig {
            loop_threshold: 3,
            stagnation_after_secs: 60,
        }
    }

    fn stagnation_detected(projection: &SupervisionProjection, now: u64) -> bool {
        detect_supervision_candidates(projection, config(), now)
            .iter()
            .any(|record| record.candidate == InterventionCandidate::Stagnation)
    }

    #[test]
    fn supervision_replay_loop_positive_and_changed_arguments_negative() {
        let projection = replay(vec![
            call(1, "same"),
            result(2, "same"),
            call(3, "same"),
            result(4, "same"),
            call(5, "same"),
            result(6, "same"),
        ]);
        let candidates = detect_supervision_candidates(&projection, config(), 1_006);
        let candidate = candidates
            .iter()
            .find(|record| record.candidate == InterventionCandidate::Loop)
            .unwrap();
        assert_eq!(candidate.evidence.ids().len(), 6);
        assert_eq!(candidate.verdict, InterventionVerdict::Confirmed);
        assert!(matches!(
            candidate.delivery,
            InterventionDelivery::Suppressed {
                reason: SuppressionReason::ShadowMode
            }
        ));

        let changed = replay(vec![
            call(1, "same"),
            result(2, "same"),
            call(3, "different"),
            result(4, "same"),
            call(5, "same"),
            result(6, "same"),
        ]);
        assert!(!detect_supervision_candidates(&changed, config(), 1_006)
            .iter()
            .any(|record| record.candidate == InterventionCandidate::Loop));
    }

    #[test]
    fn supervision_replay_loop_negative_for_new_diff_or_result() {
        let changed_result = replay(vec![
            call(1, "same"),
            result(2, "one"),
            call(3, "same"),
            result(4, "two"),
            call(5, "same"),
            result(6, "three"),
        ]);
        assert!(
            !detect_supervision_candidates(&changed_result, config(), 1_006)
                .iter()
                .any(|record| record.candidate == InterventionCandidate::Loop)
        );
        let diff = replay(vec![
            call(1, "same"),
            result(2, "same"),
            event(
                3,
                SupervisionKind::Diff {
                    status: DiffStatus::Changed,
                },
            ),
            call(4, "same"),
            result(5, "same"),
            call(6, "same"),
            result(7, "same"),
        ]);
        assert!(!detect_supervision_candidates(&diff, config(), 1_007)
            .iter()
            .any(|record| record.candidate == InterventionCandidate::Loop));
    }

    #[test]
    fn supervision_intentional_rerun_does_not_create_loop_verdict() {
        let projection = replay(vec![
            call(1, "same"),
            result(2, "same"),
            message(
                3,
                MessageRole::User,
                "Run it once more to check reproducibility",
            ),
            call(4, "same"),
            result(5, "same"),
            message(6, MessageRole::User, "Repeat the reproduction"),
            call(7, "same"),
            result(8, "same"),
        ]);
        assert!(!detect_supervision_candidates(&projection, config(), 1_008)
            .iter()
            .any(|record| record.candidate == InterventionCandidate::Loop));
    }

    #[test]
    fn supervision_stagnation_running_heartbeat_must_be_fresh() {
        let running = |heartbeat_at| {
            let mut projection = replay(vec![message(1, MessageRole::Assistant, "tests started")]);
            projection
                .append(event(
                    2,
                    SupervisionKind::Test {
                        status: TestStatus::Running,
                    },
                ))
                .unwrap();
            projection
                .append(event(
                    3,
                    SupervisionKind::Process {
                        facts: ProcessFacts {
                            process_id: SupervisionReference::new("test-process"),
                            state: ProcessState::Running,
                            heartbeat_at,
                        },
                    },
                ))
                .unwrap();
            projection
        };
        assert!(!stagnation_detected(&running(9_950), 10_000));
        assert!(stagnation_detected(&running(9_000), 10_000));
        assert!(!stagnation_detected(&running(9_940), 10_000));
    }

    #[test]
    fn supervision_stagnation_pending_decisions_ignore_heartbeat_age() {
        for process_state in [ProcessState::WaitingInput, ProcessState::WaitingApproval] {
            let projection = replay(vec![
                message(1, MessageRole::Assistant, "waiting"),
                event(
                    2,
                    SupervisionKind::Process {
                        facts: ProcessFacts {
                            process_id: SupervisionReference::new("wait"),
                            state: process_state,
                            heartbeat_at: 1_002,
                        },
                    },
                ),
            ]);
            assert!(!stagnation_detected(&projection, 10_000));
        }
    }

    #[test]
    fn supervision_stagnation_stream_activity_must_be_fresh() {
        for facts in [
            StreamFacts {
                state: StreamState::Streaming,
                provider: ProviderState::Ready,
            },
            StreamFacts {
                state: StreamState::Idle,
                provider: ProviderState::Connecting,
            },
            StreamFacts {
                state: StreamState::Idle,
                provider: ProviderState::RetryScheduled,
            },
        ] {
            let streaming = |timestamp| {
                replay(vec![
                    message(1, MessageRole::Assistant, "working"),
                    SupervisionEvent::new(
                        2,
                        "chat-1",
                        timestamp,
                        SupervisionKind::Stream {
                            facts: facts.clone(),
                            error: None,
                        },
                    ),
                ])
            };
            assert!(!stagnation_detected(&streaming(9_950), 10_000));
            assert!(stagnation_detected(&streaming(9_000), 10_000));
            assert!(!stagnation_detected(&streaming(9_940), 10_000));
        }
    }

    #[test]
    fn supervision_stagnation_requires_a_meaningful_event() {
        let projection = replay(vec![
            call(1, "same"),
            event(
                2,
                SupervisionKind::Process {
                    facts: ProcessFacts {
                        process_id: SupervisionReference::new("stale-process"),
                        state: ProcessState::Running,
                        heartbeat_at: 1_002,
                    },
                },
            ),
        ]);
        assert!(!stagnation_detected(&projection, 10_000));
    }

    #[test]
    fn supervision_off_goal_evidence_packet_is_bounded_and_not_a_verdict() {
        let projection = replay(vec![
            event(
                1,
                SupervisionKind::Goal {
                    facts: Some(GoalFacts {
                        revision: 7,
                        active: true,
                        autonomy: true,
                        prompt_enabled: true,
                        can_pursue: true,
                    }),
                },
            ),
            event(
                2,
                SupervisionKind::Plan {
                    facts: Some(PlanFacts {
                        reference: SupervisionReference::new("plan-1"),
                        version: 2,
                        has_pending: true,
                    }),
                },
            ),
            message(3, MessageRole::Assistant, "I will inspect another area"),
        ]);
        let candidates = detect_supervision_candidates(&projection, config(), 1_003);
        let off_goal = candidates
            .iter()
            .find(|record| record.candidate == InterventionCandidate::OffGoal)
            .unwrap();
        assert_eq!(off_goal.verdict, InterventionVerdict::Inconclusive);
        assert_eq!(
            off_goal.detector_version.as_str(),
            "bd2.off_goal_evidence.v1"
        );
        assert!(off_goal.evidence.ids().iter().any(|id| id.seq == 1));
        assert!(off_goal.evidence.ids().iter().any(|id| id.seq == 3));
        assert!(off_goal.evidence.ids().len() <= OFF_GOAL_EVIDENCE_EVENTS);
    }

    #[test]
    fn supervision_terminal_provider_error_differs_from_retryable_retry() {
        let retry = replay(vec![event(
            1,
            SupervisionKind::Stream {
                facts: StreamFacts {
                    state: StreamState::Idle,
                    provider: ProviderState::RetryScheduled,
                },
                error: Some(ErrorFingerprint {
                    category: ErrorCategory::Transport,
                    http_status: Some(503),
                }),
            },
        )]);
        assert!(!detect_supervision_candidates(&retry, config(), 1_001)
            .iter()
            .any(|record| record.candidate == InterventionCandidate::ProviderDisconnect));
        let terminal = replay(vec![event(
            1,
            SupervisionKind::Stream {
                facts: StreamFacts {
                    state: StreamState::Failed,
                    provider: ProviderState::Disconnected,
                },
                error: Some(ErrorFingerprint {
                    category: ErrorCategory::Transport,
                    http_status: None,
                }),
            },
        )]);
        let candidates = detect_supervision_candidates(&terminal, config(), 1_001);
        let provider = candidates
            .iter()
            .find(|record| record.candidate == InterventionCandidate::ProviderDisconnect)
            .unwrap();
        assert_eq!(provider.evidence.ids()[0].seq, 1);
    }

    #[test]
    fn supervision_own_events_are_suppressed_from_all_detectors() {
        let mut own = event(
            1,
            SupervisionKind::Stream {
                facts: StreamFacts {
                    state: StreamState::Failed,
                    provider: ProviderState::Disconnected,
                },
                error: Some(ErrorFingerprint {
                    category: ErrorCategory::Transport,
                    http_status: None,
                }),
            },
        );
        own.origin = crate::supervision::EventOrigin::Buddy;
        let projection = replay(vec![own]);
        assert!(detect_supervision_candidates(&projection, config(), 10_000).is_empty());
    }

    #[test]
    fn supervision_candidates_never_serialize_raw_tool_or_message_secrets() {
        let secret = "Bearer sk-SECRET-SUPERVISION-CANARY";
        let projection = replay(vec![
            event(
                1,
                SupervisionKind::Goal {
                    facts: Some(GoalFacts {
                        revision: 1,
                        active: true,
                        autonomy: false,
                        prompt_enabled: true,
                        can_pursue: false,
                    }),
                },
            ),
            message(2, MessageRole::Assistant, secret),
        ]);
        let serialized =
            serde_json::to_string(&detect_supervision_candidates(&projection, config(), 1_002))
                .unwrap();
        assert!(!serialized.contains("SECRET-SUPERVISION-CANARY"));
        assert!(!serialized.contains("reasoning"));
        assert!(!serialized.contains("tool_output"));
        assert!(serialized.contains("REDACTED"));
    }
}
