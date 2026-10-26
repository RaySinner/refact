use super::*;

use super::context::{call_handle_json, eval_js_ok, eval_js_value, resolve_scoped_element};
use super::expect_poll::{js_exception_message};
use super::single_step::{execute_locator_handler, wait_for_load_state};


pub(crate) const PDF_INLINE_LIMIT_BYTES: usize = 256 * 1024;

pub(crate) fn step_pdf(
    tab: &Tab,
    idx: usize,
    options: &BrowserPdfOptions,
    artifacts_dir: &Path,
) -> StepResult {
    let result = (|| -> Result<(PathBuf, Vec<u8>), String> {
        let payload = pdf_payload(options)?;
        let response = tab
            .call_method(payload)
            .map_err(|error| format!("PDF generation failed: {error}"))?;
        let bytes = if let Some(stream) = response.stream {
            read_cdp_stream(tab, stream)?
        } else {
            base64::prelude::BASE64_STANDARD
                .decode(response.data)
                .map_err(|error| format!("PDF decode failed: {error}"))?
        };
        std::fs::create_dir_all(artifacts_dir).map_err(|error| {
            format!(
                "Failed to create browser artifacts directory {}: {error}",
                artifacts_dir.display()
            )
        })?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = artifacts_dir.join(format!("page-{nonce}-{idx}.pdf"));
        std::fs::write(&path, &bytes)
            .map_err(|error| format!("Failed to save PDF artifact {}: {error}", path.display()))?;
        Ok((path, bytes))
    })();
    match result {
        Ok((path, bytes)) => {
            let inline = (bytes.len() <= PDF_INLINE_LIMIT_BYTES)
                .then(|| base64::prelude::BASE64_STANDARD.encode(&bytes));
            StepResult::success(idx, format!("PDF saved to {}", path.display())).with_data(
                serde_json::json!({
                    "artifact": {
                        "kind": "pdf",
                        "mime": "application/pdf",
                        "path": path,
                        "bytes": bytes.len(),
                        "data": inline,
                    }
                }),
            )
        }
        Err(error) => StepResult::failure(idx, "PDF generation failed", error),
    }
}

pub(crate) fn read_cdp_stream(tab: &Tab, stream: IO::StreamHandle) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    loop {
        let chunk = tab
            .call_method(IO::Read {
                handle: stream.clone(),
                offset: None,
                size: Some(64 * 1024),
            })
            .map_err(|error| format!("PDF stream read failed: {error}"))?;
        if chunk.base_64_encoded.unwrap_or(false) {
            bytes.extend(
                base64::prelude::BASE64_STANDARD
                    .decode(chunk.data)
                    .map_err(|error| format!("PDF stream decode failed: {error}"))?,
            );
        } else {
            bytes.extend_from_slice(chunk.data.as_bytes());
        }
        if chunk.eof {
            break;
        }
    }
    tab.call_method(IO::Close { handle: stream })
        .map_err(|error| format!("PDF stream close failed: {error}"))?;
    Ok(bytes)
}

pub(crate) fn eval_invocation_target(
    object_type: &Runtime::RemoteObjectType,
    object_id: Option<&String>,
) -> Option<String> {
    matches!(object_type, Runtime::RemoteObjectType::Function)
        .then(|| object_id.cloned())
        .flatten()
}

pub(crate) fn invoke_eval_function(tab: &Tab, object_id: String) -> Result<Runtime::RemoteObject, String> {
    let invoked = tab
        .call_method(Runtime::CallFunctionOn {
            function_declaration: "function() { return this(); }".to_string(),
            object_id: Some(object_id),
            arguments: None,
            silent: None,
            return_by_value: Some(false),
            generate_preview: Some(true),
            user_gesture: Some(false),
            await_promise: Some(true),
            execution_context_id: None,
            object_group: None,
            throw_on_side_effect: None,
            unique_context_id: None,
            serialization_options: None,
        })
        .map_err(|error| error.to_string())?;
    if let Some(exception) = invoked.exception_details {
        return Err(js_exception_message(&exception));
    }
    Ok(invoked.result)
}

pub(crate) const EVAL_DEFAULT_TIMEOUT_MS: u64 = 30_000;

