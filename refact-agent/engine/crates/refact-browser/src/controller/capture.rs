use super::*;

use super::context::{MAX_ALL_TEXTS, SNAPSHOT_SUMMARY_LINES, call_handle_json, eval_js_ok, eval_js_value, release_locator_handles, resolve_element, resolve_locator_handles, resolve_scoped_element};


pub(crate) fn step_get_text(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
) -> StepResult {
    match resolve_scoped_element(tab, world, locator) {
        Ok(info) => {
            match call_handle_json(tab, world, &info.handle, browser_locators::js_get_text()) {
                Ok(result) => {
                    let text = result.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    StepResult::success(idx, format!("Got text from <{}>", info.tag))
                        .with_data(serde_json::json!({"text": text}))
                }
                Err(e) => StepResult::failure(idx, "Get text failed", e),
            }
        }
        Err(e) => StepResult::failure(idx, "Get text: resolution failed", e),
    }
}

pub(crate) fn step_get_html(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
) -> StepResult {
    match resolve_scoped_element(tab, world, locator) {
        Ok(info) => {
            match call_handle_json(tab, world, &info.handle, browser_locators::js_get_html()) {
                Ok(result) => {
                    let html = result.get("html").and_then(|v| v.as_str()).unwrap_or("");
                    StepResult::success(idx, format!("Got HTML from <{}>", info.tag))
                        .with_data(serde_json::json!({"html": html}))
                }
                Err(e) => StepResult::failure(idx, "Get HTML failed", e),
            }
        }
        Err(e) => StepResult::failure(idx, "Get HTML: resolution failed", e),
    }
}

pub(crate) fn step_get_attribute(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    attribute: &str,
) -> StepResult {
    match resolve_scoped_element(tab, world, locator) {
        Ok(info) => {
            let js = browser_locators::js_get_attribute(attribute);
            match call_handle_json(tab, world, &info.handle, &js) {
                Ok(result) => {
                    let value = result
                        .get("value")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    StepResult::success(
                        idx,
                        format!("Got attribute '{}' from <{}>", attribute, info.tag),
                    )
                    .with_data(serde_json::json!({"attribute": attribute, "value": value}))
                }
                Err(e) => StepResult::failure(idx, format!("Get attribute '{}'", attribute), e),
            }
        }
        Err(e) => StepResult::failure(idx, "Get attribute: resolution failed", e),
    }
}

pub(crate) fn element_bounding_box(info: &ElementInfo) -> Value {
    match (&info.bbox, info.visible) {
        (Some(bbox), true) => serde_json::json!({
            "x": bbox.x,
            "y": bbox.y,
            "width": bbox.width,
            "height": bbox.height,
        }),
        _ => Value::Null,
    }
}

pub(crate) fn step_bounding_box(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
) -> StepResult {
    let resolved = match resolve_element(tab, world, locator) {
        Ok(resolved) => resolved,
        Err(error) => return StepResult::failure(idx, "Bounding box: resolution failed", error),
    };
    let bounding_box = element_bounding_box(&resolved.info);
    let _ = world.release_handle(tab, &resolved.handle);
    let summary = if bounding_box.is_null() {
        format!("<{}> is not visible and has no bounding box", resolved.tag)
    } else {
        format!("Got bounding box of <{}>", resolved.tag)
    };
    StepResult::success(idx, summary).with_data(serde_json::json!({"bounding_box": bounding_box}))
}

pub(crate) fn step_count(tab: &Tab, world: &WorldManager, idx: usize, locator: &BrowserLocator) -> StepResult {
    match resolve_locator_handles(tab, world, locator) {
        Ok(handles) => {
            let count = handles.len();
            release_locator_handles(tab, world, &handles);
            StepResult::success(
                idx,
                format!("Matched {count} element(s) ({})", describe_locator(locator)),
            )
            .with_data(serde_json::json!({"count": count}))
        }
        Err(error) => StepResult::failure(idx, "Count failed", error),
    }
}

pub(crate) fn step_input_value(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
) -> StepResult {
    let resolved = match resolve_element(tab, world, locator) {
        Ok(resolved) => resolved,
        Err(error) => return StepResult::failure(idx, "Input value: resolution failed", error),
    };
    let read = call_handle_json(
        tab,
        world,
        &resolved.handle,
        browser_locators::js_input_value(),
    );
    let _ = world.release_handle(tab, &resolved.handle);
    match read {
        Ok(result) => {
            let value = result.get("value").cloned().unwrap_or(Value::Null);
            StepResult::success(idx, format!("Got input value of <{}>", resolved.tag))
                .with_data(serde_json::json!({"value": value}))
        }
        Err(error) => StepResult::failure(idx, "Input value failed", error),
    }
}

pub(crate) fn all_texts_limit(limit: Option<usize>) -> usize {
    let settings = browser_settings::current();
    limit
        .unwrap_or(settings.capture.default_all_texts)
        .min(MAX_ALL_TEXTS)
}

