use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub(crate) type CodexWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Default)]
pub(crate) struct OpenAICodexWebSocketSession {
    pub connection: Option<CodexWebSocket>,
    pub connection_key: Option<String>,
    pub turn_state: Option<String>,
    pub last_request_body: Option<Value>,
    pub last_response: Option<OpenAICodexWebSocketLastResponse>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OpenAICodexWebSocketLastResponse {
    pub response_id: String,
    pub output_items: Vec<Value>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OpenAICodexWebSocketResponseTracker {
    response_id: Option<String>,
    output_items: Vec<Value>,
}

impl OpenAICodexWebSocketResponseTracker {
    pub fn observe_event(&mut self, event: &Value) {
        if let Some(resp_id) = event
            .get("response")
            .and_then(|r| r.get("id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            self.response_id = Some(resp_id.to_string());
        }

        if event.get("type").and_then(|t| t.as_str()) == Some("response.output_item.done") {
            if let Some(item) = event.get("item") {
                self.output_items.push(item.clone());
            }
        }

        if matches!(
            event.get("type").and_then(|t| t.as_str()),
            Some("response.completed" | "response.incomplete")
        ) {
            if let Some(output) = event
                .get("response")
                .and_then(|r| r.get("output"))
                .and_then(|o| o.as_array())
            {
                self.output_items = output.clone();
            }
        }
    }

    pub fn into_last_response(self) -> Option<OpenAICodexWebSocketLastResponse> {
        let response_id = self.response_id?;
        Some(OpenAICodexWebSocketLastResponse {
            response_id,
            output_items: self.output_items,
        })
    }
}
