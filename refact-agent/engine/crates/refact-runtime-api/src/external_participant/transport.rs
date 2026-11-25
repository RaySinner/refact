use async_trait::async_trait;
use refact_uhp::{
    Client, CreateResponse, Discovery, Harness, HarnessConfig, HarnessRetirement, IdempotencyKey,
    Model, RecoveryBinding, RequestScope, Response, UhpError, VersionedHarness,
};

use super::ExternalCancelTarget;

#[derive(Clone)]
pub struct ExternalConnection {
    pub server_root: String,
    bearer_token: Option<String>,
}

impl ExternalConnection {
    pub fn new(server_root: String, bearer_token: Option<String>) -> Self {
        Self {
            server_root,
            bearer_token,
        }
    }

    fn client(&self) -> Result<Client, UhpError> {
        Client::new(&self.server_root, self.bearer_token.clone())
    }
}

#[async_trait]
pub trait ExternalParticipantTransport: Send + Sync {
    async fn discover(&self, connection: &ExternalConnection) -> Result<Discovery, UhpError>;

    async fn test_model(
        &self,
        connection: &ExternalConnection,
        harness_id: &str,
        model: &str,
    ) -> Result<Model, UhpError>;

    async fn provision(
        &self,
        connection: &ExternalConnection,
        discovery: &Discovery,
        config: &HarnessConfig,
        key: &IdempotencyKey,
        scope: &RequestScope,
    ) -> Result<Harness, UhpError>;

    async fn update(
        &self,
        connection: &ExternalConnection,
        discovery: &Discovery,
        harness_id: &str,
        config: &HarnessConfig,
        if_match: Option<&str>,
    ) -> Result<VersionedHarness, UhpError>;

    async fn cancel(
        &self,
        connection: &ExternalConnection,
        target: &ExternalCancelTarget,
    ) -> Result<Option<Response>, UhpError>;

    async fn retry(
        &self,
        connection: &ExternalConnection,
        request: &CreateResponse,
        key: &IdempotencyKey,
        scope: &RequestScope,
        binding: &RecoveryBinding,
    ) -> Result<Response, UhpError>;

    async fn retire(
        &self,
        connection: &ExternalConnection,
        discovery: &Discovery,
        harness_id: &str,
    ) -> Result<HarnessRetirement, UhpError>;
}

#[derive(Clone, Debug, Default)]
pub struct UhpParticipantTransport;

#[async_trait]
impl ExternalParticipantTransport for UhpParticipantTransport {
    async fn discover(&self, connection: &ExternalConnection) -> Result<Discovery, UhpError> {
        connection.client()?.discover().await
    }

    async fn test_model(
        &self,
        connection: &ExternalConnection,
        harness_id: &str,
        model: &str,
    ) -> Result<Model, UhpError> {
        connection
            .client()?
            .require_harness_model(harness_id, model)
            .await
    }

    async fn provision(
        &self,
        connection: &ExternalConnection,
        discovery: &Discovery,
        config: &HarnessConfig,
        key: &IdempotencyKey,
        scope: &RequestScope,
    ) -> Result<Harness, UhpError> {
        connection
            .client()?
            .create_harness(discovery, config, key, scope)
            .await
    }

    async fn update(
        &self,
        connection: &ExternalConnection,
        discovery: &Discovery,
        harness_id: &str,
        config: &HarnessConfig,
        if_match: Option<&str>,
    ) -> Result<VersionedHarness, UhpError> {
        connection
            .client()?
            .update_harness(discovery, harness_id, config, if_match)
            .await
    }

    async fn cancel(
        &self,
        connection: &ExternalConnection,
        target: &ExternalCancelTarget,
    ) -> Result<Option<Response>, UhpError> {
        let client = connection.client()?;
        match target {
            ExternalCancelTarget::Response(id) => client.cancel_response(id).await.map(Some),
            ExternalCancelTarget::Session(id) => client.cancel_session(id).await.map(|_| None),
        }
    }

    async fn retry(
        &self,
        connection: &ExternalConnection,
        request: &CreateResponse,
        key: &IdempotencyKey,
        scope: &RequestScope,
        binding: &RecoveryBinding,
    ) -> Result<Response, UhpError> {
        connection
            .client()?
            .recover_response(request, key, scope, binding)
            .await
    }

    async fn retire(
        &self,
        connection: &ExternalConnection,
        discovery: &Discovery,
        harness_id: &str,
    ) -> Result<HarnessRetirement, UhpError> {
        connection
            .client()?
            .retire_harness(discovery, harness_id)
            .await
    }
}
