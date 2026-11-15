pub use refact_http::integrations::chat_browser_context::*;

#[cfg(test)]
#[allow(unused_imports)]
use refact_chat_api::internal_roles::event;
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::chat_types::{ChatContent, ChatMessage};
#[cfg(test)]
#[allow(unused_imports)]
use refact_core::image_policy::ImagePolicy;
#[cfg(test)]
#[allow(unused_imports)]
use refact_integrations::browser_types::{
    ConsoleEntry, MutationSummaryEntry, NetworkEntry, RecorderEvent,
};
#[cfg(test)]
#[allow(unused_imports)]
use base64::Engine;

#[cfg(test)]
mod tests {
    use super::*;
    use refact_core::image_policy::ImageFormat;

    fn empty_snapshot(page_changed: bool) -> BrowserContextSnapshot {
        BrowserContextSnapshot {
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            actions: vec![],
            console: vec![],
            network: vec![],
            mutations: vec![],
            total_bytes: 8,
            page_changed,
        }
    }

    fn tiny_png() -> Vec<u8> {
        let image = image::DynamicImage::new_rgba8(2, 2);
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn test_format_browser_context_full() {
        let snapshot = BrowserContextSnapshot {
            url: "https://example.com".to_string(),
            title: "Example Page".to_string(),
            actions: vec![
                RecorderEvent::Navigation {
                    url: "https://example.com".to_string(),
                    title: "Example Page".to_string(),
                    timestamp: 45245000.0,
                },
                RecorderEvent::Click {
                    selector: "button.submit".to_string(),
                    text: "Submit Form".to_string(),
                    x: 200.0,
                    y: 350.0,
                    timestamp: 45248000.0,
                },
                RecorderEvent::Input {
                    selector: "input#email".to_string(),
                    value: "user@example.com".to_string(),
                    masked: false,
                    timestamp: 45250000.0,
                    tag: None,
                    input_type: None,
                    field_name: None,
                    placeholder: None,
                    aria_label: None,
                    role: None,
                },
            ],
            console: vec![ConsoleEntry {
                timestamp: 45246000.0,
                level: "error".to_string(),
                text: "Uncaught TypeError: Cannot read property 'x' of null".to_string(),
            }],
            network: vec![NetworkEntry {
                timestamp: 45245000.0,
                method: "GET".to_string(),
                url: "https://example.com".to_string(),
                resource_type: "Document".to_string(),
                status: Some(200),
                ..NetworkEntry::default()
            }],
            mutations: vec![MutationSummaryEntry {
                timestamp: 45247000.0,
                added: 3,
                removed: 1,
                changed: 2,
                descriptions: vec![],
            }],
            total_bytes: 500,
            page_changed: true,
        };

        let formatted = format_browser_context(&snapshot);
        assert!(formatted.contains("[Browser Context]"));
        assert!(formatted.contains("URL: https://example.com"));
        assert!(formatted.contains("Title: Example Page"));
        assert!(formatted.contains("## User Actions (since last message)"));
        assert!(formatted.contains("navigate → https://example.com"));
        assert!(formatted.contains("click → button.submit \"Submit Form\""));
        assert!(formatted.contains("input → input#email \"user@example.com\""));
        assert!(formatted.contains("## Console (since last message)"));
        assert!(formatted.contains("[error] Uncaught TypeError"));
        assert!(formatted.contains("## Network (since last message)"));
        assert!(formatted.contains("GET https://example.com → 200"));
        assert!(formatted.contains("## DOM Changes (since last message)"));
        assert!(formatted.contains("Added: 3 elements"));
        assert!(formatted.contains("Removed: 1 elements"));
        assert!(formatted.contains("Changed: 2 elements"));
    }

    #[test]
    fn test_format_browser_context_empty_sections() {
        let snapshot = BrowserContextSnapshot {
            url: "https://test.com".to_string(),
            title: "Test".to_string(),
            actions: vec![],
            console: vec![],
            network: vec![],
            mutations: vec![],
            total_bytes: 0,
            page_changed: false,
        };

        let formatted = format_browser_context(&snapshot);
        assert!(formatted.contains("[Browser Context]"));
        assert!(formatted.contains("URL: https://test.com"));
        assert!(!formatted.contains("## User Actions"));
        assert!(!formatted.contains("## Console"));
        assert!(!formatted.contains("## Network"));
        assert!(!formatted.contains("## DOM Changes"));
    }

    #[test]
    fn test_format_timestamp() {
        assert_eq!(format_timestamp(0.0), "00:00:00");
        assert_eq!(format_timestamp(45245000.0), "12:34:05");
        assert_eq!(format_timestamp(3661000.0), "01:01:01");
    }

    #[test]
    fn test_format_action_navigation() {
        let action = RecorderEvent::Navigation {
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            timestamp: 1000.0,
        };
        let formatted = format_action(&action);
        assert!(formatted.contains("navigate →"));
        assert!(formatted.contains("https://example.com"));
    }

    #[test]
    fn test_format_action_click_with_text() {
        let action = RecorderEvent::Click {
            selector: "#btn".to_string(),
            text: "OK".to_string(),
            x: 10.0,
            y: 20.0,
            timestamp: 1000.0,
        };
        let formatted = format_action(&action);
        assert!(formatted.contains("click →"));
        assert!(formatted.contains("#btn \"OK\""));
        assert!(formatted.contains("(x:10, y:20)"));
    }

    #[test]
    fn test_format_action_click_no_text() {
        let action = RecorderEvent::Click {
            selector: "#btn".to_string(),
            text: String::new(),
            x: 10.0,
            y: 20.0,
            timestamp: 1000.0,
        };
        let formatted = format_action(&action);
        assert!(formatted.contains("click → #btn (x:10, y:20)"));
    }

    #[test]
    fn test_format_action_submit() {
        let action = RecorderEvent::Submit {
            selector: "form#login".to_string(),
            method: "POST".to_string(),
            action: "/api/login".to_string(),
            timestamp: 1000.0,
        };
        let formatted = format_action(&action);
        assert!(formatted.contains("submit →"));
        assert!(formatted.contains("POST"));
        assert!(formatted.contains("/api/login"));
    }

    #[test]
    fn test_format_action_keypress_with_modifiers() {
        let action = RecorderEvent::Keypress {
            key: "Enter".to_string(),
            modifiers: vec!["Ctrl".to_string(), "Shift".to_string()],
            timestamp: 1000.0,
        };
        let formatted = format_action(&action);
        assert!(formatted.contains("keypress → Ctrl+Shift+Enter"));
    }

    #[test]
    fn test_format_action_scroll() {
        let action = RecorderEvent::Scroll {
            scroll_x: 0.0,
            scroll_y: 500.0,
            timestamp: 1000.0,
        };
        let formatted = format_action(&action);
        assert!(formatted.contains("scroll → (0, 500)"));
    }

    #[test]
    fn test_format_console_entry() {
        let entry = ConsoleEntry {
            timestamp: 1000.0,
            level: "error".to_string(),
            text: "Something failed".to_string(),
        };
        let formatted = format_console_entry(&entry);
        assert!(formatted.contains("[error]"));
        assert!(formatted.contains("Something failed"));
    }

    #[test]
    fn test_format_network_entry_with_status() {
        let entry = NetworkEntry {
            timestamp: 1000.0,
            method: "GET".to_string(),
            url: "https://api.com/data".to_string(),
            resource_type: "Fetch".to_string(),
            status: Some(200),
            ..NetworkEntry::default()
        };
        let formatted = format_network_entry(&entry);
        assert!(formatted.contains("GET https://api.com/data → 200"));
    }

    #[test]
    fn test_format_network_entry_no_status() {
        let entry = NetworkEntry {
            timestamp: 1000.0,
            method: "POST".to_string(),
            url: "https://api.com/data".to_string(),
            resource_type: "XHR".to_string(),
            status: None,
            ..NetworkEntry::default()
        };
        let formatted = format_network_entry(&entry);
        assert!(formatted.contains("POST https://api.com/data"));
        assert!(!formatted.contains("→"));
    }

    #[test]
    fn test_compute_context_size_empty() {
        let size = compute_context_size(&[], &[], &[], &[]);
        assert_eq!(size, 8);
    }

    #[test]
    fn test_compute_context_size_with_data() {
        let actions = vec![RecorderEvent::Click {
            selector: "#btn".to_string(),
            text: "OK".to_string(),
            x: 10.0,
            y: 20.0,
            timestamp: 1000.0,
        }];
        let size = compute_context_size(&actions, &[], &[], &[]);
        assert!(size > 8);
    }

    #[test]
    fn test_make_context_message_role_and_content() {
        let snapshot = BrowserContextSnapshot {
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            actions: vec![RecorderEvent::Navigation {
                url: "https://example.com".to_string(),
                title: "Example".to_string(),
                timestamp: 1000.0,
            }],
            console: vec![],
            network: vec![],
            mutations: vec![],
            total_bytes: 50,
            page_changed: false,
        };

        let messages = make_context_messages(&snapshot, None);
        let msg = messages.event;
        assert!(messages.screenshot.is_none());
        assert_eq!(msg.role, "event");
        let event = msg.extra.get("event").unwrap();
        assert_eq!(event["subkind"], "system_notice");
        assert_eq!(event["source"], "chat.browser_context");
        match &msg.content {
            ChatContent::SimpleText(text) => {
                assert!(text.contains("[Browser Context]"));
                assert!(text.contains("https://example.com"));
            }
            _ => panic!("Expected SimpleText"),
        }
    }

    #[test]
    fn navigation_only_snapshot_produces_context_message() {
        let snapshot = empty_snapshot(true);
        assert!(should_emit_context(&snapshot));
        let messages = make_context_messages(&snapshot, None);

        assert_eq!(messages.event.role, "event");
        assert_eq!(
            messages.event.extra["event"]["payload"]["page_changed"],
            true
        );
        assert!(messages
            .event
            .content
            .content_text_only()
            .contains("URL: https://example.com"));
    }

    #[test]
    fn toggle_off_does_not_capture_and_produces_text_only_context() {
        let calls = std::cell::Cell::new(0);
        let screenshot =
            capture_screenshot_message_with(false, true, &ImagePolicy::browser_capture(), &|| {
                calls.set(calls.get() + 1);
                Ok((String::new(), "image/png".to_string()))
            });
        let messages = make_context_messages(&empty_snapshot(true), None);

        assert_eq!(calls.get(), 0);
        assert!(screenshot.is_none());
        assert!(matches!(messages.event.content, ChatContent::SimpleText(_)));
        assert!(messages.screenshot.is_none());
    }

    #[test]
    fn unchanged_page_does_not_capture_screenshot() {
        let calls = std::cell::Cell::new(0);

        let screenshot =
            capture_screenshot_message_with(true, false, &ImagePolicy::browser_capture(), &|| {
                calls.set(calls.get() + 1);
                Ok((String::new(), "image/png".to_string()))
            });

        assert_eq!(calls.get(), 0);
        assert!(screenshot.is_none());
    }

    #[test]
    fn enabled_page_change_uses_mock_capture() {
        let calls = std::cell::Cell::new(0);
        let encoded = base64::prelude::BASE64_STANDARD.encode(tiny_png());

        let screenshot = capture_screenshot_message_with(
            true,
            true,
            &ImagePolicy::default().with_format(ImageFormat::Png, None),
            &|| {
                calls.set(calls.get() + 1);
                Ok((encoded.clone(), "image/png".to_string()))
            },
        );

        assert_eq!(calls.get(), 1);
        assert!(
            screenshot.is_some_and(|message| matches!(message.content, ChatContent::Multimodal(_)))
        );
    }

    #[test]
    fn unchanged_empty_snapshot_is_suppressed() {
        assert!(!should_emit_context(&empty_snapshot(false)));
    }

    #[test]
    fn screenshot_message_resizes_and_uses_capture_format() {
        let encoded = base64::prelude::BASE64_STANDARD.encode(tiny_png());
        let policy = ImagePolicy::default().with_format(ImageFormat::Webp, Some(80));

        let message = screenshot_message((encoded, "image/png".to_string()), &policy).unwrap();

        assert_eq!(message.role, "user");
        let ChatContent::Multimodal(elements) = message.content else {
            panic!("expected multimodal screenshot");
        };
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].m_type, "image/webp");
        let bytes = base64::prelude::BASE64_STANDARD
            .decode(&elements[0].m_content)
            .unwrap();
        assert!(image::load_from_memory(&bytes).is_ok());
    }

    #[test]
    fn screenshot_survives_provider_bound_linearize() {
        use refact_llm::adapter::{AdapterSettings, LlmWireAdapter};
        use refact_llm::adapters::openai_chat::OpenAiChatAdapter;
        use refact_llm::canonical::LlmRequest;

        let encoded = base64::prelude::BASE64_STANDARD.encode(tiny_png());
        let screenshot = screenshot_message(
            (encoded, "image/png".to_string()),
            &ImagePolicy::default().with_format(ImageFormat::Png, None),
        )
        .unwrap();
        let context = make_context_messages(&empty_snapshot(true), Some(screenshot));
        let linearized = crate::chat::linearize::apply_summarization_linearize(vec![
            context.event,
            context.screenshot.unwrap(),
            ChatMessage::new("user".to_string(), "inspect this page".to_string()),
        ])
        .unwrap();
        let request = LlmRequest::new("gpt-4.1".to_string(), linearized);
        let settings = AdapterSettings {
            api_key: String::new(),
            auth_token: String::new(),
            endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
            extra_headers: Default::default(),
            model_name: "gpt-4.1".to_string(),
            supports_tools: true,
            supports_reasoning: false,
            reasoning_type: None,
            supports_temperature: true,
            supports_max_completion_tokens: false,
            eof_is_done: false,
            supports_web_search: false,
            supports_cache_control: false,
        };

        let body = OpenAiChatAdapter
            .build_http(&refact_privacy::testing::cleared(request), &settings)
            .unwrap()
            .body;
        let serialized = body["messages"].to_string();

        assert!(serialized.contains("<event subkind=\\\"system_notice\\\""));
        assert!(serialized.contains("data:image/png;base64,"));
    }

    #[test]
    fn screenshot_survives_anthropic_wire_lowering() {
        use refact_llm::adapter::{AdapterSettings, LlmWireAdapter};
        use refact_llm::adapters::anthropic::AnthropicAdapter;
        use refact_llm::canonical::LlmRequest;

        let encoded = base64::prelude::BASE64_STANDARD.encode(tiny_png());
        let screenshot = screenshot_message(
            (encoded, "image/png".to_string()),
            &ImagePolicy::default().with_format(ImageFormat::Png, None),
        )
        .unwrap();
        let context = make_context_messages(&empty_snapshot(true), Some(screenshot));
        let linearized = crate::chat::linearize::apply_summarization_linearize(vec![
            context.event,
            context.screenshot.unwrap(),
            ChatMessage::new("user".to_string(), "inspect this page".to_string()),
        ])
        .unwrap();
        let request = LlmRequest::new("claude-sonnet-4-5".to_string(), linearized);
        let settings = AdapterSettings {
            api_key: String::new(),
            auth_token: String::new(),
            endpoint: "https://api.anthropic.com/v1/messages".to_string(),
            extra_headers: Default::default(),
            model_name: "claude-sonnet-4-5".to_string(),
            supports_tools: true,
            supports_reasoning: false,
            reasoning_type: None,
            supports_temperature: true,
            supports_max_completion_tokens: false,
            eof_is_done: false,
            supports_web_search: false,
            supports_cache_control: false,
        };

        let body = AnthropicAdapter
            .build_http(&refact_privacy::testing::cleared(request), &settings)
            .unwrap()
            .body;
        let serialized = body["messages"].to_string();

        assert!(serialized.contains("<event subkind=\\\"system_notice\\\""));
        assert!(serialized.contains("\"type\":\"image\""));
        assert!(serialized.contains("\"media_type\":\"image/png\""));
    }

    #[test]
    fn test_oversize_detection() {
        let large_text = "x".repeat(200);
        let actions: Vec<RecorderEvent> = (0..1000)
            .map(|i| RecorderEvent::Input {
                selector: format!("#field-{}", i),
                value: large_text.clone(),
                masked: false,
                timestamp: i as f64 * 1000.0,
                tag: None,
                input_type: None,
                field_name: None,
                placeholder: None,
                aria_label: None,
                role: None,
            })
            .collect();

        let size = compute_context_size(&actions, &[], &[], &[]);
        assert!(size > OVERSIZE_THRESHOLD);
    }

    #[test]
    fn test_format_browser_context_sections_present() {
        let snapshot = BrowserContextSnapshot {
            url: "https://example.com".to_string(),
            title: "Test".to_string(),
            actions: vec![RecorderEvent::Navigation {
                url: "https://example.com".to_string(),
                title: "Test".to_string(),
                timestamp: 1000.0,
            }],
            console: vec![ConsoleEntry {
                timestamp: 1000.0,
                level: "warn".to_string(),
                text: "test log".to_string(),
            }],
            network: vec![NetworkEntry {
                timestamp: 1000.0,
                method: "GET".to_string(),
                url: "https://example.com".to_string(),
                resource_type: "Document".to_string(),
                status: Some(200),
                ..NetworkEntry::default()
            }],
            mutations: vec![MutationSummaryEntry {
                timestamp: 1000.0,
                added: 1,
                removed: 0,
                changed: 2,
                descriptions: vec![],
            }],
            total_bytes: 100,
            page_changed: false,
        };
        let out = format_browser_context(&snapshot);
        assert!(out.contains("[Browser Context]"));
        assert!(out.contains("## User Actions (since last message)"));
        assert!(out.contains("navigate →"));
        assert!(out.contains("## Console (since last message)"));
        assert!(out.contains("[warn] test log"));
        assert!(out.contains("## Network (since last message)"));
        assert!(out.contains("GET https://example.com → 200"));
        assert!(out.contains("## DOM Changes (since last message)"));
        assert!(out.contains("Added: 1 elements"));
        assert!(out.contains("Changed: 2 elements"));
        assert!(!out.contains("Removed:"));
    }
}
