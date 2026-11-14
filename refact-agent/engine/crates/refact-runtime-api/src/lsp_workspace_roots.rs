use std::path::PathBuf;

use refact_files::path_utils::canonical_path;
use url::Url;

pub fn canonical_workspace_roots(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut canonical = folders.to_vec();
    canonical.sort();
    canonical.dedup();
    canonical
}

pub fn workspace_roots_changed(current: &[PathBuf], next: &[PathBuf]) -> bool {
    canonical_workspace_roots(current) != canonical_workspace_roots(next)
}

pub fn canonical_path_from_file_uri(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path()
        .ok()
        .map(|path| canonical_path(path.to_string_lossy().into_owned()))
}

pub fn add_workspace_root_to_set(folders: &mut Vec<PathBuf>, path: PathBuf) -> bool {
    let before = canonical_workspace_roots(folders);
    let canonical_path = canonical_path(path.to_string_lossy().into_owned());
    let mut next = before.clone();
    next.push(canonical_path);
    let next = canonical_workspace_roots(&next);
    *folders = next.clone();
    before != next
}

pub fn remove_workspace_root_from_set(folders: &mut Vec<PathBuf>, path: &PathBuf) -> bool {
    let before = canonical_workspace_roots(folders);
    let canonical_path = canonical_path(path.to_string_lossy().into_owned());
    let next = before
        .iter()
        .filter(|folder| *folder != &canonical_path)
        .cloned()
        .collect::<Vec<_>>();
    *folders = next.clone();
    before != next
}

pub fn apply_workspace_root_changes(
    folders: &mut Vec<PathBuf>,
    added: &[PathBuf],
    removed: &[PathBuf],
) -> bool {
    let before = canonical_workspace_roots(folders);
    let mut next = before.clone();
    next.extend(
        added
            .iter()
            .map(|path| canonical_path(path.to_string_lossy().into_owned())),
    );
    next = canonical_workspace_roots(&next);
    for path in removed {
        let canonical_path = canonical_path(path.to_string_lossy().into_owned());
        next.retain(|folder| folder != &canonical_path);
    }
    next = canonical_workspace_roots(&next);
    *folders = next.clone();
    before != next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_workspace_roots_changed_ignores_order_and_duplicates() {
        let first = PathBuf::from("/workspace/first");
        let second = PathBuf::from("/workspace/second");

        assert!(!workspace_roots_changed(
            &[first.clone(), second.clone()],
            &[second.clone(), first.clone()]
        ));
        assert!(!workspace_roots_changed(
            &[first.clone(), first.clone()],
            std::slice::from_ref(&first)
        ));
        assert!(workspace_roots_changed(
            std::slice::from_ref(&first),
            &[first.clone(), second]
        ));
    }

    #[test]
    fn sidebar_canonical_workspace_roots_are_sorted_and_deduplicated() {
        let first = PathBuf::from("/workspace/first");
        let second = PathBuf::from("/workspace/second");

        assert_eq!(
            canonical_workspace_roots(&[second.clone(), first.clone(), second]),
            vec![first, PathBuf::from("/workspace/second")]
        );
    }

    #[test]
    fn sidebar_canonical_path_from_file_uri_ignores_non_file_uris() {
        let workspace_root = std::env::temp_dir().join("refact-sidebar-uri-root");
        let file_uri = Url::from_directory_path(&workspace_root).unwrap();

        assert_eq!(
            canonical_path_from_file_uri(&file_uri),
            Some(canonical_path(workspace_root.to_string_lossy().into_owned()))
        );
        assert!(canonical_path_from_file_uri(&Url::parse("untitled:Untitled-1").unwrap()).is_none());
        assert!(canonical_path_from_file_uri(
            &Url::parse("vscode-remote://ssh-remote%2Bhost/home/user/project").unwrap()
        )
        .is_none());
    }

    #[test]
    fn sidebar_workspace_root_set_helpers_compare_canonical_sets() {
        let first_raw = std::env::temp_dir().join("refact-sidebar-first");
        let first = canonical_path(first_raw.to_string_lossy().into_owned());
        let first_duplicate = first_raw.join(".");
        let second_raw = std::env::temp_dir().join("refact-sidebar-second");
        let second = canonical_path(second_raw.to_string_lossy().into_owned());
        let missing = std::env::temp_dir().join("refact-sidebar-missing");
        let mut folders = vec![first.clone()];

        assert!(!add_workspace_root_to_set(
            &mut folders,
            first_duplicate.clone()
        ));
        assert_eq!(folders, vec![first.clone()]);

        assert!(add_workspace_root_to_set(&mut folders, second_raw));
        assert_eq!(
            folders,
            canonical_workspace_roots(&[first.clone(), second.clone()])
        );

        assert!(!remove_workspace_root_from_set(&mut folders, &missing));
        assert_eq!(
            folders,
            canonical_workspace_roots(&[first.clone(), second.clone()])
        );

        assert!(remove_workspace_root_from_set(
            &mut folders,
            &first_duplicate
        ));
        assert_eq!(folders, vec![second]);

        assert!(!remove_workspace_root_from_set(&mut folders, &first_raw));
    }

    #[test]
    fn sidebar_workspace_root_changes_are_compared_after_full_mutation() {
        let first_raw = std::env::temp_dir().join("refact-sidebar-batch-first");
        let first = canonical_path(first_raw.to_string_lossy().into_owned());
        let second_raw = std::env::temp_dir().join("refact-sidebar-batch-second");
        let second = canonical_path(second_raw.to_string_lossy().into_owned());
        let missing = std::env::temp_dir().join("refact-sidebar-batch-missing");
        let mut folders = vec![first.clone()];

        assert!(!apply_workspace_root_changes(
            &mut folders,
            std::slice::from_ref(&first_raw),
            &[]
        ));
        assert_eq!(folders, vec![first.clone()]);

        assert!(!apply_workspace_root_changes(
            &mut folders,
            &[],
            std::slice::from_ref(&missing)
        ));
        assert_eq!(folders, vec![first.clone()]);

        assert!(!apply_workspace_root_changes(
            &mut folders,
            std::slice::from_ref(&second_raw),
            std::slice::from_ref(&second_raw)
        ));
        assert_eq!(folders, vec![first.clone()]);

        assert!(apply_workspace_root_changes(
            &mut folders,
            std::slice::from_ref(&second_raw),
            std::slice::from_ref(&first_raw)
        ));
        assert_eq!(folders, vec![second]);
    }
}
