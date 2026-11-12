use super::*;

pub(crate) async fn fetch_provider_quota(
    ctx: &ChatCtx,
    gcx: &Arc<GlobalContext>,
    quota_cache: &Arc<AMutex<ProviderQuotaCache>>,
    provider_name: &str,
    base_provider: &str,
) -> ProviderQuotaSnapshot {
    let result: Result<(ProviderQuotaSource, Value), String> = match base_provider {
        "claude_code" => fetch_claude_code_usage_with_refresh(ctx, gcx, provider_name)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
            .map(|value| (ProviderQuotaSource::ClaudeCode, value)),
        "openai_codex" => fetch_openai_codex_usage_with_refresh(ctx, provider_name)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
            .map(|value| (ProviderQuotaSource::OpenaiCodex, value)),
        "openrouter" => {
            let resolved = resolve_provider_for_base(ctx, provider_name, "openrouter").await;
            match resolved {
                Ok((provider, client)) => {
                    match downcast_provider::<OpenRouterProvider>(provider.as_ref(), "OpenRouter") {
                        Ok(provider) => provider
                            .fetch_quota_info(&client)
                            .await
                            .and_then(|value| {
                                serde_json::to_value(value).map_err(|error| error.to_string())
                            })
                            .map(|value| (ProviderQuotaSource::OpenRouter, value)),
                        Err(error) => Err(error.message),
                    }
                }
                Err(error) => Err(error.message),
            }
        }
        "google_antigravity" => {
            fetch_google_antigravity_usage_with_refresh(ctx, quota_cache, provider_name)
                .await
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
                .map(|value| (ProviderQuotaSource::GoogleAntigravity, value))
        }
        "opencode" => {
            let resolved = resolve_provider_for_base(ctx, provider_name, "opencode").await;
            match resolved {
                Ok((provider, client)) => {
                    match downcast_provider::<OpenCodeProvider>(provider.as_ref(), "OpenCode") {
                        Ok(provider) if provider.usage_endpoint.trim().is_empty() => {
                            return ProviderQuotaSnapshot::unavailable(
                                provider_name,
                                base_provider,
                            );
                        }
                        Ok(provider) => provider
                            .fetch_usage(&client)
                            .await
                            .and_then(|value| {
                                serde_json::to_value(value).map_err(|error| error.to_string())
                            })
                            .map(|value| (ProviderQuotaSource::Opencode, value)),
                        Err(error) => Err(error.message),
                    }
                }
                Err(error) => Err(error.message),
            }
        }
        "xai_oauth" => {
            let provider = {
                let registry = ctx.model.providers.read().await;
                registry
                    .get(provider_name)
                    .map(|provider| provider.clone_box())
            };
            match provider.and_then(|provider| {
                provider
                    .as_any()
                    .downcast_ref::<XAIOAuthProvider>()
                    .cloned()
            }) {
                Some(provider) => provider
                    .fetch_usage()
                    .await
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    })
                    .map(|value| (ProviderQuotaSource::XaiOauth, value)),
                None => Err(format!("Provider '{provider_name}' is not available")),
            }
        }
        "litellm" => {
            let provider = {
                let registry = ctx.model.providers.read().await;
                registry
                    .get(provider_name)
                    .map(|provider| provider.clone_box())
            };
            match provider
                .and_then(|provider| provider.as_any().downcast_ref::<LiteLLMProvider>().cloned())
            {
                Some(provider) => match provider.query_budget(&ctx.runtime.http_client).await {
                    LiteLLMBudgetQueryResult::Available(value) => serde_json::to_value(value)
                        .map(|value| (ProviderQuotaSource::Litellm, value))
                        .map_err(|error| error.to_string()),
                    LiteLLMBudgetQueryResult::Unavailable(_) => {
                        return ProviderQuotaSnapshot::unavailable(provider_name, base_provider);
                    }
                    LiteLLMBudgetQueryResult::Error(error) => {
                        Err(format!("LiteLLM budget query failed: {error:?}"))
                    }
                },
                None => Err(format!("Provider '{provider_name}' is not available")),
            }
        }
        _ => return ProviderQuotaSnapshot::unavailable(provider_name, base_provider),
    };
    match result {
        Ok((source, details)) => {
            quota_snapshot_from_details(provider_name, base_provider, source, details)
        }
        Err(error) => ProviderQuotaSnapshot::error(
            provider_name,
            base_provider,
            match base_provider {
                "claude_code" => ProviderQuotaSource::ClaudeCode,
                "openai_codex" => ProviderQuotaSource::OpenaiCodex,
                "openrouter" => ProviderQuotaSource::OpenRouter,
                "opencode" => ProviderQuotaSource::Opencode,
                "google_antigravity" => ProviderQuotaSource::GoogleAntigravity,
                "xai_oauth" => ProviderQuotaSource::XaiOauth,
                "litellm" => ProviderQuotaSource::Litellm,
                _ => ProviderQuotaSource::Unavailable,
            },
            error,
        ),
    }
}


pub(crate) async fn provider_quota_cached(
    ctx: &ChatCtx,
    gcx: &Arc<GlobalContext>,
    quota_cache: &Arc<AMutex<ProviderQuotaCache>>,
    provider_name: &str,
    base_provider: &str,
    refresh: bool,
) -> ProviderQuotaSnapshot {
    let (cached, prior) = {
        let cache = quota_cache.lock().await;
        (
            if refresh {
                None
            } else {
                cache.get_fresh(provider_name, ProviderQuotaCache::now_timestamp())
            },
            cache.get_stale(provider_name),
        )
    };
    if !refresh {
        if let Some(snapshot) = cached {
            return snapshot;
        }
    }
    let snapshot = fetch_provider_quota(ctx, gcx, quota_cache, provider_name, base_provider).await;
    if let Some(fallback) = stale_quota_fallback(&snapshot, prior) {
        return fallback;
    }
    quota_cache
        .lock()
        .await
        .insert(provider_name.to_string(), snapshot.clone());
    snapshot
}


pub(crate) fn stale_quota_fallback(
    fetched: &ProviderQuotaSnapshot,
    prior: Option<ProviderQuotaSnapshot>,
) -> Option<ProviderQuotaSnapshot> {
    let error = fetched.error.as_ref()?;
    let mut prior = prior?;
    if !prior.available || prior.source == ProviderQuotaSource::Unavailable {
        return None;
    }
    prior.error = Some(error.clone());
    prior.stale = true;
    Some(prior)
}


pub(crate) async fn invalidate_provider_quota(
    quota_cache: &Arc<AMutex<ProviderQuotaCache>>,
    provider_name: &str,
) {
    quota_cache.lock().await.invalidate(provider_name);
}
