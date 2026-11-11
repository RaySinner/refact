use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::SystemTime;

use serde::Deserialize;

use crate::path_utils::{any_glob_matches_path, canonical_path, canonicalize_normalized_path};

#[derive(Debug, Clone, Deserialize)]
pub struct IndexingSettings {
    #[serde(default)]
    pub blocklist: Vec<String>,
    #[serde(default)]
    pub additional_indexing_dirs: Vec<String>,
}

impl Default for IndexingSettings {
    fn default() -> Self {
        serde_yaml::from_str(include_str!(
            "../../../src/yaml_configs/default_indexing.yaml"
        ))
        .expect("src/yaml_configs/default_indexing.yaml to be valid IndexingSettings")
    }
}

pub struct IndexingEverywhere {
    pub global: IndexingSettings,
    pub vcs_indexing_settings_map: HashMap<String, IndexingSettings>,
    pub loaded_ts: u64,
}

impl Default for IndexingEverywhere {
    fn default() -> Self {
        IndexingEverywhere {
            global: IndexingSettings::default(),
            vcs_indexing_settings_map: HashMap::new(),
            loaded_ts: 0,
        }
    }
}

impl IndexingEverywhere {
    pub fn indexing_for_path(&self, path: &Path) -> IndexingSettings {
        assert!(path.is_absolute());
        let path = canonicalize_normalized_path(path.to_path_buf());
        let mut result: IndexingSettings = self.global.clone();

        let mut best_vcs: Option<IndexingSettings> = None;
        let mut best_pathbuf: Option<PathBuf> = None;
        for (vcs, vcs_settings) in &self.vcs_indexing_settings_map {
            let vcs_pathbuf = canonicalize_normalized_path(PathBuf::from(vcs));
            if path.starts_with(&vcs_pathbuf) {
                if best_vcs.is_none()
                    || vcs_pathbuf.components().count()
                        > best_pathbuf.clone().unwrap().components().count()
                {
                    best_vcs = Some(vcs_settings.clone());
                    best_pathbuf = Some(vcs_pathbuf);
                }
            }
        }

        if let Some(t) = best_vcs {
            result.blocklist.extend(t.blocklist);
            result
                .additional_indexing_dirs
                .extend(t.additional_indexing_dirs);
        }

        result
    }
}

const INDEXING_TOO_OLD_SECS: u64 = 30;

#[derive(Clone, PartialEq, Eq)]
struct IndexingFileStamp {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: Option<u64>,
}

struct IndexingCheckState {
    checked_ts: u64,
    loaded_ts: u64,
    stamps: Vec<IndexingFileStamp>,
}

static INDEXING_CHECK_STATES: OnceLock<Mutex<HashMap<usize, IndexingCheckState>>> = OnceLock::new();

async fn indexing_file_stamps(paths: &[PathBuf]) -> Vec<IndexingFileStamp> {
    let mut stamps = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = tokio::fs::metadata(path).await.ok();
        stamps.push(IndexingFileStamp {
            path: path.clone(),
            modified: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok()),
            len: metadata.as_ref().map(|metadata| metadata.len()),
        });
    }
    stamps
}

pub async fn load_indexing_yaml(
    indexing_yaml_path: &Path,
    relative_path_base: Option<&PathBuf>,
) -> Result<IndexingSettings, String> {
    let content = tokio::fs::read_to_string(&indexing_yaml_path)
        .await
        .map_err(|e| format!("load {} failed\n{}", indexing_yaml_path.display(), e))?;

    _load_indexing_yaml_str(&content, relative_path_base)
        .map_err(|e| format!("load {} failed\n{}", indexing_yaml_path.display(), e))
}

pub fn is_blocklisted(indexing_settings: &IndexingSettings, path: &Path) -> bool {
    any_glob_matches_path(&indexing_settings.blocklist, path)
}

pub async fn reload_indexing_everywhere_if_needed(
    config_dir: &Path,
    indexing_yaml: &str,
    workspace_vcs_roots: &[PathBuf],
    indexing_cache: &RwLock<Arc<IndexingEverywhere>>,
    cache_key: usize,
) -> Arc<IndexingEverywhere> {
    reload_indexing_everywhere(
        config_dir,
        indexing_yaml,
        workspace_vcs_roots,
        indexing_cache,
        cache_key,
        false,
    )
    .await
}

pub async fn reload_indexing_everywhere_now(
    config_dir: &Path,
    indexing_yaml: &str,
    workspace_vcs_roots: &[PathBuf],
    indexing_cache: &RwLock<Arc<IndexingEverywhere>>,
    cache_key: usize,
) -> Arc<IndexingEverywhere> {
    reload_indexing_everywhere(
        config_dir,
        indexing_yaml,
        workspace_vcs_roots,
        indexing_cache,
        cache_key,
        true,
    )
    .await
}

