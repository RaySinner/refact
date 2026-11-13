use std::sync::Arc;
use tracing::info;

use async_trait::async_trait;
use tokio::sync::Mutex as AMutex;

use crate::at_commands_ctx::{AtCommand, AtCommandMember, AtCommandsContext, AtParam};
use refact_core::chat_types::{ChatMessage, ContextEnum};

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

