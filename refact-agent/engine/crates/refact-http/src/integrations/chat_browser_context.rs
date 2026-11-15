use base64::Engine;
use headless_chrome::Tab;
use refact_core::image_policy::{resize_to_policy, ImageFormat, ImagePolicy};
use serde_json::json;

use refact_core::chat_types::{ChatContent, ChatMessage, MultimodalElement};
use refact_chat_api::internal_roles::{event, EventSubkind};
use refact_integrations::browser_types::{
    ConsoleEntry, MutationSummaryEntry, NetworkEntry, RecorderEvent,
};
use refact_runtime_api::chat_ctx::ChatCtx;

pub const OVERSIZE_THRESHOLD: usize = 100 * 1024;

pub struct BrowserContextSnapshot {
    pub url: String,
    pub title: String,
    pub actions: Vec<RecorderEvent>,
    pub console: Vec<ConsoleEntry>,
    pub network: Vec<NetworkEntry>,
    pub mutations: Vec<MutationSummaryEntry>,
    pub total_bytes: usize,
    pub page_changed: bool,
}

pub struct BrowserContextMessages {
    pub event: ChatMessage,
    pub screenshot: Option<ChatMessage>,
}

type ScreenshotCapture = dyn Fn(&Tab) -> Result<(String, String), String> + Send + Sync;

pub fn should_emit_context(snapshot: &BrowserContextSnapshot) -> bool {
    snapshot.page_changed
        || !snapshot.actions.is_empty()
        || !snapshot.console.is_empty()
        || !snapshot.network.is_empty()
        || !snapshot.mutations.is_empty()
}

pub fn format_browser_context(snapshot: &BrowserContextSnapshot) -> String {
    let mut out = format!(
        "[Browser Context]\nURL: {}\nTitle: {}",
        snapshot.url, snapshot.title
    );

    if !snapshot.actions.is_empty() {
        out.push_str("\n\n## User Actions (since last message)");
        for action in &snapshot.actions {
            out.push('\n');
            out.push_str(&format_action(action));
        }
    }

    if !snapshot.console.is_empty() {
        out.push_str("\n\n## Console (since last message)");
        for entry in &snapshot.console {
            out.push('\n');
            out.push_str(&format_console_entry(entry));
        }
    }

    if !snapshot.network.is_empty() {
        out.push_str("\n\n## Network (since last message)");
        for entry in &snapshot.network {
            out.push('\n');
            out.push_str(&format_network_entry(entry));
        }
    }

    if !snapshot.mutations.is_empty() {
        out.push_str("\n\n## DOM Changes (since last message)");
        let total_added: u32 = snapshot.mutations.iter().map(|m| m.added).sum();
        let total_removed: u32 = snapshot.mutations.iter().map(|m| m.removed).sum();
        let total_changed: u32 = snapshot.mutations.iter().map(|m| m.changed).sum();
        if total_added > 0 {
            out.push_str(&format!("\nAdded: {} elements", total_added));
        }
        if total_removed > 0 {
            out.push_str(&format!("\nRemoved: {} elements", total_removed));
        }
        if total_changed > 0 {
            out.push_str(&format!("\nChanged: {} elements", total_changed));
        }
    }

    out
}

pub fn format_timestamp(ts: f64) -> String {
    let total_secs = (ts / 1000.0) as u64;
    let hours = (total_secs / 3600) % 24;
    let minutes = (total_secs / 60) % 60;
    let seconds = total_secs % 60;
    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}

pub fn format_action(action: &RecorderEvent) -> String {
    match action {
        RecorderEvent::Navigation { url, timestamp, .. } => {
            format!("[{}] navigate → {}", format_timestamp(*timestamp), url)
        }
        RecorderEvent::Click {
            selector,
            text,
            x,
            y,
            timestamp,
        } => {
            let label = if text.is_empty() {
                selector.clone()
            } else {
                format!("{} \"{}\"", selector, text)
            };
            format!(
                "[{}] click → {} (x:{}, y:{})",
                format_timestamp(*timestamp),
                label,
                *x as i32,
                *y as i32
            )
        }
        RecorderEvent::Input {
            selector,
            value,
            timestamp,
            tag,
            input_type,
            placeholder,
            ..
        } => {
            let field_desc = match (
                tag.as_deref(),
                input_type.as_deref(),
                placeholder.as_deref(),
            ) {
                (Some(t), Some(it), Some(ph)) => {
                    format!("<{}[type={}] placeholder=\"{}\">", t, it, ph)
                }
                (Some(t), Some(it), None) => format!("<{}[type={}]>", t, it),
                (Some(t), None, Some(ph)) => format!("<{} placeholder=\"{}\">", t, ph),
                _ => selector.clone(),
            };
            format!(
                "[{}] input → {} \"{}\"",
                format_timestamp(*timestamp),
                field_desc,
                value
            )
        }
        RecorderEvent::Keypress {
            key,
            modifiers,
            timestamp,
        } => {
            let mods = if modifiers.is_empty() {
                String::new()
            } else {
                format!("{}+", modifiers.join("+"))
            };
            format!(
                "[{}] keypress → {}{}",
                format_timestamp(*timestamp),
                mods,
                key
            )
        }
        RecorderEvent::Submit {
            selector,
            method,
            action,
            timestamp,
        } => {
            format!(
                "[{}] submit → {} {} {}",
                format_timestamp(*timestamp),
                selector,
                method,
                action
            )
        }
        RecorderEvent::Scroll {
            scroll_x,
            scroll_y,
            timestamp,
        } => {
            format!(
                "[{}] scroll → ({}, {})",
                format_timestamp(*timestamp),
                *scroll_x as i32,
                *scroll_y as i32
            )
        }
        RecorderEvent::MutationSummary {
            added,
            removed,
            changed,
            timestamp,
        } => {
            format!(
                "[{}] dom-change → +{} -{} ~{}",
                format_timestamp(*timestamp),
                added,
                removed,
                changed
            )
        }
        RecorderEvent::ToolbarAction { action, timestamp } => {
            format!("[{}] toolbar → {}", format_timestamp(*timestamp), action)
        }
    }
}

