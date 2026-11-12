use crate::call_validation::ChatMessage;
use crate::chat::ctx::ChatCtx;
use refact_privacy::{Destination, DestinationId, DestinationKind, PrivacyAuditError, PrivacyAudited};

use super::*;

pub(crate) struct AuditedSubchatReport(Vec<(usize, refact_privacy::FileRecord)>);

impl PrivacyAudited for AuditedSubchatReport {
    fn privacy_records(
        &self,
    ) -> Result<Vec<(usize, refact_privacy::FileRecord)>, PrivacyAuditError> {
        Ok(self.0.clone())
    }
}

pub(crate) fn unique_privacy_records(
    messages: &[ChatMessage],
) -> Result<Vec<(usize, refact_privacy::FileRecord)>, PrivacyAuditError> {
    refact_privacy::records_from_messages(messages).map(|indexed| {
        indexed
            .into_iter()
            .fold(Vec::new(), |mut records, (message_index, record)| {
                if !records.iter().any(|(_, existing)| existing == &record) {
                    records.push((message_index, record));
                }
                records
            })
    })
}

pub(crate) fn subagent_destination(model_id: &str) -> Destination {
    let id = model_id
        .split_once('/')
        .map_or(model_id, |(provider, _)| provider);
    Destination {
        id: DestinationId(id.to_string()),
        kind: DestinationKind::SubagentModel,
        display_name: model_id.to_string(),
    }
}

pub(crate) fn gate_subchat_boundary(
    ctx: &ChatCtx,
    messages: &[ChatMessage],
    model_id: &str,
) -> Result<(), String> {
    let records = unique_privacy_records(messages).map_err(|error| error.to_string())?;
    let policy = ctx.privacy_policy_load.read().unwrap().policy.clone();
    let compiled = policy.compile().map_err(|error| error.to_string())?;
    match refact_privacy::clear(
        AuditedSubchatReport(records),
        &subagent_destination(model_id),
        &compiled,
    ) {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.offending.is_empty() => Err(refusal.message),
        Err(refusal) => Err(refusal.model_facing().to_string()),
    }
}

pub(crate) fn prepare_subchat_messages(
    ctx: &ChatCtx,
    messages: Vec<ChatMessage>,
    model_id: &str,
) -> Result<Vec<ChatMessage>, String> {
    let messages = refact_core::active_context::active_context(&messages)
        .map_err(|e| e.to_string())?
        .messages;
    gate_subchat_boundary(ctx, &messages, model_id)?;
    if refact_privacy::records_from_messages(&messages)
        .map_err(|error| error.to_string())?
        .is_empty()
    {
        return Ok(messages);
    }

    let mut prepared = Vec::with_capacity(messages.len() + 1);
    prepared.push(ChatMessage::new(
        "system".to_string(),
        GUARDED_REPORT_INSTRUCTION.to_string(),
    ));
    prepared.extend(messages);
    Ok(prepared)
}

pub(crate) fn apply_subchat_report_policy(
    policy: &refact_privacy::SubagentPolicy,
    messages: &mut [ChatMessage],
) -> Result<(), PrivacyAuditError> {
    let Some(report_index) = messages
        .iter()
        .rposition(|message| message.role == "assistant")
    else {
        return Ok(());
    };
    let records = unique_privacy_records(messages)?;
    messages[report_index].extra.remove("privacy");
    if policy.report_declassifies {
        return Ok(());
    }

    refact_privacy::merge_records(
        &mut messages[report_index],
        records.into_iter().map(|(_, record)| record),
    );
    Ok(())
}