pub(crate) fn step_all_texts(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    mode: BrowserTextMode,
    limit: Option<usize>,
) -> StepResult {
    let handles = match resolve_locator_handles(tab, world, locator) {
        Ok(handles) => handles,
        Err(error) => return StepResult::failure(idx, "All texts: resolution failed", error),
    };
    let total = handles.len();
    let mut texts = Vec::new();
    let mut failure = None;
    for handle in handles.iter().take(all_texts_limit(limit)) {
        match call_handle_json(tab, world, handle, browser_locators::js_element_text(mode)) {
            Ok(result) => texts.push(result.get("text").cloned().unwrap_or(Value::Null)),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    release_locator_handles(tab, world, &handles);
    match failure {
        Some(error) => StepResult::failure(idx, "All texts failed", error),
        None => StepResult::success(idx, format!("Read {} of {total} text(s)", texts.len()))
            .with_data(serde_json::json!({"texts": texts, "total": total})),
    }
}

pub(crate) fn step_element_state(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
) -> StepResult {
    let resolved = match resolve_element(tab, world, locator) {
        Ok(resolved) => resolved,
        Err(error) => return StepResult::failure(idx, "Element state: resolution failed", error),
    };
    let state = world.element_states(tab, &resolved.handle);
    let _ = world.release_handle(tab, &resolved.handle);
    match state {
        Ok(state) => StepResult::success(idx, format!("Read element state of <{}>", resolved.tag))
            .with_data(serde_json::json!({"state": state})),
        Err(error) => StepResult::failure(idx, "Element state failed", error.to_string()),
    }
}

pub(crate) fn step_extract_links(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: Option<&BrowserLocator>,
    limit: Option<usize>,
) -> StepResult {
    let settings = browser_settings::current();
    let effective_limit = limit.unwrap_or(50).min(settings.capture.max_extract_links);
    let js = browser_locators::js_extract_links(effective_limit);
    let result = match locator {
        Some(locator) => serde_json::to_value(locator)
            .map_err(|error| format!("Failed to serialize browser locator: {error}"))
            .and_then(|locator| {
                world.call_injected(
                    tab,
                    "extractLinks",
                    serde_json::json!([locator, effective_limit]),
                )
            }),
        None => eval_js_ok(tab, &js),
    };
    match result {
        Ok(result) => StepResult::success(idx, "Extracted links".to_string()).with_data(result),
        Err(e) => StepResult::failure(idx, "Extract links failed", e),
    }
}

pub(crate) fn truncate_table_rows(mut result: Value, limit: Option<usize>) -> Value {
    let settings = browser_settings::current();
    let effective_limit = limit
        .unwrap_or(settings.capture.max_extract_table_rows)
        .min(settings.capture.max_extract_table_rows);
    if let Some(rows) = result.get_mut("rows").and_then(Value::as_array_mut) {
        rows.truncate(effective_limit);
    }
    result
}

pub(crate) fn step_extract_table(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    limit: Option<usize>,
) -> StepResult {
    match resolve_scoped_element(tab, world, locator) {
        Ok(info) => match call_handle_json(
            tab,
            world,
            &info.handle,
            browser_locators::js_extract_table(),
        ) {
            Ok(result) => StepResult::success(idx, format!("Extracted table from <{}>", info.tag))
                .with_data(truncate_table_rows(result, limit)),
            Err(e) => StepResult::failure(idx, "Extract table failed", e),
        },
        Err(e) => StepResult::failure(idx, "Extract table: resolution failed", e),
    }
}

pub(crate) fn step_dom_snapshot(
    tab: &Tab,
    idx: usize,
    selector: &str,
    max_chars: Option<usize>,
) -> StepResult {
    let settings = browser_settings::current();
    let limit = max_chars
        .unwrap_or(5000)
        .min(settings.capture.max_dom_snapshot_chars);
    let js = format!(
        r#"(function() {{
  var el = document.querySelector({sel});
  if (!el) return JSON.stringify({{error: 'Selector not found'}});
  var full = el.outerHTML;
  var truncated = false;
  var html = full;
  if (html.length > {limit}) {{
    html = html.substring(0, {limit}) + '... (truncated)';
    truncated = true;
  }}
  return JSON.stringify({{ok: true, html: html, length: full.length, truncated: truncated, max_chars: {limit}}});
}})()"#,
        sel = js_string_literal(selector),
        limit = limit,
    );
    match eval_js_ok(tab, &js) {
        Ok(result) => {
            StepResult::success(idx, "DOM snapshot captured".to_string()).with_data(result)
        }
        Err(e) => StepResult::failure(idx, "DOM snapshot failed", e),
    }
}

