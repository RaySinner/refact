use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::autonomous_redaction::redact_and_cap_text;

pub const SUPERVISION_SCHEMA_VERSION: u16 = 1;
pub const MAX_SUPERVISION_EVENTS: usize = 32;
pub const MAX_MESSAGE_BYTES: usize = 512;
pub const MAX_REFERENCE_BYTES: usize = 128;
pub const MAX_PROCESS_FACTS: usize = 8;
pub const MAX_EVIDENCE_IDS: usize = 16;
const MAX_HASH_NODES: usize = 256;
const MAX_HASH_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct RedactedText<const LIMIT: usize>(String);

impl<const LIMIT: usize> RedactedText<LIMIT> {
    pub fn new(raw: &str) -> Self {
        Self(redact_and_cap_text(raw, LIMIT))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de, const LIMIT: usize> Deserialize<'de> for RedactedText<LIMIT> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::new(&raw))
    }
}

pub type MessageBody = RedactedText<MAX_MESSAGE_BYTES>;
pub type SupervisionReference = RedactedText<MAX_REFERENCE_BYTES>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct SupervisionVersion;

impl TryFrom<u16> for SupervisionVersion {
    type Error = &'static str;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if value == SUPERVISION_SCHEMA_VERSION {
            Ok(Self)
        } else {
            Err("unsupported supervision schema version")
        }
    }
}

impl From<SupervisionVersion> for u16 {
    fn from(_: SupervisionVersion) -> Self {
        SUPERVISION_SCHEMA_VERSION
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SupervisionHash([u8; 32]);

impl SupervisionHash {
    pub fn normalized_arguments(value: &Value) -> Self {
        Self::normalized_payload("supervision.args.v1", value)
    }

    pub fn normalized_result(value: &Value) -> Self {
        Self::normalized_payload("supervision.result.v1", value)
    }

    fn normalized_payload(domain: &str, value: &Value) -> Self {
        let mut remaining = MAX_HASH_NODES;
        let normalized = normalize_hash_value(value, 0, &mut remaining);
        Self::digest(domain, normalized.to_string().as_bytes())
    }

    fn digest(domain: &str, bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(domain.as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        Self(hasher.finalize().into())
    }
}

impl TryFrom<String> for SupervisionHash {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid supervision hash");
        }
        let bytes = hex::decode(value).map_err(|_| "invalid supervision hash")?;
        let bytes = bytes.try_into().map_err(|_| "invalid supervision hash")?;
        Ok(Self(bytes))
    }
}

impl From<SupervisionHash> for String {
    fn from(value: SupervisionHash) -> Self {
        hex::encode(value.0)
    }
}

fn private_payload_key(key: &str) -> bool {
    let compact: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "reasoning",
        "thinking",
        "apikey",
        "authorization",
        "credential",
        "password",
        "secret",
        "token",
    ]
    .iter()
    .any(|private| compact.contains(private))
}

