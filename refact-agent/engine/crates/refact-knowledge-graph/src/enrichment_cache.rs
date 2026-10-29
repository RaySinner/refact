use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::memo::MemoRecord;

const ENRICHMENT_CACHE_MAX_ENTRIES: usize = 128;
const ENRICHMENT_CACHE_MAX_BYTES: usize = 4 * 1024 * 1024;
const ENRICHMENT_CACHE_TTL: Duration = Duration::from_secs(30);
const ENRICHMENT_EMPTY_CACHE_TTL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EnrichmentCacheKey {
    pub query_fingerprint: [u8; 32],
    pub workspace_scope_fingerprint: [u8; 32],
    pub allowed_roots_fingerprint: [u8; 32],
    pub privacy_generation: u64,
    pub index_generation: u64,
    pub embedding_config_fingerprint: [u8; 32],
    pub current_root_fingerprint: [u8; 32],
    pub top_n_memories: usize,
    pub top_n_trajectories: usize,
    pub score_threshold_bits: u32,
}

#[derive(Clone)]
pub struct MemoSourceFingerprint {
    pub path: PathBuf,
    pub size: u64,
    pub modified_ns: u128,
    pub content_fingerprint: [u8; 32],
}

#[derive(Clone)]
pub struct CachedMemo {
    pub memo: MemoRecord,
    pub source: MemoSourceFingerprint,
}

#[derive(Clone)]
pub struct CachedEnrichmentResult {
    pub memories: Vec<CachedMemo>,
    pub result_fingerprint: [u8; 32],
    pub bytes: usize,
    pub cacheable: bool,
}

impl CachedEnrichmentResult {
    pub fn empty() -> Self {
        Self {
            memories: Vec::new(),
            result_fingerprint: fingerprint_bytes(&[b"empty-enrichment-result"]),
            bytes: 0,
            cacheable: true,
        }
    }
}

struct EnrichmentCacheEntry {
    result: CachedEnrichmentResult,
    expires_at: Instant,
}

#[derive(Default)]
struct EnrichmentCacheState {
    entries: HashMap<EnrichmentCacheKey, EnrichmentCacheEntry>,
    lru: VecDeque<EnrichmentCacheKey>,
    inflight: HashMap<EnrichmentCacheKey, watch::Sender<bool>>,
    bytes: usize,
}

#[derive(Default)]
pub struct EnrichmentCache {
    state: StdMutex<EnrichmentCacheState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnrichmentCacheDisposition {
    Hit,
    Miss,
    Coalesced,
}

impl EnrichmentCache {
    pub async fn get_or_fetch<F, Fut>(
        &self,
        key: EnrichmentCacheKey,
        fetch: F,
    ) -> Result<(CachedEnrichmentResult, EnrichmentCacheDisposition), String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<CachedEnrichmentResult, String>>,
    {
        let mut fetch = Some(fetch);
        let mut coalesced = false;
        loop {
            enum Next {
                Hit(CachedEnrichmentResult),
                Wait(watch::Receiver<bool>),
                Fetch,
            }
            let next = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.purge_expired();
                if let Some(entry) = state.entries.get(&key) {
                    let result = entry.result.clone();
                    state.touch(&key);
                    Next::Hit(result)
                } else if let Some(sender) = state.inflight.get(&key) {
                    Next::Wait(sender.subscribe())
                } else {
                    let (sender, _) = watch::channel(false);
                    state.inflight.insert(key, sender);
                    Next::Fetch
                }
            };

            match next {
                Next::Hit(result) => {
                    return Ok((
                        result,
                        if coalesced {
                            EnrichmentCacheDisposition::Coalesced
                        } else {
                            EnrichmentCacheDisposition::Hit
                        },
                    ));
                }
                Next::Wait(mut receiver) => {
                    coalesced = true;
                    let _ = receiver.changed().await;
                }
                Next::Fetch => {
                    let mut inflight = EnrichmentInFlightGuard {
                        cache: self,
                        key,
                        active: true,
                    };
                    let result = fetch.take().expect("enrichment cache fetch used once")().await;
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let sender = state.inflight.remove(&key);
                    if let Ok(result) = &result {
                        if result.cacheable {
                            state.insert(key, result.clone());
                        }
                    }
                    if let Some(sender) = sender {
                        sender.send_replace(true);
                    }
                    inflight.active = false;
                    return result.map(|result| (result, EnrichmentCacheDisposition::Miss));
                }
            }
        }
    }

