use std::collections::HashSet;

use refact_core::chat_types::ChatMessage;

use crate::gate::{PrivacyAuditError, records_from_messages};
use crate::record::{Attribution, FileRecord, PrivacyRecord};

pub fn attach_record(message: &mut ChatMessage, record: FileRecord) {
    merge_records(message, std::iter::once(record));
}

fn record_is_persistable(record: &FileRecord) -> bool {
    record.zone != "normal" || matches!(record.attribution, Attribution::Declared)
}

pub fn merge_records(message: &mut ChatMessage, records: impl IntoIterator<Item = FileRecord>) {
    let mut privacy = message
        .extra
        .remove("privacy")
        .and_then(|value| serde_json::from_value::<PrivacyRecord>(value).ok())
        .unwrap_or_default();
    let mut seen = privacy.files.iter().cloned().collect::<HashSet<_>>();
    for record in records {
        if !record_is_persistable(&record) {
            continue;
        }
        if seen.insert(record.clone()) {
            privacy.files.push(record);
        }
    }
    if !privacy.files.is_empty() {
        message.extra.insert(
            "privacy".to_string(),
            serde_json::to_value(privacy).expect("privacy records should serialize"),
        );
    }
}

pub fn records_to_carry(
    sources: &[ChatMessage],
) -> Result<Vec<FileRecord>, PrivacyAuditError> {
    records_from_messages(sources).map(|indexed| {
        let mut seen = HashSet::with_capacity(indexed.len());
        indexed
            .into_iter()
            .filter_map(|(_, record)| {
                (record_is_persistable(&record) && seen.insert(record.clone())).then_some(record)
            })
            .collect()
    })
}

pub fn merge_message_records(
    message: &mut ChatMessage,
    sources: &[ChatMessage],
) -> Result<(), PrivacyAuditError> {
    let records = records_to_carry(sources)?;
    merge_records(message, records);
    Ok(())
}

pub fn carry_records_into(
    message: &mut ChatMessage,
    sources: &[ChatMessage],
) -> Result<(), PrivacyAuditError> {
    merge_message_records(message, sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Attribution;

    fn record(path: &str, zone: &str, attribution: Attribution) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            zone: zone.to_string(),
            attribution,
        }
    }

    #[test]
    fn attach_record_inserts_declared_and_guarded_records() {
        let mut message = ChatMessage::default();

        attach_record(
            &mut message,
            record("src/main.rs", "normal", Attribution::Declared),
        );
        attach_record(
            &mut message,
            record(".env", "secrets", Attribution::Observed),
        );

        let privacy: PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();
        assert_eq!(
            privacy.files,
            vec![
                record("src/main.rs", "normal", Attribution::Declared),
                record(".env", "secrets", Attribution::Observed),
            ]
        );
    }

    #[test]
    fn merge_records_deduplicates_in_first_seen_order_and_retains_existing() {
        let mut message = ChatMessage::default();
        let first = record("a.rs", "normal", Attribution::Declared);
        let second = record(".env", "secrets", Attribution::Declared);

        attach_record(&mut message, first.clone());
        merge_records(&mut message, [first.clone(), second.clone()]);

        let privacy: PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, vec![first, second]);
    }

    #[test]
    fn merge_records_filters_normal_inferred_but_keeps_declared_normal() {
        let observed_normal = record(
            "/proj/.venv/lib/python3.11/site-packages/mod.py",
            "normal",
            Attribution::Observed,
        );
        let declared_normal = record("src/main.rs", "normal", Attribution::Declared);
        let mut message = ChatMessage::default();

        merge_records(&mut message, [observed_normal, declared_normal.clone()]);

        let privacy: PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, vec![declared_normal]);
    }

    #[test]
    fn merge_records_keeps_existing_records_unfiltered() {
        let stale = record("/legacy/observed.rs", "normal", Attribution::Observed);
        let mut message = ChatMessage::default();
        message.extra.insert(
            "privacy".to_string(),
            serde_json::to_value(PrivacyRecord {
                files: vec![stale.clone()],
            })
            .unwrap(),
        );

        merge_records(&mut message, []);

        let privacy: PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, vec![stale]);
    }

    #[test]
    fn merge_records_defaults_malformed_existing_metadata() {
        let guarded = record(".env", "secrets", Attribution::Observed);
        let mut message = ChatMessage::default();
        message.extra.insert(
            "privacy".to_string(),
            serde_json::json!({ "files": "not-an-array" }),
        );

        merge_records(&mut message, [guarded.clone()]);

        let privacy: PrivacyRecord =
            serde_json::from_value(message.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, vec![guarded]);
    }

    #[test]
    fn merge_records_skips_insertion_when_nothing_persists() {
        let mut message = ChatMessage::default();

        merge_records(
            &mut message,
            [record("src/main.rs", "normal", Attribution::Observed)],
        );

        assert!(message.extra.get("privacy").is_none());
    }

    #[test]
    fn records_to_carry_keeps_first_seen_order_and_deduplicates() {
        let first = record("a.rs", "secrets", Attribution::Observed);
        let second = record("b.rs", "secrets", Attribution::Observed);
        let mut source_a = ChatMessage::default();
        let mut source_b = ChatMessage::default();
        merge_records(&mut source_a, [first.clone(), second.clone()]);
        merge_records(&mut source_b, [second.clone(), first.clone()]);

        let carried = records_to_carry(&[source_a, source_b]).unwrap();

        assert_eq!(carried, vec![first, second]);
    }

    #[test]
    fn records_to_carry_propagates_malformed_source_errors() {
        let mut malformed = ChatMessage::default();
        malformed.extra.insert(
            "privacy".to_string(),
            serde_json::json!({ "files": "not-an-array" }),
        );

        let error = records_to_carry(&[malformed]).unwrap_err();

        assert!(error.message.contains("malformed privacy metadata"));
    }

    #[test]
    fn merge_message_records_unions_source_records() {
        let first = record("a.rs", "normal", Attribution::Declared);
        let second = record(".env", "secrets", Attribution::Observed);
        let third = record("later.rs", "normal", Attribution::Heuristic);
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
    fn carry_records_into_mirrors_merge_message_records() {
        let guarded = record(".env", "secrets", Attribution::Observed);
        let mut source = ChatMessage::default();
        merge_records(&mut source, [guarded.clone()]);
        let mut target = ChatMessage::default();

        carry_records_into(&mut target, &[source]).unwrap();

        let privacy: PrivacyRecord =
            serde_json::from_value(target.extra["privacy"].clone()).unwrap();
        assert_eq!(privacy.files, vec![guarded]);
    }
}
