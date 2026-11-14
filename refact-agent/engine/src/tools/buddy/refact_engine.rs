pub use refact_tools::tools::buddy::refact_engine::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::fs;
#[cfg(test)]
#[allow(unused_imports)]
use std::io::Read;
#[cfg(test)]
#[allow(unused_imports)]
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use std::time::{Duration, SystemTime};
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use regex::Regex;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::{json, Value};
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use uuid::Uuid;
#[cfg(test)]
#[allow(unused_imports)]
use walkdir::WalkDir;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{IndexAddOption, Repository, RepositoryInitOptions, Signature};

    fn create_fake_repo(path: &Path) -> Repository {
        let mut opts = RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = Repository::init_opts(path, &opts).unwrap();
        fs::write(path.join("README.md"), "hello refact\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        repo
    }

    #[test]
    fn refact_engine_clone_caches_for_24h() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let _repo = create_fake_repo(&source);
        let cache = dir.path().join("cache");
        let first =
            clone_refact_engine_from_url(&cache, "main", false, source.to_str().unwrap(), false)
                .unwrap();
        let second =
            clone_refact_engine_from_url(&cache, "main", false, "invalid://unused", false).unwrap();
        assert!(first.contains("Cloned to"));
        assert!(second.contains("Mirror up-to-date"));
        assert!(cache.join("main").join("README.md").is_file());
    }

    #[cfg_attr(windows, ignore = "needs symlink privileges Windows does not grant by default")]
    #[test]
    fn refact_engine_cat_rejects_path_outside_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mirror");
        fs::create_dir_all(&root).unwrap();
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, "secret").unwrap();
        let link = root.join("link.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, &link).unwrap();
        let err = cat_refact_engine_file_at(&root, "link.txt", None, None).unwrap_err();
        assert!(err.contains("outside"));
    }

    #[test]
    fn refact_engine_cat_rejects_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mirror");
        fs::create_dir_all(&root).unwrap();
        let err = cat_refact_engine_file_at(&root, "../outside.txt", None, None).unwrap_err();
        assert!(err.contains(".."));
    }

    #[test]
    fn refact_engine_search_falls_back_for_glob_scope() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mirror");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src").join("lib.rs"),
            "fn refact_engine_marker() {}\n",
        )
        .unwrap();

        let output = search_refact_engine_at(&root, "refact_engine_marker", Some("*.rs")).unwrap();
        assert!(output.contains("src"));
        assert!(!output.contains("canonicalize"));
    }

    #[test]
    fn refact_engine_search_falls_back_for_missing_bare_scope() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mirror");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("README.md"), "chat marker\n").unwrap();

        let output = search_refact_engine_at(&root, "chat marker", Some("chat")).unwrap();
        assert!(output.contains("README.md"));
        assert!(!output.contains("canonicalize"));
    }

    #[test]
    fn refact_engine_search_rejects_dotdot_scope() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mirror");
        fs::create_dir_all(&root).unwrap();
        let err = search_refact_engine_at(&root, "secret", Some("../outside")).unwrap_err();
        assert!(err.contains(".."));
    }

    #[test]
    fn refact_engine_requires_fresh_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let err = require_fresh_mirror(&missing).unwrap_err();
        assert_eq!(
            err,
            "engine mirror missing or stale; run refact_engine_clone"
        );
    }
}
