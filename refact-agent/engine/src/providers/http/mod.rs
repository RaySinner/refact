#![allow(unused_imports)]

use axum::extract::{Path, Query, State};
use axum::http::{Response, StatusCode};
use hyper::Body;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use crate::app_state::AppState;
use refact_runtime_api::GlobalContextCtx;
use crate::buddy::drafts::{draft_kind_str, DraftTarget, DraftValidationError};
use crate::buddy::types::DraftKind;
use crate::caps::model_caps::get_model_caps;
use crate::chat::ctx::ChatCtx;
use crate::custom_error::ScratchError;
use crate::global_context::GlobalContext;
use tokio::sync::Mutex as AMutex;
use crate::providers::config::ProviderDefaults;
use refact_core::provider_types::{ProjectModelDefaults, project_model_defaults_path};
use crate::providers::config_store;
use refact_providers::identity::{provider_identity_from_yaml, validate_provider_instance_id};
use crate::providers::instance::ProviderInstance;
use crate::providers::{create_provider, delete_provider_config, PROVIDER_NAMES};
use crate::providers::traits::{
    AvailableModel, CustomModelConfig, ModelSource, ProviderModel, ProviderRuntime, ProviderTrait,
    extra_headers_mapping_to_hash_map, parse_extra_headers_value,
};
use super::openrouter::OpenRouterProvider;
use super::google_gemini::GoogleGeminiProvider;
use super::claude_code::ClaudeCodeProvider;
use super::openai_codex::{OpenAICodexProvider, UsageRequestError};
use super::opencode::OpenCodeProvider;
use crate::providers::litellm::{LiteLLMBudgetQueryResult, LiteLLMProvider};
use crate::providers::quota::{
    ProviderQuotaCache, ProviderQuotaFact, ProviderQuotaFactValue, ProviderQuotaSnapshot,
    ProviderQuotaSource, ProviderQuotaWindow,
};
use crate::providers::google_antigravity::{
    GoogleAntigravityProvider, GoogleAntigravityQuotaRequestError,
};
use crate::providers::xai_oauth::XAIOAuthProvider;

mod common;
mod listing;
mod defaults;
mod models;
mod settings;
mod oauth;
mod oauth_callback;
mod usage;
mod quota;
mod quota_cache;

pub(crate) use common::*;
pub use listing::*;
pub use defaults::*;
pub use models::*;
pub(crate) use settings::*;
pub use oauth::*;
pub use oauth_callback::*;
pub use usage::*;
pub(crate) use quota::*;
pub(crate) use quota_cache::*;

#[cfg(test)]
mod tests;
