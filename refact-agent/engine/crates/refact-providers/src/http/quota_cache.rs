use std::sync::Arc;

use tokio::sync::Mutex as AMutex;

use crate::quota::{ProviderQuotaCache, ProviderQuotaSnapshot, ProviderQuotaSource};

pub fn stale_quota_fallback(
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

pub async fn invalidate_provider_quota(
    quota_cache: &Arc<AMutex<ProviderQuotaCache>>,
    provider_name: &str,
) {
    quota_cache.lock().await.invalidate(provider_name);
}
