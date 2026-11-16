use std::collections::HashSet;
use std::sync::Arc;

use refact_buddy_core::actor::make_runtime_event;
use refact_providers::config_store;
use refact_providers::traits::ProviderTrait;
pub use refact_providers::oauth_refresh::{
    clear_invalid_refresh_token, is_invalid_refresh_token, is_permanent_refresh_error,
    mark_invalid_refresh_token,
};

use crate::buddy_runtime::buddy_enqueue_event;
use crate::global_context::{app_state_from_gcx, GlobalContext};
use crate::providers_oauth_runtime::needs_refresh;

const REFRESH_CHECK_INTERVAL_SECS: u64 = 60;
static OAUTH_FAILED_INSTANCES: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::OnceLock::new();

fn oauth_failed_instances() -> &'static std::sync::Mutex<HashSet<String>> {
    OAUTH_FAILED_INSTANCES.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}


const REFRESH_CHECK_INTERVAL_SECS: u64 = 60;

fn mark_oauth_failure(instance_id: &str) -> bool {
    oauth_failed_instances()
        .lock()
        .map(|mut failures| failures.insert(instance_id.to_string()))
        .unwrap_or(true)
}

fn clear_oauth_failure(instance_id: &str) -> bool {
    oauth_failed_instances()
        .lock()
        .map(|mut failures| failures.remove(instance_id))
        .unwrap_or(false)
}

#[cfg(test)]
fn oauth_failed_instance_count_for_test() -> usize {
    oauth_failed_instances()
        .lock()
        .map(|failures| failures.len())
        .unwrap_or(0)
}

#[cfg(test)]
fn clear_refresh_tracking_for_test() {
    if let Ok(mut failures) = oauth_failed_instances().lock() {
        failures.clear();
    }
}

#[cfg(test)]
fn collect_oauth_refresh_instances_for_base(
    providers: Vec<(String, String)>,
    base_provider: &str,
) -> Vec<String> {
    providers
        .into_iter()
        .filter_map(|(instance_id, base)| (base == base_provider).then_some(instance_id))
        .collect()
}

#[derive(Clone)]
struct OAuthRefreshCandidate<T> {
    instance_id: String,
    display_name: String,
    oauth_tokens: T,
}

pub async fn oauth_token_refresh_background_task(gcx: Arc<GlobalContext>) {
    refresh_expiring_oauth_tokens(&gcx).await;
    loop {
        let shutdown_flag = gcx.shutdown_flag.clone();
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(REFRESH_CHECK_INTERVAL_SECS)) => {}
            _ = async {
                while !shutdown_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            } => {
                tracing::info!("OAuth token refresh: shutdown detected, stopping");
                return;
            }
        }
        refresh_expiring_oauth_tokens(&gcx).await;
    }
}

pub(crate) async fn refresh_expiring_oauth_tokens(gcx: &Arc<GlobalContext>) {
    refresh_expiring_claude_code_tokens(gcx).await;
    let (http_client, config_dir) = { (gcx.http_client.clone(), gcx.config_dir.clone()) };
    try_refresh_openai_codex_instances(gcx, &http_client, &config_dir).await;
    try_refresh_xai_oauth_instances(gcx, &http_client, &config_dir).await;
    try_refresh_google_antigravity_instances(gcx, &http_client, &config_dir).await;
}

pub(crate) async fn refresh_expiring_claude_code_tokens(gcx: &Arc<GlobalContext>) {
    let http_client = gcx.http_client.clone();
    try_refresh_claude_code_instances(gcx, &http_client).await;
}

async fn try_refresh_claude_code_instances(
    gcx: &Arc<GlobalContext>,
    http_client: &reqwest::Client,
) {
    let candidates = {
        let registry = gcx.providers.read().await;
        registry
            .iter()
            .filter(|(_, provider)| provider.base_provider_name() == "claude_code")
            .filter_map(|(_, provider)| {
                let oauth_tokens = provider
                    .as_any()
                    .downcast_ref::<refact_providers::claude_code::ClaudeCodeProvider>()?
                    .oauth_tokens
                    .clone();
                Some(OAuthRefreshCandidate {
                    instance_id: provider.name().to_string(),
                    display_name: provider.display_name().to_string(),
                    oauth_tokens,
                })
            })
            .collect::<Vec<_>>()
    };

    for candidate in candidates {
        try_refresh_claude_code(gcx, http_client, candidate).await;
    }
}