pub(crate) fn step_accessibility_snapshot(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    options: &AccessibilitySnapshotOptions,
) -> StepResult {
    let settings = browser_settings::current();
    let root = match options.locator.as_ref() {
        Some(locator) => match resolve_element(tab, world, locator) {
            Ok(resolved) => Some(resolved.handle),
            Err(error) => {
                return StepResult::failure(
                    idx,
                    "Accessibility snapshot root resolution failed",
                    error,
                )
            }
        },
        None => None,
    };
    let snapshot_options = SnapshotOptions {
        mode: match options.mode {
            AccessibilitySnapshotMode::Ai => SnapshotMode::Ai,
            AccessibilitySnapshotMode::Default => SnapshotMode::Default,
        },
        refs: options.refs_enabled(),
        boxes: options.boxes,
        depth: options.depth,
        ..Default::default()
    };
    let snapshot = world.aria_snapshot(tab, root.clone(), snapshot_options);
    if let Some(root) = root {
        let _ = world.release_handle(tab, &root);
    }
    match snapshot {
        Ok(mut snapshot) => {
            let limit = options
                .max_chars
                .unwrap_or(settings.capture.default_aria_snapshot_chars)
                .min(settings.capture.max_aria_snapshot_chars);
            let frames = frames_for_snapshot(tab, &snapshot.yaml);
            snapshot.yaml = truncate_chars(snapshot.yaml, limit);
            let mut data = serde_json::to_value(snapshot)
                .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
            if !frames.is_empty() {
                if let Some(object) = data.as_object_mut() {
                    object.insert(
                        "frames".to_string(),
                        serde_json::to_value(&frames).unwrap_or_default(),
                    );
                    object.insert(
                        "frames_hint".to_string(),
                        Value::String(
                            "iframe contents are not part of this snapshot; act inside a frame with locator.frames (an outermost-first chain of iframe locators) or snapshot with a locator that targets the iframe"
                                .to_string(),
                        ),
                    );
                }
            }
            StepResult::success(idx, "Accessibility snapshot").with_data(data)
        }
        Err(error) => StepResult::failure(idx, "Accessibility snapshot failed", error.to_string()),
    }
}

pub(crate) fn truncate_chars(value: String, max_chars: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= max_chars {
        return value;
    }
    const SUFFIX: &str = "\n# ... (truncated)";
    let suffix_chars = SUFFIX.chars().count();
    if max_chars <= suffix_chars {
        return value.chars().take(max_chars).collect();
    }
    let prefix = value
        .chars()
        .take(max_chars - suffix_chars)
        .collect::<String>();
    let mut truncated = prefix
        .rsplit_once('\n')
        .map(|(complete, _)| complete.to_string())
        .unwrap_or(prefix);
    truncated.push_str(SUFFIX);
    truncated
}

pub(crate) fn capture_viewport_screenshot(
    tab: &Tab,
    policy: &ImagePolicy,
) -> Result<(String, String), String> {
    let options = BrowserScreenshotOptions {
        image_type: Some(match policy.format {
            ImageFormat::Png => BrowserScreenshotType::Png,
            ImageFormat::Jpeg => BrowserScreenshotType::Jpeg,
            ImageFormat::Webp => BrowserScreenshotType::Webp,
        }),
        quality: policy.quality,
        ..Default::default()
    };
    capture_screenshot(tab, &WorldManager::default(), &options, None, policy)
        .map(|capture| (capture.data, capture.mime))
}

pub(crate) fn report_screenshot_requested(
    attach_screenshot: Option<bool>,
    page_context: PageContextMode,
    has_screenshot_step: bool,
) -> bool {
    match attach_screenshot {
        Some(attach) => attach,
        None => match page_context {
            PageContextMode::None => false,
            PageContextMode::Snapshot => has_screenshot_step,
            PageContextMode::Screenshot | PageContextMode::Both => true,
        },
    }
}

pub(crate) fn report_snapshot_requested(page_context: PageContextMode, page_changed: bool) -> bool {
    page_context.includes_snapshot() && page_changed
}

pub(crate) fn console_counts(console: &[ConsoleEntry], page_errors: &[String]) -> BrowserConsoleCounts {
    let level_is = |entry: &ConsoleEntry, names: &[&str]| {
        names
            .iter()
            .any(|name| entry.level.eq_ignore_ascii_case(name))
    };
    BrowserConsoleCounts {
        errors: page_errors.len()
            + console
                .iter()
                .filter(|entry| level_is(entry, &["error", "assert"]))
                .count(),
        warnings: console
            .iter()
            .filter(|entry| level_is(entry, &["warning", "warn"]))
            .count(),
    }
}

pub(crate) fn notable_main_document_status(network: &[NetworkEntry], url: Option<&str>) -> Option<u16> {
    let status = network
        .iter()
        .filter(|entry| entry.resource_type.eq_ignore_ascii_case("document"))
        .filter(|entry| url.is_none_or(|url| entry.url == url))
        .filter_map(|entry| entry.status)
        .next_back()?;
    (!(200..300).contains(&status)).then_some(status)
}

pub(crate) fn snapshot_head(yaml: &str, max_lines: usize) -> String {
    yaml.lines().take(max_lines).collect::<Vec<_>>().join("\n")
}

pub(crate) fn snapshot_mentions_iframe(yaml: &str) -> bool {
    yaml.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("- iframe") || trimmed.starts_with("- frame")
    })
}

