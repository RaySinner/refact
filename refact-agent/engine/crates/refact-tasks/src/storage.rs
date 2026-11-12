use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use git2::Repository;
use tokio::fs;
use tracing::warn;
use uuid::Uuid;

use crate::types::{BoardCard, TaskBoard, TaskMeta, TaskStatus};

pub trait TaskPaths {
    fn task_project_dirs(&self) -> Vec<PathBuf>;
    fn task_config_dir(&self) -> PathBuf;
}

const TASKS_DIR: &str = "tasks";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardConflict {
    pub expected: u64,
    pub actual: u64,
}

impl BoardConflict {
    pub fn message(&self) -> String {
        format!(
            "Board rev mismatch: expected {}, actual {}",
            self.expected, self.actual
        )
    }
}

pub async fn get_tasks_dir_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
) -> Result<PathBuf, String> {
    let project_dirs = ctx.task_project_dirs();
    let workspace_root = project_dirs.first().ok_or("No workspace folder found")?;
    Ok(workspace_root.join(".refact").join(TASKS_DIR))
}

pub fn get_global_tasks_dir_for_ctx(ctx: &(impl TaskPaths + ?Sized)) -> PathBuf {
    ctx.task_config_dir().join(TASKS_DIR)
}

pub async fn ensure_tasks_dir_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
) -> Result<PathBuf, String> {
    let dir = get_tasks_dir_for_ctx(ctx).await?;
    if !dir.exists() {
        fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

pub async fn list_tasks_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
) -> Result<Vec<TaskMeta>, String> {
    let tasks_dirs = get_all_tasks_dirs_for_ctx(ctx).await;

    let mut tasks = vec![];
    let mut seen_ids = HashSet::new();

    for tasks_dir in tasks_dirs {
        if !tasks_dir.exists() {
            continue;
        }

        match crate::index::list_tasks_from_index_or_rebuild(&tasks_dir).await {
            Ok(metas) => {
                for meta in metas {
                    if seen_ids.insert(meta.id.clone()) {
                        tasks.push(meta);
                    }
                }
            }
            Err(e) => warn!("Failed to list tasks from {:?}: {}", tasks_dir, e),
        }
    }

    tasks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(tasks)
}

pub async fn save_task_meta_file_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
    task_id: &str,
    meta: &TaskMeta,
) -> Result<(), String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let meta_path = task_dir.join("meta.yaml");
    let content = serde_yaml::to_string(meta).map_err(|e| e.to_string())?;
    fs::write(&meta_path, content)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(tasks_dir) = task_dir.parent() {
        crate::index::upsert_task_index_entry(tasks_dir, meta).await?;
    }
    Ok(())
}

pub async fn load_planner_instructions_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
    task_id: &str,
) -> Result<String, String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let path = task_dir.join("planner_instructions.md");
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(&path).await.map_err(|e| e.to_string())
}

pub async fn save_planner_instructions_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
    task_id: &str,
    content: &str,
) -> Result<(), String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let path = task_dir.join("planner_instructions.md");
    fs::write(&path, content).await.map_err(|e| e.to_string())
}

pub async fn delete_task_files_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
    task_id: &str,
) -> Result<(), String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let tasks_dir = task_dir.parent().map(Path::to_path_buf);
    fs::remove_dir_all(&task_dir)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(tasks_dir) = tasks_dir {
        if let Err(e) = crate::index::remove_task_index_entry(&tasks_dir, task_id).await {
            warn!(
                "Failed to remove task {} from index {:?}: {}",
                task_id, tasks_dir, e
            );
        }
    }
    Ok(())
}

pub fn get_task_trajectory_dir(task_dir: &PathBuf, role: &str, agent_id: Option<&str>) -> PathBuf {
    let base = task_dir.join("trajectories").join(role);
    match agent_id {
        Some(id) => base.join(id),
        None => base,
    }
}

fn detect_git_branch_and_commit(workspace_root: &Path) -> (Option<String>, Option<String>) {
    let repo = match Repository::open(workspace_root) {
        Ok(r) => r,
        Err(_) => return (None, None),
    };

    let branch = match repo.head() {
        Ok(head) => head.shorthand().map(|s| s.to_string()),
        Err(_) => None,
    };

    let commit = match repo.head() {
        Ok(head) => match head.peel_to_commit() {
            Ok(c) => Some(c.id().to_string()),
            Err(_) => None,
        },
        Err(_) => None,
    };

    (branch, commit)
}

