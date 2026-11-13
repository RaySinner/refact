pub use refact_tools::tools::tool_search_trajectories::*;

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
use refact_core::chat_types::{ChatMessage, ChatContent, ContextEnum};
#[cfg(test)]
#[allow(unused_imports)]
use crate::tools::tools_description::{ Tool, ToolDesc, ToolSource, ToolSourceType, json_schema_from_params, };
#[cfg(test)]
#[allow(unused_imports)]
use refact_runtime_api::memories::memories_search;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::settings_guard::SettingsGuard;
    use serial_test::serial;

    #[test]
    #[serial(runtime_settings)]
    fn hist_search_preview_chars_setting_is_read_from_settings() {
        let _guard = SettingsGuard::install(|settings| {
            settings.hist_search_preview_chars = 25;
        });

        assert_eq!(
            refact_core::trajectory_settings::current().hist_search_preview_chars,
            25
        );
        let notice = hist_search_preview_notice(25, 900, 25);
        assert!(
            notice.contains("showing 25 of 900 preview characters"),
            "{}",
            notice
        );
        assert!(
            notice.contains("hist_search_preview_chars = 25"),
            "{}",
            notice
        );
    }

    #[test]
    fn hist_search_preview_notice_quantifies_shown_and_total() {
        let notice = hist_search_preview_notice(400, 12_345, 400);
        assert!(
            notice.contains("showing 400 of 12345 preview characters"),
            "{}",
            notice
        );
        assert!(
            notice.contains("hist_search_preview_chars = 400"),
            "{}",
            notice
        );
    }
}
