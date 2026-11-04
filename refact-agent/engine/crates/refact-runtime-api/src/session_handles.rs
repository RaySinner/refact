use refact_chat_api::SessionsMap;

use crate::SessionsHandle;

pub fn sessions_map_from_handle(handle: &std::sync::Arc<dyn SessionsHandle>) -> SessionsMap {
    handle
        .as_any()
        .downcast_ref::<SessionsMap>()
        .expect("engine sessions handle wraps SessionsMap")
        .clone()
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use async_trait::async_trait;
    use refact_chat_api::ChatSession;
    use refact_chat_api::SessionState;
    use tokio::sync::{Mutex as AMutex, RwLock as ARwLock};

    struct FakeSessionsHandle {
        map: SessionsMap,
    }

    #[async_trait]
    impl SessionsHandle for FakeSessionsHandle {
        fn as_any(&self) -> &dyn Any {
            &self.map
        }

        async fn session_ids(&self) -> Vec<String> {
            self.map.read().await.keys().cloned().collect()
        }

        async fn session_state(&self, _chat_id: &str) -> Option<SessionState> {
            None
        }
    }

    #[tokio::test]
    async fn bridge_preserves_map_identity() {
        let map: SessionsMap = Arc::new(ARwLock::new(HashMap::new()));
        let handle: Arc<dyn SessionsHandle> = Arc::new(FakeSessionsHandle {
            map: map.clone(),
        });
        let returned = sessions_map_from_handle(&handle);
        assert!(Arc::ptr_eq(&map, &returned));
    }

    #[tokio::test]
    async fn clone_shows_mutation_through_handle() {
        let map: SessionsMap = Arc::new(ARwLock::new(HashMap::new()));
        let handle: Arc<dyn SessionsHandle> = Arc::new(FakeSessionsHandle {
            map: map.clone(),
        });
        let returned = sessions_map_from_handle(&handle);
        returned
            .write()
            .await
            .insert("chat-1".to_string(), Arc::new(AMutex::new(dummy_session())));
        let source = handle
            .as_any()
            .downcast_ref::<SessionsMap>()
            .expect("engine sessions handle wraps SessionsMap");
        assert!(source.read().await.contains_key("chat-1"));
    }

    fn dummy_session() -> ChatSession {
        ChatSession::new("chat-1".to_string())
    }

    #[test]
    #[should_panic(expected = "engine sessions handle wraps SessionsMap")]
    fn wrong_type_handle_panics() {
        struct OtherHandle;

        #[async_trait]
        impl SessionsHandle for OtherHandle {
            fn as_any(&self) -> &dyn Any {
                &0u8
            }

            async fn session_ids(&self) -> Vec<String> {
                Vec::new()
            }

            async fn session_state(&self, _chat_id: &str) -> Option<SessionState> {
                None
            }
        }

        let handle: Arc<dyn SessionsHandle> = Arc::new(OtherHandle);
        let _ = sessions_map_from_handle(&handle);
    }
}