pub(crate) fn step_eval(tab: &Tab, idx: usize, expression: &str, timeout_ms: Option<u64>) -> StepResult {
    let timeout_ms = timeout_ms.unwrap_or(EVAL_DEFAULT_TIMEOUT_MS);
    let evaluated = match tab.call_method(Runtime::Evaluate {
        expression: expression.to_string(),
        return_by_value: Some(false),
        generate_preview: Some(true),
        silent: Some(false),
        await_promise: Some(true),
        include_command_line_api: Some(false),
        user_gesture: Some(true),
        object_group: None,
        context_id: None,
        throw_on_side_effect: None,
        timeout: Some(timeout_ms as f64),
        disable_breaks: None,
        repl_mode: None,
        allow_unsafe_eval_blocked_by_csp: None,
        unique_context_id: None,
        serialization_options: None,
    }) {
        Ok(evaluated) => evaluated,
        Err(error) => {
            let message = error.to_string();
            let summary = if message.contains("timed out") || message.contains("Timeout") {
                format!("Eval timed out after {timeout_ms}ms")
            } else {
                "Eval failed".to_string()
            };
            return StepResult::failure(idx, summary, message);
        }
    };
    if let Some(exception) = evaluated.exception_details {
        return StepResult::failure(
            idx,
            "Eval threw an exception",
            js_exception_message(&exception),
        );
    }
    let remote =
        match eval_invocation_target(&evaluated.result.Type, evaluated.result.object_id.as_ref()) {
            Some(object_id) => match invoke_eval_function(tab, object_id) {
                Ok(remote) => remote,
                Err(error) => return StepResult::failure(idx, "Eval threw an exception", error),
            },
            None => evaluated.result,
        };
    let (value, description) = eval_remote_object_value(tab, remote);
    StepResult::success(idx, "Eval completed".to_string())
        .with_data(serde_json::json!({"value": value, "description": description}))
}

pub(crate) fn eval_remote_object_value(tab: &Tab, remote: Runtime::RemoteObject) -> (Value, String) {
    let description = remote
        .description
        .clone()
        .unwrap_or_else(|| format!("{:?}", remote.Type).to_ascii_lowercase());
    if let Some(value) = remote.value {
        return (value, description);
    }
    let Some(object_id) = remote.object_id else {
        return (Value::Null, description);
    };
    let serialized = tab
        .call_method(Runtime::CallFunctionOn {
            function_declaration: EVAL_SERIALIZE_JS.to_string(),
            object_id: Some(object_id),
            arguments: None,
            silent: Some(true),
            return_by_value: Some(true),
            generate_preview: None,
            user_gesture: Some(false),
            await_promise: Some(false),
            execution_context_id: None,
            object_group: None,
            throw_on_side_effect: None,
            unique_context_id: None,
            serialization_options: None,
        })
        .ok()
        .filter(|invoked| invoked.exception_details.is_none())
        .and_then(|invoked| invoked.result.value);
    match serialized {
        Some(value) => (value, description),
        None => (Value::Null, description),
    }
}

pub(crate) const EVAL_SERIALIZE_JS: &str = r#"function() {
  var seen = [];
  var replacer = function(key, value) {
    if (typeof value === 'bigint') return value.toString() + 'n';
    if (typeof value === 'function') return '[Function ' + (value.name || 'anonymous') + ']';
    if (typeof value === 'symbol') return value.toString();
    if (value && typeof value === 'object') {
      if (typeof Node !== 'undefined' && value instanceof Node) {
        return value.nodeType === 1 ? '<' + value.tagName.toLowerCase() + '>' : '[' + value.nodeName + ']';
      }
      if (value instanceof Map) return Object.fromEntries(value);
      if (value instanceof Set) return Array.from(value);
      if (value instanceof Error) return { name: value.name, message: value.message, stack: value.stack };
      if (seen.indexOf(value) !== -1) return '[Circular]';
      seen.push(value);
    }
    return value;
  };
  return JSON.parse(JSON.stringify(this, replacer));
}"#;

pub(crate) fn eval_js_awaited(tab: &Tab, expression: &str) -> Result<serde_json::Value, String> {
    let result = tab
        .call_method(Runtime::Evaluate {
            expression: expression.to_string(),
            return_by_value: Some(true),
            generate_preview: None,
            silent: Some(false),
            await_promise: Some(true),
            include_command_line_api: Some(false),
            user_gesture: Some(true),
            object_group: None,
            context_id: None,
            throw_on_side_effect: None,
            timeout: None,
            disable_breaks: None,
            repl_mode: None,
            allow_unsafe_eval_blocked_by_csp: None,
            unique_context_id: None,
            serialization_options: None,
        })
        .map_err(|error| format!("JS evaluation failed: {error}"))?;
    if let Some(exception) = result.exception_details {
        return Err(exception
            .exception
            .as_ref()
            .and_then(|value| value.description.clone())
            .unwrap_or(exception.text));
    }
    Ok(result.result.value.unwrap_or(serde_json::Value::Null))
}

