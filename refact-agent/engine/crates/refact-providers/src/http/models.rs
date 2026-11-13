use serde::{Deserialize, Serialize};

use crate::google_gemini::GoogleGeminiHealthInfo;
use crate::openrouter::OpenRouterHealthInfo;
use crate::traits::{AvailableModel, CustomModelConfig, ProviderModel};

#[derive(Serialize)]
pub struct SimplifiedModel {
    pub name: String,
    pub enabled: bool,
    pub removable: bool,
    pub user_configured: bool,
}

#[derive(Serialize)]
pub struct ModelsResponse {
    pub chat_models: Vec<SimplifiedModel>,
    pub completion_models: Vec<SimplifiedModel>,
    pub embedding_model: Option<SimplifiedModel>,
}

impl From<&ProviderModel> for SimplifiedModel {
    fn from(model: &ProviderModel) -> Self {
        SimplifiedModel {
            name: model.id.clone(),
            enabled: model.enabled,
            removable: model.removable,
            user_configured: model.user_configured,
        }
    }
}

#[derive(Deserialize)]
pub struct ModelsQueryParams {
    #[serde(rename = "provider-name")]
    pub provider_name: String,
}

#[derive(Serialize)]
pub struct AvailableModelsResponse {
    pub models: Vec<AvailableModel>,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct OpenRouterHealthResponse {
    pub ok: bool,
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<OpenRouterHealthInfo>,
}

#[derive(Serialize)]
pub struct GoogleGeminiHealthResponse {
    pub ok: bool,
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<GoogleGeminiHealthInfo>,
}

#[derive(Deserialize)]
pub struct ModelToggleRequest {
    pub model_id: String,
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct ModelProviderRequest {
    pub model_id: String,
    #[serde(default)]
    pub selected_provider: Option<String>,
}

#[derive(Deserialize)]
pub struct AddCustomModelRequest {
    pub id: String,
    #[serde(flatten)]
    pub config: CustomModelConfig,
}

#[derive(Deserialize)]
pub struct RemoveCustomModelRequest {
    pub model_id: String,
}
