use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use reqwest::header::{HeaderMap, AUTHORIZATION};
use reqwest::{Method, RequestBuilder, Url};
use serde::de::DeserializeOwned;

use crate::{
    ConformanceClass, CreateResponse, Discovery, ErrorEnvelope, IdempotencyKey, RequestFingerprint,
    RequestScope, RecoveryBinding, Response, ResponseStream, UhpError, IDEMPOTENCY_KEY_HEADER,
    UHP_VERSION, UHP_VERSION_HEADER,
};

#[derive(Clone)]
pub struct Client {
    base_url: Url,
    bearer_token: Option<Arc<str>>,
    http: reqwest::Client,
    idempotency: Arc<Mutex<HashMap<String, RequestFingerprint>>>,
}

impl Client {
    pub fn new(base_url: &str, bearer_token: Option<String>) -> Result<Self, UhpError> {
        let mut normalized = base_url.to_string();
        if !normalized.ends_with('/') {
            normalized.push('/');
        }
        let base_url =
            Url::parse(&normalized).map_err(|error| UhpError::InvalidBaseUrl(error.to_string()))?;
        if base_url.cannot_be_a_base() {
            return Err(UhpError::InvalidBaseUrl(
                "URL cannot contain path segments".to_string(),
            ));
        }
        let http = reqwest::Client::builder()
            .build()
            .map_err(|source| UhpError::Transport {
                operation: "build HTTP client",
                source,
            })?;
        Ok(Self {
            base_url,
            bearer_token: bearer_token.map(Arc::from),
            http,
            idempotency: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn discover(&self) -> Result<Discovery, UhpError> {
        let request = self
            .http
            .request(Method::GET, self.endpoint(&["v1", "uhp"])?)
            .header(UHP_VERSION_HEADER, UHP_VERSION);
        let discovery: Discovery = self.execute(request, "discovery").await?;
        validate_discovery(&discovery)?;
        Ok(discovery)
    }

    pub async fn create_response(
        &self,
        request: &CreateResponse,
        idempotency_key: &IdempotencyKey,
        scope: &RequestScope,
    ) -> Result<Response, UhpError> {
        request.validate(false)?;
        let fingerprint = request.fingerprint(scope)?;
        self.register_idempotency(idempotency_key, fingerprint)?;
        let builder = self.response_request(request, idempotency_key)?;
        let response: Response = self.execute(builder, "create response").await?;
        validate_response(&response, "create response")?;
        Ok(response)
    }

    pub async fn create_response_stream(
        &self,
        request: &CreateResponse,
        idempotency_key: &IdempotencyKey,
        scope: &RequestScope,
    ) -> Result<ResponseStream, UhpError> {
        request.validate(true)?;
        let fingerprint = request.fingerprint(scope)?;
        self.register_idempotency(idempotency_key, fingerprint)?;
        let builder = self.response_request(request, idempotency_key)?;
        let response = builder.send().await.map_err(|source| UhpError::Transport {
            operation: "create streaming response",
            source,
        })?;
        validate_version(response.headers())?;
        if !response.status().is_success() {
            return Err(decode_server_error(response).await?);
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type.starts_with("text/event-stream") {
            return Err(UhpError::InvalidResponse {
                operation: "create streaming response",
                message: "expected Content-Type text/event-stream".to_string(),
            });
        }
        Ok(ResponseStream::new(response))
    }

    pub async fn recover_response(
        &self,
        request: &CreateResponse,
        idempotency_key: &IdempotencyKey,
        scope: &RequestScope,
        binding: &RecoveryBinding,
    ) -> Result<Response, UhpError> {
        let actual = request.fingerprint(scope)?;
        if actual != binding.request_fingerprint {
            return Err(UhpError::FingerprintConflict {
                expected: binding.request_fingerprint.clone(),
                actual,
            });
        }
        self.register_idempotency(idempotency_key, binding.request_fingerprint.clone())?;
        if let Some(response_id) = &binding.response_id {
            return self.get_response(response_id).await;
        }
        if request.is_streaming() {
            let stream = self
                .create_response_stream(request, idempotency_key, scope)
                .await?;
            return Ok(stream.complete().await?);
        }
        self.create_response(request, idempotency_key, scope).await
    }

    pub async fn get_response(&self, response_id: &str) -> Result<Response, UhpError> {
        let request = self.authenticated(
            Method::GET,
            self.endpoint(&["v1", "responses", response_id])?,
        );
        let response: Response = self.execute(request, "get stored response").await?;
        validate_response(&response, "get stored response")?;
        Ok(response)
    }

    pub async fn cancel_response(&self, response_id: &str) -> Result<Response, UhpError> {
        let request = self.authenticated(
            Method::POST,
            self.endpoint(&["v1", "responses", response_id, "cancel"])?,
        );
        let response: Response = self.execute(request, "cancel response").await?;
        validate_response(&response, "cancel response")?;
        Ok(response)
    }

    pub async fn cancel_session(&self, session_id: &str) -> Result<(), UhpError> {
        let request = self.authenticated(
            Method::POST,
            self.endpoint(&["v1", "sessions", session_id, "cancel"])?,
        );
        let response = request.send().await.map_err(|source| UhpError::Transport {
            operation: "cancel session",
            source,
        })?;
        validate_version(response.headers())?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(decode_server_error(response).await?)
        }
    }

    fn response_request(
        &self,
        request: &CreateResponse,
        idempotency_key: &IdempotencyKey,
    ) -> Result<RequestBuilder, UhpError> {
        let body = serde_json::to_vec(request).map_err(|source| UhpError::Schema {
            operation: "serialize response request",
            source,
        })?;
        Ok(self
            .authenticated(Method::POST, self.endpoint(&["v1", "responses"])?)
            .header(IDEMPOTENCY_KEY_HEADER, idempotency_key.as_str())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body))
    }

    fn authenticated(&self, method: Method, url: Url) -> RequestBuilder {
        let request = self
            .http
            .request(method, url)
            .header(UHP_VERSION_HEADER, UHP_VERSION);
        match &self.bearer_token {
            Some(token) => request.header(AUTHORIZATION, format!("Bearer {token}")),
            None => request,
        }
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url, UhpError> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|_| UhpError::InvalidBaseUrl("URL cannot contain path segments".to_string()))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    fn register_idempotency(
        &self,
        key: &IdempotencyKey,
        fingerprint: RequestFingerprint,
    ) -> Result<(), UhpError> {
        let mut registry = self
            .idempotency
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(expected) = registry.get(key.as_str()) {
            if expected != &fingerprint {
                return Err(UhpError::FingerprintConflict {
                    expected: expected.clone(),
                    actual: fingerprint,
                });
            }
            return Ok(());
        }
        registry.insert(key.as_str().to_string(), fingerprint);
        Ok(())
    }

    async fn execute<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &'static str,
    ) -> Result<T, UhpError> {
        let response = request
            .send()
            .await
            .map_err(|source| UhpError::Transport { operation, source })?;
        validate_version(response.headers())?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|source| UhpError::Transport { operation, source })?;
        if !status.is_success() {
            let envelope: ErrorEnvelope =
                serde_json::from_slice(&body).map_err(|source| UhpError::Schema {
                    operation: "decode error envelope",
                    source,
                })?;
            return Err(UhpError::Server {
                status: status.as_u16(),
                error: envelope.error,
            });
        }
        serde_json::from_slice(&body).map_err(|source| UhpError::Schema { operation, source })
    }
}

