use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use refact_files::file_filter::KNOWLEDGE_FOLDER_NAME;
use refact_knowledge_graph::kg_structs::KnowledgeFrontmatter;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use tokio::sync::mpsc;

use crate::files_correction::get_project_dirs;
use crate::global_context::GlobalContext;
use crate::ChatCtx;

pub use refact_core::knowledge_index::{
    format_related_memories_section, KnowledgeCard, KnowledgeIndex, KnowledgeSearchFilters,
    KnowledgeSearchHit,
};

#[async_trait]
pub trait KnowledgeBuilder: Send + Sync {
    async fn build_knowledge_index_for_ctx(&self, ctx: &ChatCtx) -> KnowledgeIndex;
}

pub struct EngineKnowledgeBuilder;

#[async_trait]
impl KnowledgeBuilder for EngineKnowledgeBuilder {
    async fn build_knowledge_index_for_ctx(&self, ctx: &ChatCtx) -> KnowledgeIndex {
        build_knowledge_index_for_ctx_impl(ctx).await
    }
}

pub async fn build_knowledge_index_for_ctx(ctx: &ChatCtx) -> KnowledgeIndex {
    ctx.knowledge_builder
        .build_knowledge_index_for_ctx(ctx)
        .await
}



fn path_has_any_relative_component(path: &Path, root: &Path, components: &[&str]) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    path_components_match(relative, components)
}

fn path_components_match(path: &Path, components: &[&str]) -> bool {
    path.components().any(|c| {
        let candidate = c.as_os_str().to_string_lossy();
        components.iter().any(|component| candidate == *component)
    })
}

fn is_tmp_path(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    file_name.ends_with(".tmp") || file_name.contains(".tmp-")
}

fn push_unique(values: &mut Vec<String>, value: impl Into<String>) {
    let value = value.into();
    if !value.trim().is_empty() && !values.contains(&value) {
        values.push(value);
    }
}

fn first_nonempty_line(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return Some(trimmed.trim_start_matches('#').trim().to_string());
    }
    None
}