fn normalize_hash_value(value: &Value, depth: usize, remaining: &mut usize) -> Value {
    if depth >= MAX_HASH_DEPTH || *remaining == 0 {
        return Value::String("[bounded]".into());
    }
    *remaining -= 1;
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort();
            let mut normalized = serde_json::Map::new();
            for key in keys.into_iter().take(MAX_HASH_NODES) {
                if *remaining == 0 {
                    break;
                }
                if !private_payload_key(key) {
                    let safe_key = SupervisionReference::new(key);
                    normalized.insert(
                        safe_key.0,
                        normalize_hash_value(&object[key], depth + 1, remaining),
                    );
                }
            }
            Value::Object(normalized)
        }
        Value::Array(values) => {
            let mut normalized = Vec::new();
            for value in values.iter().take(MAX_HASH_NODES) {
                if *remaining == 0 {
                    break;
                }
                normalized.push(normalize_hash_value(value, depth + 1, remaining));
            }
            Value::Array(normalized)
        }
        Value::String(text) => {
            let redacted = MessageBody::new(text);
            Value::String(redacted.0.split_whitespace().collect::<Vec<_>>().join(" "))
        }
        other => other.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventOrigin {
    Chat,
    Buddy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    SystemNotice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Transport,
    StreamCorrupted,
    Timeout,
    RateLimited,
    Authentication,
    Permission,
    InvalidRequest,
    Unavailable,
    Tool,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorFingerprint {
    pub category: ErrorCategory,
    pub http_status: Option<u16>,
}

impl ErrorFingerprint {
    pub fn hash(&self) -> SupervisionHash {
        SupervisionHash::digest(
            "supervision.error.v1",
            serde_json::json!([self.category, self.http_status])
                .to_string()
                .as_bytes(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffStatus {
    Clean,
    Changed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    NotRun,
    Running,
    Passed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamState {
    Idle,
    Streaming,
    Finished,
    Failed,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderState {
    Ready,
    Connecting,
    RetryScheduled,
    Disconnected,
    Failed,
    RateLimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    WaitingInput,
    WaitingApproval,
    Finished,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalFacts {
    pub revision: u64,
    pub active: bool,
    pub autonomy: bool,
    pub prompt_enabled: bool,
    pub can_pursue: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanFacts {
    pub reference: SupervisionReference,
    pub version: u64,
    pub has_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamFacts {
    pub state: StreamState,
    pub provider: ProviderState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessFacts {
    pub process_id: SupervisionReference,
    pub state: ProcessState,
    pub heartbeat_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SupervisionKind {
    Message {
        role: MessageRole,
        message_id: SupervisionReference,
        body: Option<MessageBody>,
    },
    ToolCall {
        tool_name: SupervisionReference,
        normalized_args_hash: SupervisionHash,
    },
    ToolResult {
        tool_name: SupervisionReference,
        result_hash: SupervisionHash,
        status: ResultStatus,
        error: Option<ErrorFingerprint>,
    },
    Diff {
        status: DiffStatus,
    },
    Test {
        status: TestStatus,
    },
    Goal {
        facts: Option<GoalFacts>,
    },
    Plan {
        facts: Option<PlanFacts>,
    },
    Task {
        reference: Option<SupervisionReference>,
    },
    Stream {
        facts: StreamFacts,
        error: Option<ErrorFingerprint>,
    },
    Process {
        facts: ProcessFacts,
    },
    Intervention {
        delivery_reference: SupervisionReference,
        message_id: Option<SupervisionReference>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceId {
    pub chat_id: SupervisionReference,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisionEvent {
    pub schema_version: SupervisionVersion,
    pub seq: u64,
    pub chat_id: SupervisionReference,
    pub timestamp: u64,
    pub origin: EventOrigin,
    pub data: SupervisionKind,
}

impl SupervisionEvent {
    pub fn new(seq: u64, chat_id: &str, timestamp: u64, data: SupervisionKind) -> Self {
        let origin = if matches!(data, SupervisionKind::Intervention { .. }) {
            EventOrigin::Buddy
        } else {
            EventOrigin::Chat
        };
        Self {
            schema_version: SupervisionVersion,
            seq,
            chat_id: SupervisionReference::new(chat_id),
            timestamp,
            origin,
            data,
        }
    }

    pub fn is_own_event(&self) -> bool {
        self.origin == EventOrigin::Buddy
            || matches!(self.data, SupervisionKind::Intervention { .. })
    }

    pub fn evidence_id(&self) -> EvidenceId {
        EvidenceId {
            chat_id: self.chat_id.clone(),
            seq: self.seq,
        }
    }

    pub fn hash(&self) -> Result<SupervisionHash, serde_json::Error> {
        Ok(SupervisionHash::digest(
            "supervision.event.v1",
            &serde_json::to_vec(self)?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentProcess {
    seq: u64,
    facts: ProcessFacts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentFacts {
    goal: Option<GoalFacts>,
    plan: Option<PlanFacts>,
    task: Option<SupervisionReference>,
    stream: Option<StreamFacts>,
    diff: Option<DiffStatus>,
    test: Option<TestStatus>,
    processes: BTreeMap<SupervisionReference, CurrentProcess>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ProjectionWire")]
pub struct SupervisionProjection {
    schema_version: SupervisionVersion,
    chat_id: SupervisionReference,
    last_seq: Option<u64>,
    events: VecDeque<SupervisionEvent>,
    current: CurrentFacts,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionWire {
    schema_version: SupervisionVersion,
    chat_id: SupervisionReference,
    last_seq: Option<u64>,
    events: VecDeque<SupervisionEvent>,
    current: CurrentFacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionError {
    WrongChat,
    OutOfOrder,
    InvalidBounds,
    InvalidEvidence,
}

impl std::fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::WrongChat => "supervision chat mismatch",
            Self::OutOfOrder => "supervision sequence out of order",
            Self::InvalidBounds => "supervision bounds exceeded",
            Self::InvalidEvidence => "invalid supervision evidence",
        })
    }
}

impl std::error::Error for ProjectionError {}

impl TryFrom<ProjectionWire> for SupervisionProjection {
    type Error = ProjectionError;

    fn try_from(wire: ProjectionWire) -> Result<Self, Self::Error> {
        if wire.events.len() > MAX_SUPERVISION_EVENTS
            || wire.current.processes.len() > MAX_PROCESS_FACTS
        {
            return Err(ProjectionError::InvalidBounds);
        }
        let mut previous = None;
        for event in &wire.events {
            if event.chat_id != wire.chat_id {
                return Err(ProjectionError::WrongChat);
            }
            if previous.is_some_and(|seq| event.seq <= seq) {
                return Err(ProjectionError::OutOfOrder);
            }
            previous = Some(event.seq);
        }
        if previous != wire.last_seq {
            return Err(ProjectionError::OutOfOrder);
        }
        for (id, process) in &wire.current.processes {
            if id != &process.facts.process_id
                || !wire.last_seq.is_some_and(|seq| process.seq <= seq)
            {
                return Err(ProjectionError::OutOfOrder);
            }
        }
        Ok(Self {
            schema_version: wire.schema_version,
            chat_id: wire.chat_id,
            last_seq: wire.last_seq,
            events: wire.events,
            current: wire.current,
        })
    }
}

impl SupervisionProjection {
    pub fn new(chat_id: &str) -> Self {
        Self {
            schema_version: SupervisionVersion,
            chat_id: SupervisionReference::new(chat_id),
            last_seq: None,
            events: VecDeque::new(),
            current: CurrentFacts::default(),
        }
    }

    pub fn append(&mut self, event: SupervisionEvent) -> Result<(), ProjectionError> {
        if event.chat_id != self.chat_id {
            return Err(ProjectionError::WrongChat);
        }
        if self.last_seq.is_some_and(|seq| event.seq <= seq) {
            return Err(ProjectionError::OutOfOrder);
        }
        if !event.is_own_event() {
            match &event.data {
                SupervisionKind::Goal { facts } => self.current.goal = facts.clone(),
                SupervisionKind::Plan { facts } => self.current.plan = facts.clone(),
                SupervisionKind::Task { reference } => self.current.task = reference.clone(),
                SupervisionKind::Stream { facts, .. } => self.current.stream = Some(facts.clone()),
                SupervisionKind::Diff { status } => self.current.diff = Some(*status),
                SupervisionKind::Test { status } => self.current.test = Some(*status),
                SupervisionKind::Process { facts } => {
                    self.current.processes.insert(
                        facts.process_id.clone(),
                        CurrentProcess {
                            seq: event.seq,
                            facts: facts.clone(),
                        },
                    );
                    if self.current.processes.len() > MAX_PROCESS_FACTS {
                        let oldest = self
                            .current
                            .processes
                            .iter()
                            .min_by_key(|(_, process)| process.seq)
                            .map(|(id, _)| id.clone());
                        if let Some(oldest) = oldest {
                            self.current.processes.remove(&oldest);
                        }
                    }
                }
                _ => {}
            }
        }
        self.last_seq = Some(event.seq);
        self.events.push_back(event);
        if self.events.len() > MAX_SUPERVISION_EVENTS {
            self.events.pop_front();
        }
        Ok(())
    }

    pub fn events(&self) -> impl Iterator<Item = &SupervisionEvent> {
        self.events.iter()
    }

    pub fn detection_events(&self) -> impl Iterator<Item = &SupervisionEvent> {
        self.events.iter().filter(|event| !event.is_own_event())
    }

    pub fn goal(&self) -> Option<&GoalFacts> {
        self.current.goal.as_ref()
    }

    pub fn plan(&self) -> Option<&PlanFacts> {
        self.current.plan.as_ref()
    }

    pub fn task(&self) -> Option<&SupervisionReference> {
        self.current.task.as_ref()
    }

    pub fn stream(&self) -> Option<&StreamFacts> {
        self.current.stream.as_ref()
    }

    pub fn processes(&self) -> impl Iterator<Item = &ProcessFacts> {
        self.current
            .processes
            .values()
            .map(|process| &process.facts)
    }

    pub fn snapshot(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn snapshot_hash(&self) -> Result<SupervisionHash, serde_json::Error> {
        Ok(SupervisionHash::digest(
            "supervision.projection.v1",
            self.snapshot()?.as_bytes(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "EvidenceWire")]
pub struct SupervisionEvidence {
    ids: Vec<EvidenceId>,
    hash: SupervisionHash,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceWire {
    ids: Vec<EvidenceId>,
    hash: SupervisionHash,
}

impl TryFrom<EvidenceWire> for SupervisionEvidence {
    type Error = ProjectionError;

    fn try_from(wire: EvidenceWire) -> Result<Self, Self::Error> {
        if wire.ids.is_empty() || wire.ids.len() > MAX_EVIDENCE_IDS {
            return Err(ProjectionError::InvalidEvidence);
        }
        if wire
            .ids
            .windows(2)
            .any(|pair| pair[0].chat_id != pair[1].chat_id || pair[0].seq >= pair[1].seq)
        {
            return Err(ProjectionError::InvalidEvidence);
        }
        Ok(Self {
            ids: wire.ids,
            hash: wire.hash,
        })
    }
}

impl SupervisionEvidence {
    pub fn from_events(events: &[SupervisionEvent]) -> Result<Self, ProjectionError> {
        if events.is_empty() || events.len() > MAX_EVIDENCE_IDS {
            return Err(ProjectionError::InvalidEvidence);
        }
        let mut ordered = BTreeMap::new();
        for event in events {
            if event.is_own_event() || event.chat_id != events[0].chat_id {
                return Err(ProjectionError::InvalidEvidence);
            }
            if ordered.insert(event.evidence_id(), event).is_some() {
                return Err(ProjectionError::InvalidEvidence);
            }
        }
        let bytes = serde_json::to_vec(&ordered.values().collect::<Vec<_>>())
            .map_err(|_| ProjectionError::InvalidEvidence)?;
        Ok(Self {
            ids: ordered.into_keys().collect(),
            hash: SupervisionHash::digest("supervision.evidence.v1", &bytes),
        })
    }

    pub fn ids(&self) -> &[EvidenceId] {
        &self.ids
    }

    pub fn hash(&self) -> &SupervisionHash {
        &self.hash
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionCandidate {
    StalledTurn,
    Loop,
    Stagnation,
    OffGoal,
    ProviderDisconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionVerdict {
    Confirmed,
    Dismissed,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionReason {
    ShadowMode,
    Duplicate,
    Cooldown,
    Authority,
    AutonomyOff,
    UnsafeBoundary,
    StaleEvidence,
    NotConfirmed,
    DeliveryFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum InterventionDelivery {
    Sent {
        delivery_id: SupervisionReference,
        message_id: SupervisionReference,
        sent_at: u64,
    },
    Suppressed {
        reason: SuppressionReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterventionRecord {
    pub schema_version: SupervisionVersion,
    pub candidate: InterventionCandidate,
    pub evidence: SupervisionEvidence,
    pub detector_version: SupervisionReference,
    pub verdict: InterventionVerdict,
    pub delivery: InterventionDelivery,
    pub detected_at: u64,
    pub decided_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(seq: u64, body: &str) -> SupervisionEvent {
        SupervisionEvent::new(
            seq,
            "chat-1",
            1_000 + seq,
            SupervisionKind::Message {
                role: MessageRole::Assistant,
                message_id: SupervisionReference::new(&format!("message-{seq}")),
                body: Some(MessageBody::new(body)),
            },
        )
    }

    fn goal(seq: u64) -> SupervisionEvent {
        SupervisionEvent::new(
            seq,
            "chat-1",
            1_000 + seq,
            SupervisionKind::Goal {
                facts: Some(GoalFacts {
                    revision: 3,
                    active: true,
                    autonomy: false,
                    prompt_enabled: true,
                    can_pursue: false,
                }),
            },
        )
    }

    #[test]
    fn supervision_roundtrip_and_offline_replay_are_deterministic() {
        let mut original = SupervisionProjection::new("chat-1");
        let events = vec![goal(0), message(1, "first"), message(2, "second")];
        for event in &events {
            original.append(event.clone()).unwrap();
        }
        let serialized = original.snapshot().unwrap();
        let mut restored: SupervisionProjection = serde_json::from_str(&serialized).unwrap();
        assert_eq!(original, restored);
        assert_eq!(serialized, restored.snapshot().unwrap());
        assert_eq!(
            original.snapshot_hash().unwrap(),
            restored.snapshot_hash().unwrap()
        );
        let mut replayed = SupervisionProjection::new("chat-1");
        for event in events {
            replayed.append(event).unwrap();
        }
        assert_eq!(replayed.snapshot().unwrap(), serialized);
        original.append(message(3, "next")).unwrap();
        restored.append(message(3, "next")).unwrap();
        assert_eq!(original.snapshot().unwrap(), restored.snapshot().unwrap());
    }

    #[test]
    fn supervision_ring_and_utf8_messages_are_bounded() {
        let mut projection = SupervisionProjection::new("chat-1");
        projection.append(goal(0)).unwrap();
        let text = "🐾 привет 東京 ".repeat(200);
        for seq in 1..=100 {
            projection.append(message(seq, &text)).unwrap();
        }
        assert_eq!(projection.events().count(), MAX_SUPERVISION_EVENTS);
        assert_eq!(projection.events().next().unwrap().seq, 69);
        assert_eq!(projection.goal().unwrap().revision, 3);
        for event in projection.events() {
            let SupervisionKind::Message {
                body: Some(body), ..
            } = &event.data
            else {
                panic!("expected message");
            };
            assert!(body.as_str().len() <= MAX_MESSAGE_BYTES);
            assert!(std::str::from_utf8(body.as_str().as_bytes()).is_ok());
        }
        let restored: SupervisionProjection =
            serde_json::from_str(&projection.snapshot().unwrap()).unwrap();
        assert_eq!(restored, projection);
        assert!(projection.snapshot().unwrap().len() < 32_000);
    }

    #[test]
    fn supervision_current_facts_and_processes_remain_bounded() {
        let mut projection = SupervisionProjection::new("chat-1");
        projection.append(goal(0)).unwrap();
        for seq in 1..=20 {
            projection
                .append(SupervisionEvent::new(
                    seq,
                    "chat-1",
                    1_000 + seq,
                    SupervisionKind::Process {
                        facts: ProcessFacts {
                            process_id: SupervisionReference::new(&format!("process-{seq:02}")),
                            state: ProcessState::Running,
                            heartbeat_at: 1_000 + seq,
                        },
                    },
                ))
                .unwrap();
        }
        assert_eq!(projection.processes().count(), MAX_PROCESS_FACTS);
        assert_eq!(
            projection.processes().next().unwrap().process_id.as_str(),
            "process-13"
        );
        projection
            .append(SupervisionEvent::new(
                21,
                "chat-1",
                1_021,
                SupervisionKind::Plan {
                    facts: Some(PlanFacts {
                        reference: SupervisionReference::new("plan-1"),
                        version: 2,
                        has_pending: true,
                    }),
                },
            ))
            .unwrap();
        projection
            .append(SupervisionEvent::new(
                22,
                "chat-1",
                1_022,
                SupervisionKind::Task {
                    reference: Some(SupervisionReference::new("task-1")),
                },
            ))
            .unwrap();
        projection
            .append(SupervisionEvent::new(
                23,
                "chat-1",
                1_023,
                SupervisionKind::Stream {
                    facts: StreamFacts {
                        state: StreamState::Streaming,
                        provider: ProviderState::Ready,
                    },
                    error: None,
                },
            ))
            .unwrap();
        assert!(projection.plan().unwrap().has_pending);
        assert_eq!(projection.task().unwrap().as_str(), "task-1");
        assert_eq!(projection.stream().unwrap().state, StreamState::Streaming);
        projection
            .append(SupervisionEvent::new(
                24,
                "chat-1",
                1_024,
                SupervisionKind::Goal { facts: None },
            ))
            .unwrap();
        assert!(projection.goal().is_none());
        let restored: SupervisionProjection =
            serde_json::from_str(&projection.snapshot().unwrap()).unwrap();
        assert_eq!(restored, projection);
    }

    #[test]
    fn supervision_rejects_wrong_chat_order_and_oversized_reload() {
        let mut projection = SupervisionProjection::new("chat-1");
        projection.append(message(1, "safe")).unwrap();
        let before = projection.snapshot().unwrap();
        assert_eq!(
            projection.append(message(1, "duplicate")),
            Err(ProjectionError::OutOfOrder)
        );
        let mut wrong = message(2, "wrong chat");
        wrong.chat_id = SupervisionReference::new("chat-2");
        assert_eq!(projection.append(wrong), Err(ProjectionError::WrongChat));
        assert_eq!(before, projection.snapshot().unwrap());
        let mut value = serde_json::to_value(&projection).unwrap();
        value["events"] = json!((0..=MAX_SUPERVISION_EVENTS as u64)
            .map(|seq| message(seq, "safe"))
            .collect::<Vec<_>>());
        assert!(serde_json::from_value::<SupervisionProjection>(value).is_err());
        let mut value = serde_json::to_value(&projection).unwrap();
        value["schema_version"] = json!(2);
        assert!(serde_json::from_value::<SupervisionProjection>(value).is_err());
    }

    #[test]
    fn supervision_redacts_before_storage_debug_and_reload() {
        let raw = "Bearer sk-VERYSECRET123456 token=private-value https://example.test?q=canary";
        let event = message(0, raw);
        let mut projection = SupervisionProjection::new("chat-1");
        projection.append(event).unwrap();
        for serialized in [projection.snapshot().unwrap(), format!("{projection:?}")] {
            for secret in ["VERYSECRET", "private-value", "canary"] {
                assert!(!serialized.contains(secret), "leaked {secret}");
            }
            assert!(serialized.contains("REDACTED"));
        }
        let mut value = serde_json::to_value(message(1, "safe")).unwrap();
        value["data"]["body"] = json!(raw.repeat(100));
        value["chat_id"] = json!("token=chat-secret");
        let restored: SupervisionEvent = serde_json::from_value(value).unwrap();
        let serialized = serde_json::to_string(&restored).unwrap();
        assert!(!serialized.contains("VERYSECRET"));
        assert!(!serialized.contains("chat-secret"));
        let SupervisionKind::Message {
            body: Some(body), ..
        } = restored.data
        else {
            panic!("expected message");
        };
        assert!(body.as_str().len() <= MAX_MESSAGE_BYTES);
        let long_secret = format!("token={} tail", "s".repeat(10_000));
        assert!(!MessageBody::new(&long_secret).as_str().contains("ssss"));
    }

    #[test]
    fn supervision_hashes_are_stable_normalized_and_not_secret_channels() {
        let first = json!({"query": " hello   world ", "limit": 3,
            "password": "first-secret", "nested": {"api_key": "secret-a"},
            "reasoning_content": "private-reasoning", "thinking_blocks": ["private-thinking"]});
        let second = json!({"nested": {"api_key": "secret-b"}, "limit": 3,
            "query": "hello world", "password": "second-secret"});
        let hash = SupervisionHash::normalized_arguments(&first);
        assert_eq!(hash, SupervisionHash::normalized_arguments(&second));
        assert_ne!(
            hash,
            SupervisionHash::normalized_arguments(&json!({"query": "different"}))
        );
        assert_ne!(hash, SupervisionHash::normalized_result(&first));
        let serialized = serde_json::to_string(&hash).unwrap();
        assert_eq!(serialized.len(), 66);
        assert_eq!(
            hash,
            serde_json::from_str::<SupervisionHash>(&serialized).unwrap()
        );
        assert!(!serialized.contains("secret"));
        assert!(serde_json::from_str::<SupervisionHash>("\"raw-tool-args\"").is_err());
        assert!(
            serde_json::from_str::<SupervisionHash>(&format!("\"{}\"", "g".repeat(64))).is_err()
        );
    }

    #[test]
    fn supervision_tool_and_provider_payloads_cannot_enter_snapshots() {
        let mut projection = SupervisionProjection::new("chat-1");
        let raw = json!({"output": "RAW_TOOL_CANARY", "token": "SECRET_CANARY"});
        projection
            .append(SupervisionEvent::new(
                0,
                "chat-1",
                1_000,
                SupervisionKind::ToolCall {
                    tool_name: SupervisionReference::new("search_pattern"),
                    normalized_args_hash: SupervisionHash::normalized_arguments(&raw),
                },
            ))
            .unwrap();
        projection
            .append(SupervisionEvent::new(
                1,
                "chat-1",
                1_001,
                SupervisionKind::ToolResult {
                    tool_name: SupervisionReference::new("search_pattern"),
                    result_hash: SupervisionHash::normalized_result(&raw),
                    status: ResultStatus::Failed,
                    error: Some(ErrorFingerprint {
                        category: ErrorCategory::Permission,
                        http_status: None,
                    }),
                },
            ))
            .unwrap();
        projection
            .append(SupervisionEvent::new(
                2,
                "chat-1",
                1_002,
                SupervisionKind::Stream {
                    facts: StreamFacts {
                        state: StreamState::Failed,
                        provider: ProviderState::Disconnected,
                    },
                    error: Some(ErrorFingerprint {
                        category: ErrorCategory::Transport,
                        http_status: Some(503),
                    }),
                },
            ))
            .unwrap();
        for snapshot in [projection.snapshot().unwrap(), format!("{projection:?}")] {
            assert!(!snapshot.contains("RAW_TOOL_CANARY"));
            assert!(!snapshot.contains("SECRET_CANARY"));
            assert!(!snapshot.contains("reasoning_content"));
            assert!(!snapshot.contains("thinking_blocks"));
        }
        for field in [
            "arguments",
            "result",
            "reasoning_content",
            "thinking_blocks",
            "provider_error",
        ] {
            let mut event = serde_json::to_value(message(3, "safe")).unwrap();
            event["data"][field] = json!("PRIVATE_CANARY");
            assert!(
                serde_json::from_value::<SupervisionEvent>(event).is_err(),
                "accepted {field}"
            );
        }
        let error = ErrorFingerprint {
            category: ErrorCategory::Transport,
            http_status: Some(503),
        };
        assert_eq!(error.hash(), error.clone().hash());
    }

    #[test]
    fn supervision_own_events_are_marked_and_excluded_from_detection() {
        let mut projection = SupervisionProjection::new("chat-1");
        projection.append(message(0, "ordinary")).unwrap();
        let own = SupervisionEvent::new(
            1,
            "chat-1",
            1_001,
            SupervisionKind::Intervention {
                delivery_reference: SupervisionReference::new("delivery-1"),
                message_id: Some(SupervisionReference::new("notice-1")),
            },
        );
        assert_eq!(own.origin, EventOrigin::Buddy);
        assert!(own.is_own_event());
        let restored: SupervisionEvent =
            serde_json::from_str(&serde_json::to_string(&own).unwrap()).unwrap();
        assert!(restored.is_own_event());
        projection.append(restored).unwrap();
        let mut own_goal = goal(2);
        own_goal.origin = EventOrigin::Buddy;
        projection.append(own_goal).unwrap();
        assert_eq!(projection.events().count(), 3);
        assert_eq!(projection.detection_events().count(), 1);
        assert!(projection.goal().is_none());
        assert!(SupervisionEvidence::from_events(&[own]).is_err());
    }

    #[test]
    fn supervision_evidence_ids_hash_and_intervention_shapes_roundtrip() {
        let first = message(1, "first");
        let second = message(2, "second");
        let evidence = SupervisionEvidence::from_events(&[second.clone(), first.clone()]).unwrap();
        assert_eq!(evidence.ids(), &[first.evidence_id(), second.evidence_id()]);
        assert_eq!(
            evidence,
            SupervisionEvidence::from_events(&[first.clone(), second]).unwrap()
        );
        assert!(SupervisionEvidence::from_events(&[]).is_err());
        assert!(SupervisionEvidence::from_events(&[first.clone(), first]).is_err());
        for delivery in [
            InterventionDelivery::Sent {
                delivery_id: SupervisionReference::new("delivery-1"),
                message_id: SupervisionReference::new("notice-1"),
                sent_at: 1_005,
            },
            InterventionDelivery::Suppressed {
                reason: SuppressionReason::ShadowMode,
            },
        ] {
            let record = InterventionRecord {
                schema_version: SupervisionVersion,
                candidate: InterventionCandidate::Loop,
                evidence: evidence.clone(),
                detector_version: SupervisionReference::new("loop.v1"),
                verdict: InterventionVerdict::Confirmed,
                delivery,
                detected_at: 1_003,
                decided_at: 1_004,
            };
            let serialized = serde_json::to_string(&record).unwrap();
            assert_eq!(
                record,
                serde_json::from_str::<InterventionRecord>(&serialized).unwrap()
            );
            let value = serde_json::to_value(&record).unwrap();
            match record.delivery {
                InterventionDelivery::Sent { .. } => {
                    assert_eq!(value["delivery"]["status"], "sent");
                    assert_eq!(value["delivery"]["message_id"], "notice-1");
                    assert!(value["delivery"].get("reason").is_none());
                }
                InterventionDelivery::Suppressed { .. } => {
                    assert_eq!(value["delivery"]["status"], "suppressed");
                    assert_eq!(value["delivery"]["reason"], "shadow_mode");
                    assert!(value["delivery"].get("message_id").is_none());
                }
            }
        }
    }
}