pub(crate) const PAGE_CONTENT_JS: &str = r#"(function() {
  var out = '';
  if (document.doctype) out = new XMLSerializer().serializeToString(document.doctype);
  if (document.documentElement) out += document.documentElement.outerHTML;
  return out;
})()"#;

pub(crate) fn step_set_content(
    tab: &Tab,
    world: &WorldManager,
    network_monitor: &NetworkMonitorHandle,
    idx: usize,
    html: &str,
    wait_until: Option<BrowserLoadState>,
) -> StepResult {
    let settings = browser_settings::current();
    let frame_id = match tab.call_method(Page::GetFrameTree(None)) {
        Ok(response) => response.frame_tree.frame.id,
        Err(error) => {
            return StepResult::failure(
                idx,
                "Set content",
                format!("Failed to read browser frame tree: {error}"),
            );
        }
    };
    if let Err(error) = tab.call_method(Page::SetDocumentContent {
        frame_id,
        html: html.to_string(),
    }) {
        return StepResult::failure(
            idx,
            "Set content",
            format!("Failed to set document content: {error}"),
        );
    }
    let _ = world.release_all(tab);
    let state = wait_until.unwrap_or(BrowserLoadState::Load);
    let wait = wait_for_load_state(
        network_monitor,
        idx,
        state,
        settings.timing.default_wait_timeout_ms,
    );
    let summary = format!("Set page content ({} bytes)", html.len());
    if wait.ok {
        StepResult::success(idx, summary)
    } else {
        StepResult::failure(
            idx,
            summary,
            format!(
                "Content was set but {} failed: {}",
                wait.summary,
                wait.error.as_deref().unwrap_or(&wait.summary)
            ),
        )
    }
}

pub(crate) fn step_page_content(tab: &Tab, idx: usize, artifacts_dir: &Path) -> StepResult {
    let html = match eval_js_value(tab, PAGE_CONTENT_JS) {
        Ok(value) => value.as_str().unwrap_or_default().to_string(),
        Err(error) => return StepResult::failure(idx, "Page content", error),
    };
    let bytes = html.len();
    if bytes <= http_client::HTTP_INLINE_BODY_LIMIT_BYTES {
        return StepResult::success(idx, format!("Read page content ({bytes} bytes)"))
            .with_data(serde_json::json!({"html": html, "bytes": bytes}));
    }
    match save_page_content_artifact(artifacts_dir, idx, &html) {
        Ok(path) => StepResult::success(
            idx,
            format!(
                "Read page content ({bytes} bytes) saved to {}",
                path.display()
            ),
        )
        .with_data(serde_json::json!({
            "bytes": bytes,
            "artifact": {
                "kind": "page_content",
                "mime": "text/html",
                "path": path,
                "bytes": bytes,
            }
        })),
        Err(error) => StepResult::failure(idx, "Page content", error),
    }
}

pub(crate) fn save_page_content_artifact(
    artifacts_dir: &Path,
    idx: usize,
    html: &str,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(artifacts_dir).map_err(|error| {
        format!(
            "Failed to create browser artifacts directory {}: {error}",
            artifacts_dir.display()
        )
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = artifacts_dir.join(format!("content-{nonce}-{idx}.html"));
    std::fs::write(&path, html).map_err(|error| {
        format!(
            "Failed to save page content artifact {}: {error}",
            path.display()
        )
    })?;
    Ok(path)
}

pub(crate) fn tag_source(url: Option<&str>, content: Option<&str>, action: &str) -> Result<(), String> {
    match (url, content) {
        (Some(_), Some(_)) => Err(format!("{action} accepts either url or content, not both")),
        (None, None) => Err(format!("{action} requires either url or content")),
        _ => Ok(()),
    }
}

pub(crate) fn add_script_tag_js(
    url: Option<&str>,
    content: Option<&str>,
    script_type: Option<&str>,
) -> String {
    let type_js = match script_type {
        Some(value) => format!("element.type = {};", js_string_literal(value)),
        None => String::new(),
    };
    let body = match url {
        Some(url) => format!(
            r#"element.src = {url};
    element.addEventListener('load', function() {{ resolve(true); }});
    element.addEventListener('error', function() {{ reject(new Error('Failed to load script ' + {url})); }});
    (document.head || document.documentElement).appendChild(element);"#,
            url = js_string_literal(url),
        ),
        None => format!(
            r#"element.text = {content};
    (document.head || document.documentElement).appendChild(element);
    resolve(true);"#,
            content = js_string_literal(content.unwrap_or_default()),
        ),
    };
    format!(
        r#"(function() {{
  return new Promise(function(resolve, reject) {{
    var element = document.createElement('script');
    {type_js}
    {body}
  }});
}})()"#
    )
}

