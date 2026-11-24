use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::UhpError;

pub const UHP_VERSION: &str = "2026-10-19";
pub const UHP_VERSION_HEADER: &str = "UHP-Version";
pub const IDEMPOTENCY_KEY_HEADER: &str = "Idempotency-Key";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConformanceClass {
    Core,
    Extended,
    Full,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Implementation {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Capabilities {
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub sessions: bool,
    #[serde(default)]
    pub cancellation: bool,
    #[serde(default)]
    pub files_input: bool,
    #[serde(default)]
    pub files_output: bool,
    #[serde(default)]
    pub session_listing: bool,
    #[serde(default)]
    pub harness_management: bool,
    #[serde(default)]
    pub harness_retirement: bool,
    #[serde(default)]
    pub input_history: bool,
    #[serde(default)]
    pub input_requests: bool,
    #[serde(default)]
    pub resume: bool,
    #[serde(default)]
    pub session_sharing: bool,
    #[serde(default)]
    pub session_branching: bool,
    #[serde(default)]
    pub idempotency: bool,
    #[serde(default)]
    pub harness_idempotency: bool,
    #[serde(default)]
    pub plugins: bool,
    #[serde(default)]
    pub environments: bool,
    #[serde(default)]
    pub memories: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, bool>,
}

impl Capabilities {
    pub fn get(&self, name: &str) -> bool {
        match name {
            "streaming" => self.streaming,
            "sessions" => self.sessions,
            "cancellation" => self.cancellation,
            "files_input" => self.files_input,
            "files_output" => self.files_output,
            "session_listing" => self.session_listing,
            "harness_management" => self.harness_management,
            "harness_retirement" => self.harness_retirement,
            "input_history" => self.input_history,
            "input_requests" => self.input_requests,
            "resume" => self.resume,
            "session_sharing" => self.session_sharing,
            "session_branching" => self.session_branching,
            "idempotency" => self.idempotency,
            "harness_idempotency" => self.harness_idempotency,
            "plugins" => self.plugins,
            "environments" => self.environments,
            "memories" => self.memories,
            other => self.extra.get(other).copied().unwrap_or(false),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Discovery {
    #[serde(rename = "object")]
    pub object: String,
    pub protocol: String,
    pub versions: Vec<String>,
    pub default_version: String,
    pub conformance_class: ConformanceClass,
    pub capabilities: Capabilities,
    #[serde(default)]
    pub implementation: Option<Implementation>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Discovery {
    pub fn capability(&self, name: &str) -> bool {
        self.capabilities.get(name)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    InProgress,
    Completed,
    Failed,
    Incomplete,
    Cancelled,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct UhpErrorObject {
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub param: Option<String>,
    #[serde(default)]
    pub detail: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl fmt::Display for UhpErrorObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/{}: {}",
            self.error_type, self.code, self.message
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ErrorEnvelope {
    pub error: UhpErrorObject,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct IncompleteDetails {
    pub reason: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ResponseMetadata {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub harness_id: Option<String>,
    #[serde(default)]
    pub ignored_fields: Vec<String>,
    #[serde(default)]
    pub requested_model: Option<String>,
    #[serde(default)]
    pub model_fallback: bool,
    #[serde(default)]
    pub model_fallback_reason: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Response {
    pub id: String,
    #[serde(rename = "object")]
    pub object: String,
    pub created_at: u64,
    pub status: ResponseStatus,
    pub output: Vec<OutputItem>,
    pub model: String,
    #[serde(default)]
    pub error: Option<UhpErrorObject>,
    #[serde(default)]
    pub incomplete_details: Option<IncompleteDetails>,
    #[serde(default)]
    pub previous_response_id: Option<String>,
    pub store: bool,
    #[serde(default)]
    pub usage: Option<Usage>,
    pub metadata: ResponseMetadata,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum OutputItem {
    #[serde(rename = "message")]
    Message(OutputMessage),
    #[serde(rename = "reasoning")]
    Reasoning,
    #[serde(rename = "function_call")]
    FunctionCall,
    #[serde(rename = "function_call_output")]
    FunctionCallOutput,
    #[serde(rename = "input_request")]
    InputRequest,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct OutputMessage {
    pub id: String,
    pub role: String,
    pub status: ResponseStatus,
    pub content: Vec<OutputContentPart>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum OutputContentPart {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(other)]
    Unknown,
}

pub fn completed_assistant_bodies(response: &Response) -> Vec<String> {
    if response.status != ResponseStatus::Completed {
        return Vec::new();
    }
    response
        .output
        .iter()
        .filter_map(|item| match item {
            OutputItem::Message(message)
                if message.status == ResponseStatus::Completed && message.role == "assistant" =>
            {
                let body = message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        OutputContentPart::OutputText { text } => Some(text.as_str()),
                        OutputContentPart::Unknown => None,
                    })
                    .collect::<String>();
                (!body.trim().is_empty()).then_some(body)
            }
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct CreateResponseMetadata {
    pub harness_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct CreateResponse {
    input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    metadata: CreateResponseMetadata,
    stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<String>,
    store: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_step: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_seconds: Option<u64>,
}

impl CreateResponse {
    pub fn new(input: impl Into<String>, harness_id: impl Into<String>) -> Self {
        Self {
            input: input.into(),
            model: None,
            metadata: CreateResponseMetadata {
                harness_id: harness_id.into(),
                environment: None,
                extra: BTreeMap::new(),
            },
            stream: false,
            previous_response_id: None,
            store: true,
            max_step: None,
            timeout_seconds: None,
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn previous_response_id(mut self, response_id: impl Into<String>) -> Self {
        self.previous_response_id = Some(response_id.into());
        self
    }

    pub fn streaming(mut self) -> Self {
        self.stream = true;
        self
    }

    pub fn max_step(mut self, max_step: u64) -> Self {
        self.max_step = Some(max_step);
        self
    }

    pub fn timeout_seconds(mut self, timeout_seconds: u64) -> Self {
        self.timeout_seconds = Some(timeout_seconds);
        self
    }

    pub fn environment(mut self, environment: impl Into<String>) -> Self {
        self.metadata.environment = Some(environment.into());
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.extra.insert(key.into(), value);
        self
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn harness_id(&self) -> &str {
        &self.metadata.harness_id
    }

    pub fn is_streaming(&self) -> bool {
        self.stream
    }

    pub fn fingerprint(&self, scope: &RequestScope) -> Result<RequestFingerprint, UhpError> {
        let body = canonicalize(
            serde_json::to_value(self).map_err(|source| UhpError::Schema {
                operation: "serialize response request",
                source,
            })?,
        );
        request_fingerprint("POST", "/v1/responses", scope, body)
    }

    pub(crate) fn validate(&self, streaming: bool) -> Result<(), UhpError> {
        if self.stream != streaming || !self.store {
            return Err(UhpError::InvalidResponse {
                operation: "validate response request",
                message: if streaming {
                    "streaming recovery requires stream=true and store=true".to_string()
                } else {
                    "non-streaming response reads require stream=false and store=true".to_string()
                },
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestScope {
    pub participant_id: String,
    pub credential_scope_id: String,
}

impl RequestScope {
    pub fn new(participant_id: impl Into<String>, credential_scope_id: impl Into<String>) -> Self {
        Self {
            participant_id: participant_id.into(),
            credential_scope_id: credential_scope_id.into(),
        }
    }
}

#[derive(Serialize)]
struct FingerprintInput<'a> {
    method: &'static str,
    path: &'static str,
    protocol_version: &'static str,
    participant_id: &'a str,
    credential_scope_id: &'a str,
    body: Value,
}

pub(crate) fn request_fingerprint(
    method: &'static str,
    path: &'static str,
    scope: &RequestScope,
    body: Value,
) -> Result<RequestFingerprint, UhpError> {
    let input = canonicalize(
        serde_json::to_value(FingerprintInput {
            method,
            path,
            protocol_version: UHP_VERSION,
            participant_id: &scope.participant_id,
            credential_scope_id: &scope.credential_scope_id,
            body,
        })
        .map_err(|source| UhpError::Schema {
            operation: "serialize request fingerprint",
            source,
        })?,
    );
    let bytes = serde_json::to_vec(&input).map_err(|source| UhpError::Schema {
        operation: "serialize request fingerprint",
        source,
    })?;
    Ok(RequestFingerprint(hex::encode(Sha256::digest(bytes))))
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted = object
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(array) => Value::Array(array.into_iter().map(canonicalize).collect()),
        other => other,
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn new(value: impl Into<String>) -> Result<Self, UhpError> {
        let value = value.into();
        if value.is_empty()
            || !value.is_ascii()
            || value.chars().any(|character| character.is_control())
        {
            return Err(UhpError::InvalidIdempotencyKey);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for IdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IdempotencyKey(..)")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct RequestFingerprint(String);

impl RequestFingerprint {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UhpDeliveryRequestState {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UhpSessionBinding {
    pub participant_id: String,
    pub session_id: String,
    #[serde(default)]
    pub last_response_id: Option<String>,
    #[serde(default)]
    pub last_terminal_status: Option<ResponseStatus>,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub replay_cursor: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UhpDeliveryState {
    pub delivery_id: String,
    pub room_id: String,
    pub event_id: String,
    pub recipient_id: String,
    pub logical_attempt: u64,
    pub request_fingerprint: RequestFingerprint,
    pub idempotency_key: IdempotencyKey,
    pub request_state: UhpDeliveryRequestState,
    #[serde(default)]
    pub response_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub source_revision: Option<String>,
    #[serde(default)]
    pub source_digest: Option<String>,
    #[serde(default)]
    pub environment_version: Option<u64>,
    #[serde(default)]
    pub error_class: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceCheckStatus {
    Pass,
    Fail,
    Skip,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct EvidenceCheck {
    pub name: String,
    pub status: EvidenceCheckStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExternalEvidenceRecord {
    pub smoke_suite_id: String,
    pub server_version: String,
    pub cli_version: String,
    pub model_version: String,
    pub checks: Vec<EvidenceCheck>,
    pub generated_at: u64,
    pub sanitized_report_location: String,
}
