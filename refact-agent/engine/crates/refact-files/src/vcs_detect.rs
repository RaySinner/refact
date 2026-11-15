use std::path::{Path, PathBuf};

pub async fn detect_vcs_for_a_file_path(file_path: &Path) -> Option<(PathBuf, &'static str)> {
    let mut dir = file_path.to_path_buf();
    if dir.is_file() {
        dir.pop();
    }
    loop {
        if let Some(vcs_type) = get_vcs_type(&dir) {
            return Some((dir, vcs_type));
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

pub fn get_vcs_type(path: &Path) -> Option<&'static str> {
    if path.join(".git").exists() {
        Some("git")
    } else if path.join(".svn").is_dir() {
        Some("svn")
    } else if path.join(".hg").is_dir() {
        Some("hg")
    } else {
        None
    }
}
