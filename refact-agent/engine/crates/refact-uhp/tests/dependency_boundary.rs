use std::collections::BTreeSet;

#[test]
fn production_dependencies_stay_within_standalone_protocol_boundary() {
    let manifest = include_str!("../Cargo.toml");
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("\n[")
        .next()
        .unwrap();
    let actual = dependencies
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            (!line.is_empty()).then(|| line.split('=').next().unwrap().trim().to_string())
        })
        .collect::<BTreeSet<_>>();
    let expected = ["hex", "reqwest", "serde", "serde_json", "sha2", "thiserror"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);

    let source = [
        include_str!("../src/lib.rs"),
        include_str!("../src/client.rs"),
        include_str!("../src/error.rs"),
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
