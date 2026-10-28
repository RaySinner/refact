use refact_chat_api::{
    ToolEnrichmentKind, ToolEnrichmentProvenance, ToolEnrichmentReference,
    ToolEnrichmentReferenceDetails,
};
use refact_core::chat_types::{ChatContent, ChatMessage};
use refact_privacy::PrivacyRecord;

pub fn tool_result_is_privacy_restricted(message: &ChatMessage) -> bool {
    if message.extra.contains_key("privacy_shell")
        || message.extra.contains_key("privacy_observation")
    {
        return true;
    }
    match message.extra.get("privacy") {
        Some(value) => serde_json::from_value::<PrivacyRecord>(value.clone())
            .map(|privacy| privacy.files.iter().any(|file| file.zone != "normal"))
            .unwrap_or(true),
        None => false,
    }
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn is_safe_mime(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'.' | b'-'))
}

pub fn native_references(message: &ChatMessage) -> Vec<ToolEnrichmentReference> {
    let mut references: Vec<ToolEnrichmentReference> = Vec::new();
    if let ChatContent::ContextFiles(files) = &message.content {
        for file in files {
            let mut path = reference(ToolEnrichmentKind::Path, &file.file_name);
            path.line1 = (file.line1 > 0).then_some(file.line1);
            path.line2 = (file.line2 > 0).then_some(file.line2);
            path.source = Some("context_file".to_string());
            references.push(path);
            references.extend(file.symbols.iter().map(|symbol| {
                let mut reference = reference(ToolEnrichmentKind::Symbol, symbol);
                reference.source = Some("context_file".to_string());
                reference
            }));
        }
    }
    if let Some(results) = message
        .extra
        .get("search_results")
        .and_then(serde_json::Value::as_array)
    {
        references.extend(results.iter().filter_map(|result| {
            let url = result.get("url")?.as_str()?;
            let mut reference = reference(ToolEnrichmentKind::Url, url);
            reference.label = result
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string);
            reference.summary = result
                .get("source")
                .and_then(serde_json::Value::as_str)
                .filter(|source| is_safe_identifier(source))
                .map(|source| {
                    let span = result
                        .get("citation_span")
                        .and_then(serde_json::Value::as_u64)
                        .map(|span| format!(" · span: {span}"))
                        .unwrap_or_default();
                    format!("source: {source}{span}")
                });
            Some(reference)
        }));
    }
    if let Some(fetch) = message.extra.get("web_fetch") {
        let source = fetch.get("source").and_then(serde_json::Value::as_str);
        let status = fetch
            .get("status")
            .and_then(serde_json::Value::as_u64)
            .map(|status| status.to_string());
        let content_type = fetch
            .get("content_type")
            .and_then(serde_json::Value::as_str)
            .filter(|content_type| is_safe_mime(content_type));
        for (field, label) in [("requested_url", "requested"), ("final_url", "final")] {
            let Some(url) = fetch.get(field).and_then(serde_json::Value::as_str) else {
                continue;
            };
            let mut reference = reference(ToolEnrichmentKind::Url, url);
            reference.label = source
                .filter(|source| matches!(*source, "jina_reader" | "simple_fetch"))
                .map(|source| format!("{label} · {source}"));
            reference.status = status.clone();
            reference.summary = content_type.map(|content_type| content_type.to_string());
            references.push(reference);
        }
    }
    if let Some(browser) = message.extra.get("browser") {
        if let Some(url) = browser.get("page_url").and_then(serde_json::Value::as_str) {
            let mut reference = reference(ToolEnrichmentKind::Url, url);
            reference.label = Some("browser page".to_string());
            reference.status = browser
                .get("status")
                .and_then(serde_json::Value::as_u64)
                .map(|status| status.to_string());
            references.push(reference);
        }
        for (field, label) in [
            ("network_count", "network requests"),
            ("console_errors", "console errors"),
            ("console_warnings", "console warnings"),
        ] {
            let Some(count) = browser.get(field).and_then(serde_json::Value::as_u64) else {
                continue;
            };
            let mut reference = reference(
                ToolEnrichmentKind::Diagnostic,
                &format!("browser:{field}:{count}"),
            );
            reference.label = Some(label.to_string());
            references.push(reference);
        }
        if let Some(artifacts) = browser
            .get("artifacts")
            .and_then(serde_json::Value::as_array)
        {
            references.extend(artifacts.iter().filter_map(|artifact| {
                let id = artifact.get("id")?.as_str()?;
                let kind = artifact.get("kind")?.as_str()?;
                let mime = artifact.get("mime")?.as_str()?;
                let bytes = artifact.get("bytes")?.as_u64()?;
                if !is_safe_identifier(id) || !is_safe_identifier(kind) || !is_safe_mime(mime) {
                    return None;
                }
                let mut reference =
                    reference(ToolEnrichmentKind::Artifact, &format!("artifact:{id}"));
                reference.label = Some(kind.to_string());
                reference.summary = Some(format!("{mime} · {bytes} bytes"));
                Some(reference)
            }));
        }
    }
    if let Some(mcp) = message.extra.get("mcp") {
        let server = mcp.get("server").and_then(serde_json::Value::as_str);
        let tool = mcp.get("tool").and_then(serde_json::Value::as_str);
        let schema_hash = mcp.get("schema_hash").and_then(serde_json::Value::as_str);
        let result_type = mcp.get("result_type").and_then(serde_json::Value::as_str);
        let status = mcp.get("status").and_then(serde_json::Value::as_str);
        if let (Some(server), Some(tool), Some(schema_hash), Some(result_type), Some(status)) =
            (server, tool, schema_hash, result_type, status)
        {
            if !is_safe_identifier(server)
                || !is_safe_identifier(tool)
                || schema_hash.len() != 16
                || !schema_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !matches!(
                    result_type,
                    "json" | "text" | "multimodal" | "context_files"
                )
                || !matches!(status, "success" | "failed")
            {
                return references;
            }
            let mut reference = reference(
                ToolEnrichmentKind::Symbol,
                &format!("mcp::{server}::{tool}"),
            );
            reference.label = Some(format!("MCP {server}/{tool}"));
            reference.summary = Some(format!("schema {schema_hash} · {result_type}"));
            reference.status = Some(status.to_string());
            references.push(reference);
        }
    }
    for citation in &message.citations {
        let Some(url) = citation.get("url").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let mut reference = reference(ToolEnrichmentKind::Citation, url);
        reference.label = citation
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string);
        if let (Some(start), Some(end)) = (
            citation
                .get("start_char_index")
                .and_then(serde_json::Value::as_u64),
            citation
                .get("end_char_index")
                .and_then(serde_json::Value::as_u64),
        ) {
            reference.summary = Some(format!("span: {start}-{end}"));
        }
        references.push(reference);
    }
    if let Some(exec) = message.extra.get("exec") {
        if let Some(process_id) = exec.get("process_id").and_then(serde_json::Value::as_str) {
            let mut reference = reference(ToolEnrichmentKind::Process, process_id);
            reference.label = exec
                .get("short_description")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string);
            reference.status = exec
                .get("status")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string);
            references.push(reference);
        }
    }
    if let Some(agent_id) = message
        .extra
        .get("background_agent_id")
        .and_then(serde_json::Value::as_str)
    {
        let mut agent_reference = reference(ToolEnrichmentKind::Agent, agent_id);
        agent_reference.status = message
            .extra
            .get("background_agent_status")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string);
        agent_reference.details = Some(ToolEnrichmentReferenceDetails {
            parent_chat_id: message
                .extra
                .get("background_agent_parent_chat_id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
            child_chat_id: message
                .extra
                .get("child_chat_id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string),
            result_available: message
                .extra
                .get("background_agent_result_available")
                .and_then(serde_json::Value::as_bool),
            conflict: message
                .extra
                .get("background_agent_conflict")
                .and_then(serde_json::Value::as_bool),
            ..Default::default()
        });
        references.push(agent_reference);
        for (key, status) in [("target_files", "target"), ("edited_files", "edited")] {
            if let Some(paths) = message.extra.get(key).and_then(serde_json::Value::as_array) {
                references.extend(paths.iter().filter_map(|path| {
                    let path = path.as_str()?;
                    let mut reference = reference(ToolEnrichmentKind::Path, path);
                    reference.label = Some(format!("agent {status}"));
                    reference.status = Some(status.to_string());
                    reference.details = Some(ToolEnrichmentReferenceDetails {
                        action: Some(status.to_string()),
                        ..Default::default()
                    });
                    Some(reference)
                }));
            }
        }
    }
    references
}

