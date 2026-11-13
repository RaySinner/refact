pub use refact_tools::tools::tool_rm::*;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use crate::tools::tools_description::Tool;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rm_schema_no_longer_advertises_max_depth() {
        let schema = ToolRm {
            config_path: String::new(),
        }
        .tool_description()
        .input_schema;
        let rendered = serde_json::to_string(&schema).unwrap();

        assert!(
            !rendered.contains("max_depth"),
            "the dead max_depth arg is still advertised: {}",
            rendered
        );
        assert!(rendered.contains("recursive") && rendered.contains("dry_run"));
    }

    #[test]
    fn rm_parse_recursive_ignores_removed_max_depth_arg() {
        let args = HashMap::from_iter([
            ("recursive".to_string(), Value::Bool(true)),
            ("max_depth".to_string(), serde_json::json!(3)),
        ]);

        assert_eq!(ToolRm::parse_recursive(&args).unwrap(), (true, false));
    }
}
