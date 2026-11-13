use std::sync::Arc;

use base64::Engine;
use refact_browser::{compute_frame_hash, ensure_injection_into_all_tabs};
use refact_chat_api::{BrowserTabInfo, ChatEvent, TimelineEntry};
use refact_core::image_policy::{resize_to_policy, ImageFormat, ImagePolicy};
use refact_integrations::browser_types::{ConsoleEntry, NetworkEntry, RecorderEvent};

use crate::global_context::GlobalContext;

const FRAME_MAX_SIDE: u32 = 1280;

fn format_ts(ts_ms: f64) -> String {
    let total_secs = (ts_ms / 1000.0) as u64;
    let hours = (total_secs / 3600) % 24;
    let minutes = (total_secs / 60) % 60;
    let seconds = total_secs % 60;
    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}

fn recorder_events_to_timeline(
    actions: &[RecorderEvent],
    console: &[ConsoleEntry],
    network: &[NetworkEntry],
) -> Vec<TimelineEntry> {
    let mut entries = Vec::new();

    for event in actions {
        let ts = format_ts(event.timestamp());
        let (entry_type, summary) = match event {
            RecorderEvent::Navigation { url, .. } => {
                ("navigation".to_string(), format!("navigate → {}", url))
            }
            RecorderEvent::Click {
                selector,
                text,
                x,
                y,
                ..
            } => {
                let label = if text.is_empty() {
                    selector.clone()
                } else {
                    format!("{} \"{}\"", selector, text)
                };
                (
                    "click".to_string(),
                    format!("click → {} (x:{}, y:{})", label, *x as i32, *y as i32),
                )
            }
            RecorderEvent::Input { selector, .. } => {
                ("input".to_string(), format!("input → {}", selector))
            }
            RecorderEvent::Keypress { key, modifiers, .. } => {
                let mods = if modifiers.is_empty() {
                    String::new()
                } else {
                    format!("{}+", modifiers.join("+"))
                };
                (
                    "keypress".to_string(),
                    format!("keypress → {}{}", mods, key),
                )
            }
            RecorderEvent::Submit {
                selector,
                method,
                action,
                ..
            } => (
                "submit".to_string(),
                format!("submit → {} {} {}", selector, method, action),
            ),
            RecorderEvent::Scroll {
                scroll_x, scroll_y, ..
            } => (
                "scroll".to_string(),
                format!("scroll → ({}, {})", *scroll_x as i32, *scroll_y as i32),
            ),
            RecorderEvent::MutationSummary {
                added,
                removed,
                changed,
                ..
            } => (
                "mutation".to_string(),
                format!("dom-change → +{} -{} ~{}", added, removed, changed),
            ),
            RecorderEvent::ToolbarAction { action, .. } => {
                ("toolbar".to_string(), format!("toolbar → {}", action))
            }
        };
        entries.push(TimelineEntry {
            timestamp: ts,
            source: "user".to_string(),
            entry_type,
            summary,
            details: None,
        });
    }

    for entry in console {
        entries.push(TimelineEntry {
            timestamp: format_ts(entry.timestamp),
            source: "agent".to_string(),
            entry_type: "console".to_string(),
            summary: format!("[{}] {}", entry.level, entry.text),
            details: None,
        });
    }

    for entry in network {
        entries.push(TimelineEntry {
            timestamp: format_ts(entry.timestamp),
            source: "agent".to_string(),
            entry_type: "network".to_string(),
            summary: format!(
                "{} {}{}",
                if entry.method.is_empty() {
                    "GET"
                } else {
                    &entry.method
                },
                entry.url,
                entry
                    .status
                    .map(|status| format!(" → {}", status))
                    .unwrap_or_default()
            ),
            details: None,
        });
    }

    entries
}