pub fn git_references(message: &ChatMessage) -> Vec<ToolEnrichmentReference> {
    let Some(git) = message.extra.get("git") else {
        return Vec::new();
    };
    let Some(status) = git.get("status").and_then(serde_json::Value::as_str) else {
        return Vec::new();
    };
    let target = git
        .get("path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("workspace");
    let mut reference = reference(ToolEnrichmentKind::Git, target);
    reference.status = Some(status.to_string());
    reference.details = Some(ToolEnrichmentReferenceDetails {
        short_sha: git
            .get("short_sha")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        scope: git
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        ..Default::default()
    });
    reference.truncated = git
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    vec![reference]
}

pub fn review_references(message: &ChatMessage) -> Vec<ToolEnrichmentReference> {
    let Some(references) = message
        .extra
        .get("review_refs")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    references
        .iter()
        .filter_map(|value| {
            let path = value.get("path")?.as_str()?;
            let mut reference = reference(ToolEnrichmentKind::Review, path);
            reference.label = value
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string);
            reference.status = value
                .get("severity")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string);
            reference.details = Some(ToolEnrichmentReferenceDetails {
                action: value
                    .get("evidence_kind")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
                scope: value
                    .get("scope")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
                ..Default::default()
            });
            reference.line1 = value
                .get("line1")
                .and_then(serde_json::Value::as_u64)
                .and_then(|line| usize::try_from(line).ok());
            reference.line2 = value
                .get("line2")
                .and_then(serde_json::Value::as_u64)
                .and_then(|line| usize::try_from(line).ok());
            Some(reference)
        })
        .collect()
}

pub fn reference(kind: ToolEnrichmentKind, target: &str) -> ToolEnrichmentReference {
    ToolEnrichmentReference {
        provenance: ToolEnrichmentProvenance::Native,
        ..ToolEnrichmentReference::new(kind, target)
    }
}