async fn try_refresh_claude_code(
    gcx: &Arc<GlobalContext>,
    http_client: &reqwest::Client,
    candidate: OAuthRefreshCandidate<refact_providers::claude_code_oauth::OAuthTokens>,
) {
    let oauth_tokens = candidate.oauth_tokens;
    let instance_id = candidate.instance_id;
    let display_name = candidate.display_name;

    if oauth_tokens.is_empty() || oauth_tokens.refresh_token.is_empty() {
        return;
    }

    if !needs_refresh(oauth_tokens.expires_at) {
        return;
    }

    if is_invalid_refresh_token(&instance_id, &oauth_tokens.refresh_token) {
        return;
    }

    tracing::info!(
        "{}: refreshing OAuth token (expires_at={})",
        display_name,
        oauth_tokens.expires_at
    );

    match force_refresh_claude_code_for_retry(
        gcx,
        http_client,
        &instance_id,
        &oauth_tokens.access_token,
        Some(oauth_tokens.expires_at),
        None,
    )
    .await
    {
        Ok(Some(_)) => {
            tracing::info!("{}: OAuth token refreshed successfully", display_name);
            if clear_oauth_failure(&instance_id) {
                let ev = make_runtime_event(
                    "connection_restored",
                    &format!("{}: OAuth token refreshed", display_name),
                    "provider",
                    &format!("oauth_{}", instance_id),
                    "completed",
                    None,
                );
                crate::buddy_runtime::buddy_enqueue_event(
                    app_state_from_gcx((*gcx).clone()).await,
                    ev,
                )
                .await;
            }
        }
        Ok(None) => {}
        Err(e) => {
            let first_failure = mark_oauth_failure(&instance_id);
            if is_permanent_refresh_error(&e) {
                mark_invalid_refresh_token(&instance_id, &oauth_tokens.refresh_token);
                if first_failure {
                    tracing::warn!(
                        "{}: OAuth refresh token is invalid; clearing saved OAuth tokens. Please log in again: {}",
                        display_name,
                        e
                    );
                } else {
                    tracing::debug!(
                        "{}: OAuth refresh token is still invalid: {}",
                        display_name,
                        e
                    );
                }
                if first_failure {
                    let ev = make_runtime_event(
                        "connection_lost",
                        &format!("{} OAuth expired — please log in again", display_name),
                        "provider",
                        &format!("oauth_{}", instance_id),
                        "failed",
                        Some("high"),
                    );
                    crate::buddy_runtime::buddy_enqueue_event(
                        app_state_from_gcx((*gcx).clone()).await,
                        ev,
                    )
                    .await;
                }
                return;
            }
            if first_failure {
                tracing::warn!("{}: OAuth token refresh failed: {}", display_name, e);
                let ev = make_runtime_event(
                    "connection_lost",
                    &format!("{}: OAuth refresh failed", display_name),
                    "provider",
                    &format!("oauth_{}", instance_id),
                    "failed",
                    Some("high"),
                );
                crate::buddy_runtime::buddy_enqueue_event(
                    app_state_from_gcx((*gcx).clone()).await,
                    ev,
                )
                .await;
            } else {
                tracing::debug!("{}: OAuth token refresh still failing: {}", display_name, e);
            }
        }
    }
}

pub async fn force_refresh_claude_code_for_retry(
    gcx: &Arc<GlobalContext>,
    http_client: &reqwest::Client,
    provider_name: &str,
    rejected_access_token: &str,
    rejected_expires_at: Option<i64>,
    rejected_status: Option<reqwest::StatusCode>,
) -> Result<Option<refact_providers::claude_code::ClaudeCodeProvider>, String> {
    crate::providers_oauth_runtime::force_refresh_claude_code_for_retry(
        &gcx.providers,
        http_client,
        &gcx.config_dir,
        &gcx.caps_state,
        &gcx.tool_catalog_generations,
        provider_name,
        rejected_access_token,
        rejected_expires_at,
        rejected_status,
    )
    .await
}

#[cfg(test)]
fn proactive_refresh_can_reuse_newer_expiry(
    rejected_expires_at: Option<i64>,
    rejected_status: Option<reqwest::StatusCode>,
    current_expires_at: i64,
) -> bool {
    crate::providers_oauth_runtime::proactive_refresh_can_reuse_newer_expiry(
        rejected_expires_at,
        rejected_status,
        current_expires_at,
    )
}