pub(crate) fn child_frame_summaries(tab: &Tab) -> Vec<BrowserFrameSummary> {
    fn walk(node: &Page::FrameTree, depth: usize, out: &mut Vec<BrowserFrameSummary>) {
        if depth > 0 {
            out.push(BrowserFrameSummary {
                frame_id: node.frame.id.clone(),
                parent_id: node.frame.parent_id.clone(),
                depth,
                url: refact_browser::mask_text(&node.frame.url),
                name: node.frame.name.clone().filter(|name| !name.is_empty()),
            });
        }
        for child in node.child_frames.iter().flatten() {
            walk(child, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    if let Ok(tree) = tab.call_method(Page::GetFrameTree(None)) {
        walk(&tree.frame_tree, 0, &mut out);
    }
    out
}

pub(crate) fn frames_for_snapshot(tab: &Tab, yaml: &str) -> Vec<BrowserFrameSummary> {
    if snapshot_mentions_iframe(yaml) {
        child_frame_summaries(tab)
    } else {
        Vec::new()
    }
}

pub(crate) fn build_page_snapshot(yaml: String, artifacts_dir: &Path) -> Result<BrowserPageSnapshot, String> {
    let settings = browser_settings::current();
    let bytes = yaml.len();
    let lines = yaml.lines().count();
    if bytes <= settings.capture.max_inline_snapshot_bytes {
        return Ok(BrowserPageSnapshot {
            yaml,
            lines,
            bytes,
            truncated: false,
            artifact: None,
            frames: Vec::new(),
        });
    }
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
    let path = artifacts_dir.join(format!("snapshot-{nonce}.yaml"));
    std::fs::write(&path, yaml.as_bytes()).map_err(|error| {
        format!(
            "Failed to save aria snapshot artifact {}: {error}",
            path.display()
        )
    })?;
    Ok(BrowserPageSnapshot {
        yaml: snapshot_head(&yaml, SNAPSHOT_SUMMARY_LINES),
        lines,
        bytes,
        truncated: true,
        artifact: Some(BrowserSnapshotArtifact {
            kind: "aria_snapshot".to_string(),
            mime: "text/yaml".to_string(),
            path,
            bytes,
        }),
        frames: Vec::new(),
    })
}

pub(crate) fn capture_page_snapshot(
    tab: &Tab,
    world: &WorldManager,
    artifacts_dir: &Path,
) -> Result<BrowserPageSnapshot, String> {
    let snapshot = world
        .aria_snapshot(
            tab,
            None,
            SnapshotOptions {
                mode: SnapshotMode::Ai,
                refs: true,
                ..Default::default()
            },
        )
        .map_err(|error| error.to_string())?;
    let frames = frames_for_snapshot(tab, &snapshot.yaml);
    let mut page_snapshot = build_page_snapshot(snapshot.yaml, artifacts_dir)?;
    page_snapshot.frames = frames;
    Ok(page_snapshot)
}

pub(crate) fn capture_report_screenshot(tab: &Tab, policy: &ImagePolicy) -> Result<BrowserScreenshot, String> {
    let (data, mime) = capture_viewport_screenshot_png(tab)?;
    let bytes = base64::prelude::BASE64_STANDARD
        .decode(data)
        .map_err(|error| format!("Screenshot decode failed: {error}"))?;
    let (resized, mime) = resize_to_policy(&bytes, &mime, policy)?;
    Ok(BrowserScreenshot {
        mime,
        data: base64::prelude::BASE64_STANDARD.encode(resized),
    })
}

pub(crate) fn capture_viewport_screenshot_png(tab: &Tab) -> Result<(String, String), String> {
    let result = tab
        .call_method(Page::CaptureScreenshot {
            format: Some(Page::CaptureScreenshotFormatOption::Png),
            clip: None,
            quality: None,
            from_surface: Some(true),
            capture_beyond_viewport: Some(false),
            optimize_for_speed: None,
        })
        .map_err(|error| error.to_string())?;
    Ok((result.data, "image/png".to_string()))
}

pub(crate) struct PolicyScreenshot {
    pub(crate) data: String,
    pub(crate) mime: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes: usize,
}

pub(crate) fn screenshot_metrics(tab: &Tab) -> Result<ScreenshotMetrics, String> {
    let metrics = tab
        .call_method(Page::GetLayoutMetrics(None))
        .map_err(|error| format!("Failed to read screenshot layout metrics: {error}"))?;
    let device_scale_factor = eval_js_value(tab, "window.devicePixelRatio")
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(1.0);
    Ok(ScreenshotMetrics {
        page_x: metrics.visual_viewport.page_x,
        page_y: metrics.visual_viewport.page_y,
        viewport_scale: metrics.visual_viewport.scale.max(f64::EPSILON),
        device_scale_factor,
        viewport_width: metrics.visual_viewport.client_width,
        viewport_height: metrics.visual_viewport.client_height,
        content_width: metrics.content_size.width,
        content_height: metrics.content_size.height,
    })
}

pub(crate) fn omit_background_applies(options: &BrowserScreenshotOptions) -> bool {
    options.omit_background && options.image_type.unwrap_or_default() != BrowserScreenshotType::Jpeg
}

pub(crate) fn screenshot_policy(options: &BrowserScreenshotOptions, policy: &ImagePolicy) -> ImagePolicy {
    policy.clone().with_format(
        match options.image_type.unwrap_or_default() {
            BrowserScreenshotType::Png => ImageFormat::Png,
            BrowserScreenshotType::Jpeg => ImageFormat::Jpeg,
            BrowserScreenshotType::Webp => ImageFormat::Webp,
        },
        options.quality.or(policy.quality),
    )
}

pub(crate) fn capture_screenshot(
    tab: &Tab,
    world: &WorldManager,
    options: &BrowserScreenshotOptions,
    element: Option<BrowserScreenshotClip>,
    policy: &ImagePolicy,
) -> Result<PolicyScreenshot, String> {
    let metrics = screenshot_metrics(tab)?;
    let capture = screenshot_capture(options, metrics, element)?;
    let cleanup = prepare_screenshot(tab, world, options)?;
    let transparent = omit_background_applies(options);
    let background_result = if transparent {
        tab.call_method(Emulation::SetDefaultBackgroundColorOverride {
            color: Some(DOM::RGBA {
                r: 0,
                g: 0,
                b: 0,
                a: Some(0.0),
            }),
        })
        .map(|_| ())
        .map_err(|error| format!("Failed to omit screenshot background: {error}"))
    } else {
        Ok(())
    };
    let raw = background_result.and_then(|_| {
        tab.call_method(Page::CaptureScreenshot {
            format: Some(capture.format),
            clip: Some(capture.clip),
            quality: capture.quality,
            from_surface: Some(true),
            capture_beyond_viewport: Some(capture.capture_beyond_viewport),
            optimize_for_speed: None,
        })
        .map_err(|error| format!("Screenshot capture failed: {error}"))
    });
    let background_cleanup = if transparent {
        tab.call_method(Emulation::SetDefaultBackgroundColorOverride { color: None })
            .map(|_| ())
            .map_err(|error| format!("Failed to restore screenshot background: {error}"))
    } else {
        Ok(())
    };
    if cleanup {
        let _ = world.eval_in_utility(tab, "window.__refactScreenshotCleanup?.()");
    }
    let raw = raw?;
    background_cleanup?;
    let raw_bytes = base64::prelude::BASE64_STANDARD
        .decode(raw.data)
        .map_err(|error| format!("Screenshot decode failed: {error}"))?;
    let (processed, mime) = resize_to_policy(
        &raw_bytes,
        capture.mime,
        &screenshot_policy(options, policy),
    )?;
    let decoded = image::load_from_memory(&processed)
        .map_err(|error| format!("Processed screenshot decode failed: {error}"))?;
    Ok(PolicyScreenshot {
        data: base64::prelude::BASE64_STANDARD.encode(&processed),
        mime,
        width: decoded.width(),
        height: decoded.height(),
        bytes: processed.len(),
    })
}

pub(crate) fn prepare_screenshot(
    tab: &Tab,
    world: &WorldManager,
    options: &BrowserScreenshotOptions,
) -> Result<bool, String> {
    let mut boxes = Vec::new();
    for locator in &options.mask {
        let handles = resolve_locator_handles(tab, world, locator)?;
        if handles.is_empty() {
            continue;
        }
        for handle in handles {
            let value = world
                .call_function_on(
                    tab,
                    &handle,
                    "function() { const r = this.getBoundingClientRect(); return {x:r.x+scrollX,y:r.y+scrollY,width:r.width,height:r.height}; }",
                    Vec::new(),
                )
                .map_err(|error| error.to_string());
            let _ = world.release_handle(tab, &handle);
            boxes.push(value?);
        }
    }
    let hide_caret = options.caret.unwrap_or_default() == BrowserScreenshotCaret::Hide;
    let disable_animations =
        options.animations.unwrap_or_default() == BrowserScreenshotAnimations::Disabled;
    if boxes.is_empty() && options.style.is_none() && !hide_caret && !disable_animations {
        return Ok(false);
    }
    let script = prepare_screenshot_script(
        &boxes,
        options.style.as_deref().unwrap_or(""),
        hide_caret,
        disable_animations,
        options.mask_color.as_deref().unwrap_or("#FF00FF"),
    )?;
    world.eval_in_utility(tab, &script)?;
    Ok(true)
}

pub(crate) fn prepare_screenshot_script(
    boxes: &[serde_json::Value],
    style: &str,
    hide_caret: bool,
    disable_animations: bool,
    mask_color: &str,
) -> Result<String, String> {
    Ok(format!(
        r#"(() => {{
  window.__refactScreenshotCleanup?.();
  const root = document.documentElement;
  const css = {} + {} + {};
  const collectRoots = (scope, roots) => {{
    roots.push(scope);
    const walker = document.createTreeWalker(scope, NodeFilter.SHOW_ELEMENT);
    do {{
      const node = walker.currentNode;
      const shadow = node instanceof Element ? node.shadowRoot : null;
      if (shadow) collectRoots(shadow, roots);
    }} while (walker.nextNode());
    return roots;
  }};
  const roots = css || {} ? collectRoots(document, []) : [];
  const styles = [];
  if (css) {{
    for (const scope of roots) {{
      const style = document.createElement('style');
      style.dataset.refactScreenshot = 'true';
      style.textContent = css;
      (scope === document ? root : scope).appendChild(style);
      styles.push(style);
    }}
  }}
  const maskRoot = document.createElement('div');
  maskRoot.dataset.refactScreenshotMask = 'true';
  Object.assign(maskRoot.style, {{position:'absolute',left:'0',top:'0',pointerEvents:'none',zIndex:'2147483647'}});
  const boxes = {};
  const color = {};
  for (const box of boxes) {{
    const mask = document.createElement('div');
    Object.assign(mask.style, {{position:'absolute',left:box.x+'px',top:box.y+'px',width:box.width+'px',height:box.height+'px',background:color}});
    maskRoot.appendChild(mask);
  }}
  root.appendChild(maskRoot);
  const animations = {} ? roots.flatMap(scope => scope.getAnimations()).map(animation => ({{animation,currentTime:animation.currentTime,playState:animation.playState}})) : [];
  for (const saved of animations) {{ try {{ saved.animation.finish(); }} catch {{ saved.animation.cancel(); }} }}
  window.__refactScreenshotCleanup = () => {{
    for (const style of styles) style.remove();
    maskRoot.remove();
    for (const saved of animations) {{
      try {{ saved.animation.currentTime = saved.currentTime; if (saved.playState === 'running') saved.animation.play(); else saved.animation.pause(); }} catch {{}}
    }}
    delete window.__refactScreenshotCleanup;
  }};
}})()"#,
        js_string_literal(style),
        if hide_caret {
            js_string_literal("\n*,*::before,*::after{caret-color:transparent!important}")
        } else {
            js_string_literal("")
        },
        if disable_animations {
            js_string_literal("\n*,*::before,*::after{transition-delay:0s!important;transition-duration:0s!important;animation-delay:0s!important;animation-duration:0s!important}")
        } else {
            js_string_literal("")
        },
        disable_animations,
        serde_json::to_string(boxes).map_err(|error| error.to_string())?,
        js_string_literal(mask_color),
        disable_animations,
    ))
}