pub async fn create_task_files_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
    name: &str,
) -> Result<TaskMeta, String> {
    let tasks_dir = ensure_tasks_dir_for_ctx(ctx).await?;
    let task_id = Uuid::new_v4().to_string();
    let task_dir = tasks_dir.join(&task_id);

    fs::create_dir_all(&task_dir)
        .await
        .map_err(|e| e.to_string())?;
    fs::create_dir_all(task_dir.join("trajectories").join("planner"))
        .await
        .map_err(|e| e.to_string())?;
    fs::create_dir_all(task_dir.join("trajectories").join("agents"))
        .await
        .map_err(|e| e.to_string())?;

    let project_dirs = ctx.task_project_dirs();
    let (base_branch, base_commit) = project_dirs
        .first()
        .map(|p| detect_git_branch_and_commit(p))
        .unwrap_or((None, None));

    let now = Utc::now().to_rfc3339();
    let has_user_provided_name = !name.trim().is_empty() && name.to_lowercase() != "new task";
    let meta = TaskMeta {
        schema_version: 1,
        id: task_id.clone(),
        name: if has_user_provided_name {
            name.to_string()
        } else {
            "New Task".to_string()
        },
        status: TaskStatus::Planning,
        created_at: now.clone(),
        updated_at: now,
        cards_total: 0,
        cards_done: 0,
        cards_failed: 0,
        agents_active: 0,
        base_branch,
        base_commit,
        default_agent_model: None,
        is_name_generated: has_user_provided_name,
        last_agents_summary_at: None,
        planner_session_state: None,
    };

    save_task_meta_file_for_ctx(ctx, &task_id, &meta).await?;
    save_board_for_ctx(ctx, &task_id, &TaskBoard::default()).await?;
    save_planner_instructions_for_ctx(ctx, &task_id, "").await?;

    Ok(meta)
}

async fn atomic_write_file(tmp_path: &Path, dest_path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    if dest_path.exists() {
        fs::remove_file(dest_path)
            .await
            .map_err(|e| format!("Failed to remove existing file: {}", e))?;
    }
    fs::rename(tmp_path, dest_path)
        .await
        .map_err(|e| format!("Failed to rename: {}", e))
}

pub async fn get_all_tasks_dirs_for_ctx(ctx: &(impl TaskPaths + ?Sized)) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ctx
        .task_project_dirs()
        .into_iter()
        .map(|p| p.join(".refact").join(TASKS_DIR))
        .filter(|p| p.exists())
        .collect();

    let global_dir = ctx.task_config_dir().join(TASKS_DIR);
    if global_dir.exists() {
        dirs.push(global_dir);
    }

    dirs
}

pub async fn find_task_dir_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
    task_id: &str,
) -> Result<PathBuf, String> {
    validate_task_id(task_id)?;
    for tasks_dir in get_all_tasks_dirs_for_ctx(ctx).await {
        let candidate = tasks_dir.join(task_id);
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    Err(format!("Task not found: {}", task_id))
}

pub fn validate_task_id(task_id: &str) -> Result<(), String> {
    if task_id.is_empty() {
        return Err("Task ID cannot be empty".into());
    }
    if task_id.len() > 128 {
        return Err("Task ID too long".into());
    }
    if !task_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "Task ID must contain only alphanumeric characters, hyphens, or underscores".into(),
        );
    }
    Ok(())
}

async fn load_task_meta_from_path(path: &PathBuf) -> Result<TaskMeta, String> {
    let content = fs::read_to_string(path).await.map_err(|e| e.to_string())?;
    serde_yaml::from_str(&content).map_err(|e| e.to_string())
}

pub async fn load_task_meta_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
    task_id: &str,
) -> Result<TaskMeta, String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let meta_path = task_dir.join("meta.yaml");
    load_task_meta_from_path(&meta_path).await
}

pub async fn load_board_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
    task_id: &str,
) -> Result<TaskBoard, String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let board_path = task_dir.join("board.yaml");
    if !board_path.exists() {
        return Ok(TaskBoard::default());
    }
    let content = fs::read_to_string(&board_path)
        .await
        .map_err(|e| e.to_string())?;
    serde_yaml::from_str(&content).map_err(|e| e.to_string())
}

pub async fn save_board_for_ctx(
    ctx: &(impl TaskPaths + ?Sized),
    task_id: &str,
    board: &TaskBoard,
) -> Result<(), String> {
    let task_dir = find_task_dir_for_ctx(ctx, task_id).await?;
    let board_path = task_dir.join("board.yaml");
    let tmp_path = task_dir.join("board.yaml.tmp");
    let content = serde_yaml::to_string(board).map_err(|e| e.to_string())?;
    fs::write(&tmp_path, &content)
        .await
        .map_err(|e| e.to_string())?;
    atomic_write_file(&tmp_path, &board_path).await
}

pub fn active_agent_count(cards: &[BoardCard]) -> usize {
    cards
        .iter()
        .filter(|card| card.column == "doing")
        .map(|card| {
            if let Some(variants) = card.ab_variants.as_ref() {
                if variants.winner.is_none() {
                    return [variants.a.finish.as_ref(), variants.b.finish.as_ref()]
                        .into_iter()
                        .filter(|finish| finish.is_none())
                        .count();
                }
            }
            usize::from(card.agent_chat_id.is_some())
        })
        .sum()
}

pub fn infer_task_id_from_chat_id(chat_id: &str) -> Option<String> {
    if let Some(id) = chat_id.strip_prefix("orch-") {
        return Some(id.to_string());
    }
    if let Some(id) = chat_id.strip_prefix("plan-") {
        return Some(id.to_string());
    }
    if let Some(rest) = chat_id.strip_prefix("planner-") {
        if let Some((task_id, suffix)) = rest.rsplit_once('-') {
            if !task_id.is_empty()
                && !suffix.is_empty()
                && suffix.chars().all(|c| c.is_ascii_digit())
            {
                return Some(task_id.to_string());
            }
        }
        return Some(rest.to_string());
    }
    None
}
