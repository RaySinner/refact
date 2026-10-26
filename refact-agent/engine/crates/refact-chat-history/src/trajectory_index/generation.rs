use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileGeneration {
    file_len: u64,
    file_modified_at: SystemTime,
}

impl FileGeneration {
    pub(super) fn file_len(&self) -> u64 {
        self.file_len
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DirectoryContentsGeneration {
    metadata: FileGeneration,
    entry_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DirectoryGeneration {
    root: Option<DirectoryContentsGeneration>,
    index: Option<FileGeneration>,
    children: Vec<(String, DirectoryContentsGeneration)>,
    indexed_files: Vec<(String, Option<FileGeneration>)>,
}

pub(super) fn path_generation(path: &Path) -> Result<Option<FileGeneration>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Failed to read metadata {:?}: {error}", path)),
    };
    if metadata.file_type().is_symlink() {
        return Ok(None);
    }
    Ok(Some(FileGeneration {
        file_len: metadata.len(),
        file_modified_at: metadata
            .modified()
            .map_err(|error| format!("Failed to read modified time for {:?}: {error}", path))?,
    }))
}

pub(super) fn regular_file_generation(path: &Path) -> Result<Option<FileGeneration>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Failed to read metadata {:?}: {error}", path)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(None);
    }
    Ok(Some(FileGeneration {
        file_len: metadata.len(),
        file_modified_at: metadata
            .modified()
            .map_err(|error| format!("Failed to read modified time for {:?}: {error}", path))?,
    }))
}

pub(super) fn directory_contents_generation(
    path: &Path,
) -> Result<Option<DirectoryContentsGeneration>, String> {
    let Some(metadata) = path_generation(path)? else {
        return Ok(None);
    };
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Failed to read directory {:?}: {error}", path)),
    };
    let mut entry_count = 0;
    for entry in entries {
        entry.map_err(|error| format!("Failed to read directory entry in {:?}: {error}", path))?;
        entry_count += 1;
    }
    Ok(Some(DirectoryContentsGeneration {
        metadata,
        entry_count,
    }))
}

pub(super) fn index_file_names(index: &TrajectoryIndex) -> Vec<String> {
    let mut names: Vec<String> = index
        .entries
        .iter()
        .map(|entry| entry.file_name.clone())
        .chain(
            index
                .skipped_files
                .iter()
                .map(|entry| entry.file_name.clone()),
        )
        .collect();
    names.sort();
    names.dedup();
    names
}

pub(super) fn directory_generation_sync(
    dir: &Path,
    indexed_file_names: Vec<String>,
) -> Result<DirectoryGeneration, String> {
    let root = directory_contents_generation(dir)?;
    let mut child_names = Vec::new();
    if root.is_some() {
        let entries = std::fs::read_dir(dir)
            .map_err(|error| format!("Failed to read trajectory directory {:?}: {error}", dir))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "Failed to read trajectory directory entry in {:?}: {error}",
                    dir
                )
            })?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| format!("Trajectory directory has non-UTF-8 entry in {:?}", dir))?;
            let file_type = entry.file_type().map_err(|error| {
                format!(
                    "Failed to read trajectory directory entry type in {:?}: {error}",
                    dir
                )
            })?;
            if file_type.is_dir() && !file_type.is_symlink() && !name.starts_with('.') {
                child_names.push(name);
            }
        }
    }
    child_names.sort();
    child_names.dedup();

    let mut children = Vec::with_capacity(child_names.len());
    for child in child_names {
        let Some(generation) = directory_contents_generation(&dir.join(&child))? else {
            return Err(format!(
                "Trajectory child directory changed while checking freshness: {:?}",
                dir.join(&child)
            ));
        };
        children.push((child, generation));
    }

    let indexed_files = indexed_file_names
        .into_iter()
        .map(|file_name| {
            regular_file_generation(&dir.join(&file_name)).map(|generation| (file_name, generation))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(DirectoryGeneration {
        root,
        index: path_generation(&trajectory_index_path(dir))?,
        children,
        indexed_files,
    })
}

pub(super) async fn directory_generation(
    dir: &Path,
    indexed_file_names: Vec<String>,
) -> Result<DirectoryGeneration, String> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || directory_generation_sync(&dir, indexed_file_names))
        .await
        .map_err(|error| format!("Trajectory index generation task failed: {error}"))?
}