pub(crate) fn step_screenshot(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    options: &BrowserScreenshotOptions,
    policy: &ImagePolicy,
) -> StepResult {
    match capture_screenshot(tab, world, options, None, policy) {
        Ok(capture) => {
            StepResult::success(idx, "Screenshot captured").with_data(image_artifact_data(&capture))
        }
        Err(error) => StepResult::failure(idx, "Screenshot failed", error),
    }
}

pub(crate) fn image_artifact_data(capture: &PolicyScreenshot) -> Value {
    serde_json::json!({
        "artifact": {
            "kind": "image",
            "mime": capture.mime,
            "data": capture.data,
            "width": capture.width,
            "height": capture.height,
            "bytes": capture.bytes,
        },
    })
}

pub(crate) fn persist_image_artifact(mut result: StepResult, artifacts_dir: &Path) -> StepResult {
    let Some(artifact) = result
        .data
        .as_mut()
        .and_then(Value::as_object_mut)
        .and_then(|data| data.get_mut("artifact"))
        .and_then(Value::as_object_mut)
    else {
        return result;
    };
    if artifact.get("kind").and_then(Value::as_str) != Some("image")
        || artifact.contains_key("path")
    {
        return result;
    }
    let (Some(mime), Some(encoded)) = (
        artifact.get("mime").and_then(Value::as_str),
        artifact.get("data").and_then(Value::as_str),
    ) else {
        return result;
    };
    let extension = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "bin",
    };
    let Ok(bytes) = base64::prelude::BASE64_STANDARD.decode(encoded) else {
        return result;
    };
    let dir = artifacts_dir.join("screenshots");
    if std::fs::create_dir_all(&dir).is_err() {
        return result;
    }
    let path = dir.join(format!("shot-{}.{extension}", uuid::Uuid::new_v4()));
    if std::fs::write(&path, bytes).is_ok() {
        artifact.insert(
            "path".to_string(),
            Value::String(path.to_string_lossy().into_owned()),
        );
    }
    result
}