fn yaml_value_string(value: &YamlValue) -> Option<String> {
    match value {
        YamlValue::String(value) => Some(value.clone()),
        YamlValue::Number(value) => Some(value.to_string()),
        YamlValue::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn yaml_string(mapping: &YamlMapping, key: &str) -> Option<String> {
    mapping
        .get(&YamlValue::String(key.to_string()))
        .and_then(yaml_value_string)
}

fn yaml_string_list(mapping: &YamlMapping, key: &str) -> Vec<String> {
    let Some(value) = mapping.get(&YamlValue::String(key.to_string())) else {
        return Vec::new();
    };
    match value {
        YamlValue::Sequence(values) => values.iter().filter_map(yaml_value_string).collect(),
        YamlValue::String(value) => value
            .split(',')
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_yaml_frontmatter(text: &str) -> (YamlMapping, usize) {
    if !text.starts_with("---") {
        return (YamlMapping::new(), 0);
    }
    let rest = &text[3..];
    let Some(end_idx) = rest.find("\n---") else {
        return (YamlMapping::new(), 0);
    };
    let yaml_content = &rest[..end_idx];
    let mut end_offset = 3 + end_idx + 4;
    if text.len() > end_offset && text.as_bytes().get(end_offset) == Some(&b'\n') {
        end_offset += 1;
    }
    let mapping = match serde_yaml::from_str::<YamlValue>(yaml_content) {
        Ok(YamlValue::Mapping(mapping)) => mapping,
        _ => YamlMapping::new(),
    };
    (mapping, end_offset)
}

fn mapping_is_inactive(mapping: &YamlMapping) -> bool {
    matches!(
        yaml_string(mapping, "status")
            .unwrap_or_else(|| "active".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "archived" | "deprecated" | "superseded"
    )
}

fn component_matches(component: Component<'_>, expected: &str) -> bool {
    matches!(component, Component::Normal(value) if value.to_str() == Some(expected))
}

pub fn extract_task_id_from_path(path: &Path) -> Option<String> {
    let components = path.components().collect::<Vec<_>>();
    for window in components.windows(3) {
        if component_matches(window[0], ".refact") && component_matches(window[1], "tasks") {
            if let Component::Normal(task_id) = window[2] {
                return Some(task_id.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn task_card_tags(mapping: &YamlMapping) -> Vec<String> {
    let mut tags = Vec::new();
    for field in ["card_id", "relevant_cards"] {
        for value in yaml_string_list(mapping, field) {
            push_unique(&mut tags, format!("scope:card:{}", value));
        }
    }
    if let Some(value) = yaml_string(mapping, "card_id") {
        push_unique(&mut tags, format!("scope:card:{}", value));
    }
    tags
}

fn task_card_from_mapping(
    mapping: &YamlMapping,
    path: &Path,
    directory_kind: &str,
    body: &str,
) -> KnowledgeCard {
    let task_id = yaml_string(mapping, "task_id").or_else(|| extract_task_id_from_path(path));
    let title = yaml_string(mapping, "title")
        .or_else(|| yaml_string(mapping, "name"))
        .unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
    let kind = yaml_string(mapping, "kind").or_else(|| {
        if directory_kind == "memories" {
            Some("freeform".to_string())
        } else {
            None
        }
    });
    let namespace = yaml_string(mapping, "namespace").unwrap_or_else(|| "task".to_string());
    let mut tags = yaml_string_list(mapping, "tags");
    push_unique(&mut tags, "scope:task");
    push_unique(&mut tags, format!("type:{}", directory_kind));
    push_unique(&mut tags, format!("namespace:{}", namespace));
    if let Some(task_id) = &task_id {
        push_unique(&mut tags, format!("scope:task:{}", task_id));
    }
    if let Some(kind) = &kind {
        push_unique(&mut tags, format!("kind:{}", kind));
    }
    for tag in task_card_tags(mapping) {
        push_unique(&mut tags, tag);
    }

    let mut filenames = Vec::new();
    if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
        filenames.push(file_name.to_string());
    }
    if let Some(stem) = path.file_stem().and_then(|name| name.to_str()) {
        push_unique(&mut filenames, stem.to_string());
    }
    if let Some(slug) = yaml_string(mapping, "slug") {
        push_unique(&mut filenames, slug);
    }

    KnowledgeCard {
        id: path.to_string_lossy().to_string(),
        title,
        summary: first_nonempty_line(body),
        description: None,
        tags,
        filenames,
        entities: Vec::new(),
        related_files: Vec::new(),
        related_entities: Vec::new(),
        kind,
        created: None,
        created_at: yaml_string(mapping, "created_at"),
        updated: yaml_string(mapping, "updated_at"),
        file_path: path.to_path_buf(),
    }
}

pub async fn knowledge_dir_candidates(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = get_project_dirs(gcx.clone())
        .await
        .into_iter()
        .map(|d| d.join(KNOWLEDGE_FOLDER_NAME))
        .collect();
    let global_dir = gcx.config_dir.join("knowledge");
    dirs.push(global_dir);
    dirs.sort();
    dirs.dedup();
    dirs
}

async fn knowledge_dirs_for_index(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    knowledge_dir_candidates(gcx)
        .await
        .into_iter()
        .filter(|dir| dir.exists())
        .collect()
}

pub async fn knowledge_watch_roots(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    let roots = knowledge_dir_candidates(gcx).await;
    for root in &roots {
        let _ = tokio::fs::create_dir_all(root).await;
    }
    roots
}

fn is_under_knowledge_root(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

pub async fn refresh_knowledge_index_path(
    gcx: Arc<GlobalContext>,
    path: &Path,
    is_remove: bool,
) -> bool {
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return false;
    }
    let roots = knowledge_dir_candidates(gcx.clone()).await;
    if !is_under_knowledge_root(path, &roots) {
        return false;
    }

    if is_remove {
        let mut index = gcx.knowledge_index.lock().await;
        index.remove_path(path);
        index.remove_paths_under(path);
        return true;
    }

    let root = match roots.iter().find(|root| path.starts_with(root)) {
        Some(root) => root,
        None => return false,
    };
    if !should_index_markdown_path(path, root, &["archive", "archived", ".history"]) {
        let mut index = gcx.knowledge_index.lock().await;
        index.remove_path(path);
        return true;
    }
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
        _ => {
            let mut index = gcx.knowledge_index.lock().await;
            index.remove_path(path);
            return true;
        }
    };
    if !metadata.is_file() {
        return true;
    }
    let text = match tokio::fs::read_to_string(path).await {
        Ok(text) => text,
        Err(_) => return true,
    };
    let (frontmatter, content_start) = KnowledgeFrontmatter::parse(&text);
    let content = text.get(content_start..).unwrap_or("");
    let mut index = gcx.knowledge_index.lock().await;
    index.remove_path(path);
    if frontmatter.is_archived() || frontmatter.is_deprecated() {
        return true;
    }
    let path = path.to_path_buf();
    index.add_signature(
        refact_buddy_core::memory_dedup::content_signature(content),
        path.clone(),
    );
    index.add_from_frontmatter(path, &frontmatter, Some(content));
    true
}

pub async fn knowledge_index_watcher_background_task(gcx: Arc<GlobalContext>) {
    let roots = knowledge_watch_roots(gcx.clone()).await;
    if roots.is_empty() {
        tracing::info!("knowledge_index: watcher skipped because no knowledge roots exist");
        return;
    }
    let knowledge_dirs = knowledge_dir_candidates(gcx.clone()).await;
    let (tx, mut rx) = mpsc::channel(1024);
    let dropped_events = Arc::new(AtomicUsize::new(0));
    let dropped_events_for_callback = dropped_events.clone();
    let mut watcher = match RecommendedWatcher::new(
        move |result: Result<notify::Event, notify::Error>| {
            if let Ok(event) = result {
                if event
                    .paths
                    .iter()
                    .any(|path| is_under_knowledge_root(path, &knowledge_dirs))
                {
                    if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(event) {
                        let dropped =
                            dropped_events_for_callback.fetch_add(1, Ordering::Relaxed) + 1;
                        if dropped % 100 == 0 {
                            tracing::warn!("knowledge_index: dropped {dropped} watcher events because the channel is full");
                        }
                    }
                }
            }
        },
        Config::default(),
    ) {
        Ok(watcher) => watcher,
        Err(error) => {
            tracing::warn!("knowledge_index: failed to create watcher: {error}");
            return;
        }
    };
    for root in &roots {
        if let Err(error) = watcher.watch(root, RecursiveMode::Recursive) {
            tracing::warn!(
                "knowledge_index: failed to watch {}: {error}",
                root.display()
            );
        }
    }

    let mut pending_paths = std::collections::HashSet::<PathBuf>::new();
    let mut pending_since = None;
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Some(event) => {
                    if matches!(event.kind, notify::EventKind::Create(_) | notify::EventKind::Modify(_) | notify::EventKind::Remove(_)) {
                        for path in event.paths {
                            pending_paths.insert(path);
                        }
                        if pending_since.is_none() {
                            pending_since = Some(Instant::now());
                        }
                    }
                }
                None => break,
            },
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                if gcx.shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
            }
        }
        if pending_since.is_some_and(|since| since.elapsed() >= Duration::from_millis(300)) {
            for path in pending_paths.drain() {
                let is_remove = !path.exists();
                refresh_knowledge_index_path(gcx.clone(), &path, is_remove).await;
            }
            pending_since = None;
        }
    }
}

pub async fn build_knowledge_index(gcx: Arc<GlobalContext>) -> KnowledgeIndex {
    let mut index = KnowledgeIndex::empty();

    scan_knowledge_dirs(&mut index, knowledge_dirs_for_index(gcx.clone()).await).await;

    let task_dirs = crate::task_storage::get_all_tasks_dirs(gcx).await;
    scan_task_dirs(&mut index, task_dirs).await;

    index.mark_ready();
    index
}

async fn knowledge_dirs_for_index_for_ctx(ctx: &ChatCtx) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = crate::files_correction::get_unscoped_project_dirs_for_ctx(ctx)
        .await
        .into_iter()
        .map(|dir| dir.join(KNOWLEDGE_FOLDER_NAME))
        .collect();
    dirs.push(ctx.paths.config_dir.join("knowledge"));
    dirs.sort();
    dirs.dedup();
    dirs.into_iter().filter(|dir| dir.exists()).collect()
}

pub async fn build_knowledge_index_for_ctx_impl(ctx: &ChatCtx) -> KnowledgeIndex {
    let mut index = KnowledgeIndex::empty();

    scan_knowledge_dirs(&mut index, knowledge_dirs_for_index_for_ctx(ctx).await).await;

    let task_dirs = crate::task_storage::get_all_tasks_dirs_for_ctx(ctx).await;
    scan_task_dirs(&mut index, task_dirs).await;

    index.mark_ready();
    index
}

async fn scan_knowledge_dirs(index: &mut KnowledgeIndex, knowledge_dirs: Vec<PathBuf>) {
    for path_buf in collect_knowledge_markdown_paths(knowledge_dirs).await {
        let text = match tokio::fs::read_to_string(&path_buf).await {
            Ok(t) => t,
            Err(_) => continue,
        };
        let (fm, content_start) = KnowledgeFrontmatter::parse(&text);
        if fm.is_archived() || fm.is_deprecated() {
            continue;
        }

        let content_slice = text.get(content_start..).unwrap_or("");
        index.add_signature(
            refact_buddy_core::memory_dedup::content_signature(content_slice),
            path_buf.clone(),
        );
        index.add_from_frontmatter(path_buf, &fm, Some(content_slice));
    }
}

async fn collect_knowledge_markdown_paths(knowledge_dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    tokio::task::spawn_blocking(move || collect_knowledge_markdown_paths_blocking(knowledge_dirs))
        .await
        .unwrap_or_default()
}

fn collect_knowledge_markdown_paths_blocking(knowledge_dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for dir in knowledge_dirs {
        for entry in walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if should_index_markdown_path(path, &dir, &["archive", "archived", ".history"]) {
                paths.push(path.to_path_buf());
            }
        }
    }
    paths
}

fn should_index_markdown_path(path: &Path, root: &Path, ignored_components: &[&str]) -> bool {
    if !path.is_file() {
        return false;
    }
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return false;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "md" && ext != "mdx" {
        return false;
    }
    !path_has_any_relative_component(path, root, ignored_components) && !is_tmp_path(path)
}

#[derive(Debug, Clone)]
struct TaskMarkdownPath {
    path: PathBuf,
    directory_kind: &'static str,
}

async fn collect_task_markdown_paths(task_roots: Vec<PathBuf>) -> Vec<TaskMarkdownPath> {
    tokio::task::spawn_blocking(move || collect_task_markdown_paths_blocking(task_roots))
        .await
        .unwrap_or_default()
}

fn collect_task_markdown_paths_blocking(task_roots: Vec<PathBuf>) -> Vec<TaskMarkdownPath> {
    let mut paths = Vec::new();
    for tasks_dir in task_roots {
        let task_entries = match std::fs::read_dir(&tasks_dir) {
            Ok(entries) => entries.filter_map(|entry| entry.ok()).collect::<Vec<_>>(),
            Err(_) => continue,
        };
        for task_entry in task_entries {
            let task_dir = task_entry.path();
            if !task_dir.is_dir() {
                continue;
            }
            for subdir in ["memories", "documents"] {
                let scan_dir = task_dir.join(subdir);
                if !scan_dir.exists() {
                    continue;
                }
                for entry in walkdir::WalkDir::new(&scan_dir)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    let path = entry.path();
                    if should_index_markdown_path(
                        path,
                        &scan_dir,
                        &[".history", "archived", "archive"],
                    ) {
                        paths.push(TaskMarkdownPath {
                            path: path.to_path_buf(),
                            directory_kind: subdir,
                        });
                    }
                }
            }
        }
    }
    paths
}

async fn scan_task_dirs(index: &mut KnowledgeIndex, task_roots: Vec<PathBuf>) {
    for task_path in collect_task_markdown_paths(task_roots).await {
        let text = match tokio::fs::read_to_string(&task_path.path).await {
            Ok(t) => t,
            Err(_) => continue,
        };
        let (mapping, content_start) = parse_yaml_frontmatter(&text);
        if mapping_is_inactive(&mapping) {
            continue;
        }
        let content_slice = text.get(content_start..).unwrap_or("");
        let card = task_card_from_mapping(
            &mapping,
            &task_path.path,
            task_path.directory_kind,
            content_slice,
        );
        index.add_card_with_content(card, Some(content_slice));
    }
}
