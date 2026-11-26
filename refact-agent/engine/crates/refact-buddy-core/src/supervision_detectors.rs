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
const MISSING_GOAL_CONTROL_TOOLS: &[&str] = &[
    "set_goal",
    "set_goal_budget",
    "get_goal",
    "update_goal",
    "validate_goal",
    "goal_control",
    "pause_goal",
    "snooze_goal",
    "set_plan",
    "update_plan",
    "get_plan",
    "sleep",
    "process_wait",
    "agent_wait",
    "wait_agents",
];

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
        policy_key: None,
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

fn detect_missing_goal(projection: &SupervisionProjection, now: u64) -> Option<InterventionRecord> {
    if projection.goal().is_some() {
        return None;
    }
    let events = projection.detection_events().collect::<Vec<_>>();
    let trigger = *events.last()?;
    let SupervisionKind::ToolCall { tool_name, .. } = &trigger.data else {
        return None;
    };
    if MISSING_GOAL_CONTROL_TOOLS.contains(&tool_name.as_str()) {
        return None;
    }
    let user_index = events.iter().rposition(|event| {
        matches!(
            event.data,
            SupervisionKind::Message {
                role: MessageRole::User,
                ..
            }
        )
    })?;
    let turn = &events[user_index..events.len() - 1];
    if turn.iter().any(|event| match &event.data {
        SupervisionKind::Goal { facts: Some(_) } => true,
        SupervisionKind::ToolCall { tool_name, .. } => {
            !MISSING_GOAL_CONTROL_TOOLS.contains(&tool_name.as_str())
        }
        _ => false,
    }) {
        return None;
    }
    let mut evidence = vec![events[user_index].clone(), trigger.clone()];
    evidence.dedup_by_key(|event| event.seq);
    shadow_record(
        InterventionCandidate::MissingGoal,
        InterventionVerdict::Confirmed,
        "bd2.missing_goal.v1",
        &evidence,
        now,
    )
}

pub fn stalled_turn_reason_context(projection: &SupervisionProjection) -> Option<&'static str> {
    let boundary = projection.boundary()?;
    if projection
        .goal()
        .is_some_and(|goal| goal.active && goal.can_pursue)
    {
        Some("goal_active")
    } else if boundary.card_agent {
        Some("card_doing")
    } else if projection.goal().is_some() {
        None
    } else if boundary.task_chat && projection.goal().is_none() {
        Some("missing_goal")
    } else if projection.plan().is_some_and(|plan| plan.has_pending) {
        Some("plan_pending")
    } else {
        None
    }
}

