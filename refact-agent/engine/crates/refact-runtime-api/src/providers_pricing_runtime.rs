use std::sync::Arc;

use crate::chat_ctx::ChatCtx;
use crate::global_context::GlobalContext;
use crate::GlobalContextCtx;
use refact_providers::traits::ModelPricing;

pub use refact_pricing_core::{
    compute_cost, pricing_provider_aliases, standard_model_pricing_from_caps,
};

pub async fn lookup_model_pricing(
    gcx: &Arc<GlobalContext>,
    model_id: &str,
) -> Option<ModelPricing> {
    lookup_model_pricing_for_ctx(&gcx.chat_ctx(gcx.clone()), model_id).await
}

pub async fn lookup_model_pricing_for_ctx(
    ctx: &ChatCtx,
    model_id: &str,
) -> Option<ModelPricing> {
    if let Some(custom_pricing) = lookup_provider_custom_pricing_for_ctx(ctx, model_id).await {
        return Some(custom_pricing);
    }

    let caps_state = ctx.model.caps.clone();
    if let Some(pricing) = caps_state
        .read()
        .await
        .caps
        .as_ref()
        .and_then(|caps| standard_model_pricing_from_caps(&caps.model_caps, model_id))
    {
        return Some(pricing);
    }

    let caps = ctx
        .caps
        .try_load_caps_quickly(0)
        .await
        .ok()?;
    standard_model_pricing_from_caps(&caps.model_caps, model_id)
}

// dead: keep until provider custom pricing callers are confirmed
#[allow(dead_code)]
async fn lookup_provider_custom_pricing(
    gcx: &Arc<GlobalContext>,
    model_id: &str,
) -> Option<ModelPricing> {
    let (provider_name, model_name) = model_id.split_once('/')?;
    let registry = gcx.providers.read().await;
    registry
        .get(provider_name)
        .and_then(|provider| provider.custom_model_pricing(model_name))
}

async fn lookup_provider_custom_pricing_for_ctx(
    ctx: &ChatCtx,
    model_id: &str,
) -> Option<ModelPricing> {
    let (provider_name, model_name) = model_id.split_once('/')?;
    let registry = ctx.model.providers.read().await;
    registry
        .get(provider_name)
        .and_then(|provider| provider.custom_model_pricing(model_name))
}