pub(crate) fn add_style_tag_js(url: Option<&str>, content: Option<&str>) -> String {
    let body = match url {
        Some(url) => format!(
            r#"var element = document.createElement('link');
    element.rel = 'stylesheet';
    element.href = {url};
    element.addEventListener('load', function() {{ resolve(true); }});
    element.addEventListener('error', function() {{ reject(new Error('Failed to load stylesheet ' + {url})); }});
    (document.head || document.documentElement).appendChild(element);"#,
            url = js_string_literal(url),
        ),
        None => format!(
            r#"var element = document.createElement('style');
    element.textContent = {content};
    (document.head || document.documentElement).appendChild(element);
    resolve(true);"#,
            content = js_string_literal(content.unwrap_or_default()),
        ),
    };
    format!(
        r#"(function() {{
  return new Promise(function(resolve, reject) {{
    {body}
  }});
}})()"#
    )
}

pub(crate) fn step_add_script_tag(
    tab: &Tab,
    idx: usize,
    url: Option<&str>,
    content: Option<&str>,
    script_type: Option<&str>,
) -> StepResult {
    if let Err(error) = tag_source(url, content, "add_script_tag") {
        return StepResult::failure(idx, "Add script tag", error);
    }
    match eval_js_awaited(tab, &add_script_tag_js(url, content, script_type)) {
        Ok(_) => StepResult::success(
            idx,
            match url {
                Some(url) => format!("Added script tag {url}"),
                None => "Added inline script tag".to_string(),
            },
        ),
        Err(error) => StepResult::failure(idx, "Add script tag", error),
    }
}

pub(crate) fn step_add_style_tag(
    tab: &Tab,
    idx: usize,
    url: Option<&str>,
    content: Option<&str>,
) -> StepResult {
    if let Err(error) = tag_source(url, content, "add_style_tag") {
        return StepResult::failure(idx, "Add style tag", error);
    }
    match eval_js_awaited(tab, &add_style_tag_js(url, content)) {
        Ok(_) => StepResult::success(
            idx,
            match url {
                Some(url) => format!("Added style tag {url}"),
                None => "Added inline style tag".to_string(),
            },
        ),
        Err(error) => StepResult::failure(idx, "Add style tag", error),
    }
}

pub(crate) fn event_constructor(event_type: &str, event_init: Option<&Value>) -> &'static str {
    match event_type {
        "auxclick" | "click" | "dblclick" | "mousedown" | "mouseenter" | "mouseleave"
        | "mousemove" | "mouseout" | "mouseover" | "mouseup" | "mousewheel" => "MouseEvent",
        "keydown" | "keyup" | "keypress" | "textInput" => "KeyboardEvent",
        "touchstart" | "touchmove" | "touchend" | "touchcancel" => "TouchEvent",
        "pointerover" | "pointerout" | "pointerenter" | "pointerleave" | "pointerdown"
        | "pointerup" | "pointermove" | "pointercancel" | "gotpointercapture"
        | "lostpointercapture" => "PointerEvent",
        "focus" | "blur" => "FocusEvent",
        "drag" | "dragstart" | "dragend" | "dragover" | "dragenter" | "dragleave" | "dragexit"
        | "drop" => "DragEvent",
        "wheel" => "WheelEvent",
        "deviceorientation" | "deviceorientationabsolute" => "DeviceOrientationEvent",
        "devicemotion" => "DeviceMotionEvent",
        _ => match event_init.and_then(|init| init.get("detail")) {
            Some(_) => "CustomEvent",
            None => "Event",
        },
    }
}

