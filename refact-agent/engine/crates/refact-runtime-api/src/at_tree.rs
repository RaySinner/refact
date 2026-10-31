use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "svg", "mp3", "mp4", "wav", "avi", "mov",
    "mkv", "flv", "webm", "zip", "tar", "gz", "rar", "7z", "bz2", "xz", "exe", "dll", "so",
    "dylib", "bin", "obj", "o", "a", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "woff",
    "woff2", "ttf", "otf", "eot", "pyc", "pyo", "class", "jar", "war", "db", "sqlite", "sqlite3",
    "lock", "sum",
];

const SKIP_DIRS: &[&str] = &[
    "__pycache__",
    "node_modules",
    ".git",
    ".svn",
    ".hg",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
];

pub const MAX_TREE_PATHS: usize = 20_000;

pub struct BuildBudget {
    remaining_paths: usize,
    pub truncated: bool,
    abort_flag: Option<Arc<AtomicBool>>,
}

impl BuildBudget {
    pub fn new(max_paths: usize, abort_flag: Option<Arc<AtomicBool>>) -> Self {
        BuildBudget {
            remaining_paths: max_paths,
            truncated: false,
            abort_flag,
        }
    }

    fn aborted(&self) -> bool {
        self.abort_flag
            .as_ref()
            .map(|flag| flag.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    fn take_one(&mut self) -> bool {
        if self.aborted() {
            self.truncated = true;
            return false;
        }
        if self.remaining_paths == 0 {
            self.truncated = true;
            return false;
        }
        self.remaining_paths -= 1;
        true
    }
}

pub struct TreeNode {
    pub children: HashMap<String, TreeNode>,
    pub file_size: Option<u64>,
    pub line_count: Option<usize>,
    pub source_path: Option<PathBuf>,
}

impl TreeNode {
    pub fn new() -> Self {
        TreeNode {
            children: HashMap::new(),
            file_size: None,
            line_count: None,
            source_path: None,
        }
    }

    pub fn build(paths: &[PathBuf]) -> Self {
        let mut budget = BuildBudget::new(MAX_TREE_PATHS, None);
        TreeNode::build_with_budget(paths, &mut budget)
    }

    pub fn build_with_budget(paths: &[PathBuf], budget: &mut BuildBudget) -> Self {
        let mut root = TreeNode::new();
        for path in paths {
            if should_skip_path(path) {
                continue;
            }
            if !budget.take_one() {
                break;
            }
            root.insert_path(path, path);
        }
        root
    }

    pub fn build_relative(paths: &[PathBuf], base: &Path) -> Self {
        let mut budget = BuildBudget::new(MAX_TREE_PATHS, None);
        TreeNode::build_relative_with_budget(paths, base, &mut budget)
    }

    pub fn build_relative_with_budget(
        paths: &[PathBuf],
        base: &Path,
        budget: &mut BuildBudget,
    ) -> Self {
        let mut root = TreeNode::new();
        for path in paths {
            let display_path = path
                .strip_prefix(base)
                .unwrap_or(path.as_path())
                .to_path_buf();
            if should_skip_path(&display_path) {
                continue;
            }
            if !budget.take_one() {
                break;
            }
            root.insert_path(&display_path, path);
        }
        root
    }

    fn insert_path(&mut self, display_path: &Path, source_path: &Path) {
        let components: Vec<_> = display_path.components().collect();
        let last_idx = components.len().saturating_sub(1);
        let mut node = self;

        for (i, component) in components.iter().enumerate() {
            let key = component.as_os_str().to_string_lossy().to_string();
            node = node.children.entry(key).or_insert_with(TreeNode::new);

            if i == last_idx {
                node.source_path = Some(source_path.to_path_buf());
                if let Ok(meta) = fs::metadata(source_path) {
                    node.file_size = Some(meta.len());
                    if !is_binary_file(source_path) {
                        node.line_count = count_lines(source_path);
                    }
                }
            }
        }
    }

    pub fn is_dir(&self) -> bool {
        !self.children.is_empty()
    }

    pub fn source_paths_limited(&self, limit: usize) -> Vec<PathBuf> {
        fn collect(node: &TreeNode, paths: &mut Vec<PathBuf>, limit: usize) {
            if paths.len() >= limit {
                return;
            }
            if !node.is_dir() {
                if let Some(path) = &node.source_path {
                    paths.push(path.clone());
                }
                return;
            }
            let mut children = node.children.iter().collect::<Vec<_>>();
            children.sort_by(|left, right| left.0.cmp(right.0));
            for (_, child) in children {
                collect(child, paths, limit);
                if paths.len() >= limit {
                    break;
                }
            }
        }

        let mut paths = Vec::new();
        collect(self, &mut paths, limit);
        paths
    }
}

fn should_skip_path(path: &Path) -> bool {
    for component in path.components() {
        let name = component.as_os_str().to_string_lossy();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            return true;
        }
    }
    is_binary_file(path)
}

fn is_binary_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| BINARY_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn count_lines(path: &Path) -> Option<usize> {
    fs::read_to_string(path).ok().map(|c| c.lines().count())
}
