use std::collections::BTreeMap;
use std::pin::Pin;

use futures::Stream;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{harness::require_capability, Client, Discovery, UhpError};

pub type EnvironmentByteStream =
    Pin<Box<dyn Stream<Item = Result<Vec<u8>, std::io::Error>> + Send + 'static>>;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentStatus {
    Empty,
    Building,
    Ready,
    Failed,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentBuildStatus {
    Building,
    Ready,
    Failed,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct EnvironmentFiles {
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub bytes: u64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct EnvironmentPackage {
    pub manager: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct DeclaredPackage {
    pub spec: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct EnvironmentRuntime {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct EnvironmentVersion {
    pub version: u64,
    pub status: EnvironmentBuildStatus,
    #[serde(default)]
    pub started_at: Option<u64>,
    #[serde(default)]
    pub finished_at: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub files: Option<u64>,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub packages: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for EnvironmentBuildStatus {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct EnvironmentBuild {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub object: Option<String>,
    pub version: u64,
    pub status: EnvironmentBuildStatus,
    #[serde(default)]
    pub started_at: Option<u64>,
    #[serde(default)]
    pub finished_at: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub log: Option<String>,
    #[serde(default)]
    pub packages: Vec<EnvironmentPackage>,
    #[serde(default)]
    pub files: Option<u64>,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Environment {
    pub id: String,
    #[serde(default)]
    pub object: Option<String>,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub entry: Option<String>,
    pub status: EnvironmentStatus,
    pub mount: String,
    #[serde(default)]
    pub version: Option<u64>,
    #[serde(rename = "latestVersion")]
    #[serde(default)]
    pub latest_version: Option<u64>,
    #[serde(default)]
    pub files: Option<EnvironmentFiles>,
    #[serde(default)]
    pub packages: Vec<EnvironmentPackage>,
    #[serde(default)]
    pub declared: BTreeMap<String, Vec<DeclaredPackage>>,
    #[serde(default)]
    pub runtime: Option<EnvironmentRuntime>,
    #[serde(default)]
    pub versions: Vec<EnvironmentVersion>,
    #[serde(default)]
    pub build: Option<EnvironmentVersion>,
    #[serde(rename = "createdAt")]
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(rename = "updatedAt")]
    #[serde(default)]
    pub updated_at: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Environment {
    pub fn active_version(&self) -> Option<&EnvironmentVersion> {
        let active = self.version?;
        self.versions
            .iter()
            .find(|version| version.version == active)
    }

    pub fn ensure_ready(&self) -> Result<(), UhpError> {
        let ready = self.active_version().is_some_and(|version| {
            version.status == EnvironmentBuildStatus::Ready
                && version.error.as_deref().map_or(true, str::is_empty)
        });
        if ready {
            Ok(())
        } else {
            Err(UhpError::EnvironmentNotReady {
                environment_id: self.id.clone(),
                status: Some(self.status.clone()),
                version: self.version,
            })
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct EnvironmentPackageSpecs {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pip: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub npm: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apt: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct EnvironmentConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packages: Option<EnvironmentPackageSpecs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<EnvironmentRuntime>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl EnvironmentConfig {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn entry(mut self, entry: impl Into<String>) -> Self {
        self.entry = Some(entry.into());
        self
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct EnvironmentGit {
    pub url: String,
    #[serde(rename = "ref")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct EnvironmentGitImport {
    pub git: EnvironmentGit,
    #[serde(default)]
    pub replace: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl EnvironmentGitImport {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            git: EnvironmentGit {
                url: url.into(),
                reference: None,
                extra: BTreeMap::new(),
            },
            ..Self::default()
        }
    }

    pub fn reference(mut self, reference: impl Into<String>) -> Self {
        self.git.reference = Some(reference.into());
        self
    }

    pub fn replace(mut self) -> Self {
        self.replace = true;
        self
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct EnvironmentImportResult {
    #[serde(default)]
    pub written: Option<u64>,
    #[serde(default)]
    pub skipped: Option<u64>,
    #[serde(default)]
    pub count: Option<u64>,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct EnvironmentVersions {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub active: Option<u64>,
    pub versions: Vec<EnvironmentVersion>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct EnvironmentList {
    environments: Vec<Environment>,
}

impl Client {
    pub async fn create_environment(
        &self,
        discovery: &Discovery,
        config: &EnvironmentConfig,
    ) -> Result<Environment, UhpError> {
        require_capability(discovery, "environments", "create environment")?;
        let request = self
            .authenticated(Method::POST, self.endpoint(&["v1", "environments"])?)
            .json(config);
        self.execute(request, "create environment").await
    }

    pub async fn list_environments(
        &self,
        discovery: &Discovery,
    ) -> Result<Vec<Environment>, UhpError> {
        require_capability(discovery, "environments", "list environments")?;
        let request = self.authenticated(Method::GET, self.endpoint(&["v1", "environments"])?);
        let list: EnvironmentList = self.execute(request, "list environments").await?;
        Ok(list.environments)
    }

    pub async fn get_environment(
        &self,
        discovery: &Discovery,
        environment_id: &str,
    ) -> Result<Environment, UhpError> {
        require_capability(discovery, "environments", "get environment")?;
        let request = self.authenticated(
            Method::GET,
            self.endpoint(&["v1", "environments", environment_id])?,
        );
        self.execute(request, "get environment")
            .await
            .map_err(|error| map_environment_error(error, environment_id))
    }

    pub async fn get_ready_environment(
        &self,
        discovery: &Discovery,
        environment_id: &str,
    ) -> Result<Environment, UhpError> {
        let environment = self.get_environment(discovery, environment_id).await?;
        environment.ensure_ready()?;
        Ok(environment)
    }

    pub async fn import_environment_bytes(
        &self,
        discovery: &Discovery,
        environment_id: &str,
        bytes: Vec<u8>,
        content_type: &str,
        replace: bool,
    ) -> Result<EnvironmentImportResult, UhpError> {
        self.import_environment_body(
            discovery,
            environment_id,
            reqwest::Body::from(bytes),
            content_type,
            replace,
        )
        .await
    }

    pub async fn import_environment_stream(
        &self,
        discovery: &Discovery,
        environment_id: &str,
        stream: EnvironmentByteStream,
        content_type: &str,
        replace: bool,
    ) -> Result<EnvironmentImportResult, UhpError> {
        self.import_environment_body(
            discovery,
            environment_id,
            reqwest::Body::wrap_stream(stream),
            content_type,
            replace,
        )
        .await
    }

    async fn import_environment_body(
        &self,
        discovery: &Discovery,
        environment_id: &str,
        body: reqwest::Body,
        content_type: &str,
        replace: bool,
    ) -> Result<EnvironmentImportResult, UhpError> {
        require_capability(discovery, "environments", "import environment")?;
        if content_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        {
            return Err(UhpError::InvalidResponse {
                operation: "import environment archive",
                message: "archive Content-Type must not be application/json".to_string(),
            });
        }
        let mut url = self.endpoint(&["v1", "environments", environment_id, "import"])?;
        url.query_pairs_mut()
            .append_pair("replace", if replace { "1" } else { "0" });
        let request = self
            .authenticated(Method::POST, url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body);
        self.execute(request, "import environment archive")
            .await
            .map_err(|error| map_environment_error(error, environment_id))
    }

    pub async fn import_environment_git(
        &self,
        discovery: &Discovery,
        environment_id: &str,
        import: &EnvironmentGitImport,
    ) -> Result<EnvironmentImportResult, UhpError> {
        require_capability(discovery, "environments", "import environment")?;
        let request = self
            .authenticated(
                Method::POST,
                self.endpoint(&["v1", "environments", environment_id, "import"])?,
            )
            .json(import);
        self.execute(request, "import environment git")
            .await
            .map_err(|error| map_environment_error(error, environment_id))
    }

    pub async fn build_environment(
        &self,
        discovery: &Discovery,
        environment_id: &str,
    ) -> Result<EnvironmentBuild, UhpError> {
        require_capability(discovery, "environments", "build environment")?;
        let request = self.authenticated(
            Method::POST,
            self.endpoint(&["v1", "environments", environment_id, "build"])?,
        );
        self.execute(request, "build environment")
            .await
            .map_err(|error| map_environment_error(error, environment_id))
    }

    pub async fn get_environment_build(
        &self,
        discovery: &Discovery,
        environment_id: &str,
        version: u64,
    ) -> Result<EnvironmentBuild, UhpError> {
        require_capability(discovery, "environments", "get environment build")?;
        let version = version.to_string();
        let request = self.authenticated(
            Method::GET,
            self.endpoint(&["v1", "environments", environment_id, "builds", &version])?,
        );
        self.execute(request, "get environment build")
            .await
            .map_err(|error| map_environment_error(error, environment_id))
    }

    pub async fn list_environment_versions(
        &self,
        discovery: &Discovery,
        environment_id: &str,
    ) -> Result<EnvironmentVersions, UhpError> {
        require_capability(discovery, "environments", "list environment versions")?;
        let request = self.authenticated(
            Method::GET,
            self.endpoint(&["v1", "environments", environment_id, "versions"])?,
        );
        self.execute(request, "list environment versions")
            .await
            .map_err(|error| map_environment_error(error, environment_id))
    }

    pub async fn activate_environment_version(
        &self,
        discovery: &Discovery,
        environment_id: &str,
        version: u64,
    ) -> Result<Environment, UhpError> {
        require_capability(discovery, "environments", "activate environment version")?;
        let version_string = version.to_string();
        let request = self.authenticated(
            Method::POST,
            self.endpoint(&[
                "v1",
                "environments",
                environment_id,
                "versions",
                &version_string,
                "activate",
            ])?,
        );
        let environment: Environment = self
            .execute(request, "activate environment version")
            .await
            .map_err(|error| map_environment_error(error, environment_id))?;
        if environment.version != Some(version) {
            return Err(UhpError::InvalidResponse {
                operation: "activate environment version",
                message: "active version does not match the requested version".to_string(),
            });
        }
        environment.ensure_ready()?;
        Ok(environment)
    }
}

fn map_environment_error(error: UhpError, environment_id: &str) -> UhpError {
    match error {
        UhpError::Server { status, error } => match error.code.as_str() {
            "environment_not_found" => UhpError::EnvironmentNotFound {
                environment_id: environment_id.to_string(),
            },
            "environment_not_ready" => UhpError::EnvironmentNotReady {
                environment_id: environment_id.to_string(),
                status: None,
                version: None,
            },
            "environment_busy" => UhpError::EnvironmentBusy {
                environment_id: environment_id.to_string(),
            },
            "environment_unavailable" => UhpError::EnvironmentUnavailable {
                environment_id: environment_id.to_string(),
                status,
            },
            _ => UhpError::Server { status, error },
        },
        other => other,
    }
}
