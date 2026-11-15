use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::global_context::GlobalContext;
use crate::integr_abstract::IntegrationTrait;
use crate::integrations::notifier::NotifierBackend;
use refact_integrations::integr_abstract::IntegrationCommon;

pub const INTEGRATION_ID: &str = "notifier_telegram";
const TELEGRAM_API_BASE: &str = "https://api.telegram.org";
const TELEGRAM_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct TelegramNotifierConfig {
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub default_chat_id: String,
    #[cfg(any(test, feature = "test-util"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_for_test: Option<String>,
}

#[derive(Default)]
pub struct IntegrationNotifierTelegram {
    pub common: IntegrationCommon,
    pub cfg: TelegramNotifierConfig,
    pub config_path: String,
}

#[async_trait]
impl IntegrationTrait for IntegrationNotifierTelegram {
    async fn integr_settings_apply(
        &mut self,
        _gcx: Arc<GlobalContext>,
        config_path: String,
        value: &serde_json::Value,
    ) -> Result<(), serde_json::Error> {
        self.cfg = serde_json::from_value(value.clone())?;
        self.common = serde_json::from_value(value.clone())?;
        self.config_path = config_path;
        Ok(())
    }

    fn integr_settings_as_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.cfg).unwrap()
    }

    fn integr_common(&self) -> IntegrationCommon {
        self.common.clone()
    }

    async fn integr_tools(
        &self,
        _integr_name: &str,
    ) -> Vec<Box<dyn crate::Tool + Send>> {
        vec![]
    }

    fn integr_schema(&self) -> &str {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../src/integrations/notifier/notifier_telegram_schema.yaml"
        ))
    }
}

pub async fn backend_from_config(
    gcx: Arc<GlobalContext>,
    config_path: String,
    value: &serde_json::Value,
) -> Result<Box<dyn NotifierBackend>, String> {
    let mut integration = IntegrationNotifierTelegram::default();
    integration
        .integr_settings_apply(gcx.clone(), config_path, value)
        .await
        .map_err(|error| format!("failed to apply Telegram notifier settings: {error}"))?;
    let notifier = TelegramNotifier::new(integration.cfg, gcx.http_client.clone());
    #[cfg(any(test, feature = "test-util"))]
    {
        let mut notifier = notifier;
        if let Some(api_base) = notifier.cfg.api_base_for_test.clone() {
            notifier.api_base = api_base;
        }
        return Ok(Box::new(notifier));
    }
    #[cfg(not(any(test, feature = "test-util")))]
    Ok(Box::new(notifier))
}

pub struct TelegramNotifier {
    cfg: TelegramNotifierConfig,
    client: reqwest::Client,
    api_base: String,
}

impl TelegramNotifier {
    pub fn new(cfg: TelegramNotifierConfig, client: reqwest::Client) -> Self {
        Self::with_api_base(cfg, client, TELEGRAM_API_BASE.to_string())
    }

    pub fn with_api_base(
        cfg: TelegramNotifierConfig,
        client: reqwest::Client,
        api_base: String,
    ) -> Self {
        Self {
            cfg,
            client,
            api_base,
        }
    }

    fn endpoint(&self, token: &str) -> String {
        format!(
            "{}/bot{token}/sendMessage",
            self.api_base.trim_end_matches('/'),
        )
    }
}

#[async_trait]
impl NotifierBackend for TelegramNotifier {
    async fn send(&self, target: Option<&str>, text: &str) -> Result<(), String> {
        let token = self.cfg.bot_token.trim();
        if token.is_empty() {
            return Err("Telegram bot token is required".to_string());
        }
        let chat_id = target
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .unwrap_or_else(|| self.cfg.default_chat_id.trim());
        if chat_id.is_empty() {
            return Err("Telegram chat id is required".to_string());
        }

        let request = self
            .client
            .post(self.endpoint(token))
            .json(&json!({"chat_id": chat_id, "text": text}))
            .timeout(TELEGRAM_TIMEOUT);
        let response = tokio::time::timeout(TELEGRAM_TIMEOUT, request.send())
            .await
            .map_err(|_| "Telegram notifier request timed out".to_string())?
            .map_err(|error| redact_token(&format!("Telegram notifier failed: {error}"), token))?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(redact_token(
            &format!(
                "Telegram notifier returned {status}: {}",
                body.chars().take(500).collect::<String>()
            ),
            token,
        ))
    }
}

fn redact_token(text: &str, token: &str) -> String {
    if token.is_empty() {
        return text.to_string();
    }
    text.replace(token, "[REDACTED]")
}