pub(crate) fn is_image_capture_step(step: &BrowserStep) -> bool {
    matches!(
        step,
        BrowserStep::Screenshot { .. } | BrowserStep::ScreenshotElement { .. }
    )
}

pub(crate) fn step_screenshot_element(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    options: &BrowserScreenshotOptions,
    policy: &ImagePolicy,
) -> StepResult {
    let info = match resolve_scoped_element(tab, world, locator) {
        Ok(i) => i,
        Err(e) => return StepResult::failure(idx, "Screenshot element: resolution failed", e),
    };

    let bbox = match &info.bbox {
        Some(b) if b.width > 0.0 && b.height > 0.0 => b,
        _ => {
            return StepResult::failure(idx, "Screenshot element", "Element has no visible bounds")
        }
    };
    let metrics = match screenshot_metrics(tab) {
        Ok(metrics) => metrics,
        Err(error) => return StepResult::failure(idx, "Screenshot element", error),
    };

    let clip = BrowserScreenshotClip {
        x: metrics.page_x + bbox.x,
        y: metrics.page_y + bbox.y,
        width: bbox.width,
        height: bbox.height,
    };

    match capture_screenshot(tab, world, options, Some(clip), policy) {
        Ok(capture) => StepResult::success(idx, format!("Element screenshot of <{}>", info.tag))
            .with_data(image_artifact_data(&capture)),
        Err(e) => StepResult::failure(idx, "Element screenshot failed", e),
    }
}