pub(crate) async fn refresh_claude_code_instance_if_needed(
    gcx: &Arc<GlobalContext>,
    provider_name: &str,
) -> Result<(), String> {
    let tokens = {
        let registry = gcx.providers.read().await;
        registry
            .get(provider_name)
            .and_then(|provider| {
                provider
                    .as_any()
                    .downcast_ref::<refact_providers::claude_code::ClaudeCodeProvider>()
            })
            .map(|provider| provider.oauth_tokens.clone())
    };
    let Some(tokens) = tokens else {
        return Ok(());
    };
    if !needs_refresh(tokens.expires_at) {
        return Ok(());
    }
    force_refresh_claude_code_for_retry(
        gcx,
        &gcx.http_client,
        provider_name,
        &tokens.access_token,
        Some(tokens.expires_at),
        None,
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
async fn save_claude_code_refreshed_tokens(
    gcx: &Arc<GlobalContext>,
    config_dir: &std::path::Path,
    provider_name: &str,
    expected_tokens: &refact_providers::claude_code_oauth::OAuthTokens,
    refreshed_tokens: &refact_providers::claude_code_oauth::OAuthTokens,
) -> Result<bool, String> {
    crate::providers_oauth_runtime::save_claude_code_refreshed_tokens(
        &gcx.providers,
        config_dir,
        provider_name,
        expected_tokens,
        refreshed_tokens,
    )
    .await
}

async fn try_refresh_openai_codex_instances(
    gcx: &Arc<GlobalContext>,
    http_client: &reqwest::Client,
    config_dir: &std::path::Path,
) {
    let candidates = {
        let registry = gcx.providers.read().await;
        registry
            .iter()
            .filter(|(_, provider)| provider.base_provider_name() == "openai_codex")
            .filter_map(|(_, provider)| {
                let oauth_tokens = provider
                    .as_any()
                    .downcast_ref::<refact_providers::openai_codex::OpenAICodexProvider>()?
                    .oauth_tokens
                    .clone();
                Some(OAuthRefreshCandidate {
                    instance_id: provider.name().to_string(),
                    display_name: provider.display_name().to_string(),
                    oauth_tokens,
                })
            })
            .collect::<Vec<_>>()
    };

    for candidate in candidates {
        try_refresh_openai_codex(gcx, http_client, config_dir, candidate).await;
    }
}

async fn try_refresh_openai_codex(
    gcx: &Arc<GlobalContext>,
    http_client: &reqwest::Client,
    config_dir: &std::path::Path,
    candidate: OAuthRefreshCandidate<refact_providers::openai_codex_oauth::OAuthTokens>,
) {
    let instance_id = candidate.instance_id;
    let display_name = candidate.display_name;
    let candidate_tokens = candidate.oauth_tokens;

    if candidate_tokens.is_empty() || candidate_tokens.refresh_token.is_empty() {
        return;
    }

    if !needs_refresh(candidate_tokens.expires_at) {
        return;
    }

    if is_invalid_refresh_token(&instance_id, &candidate_tokens.refresh_token) {
        return;
    }

    let _guard =
        match refact_providers::openai_codex::OpenAICodexProvider::lock_refresh_guard().await {
            Ok(guard) => guard,
            Err(error) => {
                tracing::warn!(
                    "{}: failed to acquire OAuth refresh guard: {}",
                    display_name,
                    error
                );
                return;
            }
        };
    let Some(oauth_tokens) = ({
        let registry = gcx.providers.read().await;
        registry
            .get(&instance_id)
            .and_then(|provider| {
                provider
                    .as_any()
                    .downcast_ref::<refact_providers::openai_codex::OpenAICodexProvider>()
            })
            .map(|provider| provider.oauth_tokens.clone())
    }) else {
        return;
    };
    if oauth_tokens.is_empty()
        || oauth_tokens.refresh_token.is_empty()
        || !needs_refresh(oauth_tokens.expires_at)
        || is_invalid_refresh_token(&instance_id, &oauth_tokens.refresh_token)
    {
        return;
    }

    tracing::info!(
        "{}: refreshing OAuth token (expires_at={})",
        display_name,
        oauth_tokens.expires_at
    );

    match refact_providers::openai_codex_oauth::refresh_access_token(
        http_client,
        &oauth_tokens.refresh_token,
    )
    .await
    {
        Ok(new_tokens) => {
            tracing::info!("{}: OAuth token refreshed successfully", display_name);
            let saved = match save_refreshed_tokens(
                gcx,
                config_dir,
                &instance_id,
                &oauth_tokens,
                &new_tokens.access_token,
                &new_tokens.refresh_token,
                new_tokens.expires_at,
            )
            .await
            {
                Ok(saved) => saved,
                Err(e) => {
                    tracing::warn!("{}: failed to save refreshed tokens: {}", display_name, e);
                    false
                }
            };
            if saved && clear_oauth_failure(&instance_id) {
                let ev = make_runtime_event(
                    "connection_restored",
                    &format!("{}: OAuth token refreshed", display_name),
                    "provider",
                    &format!("oauth_{}", instance_id),
                    "completed",
                    None,
                );
                crate::buddy_runtime::buddy_enqueue_event(
                    app_state_from_gcx((*gcx).clone()).await,
                    ev,
                )
                .await;
            }
        }
        Err(e) => {
            let first_failure = mark_oauth_failure(&instance_id);
            if is_permanent_refresh_error(&e) {
                mark_invalid_refresh_token(&instance_id, &oauth_tokens.refresh_token);
                if first_failure {
                    tracing::warn!(
                        "{}: OAuth refresh token is invalid; clearing saved refresh token. Please log in again if Codex stops working: {}",
                        display_name,
                        e
                    );
                } else {
                    tracing::debug!(
                        "{}: OAuth refresh token is still invalid: {}",
                        display_name,
                        e
                    );
                }
                let cleared = match save_refreshed_tokens(
                    gcx,
                    config_dir,
                    &instance_id,
                    &oauth_tokens,
                    "",
                    "",
                    0,
                )
                .await
                {
                    Ok(cleared) => cleared,
                    Err(save_err) => {
                        tracing::warn!(
                            "{}: failed to clear invalid OAuth refresh token: {}",
                            display_name,
                            save_err
                        );
                        false
                    }
                };
                if first_failure && cleared {
                    let ev = make_runtime_event(
                        "connection_lost",
                        &format!(
                            "{} OAuth expired — please log in again if needed",
                            display_name
                        ),
                        "provider",
                        &format!("oauth_{}", instance_id),
                        "failed",
                        Some("high"),
                    );
                    crate::buddy_runtime::buddy_enqueue_event(
                        app_state_from_gcx((*gcx).clone()).await,
                        ev,
                    )
                    .await;
                }
                return;
            }
            if first_failure {
                tracing::warn!("{}: OAuth token refresh failed: {}", display_name, e);
                let ev = make_runtime_event(
                    "connection_lost",
                    &format!("{}: OAuth refresh failed", display_name),
                    "provider",
                    &format!("oauth_{}", instance_id),
                    "failed",
                    Some("high"),
                );
                crate::buddy_runtime::buddy_enqueue_event(
                    app_state_from_gcx((*gcx).clone()).await,
                    ev,
                )
                .await;
            } else {
                tracing::debug!("{}: OAuth token refresh still failing: {}", display_name, e);
            }
        }
    }
}

async fn try_refresh_xai_oauth_instances(
    gcx: &Arc<GlobalContext>,
    http_client: &reqwest::Client,
    config_dir: &std::path::Path,
) {
    let candidates = {
        let registry = gcx.providers.read().await;
        registry
            .iter()
            .filter(|(_, provider)| provider.base_provider_name() == "xai_oauth")
            .filter_map(|(_, provider)| {
                let oauth_tokens = provider
                    .as_any()
                    .downcast_ref::<refact_providers::xai_oauth::XAIOAuthProvider>()?
                    .oauth_tokens
                    .clone();
                Some(OAuthRefreshCandidate {
                    instance_id: provider.name().to_string(),
                    display_name: provider.display_name().to_string(),
                    oauth_tokens,
                })
            })
            .collect::<Vec<_>>()
    };

    for candidate in candidates {
        try_refresh_xai_oauth(gcx, http_client, config_dir, candidate).await;
    }
}

async fn try_refresh_xai_oauth(
    gcx: &Arc<GlobalContext>,
    http_client: &reqwest::Client,
    config_dir: &std::path::Path,
    candidate: OAuthRefreshCandidate<refact_providers::xai_oauth_flow::OAuthTokens>,
) {
    let instance_id = candidate.instance_id;
    let display_name = candidate.display_name;
    let candidate_tokens = candidate.oauth_tokens;

    if candidate_tokens.is_empty() || candidate_tokens.refresh_token.is_empty() {
        return;
    }

    if !needs_refresh(candidate_tokens.expires_at) {
        return;
    }

    if is_invalid_refresh_token(&instance_id, &candidate_tokens.refresh_token) {
        return;
    }

    let _guard = match refact_providers::xai_oauth::XAIOAuthProvider::lock_refresh_guard().await {
        Ok(guard) => guard,
        Err(error) => {
            tracing::warn!(
                "{}: failed to acquire OAuth refresh guard: {}",
                display_name,
                error
            );
            return;
        }
    };
    let Some(oauth_tokens) = ({
        let registry = gcx.providers.read().await;
        registry
            .get(&instance_id)
            .and_then(|provider| {
                provider
                    .as_any()
                    .downcast_ref::<refact_providers::xai_oauth::XAIOAuthProvider>()
            })
            .map(|provider| provider.oauth_tokens.clone())
    }) else {
        return;
    };
    if oauth_tokens.is_empty()
        || oauth_tokens.refresh_token.is_empty()
        || !needs_refresh(oauth_tokens.expires_at)
        || is_invalid_refresh_token(&instance_id, &oauth_tokens.refresh_token)
    {
        return;
    }

    tracing::info!(
        "{}: refreshing OAuth token (expires_at={})",
        display_name,
        oauth_tokens.expires_at
    );

    match refact_providers::xai_oauth_flow::refresh_access_token(
        http_client,
        &oauth_tokens.refresh_token,
    )
    .await
    {
        Ok(new_tokens) => {
            tracing::info!("{}: OAuth token refreshed successfully", display_name);
            let saved = match save_xai_refreshed_tokens(
                gcx,
                config_dir,
                &instance_id,
                &oauth_tokens,
                &new_tokens.access_token,
                &new_tokens.refresh_token,
                new_tokens.expires_at,
            )
            .await
            {
                Ok(saved) => saved,
                Err(e) => {
                    tracing::warn!("{}: failed to save refreshed tokens: {}", display_name, e);
                    false
                }
            };
            if saved && clear_oauth_failure(&instance_id) {
                let ev = make_runtime_event(
                    "connection_restored",
                    &format!("{}: OAuth token refreshed", display_name),
                    "provider",
                    &format!("oauth_{}", instance_id),
                    "completed",
                    None,
                );
                crate::buddy_runtime::buddy_enqueue_event(
                    app_state_from_gcx((*gcx).clone()).await,
                    ev,
                )
                .await;
            }
        }
        Err(e) => {
            let first_failure = mark_oauth_failure(&instance_id);
            if is_permanent_refresh_error(&e) {
                mark_invalid_refresh_token(&instance_id, &oauth_tokens.refresh_token);
                if first_failure {
                    tracing::warn!(
                        "{}: OAuth refresh token is invalid; clearing saved refresh token. Please log in again if Grok stops working: {}",
                        display_name,
                        e
                    );
                } else {
                    tracing::debug!(
                        "{}: OAuth refresh token is still invalid: {}",
                        display_name,
                        e
                    );
                }
                let cleared = match save_xai_refreshed_tokens(
                    gcx,
                    config_dir,
                    &instance_id,
                    &oauth_tokens,
                    "",
                    "",
                    0,
                )
                .await
                {
                    Ok(cleared) => cleared,
                    Err(save_err) => {
                        tracing::warn!(
                            "{}: failed to clear invalid OAuth refresh token: {}",
                            display_name,
                            save_err
                        );
                        false
                    }
                };
                if first_failure && cleared {
                    let ev = make_runtime_event(
                        "connection_lost",
                        &format!(
                            "{} OAuth expired — please log in again if needed",
                            display_name
                        ),
                        "provider",
                        &format!("oauth_{}", instance_id),
                        "failed",
                        Some("high"),
                    );
                    crate::buddy_runtime::buddy_enqueue_event(
                        app_state_from_gcx((*gcx).clone()).await,
                        ev,
                    )
                    .await;
                }
                return;
            }
            if first_failure {
                tracing::warn!("{}: OAuth token refresh failed: {}", display_name, e);
                let ev = make_runtime_event(
                    "connection_lost",
                    &format!("{}: OAuth refresh failed", display_name),
                    "provider",
                    &format!("oauth_{}", instance_id),
                    "failed",
                    Some("high"),
                );
                crate::buddy_runtime::buddy_enqueue_event(
                    app_state_from_gcx((*gcx).clone()).await,
                    ev,
                )
                .await;
            } else {
                tracing::debug!("{}: OAuth token refresh still failing: {}", display_name, e);
            }
        }
    }
}

async fn try_refresh_google_antigravity_instances(
    gcx: &Arc<GlobalContext>,
    http_client: &reqwest::Client,
    config_dir: &std::path::Path,
) {
    let candidates = {
        let registry = gcx.providers.read().await;
        registry
            .iter()
            .filter(|(_, provider)| provider.base_provider_name() == "google_antigravity")
            .filter_map(|(_, provider)| {
                let oauth_tokens = provider
                    .as_any()
                    .downcast_ref::<
                        refact_providers::google_antigravity::GoogleAntigravityProvider,
                    >()?
                    .oauth_tokens
                    .clone();
                Some(OAuthRefreshCandidate {
                    instance_id: provider.name().to_string(),
                    display_name: provider.display_name().to_string(),
                    oauth_tokens,
                })
            })
            .collect::<Vec<_>>()
    };

    for candidate in candidates {
        try_refresh_google_antigravity(gcx, http_client, config_dir, candidate).await;
    }
}

async fn try_refresh_google_antigravity(
    gcx: &Arc<GlobalContext>,
    http_client: &reqwest::Client,
    config_dir: &std::path::Path,
    candidate: OAuthRefreshCandidate<refact_providers::google_antigravity_oauth::OAuthTokens>,
) {
    use refact_providers::google_antigravity::GoogleAntigravityProvider;

    let instance_id = candidate.instance_id;
    let display_name = candidate.display_name;
    let candidate_tokens = candidate.oauth_tokens;

    if candidate_tokens.is_empty() || candidate_tokens.refresh_token.is_empty() {
        return;
    }

    if !needs_refresh(candidate_tokens.expires_at) {
        return;
    }

    if is_invalid_refresh_token(&instance_id, &candidate_tokens.refresh_token) {
        return;
    }

    let _guard = match GoogleAntigravityProvider::lock_refresh_guard().await {
        Ok(guard) => guard,
        Err(error) => {
            tracing::warn!(
                "{}: failed to acquire OAuth refresh guard: {}",
                display_name,
                error
            );
            return;
        }
    };
    let Some(oauth_tokens) = ({
        let registry = gcx.providers.read().await;
        registry
            .get(&instance_id)
            .and_then(|provider| {
                provider.as_any().downcast_ref::<
                    refact_providers::google_antigravity::GoogleAntigravityProvider,
                >()
            })
            .map(|provider| provider.oauth_tokens.clone())
    }) else {
        return;
    };
    if oauth_tokens.is_empty()
        || oauth_tokens.refresh_token.is_empty()
        || !needs_refresh(oauth_tokens.expires_at)
        || is_invalid_refresh_token(&instance_id, &oauth_tokens.refresh_token)
    {
        return;
    }

    tracing::info!(
        "{}: refreshing OAuth token (expires_at={})",
        display_name,
        oauth_tokens.expires_at
    );

    match refact_providers::google_antigravity_oauth::refresh_access_token(
        http_client,
        &oauth_tokens.refresh_token,
    )
    .await
    {
        Ok(new_tokens) => {
            tracing::info!("{}: OAuth token refreshed successfully", display_name);
            let project_id = if new_tokens.project_id.is_empty() {
                &oauth_tokens.project_id
            } else {
                &new_tokens.project_id
            };
            let saved = match save_google_antigravity_refreshed_tokens(
                gcx,
                config_dir,
                &instance_id,
                &oauth_tokens,
                &new_tokens.access_token,
                &new_tokens.refresh_token,
                new_tokens.expires_at,
                project_id,
            )
            .await
            {
                Ok(saved) => saved,
                Err(e) => {
                    tracing::warn!("{}: failed to save refreshed tokens: {}", display_name, e);
                    false
                }
            };
            if saved && clear_oauth_failure(&instance_id) {
                let ev = make_runtime_event(
                    "connection_restored",
                    &format!("{}: OAuth token refreshed", display_name),
                    "provider",
                    &format!("oauth_{}", instance_id),
                    "completed",
                    None,
                );
                crate::buddy_runtime::buddy_enqueue_event(
                    app_state_from_gcx((*gcx).clone()).await,
                    ev,
                )
                .await;
            }
        }
        Err(e) => {
            let first_failure = mark_oauth_failure(&instance_id);
            if is_permanent_refresh_error(&e) {
                mark_invalid_refresh_token(&instance_id, &oauth_tokens.refresh_token);
                if first_failure {
                    tracing::warn!(
                        concat!(
                            "{}: OAuth refresh token is invalid; clearing saved refresh token. ",
                            "Please log in again if Google Antigravity stops working: {}"
                        ),
                        display_name,
                        e
                    );
                } else {
                    tracing::debug!(
                        "{}: OAuth refresh token is still invalid: {}",
                        display_name,
                        e
                    );
                }
                let cleared = match save_google_antigravity_refreshed_tokens(
                    gcx,
                    config_dir,
                    &instance_id,
                    &oauth_tokens,
                    "",
                    "",
                    0,
                    &oauth_tokens.project_id,
                )
                .await
                {
                    Ok(cleared) => cleared,
                    Err(save_err) => {
                        tracing::warn!(
                            "{}: failed to clear invalid OAuth refresh token: {}",
                            display_name,
                            save_err
                        );
                        false
                    }
                };
                if first_failure && cleared {
                    let ev = make_runtime_event(
                        "connection_lost",
                        &format!(
                            "{} OAuth expired — please log in again if needed",
                            display_name
                        ),
                        "provider",
                        &format!("oauth_{}", instance_id),
                        "failed",
                        Some("high"),
                    );
                    crate::buddy_runtime::buddy_enqueue_event(
                        app_state_from_gcx((*gcx).clone()).await,
                        ev,
                    )
                    .await;
                }
                return;
            }
            if first_failure {
                tracing::warn!("{}: OAuth token refresh failed: {}", display_name, e);
                let ev = make_runtime_event(
                    "connection_lost",
                    &format!("{}: OAuth refresh failed", display_name),
                    "provider",
                    &format!("oauth_{}", instance_id),
                    "failed",
                    Some("high"),
                );
                crate::buddy_runtime::buddy_enqueue_event(
                    app_state_from_gcx((*gcx).clone()).await,
                    ev,
                )
                .await;
            } else {
                tracing::debug!("{}: OAuth token refresh still failing: {}", display_name, e);
            }
        }
    }
}

async fn save_google_antigravity_refreshed_tokens(
    gcx: &Arc<GlobalContext>,
    config_dir: &std::path::Path,
    provider_name: &str,
    expected_tokens: &refact_providers::google_antigravity_oauth::OAuthTokens,
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
    project_id: &str,
) -> Result<bool, String> {
    let registry_matches = {
        let registry = gcx.providers.read().await;
        registry
            .get(provider_name)
            .and_then(|provider| {
                provider.as_any().downcast_ref::<
                    refact_providers::google_antigravity::GoogleAntigravityProvider,
                >()
            })
            .is_some_and(|provider| &provider.oauth_tokens == expected_tokens)
    };
    if !registry_matches {
        return Ok(false);
    }

    let updated = config_store::update_provider_config_if(config_dir, provider_name, |existing| {
        let Some(value) = existing else {
            return Ok(None);
        };
        let mut yaml_map = value.as_mapping().cloned().ok_or_else(|| {
            "Config file root is not a YAML mapping. Cannot safely patch.".to_string()
        })?;

        let mut tokens_map = yaml_map
            .get(&serde_yaml::Value::String("oauth_tokens".to_string()))
            .and_then(|v| v.as_mapping())
            .cloned()
            .unwrap_or_default();
        let current_tokens: refact_providers::google_antigravity_oauth::OAuthTokens =
            serde_yaml::from_value(serde_yaml::Value::Mapping(tokens_map.clone()))
                .map_err(|error| format!("Failed to parse existing OAuth tokens: {}", error))?;
        if &current_tokens != expected_tokens {
            return Ok(None);
        }

        tokens_map.insert(
            serde_yaml::Value::String("access_token".to_string()),
            serde_yaml::Value::String(access_token.to_string()),
        );
        tokens_map.insert(
            serde_yaml::Value::String("refresh_token".to_string()),
            serde_yaml::Value::String(refresh_token.to_string()),
        );
        tokens_map.insert(
            serde_yaml::Value::String("expires_at".to_string()),
            serde_yaml::Value::Number(serde_yaml::Number::from(expires_at)),
        );
        tokens_map.insert(
            serde_yaml::Value::String("project_id".to_string()),
            serde_yaml::Value::String(project_id.to_string()),
        );

        yaml_map.insert(
            serde_yaml::Value::String("oauth_tokens".to_string()),
            serde_yaml::Value::Mapping(tokens_map),
        );

        Ok(Some(serde_yaml::Value::Mapping(yaml_map)))
    })
    .await?;
    if updated.is_none() {
        return Ok(false);
    }

    let changed = {
        let mut registry = gcx.providers.write().await;
        registry
            .get_mut(provider_name)
            .and_then(|provider| {
                provider.as_any_mut().downcast_mut::<
                    refact_providers::google_antigravity::GoogleAntigravityProvider,
                >()
            })
            .filter(|provider| &provider.oauth_tokens == expected_tokens)
            .map(|provider| {
                provider.apply_oauth_refresh_tokens(access_token, refresh_token, expires_at);
                provider.oauth_tokens.project_id = project_id.to_string();
            })
            .is_some()
    };

    if changed {
        let caps_state = gcx.caps_state.clone();
        let mut caps_state = caps_state.write().await;
        caps_state.caps = None;
        caps_state.last_attempted_ts = 0;
        gcx.tool_catalog_generations.advance_capabilities();
    }

    Ok(changed)
}

async fn save_xai_refreshed_tokens(
    gcx: &Arc<GlobalContext>,
    config_dir: &std::path::Path,
    provider_name: &str,
    expected_tokens: &refact_providers::xai_oauth_flow::OAuthTokens,
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
) -> Result<bool, String> {
    let registry_matches = {
        let registry = gcx.providers.read().await;
        registry
            .get(provider_name)
            .and_then(|provider| {
                provider
                    .as_any()
                    .downcast_ref::<refact_providers::xai_oauth::XAIOAuthProvider>()
            })
            .is_some_and(|provider| &provider.oauth_tokens == expected_tokens)
    };
    if !registry_matches {
        return Ok(false);
    }

    let updated = config_store::update_provider_config_if(config_dir, provider_name, |existing| {
        let Some(value) = existing else {
            return Ok(None);
        };
        let mut yaml_map = value.as_mapping().cloned().ok_or_else(|| {
            "Config file root is not a YAML mapping. Cannot safely patch.".to_string()
        })?;

        let mut tokens_map = yaml_map
            .get(&serde_yaml::Value::String("oauth_tokens".to_string()))
            .and_then(|v| v.as_mapping())
            .cloned()
            .unwrap_or_default();
        let current_tokens: refact_providers::xai_oauth_flow::OAuthTokens =
            serde_yaml::from_value(serde_yaml::Value::Mapping(tokens_map.clone()))
                .map_err(|error| format!("Failed to parse existing OAuth tokens: {}", error))?;
        if &current_tokens != expected_tokens {
            return Ok(None);
        }

        tokens_map.insert(
            serde_yaml::Value::String("access_token".to_string()),
            serde_yaml::Value::String(access_token.to_string()),
        );
        tokens_map.insert(
            serde_yaml::Value::String("refresh_token".to_string()),
            serde_yaml::Value::String(refresh_token.to_string()),
        );
        tokens_map.insert(
            serde_yaml::Value::String("expires_at".to_string()),
            serde_yaml::Value::Number(serde_yaml::Number::from(expires_at)),
        );

        yaml_map.insert(
            serde_yaml::Value::String("oauth_tokens".to_string()),
            serde_yaml::Value::Mapping(tokens_map),
        );

        Ok(Some(serde_yaml::Value::Mapping(yaml_map)))
    })
    .await?;
    if updated.is_none() {
        return Ok(false);
    }

    let changed = {
        let mut registry = gcx.providers.write().await;
        registry
            .get_mut(provider_name)
            .and_then(|provider| {
                provider
                    .as_any_mut()
                    .downcast_mut::<refact_providers::xai_oauth::XAIOAuthProvider>()
            })
            .filter(|provider| &provider.oauth_tokens == expected_tokens)
            .map(|provider| {
                provider.apply_oauth_refresh_tokens(access_token, refresh_token, expires_at);
            })
            .is_some()
    };

    if changed {
        let caps_state = gcx.caps_state.clone();
        let mut caps_state = caps_state.write().await;
        caps_state.caps = None;
        caps_state.last_attempted_ts = 0;
        gcx.tool_catalog_generations.advance_capabilities();
    }

    Ok(changed)
}


pub(crate) async fn save_refreshed_tokens(
    gcx: &Arc<GlobalContext>,
    config_dir: &std::path::Path,
    provider_name: &str,
    expected_tokens: &refact_providers::openai_codex_oauth::OAuthTokens,
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
) -> Result<bool, String> {
    let registry_matches = {
        let registry = gcx.providers.read().await;
        registry
            .get(provider_name)
            .and_then(|provider| {
                provider
                    .as_any()
                    .downcast_ref::<refact_providers::openai_codex::OpenAICodexProvider>()
            })
            .is_some_and(|provider| &provider.oauth_tokens == expected_tokens)
    };
    if !registry_matches {
        return Ok(false);
    }

    let updated = config_store::update_provider_config_if(config_dir, provider_name, |existing| {
        let Some(value) = existing else {
            return Ok(None);
        };
        let mut yaml_map = value.as_mapping().cloned().ok_or_else(|| {
            "Config file root is not a YAML mapping. Cannot safely patch.".to_string()
        })?;

        let mut tokens_map = yaml_map
            .get(&serde_yaml::Value::String("oauth_tokens".to_string()))
            .and_then(|v| v.as_mapping())
            .cloned()
            .unwrap_or_default();
        let current_tokens: refact_providers::openai_codex_oauth::OAuthTokens =
            serde_yaml::from_value(serde_yaml::Value::Mapping(tokens_map.clone()))
                .map_err(|error| format!("Failed to parse existing OAuth tokens: {}", error))?;
        if &current_tokens != expected_tokens {
            return Ok(None);
        }

        tokens_map.insert(
            serde_yaml::Value::String("access_token".to_string()),
            serde_yaml::Value::String(access_token.to_string()),
        );
        tokens_map.insert(
            serde_yaml::Value::String("refresh_token".to_string()),
            serde_yaml::Value::String(refresh_token.to_string()),
        );
        tokens_map.insert(
            serde_yaml::Value::String("expires_at".to_string()),
            serde_yaml::Value::Number(serde_yaml::Number::from(expires_at)),
        );

        yaml_map.insert(
            serde_yaml::Value::String("oauth_tokens".to_string()),
            serde_yaml::Value::Mapping(tokens_map),
        );

        Ok(Some(serde_yaml::Value::Mapping(yaml_map)))
    })
    .await?;
    if updated.is_none() {
        return Ok(false);
    }

    let changed = {
        let mut registry = gcx.providers.write().await;
        registry
            .get_mut(provider_name)
            .and_then(|provider| {
                provider
                    .as_any_mut()
                    .downcast_mut::<refact_providers::openai_codex::OpenAICodexProvider>()
            })
            .filter(|provider| &provider.oauth_tokens == expected_tokens)
            .map(|provider| {
                provider.apply_oauth_refresh_tokens(access_token, refresh_token, expires_at);
            })
            .is_some()
    };

    if changed {
        let caps_state = gcx.caps_state.clone();
        let mut caps_state = caps_state.write().await;
        caps_state.caps = None;
        caps_state.last_attempted_ts = 0;
        gcx.tool_catalog_generations.advance_capabilities();
    }

    Ok(changed)
}


#[cfg(test)]
mod tests {
    fn refresh_tracking_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("refresh tracking test lock poisoned")
    }

    use super::*;

    #[test]
    fn permanent_refresh_error_detects_invalid_grant() {
        assert!(super::is_permanent_refresh_error(
            r#"Token refresh failed (400 Bad Request): {"error":"invalid_grant"}"#
        ));
        assert!(super::is_permanent_refresh_error("INVALID_GRANT"));
        assert!(super::is_permanent_refresh_error("Invalid_Grant"));
        assert!(super::is_permanent_refresh_error(
            r#"Token refresh failed (400 Bad Request): {"error":{"code":"Invalid_Grant"}}"#
        ));
    }

    #[test]
    fn permanent_refresh_error_ignores_transient_failure() {
        for error in [
            "Token refresh request failed: operation timed out",
            "Token refresh failed (500 Internal Server Error)",
            "network connection reset by peer",
        ] {
            assert!(!super::is_permanent_refresh_error(error), "{error}");
        }
    }

    #[test]
    fn invalid_refresh_token_tracking_is_per_instance() {
        let _guard = refresh_tracking_test_guard();
        super::clear_refresh_tracking_for_test();
        super::mark_invalid_refresh_token("openai_codex", "same-refresh-token-test");

        assert!(super::is_invalid_refresh_token(
            "openai_codex",
            "same-refresh-token-test"
        ));
        assert!(!super::is_invalid_refresh_token(
            "openai_codex_2",
            "same-refresh-token-test"
        ));
        super::clear_invalid_refresh_token("openai_codex", "same-refresh-token-test");
        assert!(!super::is_invalid_refresh_token(
            "openai_codex",
            "same-refresh-token-test"
        ));

        super::clear_refresh_tracking_for_test();
    }

    #[test]
    fn oauth_failure_tracking_is_per_instance() {
        let _guard = refresh_tracking_test_guard();
        super::clear_refresh_tracking_for_test();

        assert!(super::mark_oauth_failure("claude_code"));
        assert!(super::mark_oauth_failure("claude_code_2"));
        assert!(!super::mark_oauth_failure("claude_code"));
        assert_eq!(super::oauth_failed_instance_count_for_test(), 2);
        assert!(super::clear_oauth_failure("claude_code"));
        assert_eq!(super::oauth_failed_instance_count_for_test(), 1);

        super::clear_refresh_tracking_for_test();
    }

    #[test]
    fn oauth_refresh_helper_collects_all_instances_for_base() {
        let providers = vec![
            ("claude_code".to_string(), "claude_code".to_string()),
            ("claude_code_work".to_string(), "claude_code".to_string()),
            ("openai_codex".to_string(), "openai_codex".to_string()),
        ];

        assert_eq!(
            super::collect_oauth_refresh_instances_for_base(providers, "claude_code"),
            vec!["claude_code".to_string(), "claude_code_work".to_string()]
        );
    }

    #[test]
    fn claude_code_auth_retry_does_not_reuse_rejected_token_with_newer_expiry() {
        assert!(!super::proactive_refresh_can_reuse_newer_expiry(
            Some(1),
            Some(reqwest::StatusCode::UNAUTHORIZED),
            i64::MAX,
        ));
        assert!(super::proactive_refresh_can_reuse_newer_expiry(
            Some(1),
            None,
            i64::MAX,
        ));
    }
}

