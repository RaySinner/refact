use std::collections::BTreeSet;

fn production_dependency_names(manifest: &str) -> BTreeSet<String> {
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("\n[")
        .next()
        .unwrap();
    dependencies
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, _) = line
                .split_once('=')
                .expect("dependency entry must contain '='");
            let name = key.split('.').next().unwrap().trim();
            assert!(!name.is_empty(), "dependency name must not be empty");
            Some(name.to_string())
        })
        .collect()
}

fn assert_production_dependency_boundary(manifest: &str) {
    let actual = production_dependency_names(manifest);
    let expected = [
        "eventsource-stream",
        "futures",
        "hex",
        "reqwest",
        "serde",
        "serde_json",
        "sha2",
        "thiserror",
        "tokio",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "standalone protocol dependencies changed");
}

#[test]
fn production_dependencies_stay_within_standalone_protocol_boundary() {
    let manifest = include_str!("../Cargo.toml");
    assert_production_dependency_boundary(manifest);

    let source = [
        include_str!("../src/lib.rs"),
        include_str!("../src/client.rs"),
        include_str!("../src/error.rs"),
        include_str!("../src/recovery.rs"),
        include_str!("../src/session.rs"),
        include_str!("../src/streaming.rs"),
        include_str!("../src/types.rs"),
    ]
    .join("\n");
    for forbidden in [
        "AppState",
        "GlobalContext",
        "refact_room",
        "refact-room",
        "refact_tasks",
        "refact-tasks",
        "rusqlite",
        "std::fs",
        "tokio::fs",
        "claude-code",
        "opencode",
        "codex",
    ] {
        assert!(
            !source.contains(forbidden),
            "forbidden boundary marker: {forbidden}"
        );
    }
}

#[test]
fn dependency_keys_normalize_workspace_notation() {
    let manifest = r#"
[dependencies]
hex = "0.4.3"
reqwest.workspace = true
serde.workspace=true
serde_json = { workspace = true }

[dev-dependencies]
tokio.workspace = true
"#;

    let actual = production_dependency_names(manifest);
    let expected = ["hex", "reqwest", "serde", "serde_json"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);
}

#[test]
#[should_panic(expected = "standalone protocol dependencies changed")]
fn workspace_notation_does_not_hide_forbidden_dependencies() {
    let manifest = r#"
[dependencies]
hex = "0.4.3"
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
sha2 = "0.10.8"
thiserror = "2.0.12"
refact-room.workspace = true
"#;

    assert_production_dependency_boundary(manifest);
}