pub(crate) fn dispatch_event_js(event_type: &str, event_init: Option<&Value>) -> String {
    let constructor = event_constructor(event_type, event_init);
    let init = event_init.cloned().unwrap_or_else(|| serde_json::json!({}));
    format!(
        r#"function() {{
  var init = Object.assign({{bubbles: true, cancelable: true, composed: true}}, {init});
  this.dispatchEvent(new {constructor}({event_type}, init));
  return JSON.stringify({{dispatched: true}});
}}"#,
        event_type = js_string_literal(event_type),
    )
}

pub(crate) fn step_dispatch_event(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    event_type: &str,
    event_init: Option<&Value>,
) -> StepResult {
    let info = match resolve_scoped_element(tab, world, locator) {
        Ok(info) => info,
        Err(error) => return StepResult::failure(idx, "Dispatch event: resolution failed", error),
    };
    let js = dispatch_event_js(event_type, event_init);
    match call_handle_json(tab, world, &info.handle, &js) {
        Ok(_) => StepResult::success(idx, format!("Dispatched {event_type} on <{}>", info.tag))
            .with_data(serde_json::json!({
                "event_type": event_type,
                "constructor": event_constructor(event_type, event_init),
            })),
        Err(error) => StepResult::failure(idx, format!("Dispatch event '{event_type}'"), error),
    }
}

pub(crate) fn step_styles(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    property_filter: Option<&str>,
) -> StepResult {
    match resolve_scoped_element(tab, world, locator) {
        Ok(info) => {
            let filter_js = match property_filter {
                Some(f) if !f.is_empty() => format!(
                    ".filter(function(s) {{ return s.includes({}); }})",
                    js_string_literal(f),
                ),
                _ => String::new(),
            };
            let js = format!(
                r#"function() {{
  var el = this;
  if (!el) return JSON.stringify({{error: 'No resolved element'}});
  var cs = window.getComputedStyle(el);
  var props = [];
  for (var i = 0; i < cs.length; i++) {{
    props.push(cs[i] + ': ' + cs.getPropertyValue(cs[i]));
  }}
  props = props{filter};
  if (props.length > 50) props = props.slice(0, 50).concat(['... (' + (props.length - 50) + ' more)']);
  return JSON.stringify({{ok: true, styles: props}});
}}"#,
                filter = filter_js,
            );
            match call_handle_json(tab, world, &info.handle, &js) {
                Ok(result) => StepResult::success(idx, format!("Got styles for <{}>", info.tag))
                    .with_data(result),
                Err(e) => StepResult::failure(idx, "Styles failed", e),
            }
        }
        Err(e) => StepResult::failure(idx, "Styles: resolution failed", e),
    }
}

pub(crate) const TAB_LOG_MAX_ENTRIES: usize = 50;

pub(crate) fn tab_log_from_buffer(idx: usize, console: &[ConsoleEntry]) -> StepResult {
    let start = console.len().saturating_sub(TAB_LOG_MAX_ENTRIES);
    let entries = &console[start..];
    let summary = if entries.is_empty() {
        "Tab log: no console messages captured yet in this session".to_string()
    } else {
        format!(
            "Tab log: {} most recent console message(s) (session captured {})",
            entries.len(),
            console.len()
        )
    };
    StepResult::success(idx, summary).with_data(serde_json::json!({
        "entries": entries,
        "total_captured": console.len(),
        "truncated": start > 0,
    }))
}

pub(crate) fn step_tab_log(idx: usize) -> StepResult {
    StepResult::failure(
        idx,
        "Tab log",
        "tab_log needs the session console buffer, which is only available in a top-level batch; run it as a normal step rather than inside a locator handler",
    )
}

pub(crate) fn step_add_locator_handler(
    idx: usize,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    name: &str,
    locator: &BrowserLocator,
    action: &LocatorHandlerAction,
    times: Option<u32>,
    no_wait_after: bool,
) -> StepResult {
    let Some(handlers) = handlers else {
        return StepResult::failure(idx, "Add locator handler", "Handler registry unavailable");
    };
    let handler = match LocatorHandler::registered(
        name.to_string(),
        locator.clone(),
        action.clone(),
        times,
        no_wait_after,
    ) {
        Ok(Some(handler)) => handler,
        Ok(None) => {
            return StepResult::success(idx, format!("Locator handler '{name}' not registered"));
        }
        Err(error) => return StepResult::failure(idx, "Add locator handler", error),
    };
    match handlers.lock() {
        Ok(mut registry) => {
            registry.register(handler);
            StepResult::success(idx, format!("Registered locator handler '{name}'"))
        }
        Err(error) => StepResult::failure(
            idx,
            "Add locator handler",
            format!("Failed to lock locator handlers: {error}"),
        ),
    }
}

