use std::path::{Path, PathBuf};

use tokio::fs;

use crate::types::{BoardCard, TaskBoard, TaskMeta};

pub trait TaskPaths {
    fn task_project_dirs(&self) -> Vec<PathBuf>;
    fn task_config_dir(&self) -> PathBuf;
}

const TASKS_DIR: &str = "tasks";

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
