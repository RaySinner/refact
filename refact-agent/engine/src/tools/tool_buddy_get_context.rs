pub use refact_tools::tools::tool_buddy_get_context::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::HashMap;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::Arc;
#[cfg(test)]
#[allow(unused_imports)]
use async_trait::async_trait;
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::Value;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::Mutex as AMutex;
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::AtCommandsContext;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{Tool, ToolDesc, ToolSource, ToolSourceType};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buddy_get_context_sections() {
        let mut args = HashMap::new();
        args.insert(
            "sections".to_string(),
            Value::Array(vec![Value::String("project_info".to_string())]),
        );
        assert_eq!(
            parse_requested_sections(&args).unwrap(),
            vec!["project_info".to_string()]
        );

        args.insert(
            "sections".to_string(),
            Value::String("not json".to_string()),
        );
        let err = parse_requested_sections(&args).unwrap_err();
        assert!(err.contains("valid sections"));

        args.insert(
            "sections".to_string(),
            Value::Array(vec![Value::String("unknown".to_string())]),
        );
        let err = parse_requested_sections(&args).unwrap_err();
        assert!(err.contains("unknown section"));
        assert!(err.contains("project_info"));
    }

    #[test]
    fn test_redact_config() {
        let raw = r#"{"api_key": "sk-secret123456789012345", "name": "test"}"#;
        let redacted = redact_config(raw);
        assert!(!redacted.contains("sk-secret123456789012345"));
        assert!(redacted.contains("[REDACTED]"));
        assert!(redacted.contains("test"));
    }
}
