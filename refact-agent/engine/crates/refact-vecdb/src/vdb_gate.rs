use std::path::Path;

use refact_core::memory_plane::MemoryPlaneRoots;
use refact_privacy::{Destination, PolicyLoad};

pub fn embedding_file_is_allowed(
    policy_load: &PolicyLoad,
    roots: &MemoryPlaneRoots,
    destination: &Destination,
    path: &Path,
) -> Result<(), String> {
    let compiled = match policy_load.policy.compile() {
        Ok(compiled) => compiled,
        Err(error) => {
            return Err(format!("privacy policy failed to compile: {error}"));
        }
    };
    let mut privacy_roots = roots.project_roots.clone();
    privacy_roots.extend(roots.global_knowledge_root.iter().cloned());
    privacy_roots.extend(roots.global_trajectories_root.iter().cloned());
    let zone = compiled.zone_for_path_with_roots(path, privacy_roots);
    if zone.name == "blocked" || !destination.matches_send_to(&zone.send_to) {
        return Err(format!(
            "zone '{}' forbids embedding destination '{}'",
            zone.name, destination.id.0
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_privacy::{
        DestinationId, DestinationKind, PrivacyPolicy, ShellBehavior, Zone,
    };
    use std::sync::Arc;

    #[test]
    fn embedding_gate_allows_normal_and_blocks_guarded_memory_files() {
        let root =
            std::env::temp_dir().join(format!("refact-embedding-policy-{}", uuid::Uuid::new_v4()));
        let guarded = root.join(".refact/knowledge/guarded.md");
        let normal = root.join(".refact/knowledge/normal.md");
        std::fs::create_dir_all(guarded.parent().unwrap()).unwrap();
        std::fs::write(&guarded, "guarded").unwrap();
        std::fs::write(&normal, "normal").unwrap();
        let policy_load = PolicyLoad {
            policy: Arc::new(PrivacyPolicy {
                blocked: Vec::new(),
                zones: vec![
                    Zone {
                        name: "guarded".to_string(),
                        patterns: vec![guarded.to_string_lossy().into_owned()],
                        send_to: vec!["trusted".to_string()],
                        on_shell_read: ShellBehavior::Withhold,
                    },
                    Zone {
                        name: "normal".to_string(),
                        patterns: vec!["**".to_string()],
                        send_to: vec!["*".to_string()],
                        on_shell_read: ShellBehavior::Withhold,
                    },
                ],
                ..Default::default()
            }),
            error: None,
            source_paths: Vec::new(),
        };
        let roots = MemoryPlaneRoots::new(vec![root.clone()], None, None);
        let destination = Destination {
            id: DestinationId("embedding".to_string()),
            kind: DestinationKind::Provider,
            display_name: "embedding/model".to_string(),
        };

        assert!(embedding_file_is_allowed(&policy_load, &roots, &destination, &guarded).is_err());
        assert!(embedding_file_is_allowed(&policy_load, &roots, &destination, &normal).is_ok());
        let _ = std::fs::remove_dir_all(root);
    }
}