pub fn format_console_entry(entry: &ConsoleEntry) -> String {
    format!(
        "[{}] [{}] {}",
        format_timestamp(entry.timestamp),
        entry.level,
        entry.text
    )
}

pub fn format_network_entry(entry: &NetworkEntry) -> String {
    let status_str = entry
        .status
        .map(|s| format!(" → {}", s))
        .unwrap_or_default();
    format!(
        "[{}] {} {}{}",
        format_timestamp(entry.timestamp),
        entry.method,
        entry.url,
        status_str
    )
}

pub fn compute_context_size(
    actions: &[RecorderEvent],
    console: &[ConsoleEntry],
    network: &[NetworkEntry],
    mutations: &[MutationSummaryEntry],
) -> usize {
    serde_json::to_string(actions).unwrap_or_default().len()
        + serde_json::to_string(console).unwrap_or_default().len()
        + serde_json::to_string(network).unwrap_or_default().len()
        + serde_json::to_string(mutations).unwrap_or_default().len()
}

pub async fn get_browser_context_for_chat(
    ctx: &ChatCtx,
    chat_id: &str,
) -> Option<BrowserContextSnapshot> {
    let (_, runtime_arc) = crate::integrations::browser_runtime::find_runtime_by_chat_id_for_ctx(
        ctx,
        chat_id,
    )
    .await?;
    let rt = runtime_arc.lock().await;

    if !rt.is_connected {
        return None;
    }

    let (url, title) = if let Some(tab) = rt.get_active_tab() {
        let url = tab.get_url();
        let title = tab.get_title().unwrap_or_default();
        (url, title)
    } else {
        (String::new(), String::new())
    };

    let actions = rt.action_buffer[rt.last_send_action_cursor..].to_vec();
    let console = rt.console_buffer[rt.last_send_console_cursor..].to_vec();
    let network = rt.network_buffer[rt.last_send_network_cursor..].to_vec();
    let mutations = rt.mutation_summary[rt.last_send_mutation_cursor..].to_vec();

    let total_bytes = compute_context_size(&actions, &console, &network, &mutations);

    let page_changed = rt.page_changed(&url);

    let snapshot = BrowserContextSnapshot {
        url,
        title,
        actions,
        console,
        network,
        mutations,
        total_bytes,
        page_changed,
    };
    if !should_emit_context(&snapshot) {
        return None;
    }

    Some(snapshot)
}

pub async fn commit_browser_cursors(ctx: &ChatCtx, chat_id: &str) {
    if let Some((_, runtime_arc)) =
        crate::integrations::browser_runtime::find_runtime_by_chat_id_for_ctx(ctx, chat_id).await
    {
        let mut rt = runtime_arc.lock().await;
        let url = rt.get_active_tab().map(|tab| tab.get_url());
        rt.commit_cursors(url.as_deref());
    }
}

async fn image_policy_for_chat(ctx: &ChatCtx, chat_id: &str) -> ImagePolicy {
    let model_id = {
        let sessions = ctx.chat.sessions.read().await;
        let session = sessions.get(chat_id).cloned();
        drop(sessions);
        let Some(session) = session else {
            return ImagePolicy::browser_capture();
        };
        let model_id = session.lock().await.thread.model.clone();
        model_id
    };
    let Ok(caps) = ctx.caps.try_load_caps_quickly(0).await else {
        return ImagePolicy::browser_capture();
    };
    refact_caps_core::model_resolve::resolve_chat_model(caps, &model_id)
        .map(|model| ImagePolicy::for_model(&model.base).with_format(ImageFormat::Webp, Some(80)))
        .unwrap_or_else(|_| ImagePolicy::browser_capture())
}

