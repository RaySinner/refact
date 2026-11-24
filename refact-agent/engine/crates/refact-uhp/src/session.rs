use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};

use crate::{CreateResponse, Response, ResponseStatus, UhpError};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionBinding {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub latest_response_id: Option<String>,
    #[serde(default)]
    pub harness_id: Option<String>,
    #[serde(default)]
    pub task_pending: bool,
}

#[derive(Debug)]
struct ChainState {
    binding: SessionBinding,
}

#[derive(Clone, Debug)]
pub struct SessionChain {
    state: Arc<Mutex<ChainState>>,
    available: Arc<Notify>,
}

impl SessionChain {
    pub fn new() -> Self {
        Self::restore(SessionBinding::default()).expect("empty session binding is valid")
    }

    pub fn restore(binding: SessionBinding) -> Result<Self, UhpError> {
        if binding.session_id.is_some() != binding.latest_response_id.is_some() {
            return Err(chain_error(
                "session_id and latest_response_id must be restored together",
            ));
        }
        Ok(Self {
            state: Arc::new(Mutex::new(ChainState { binding })),
            available: Arc::new(Notify::new()),
        })
    }

    pub async fn binding(&self) -> SessionBinding {
        self.state.lock().await.binding.clone()
    }

    pub async fn begin(&self) -> SessionTaskGuard {
        loop {
            let notified = self.available.notified();
            let mut state = self.state.lock().await;
            if !state.binding.task_pending {
                let previous_response_id = state.binding.latest_response_id.clone();
                state.binding.task_pending = true;
                return SessionTaskGuard {
                    chain: self.clone(),
                    previous_response_id,
                    harness_id: None,
                };
            }
            drop(state);
            notified.await;
        }
    }

    pub async fn recover_terminal(&self, response: &Response) -> Result<(), UhpError> {
        let mut state = self.state.lock().await;
        let previous_response_id = state.binding.latest_response_id.clone();
        if !state.binding.task_pending
            && state.binding.latest_response_id.as_deref() == Some(response.id.as_str())
        {
            return Ok(());
        }
        if !state.binding.task_pending {
            return Err(chain_error(
                "no pending task accepts this terminal response",
            ));
        }
        validate_response(&state.binding, &previous_response_id, None, response)?;
        complete(&mut state.binding, response)?;
        drop(state);
        self.available.notify_one();
        Ok(())
    }
}

impl Default for SessionChain {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct SessionTaskGuard {
    chain: SessionChain,
    previous_response_id: Option<String>,
    harness_id: Option<String>,
}

impl SessionTaskGuard {
    pub async fn request(
        &mut self,
        input: impl Into<String>,
        harness_id: impl Into<String>,
    ) -> Result<CreateResponse, UhpError> {
        let harness_id = harness_id.into();
        let mut state = self.chain.state.lock().await;
        if let Some(expected) = &state.binding.harness_id {
            if expected != &harness_id {
                return Err(chain_error(
                    "configured harness cannot change within a session",
                ));
            }
        }
        state.binding.harness_id = Some(harness_id.clone());
        self.harness_id = Some(harness_id.clone());
        let mut request = CreateResponse::new(input, harness_id);
        if let Some(previous) = &self.previous_response_id {
            request = request.previous_response_id(previous);
        }
        Ok(request)
    }

    pub async fn observe_created(&mut self, response: &Response) -> Result<(), UhpError> {
        if response.status != ResponseStatus::InProgress {
            return Err(chain_error("created response must be in_progress"));
        }
        let state = self.chain.state.lock().await;
        validate_response(
            &state.binding,
            &self.previous_response_id,
            self.harness_id.as_deref(),
            response,
        )
    }

    pub async fn complete(self, response: &Response) -> Result<(), UhpError> {
        let mut state = self.chain.state.lock().await;
        validate_response(
            &state.binding,
            &self.previous_response_id,
            self.harness_id.as_deref(),
            response,
        )?;
        complete(&mut state.binding, response)?;
        drop(state);
        self.chain.available.notify_one();
        Ok(())
    }
}

fn validate_response(
    binding: &SessionBinding,
    previous_response_id: &Option<String>,
    request_harness_id: Option<&str>,
    response: &Response,
) -> Result<(), UhpError> {
    if &response.previous_response_id != previous_response_id {
        return Err(chain_error(
            "response does not continue the expected chain position",
        ));
    }
    let session_id = response
        .metadata
        .session_id
        .as_deref()
        .ok_or_else(|| chain_error("response is missing metadata.session_id"))?;
    if let Some(expected) = &binding.session_id {
        if expected != session_id {
            return Err(chain_error("response belongs to a different session"));
        }
    }
    let expected_harness = binding.harness_id.as_deref().or(request_harness_id);
    if let (Some(expected), Some(actual)) =
        (expected_harness, response.metadata.harness_id.as_deref())
    {
        if expected != actual {
            return Err(chain_error(
                "response belongs to a different configured harness",
            ));
        }
    }
    Ok(())
}

fn complete(binding: &mut SessionBinding, response: &Response) -> Result<(), UhpError> {
    if !matches!(
        response.status,
        ResponseStatus::Completed
            | ResponseStatus::Incomplete
            | ResponseStatus::Failed
            | ResponseStatus::Cancelled
    ) {
        return Err(chain_error("session task requires a terminal response"));
    }
    binding.session_id = response.metadata.session_id.clone();
    binding.latest_response_id = Some(response.id.clone());
    if let Some(harness_id) = &response.metadata.harness_id {
        binding.harness_id = Some(harness_id.clone());
    }
    binding.task_pending = false;
    Ok(())
}

fn chain_error(message: &str) -> UhpError {
    UhpError::InvalidResponse {
        operation: "session chain",
        message: message.to_string(),
    }
}
