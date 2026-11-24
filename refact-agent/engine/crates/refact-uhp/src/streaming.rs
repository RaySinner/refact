use std::collections::BTreeMap;

use eventsource_stream::Eventsource;
use futures::{stream::BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{OutputItem, Response, ResponseStatus};

#[derive(Clone, Debug, PartialEq)]
pub struct StreamEvent {
    pub sequence_number: u64,
    pub kind: StreamEventKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StreamEventKind {
    Created(Response),
    InProgress(Response),
    OutputItemAdded {
        output_index: u64,
        item: OutputItem,
    },
    OutputItemDone {
        output_index: u64,
        item: OutputItem,
    },
    OutputTextDelta {
        item_id: String,
        output_index: u64,
        content_index: u64,
        delta: String,
    },
    OutputTextDone {
        item_id: String,
        output_index: u64,
        content_index: u64,
        text: String,
    },
    NonTerminalError(StreamErrorObject),
    Terminal(Response),
    Other {
        event_type: String,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct StreamErrorObject {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub param: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamFailureKind {
    SequenceGap { expected: u64, actual: u64 },
    MissingCreated,
    DuplicateCreated,
    MissingTerminal,
    EventAfterTerminal,
    InvalidEvent(String),
    MalformedJson(String),
    Transport(String),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("UHP stream failed: {kind:?}; recovery response id: {response_id:?}")]
pub struct StreamFailure {
    pub kind: StreamFailureKind,
    pub response_id: Option<String>,
}

pub struct ResponseStream {
    frames: BoxStream<'static, Result<String, String>>,
    expected_sequence: u64,
    created: bool,
    response_id: Option<String>,
    pending_terminal: Option<StreamEvent>,
    finished: bool,
}

impl ResponseStream {
    pub(crate) fn new(response: reqwest::Response) -> Self {
        let frames = response
            .bytes_stream()
            .eventsource()
            .map(|event| match event {
                Ok(event) => Ok(event.data),
                Err(error) => Err(error.to_string()),
            })
            .boxed();
        Self {
            frames,
            expected_sequence: 0,
            created: false,
            response_id: None,
            pending_terminal: None,
            finished: false,
        }
    }

    pub fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, StreamFailure> {
        let result = self.next_event_inner().await;
        if result.is_err() {
            self.finished = true;
        }
        result
    }

    async fn next_event_inner(&mut self) -> Result<Option<StreamEvent>, StreamFailure> {
        if self.finished {
            return Ok(None);
        }
        loop {
            let frame = match self.frames.next().await {
                Some(Ok(frame)) => frame,
                Some(Err(error)) => {
                    self.finished = true;
                    return Err(self.failure(StreamFailureKind::Transport(error)));
                }
                None => {
                    self.finished = true;
                    if let Some(terminal) = self.pending_terminal.take() {
                        return Ok(Some(terminal));
                    }
                    let kind = if self.created {
                        StreamFailureKind::MissingTerminal
                    } else {
                        StreamFailureKind::MissingCreated
                    };
                    return Err(self.failure(kind));
                }
            };
            if self.pending_terminal.is_some() {
                self.finished = true;
                return Err(self.failure(StreamFailureKind::EventAfterTerminal));
            }
            let event = self.decode(&frame)?;
            if event.sequence_number != self.expected_sequence {
                self.finished = true;
                return Err(self.failure(StreamFailureKind::SequenceGap {
                    expected: self.expected_sequence,
                    actual: event.sequence_number,
                }));
            }
            self.expected_sequence += 1;
            if !self.created && !matches!(event.kind, StreamEventKind::Created(_)) {
                self.finished = true;
                return Err(self.failure(StreamFailureKind::MissingCreated));
            }
            match event.kind {
                StreamEventKind::Created(ref response) => {
                    if self.created {
                        self.finished = true;
                        return Err(self.failure(StreamFailureKind::DuplicateCreated));
                    }
                    self.validate_response(response, ResponseStatus::InProgress)?;
                    self.response_id = Some(response.id.clone());
                    self.created = true;
                    return Ok(Some(event));
                }
                StreamEventKind::InProgress(ref response) => {
                    self.validate_response(response, ResponseStatus::InProgress)?;
                    return Ok(Some(event));
                }
                StreamEventKind::Terminal(ref response) => {
                    self.validate_terminal(response)?;
                    self.pending_terminal = Some(event);
                }
                _ => return Ok(Some(event)),
            }
        }
    }

    pub async fn complete(mut self) -> Result<Response, StreamFailure> {
        loop {
            match self.next_event().await? {
                Some(StreamEvent {
                    kind: StreamEventKind::Terminal(response),
                    ..
                }) => return Ok(response),
                Some(_) => {}
                None => {
                    return Err(StreamFailure {
                        kind: StreamFailureKind::MissingTerminal,
                        response_id: self.response_id,
                    })
                }
            }
        }
    }

    fn decode(&self, frame: &str) -> Result<StreamEvent, StreamFailure> {
        let value: Value = serde_json::from_str(frame)
            .map_err(|error| self.failure(StreamFailureKind::MalformedJson(error.to_string())))?;
        let event_type = required_string(&value, "type")
            .map_err(|error| self.failure(StreamFailureKind::InvalidEvent(error)))?;
        let sequence_number = value
            .get("sequence_number")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                self.failure(StreamFailureKind::InvalidEvent(
                    "sequence_number must be an unsigned integer".to_string(),
                ))
            })?;
        let kind = match event_type.as_str() {
            "response.created" => StreamEventKind::Created(self.response(&value)?),
            "response.in_progress" => StreamEventKind::InProgress(self.response(&value)?),
            "response.output_item.added" => StreamEventKind::OutputItemAdded {
                output_index: self.index(&value, "output_index")?,
                item: self.item(&value)?,
            },
            "response.output_item.done" => StreamEventKind::OutputItemDone {
                output_index: self.index(&value, "output_index")?,
                item: self.item(&value)?,
            },
            "response.output_text.delta" => StreamEventKind::OutputTextDelta {
                item_id: self.value_string(&value, "item_id")?,
                output_index: self.index(&value, "output_index")?,
                content_index: self.index(&value, "content_index")?,
                delta: self.value_string(&value, "delta")?,
            },
            "response.output_text.done" => StreamEventKind::OutputTextDone {
                item_id: self.value_string(&value, "item_id")?,
                output_index: self.index(&value, "output_index")?,
                content_index: self.index(&value, "content_index")?,
                text: self.value_string(&value, "text")?,
            },
            "error" => StreamEventKind::NonTerminalError(self.decode_value(value)?),
            "response.completed" | "response.incomplete" | "response.failed" => {
                let response = self.response(&value)?;
                let valid = match event_type.as_str() {
                    "response.completed" => response.status == ResponseStatus::Completed,
                    "response.incomplete" => response.status == ResponseStatus::Incomplete,
                    "response.failed" => matches!(
                        response.status,
                        ResponseStatus::Failed | ResponseStatus::Cancelled
                    ),
                    _ => false,
                };
                if !valid {
                    return Err(self.failure(StreamFailureKind::InvalidEvent(
                        "terminal event and response status disagree".to_string(),
                    )));
                }
                StreamEventKind::Terminal(response)
            }
            _ => StreamEventKind::Other { event_type },
        };
        Ok(StreamEvent {
            sequence_number,
            kind,
        })
    }

    fn response(&self, value: &Value) -> Result<Response, StreamFailure> {
        let response = value.get("response").cloned().ok_or_else(|| {
            self.failure(StreamFailureKind::InvalidEvent(
                "response field is required".to_string(),
            ))
        })?;
        self.decode_value(response)
    }

    fn item(&self, value: &Value) -> Result<OutputItem, StreamFailure> {
        let item = value.get("item").cloned().ok_or_else(|| {
            self.failure(StreamFailureKind::InvalidEvent(
                "item field is required".to_string(),
            ))
        })?;
        self.decode_value(item)
    }

    fn decode_value<T: serde::de::DeserializeOwned>(
        &self,
        value: Value,
    ) -> Result<T, StreamFailure> {
        serde_json::from_value(value)
            .map_err(|error| self.failure(StreamFailureKind::InvalidEvent(error.to_string())))
    }

    fn index(&self, value: &Value, field: &str) -> Result<u64, StreamFailure> {
        value.get(field).and_then(Value::as_u64).ok_or_else(|| {
            self.failure(StreamFailureKind::InvalidEvent(format!(
                "{field} must be an unsigned integer"
            )))
        })
    }

    fn value_string(&self, value: &Value, field: &str) -> Result<String, StreamFailure> {
        required_string(value, field)
            .map_err(|error| self.failure(StreamFailureKind::InvalidEvent(error)))
    }

    fn validate_response(
        &self,
        response: &Response,
        expected: ResponseStatus,
    ) -> Result<(), StreamFailure> {
        if response.object != "response" || response.status != expected {
            return Err(self.failure(StreamFailureKind::InvalidEvent(format!(
                "expected response object with status {expected:?}"
            ))));
        }
        if let Some(response_id) = &self.response_id {
            if response_id != &response.id {
                return Err(self.failure(StreamFailureKind::InvalidEvent(
                    "response id changed within stream".to_string(),
                )));
            }
        }
        Ok(())
    }

    fn validate_terminal(&self, response: &Response) -> Result<(), StreamFailure> {
        if response.object != "response"
            || !matches!(
                response.status,
                ResponseStatus::Completed
                    | ResponseStatus::Incomplete
                    | ResponseStatus::Failed
                    | ResponseStatus::Cancelled
            )
        {
            return Err(self.failure(StreamFailureKind::InvalidEvent(
                "terminal event requires a terminal response status".to_string(),
            )));
        }
        if let Some(response_id) = &self.response_id {
            if response_id != &response.id {
                return Err(self.failure(StreamFailureKind::InvalidEvent(
                    "response id changed within stream".to_string(),
                )));
            }
        }
        Ok(())
    }

    fn failure(&self, kind: StreamFailureKind) -> StreamFailure {
        StreamFailure {
            kind,
            response_id: self.response_id.clone(),
        }
    }
}

fn required_string(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| format!("{field} must be a string"))
}