async fn capture_browser_screenshot_with(
    ctx: &ChatCtx,
    chat_id: &str,
    attach_screenshot: bool,
    page_changed: bool,
    capture: &ScreenshotCapture,
) -> Option<ChatMessage> {
    if !attach_screenshot || !page_changed {
        return None;
    }
    let (_, runtime_arc) =
        crate::integrations::browser_runtime::find_runtime_by_chat_id_for_ctx(ctx, chat_id).await?;
    let policy = image_policy_for_chat(ctx, chat_id).await;
    let rt = runtime_arc.lock().await;
    let tab = rt.get_active_tab()?;
    capture_screenshot_message_with(true, true, &policy, &|| capture(&tab))
}

pub async fn capture_browser_screenshot(
    ctx: &ChatCtx,
    chat_id: &str,
) -> Option<ChatMessage> {
    capture_browser_screenshot_with(
        ctx,
        chat_id,
        true,
        true,
        &crate::integrations::browser_controller::capture_viewport_screenshot_png,
    )
    .await
}

pub fn screenshot_message(
    capture: (String, String),
    policy: &ImagePolicy,
) -> Result<ChatMessage, String> {
    let (encoded, mime) = capture;
    let raw = base64::prelude::BASE64_STANDARD
        .decode(encoded)
        .map_err(|error| format!("Base64 decode failed: {error}"))?;
    let (resized, resized_mime) = resize_to_policy(&raw, &mime, policy)?;
    Ok(ChatMessage {
        role: "user".to_string(),
        content: ChatContent::Multimodal(vec![MultimodalElement::new(
            resized_mime,
            base64::prelude::BASE64_STANDARD.encode(resized),
        )?]),
        ..Default::default()
    })
}

pub fn capture_screenshot_message_with(
    attach_screenshot: bool,
    page_changed: bool,
    policy: &ImagePolicy,
    capture: &dyn Fn() -> Result<(String, String), String>,
) -> Option<ChatMessage> {
    if !attach_screenshot || !page_changed {
        return None;
    }
    screenshot_message(capture().ok()?, policy).ok()
}

pub async fn maybe_insert_browser_context(
    ctx: &ChatCtx,
    chat_id: &str,
    has_browser_meta: bool,
    attach_screenshot_on_send: bool,
) -> Option<(BrowserContextMessages, bool)> {
    if !has_browser_meta {
        return None;
    }

    let snapshot = get_browser_context_for_chat(ctx, chat_id).await?;

    if snapshot.total_bytes > OVERSIZE_THRESHOLD {
        return Some((make_context_messages(&snapshot, None), true));
    }

    let screenshot = capture_browser_screenshot_with(
        ctx,
        chat_id,
        attach_screenshot_on_send,
        snapshot.page_changed,
        &crate::integrations::browser_controller::capture_viewport_screenshot_png,
    )
    .await;

    commit_browser_cursors(ctx, chat_id).await;

    Some((make_context_messages(&snapshot, screenshot), false))
}

pub fn apply_decision_to_snapshot(
    snapshot: &mut BrowserContextSnapshot,
    include_actions: bool,
    include_console: bool,
    include_network: bool,
    include_mutations: bool,
    last_n_actions: Option<usize>,
    last_n_console: Option<usize>,
    last_n_network: Option<usize>,
) {
    if !include_actions {
        snapshot.actions.clear();
    } else if let Some(n) = last_n_actions {
        if snapshot.actions.len() > n {
            let start = snapshot.actions.len() - n;
            snapshot.actions = snapshot.actions[start..].to_vec();
        }
    }
    if !include_console {
        snapshot.console.clear();
    } else if let Some(n) = last_n_console {
        if snapshot.console.len() > n {
            let start = snapshot.console.len() - n;
            snapshot.console = snapshot.console[start..].to_vec();
        }
    }
    if !include_network {
        snapshot.network.clear();
    } else if let Some(n) = last_n_network {
        if snapshot.network.len() > n {
            let start = snapshot.network.len() - n;
            snapshot.network = snapshot.network[start..].to_vec();
        }
    }
    if !include_mutations {
        snapshot.mutations.clear();
    }
    snapshot.total_bytes = compute_context_size(
        &snapshot.actions,
        &snapshot.console,
        &snapshot.network,
        &snapshot.mutations,
    );
}

pub fn make_context_messages(
    snapshot: &BrowserContextSnapshot,
    screenshot: Option<ChatMessage>,
) -> BrowserContextMessages {
    let text = format_browser_context(snapshot);
    BrowserContextMessages {
        event: event(
            EventSubkind::SystemNotice,
            "chat.browser_context",
            json!({
                "url": snapshot.url,
                "title": snapshot.title,
                "actions": snapshot.actions.len(),
                "console": snapshot.console.len(),
                "network": snapshot.network.len(),
                "mutations": snapshot.mutations.len(),
                "total_bytes": snapshot.total_bytes,
                "page_changed": snapshot.page_changed,
            }),
            text,
        ),
        screenshot,
    }
}
