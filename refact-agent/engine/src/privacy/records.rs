pub use refact_privacy::model_context::{
    apply_shell_observation_with_scope, attach_declared_output_files, declared_file_record,
    declared_file_records_with_scope, derived_zone_for_path, file_record,
    filter_path_enrichment_for_model_context_with_scope, observed_file_records, resolve_shell_ask,
    shell_ask_pending, PrivacyRecordScope, ShellReadDecision, SHELL_APPROVAL_MESSAGE,
    SHELL_WITHHELD_MESSAGE,
};

pub use refact_privacy::observation_reason::{
    provider_destination, shell_observation_needed, shell_observation_needed_for_session,
    shell_observation_reason_for_policy, shell_observation_reason_for_session_with_policy,
    ObservationReason,
};

pub use refact_privacy::{new_derived_privacy_zones, DerivedPrivacyZones, DerivedZone};

pub use refact_privacy::message_records::{
    attach_record, carry_records_into, merge_message_records, merge_records, records_to_carry,
};

pub use super::gcx_wrappers::{
    apply_shell_observation, declared_file_records, privacy_record_scope,
};
pub(crate) use super::gcx_wrappers::{
    filter_path_enrichment_for_model_context, tool_enrichment_from_path_references,
};

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use refact_chat_api::{ToolEnrichmentKind, ToolEnrichmentProvenance};
    use refact_exec::{ObservationStatus, ObservedAccess};
    use refact_privacy::{
        Attribution, FileRecord, PrivacyPolicy, PrivacyRecord, ShellBehavior, SubagentPolicy, Zone,
    };

    use super::*;
    use crate::call_validation::ChatMessage;
    use crate::global_context::GlobalContext;

    async fn gcx_with_policy(
        workspace_file: &Path,
        send_to: &[&str],
        behavior: ShellBehavior,
    ) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_files.lock().unwrap() = vec![workspace_file.to_path_buf()];
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace_file
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()];
        gcx.privacy_policy_load.write().unwrap().policy = Arc::new(PrivacyPolicy {
            blocked: Vec::new(),
            zones: vec![
                Zone {
                    name: "secrets".to_string(),
                    patterns: vec![workspace_file
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()],
                    send_to: send_to.iter().map(|value| (*value).to_string()).collect(),
                    on_shell_read: behavior,
                },
                Zone {
                    name: "normal".to_string(),
                    patterns: vec!["*".to_string()],
                    send_to: vec!["*".to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                },
            ],
            subagents: SubagentPolicy::default(),
            ..Default::default()
        });
        gcx
    }

    fn tool_message(content: &str) -> ChatMessage {
        ChatMessage::new("tool".to_string(), content.to_string())
    }

    async fn gcx_with_zones(workspace: &Path, zones: Vec<Zone>) -> Arc<GlobalContext> {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        *gcx.documents_state.workspace_folders.lock().unwrap() = vec![workspace.to_path_buf()];
        gcx.privacy_policy_load.write().unwrap().policy = Arc::new(PrivacyPolicy {
            blocked: Vec::new(),
            zones,
            subagents: SubagentPolicy::default(),
            ..Default::default()
        });
        gcx
    }

    #[tokio::test]
    async fn path_enrichment_omits_destination_guarded_candidates() {
        let temp = tempfile::tempdir().unwrap();
        let public = temp.path().join("public.rs");
        let secret = temp.path().join("secret.rs");
        std::fs::write(&public, "pub fn visible() {}\n").unwrap();
        std::fs::write(&secret, "pub fn guarded() {}\n").unwrap();
        let gcx = gcx_with_zones(
            temp.path(),
            vec![
                Zone {
                    name: "secrets".to_string(),
                    patterns: vec!["secret.rs".to_string()],
                    send_to: vec![],
                    on_shell_read: ShellBehavior::Withhold,
                },
                Zone {
                    name: "normal".to_string(),
                    patterns: vec!["*".to_string()],
                    send_to: vec!["*".to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                },
            ],
        )
        .await;
        let collected = crate::exec::path_enrichment::collect(
            "cat public.rs secret.rs",
            temp.path(),
            temp.path(),
            "",
        );

        let enrichment = filter_path_enrichment_for_model_context(
            gcx,
            &provider_destination("untrusted/model"),
            &new_derived_privacy_zones(),
            collected,
        )
        .await;

        assert_eq!(enrichment.references.len(), 1);
        assert_eq!(enrichment.references[0].path, "public.rs");
        assert_eq!(enrichment.withheld_count, 1);
        let envelope = tool_enrichment_from_path_references(enrichment);
        assert_eq!(envelope.references.len(), 1);
        assert_eq!(envelope.references[0].kind, ToolEnrichmentKind::Path);
        assert_eq!(envelope.references[0].target, "public.rs");
        assert_eq!(
            envelope.references[0].provenance,
            ToolEnrichmentProvenance::Heuristic
        );
        let expected_confidence = if cfg!(windows) { Some(0.3) } else { Some(0.9) };
        assert_eq!(envelope.references[0].confidence, expected_confidence);
        assert!(envelope.truncated);
    }

    #[tokio::test]
    async fn derived_static_tie_uses_zone_order() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file.txt");
        std::fs::write(&file, "value").unwrap();
        let gcx = gcx_with_zones(
            temp.path(),
            vec![
                Zone {
                    name: "derived-earlier".to_string(),
                    patterns: vec!["derived-only.txt".to_string()],
                    send_to: vec!["provider-a".to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                },
                Zone {
                    name: "static-later".to_string(),
                    patterns: vec!["file.txt".to_string()],
                    send_to: vec!["provider-a".to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                },
                Zone {
                    name: "normal".to_string(),
                    patterns: vec!["*".to_string()],
                    send_to: vec!["*".to_string()],
                    on_shell_read: ShellBehavior::Withhold,
                },
            ],
        )
        .await;
        let derived_zones = new_derived_privacy_zones();
        derived_zones.write().unwrap().insert(
            file.clone(),
            DerivedZone {
                zone: "derived-earlier".to_string(),
                origin: None,
            },
        );

        let record = file_record(
            &privacy_record_scope(&gcx),
            &file,
            Attribution::Observed,
            &derived_zones,
        )
        .unwrap();

        assert_eq!(record.zone, "derived-earlier");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shared_sink_writes_do_not_inherit_guarded_zone() {
        let temp = tempfile::tempdir().unwrap();
        let secret = temp.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let gcx = gcx_with_policy(&secret, &[], ShellBehavior::Withhold).await;
        let derived_zones = new_derived_privacy_zones();
        let mut message = tool_message("secret output");

        apply_shell_observation(
            &gcx,
            "cat secret.txt 2>/dev/null",
            temp.path(),
            &provider_destination("untrusted/model"),
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![secret.clone()],
                writes: vec![PathBuf::from("/dev/null")],
            }),
            &derived_zones,
            &mut message,
        )
        .await
        .unwrap();

        let tainted = derived_zones.read().unwrap();
        assert!(
            !tainted.keys().any(|path| path == Path::new("/dev/null")),
            "shared character devices must never inherit a guarded zone"
        );
    }

    #[tokio::test]
    async fn vcs_metadata_writes_do_not_inherit_guarded_zone() {
        let temp = tempfile::tempdir().unwrap();
        let secret = temp.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let commit_editmsg = temp.path().join(".git").join("COMMIT_EDITMSG");
        let object = temp
            .path()
            .join(".git")
            .join("objects")
            .join("ab")
            .join("cd");
        std::fs::create_dir_all(commit_editmsg.parent().unwrap()).unwrap();
        std::fs::create_dir_all(object.parent().unwrap()).unwrap();
        std::fs::write(&commit_editmsg, "message").unwrap();
        std::fs::write(&object, "object").unwrap();
        let gcx = gcx_with_policy(&secret, &[], ShellBehavior::Withhold).await;
        let derived_zones = new_derived_privacy_zones();
        let destination = provider_destination("untrusted/model");
        let mut commit_message = tool_message("committed");

        apply_shell_observation(
            &gcx,
            "git commit",
            temp.path(),
            &destination,
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![secret.clone()],
                writes: vec![commit_editmsg.clone(), object.clone()],
            }),
            &derived_zones,
            &mut commit_message,
        )
        .await
        .unwrap();

        {
            let tainted = derived_zones.read().unwrap();
            assert!(
                !tainted.keys().any(|path| path == commit_editmsg.as_path()),
                "version-control churn must never inherit a guarded zone"
            );
            assert!(
                tainted.keys().any(|path| path == object.as_path()),
                "git object files can hold real content and stay taintable"
            );
        }

        let mut next_commit = tool_message("second commit");
        apply_shell_observation(
            &gcx,
            "git commit",
            temp.path(),
            &destination,
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![commit_editmsg],
                writes: Vec::new(),
            }),
            &derived_zones,
            &mut next_commit,
        )
        .await
        .unwrap();

        assert_eq!(next_commit.content.content_text_only(), "second commit");
        assert!(next_commit.extra.get("privacy").is_none());
    }

    #[tokio::test]
    async fn inherited_zone_is_reported_with_its_origin() {
        let temp = tempfile::tempdir().unwrap();
        let secret = temp.path().join("secret.txt");
        let derived = temp.path().join("derived.txt");
        std::fs::write(&secret, "secret").unwrap();
        std::fs::write(&derived, "copy").unwrap();
        let gcx = gcx_with_policy(&secret, &[], ShellBehavior::Withhold).await;
        let derived_zones = new_derived_privacy_zones();
        let destination = provider_destination("untrusted/model");
        let mut copy_message = tool_message("copied");

        apply_shell_observation(
            &gcx,
            "cat secret.txt > derived.txt",
            temp.path(),
            &destination,
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![secret.clone()],
                writes: vec![derived.clone()],
            }),
            &derived_zones,
            &mut copy_message,
        )
        .await
        .unwrap();

        assert_eq!(
            derived_zone_for_path(
                &privacy_record_scope(&gcx),
                &derived,
                &derived_zones,
            )
            .and_then(|zone| zone.origin)
            .unwrap(),
            refact_core::chat_types::normalize_file_name(secret.to_string_lossy().into_owned())
        );

        let mut read_message = tool_message("derived secret");
        apply_shell_observation(
            &gcx,
            "cat derived.txt",
            temp.path(),
            &destination,
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![derived.clone()],
                writes: Vec::new(),
            }),
            &derived_zones,
            &mut read_message,
        )
        .await
        .unwrap();

        let withheld = read_message.content.content_text_only();
        assert!(withheld.starts_with("Output withheld by user privacy policy"));
        assert!(withheld.contains(&format!(
            "(zone \"secrets\", inherited from \"{}\")",
            refact_core::chat_types::normalize_file_name(secret.to_string_lossy().into_owned())
        )));
        assert!(withheld.contains("session-derived label"));

        let mut plain_message = tool_message("secret output");
        apply_shell_observation(
            &gcx,
            "cat secret.txt",
            temp.path(),
            &destination,
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![secret.clone()],
                writes: Vec::new(),
            }),
            &derived_zones,
            &mut plain_message,
        )
        .await
        .unwrap();

        let plain = plain_message.content.content_text_only();
        assert!(plain.contains(&format!(
            "  - {} (zone \"secrets\")",
            refact_core::chat_types::normalize_file_name(secret.to_string_lossy().into_owned())
        )));
        assert!(!plain.contains("inherited from"));
        assert!(!plain.contains("session-derived label"));
    }

    #[tokio::test]
    async fn observed_guarded_read_withholds_output_and_keeps_local_copy() {
        let temp = tempfile::tempdir().unwrap();
        let secret = temp.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let gcx = gcx_with_policy(&secret, &[], ShellBehavior::Withhold).await;
        let derived_zones = new_derived_privacy_zones();
        let mut message = tool_message("secret output");

        let decision = apply_shell_observation(
            &gcx,
            "cat secret.txt",
            temp.path(),
            &provider_destination("untrusted/model"),
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![secret.clone()],
                writes: Vec::new(),
            }),
            &derived_zones,
            &mut message,
        )
        .await
        .unwrap();

        assert_eq!(decision, ShellReadDecision::Pass);
        let withheld = message.content.content_text_only();
        assert!(withheld.starts_with("Output withheld by user privacy policy"));
        assert!(withheld.contains("secret.txt"));
        assert!(withheld.contains("zone \"secrets\""));
        assert_eq!(
            message.extra["privacy_shell"]["local_only_output"],
            "secret output"
        );
        assert_eq!(message.extra["privacy"]["files"][0]["zone"], "secrets");
        assert_eq!(
            message.extra["privacy"]["files"][0]["attribution"],
            "observed"
        );
        assert!(refact_privacy::records_from_messages(&[message])
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn observed_read_allowed_for_destination_passes_through() {
        let temp = tempfile::tempdir().unwrap();
        let secret = temp.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let gcx = gcx_with_policy(&secret, &["trusted"], ShellBehavior::Withhold).await;
        let destination = provider_destination("trusted/model");
        let derived_zones = new_derived_privacy_zones();
        let mut message = tool_message("allowed output");

        assert!(!shell_observation_needed(
            &gcx.privacy_policy_load,
            &destination
        ));
        let decision = apply_shell_observation(
            &gcx,
            "cat secret.txt",
            temp.path(),
            &destination,
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![secret],
                writes: Vec::new(),
            }),
            &derived_zones,
            &mut message,
        )
        .await
        .unwrap();

        assert_eq!(decision, ShellReadDecision::Pass);
        assert_eq!(message.content.content_text_only(), "allowed output");
        assert!(!message.extra.contains_key("privacy_shell"));
    }

    #[tokio::test]
    async fn tool_shell_derived_write_inherits_secret_zone_for_session() {
        let temp = tempfile::tempdir().unwrap();
        let secret = temp.path().join("secret.txt");
        let derived = temp.path().join("derived.txt");
        std::fs::write(&secret, "secret").unwrap();
        std::fs::write(&derived, "copy").unwrap();
        let gcx = gcx_with_policy(&secret, &[], ShellBehavior::Withhold).await;
        let derived_zones = new_derived_privacy_zones();
        let destination = provider_destination("untrusted/model");
        let mut copy_message = tool_message("copied");

        apply_shell_observation(
            &gcx,
            "cat secret.txt > derived.txt",
            temp.path(),
            &destination,
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![secret],
                writes: vec![derived.clone()],
            }),
            &derived_zones,
            &mut copy_message,
        )
        .await
        .unwrap();

        gcx.documents_state.workspace_files.lock().unwrap().clear();
        assert!(shell_observation_needed_for_session(
            &gcx.privacy_policy_load,
            &destination,
            &derived_zones
        ));

        let mut read_message = tool_message("derived secret");
        apply_shell_observation(
            &gcx,
            "cat derived.txt",
            temp.path(),
            &destination,
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![derived],
                writes: Vec::new(),
            }),
            &derived_zones,
            &mut read_message,
        )
        .await
        .unwrap();

        assert_eq!(read_message.extra["privacy"]["files"][0]["zone"], "secrets");
        let withheld = read_message.content.content_text_only();
        assert!(withheld.starts_with("Output withheld by user privacy policy"));
        assert!(withheld.contains("zone \"secrets\""));

        let mut other_session_message = tool_message("ordinary output");
        apply_shell_observation(
            &gcx,
            "cat derived.txt",
            temp.path(),
            &destination,
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![temp.path().join("derived.txt")],
                writes: Vec::new(),
            }),
            &new_derived_privacy_zones(),
            &mut other_session_message,
        )
        .await
        .unwrap();

        assert!(
            other_session_message.extra.get("privacy").is_none(),
            "a fresh session must not inherit the derived secret zone, so the read stays \
             unguarded and its inert normal record is not persisted"
        );
        assert_eq!(
            other_session_message.content.content_text_only(),
            "ordinary output"
        );
    }

    #[tokio::test]
    async fn unavailable_observation_is_degraded_and_fail_open() {
        let temp = tempfile::tempdir().unwrap();
        let secret = temp.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let gcx = gcx_with_policy(&secret, &[], ShellBehavior::Withhold).await;
        let derived_zones = new_derived_privacy_zones();
        let mut message = tool_message("heuristic output");

        let decision = apply_shell_observation(
            &gcx,
            "cat secret.txt",
            temp.path(),
            &provider_destination("untrusted/model"),
            ObservationStatus::Unavailable("ptrace unavailable".to_string()),
            &derived_zones,
            &mut message,
        )
        .await
        .unwrap();

        assert_eq!(decision, ShellReadDecision::Pass);
        assert_eq!(message.content.content_text_only(), "heuristic output");
        assert_eq!(message.extra["privacy_observation"]["degraded"], true);
        assert_eq!(message.extra["privacy"]["files"][0]["zone"], "secrets");
        assert_eq!(
            message.extra["privacy"]["files"][0]["attribution"],
            "heuristic"
        );
        assert!(!message.extra.contains_key("privacy_shell"));
        assert!(refact_privacy::records_from_messages(&[message])
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn pending_observation_is_not_degraded() {
        let temp = tempfile::tempdir().unwrap();
        let secret = temp.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let gcx = gcx_with_policy(&secret, &[], ShellBehavior::Withhold).await;
        let derived_zones = new_derived_privacy_zones();
        let mut message = tool_message("background started");

        let decision = apply_shell_observation(
            &gcx,
            "sleep 30",
            temp.path(),
            &provider_destination("untrusted/model"),
            ObservationStatus::Pending(ObservedAccess::default()),
            &derived_zones,
            &mut message,
        )
        .await
        .unwrap();

        assert_eq!(decision, ShellReadDecision::Pass);
        assert_eq!(message.extra["privacy_observation"]["status"], "pending");
        assert_eq!(message.extra["privacy_observation"]["degraded"], false);
    }

    #[tokio::test]
    async fn ask_read_stays_hidden_until_decided_without_rerunning_command() {
        let temp = tempfile::tempdir().unwrap();
        let secret = temp.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        let gcx = gcx_with_policy(&secret, &[], ShellBehavior::Ask).await;
        let derived_zones = new_derived_privacy_zones();
        let mut message = tool_message("approval output");

        let decision = apply_shell_observation(
            &gcx,
            "cat secret.txt",
            temp.path(),
            &provider_destination("untrusted/model"),
            ObservationStatus::Observed(ObservedAccess {
                reads: vec![secret],
                writes: Vec::new(),
            }),
            &derived_zones,
            &mut message,
        )
        .await
        .unwrap();

        assert_eq!(decision, ShellReadDecision::Ask);
        assert!(shell_ask_pending(&message));
        let pending = message.content.content_text_only();
        assert!(pending.starts_with("Output awaiting user approval"));
        assert!(pending.contains("zone \"secrets\""));
        assert!(resolve_shell_ask(&mut message, true));
        assert_eq!(message.content.content_text_only(), "approval output");
        assert!(!shell_ask_pending(&message));
        assert!(refact_privacy::records_from_messages(&[message])
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn restrictive_policy_requires_observation_outside_the_workspace() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        gcx.privacy_policy_load.write().unwrap().policy = Arc::new(PrivacyPolicy {
            zones: vec![Zone {
                name: "secrets".to_string(),
                patterns: vec![".env".to_string()],
                send_to: Vec::new(),
                on_shell_read: ShellBehavior::Withhold,
            }],
            ..PrivacyPolicy::default()
        });

        assert!(shell_observation_needed(
            &gcx.privacy_policy_load,
            &provider_destination("untrusted/model")
        ));
    }

    #[test]
    fn merge_records_preserves_existing_records_and_deduplicates() {
        let mut message = ChatMessage::default();
        let first = FileRecord {
            path: "a.rs".to_string(),
            zone: "normal".to_string(),
            attribution: Attribution::Declared,
        };
        let second = FileRecord {
            path: ".env".to_string(),
            zone: "secrets".to_string(),
            attribution: Attribution::Declared,
        };

        attach_record(&mut message, first.clone());
        merge_records(&mut message, [first.clone(), second.clone()]);

        let privacy: PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, vec![first, second]);
    }

    #[test]
    fn merge_records_deduplicates_large_input_in_first_seen_order() {
        let all = (0..5_000)
            .map(|index| FileRecord {
                path: format!("file-{index}.rs"),
                zone: if index % 2 == 0 {
                    "normal".to_string()
                } else {
                    "secrets".to_string()
                },
                attribution: Attribution::Observed,
            })
            .collect::<Vec<_>>();
        let expected = all
            .iter()
            .filter(|record| record.zone != "normal")
            .cloned()
            .collect::<Vec<_>>();
        let records = all
            .iter()
            .cloned()
            .flat_map(|record| [record.clone(), record])
            .collect::<Vec<_>>();
        let mut message = ChatMessage::default();

        merge_records(&mut message, records);

        let privacy: PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, expected);
    }

    #[test]
    fn merge_message_records_unions_source_records() {
        let first = FileRecord {
            path: "a.rs".to_string(),
            zone: "normal".to_string(),
            attribution: Attribution::Declared,
        };
        let second = FileRecord {
            path: ".env".to_string(),
            zone: "secrets".to_string(),
            attribution: Attribution::Observed,
        };
        let third = FileRecord {
            path: "later.rs".to_string(),
            zone: "normal".to_string(),
            attribution: Attribution::Heuristic,
        };
        let mut source_a = ChatMessage::default();
        let mut source_b = ChatMessage::default();
        merge_records(&mut source_a, [first.clone(), second.clone()]);
        merge_records(
            &mut source_b,
            [second.clone(), third.clone(), first.clone()],
        );
        let mut target = ChatMessage::default();

        merge_message_records(&mut target, &[source_a, source_b]).unwrap();

        let privacy: PrivacyRecord =
            serde_json::from_value(target.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, vec![first, second]);
        assert!(!privacy.files.contains(&third));
    }

    #[test]
    fn merge_records_drops_inert_normal_observed_records() {
        let observed_normal = FileRecord {
            path: "/proj/.venv/lib/python3.11/site-packages/mod.py".to_string(),
            zone: "normal".to_string(),
            attribution: Attribution::Observed,
        };
        let mut message = ChatMessage::default();

        merge_records(&mut message, [observed_normal]);

        assert!(
            message.extra.get("privacy").is_none(),
            "normal+observed records are inert for the gate and must not be persisted"
        );
    }

    #[test]
    fn merge_records_keeps_every_guarded_record_regardless_of_attribution() {
        let guarded = [
            Attribution::Observed,
            Attribution::Declared,
            Attribution::Heuristic,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, attribution)| FileRecord {
            path: format!("secret-{index}.env"),
            zone: "secrets".to_string(),
            attribution,
        })
        .collect::<Vec<_>>();
        let mut message = ChatMessage::default();

        merge_records(&mut message, guarded.clone());

        let privacy: PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, guarded);
    }

    #[test]
    fn merge_records_keeps_blocked_zone_records() {
        let blocked = FileRecord {
            path: "/etc/shadow".to_string(),
            zone: "blocked".to_string(),
            attribution: Attribution::Observed,
        };
        let mut message = ChatMessage::default();

        merge_records(&mut message, [blocked.clone()]);

        let privacy: PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, vec![blocked]);
    }

    #[test]
    fn merge_records_keeps_effective_zone_records() {
        let effective = FileRecord {
            path: "derived.txt".to_string(),
            zone: "effective:secrets+internal".to_string(),
            attribution: Attribution::Observed,
        };
        let mut message = ChatMessage::default();

        merge_records(&mut message, [effective.clone()]);

        let privacy: PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, vec![effective]);
    }

    #[test]
    fn records_to_carry_does_not_amplify_normal_observed_records() {
        let guarded = FileRecord {
            path: ".env".to_string(),
            zone: "secrets".to_string(),
            attribution: Attribution::Observed,
        };
        let declared_normal = FileRecord {
            path: "src/main.rs".to_string(),
            zone: "normal".to_string(),
            attribution: Attribution::Declared,
        };
        let mut source = ChatMessage::default();
        merge_records(&mut source, [guarded.clone(), declared_normal.clone()]);
        source.extra.insert(
            "privacy".to_string(),
            serde_json::to_value(PrivacyRecord {
                files: vec![
                    guarded.clone(),
                    declared_normal.clone(),
                    FileRecord {
                        path: "/legacy/observed.rs".to_string(),
                        zone: "normal".to_string(),
                        attribution: Attribution::Observed,
                    },
                ],
            })
            .unwrap(),
        );

        let carried = records_to_carry(&[source]).unwrap();

        assert_eq!(carried, vec![guarded, declared_normal]);
    }
}
