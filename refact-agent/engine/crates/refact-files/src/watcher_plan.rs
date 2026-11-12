use std::fs;
use std::path::{Path, PathBuf};

use crate::indexing_settings::{is_blocklisted, IndexingEverywhere};
use crate::path_classification::{
    path_has_allowed_hidden_component, path_has_hidden_component, path_is_refact_internal,
    WATCH_BLOCKLIST_PROBE_LEAF, WATCH_BLOCKLIST_PROBE_ROOT,
};
use crate::path_utils::canonicalize_normalized_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchRegistrationMode {
    Recursive,
    NonRecursive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchRegistration {
    pub path: PathBuf,
    pub mode: WatchRegistrationMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchSubtreeClass {
    Excluded,
    CleanSubtree,
    Mixed,
}

pub fn path_is_excluded_from_watch(
    scan_root: &Path,
    path: &Path,
    indexing_everywhere: &IndexingEverywhere,
) -> bool {
    if path_is_refact_internal(path) {
        return true;
    }
    let rel_path = path.strip_prefix(scan_root).unwrap_or(path);
    if path_has_allowed_hidden_component(rel_path) {
        return false;
    }
    if path_has_hidden_component(rel_path) {
        return true;
    }
    if rel_path.as_os_str().is_empty() {
        return false;
    }
    let indexing_settings = indexing_everywhere.indexing_for_path(path);
    let probe = Path::new(WATCH_BLOCKLIST_PROBE_ROOT)
        .join(rel_path)
        .join(WATCH_BLOCKLIST_PROBE_LEAF);
    is_blocklisted(&indexing_settings, &probe)
}

pub fn classify_watch_subtree(
    scan_root: &Path,
    path: &Path,
    indexing_everywhere: &IndexingEverywhere,
) -> WatchSubtreeClass {
    if path_is_excluded_from_watch(scan_root, path, indexing_everywhere) {
        return WatchSubtreeClass::Excluded;
    }

    let read_dir = match fs::read_dir(path) {
        Ok(read_dir) => read_dir,
        Err(err) => {
            tracing::debug!(
                "Skipping unreadable directory during watch planning {}: {}",
                path.display(),
                err
            );
            return WatchSubtreeClass::CleanSubtree;
        }
    };

    let mut saw_excluded_descendant = false;
    for child in read_dir {
        let child = match child {
            Ok(child) => child,
            Err(err) => {
                tracing::debug!(
                    "Skipping unreadable directory entry during watch planning {}: {}",
                    path.display(),
                    err
                );
                continue;
            }
        };
        let file_type = match child.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                tracing::debug!(
                    "Skipping directory entry with unknown type during watch planning {}: {}",
                    child.path().display(),
                    err
                );
                continue;
            }
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }

        match classify_watch_subtree(scan_root, &child.path(), indexing_everywhere) {
            WatchSubtreeClass::Excluded => saw_excluded_descendant = true,
            WatchSubtreeClass::Mixed => return WatchSubtreeClass::Mixed,
            WatchSubtreeClass::CleanSubtree => {}
        }
    }

    if saw_excluded_descendant {
        WatchSubtreeClass::Mixed
    } else {
        WatchSubtreeClass::CleanSubtree
    }
}

pub fn build_watch_plan_for_root(
    scan_root: &Path,
    indexing_everywhere: &IndexingEverywhere,
) -> Vec<WatchRegistration> {
    fn visit(
        registrations: &mut Vec<WatchRegistration>,
        scan_root: &Path,
        path: &Path,
        indexing_everywhere: &IndexingEverywhere,
    ) {
        match classify_watch_subtree(scan_root, path, indexing_everywhere) {
            WatchSubtreeClass::Excluded => {}
            WatchSubtreeClass::CleanSubtree => registrations.push(WatchRegistration {
                path: path.to_path_buf(),
                mode: WatchRegistrationMode::Recursive,
            }),
            WatchSubtreeClass::Mixed => {
                registrations.push(WatchRegistration {
                    path: path.to_path_buf(),
                    mode: WatchRegistrationMode::NonRecursive,
                });
                let read_dir = match fs::read_dir(path) {
                    Ok(read_dir) => read_dir,
                    Err(err) => {
                        tracing::debug!(
                            "Skipping unreadable directory during watch plan descent {}: {}",
                            path.display(),
                            err
                        );
                        return;
                    }
                };
                for child in read_dir {
                    let child = match child {
                        Ok(child) => child,
                        Err(err) => {
                            tracing::debug!(
                                "Skipping unreadable directory entry during watch plan descent {}: {}",
                                path.display(),
                                err
                            );
                            continue;
                        }
                    };
                    let file_type = match child.file_type() {
                        Ok(file_type) => file_type,
                        Err(err) => {
                            tracing::debug!(
                                "Skipping directory entry with unknown type during watch plan descent {}: {}",
                                child.path().display(),
                                err
                            );
                            continue;
                        }
                    };
                    if !file_type.is_dir() || file_type.is_symlink() {
                        continue;
                    }
                    visit(registrations, scan_root, &child.path(), indexing_everywhere);
                }
            }
        }
    }

    let mut registrations = Vec::new();
    visit(
        &mut registrations,
        scan_root,
        scan_root,
        indexing_everywhere,
    );
    registrations
}

pub fn build_watch_plan(
    watch_folders: &[PathBuf],
    indexing_everywhere: &IndexingEverywhere,
) -> Vec<WatchRegistration> {
    let mut plan = Vec::new();
    for root in watch_folders {
        let root = canonicalize_normalized_path(root.clone());
        plan.extend(build_watch_plan_for_root(&root, indexing_everywhere));
    }
    plan.sort_by(|a, b| {
        a.path.cmp(&b.path).then_with(|| match (a.mode, b.mode) {
            (WatchRegistrationMode::Recursive, WatchRegistrationMode::NonRecursive) => {
                std::cmp::Ordering::Less
            }
            (WatchRegistrationMode::NonRecursive, WatchRegistrationMode::Recursive) => {
                std::cmp::Ordering::Greater
            }
            _ => std::cmp::Ordering::Equal,
        })
    });
    plan.dedup_by(|a, b| a.path == b.path && a.mode == b.mode);
    plan
}
