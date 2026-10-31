use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::info;

use crate::indexing_settings::IndexingEverywhere;
use crate::path_utils::any_glob_matches_path;

pub fn _ls_files(
    indexing_everywhere: &IndexingEverywhere,
    scan_root: &Path,
    path: &PathBuf,
    recursive: bool,
    blocklist_check: bool,
) -> Result<Vec<PathBuf>, String> {
    let mut paths = vec![];
    let mut dirs_to_visit = vec![path.clone()];

    while let Some(dir) = dirs_to_visit.pop() {
        let ls_maybe = fs::read_dir(&dir);
        if ls_maybe.is_err() {
            info!(
                "failed to read directory {}: {}",
                dir.display(),
                ls_maybe.unwrap_err()
            );
            continue;
        }
        let ls: fs::ReadDir = ls_maybe.unwrap();
        let entries_maybe = ls.collect::<Result<Vec<_>, _>>();
        if entries_maybe.is_err() {
            info!(
                "failed to read directory {}: {}",
                dir.display(),
                entries_maybe.unwrap_err()
            );
            continue;
        }
        let mut entries = entries_maybe.unwrap();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let abs_path = entry.path();
            let indexing_settings = indexing_everywhere.indexing_for_path(&abs_path);
            let rel_path = abs_path.strip_prefix(scan_root).unwrap_or(&abs_path);
            if recursive && abs_path.is_dir() {
                if !blocklist_check
                    || !any_glob_matches_path(&indexing_settings.blocklist, rel_path)
                {
                    dirs_to_visit.push(abs_path);
                }
            } else if abs_path.is_file() {
                paths.push(abs_path);
            }
        }
    }
    Ok(paths)
}

pub fn ls_files(
    indexing_everywhere: &IndexingEverywhere,
    path: &PathBuf,
    recursive: bool,
) -> Result<Vec<PathBuf>, String> {
    if !path.is_dir() {
        return Err(format!("path '{}' is not a directory", path.display()));
    }

    let indexing_settings = indexing_everywhere.indexing_for_path(path);
    let mut paths = _ls_files(indexing_everywhere, path.as_path(), path, recursive, true).unwrap();
    if recursive {
        for additional_indexing_dir in indexing_settings.additional_indexing_dirs.iter() {
            let additional_path = PathBuf::from(additional_indexing_dir);
            paths.extend(
                _ls_files(
                    indexing_everywhere,
                    additional_path.as_path(),
                    &additional_path,
                    recursive,
                    false,
                )
                .unwrap(),
            );
        }
    }

    Ok(paths)
}

#[derive(Debug, Clone, Default)]
pub struct LsFilesLimited {
    pub files: Vec<PathBuf>,
    pub truncated: bool,
    pub aborted: bool,
}

impl LsFilesLimited {
    fn stopped(&self) -> bool {
        self.truncated || self.aborted
    }
}

fn _ls_files_limited(
    indexing_everywhere: &IndexingEverywhere,
    scan_root: &Path,
    path: &PathBuf,
    recursive: bool,
    blocklist_check: bool,
    budget: &mut usize,
    abort: Option<&AtomicBool>,
    out: &mut LsFilesLimited,
) {
    let mut dirs_to_visit = vec![path.clone()];

    while let Some(dir) = dirs_to_visit.pop() {
        if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            out.aborted = true;
            return;
        }
        let ls_maybe = fs::read_dir(&dir);
        if ls_maybe.is_err() {
            info!(
                "failed to read directory {}: {}",
                dir.display(),
                ls_maybe.unwrap_err()
            );
            continue;
        }
        let ls: fs::ReadDir = ls_maybe.unwrap();
        let entries_maybe = ls.collect::<Result<Vec<_>, _>>();
        if entries_maybe.is_err() {
            info!(
                "failed to read directory {}: {}",
                dir.display(),
                entries_maybe.unwrap_err()
            );
            continue;
        }
        let mut entries = entries_maybe.unwrap();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                out.aborted = true;
                return;
            }
            if *budget == 0 {
                out.truncated = true;
                return;
            }
            *budget -= 1;

            let abs_path = entry.path();
            let indexing_settings = indexing_everywhere.indexing_for_path(&abs_path);
            let rel_path = abs_path.strip_prefix(scan_root).unwrap_or(&abs_path);
            if recursive && abs_path.is_dir() {
                if !blocklist_check
                    || !any_glob_matches_path(&indexing_settings.blocklist, rel_path)
                {
                    dirs_to_visit.push(abs_path);
                }
            } else if abs_path.is_file() {
                out.files.push(abs_path);
            }
        }
    }
}

pub fn ls_files_limited(
    indexing_everywhere: &IndexingEverywhere,
    path: &PathBuf,
    recursive: bool,
    max_entries: usize,
    abort: Option<&AtomicBool>,
) -> Result<LsFilesLimited, String> {
    if !path.is_dir() {
        return Err(format!("path '{}' is not a directory", path.display()));
    }

    let indexing_settings = indexing_everywhere.indexing_for_path(path);
    let mut out = LsFilesLimited::default();
    let mut budget = max_entries;

    _ls_files_limited(
        indexing_everywhere,
        path.as_path(),
        path,
        recursive,
        true,
        &mut budget,
        abort,
        &mut out,
    );

    if recursive && !out.stopped() {
        for additional_indexing_dir in indexing_settings.additional_indexing_dirs.iter() {
            if out.stopped() {
                break;
            }
            let additional_path = PathBuf::from(additional_indexing_dir);
            _ls_files_limited(
                indexing_everywhere,
                additional_path.as_path(),
                &additional_path,
                recursive,
                false,
                &mut budget,
                abort,
                &mut out,
            );
        }
    }

    Ok(out)
}
