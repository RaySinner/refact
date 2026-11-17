#![allow(unused_imports)]

#[cfg(test)]
use axum::extract::{Path, Query, State};
#[cfg(test)]
use axum::http::{Response, StatusCode};
#[cfg(test)]
use hyper::Body;
#[cfg(test)]
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::{json, Value};
#[cfg(test)]
use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use refact_runtime_api::GlobalContextCtx;
#[cfg(test)]
use crate::buddy::drafts::{draft_kind_str, DraftTarget, DraftValidationError};
#[cfg(test)]
use crate::buddy::types::DraftKind;
#[cfg(test)]
use crate::caps::model_caps::get_model_caps;
#[cfg(test)]
use crate::chat::ctx::ChatCtx;
#[cfg(test)]
use crate::custom_error::ScratchError;
#[cfg(test)]
use crate::global_context::GlobalContext;
#[cfg(test)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
use crate::providers::config::ProviderDefaults;
#[cfg(test)]
use refact_core::provider_types::{ProjectModelDefaults, project_model_defaults_path};
#[cfg(test)]
use crate::providers::config_store;
#[cfg(test)]
use refact_providers::identity::{provider_identity_from_yaml, validate_provider_instance_id};
#[cfg(test)]
use crate::providers::instance::ProviderInstance;
#[cfg(test)]
use crate::providers::{create_provider, delete_provider_config, PROVIDER_NAMES};
#[cfg(test)]
use crate::providers::traits::{
    AvailableModel, CustomModelConfig, ModelSource, ProviderModel, ProviderRuntime, ProviderTrait,
    extra_headers_mapping_to_hash_map, parse_extra_headers_value,
};
#[cfg(test)]
use super::openrouter::OpenRouterProvider;
#[cfg(test)]
use super::google_gemini::GoogleGeminiProvider;
#[cfg(test)]
use super::claude_code::ClaudeCodeProvider;
#[cfg(test)]
use super::openai_codex::{OpenAICodexProvider, UsageRequestError};
#[cfg(test)]
use super::opencode::OpenCodeProvider;
#[cfg(test)]
use crate::providers::litellm::{LiteLLMBudgetQueryResult, LiteLLMProvider};
#[cfg(test)]
use crate::providers::quota::{
    ProviderQuotaCache, ProviderQuotaFact, ProviderQuotaFactValue, ProviderQuotaSnapshot,
    ProviderQuotaSource, ProviderQuotaWindow,
};
#[cfg(test)]
use crate::providers::google_antigravity::{
    GoogleAntigravityProvider, GoogleAntigravityQuotaRequestError,
};
#[cfg(test)]
use crate::providers::xai_oauth::XAIOAuthProvider;

pub use refact_http::providers_http::*;

#[cfg(test)]
mod tests;
