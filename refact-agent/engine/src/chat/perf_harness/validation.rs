use std::sync::Arc;

use tokio::sync::Mutex as AMutex;

use crate::chat::types::ChatSession;

#[allow(hidden_glob_reexports)]
pub use refact_perf::perf_harness::validation::*;

use refact_perf::perf_harness::types::ToolPoolWorkload;

pub async fn assert_tool_result_order(
    session: &Arc<AMutex<ChatSession>>,
    workload: &ToolPoolWorkload,
    chat_index: u8,
) -> Result<(), String> {
    let session = session.lock().await;
    let results = session
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .collect::<Vec<_>>();
    if results.len() != workload.tool_calls_per_chat as usize {
        return Err(format!(
            "tool pool fixture chat {chat_index} returned {} results instead of {}",
            results.len(),
            workload.tool_calls_per_chat
        ));
    }
    for (call_index, result) in results.iter().enumerate() {
        let expected = format!(
            "pool-{}-{chat_index}-{call_index}",
            workload.fixture_signature()
        );
        if result.tool_call_id != expected || result.tool_failed == Some(true) {
            return Err(format!(
                "tool pool fixture chat {chat_index} lost result ordering"
            ));
        }
    }
    Ok(())
}
