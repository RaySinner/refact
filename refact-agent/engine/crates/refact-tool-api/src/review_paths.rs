pub fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string()
}

pub fn paths_match(left: &str, right: &str) -> bool {
    let left = normalize_path(left);
    let right = normalize_path(right);
    left == right || left.ends_with(&format!("/{right}")) || right.ends_with(&format!("/{left}"))
}
