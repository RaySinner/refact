pub use refact_at_web::web_fetch::{execute_at_web, execute_at_web_result, WebFetchResult};
pub use refact_runtime_api::at_commands_impl::at_web::AtWeb;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tracing::{info, warn};

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
            serde_json::Value::String("main".to_string()),
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
