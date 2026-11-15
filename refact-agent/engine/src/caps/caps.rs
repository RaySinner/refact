pub use refact_runtime_api::caps_load_runtime::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_models_dev_startup_refresh_flag_is_consumed_once() {
        let gcx = crate::global_context::tests::make_test_gcx().await;
        let caps_state = gcx.caps_state.clone();
        caps_state
            .write()
            .await
            .models_dev_startup_refresh_attempted = false;

        assert!(take_models_dev_startup_refresh_flag(gcx.clone()).await);
        assert!(!take_models_dev_startup_refresh_flag(gcx).await);
    }
}