pub(crate) fn element_viewport_rect(
    tab: &Tab,
    world: &WorldManager,
    handle: &ElementHandle,
) -> Result<BrowserScreenshotClip, String> {
    let value = call_handle_json(
        tab,
        world,
        handle,
        "function() { const r = this.getBoundingClientRect(); return JSON.stringify({x:r.x,y:r.y,width:r.width,height:r.height}); }",
    )?;
    let rect: BrowserScreenshotClip = serde_json::from_value(value)
        .map_err(|error| format!("Failed to read element bounds: {error}"))?;
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return Err("Element has no visible bounds".to_string());
    }
    Ok(rect)
}

pub(crate) fn capture_element(
    tab: &Tab,
    world: &WorldManager,
    handle: &ElementHandle,
    options: &BrowserScreenshotOptions,
    policy: &ImagePolicy,
) -> Result<PolicyScreenshot, String> {
    let rect = element_viewport_rect(tab, world, handle)?;
    let metrics = screenshot_metrics(tab)?;
    capture_screenshot(
        tab,
        world,
        options,
        Some(BrowserScreenshotClip {
            x: metrics.page_x + rect.x,
            y: metrics.page_y + rect.y,
            width: rect.width,
            height: rect.height,
        }),
        policy,
    )
}

pub(crate) fn compose_captures(
    captures: &[(String, PolicyScreenshot)],
    layout: ComposeLayout,
    labels: bool,
    options: &BrowserScreenshotOptions,
    policy: &ImagePolicy,
) -> Result<PolicyScreenshot, String> {
    let tiles = captures
        .iter()
        .map(|(label, capture)| {
            base64::prelude::BASE64_STANDARD
                .decode(&capture.data)
                .map(|bytes| (label.clone(), bytes))
                .map_err(|error| format!("Capture decode failed: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let sheet = compose_sheet(&tiles, layout, labels)?;
    let (processed, mime) =
        resize_to_policy(&sheet, "image/png", &screenshot_policy(options, policy))?;
    let decoded = image::load_from_memory(&processed)
        .map_err(|error| format!("Composed sheet decode failed: {error}"))?;
    Ok(PolicyScreenshot {
        data: base64::prelude::BASE64_STANDARD.encode(&processed),
        mime,
        width: decoded.width(),
        height: decoded.height(),
        bytes: processed.len(),
    })
}

pub(crate) fn capture_json(label: &str, capture: &PolicyScreenshot) -> serde_json::Value {
    serde_json::json!({
        "label": label,
        "mime": capture.mime,
        "data": capture.data,
        "width": capture.width,
        "height": capture.height,
        "bytes": capture.bytes,
    })
}

pub(crate) fn step_screenshot_elements(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locators: &[BrowserLocator],
    compose: BrowserComposeMode,
    labels: Option<bool>,
    options: &BrowserScreenshotOptions,
    policy: &ImagePolicy,
) -> StepResult {
    if locators.is_empty() {
        return StepResult::failure(
            idx,
            "Screenshot elements",
            "screenshot_elements requires at least one locator",
        );
    }
    if locators.len() > policy.max_images {
        return StepResult::failure(
            idx,
            "Screenshot elements",
            format!(
                "screenshot_elements accepts at most {} locators, got {}",
                policy.max_images,
                locators.len()
            ),
        );
    }
    let mut captures = Vec::with_capacity(locators.len());
    for locator in locators {
        let label = describe_locator(locator);
        let resolved = match resolve_element(tab, world, locator) {
            Ok(resolved) => resolved,
            Err(error) => {
                return StepResult::failure(
                    idx,
                    "Screenshot elements",
                    format!("{label}: {error}"),
                );
            }
        };
        let capture = capture_element(tab, world, &resolved.handle, options, policy);
        let _ = world.release_handle(tab, &resolved.handle);
        match capture {
            Ok(capture) => captures.push((label, capture)),
            Err(error) => {
                return StepResult::failure(
                    idx,
                    "Screenshot elements",
                    format!("{label}: {error}"),
                );
            }
        }
    }
    let labels = labels.unwrap_or(true);
    match compose {
        BrowserComposeMode::Separate => StepResult::success(
            idx,
            format!("Captured {} element screenshots", captures.len()),
        )
        .with_data(serde_json::json!({
            "compose": "separate",
            "count": captures.len(),
            "images": captures
                .iter()
                .map(|(label, capture)| capture_json(label, capture))
                .collect::<Vec<_>>(),
        })),
        BrowserComposeMode::Grid => {
            match compose_captures(&captures, ComposeLayout::Grid, labels, options, policy) {
                Ok(sheet) => StepResult::success(
                    idx,
                    format!("Composed {} element screenshots into a grid", captures.len()),
                )
                .with_data(serde_json::json!({
                    "compose": "grid",
                    "count": captures.len(),
                    "labels": captures.iter().map(|(label, _)| label).collect::<Vec<_>>(),
                    "artifact": {"kind": "image", "mime": sheet.mime, "width": sheet.width, "height": sheet.height, "bytes": sheet.bytes},
                    "images": [capture_json("grid", &sheet)],
                })),
                Err(error) => StepResult::failure(idx, "Screenshot elements", error),
            }
        }
    }
}

pub(crate) fn step_capture_element_states(
    tab: &Tab,
    world: &WorldManager,
    idx: usize,
    locator: &BrowserLocator,
    states: &[BrowserElementState],
    labels: Option<bool>,
    options: &BrowserScreenshotOptions,
    policy: &ImagePolicy,
) -> StepResult {
    let resolved = match resolve_element(tab, world, locator) {
        Ok(resolved) => resolved,
        Err(error) => return StepResult::failure(idx, "Capture element states", error),
    };
    let outcome = drive_element_states(tab, world, &resolved.handle, states, options, policy);
    let _ = world.release_handle(tab, &resolved.handle);
    let captures = match outcome {
        Ok(captures) => captures,
        Err(error) => return StepResult::failure(idx, "Capture element states", error),
    };
    match compose_captures(
        &captures,
        ComposeLayout::Strip,
        labels.unwrap_or(true),
        options,
        policy,
    ) {
        Ok(strip) => StepResult::success(
            idx,
            format!(
                "Captured <{}> in {} states",
                resolved.info.tag,
                captures.len()
            ),
        )
        .with_data(serde_json::json!({
            "states": captures.iter().map(|(label, _)| label).collect::<Vec<_>>(),
            "artifact": {"kind": "image", "mime": strip.mime, "width": strip.width, "height": strip.height, "bytes": strip.bytes},
            "images": [capture_json("states", &strip)],
        })),
        Err(error) => StepResult::failure(idx, "Capture element states", error),
    }
}

pub(crate) fn run_element_state_sequence<T, F>(
    tab: &Tab,
    world: &WorldManager,
    handle: &ElementHandle,
    states: &[BrowserElementState],
    observe: F,
) -> Result<Vec<(BrowserElementState, T)>, String>
where
    F: FnMut(BrowserElementState) -> Result<T, String>,
{
    let keyboard = Keyboard::new(CdpKeyboardDispatcher::new(tab));
    let mut mouse = Mouse::new(CdpMouseDispatcher::new(tab), &keyboard);
    drive_element_state_actions(
        &mut mouse,
        element_state_sequence(states),
        || {
            CdpMouseDispatcher::new(tab)
                .clickable_point(handle)
                .map_err(|error| error.to_string())
        },
        |focused| {
            let script = if focused {
                browser_locators::js_focus_element()
            } else {
                browser_locators::js_blur_element()
            };
            call_handle_json(tab, world, handle, &script).map(|_| ())
        },
        observe,
    )
}

pub(crate) fn drive_element_state_actions<D, K, T, F>(
    mouse: &mut Mouse<'_, D, K>,
    actions: impl IntoIterator<Item = ElementStateAction>,
    mut hover_point: impl FnMut() -> Result<MainFrameCssPoint, String>,
    mut set_focus: impl FnMut(bool) -> Result<(), String>,
    mut observe: F,
) -> Result<Vec<(BrowserElementState, T)>, String>
where
    D: MouseDispatcher,
    K: KeyboardDispatcher,
    F: FnMut(BrowserElementState) -> Result<T, String>,
{
    let mut point: Option<MainFrameCssPoint> = None;
    let mut observations = Vec::new();
    let mut held = false;
    let mut outcome = Ok(());
    for action in actions {
        let step = match action {
            ElementStateAction::Hover => match point {
                Some(target) => Ok(target),
                None => hover_point().inspect(|resolved| point = Some(*resolved)),
            }
            .and_then(|target| {
                mouse
                    .hover(target.x, target.y)
                    .map_err(|error| error.to_string())
            }),
            ElementStateAction::MoveMouseAway => mouse
                .move_to(0.0, 0.0, 1)
                .map_err(|error| error.to_string()),
            ElementStateAction::PressAndHold => mouse
                .down(MouseButton::Left, 1)
                .map_err(|error| error.to_string())
                .inspect(|_| held = true),
            ElementStateAction::ReleaseMouse => mouse
                .up(MouseButton::Left, 1)
                .map_err(|error| error.to_string())
                .inspect(|_| held = false),
            ElementStateAction::Focus => set_focus(true),
            ElementStateAction::Blur => set_focus(false),
            ElementStateAction::Capture(state) => {
                observe(state).map(|observed| observations.push((state, observed)))
            }
        };
        if let Err(error) = step {
            outcome = Err(error);
            break;
        }
    }
    if held {
        let _ = mouse.up(MouseButton::Left, 1);
    }
    outcome.map(|()| observations)
}

pub(crate) fn drive_element_states(
    tab: &Tab,
    world: &WorldManager,
    handle: &ElementHandle,
    states: &[BrowserElementState],
    options: &BrowserScreenshotOptions,
    policy: &ImagePolicy,
) -> Result<Vec<(String, PolicyScreenshot)>, String> {
    let captures = run_element_state_sequence(tab, world, handle, states, |_| {
        capture_element(tab, world, handle, options, policy)
    })?;
    Ok(captures
        .into_iter()
        .map(|(state, capture)| (state.label().to_string(), capture))
        .collect())
}