pub fn detect_stalled_turn(
    projection: &SupervisionProjection,
    now: u64,
) -> Option<InterventionRecord> {
    let boundary = projection.boundary()?;
    if !boundary.tools_enabled || boundary.correction_present {
        return None;
    }
    let events = projection.detection_events().collect::<Vec<_>>();
    let finished = events
        .iter()
        .rev()
        .find(|event| matches!(&event.data, SupervisionKind::Process { facts } if facts.process_id.as_str() == "chat-runtime"))?;
    if !matches!(
        &finished.data,
        SupervisionKind::Process { facts } if facts.state == ProcessState::Finished
    ) {
        return None;
    }
    let assistant_index = events.iter().rposition(|event| {
        matches!(
            &event.data,
            SupervisionKind::Message {
                role: MessageRole::Assistant,
                body: Some(_),
                ..
            }
        )
    })?;
    if events[assistant_index + 1..].iter().any(|event| {
        matches!(
            &event.data,
            SupervisionKind::ToolCall { .. }
                | SupervisionKind::Message {
                    role: MessageRole::User,
                    ..
                }
        )
    }) {
        return None;
    }
    stalled_turn_reason_context(projection)?;
    shadow_record(
        InterventionCandidate::StalledTurn,
        InterventionVerdict::Confirmed,
        "bd2.stalled_turn.v1",
        &[events[assistant_index].clone()],
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
    let stalled_turn = detect_stalled_turn(projection, now);
    let off_goal = if stalled_turn.is_none() {
        detect_off_goal(projection, now)
    } else {
        None
    };
    [
        detect_loop(projection, config, now),
        detect_stagnation(projection, config, now),
        detect_missing_goal(projection, now),
        stalled_turn,
        off_goal,
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
        BoundaryFacts, GoalFacts, MessageBody, PlanFacts, ProcessFacts, ProcessState, StreamFacts,
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
        named_call(seq, "search_pattern", args)
    }

    fn named_call(seq: u64, tool_name: &str, args: &str) -> SupervisionEvent {
        event(
            seq,
            SupervisionKind::ToolCall {
                tool_name: SupervisionReference::new(tool_name),
                normalized_args_hash: SupervisionHash::normalized_arguments(&json!({"q": args})),
            },
        )
    }

    fn goal_event(seq: u64, active: bool) -> SupervisionEvent {
        event(
            seq,
            SupervisionKind::Goal {
                facts: Some(GoalFacts {
                    revision: 1,
                    active,
                    autonomy: false,
                    prompt_enabled: true,
                    can_pursue: active,
                }),
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

    fn stalled_projection(
        goal: Option<GoalFacts>,
        boundary: BoundaryFacts,
        plan_pending: bool,
    ) -> SupervisionProjection {
        let mut events = vec![event(1, SupervisionKind::Boundary { facts: boundary })];
        if let Some(facts) = goal {
            events.push(event(2, SupervisionKind::Goal { facts: Some(facts) }));
        }
        if plan_pending {
            events.push(event(
                3,
                SupervisionKind::Plan {
                    facts: Some(PlanFacts {
                        reference: SupervisionReference::new("plan-1"),
                        version: 1,
                        has_pending: true,
                    }),
                },
            ));
        }
        events.push(message(4, MessageRole::Assistant, "I stopped here"));
        events.push(event(
            5,
            SupervisionKind::Process {
                facts: ProcessFacts {
                    process_id: SupervisionReference::new("chat-runtime"),
                    state: ProcessState::Finished,
                    heartbeat_at: 1_005,
                },
            },
        ));
        replay(events)
    }

    fn active_goal() -> GoalFacts {
        GoalFacts {
            revision: 1,
            active: true,
            autonomy: true,
            prompt_enabled: true,
            can_pursue: true,
        }
    }

    #[test]
    fn stalled_turn_publishes_for_every_live_objective_without_off_goal_duplicate() {
        for (goal, boundary, plan, reason) in [
            (
                Some(active_goal()),
                BoundaryFacts::default(),
                false,
                "goal_active",
            ),
            (
                None,
                BoundaryFacts {
                    card_agent: true,
                    task_chat: true,
                    ..BoundaryFacts::default()
                },
                false,
                "card_doing",
            ),
            (
                None,
                BoundaryFacts {
                    task_chat: true,
                    ..BoundaryFacts::default()
                },
                false,
                "missing_goal",
            ),
            (None, BoundaryFacts::default(), true, "plan_pending"),
        ] {
            let projection = stalled_projection(goal, boundary, plan);
            assert_eq!(stalled_turn_reason_context(&projection), Some(reason));
            let candidates = detect_supervision_candidates(&projection, config(), 1_005);
            assert_eq!(
                candidates
                    .iter()
                    .filter(|record| record.candidate == InterventionCandidate::StalledTurn)
                    .count(),
                1,
                "{reason}"
            );
            assert!(!candidates
                .iter()
                .any(|record| record.candidate == InterventionCandidate::OffGoal));
        }
    }

    #[test]
    fn stalled_turn_ignores_tools_casual_terminal_and_corrected_boundaries() {
        let mut with_tool =
            stalled_projection(Some(active_goal()), BoundaryFacts::default(), false);
        with_tool.append(call(6, "inspect")).unwrap();
        with_tool
            .append(event(
                7,
                SupervisionKind::Process {
                    facts: ProcessFacts {
                        process_id: SupervisionReference::new("chat-runtime"),
                        state: ProcessState::Finished,
                        heartbeat_at: 1_007,
                    },
                },
            ))
            .unwrap();
        assert!(detect_stalled_turn(&with_tool, 1_007).is_none());

        assert!(detect_stalled_turn(
            &stalled_projection(None, BoundaryFacts::default(), false),
            1_005
        )
        .is_none());
        for (active, can_pursue) in [(false, false), (true, false)] {
            let goal = GoalFacts {
                active,
                can_pursue,
                ..active_goal()
            };
            assert!(detect_stalled_turn(
                &stalled_projection(Some(goal.clone()), BoundaryFacts::default(), true),
                1_005
            )
            .is_none());
            assert!(detect_stalled_turn(
                &stalled_projection(Some(goal), BoundaryFacts::default(), false),
                1_005
            )
            .is_none());
        }
        for boundary in [
            BoundaryFacts {
                tools_enabled: false,
                ..BoundaryFacts::default()
            },
            BoundaryFacts {
                correction_present: true,
                ..BoundaryFacts::default()
            },
        ] {
            assert!(detect_stalled_turn(
                &stalled_projection(Some(active_goal()), boundary, false),
                1_005
            )
            .is_none());
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
    fn missing_goal_fires_only_for_first_substantive_tool_in_each_user_turn() {
        let first = replay(vec![
            message(1, MessageRole::User, "Inspect the parser"),
            call(2, "parser"),
        ]);
        let candidates = detect_supervision_candidates(&first, config(), 1_002);
        let missing = candidates
            .iter()
            .find(|record| record.candidate == InterventionCandidate::MissingGoal)
            .unwrap();
        assert_eq!(missing.verdict, InterventionVerdict::Confirmed);
        assert_eq!(missing.evidence.ids().len(), 2);
        assert_eq!(missing.evidence.ids()[0].seq, 1);
        assert_eq!(missing.evidence.ids()[1].seq, 2);
        assert!(missing.evidence.ids().len() <= crate::supervision::MAX_EVIDENCE_IDS);

        let second = replay(vec![
            message(1, MessageRole::User, "Inspect the parser"),
            call(2, "parser"),
            named_call(3, "cat", "parser.rs"),
        ]);
        assert!(!detect_supervision_candidates(&second, config(), 1_003)
            .iter()
            .any(|record| record.candidate == InterventionCandidate::MissingGoal));

        let rearmed = replay(vec![
            message(1, MessageRole::User, "Inspect the parser"),
            call(2, "parser"),
            message(3, MessageRole::User, "Now inspect the renderer"),
            named_call(4, "cat", "renderer.rs"),
        ]);
        assert_eq!(
            detect_supervision_candidates(&rearmed, config(), 1_004)
                .iter()
                .filter(|record| record.candidate == InterventionCandidate::MissingGoal)
                .count(),
            1
        );
        assert!(detect_supervision_candidates(&rearmed, config(), 1_004)
            .iter()
            .all(|record| record.candidate != InterventionCandidate::OffGoal));
    }

    #[test]
    fn missing_goal_ignores_casual_chat_control_tools_and_installed_goals() {
        let casual = replay(vec![message(1, MessageRole::User, "Hello")]);
        assert!(detect_supervision_candidates(&casual, config(), 1_001).is_empty());

        for (index, tool_name) in MISSING_GOAL_CONTROL_TOOLS.iter().enumerate() {
            let seq = index as u64 + 2;
            let projection = replay(vec![
                message(1, MessageRole::User, "Check the current state"),
                named_call(seq, tool_name, "state"),
            ]);
            assert!(
                !detect_supervision_candidates(&projection, config(), 1_000 + seq)
                    .iter()
                    .any(|record| record.candidate == InterventionCandidate::MissingGoal),
                "unexpected missing-goal candidate for {tool_name}"
            );
        }

        for active in [true, false] {
            let projection = replay(vec![
                message(1, MessageRole::User, "Inspect the parser"),
                goal_event(2, active),
                call(3, "parser"),
            ]);
            assert!(!detect_supervision_candidates(&projection, config(), 1_003)
                .iter()
                .any(|record| record.candidate == InterventionCandidate::MissingGoal));
        }

        let installed_this_turn = replay(vec![
            message(1, MessageRole::User, "Start a focused repair"),
            goal_event(2, true),
            event(3, SupervisionKind::Goal { facts: None }),
            call(4, "parser"),
        ]);
        assert!(
            !detect_supervision_candidates(&installed_this_turn, config(), 1_004)
                .iter()
                .any(|record| record.candidate == InterventionCandidate::MissingGoal)
        );
    }

    #[test]
    fn missing_goal_evidence_is_bounded_and_redacted() {
        let secret = "Bearer sk-MISSING-GOAL-CANARY";
        let projection = replay(vec![
            message(1, MessageRole::User, secret),
            named_call(2, "search_pattern", secret),
        ]);
        let candidates = detect_supervision_candidates(&projection, config(), 1_002);
        let missing = candidates
            .iter()
            .find(|record| record.candidate == InterventionCandidate::MissingGoal)
            .unwrap();
        assert_eq!(missing.evidence.ids().len(), 2);
        let serialized = serde_json::to_string(missing).unwrap();
        assert!(!serialized.contains("MISSING-GOAL-CANARY"));
        assert!(!serialized.contains("Bearer"));
        assert!(projection.snapshot().unwrap().contains("REDACTED"));
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
        let candidates = detect_supervision_candidates(&projection, config(), 1_002);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].candidate, InterventionCandidate::OffGoal);
        assert_eq!(
            candidates[0]
                .evidence
                .ids()
                .iter()
                .map(|id| id.seq)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        let serialized = serde_json::to_string(&candidates).unwrap();
        assert!(!serialized.contains("SECRET-SUPERVISION-CANARY"));
        assert!(!serialized.contains("Bearer"));
        assert!(!serialized.contains("sk-"));
        assert!(!serialized.contains("reasoning"));
        assert!(!serialized.contains("tool_output"));
        assert!(projection.snapshot().unwrap().contains("REDACTED"));
    }
}
