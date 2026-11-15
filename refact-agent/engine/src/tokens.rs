pub use refact_runtime_api::chat_tokens::{
    cached_tokenizer, cached_tokenizer_for_ctx, cached_tokenizer_for_parts,
    tokenizer_load_is_disabled,
};
pub use refact_core::tokens_runtime::{count_text_tokens, count_text_tokens_with_fallback};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_load_is_disabled_for_fake_and_predefined_cloud_tokenizers() {
        assert!(tokenizer_load_is_disabled("fake"));
        assert!(tokenizer_load_is_disabled("fake-gemini"));
        assert!(tokenizer_load_is_disabled("openai"));
        assert!(tokenizer_load_is_disabled("anthropic"));
        assert!(tokenizer_load_is_disabled("claude"));
    }

    #[test]
    fn tokenizer_load_remains_enabled_for_external_tokenizer_sources() {
        assert!(!tokenizer_load_is_disabled("hf://Xenova/claude-tokenizer"));
        assert!(!tokenizer_load_is_disabled(
            "https://example.com/tokenizer.json"
        ));
        assert!(!tokenizer_load_is_disabled("file:///tmp/tokenizer.json"));
        assert!(!tokenizer_load_is_disabled("/tmp/tokenizer.json"));
    }
}