async fn reload_indexing_everywhere(
    config_dir: &Path,
    indexing_yaml: &str,
    workspace_vcs_roots: &[PathBuf],
    indexing_cache: &RwLock<Arc<IndexingEverywhere>>,
    cache_key: usize,
    force: bool,
) -> Arc<IndexingEverywhere> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cached = indexing_cache.read().unwrap().clone();
    let checked_ts = INDEXING_CHECK_STATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .get(&cache_key)
        .map(|state| state.checked_ts)
        .unwrap_or(cached.loaded_ts);
    if !force && checked_ts + INDEXING_TOO_OLD_SECS > now {
        return cached;
    }

    let global_indexing_path = if indexing_yaml.is_empty() {
        config_dir.join("indexing.yaml")
    } else {
        canonical_path(indexing_yaml)
    };
    let vcs_dirs: Vec<PathBuf> = workspace_vcs_roots
        .iter()
        .cloned()
        .map(canonicalize_normalized_path)
        .collect();
    let mut indexing_paths = Vec::with_capacity(vcs_dirs.len() + 1);
    indexing_paths.push(global_indexing_path.clone());
    indexing_paths.extend(
        vcs_dirs
            .iter()
            .map(|root| root.join(".refact").join("indexing.yaml")),
    );
    let stamps = indexing_file_stamps(&indexing_paths).await;
    if !force {
        let mut states = INDEXING_CHECK_STATES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();
        if states
            .get(&cache_key)
            .map(|state| state.loaded_ts == cached.loaded_ts && state.stamps == stamps)
            .unwrap_or(false)
        {
            states.get_mut(&cache_key).unwrap().checked_ts = now;
            return cached;
        }
    }

    let indexing_everywhere = {
        let global = {
            load_indexing_yaml(&global_indexing_path, None)
                .await
                .unwrap_or_else(|e| {
                    tracing::error!("cannot load {:?}: {}, fallback to defaults", config_dir, e);
                    IndexingSettings::default()
                })
        };

        let mut vcs_indexing_settings_map: HashMap<String, IndexingSettings> = HashMap::new();
        for indexing_root in vcs_dirs {
            let indexing_path = indexing_root.join(".refact").join("indexing.yaml");
            if indexing_path.exists() {
                match load_indexing_yaml(&indexing_path, Some(&indexing_root)).await {
                    Ok(indexing_settings) => {
                        vcs_indexing_settings_map.insert(
                            indexing_root.to_str().unwrap().to_string(),
                            indexing_settings,
                        );
                    }
                    Err(e) => {
                        tracing::error!("{}, skip", e);
                    }
                }
            }
        }
        IndexingEverywhere {
            global,
            vcs_indexing_settings_map,
            loaded_ts: now,
        }
    };

    let indexing_everywhere = Arc::new(indexing_everywhere);
    *indexing_cache.write().unwrap() = indexing_everywhere.clone();
    INDEXING_CHECK_STATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(
            cache_key,
            IndexingCheckState {
                checked_ts: now,
                loaded_ts: now,
                stamps,
            },
        );
    indexing_everywhere
}

fn _load_indexing_yaml_str(
    indexing_yaml_str: &str,
    relative_path_base: Option<&PathBuf>,
) -> Result<IndexingSettings, String> {
    match serde_yaml::from_str::<IndexingSettings>(indexing_yaml_str) {
        Ok(indexing_settings) => {
            let mut additional_indexing_dirs = vec![];
            for indexing_dir in indexing_settings.additional_indexing_dirs.iter() {
                if indexing_dir.is_empty() {
                    continue;
                }
                let expanded_dir = if indexing_dir.starts_with("~") {
                    if let Some(without_tilde) = indexing_dir.strip_prefix("~") {
                        let home_dir = PathBuf::from(
                            &home::home_dir()
                                .ok_or(())
                                .expect("failed to find home dir")
                                .to_string_lossy()
                                .to_string(),
                        );
                        home_dir
                            .join(without_tilde.trim_start_matches('/'))
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        indexing_dir.clone()
                    }
                } else {
                    indexing_dir.clone()
                };
                let indexing_dir_path = PathBuf::from(&expanded_dir);
                if indexing_dir_path.is_absolute() {
                    let normalized = canonical_path(&expanded_dir)
                        .to_string_lossy()
                        .into_owned();
                    additional_indexing_dirs.push(normalized);
                } else {
                    if let Some(b) = relative_path_base {
                        let joined_path = b.join(&expanded_dir).to_str().unwrap().to_string();
                        let normalized = canonical_path(&joined_path).to_string_lossy().into_owned();
                        additional_indexing_dirs.push(normalized);
                    } else {
                        tracing::error!(
                            "can't have relative path {} in the global indexing.yaml",
                            indexing_dir
                        )
                    }
                }
            }
            Ok(IndexingSettings {
                blocklist: indexing_settings.blocklist,
                additional_indexing_dirs,
            })
        }
        Err(e) => Err(format!("{}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexing_everywhere_default_starts_unloaded() {
        let indexing = IndexingEverywhere::default();

        assert!(indexing.vcs_indexing_settings_map.is_empty());
        assert_eq!(indexing.loaded_ts, 0);
    }

    #[tokio::test]
    async fn forced_reload_replaces_the_cached_indexing_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        tokio::fs::create_dir_all(&config_dir).await.unwrap();
        let config_path = config_dir.join("indexing.yaml");
        let cache = RwLock::new(Arc::new(IndexingEverywhere::default()));

        tokio::fs::write(&config_path, "blocklist:\n  - first/*\n")
            .await
            .unwrap();
        let first =
            reload_indexing_everywhere_now(&config_dir, "", &[], &cache, 1).await;
        assert_eq!(first.global.blocklist, vec!["first/*"]);

        tokio::fs::write(&config_path, "blocklist:\n  - second/*\n")
            .await
            .unwrap();
        let second =
            reload_indexing_everywhere_now(&config_dir, "", &[], &cache, 1).await;
        let cached = cache.read().unwrap().clone();

        assert_eq!(second.global.blocklist, vec!["second/*"]);
        assert_eq!(cached.global.blocklist, vec!["second/*"]);
    }
}