pub async fn browser_frame_emission_task(
    gcx: Arc<GlobalContext>,
    chat_id: String,
    runtime_id: String,
) {
    let sessions = gcx.chat_sessions.clone();
    let mut last_status_json: Option<String> = None;

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        if gcx.shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }

        let runtime_arc = {
            let browser_runtimes = gcx.browser_runtimes.clone();
            let browser_runtimes = browser_runtimes.lock().await;
            browser_runtimes.get(&runtime_id).cloned()
        };
        let runtime_arc = match runtime_arc {
            Some(arc) => arc,
            None => break,
        };

        let (
            toolbar_actions,
            timeline_entries,
            actions_len,
            console_len,
            network_len,
            mutation_len,
        ) = {
            let mut rt = runtime_arc.lock().await;
            rt.drain_raw_events();
            let toolbar_actions = rt.drain_toolbar_actions();
            let (new_actions, new_console, new_network) = rt.flush_timeline_events();
            let agent_actions = rt.drain_agent_actions();
            let mut timeline_entries =
                recorder_events_to_timeline(&new_actions, &new_console, &new_network);
            for aa in &agent_actions {
                timeline_entries.push(TimelineEntry {
                    timestamp: format_ts(aa.timestamp_ms),
                    source: "agent".to_string(),
                    entry_type: aa.action_type.clone(),
                    summary: aa.summary.clone(),
                    details: None,
                });
            }
            timeline_entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
            (
                toolbar_actions,
                timeline_entries,
                rt.action_buffer.len(),
                rt.console_buffer.len(),
                rt.network_buffer.len(),
                rt.mutation_summary.len(),
            )
        };

        {
            let rt = runtime_arc.lock().await;
            ensure_injection_into_all_tabs(&rt);
        }

        {
            let rt = runtime_arc.lock().await;
            let all_tabs: Vec<Arc<headless_chrome::Tab>> = rt
                .browser
                .get_tabs()
                .lock()
                .map(|tabs| tabs.iter().cloned().collect())
                .unwrap_or_default();
            for tab in all_tabs {
                let url = tab.get_url();
                if url.starts_with("chrome://") || url == "about:blank" {
                    let is_active = rt
                        .active_tab_target_id()
                        .map(|id| id == tab.get_target_id())
                        .unwrap_or(false);
                    if !is_active
                        && rt.recording_tab_target_id.as_deref() != Some(tab.get_target_id())
                    {
                        let _ = tab.close(false);
                    }
                }
            }
        }

        let tab_for_counts = {
            let rt = runtime_arc.lock().await;
            rt.get_active_tab()
        };
        if let Some(tab) = tab_for_counts {
            let js = format!(
                "window.__refact_toolbar_setCounts && window.__refact_toolbar_setCounts({{actions:{},console:{},network:{},mutations:{}}});",
                actions_len, console_len, network_len, mutation_len
            );
            let _ = tab.evaluate(&js, false);
        }

        if !toolbar_actions.is_empty() || !timeline_entries.is_empty() {
            let session_arc = {
                let sessions_locked = sessions.read().await;
                sessions_locked.get(&chat_id).cloned()
            };
            if let Some(session_arc) = session_arc {
                let mut session = session_arc.lock().await;
                for action in toolbar_actions {
                    session.emit(ChatEvent::BrowserToolbarAction { action });
                }
                if !timeline_entries.is_empty() {
                    session.emit(ChatEvent::BrowserTimeline {
                        events: timeline_entries,
                    });
                }
            }
        }

        let status_event = {
            let rt = runtime_arc.lock().await;
            let tab_infos = rt
                .list_tab_infos()
                .into_iter()
                .map(|t| BrowserTabInfo {
                    tab_id: t.id,
                    url: t.url,
                    title: t.title,
                })
                .collect::<Vec<_>>();
            let (url, title) = match rt.get_active_tab() {
                Some(tab) => (
                    Some(tab.get_url()).filter(|s| !s.is_empty()),
                    Some(tab.get_title().unwrap_or_default()).filter(|s| !s.is_empty()),
                ),
                None => (None, None),
            };
            ChatEvent::BrowserStatus {
                runtime_id: runtime_id.clone(),
                connected: rt.is_connected,
                active_tab: rt.active_tab_target_id().map(|s| s.to_string()),
                url,
                title,
                tabs: tab_infos,
            }
        };
        let status_json = serde_json::to_string(&status_event).ok();
        if status_json != last_status_json {
            last_status_json = status_json;
            let session_arc = {
                let sessions_locked = sessions.read().await;
                sessions_locked.get(&chat_id).cloned()
            };
            if let Some(session_arc) = session_arc {
                let mut session = session_arc.lock().await;
                session.emit(status_event);
            }
        }

        let tab = {
            let rt = runtime_arc.lock().await;
            rt.get_active_tab()
        };
        let tab = match tab {
            Some(t) => t,
            None => continue,
        };

        let screenshot_result =
            tab.call_method(headless_chrome::protocol::cdp::Page::CaptureScreenshot {
                format: Some(
                    headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Jpeg,
                ),
                clip: None,
                quality: Some(60),
                from_surface: Some(true),
                capture_beyond_viewport: Some(false),
                optimize_for_speed: Some(true),
            });

        let raw_data = match screenshot_result {
            Ok(r) => match base64::prelude::BASE64_STANDARD.decode(&r.data) {
                Ok(d) => d,
                Err(_) => continue,
            },
            Err(_) => continue,
        };

        let new_hash = compute_frame_hash(&raw_data);

        {
            let rt = runtime_arc.lock().await;
            if !rt.should_emit_frame(new_hash) {
                continue;
            }
        }

        let frame_policy = ImagePolicy {
            max_side: FRAME_MAX_SIDE,
            preferred_side: FRAME_MAX_SIDE,
            format: ImageFormat::Jpeg,
            quality: Some(60),
            ..ImagePolicy::default()
        };
        let resized = match resize_to_policy(&raw_data, "image/jpeg", &frame_policy) {
            Ok((resized, _)) => resized,
            Err(_) => continue,
        };
        let b64 = base64::prelude::BASE64_STANDARD.encode(&resized);
        let tab_id = tab.get_target_id().clone();

        {
            let mut rt = runtime_arc.lock().await;
            rt.update_frame_state(new_hash, resized);
        }

        let session_arc = {
            let sessions_locked = sessions.read().await;
            sessions_locked.get(&chat_id).cloned()
        };
        if let Some(session_arc) = session_arc {
            let mut session = session_arc.lock().await;
            session.emit(ChatEvent::BrowserFrame {
                tab_id,
                mime: "image/jpeg".to_string(),
                data: b64,
                diff_boxes: vec![],
                changed_text: None,
            });
        }
    }
}
