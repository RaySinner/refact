use std::sync::Arc;
use tracing::info;

use async_trait::async_trait;
use tokio::sync::Mutex as AMutex;
use serde_json::Value;
use std::collections::HashMap;

use crate::at_commands::at_commands::{AtCommand, AtCommandsContext, AtParam};
use crate::at_commands::execute_at::AtCommandMember;
use crate::call_validation::{ChatMessage, ContextEnum};

pub use refact_at_web::web_fetch::{execute_at_web, execute_at_web_result, WebFetchResult};

pub struct AtWeb {
    pub params: Vec<Box<dyn AtParam>>,
}

impl AtWeb {
    pub fn new() -> Self {
        AtWeb { params: vec![] }
    }
}

#[async_trait]
impl AtCommand for AtWeb {
    fn params(&self) -> &Vec<Box<dyn AtParam>> {
        &self.params
    }

    async fn at_execute(
        &self,
        ccx: Arc<AMutex<AtCommandsContext>>,
        cmd: &mut AtCommandMember,
        args: &mut Vec<AtCommandMember>,
    ) -> Result<(Vec<ContextEnum>, String), String> {
        let url = match args.get(0) {
            Some(x) => x.clone(),
            None => {
                cmd.ok = false;
                cmd.reason = Some("missing URL".to_string());
                args.clear();
                return Err("missing URL".to_string());
            }
        };
        args.truncate(1);

        let preview_cache = {
            let cgcx = ccx.lock().await;
            cgcx.app.workspace.at_commands_preview_cache.clone()
        };
        let text_from_cache = preview_cache
            .lock()
            .await
            .get(&format!("@web:{}", url.text));

        let text = match text_from_cache {
            Some(text) => text,
            None => {
                let text = execute_at_web(&url.text, None)
                    .await
                    .map_err(|e| format!("Failed to execute @web {}.\nError: {e}", url.text))?;
                preview_cache
                    .lock()
                    .await
                    .insert(format!("@web:{}", url.text), text.clone());
                text
            }
        };

        let message = ChatMessage::new("plain_text".to_string(), text);

        info!("executed @web {}", url.text);
        Ok((
            vec![ContextEnum::ChatMessage(message)],
            format!("[see text downloaded from {} above]", url.text),
        ))
    }

    fn depends_on(&self) -> Vec<String> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use tracing::warn;
    use tracing::info;
    use super::*;

    #[tokio::test]
    async fn test_execute_at_web_jina() {
        let url = "https://doc.rust-lang.org/book/ch03-04-comments.html";
        match execute_at_web(url, None).await {
            Ok(text) => info!(
                "test executed successfully (length: {} chars):\n\n{}",
                text.len(),
                &text[..text.len().min(500)]
            ),
            Err(e) => warn!("test failed with error: {e}"),
        }
    }

    #[tokio::test]
    async fn test_jina_pdf_reading() {
        let url = "https://www.w3.org/WAI/WCAG21/Techniques/pdf/PDF1.pdf";
        match execute_at_web(url, None).await {
            Ok(text) => info!(
                "PDF test executed successfully (length: {} chars)",
                text.len()
            ),
            Err(e) => warn!("PDF test failed with error: {e}"),
        }
    }

    #[tokio::test]
    async fn test_jina_with_options() {
        let url = "https://doc.rust-lang.org/book/ch03-04-comments.html";
        let mut options = HashMap::new();
        options.insert(
            "target_selector".to_string(),
            Value::String("main".to_string()),
        );
        match execute_at_web(url, Some(&options)).await {
            Ok(text) => info!(
                "options test executed successfully (length: {} chars)",
                text.len()
            ),
            Err(e) => warn!("options test failed with error: {e}"),
        }
    }
}
