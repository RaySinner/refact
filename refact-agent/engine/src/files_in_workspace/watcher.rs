use super::*;

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::info;

use crate::files_blocklist::reload_indexing_everywhere_if_needed;

pub(crate) async fn register_watch_path(
    watcher_lock: &Arc<ARwLock<RecommendedWatcher>>,
    registration: &WatchRegistration,
) {
    let mut watcher = watcher_lock.write().await;
    let recursive_mode = match registration.mode {
        WatchRegistrationMode::Recursive => RecursiveMode::Recursive,
        WatchRegistrationMode::NonRecursive => RecursiveMode::NonRecursive,
    };
    if let Err(err) = watcher.watch(&registration.path, recursive_mode) {
        tracing::warn!(
            "Failed to add watcher for {} ({:?}): {}",
            registration.path.display(),
            registration.mode,
            err
        );
    }
}

pub(crate) async fn unregister_watch_path(
    watcher_lock: &Arc<ARwLock<RecommendedWatcher>>,
    path: &Path,
) {
    let mut watcher = watcher_lock.write().await;
    if let Err(err) = watcher.unwatch(path) {
        tracing::debug!("Failed to remove watcher for {}: {}", path.display(), err);
    }
}

pub async fn watcher_init(gcx: Arc<GlobalContext>) {
    let gcx_weak = Arc::downgrade(&gcx);
    let rt = tokio::runtime::Handle::current();
    let event_callback = move |res| {
        rt.block_on(async {
            if let Ok(event) = res {
                file_watcher_event(event, gcx_weak.clone()).await;
            }
        });
    };
    let watcher = match RecommendedWatcher::new(event_callback, Config::default()) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("Failed to create file watcher (file watching disabled): {e}");
            return;
        }
    };

    let mut watch_folders = crate::files_correction::get_raw_project_dirs(gcx.clone()).await;
    watch_folders.extend(crate::files_correction::get_unscoped_project_dirs(gcx.clone()).await);
    watch_folders.sort();
    watch_folders.dedup();
    let watch_folders = watch_folders
        .into_iter()
        .map(crate::files_correction::canonicalize_normalized_path)
        .collect::<Vec<_>>();
    let indexing_everywhere = reload_indexing_everywhere_if_needed(gcx.clone()).await;
    let watch_plan = build_watch_plan(&watch_folders, indexing_everywhere.as_ref());

    let watcher_lock = Arc::new(ARwLock::new(watcher));
    for registration in &watch_plan {
        info!(
            "ADD WATCHER ({:?}): {}",
            registration.mode,
            registration.path.display()
        );
        register_watch_path(&watcher_lock, registration).await;
    }

    let new_watcher = Some(watcher_lock);
    let old_watcher = {
        std::mem::replace(
            &mut *gcx.documents_state.fs_watcher.lock().unwrap(),
            new_watcher,
        )
    };
    drop(old_watcher);
}