async fn decode_server_error(response: reqwest::Response) -> Result<UhpError, UhpError> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|source| UhpError::Transport {
            operation: "read error envelope",
            source,
        })?;
    let envelope: ErrorEnvelope =
        serde_json::from_slice(&body).map_err(|source| UhpError::Schema {
            operation: "decode error envelope",
            source,
        })?;
    Ok(UhpError::Server {
        status: status.as_u16(),
        error: envelope.error,
    })
}

fn validate_version(headers: &HeaderMap) -> Result<(), UhpError> {
    let actual = headers
        .get(UHP_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    if actual.as_deref() == Some(UHP_VERSION) {
        Ok(())
    } else {
        Err(UhpError::VersionMismatch {
            expected: UHP_VERSION,
            actual,
        })
    }
}

fn validate_discovery(discovery: &Discovery) -> Result<(), UhpError> {
    if discovery.object != "uhp.discovery" || discovery.protocol != "uhp" {
        return Err(UhpError::InvalidResponse {
            operation: "discovery",
            message: "expected object uhp.discovery and protocol uhp".to_string(),
        });
    }
    if !discovery
        .versions
        .iter()
        .any(|version| version == UHP_VERSION)
    {
        return Err(UhpError::UnsupportedProtocolVersion {
            expected: UHP_VERSION,
            supported: discovery.versions.clone(),
        });
    }
    if discovery.versions.is_empty() || !discovery.versions.contains(&discovery.default_version) {
        return Err(UhpError::InvalidResponse {
            operation: "discovery",
            message: "default_version must appear in non-empty versions".to_string(),
        });
    }
    if discovery.conformance_class == ConformanceClass::Extended
        && (!discovery.capabilities.files_input
            || !discovery.capabilities.files_output
            || !discovery.capabilities.session_listing)
    {
        return Err(UhpError::InvalidResponse {
            operation: "discovery",
            message: "extended conformance requires files and session listing capabilities"
                .to_string(),
        });
    }
    Ok(())
}

fn validate_response(response: &Response, operation: &'static str) -> Result<(), UhpError> {
    if response.object != "response" {
        return Err(UhpError::InvalidResponse {
            operation,
            message: "expected object response".to_string(),
        });
    }
    Ok(())
}
