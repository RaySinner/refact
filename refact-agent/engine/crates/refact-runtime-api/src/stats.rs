use std::path::PathBuf;
use std::sync::Arc;

use crate::files_correction::get_project_dirs;
use crate::global_context::GlobalContext;

pub async fn get_workspace_stats_dir(gcx: Arc<GlobalContext>) -> Option<PathBuf> {
    let project_dirs = get_project_dirs(gcx).await;
    project_dirs
        .first()
        .map(|first| first.join(".refact").join("stats"))
}

pub async fn get_config_stats_dir(gcx: Arc<GlobalContext>) -> PathBuf {
    gcx.config_dir.join("stats")
}

pub async fn get_stats_dir(gcx: Arc<GlobalContext>) -> PathBuf {
    if let Some(workspace_dir) = get_workspace_stats_dir(gcx.clone()).await {
        workspace_dir
    } else {
        get_config_stats_dir(gcx).await
    }
}

pub async fn get_stats_dirs_for_read(gcx: Arc<GlobalContext>) -> Vec<PathBuf> {
    let workspace_dir = get_workspace_stats_dir(gcx.clone()).await;
    let config_dir = get_config_stats_dir(gcx).await;

    let mut dirs = Vec::new();
    if let Some(workspace_dir) = workspace_dir {
        dirs.push(workspace_dir);
    }
    if !dirs.iter().any(|dir| dir == &config_dir) {
        dirs.push(config_dir);
    }
    dirs
}

pub async fn stats_writer_task(
    gcx: Arc<GlobalContext>,
    receiver: tokio::sync::mpsc::Receiver<refact_stats::event::LlmCallEvent>,
) {
    let stats_dir_fn = Arc::new(move || {
        let gcx = gcx.clone();
        Box::pin(async move { get_stats_dir(gcx).await })
            as std::pin::Pin<Box<dyn std::future::Future<Output = PathBuf> + Send>>
    });
    refact_stats::writer::stats_writer_task(stats_dir_fn, receiver).await;
}