pub(crate) fn step_remove_locator_handler(
    idx: usize,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    name: &str,
) -> StepResult {
    let Some(handlers) = handlers else {
        return StepResult::failure(
            idx,
            "Remove locator handler",
            "Handler registry unavailable",
        );
    };
    match handlers.lock() {
        Ok(mut registry) => {
            let removed = registry.unregister(name);
            StepResult::success(
                idx,
                if removed {
                    format!("Removed locator handler '{name}'")
                } else {
                    format!("Locator handler '{name}' was not registered")
                },
            )
        }
        Err(error) => StepResult::failure(
            idx,
            "Remove locator handler",
            format!("Failed to lock locator handlers: {error}"),
        ),
    }
}

pub(crate) fn dismiss_overlays(tab: &Tab, aggressive: bool) -> Result<String, String> {
    match eval_js_ok(
        tab,
        &browser_locators::js_dismiss_overlays(false, aggressive),
    ) {
        Ok(result) => {
            let count = result.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
            Ok(format!("Dismissed {count} overlay(s)"))
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn step_dismiss_overlays(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    handlers: Option<&Arc<Mutex<LocatorHandlerRegistry>>>,
    firings: &mut Vec<LocatorHandlerFiring>,
    image_policy: &ImagePolicy,
    aggressive: bool,
) -> StepResult {
    let settings = browser_settings::current();
    let Some(handlers) = handlers else {
        return match dismiss_overlays(tab, aggressive) {
            Ok(outcome) => StepResult::success(idx, outcome),
            Err(error) => StepResult::failure(idx, "Dismiss overlays failed", error),
        };
    };
    let existing = match handlers.lock() {
        Ok(registry) => registry.get(DEFAULT_DISMISS_OVERLAYS_HANDLER),
        Err(error) => {
            return StepResult::failure(
                idx,
                "Dismiss overlays failed",
                format!("Failed to lock locator handlers: {error}"),
            )
        }
    };
    let handler = existing.unwrap_or_else(LocatorHandler::dismiss_overlays);
    let lease = {
        let mut registry = match handlers.lock() {
            Ok(registry) => registry,
            Err(error) => {
                return StepResult::failure(
                    idx,
                    "Dismiss overlays failed",
                    format!("Failed to lock locator handlers: {error}"),
                )
            }
        };
        if registry.get(DEFAULT_DISMISS_OVERLAYS_HANDLER).is_none() {
            registry.register(handler);
        }
        registry.begin(DEFAULT_DISMISS_OVERLAYS_HANDLER)
    };
    let Some(lease) = lease else {
        return StepResult::failure(idx, "Dismiss overlays failed", "Handler is already running");
    };
    let result = execute_locator_handler(
        tab,
        world,
        handlers,
        firings,
        image_policy,
        Instant::now() + Duration::from_millis(settings.timing.default_wait_timeout_ms),
        &lease,
        aggressive,
    );
    let (ok, outcome) = match &result {
        Ok(outcome) => (true, outcome.clone()),
        Err(error) => (false, error.clone()),
    };
    match handlers.lock() {
        Ok(mut registry) => firings.push(registry.finish(lease, ok, outcome)),
        Err(error) => {
            return StepResult::failure(
                idx,
                "Dismiss overlays failed",
                format!("Failed to lock locator handlers: {error}"),
            )
        }
    }
    match result {
        Ok(outcome) => StepResult::success(idx, outcome),
        Err(error) => StepResult::failure(idx, "Dismiss overlays failed", error),
    }
}

pub(crate) fn step_highlight(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    style: Option<&str>,
    label: Option<&str>,
    legacy: bool,
) -> StepResult {
    let info = match resolve_scoped_element(tab, world, locator) {
        Ok(info) => info,
        Err(error) => return StepResult::failure(idx, "Highlight: resolution failed", error),
    };
    if legacy {
        return match call_handle_json(
            tab,
            world,
            &info.handle,
            browser_locators::js_highlight_element(),
        ) {
            Ok(_) => StepResult::success(idx, format!("Highlighted <{}>", info.tag)),
            Err(error) => StepResult::failure(idx, "Highlight failed", error),
        };
    }
    let function = format!(
        r#"function() {{
  const el = this;
  if (!el) return JSON.stringify({{error:'No resolved element'}});
  window.__refactHideHighlights?.();
  const root = document.createElement('div');
  root.dataset.refactHighlight = 'true';
  const shadow = root.attachShadow({{mode:'closed'}});
  const rect = el.getBoundingClientRect();
  const frame = document.createElement('div');
  const base = {{position:'fixed',left:rect.x+'px',top:rect.y+'px',width:rect.width+'px',height:rect.height+'px',boxSizing:'border-box',outline:'3px solid #E7150D',outlineOffset:'2px',pointerEvents:'none',zIndex:'2147483647'}};
  Object.assign(frame.style, base);
  frame.style.cssText += {};
  shadow.appendChild(frame);
  const label = {};
  if (label) {{
    const tag = document.createElement('div');
    tag.textContent = label;
    Object.assign(tag.style, {{position:'fixed',left:rect.x+'px',top:Math.max(0,rect.y-24)+'px',padding:'3px 6px',font:'12px sans-serif',color:'white',background:'#E7150D',borderRadius:'3px',pointerEvents:'none',zIndex:'2147483647'}});
    shadow.appendChild(tag);
  }}
  document.documentElement.appendChild(root);
  window.__refactHideHighlights = () => {{ root.remove(); delete window.__refactHideHighlights; }};
  return JSON.stringify({{ok:true}});
}}"#,
        js_string_literal(style.unwrap_or("")),
        js_string_literal(label.unwrap_or("")),
    );
    match call_handle_json(tab, world, &info.handle, &function) {
        Ok(_) => StepResult::success(idx, format!("Highlighted <{}>", info.tag)),
        Err(error) => StepResult::failure(idx, "Highlight failed", error),
    }
}

pub(crate) fn step_hide_highlight(tab: &Tab, world: &WorldManager, idx: usize) -> StepResult {
    match world.eval_in_utility(tab, "window.__refactHideHighlights?.(); true") {
        Ok(_) => StepResult::success(idx, "Hidden browser highlight annotations"),
        Err(error) => StepResult::failure(idx, "Hide highlight failed", error),
    }
}

pub fn describe_locator(locator: &BrowserLocator) -> String {
    let mut description = match &locator.strategy {
        LocatorStrategy::Ref { value } => format!("ref={}", value),
        LocatorStrategy::Css { value } => format!("css={}", value),
        LocatorStrategy::Id { value } => format!("id={}", value),
        LocatorStrategy::Name { value } => format!("name={}", value),
        LocatorStrategy::TestId { value, .. } => format!("testid={}", value),
        LocatorStrategy::Placeholder { value, .. } => format!("placeholder={}", value),
        LocatorStrategy::AltText { value, .. } => format!("alt_text={}", value),
        LocatorStrategy::Title { value, .. } => format!("title={}", value),
        LocatorStrategy::Autocomplete { value } => format!("autocomplete={}", value),
        LocatorStrategy::Text { value, exact, .. } => {
            if *exact {
                format!("text=\"{}\"", value)
            } else {
                format!("text~\"{}\"", value)
            }
        }
        LocatorStrategy::Label { value, .. } => format!("label={}", value),
        LocatorStrategy::Role { role, name, .. } => match name {
            Some(n) => format!("role={}[{}]", role, n),
            None => format!("role={}", role),
        },
        LocatorStrategy::Xpath { value } => format!("xpath={}", value),
    };
    if let Some(inner) = &locator.locator {
        description = format!("{description}.locator({})", describe_locator(inner));
    }
    if locator.filter.is_some() {
        description.push_str(".filter(...)");
    }
    if let Some(other) = &locator.and {
        description = format!("{description}.and({})", describe_locator(other));
    }
    if let Some(other) = &locator.or {
        description = format!("{description}.or({})", describe_locator(other));
    }
    if locator.first == Some(true) {
        description.push_str(".first()");
    } else if locator.last == Some(true) {
        description.push_str(".last()");
    } else if let Some(index) = locator.nth {
        description.push_str(&format!(".nth({index})"));
    }
    for frame in locator.frames.iter().rev() {
        description = format!("frame({}).{description}", describe_locator(frame));
    }
    description
}