pub(super) fn cached_index_is_fresh_sync(
    dir: &Path,
    indexed_file_names: Vec<String>,
    generation: &DirectoryGeneration,
) -> Result<bool, String> {
    if &directory_generation_sync(dir, indexed_file_names)? != generation {
        return Ok(false);
    }
    Ok(generation
        .indexed_files
        .iter()
        .all(|(_, generation)| generation.is_some()))
}

pub(super) async fn cached_index_is_fresh(
    dir: &Path,
    indexed_file_names: Vec<String>,
    generation: DirectoryGeneration,
) -> Result<bool, String> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        cached_index_is_fresh_sync(&dir, indexed_file_names, &generation)
    })
    .await
    .map_err(|error| format!("Trajectory index freshness task failed: {error}"))?
}

pub(super) fn refresh_generation_for_entry(
    state: &mut TrajectoryIndexDirectoryState,
    dir: &Path,
    file_name: &str,
) {
    let Some(generation) = state.generation.as_mut() else {
        state.generation_verified_at = None;
        return;
    };
    let (Ok(file_generation), Ok(Some(root))) = (
        regular_file_generation(&dir.join(file_name)),
        directory_contents_generation(dir),
    ) else {
        state.generation = None;
        state.generation_verified_at = None;
        return;
    };
    let entry_is_new = !generation
        .indexed_files
        .iter()
        .any(|(name, existing)| name == file_name && existing.is_some());
    let root_gained_entry = match file_name.split_once('/') {
        Some((child, _)) => !generation.children.iter().any(|(name, _)| name == child),
        None => entry_is_new,
    };
    let expected_root_entries = generation
        .root
        .as_ref()
        .map(|previous| previous.entry_count + usize::from(root_gained_entry));
    if expected_root_entries.is_some_and(|expected| expected != root.entry_count) {
        state.generation = None;
        state.generation_verified_at = None;
        return;
    }
    if let Some((child, _)) = file_name.split_once('/') {
        let Ok(Some(child_generation)) = directory_contents_generation(&dir.join(child)) else {
            state.generation = None;
            state.generation_verified_at = None;
            return;
        };
        match generation
            .children
            .iter_mut()
            .find(|(name, _)| name == child)
        {
            Some(slot) => {
                let expected_entries = slot.1.entry_count + usize::from(entry_is_new);
                if child_generation.entry_count != expected_entries {
                    state.generation = None;
                    state.generation_verified_at = None;
                    return;
                }
                slot.1 = child_generation;
            }
            None => {
                generation
                    .children
                    .push((child.to_string(), child_generation));
                generation.children.sort_by(|a, b| a.0.cmp(&b.0));
            }
        }
    }
    generation.root = Some(root);
    match generation
        .indexed_files
        .iter_mut()
        .find(|(name, _)| name == file_name)
    {
        Some(slot) => slot.1 = file_generation,
        None => {
            generation
                .indexed_files
                .push((file_name.to_string(), file_generation));
            generation.indexed_files.sort_by(|a, b| a.0.cmp(&b.0));
            generation.indexed_files.dedup_by(|a, b| a.0 == b.0);
        }
    }
    state.generation_verified_at = Some(Instant::now());
}

pub(super) fn refresh_generation_after_index_write(
    state: &mut TrajectoryIndexDirectoryState,
    dir: &Path,
    index: &TrajectoryIndex,
) {
    let Some(generation) = state.generation.as_mut() else {
        state.generation_verified_at = None;
        return;
    };
    let known: Vec<String> = generation
        .indexed_files
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    if known != index_file_names(index) {
        state.generation = None;
        state.generation_verified_at = None;
        return;
    }
    let root = directory_contents_generation(dir).ok().flatten();
    let index_generation = path_generation(&trajectory_index_path(dir)).ok().flatten();
    if root.is_none() || index_generation.is_none() {
        state.generation = None;
        state.generation_verified_at = None;
        return;
    }
    generation.root = root;
    generation.index = index_generation;
    state.generation_verified_at = Some(Instant::now());
}
