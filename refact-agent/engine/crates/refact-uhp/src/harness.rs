use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, ETAG, IF_MATCH};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    request_fingerprint, Client, Discovery, IdempotencyKey, RequestFingerprint, RequestScope,
    UhpError, IDEMPOTENCY_KEY_HEADER,
};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Harness {
    pub id: String,
    #[serde(default)]
    pub object: Option<String>,
    pub name: String,
    pub base: String,
    #[serde(default)]
    pub base_label: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Value>,
    #[serde(default)]
    pub skills: Vec<Value>,
    #[serde(default)]
    pub plugins: Vec<Value>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    #[serde(default)]
    pub max_step: Option<u64>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct HarnessConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_step: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl HarnessConfig {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            ..Self::default()
        }
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = Some(model.into());
        self
    }

    pub fn environment(mut self, environment: impl Into<String>) -> Self {
        self.environment = Some(environment.into());
        self
    }

    pub fn fingerprint(&self, scope: &RequestScope) -> Result<RequestFingerprint, UhpError> {
        let body = serde_json::to_value(self).map_err(|source| UhpError::Schema {
            operation: "serialize harness request",
            source,
        })?;
        request_fingerprint("POST", "/v1/harnesses", scope, body)
    }
}

impl From<&Harness> for HarnessConfig {
    fn from(harness: &Harness) -> Self {
        Self {
            name: Some(harness.name.clone()),
            base: harness.base.clone(),
            default_model: harness.default_model.clone(),
            system_prompt: harness.system_prompt.clone(),
            mcp_servers: harness.mcp_servers.clone(),
            skills: harness.skills.clone(),
            plugins: harness.plugins.clone(),
            environment: harness.environment.clone(),
            disabled_tools: harness.disabled_tools.clone(),
            max_step: harness.max_step,
            timeout_seconds: harness.timeout_seconds,
            extra: harness.extra.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct HarnessList {
    pub harnesses: Vec<Harness>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct VersionedHarness {
    pub harness: Harness,
    pub etag: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Model {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub backend: Option<String>,
    pub available: bool,
    #[serde(default)]
    pub default: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct BackendModels {
    #[serde(rename = "default")]
    pub default_model: Option<String>,
    #[serde(default)]
    pub models: Vec<Model>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ModelCatalog {
    pub backends: BTreeMap<String, BackendModels>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct HarnessModels {
    #[serde(default)]
    pub harness_id: Option<String>,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(rename = "default")]
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub fallback: Option<String>,
    pub models: Vec<Model>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl HarnessModels {
    pub fn model(&self, model_id: &str) -> Option<&Model> {
        self.models.iter().find(|model| model.id == model_id)
    }

    pub fn require_available(&self, model_id: &str) -> Result<&Model, UhpError> {
        self.model(model_id)
            .filter(|model| model.available)
            .ok_or_else(|| UhpError::ModelUnavailable {
                harness_id: self.harness_id.clone(),
                model: model_id.to_string(),
            })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetiredSessions {
    #[default]
    Kept,
    Retired,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct HarnessRetirement {
    #[serde(default)]
    pub id: Option<String>,
    pub deleted: bool,
    #[serde(default)]
    pub sessions: RetiredSessions,
    #[serde(rename = "restorableUntil")]
    #[serde(default)]
    pub restorable_until: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Client {
    pub async fn list_harnesses(&self) -> Result<Vec<Harness>, UhpError> {
        let request = self.authenticated(Method::GET, self.endpoint(&["v1", "harnesses"])?);
        let list: HarnessList = self.execute(request, "list harnesses").await?;
        Ok(list.harnesses)
    }

    pub async fn get_harness(&self, harness_id: &str) -> Result<VersionedHarness, UhpError> {
        let request = self.authenticated(
            Method::GET,
            self.endpoint(&["v1", "harnesses", harness_id])?,
        );
        let (harness, headers) = self.execute_with_headers(request, "get harness").await?;
        Ok(VersionedHarness {
            harness,
            etag: etag(&headers),
        })
    }

    pub async fn list_models(&self) -> Result<ModelCatalog, UhpError> {
        let request = self.authenticated(Method::GET, self.endpoint(&["v1", "models"])?);
        self.execute(request, "list models").await
    }

    pub async fn list_harness_models(&self, harness_id: &str) -> Result<HarnessModels, UhpError> {
        let request = self.authenticated(
            Method::GET,
            self.endpoint(&["v1", "harnesses", harness_id, "models"])?,
        );
        self.execute(request, "list harness models").await
    }

    pub async fn require_harness_model(
        &self,
        harness_id: &str,
        model_id: &str,
    ) -> Result<Model, UhpError> {
        let models = self.list_harness_models(harness_id).await?;
        if models.harness_id.as_deref() != Some(harness_id) {
            return Err(UhpError::InvalidResponse {
                operation: "list harness models",
                message: "harness_id does not match the requested harness".to_string(),
            });
        }
        models.require_available(model_id).cloned()
    }

    pub async fn create_harness(
        &self,
        discovery: &Discovery,
        config: &HarnessConfig,
        idempotency_key: &IdempotencyKey,
        scope: &RequestScope,
    ) -> Result<Harness, UhpError> {
        require_capability(discovery, "harness_management", "create harness")?;
        require_capability(discovery, "harness_idempotency", "create harness")?;
        if config.environment.is_some() {
            require_capability(discovery, "environments", "create harness")?;
        }
        self.register_harness_idempotency(idempotency_key, config.fingerprint(scope)?)?;
        let body = serde_json::to_vec(config).map_err(|source| UhpError::Schema {
            operation: "serialize harness request",
            source,
        })?;
        let request = self
            .authenticated(Method::POST, self.endpoint(&["v1", "harnesses"])?)
            .header(IDEMPOTENCY_KEY_HEADER, idempotency_key.as_str())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        self.execute(request, "create harness")
            .await
            .map_err(|error| map_harness_error(error, config))
    }

    pub async fn update_harness(
        &self,
        discovery: &Discovery,
        harness_id: &str,
        config: &HarnessConfig,
        if_match: Option<&str>,
    ) -> Result<VersionedHarness, UhpError> {
        require_capability(discovery, "harness_management", "update harness")?;
        if config.environment.is_some() {
            require_capability(discovery, "environments", "update harness")?;
        }
        let body = serde_json::to_vec(config).map_err(|source| UhpError::Schema {
            operation: "serialize harness update",
            source,
        })?;
        let mut request = self
            .authenticated(
                Method::PUT,
                self.endpoint(&["v1", "harnesses", harness_id])?,
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        if let Some(if_match) = if_match {
            request = request.header(IF_MATCH, if_match);
        }
        let result = self.execute_with_headers(request, "update harness").await;
        match result {
            Ok((harness, headers)) => Ok(VersionedHarness {
                harness,
                etag: etag(&headers),
            }),
            Err(UhpError::Server { status: 412, .. }) => Err(UhpError::HarnessPreconditionFailed {
                harness_id: harness_id.to_string(),
            }),
            Err(error) => Err(map_harness_error(error, config)),
        }
    }

    pub async fn retire_harness(
        &self,
        discovery: &Discovery,
        harness_id: &str,
    ) -> Result<HarnessRetirement, UhpError> {
        require_capability(discovery, "harness_management", "retire harness")?;
        let request = self.authenticated(
            Method::DELETE,
            self.endpoint(&["v1", "harnesses", harness_id])?,
        );
        let retirement: HarnessRetirement = self.execute(request, "retire harness").await?;
        if retirement.sessions == RetiredSessions::Retired && retirement.restorable_until.is_none()
        {
            return Err(UhpError::InvalidResponse {
                operation: "retire harness",
                message: "retired sessions require restorableUntil".to_string(),
            });
        }
        if !discovery.capabilities.harness_retirement
            && retirement.sessions == RetiredSessions::Retired
        {
            return Err(UhpError::InvalidResponse {
                operation: "retire harness",
                message: "server retired sessions without harness_retirement capability"
                    .to_string(),
            });
        }
        Ok(retirement)
    }
}

pub(crate) fn require_capability(
    discovery: &Discovery,
    capability: &'static str,
    operation: &'static str,
) -> Result<(), UhpError> {
    if discovery.capability(capability) {
        Ok(())
    } else {
        Err(UhpError::CapabilityUnsupported {
            capability,
            operation,
        })
    }
}

fn map_harness_error(error: UhpError, config: &HarnessConfig) -> UhpError {
    match &error {
        UhpError::Server { error: object, .. } if object.code == "unsupported_base" => {
            UhpError::UnsupportedBase {
                base: config.base.clone(),
            }
        }
        UhpError::Server { error: object, .. }
            if matches!(
                object.code.as_str(),
                "model_unavailable" | "model_not_available"
            ) =>
        {
            UhpError::ModelUnavailable {
                harness_id: None,
                model: config.default_model.clone().unwrap_or_default(),
            }
        }
        _ => error,
    }
}

fn etag(headers: &HeaderMap) -> Option<String> {
    headers
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}
