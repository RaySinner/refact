#![allow(unused_imports)]

use axum::extract::{Path, Query, State};
use axum::http::{Response, StatusCode};
use hyper::Body;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use refact_runtime_api::app_state::AppState;
use refact_runtime_api::GlobalContextCtx;
use refact_buddy_core::drafts::{draft_kind_str, DraftTarget, DraftValidationError};
use refact_buddy_core::types::DraftKind;
use refact_runtime_api::caps_model_caps::get_model_caps;
use refact_runtime_api::chat_ctx::ChatCtx;
use refact_core::custom_error::ScratchError;
use refact_runtime_api::global_context::GlobalContext;
use tokio::sync::Mutex as AMutex;
use refact_providers::config::ProviderDefaults;
use refact_core::provider_types::{ProjectModelDefaults, project_model_defaults_path};
use refact_providers::config_store;
use refact_providers::identity::{provider_identity_from_yaml, validate_provider_instance_id};
use refact_providers::instance::ProviderInstance;
use refact_providers::{create_provider, delete_provider_config, PROVIDER_NAMES};
use refact_providers::traits::{
    AvailableModel, CustomModelConfig, ModelSource, ProviderModel, ProviderRuntime, ProviderTrait,
    extra_headers_mapping_to_hash_map, parse_extra_headers_value,
};
use refact_providers::openrouter::OpenRouterProvider;
use refact_providers::google_gemini::GoogleGeminiProvider;
use refact_providers::claude_code::ClaudeCodeProvider;
use refact_providers::openai_codex::{OpenAICodexProvider, UsageRequestError};
use refact_providers::opencode::OpenCodeProvider;
use refact_providers::litellm::{LiteLLMBudgetQueryResult, LiteLLMProvider};
use refact_providers::quota::{
    ProviderQuotaCache, ProviderQuotaFact, ProviderQuotaFactValue, ProviderQuotaSnapshot,
    ProviderQuotaSource, ProviderQuotaWindow,
};
use refact_providers::google_antigravity::{
    GoogleAntigravityProvider, GoogleAntigravityQuotaRequestError,
};
use refact_providers::xai_oauth::XAIOAuthProvider;

pub mod common;
pub mod listing;
pub mod defaults;
pub mod models;
pub mod settings;
pub mod oauth;
pub mod oauth_callback;
pub mod usage;
pub mod quota;
pub mod quota_cache;

pub use common::*;
pub use listing::*;
pub use defaults::*;
pub use models::*;
pub use settings::*;
pub use oauth::*;
pub use oauth_callback::*;
pub use usage::*;
pub use quota::*;
pub use quota_cache::*;