    pub fn invalidate(&self, key: &EnrichmentCacheKey) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = state.entries.remove(key) {
            state.bytes = state.bytes.saturating_sub(entry.result.bytes);
        }
        state.lru.retain(|cached_key| cached_key != key);
    }
}

struct EnrichmentInFlightGuard<'a> {
    cache: &'a EnrichmentCache,
    key: EnrichmentCacheKey,
    active: bool,
}

impl Drop for EnrichmentInFlightGuard<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .cache
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(sender) = state.inflight.remove(&self.key) {
            sender.send_replace(true);
        }
    }
}

impl EnrichmentCacheState {
    fn purge_expired(&mut self) {
        let now = Instant::now();
        let expired = self
            .entries
            .iter()
            .filter_map(|(key, entry)| (entry.expires_at <= now).then_some(*key))
            .collect::<Vec<_>>();
        for key in expired {
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(entry.result.bytes);
            }
        }
        self.lru.retain(|key| self.entries.contains_key(key));
    }

    fn touch(&mut self, key: &EnrichmentCacheKey) {
        self.lru.retain(|cached_key| cached_key != key);
        self.lru.push_back(*key);
    }

    fn insert(&mut self, key: EnrichmentCacheKey, result: CachedEnrichmentResult) {
        if let Some(previous) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(previous.result.bytes);
        }
        let ttl = if result.memories.is_empty() {
            ENRICHMENT_EMPTY_CACHE_TTL
        } else {
            ENRICHMENT_CACHE_TTL
        };
        self.bytes = self.bytes.saturating_add(result.bytes);
        self.entries.insert(
            key,
            EnrichmentCacheEntry {
                result,
                expires_at: Instant::now() + ttl,
            },
        );
        self.touch(&key);
        while self.entries.len() > ENRICHMENT_CACHE_MAX_ENTRIES
            || self.bytes > ENRICHMENT_CACHE_MAX_BYTES
        {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(entry.result.bytes);
            }
        }
    }
}

pub fn fingerprint_bytes(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

pub fn fingerprint_strings<I>(parts: I) -> [u8; 32]
where
    I: IntoIterator<Item = String>,
{
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.finalize().into()
}

pub fn fingerprint_hex(fingerprint: &[u8; 32]) -> String {
    hex::encode(fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_key(seed: u8) -> EnrichmentCacheKey {
        EnrichmentCacheKey {
            query_fingerprint: [seed; 32],
            workspace_scope_fingerprint: [1; 32],
            allowed_roots_fingerprint: [2; 32],
            privacy_generation: 1,
            index_generation: 1,
            embedding_config_fingerprint: [3; 32],
            current_root_fingerprint: [4; 32],
            top_n_memories: 5,
            top_n_trajectories: 3,
            score_threshold_bits: 0.75f32.to_bits(),
        }
    }

    #[test]
    fn enrichment_cache_evicts_lru_entries_with_bounded_memory() {
        let mut state = EnrichmentCacheState::default();
        for seed in 0..=ENRICHMENT_CACHE_MAX_ENTRIES {
            let mut key = cache_key(1);
            key.privacy_generation = seed as u64;
            state.insert(key, CachedEnrichmentResult::empty());
        }

        assert!(state.entries.len() <= ENRICHMENT_CACHE_MAX_ENTRIES);
        let mut oldest = cache_key(1);
        oldest.privacy_generation = 0;
        assert!(!state.entries.contains_key(&oldest));
    }
}
