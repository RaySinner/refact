use chrono::{DateTime, Utc};

use crate::types::{BuddyFact, BuddyFactKind};

pub const MAX_UNCOMMITTED_STATUS_SCAN: usize = 2000;
pub const MAX_DIFF_COMMITS: usize = 200;

fn path_hash(p: &std::path::Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    p.hash(&mut h);
    format!("{:x}", h.finish())
}

pub fn count_uncommitted(project_root: &std::path::Path) -> Option<usize> {
    let repo = git2::Repository::discover(project_root).ok()?;
    count_uncommitted_in_repo(&repo)
}

pub fn count_uncommitted_in_repo(repo: &git2::Repository) -> Option<usize> {
    use git2::{StatusOptions, StatusShow};
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(false)
        .include_ignored(false)
        .show(StatusShow::IndexAndWorkdir);
    let statuses = repo.statuses(Some(&mut opts)).ok()?;
    let count = statuses
        .iter()
        .filter(|s| !s.status().is_empty())
        .take(MAX_UNCOMMITTED_STATUS_SCAN)
        .count();
    Some(count)
}

pub fn git_diff_widening(
    project_root: &std::path::Path,
    now: DateTime<Utc>,
) -> Option<(u32, Vec<String>)> {
    let repo = git2::Repository::discover(project_root).ok()?;
    let head = repo.head().ok()?.peel_to_commit().ok()?;
    let cutoff_ts = (now - chrono::Duration::hours(4)).timestamp();

    let mut walker = repo.revwalk().ok()?;
    walker
        .set_sorting(git2::Sort::TIME | git2::Sort::TOPOLOGICAL)
        .ok()?;
    walker.push(head.id()).ok()?;

    let mut oldest_in_window = None;
    let mut first_before_cutoff = None;
    for oid in walker.take(MAX_DIFF_COMMITS) {
        let oid = oid.ok()?;
        let commit = repo.find_commit(oid).ok()?;
        if commit.time().seconds() >= cutoff_ts {
            oldest_in_window = Some(oid);
        } else {
            first_before_cutoff = Some(oid);
            break;
        }
    }

    let oldest_oid = oldest_in_window?;
    let head_tree = head.tree().ok()?;
    let base_tree = if let Some(oid) = first_before_cutoff {
        repo.find_commit(oid).ok()?.tree().ok()
    } else {
        repo.find_commit(oldest_oid)
            .ok()?
            .parent(0)
            .ok()
            .and_then(|parent| parent.tree().ok())
    };

    let diff = repo
        .diff_tree_to_tree(base_tree.as_ref(), Some(&head_tree), None)
        .ok()?;
    let stats = diff.stats().ok()?;
    let lines = (stats.insertions() + stats.deletions()) as u32;

    let mut dirs = std::collections::HashSet::new();
    let _ = diff.foreach(
        &mut |delta, _| {
            if let Some(path) = delta.new_file().path() {
                if let Some(parent) = path.parent() {
                    let s = parent.to_string_lossy().into_owned();
                    if !s.is_empty() {
                        dirs.insert(s);
                    }
                }
            }
            true
        },
        None,
        None,
        None,
    );

    if lines > 500 && dirs.len() >= 3 {
        let mut top: Vec<String> = dirs.into_iter().collect();
        top.sort();
        top.truncate(5);
        Some((lines, top))
    } else {
        None
    }
}

pub fn detect_git_pressure_facts(project_root: &std::path::Path, now: DateTime<Utc>) -> Vec<BuddyFact> {
    let mut facts = vec![];
    let hash = path_hash(project_root);

    if let Some(count) = count_uncommitted(project_root) {
        if count > 25 {
            tracing::debug!("git_pressure: uncommitted files={}", count);
            facts.push(BuddyFact {
                kind: BuddyFactKind::UncommittedPressure,
                key: format!("git:pressure:{}", hash),
                source: "git_pressure",
                payload: serde_json::json!({
                    "files": count,
                    "lines": 0,
                    "dirs": [],
                }),
                seen_at: now,
                confidence: 0.9,
            });
        }
    }

    if let Some((lines, dirs)) = git_diff_widening(project_root, now) {
        tracing::debug!("git_pressure: diff widening lines={}", lines);
        facts.push(BuddyFact {
            kind: BuddyFactKind::GitDiffWidening,
            key: format!("git:widening:{}", hash),
            source: "git_pressure",
            payload: serde_json::json!({
                "files": 0,
                "lines": lines,
                "dirs": dirs,
            }),
            seen_at: now,
            confidence: 0.8,
        });
    }

    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn init_repo() -> (tempfile::TempDir, git2::Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        (dir, repo)
    }

    fn signature() -> git2::Signature<'static> {
        git2::Signature::now("test", "test@example.com").unwrap()
    }

    fn commit_paths(repo: &git2::Repository, paths: &[&str], message: &str) -> git2::Oid {
        let mut index = repo.index().unwrap();
        for path in paths {
            index.add_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = signature();
        let parents = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .and_then(|oid| repo.find_commit(oid).ok())
            .into_iter()
            .collect::<Vec<_>>();
        let parent_refs = parents.iter().collect::<Vec<_>>();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap()
    }

    #[test]
    fn uncommitted_status_scan_does_not_recurse_into_untracked_dirs() {
        let (dir, repo) = init_repo();
        commit_paths(&repo, &[], "init");
        drop(repo);
        let nested = dir.path().join("untracked").join("deep");
        fs::create_dir_all(&nested).unwrap();
        for idx in 0..30 {
            fs::write(nested.join(format!("file_{idx}.rs")), "fn main() {}\n").unwrap();
        }

        let count = count_uncommitted(dir.path()).unwrap();

        assert_eq!(count, 1);
    }
}
