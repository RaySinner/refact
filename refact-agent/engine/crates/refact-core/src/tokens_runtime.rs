use std::sync::Arc;

use tokenizers::Tokenizer;

/// Estimate as length / 3.5, since 3 is reasonable estimate for code, and 4 for natural language
fn estimate_tokens(text: &str) -> usize {
    1 + text.len() * 2 / 7
}

pub fn count_text_tokens(tokenizer: Option<Arc<Tokenizer>>, text: &str) -> Result<usize, String> {
    match tokenizer {
        Some(tokenizer) => match tokenizer.encode_fast(text, false) {
            Ok(tokens) => Ok(tokens.len()),
            Err(e) => Err(format!("Encoding error: {e}")),
        },
        None => Ok(estimate_tokens(text)),
    }
}

pub fn count_text_tokens_with_fallback(tokenizer: Option<Arc<Tokenizer>>, text: &str) -> usize {
    count_text_tokens(tokenizer, text).unwrap_or_else(|e| {
        tracing::error!("{e}");
        estimate_tokens(text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_text_tokens_without_tokenizer_estimates_from_text_length() {
        assert_eq!(count_text_tokens(None, "").unwrap(), 1);
        assert_eq!(count_text_tokens(None, "hello").unwrap(), 2);
        let long = "x".repeat(700);
        assert_eq!(count_text_tokens(None, &long).unwrap(), 201);
        assert_eq!(count_text_tokens_with_fallback(None, &long), 201);
    }
}
