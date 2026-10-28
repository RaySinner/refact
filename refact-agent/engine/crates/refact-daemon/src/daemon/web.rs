//! Daemon web serving uses a generated fetch/EventSource prefix shim for `/p/{id}` because
//! the GUI's web `engineServed` mode resolves API calls relative to the current origin.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use axum::body::{boxed, Bytes, Full};
use axum::extract::{OriginalUri, Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use flate2::write::GzEncoder;
use flate2::Compression;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use rust_embed::EmbeddedFile;
use serde::Serialize;

use crate::daemon::projects::ProjectEntry;
use crate::daemon::state::{now_ms, DaemonState};
use crate::daemon::supervisor::{WorkerInfo, WorkerState};

const PICKER_TEMPLATE: &str = include_str!("web_picker.html");
const DAEMON_GUI_BOOTSTRAP_SENTINEL: &str = "__REFACT_DAEMON_PROJECT_API_PREFIX__";
const GUI_BOOTSTRAP_CONFIG_ASSIGNMENT: &str = "window.__REFACT_BOOTSTRAP_CONFIG__ = ";

#[derive(Debug, Serialize)]
struct PickerData {
    daemon: PickerDaemon,
    projects: Vec<PickerProject>,
}

#[derive(Debug, Serialize)]
struct PickerDaemon {
    version: String,
    port: u16,
    started_at_ms: u64,
    uptime_secs: u64,
    log_path: String,
}

#[derive(Debug, Serialize)]
struct PickerProject {
    id: String,
    slug: String,
    root: String,
    pinned: bool,
    worker_state: String,
    lsp_clients: usize,
    busy_chats: usize,
    exec_running: usize,
    live_proxy_streams: u64,
    cron_next_fire_ms: Option<u64>,
    last_active_ms: u64,
    log_path: String,
}

pub(crate) async fn handle_project_picker(
    OriginalUri(uri): OriginalUri,
    State((state, port)): State<(Arc<DaemonState>, u16)>,
) -> Response {
    if let Some(response) = daemon_auth_redirect(&state, &uri) {
        return response;
    }
    let data = picker_data(state, port).await;
    html_response(StatusCode::OK, render_picker_html(&data))
}

pub(crate) async fn handle_dashboard_index(
    OriginalUri(uri): OriginalUri,
    State((state, port)): State<(Arc<DaemonState>, u16)>,
) -> Response {
    if let Some(response) = daemon_auth_redirect(&state, &uri) {
        return response;
    }
    let data = picker_data(state, port).await;
    match state.gui_assets().and_then(|get| get(INDEX_PATH)) {
        Some(asset) => asset_response(
            INDEX_PATH,
            dashboard_gui_index_body(asset.data, &data, port),
            StatusCode::OK,
        ),
        None => html_response(
            StatusCode::SERVICE_UNAVAILABLE,
            missing_gui_index_html().as_bytes().to_vec().into(),
        ),
    }
}

pub(crate) async fn handle_project_gui_redirect(OriginalUri(uri): OriginalUri) -> Response {
    let path = uri.path();
    let location = match uri.query() {
        Some(query) if !query.is_empty() => format!("{path}/?{query}"),
        _ => format!("{path}/"),
    };
    let mut response = Response::new(axum::body::boxed(axum::body::Full::from(Vec::<u8>::new())));
    *response.status_mut() = StatusCode::PERMANENT_REDIRECT;
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}

pub(crate) async fn handle_project_gui_index(
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    State((state, port)): State<(Arc<DaemonState>, u16)>,
    AxumPath(project_id): AxumPath<String>,
) -> Response {
    if let Some(response) = daemon_auth_redirect(&state, &uri) {
        return response;
    }
    if project_entry(&state, &project_id).await.is_none() {
        return html_response(
            StatusCode::NOT_FOUND,
            unknown_project_html(&project_id).into_bytes().into(),
        );
    }

    match state.gui_assets().and_then(|get| get(INDEX_PATH)) {
        Some(asset) => {
            let body = project_gui_index_body(asset.data, &project_id, port);
            let mut response = asset_response(INDEX_PATH, body, StatusCode::OK);
            if let Some(cookie) = state.auth_token.as_deref().and_then(|expected| {
                crate::daemon::auth::project_cookie_from_headers(&headers, &project_id, expected)
            }) {
                if let Ok(cookie) =
                    HeaderValue::from_str(&daemon_auth_cookie_value(Some(&project_id), &cookie))
                {
                    response.headers_mut().insert(header::SET_COOKIE, cookie);
                }
                if let Ok(cookie) = HeaderValue::from_str(&expire_daemon_auth_cookie()) {
                    response.headers_mut().append(header::SET_COOKIE, cookie);
                }
            }
            response
        }
        None => html_response(
            StatusCode::SERVICE_UNAVAILABLE,
            missing_gui_index_html().as_bytes().to_vec().into(),
        ),
    }
}

pub(crate) async fn handle_daemon_gui_asset(
    AxumPath(path): AxumPath<String>,
    State((state, _port)): State<(Arc<DaemonState>, u16)>,
    request_headers: HeaderMap,
) -> impl IntoResponse {
    if invalid_asset_path(&path) {
        return text_response(
            StatusCode::BAD_REQUEST,
            "invalid GUI asset path".to_string(),
        );
    }

    let embedded_path = format!("{ASSET_PREFIX}{path}");
    match state.gui_assets().and_then(|get| get(&embedded_path)) {
        Some(asset) => embedded_asset_response(&embedded_path, asset, &request_headers),
        None => text_response(
            StatusCode::NOT_FOUND,
            format!("GUI asset not found: {path}"),
        ),
    }
}

fn invalid_asset_path(path: &str) -> bool {
    path.is_empty() || path.split('/').any(|part| part == ".." || part.is_empty())
}

async fn project_entry(state: &DaemonState, project_id: &str) -> Option<ProjectEntry> {
    state.projects.read().await.get(project_id).cloned()
}

fn daemon_auth_redirect(state: &DaemonState, uri: &Uri) -> Option<Response> {
    let expected = state.auth_token.as_deref()?;
    let token = crate::daemon::auth::matching_daemon_query_token(uri.query(), expected)?;
    let project_id = crate::daemon::auth::requested_project_id(uri.path());
    let mut response = Response::new(axum::body::boxed(axum::body::Full::from(Vec::<u8>::new())));
    *response.status_mut() = StatusCode::SEE_OTHER;
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&redirect_without_daemon_token(uri)).ok()?,
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&daemon_auth_cookie(project_id, &token)).ok()?,
    );
    if project_id.is_some() {
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&expire_daemon_auth_cookie()).ok()?,
        );
    }
    Some(response)
}

fn daemon_auth_cookie(project_id: Option<&str>, token: &str) -> String {
    daemon_auth_cookie_value(
        project_id,
        &project_id
            .map(|project_id| crate::daemon::auth::project_cookie_value(project_id, token))
            .unwrap_or_else(|| token.to_string()),
    )
}

fn daemon_auth_cookie_value(project_id: Option<&str>, value: &str) -> String {
    match project_id {
        Some(project_id) => format!(
            "{}={}; HttpOnly; SameSite=Strict; Path={}/",
            crate::daemon::auth::DAEMON_AUTH_COOKIE,
            value,
            project_api_prefix(project_id)
        ),
        None => format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/",
            crate::daemon::auth::DAEMON_AUTH_COOKIE,
            value
        ),
    }
}

fn expire_daemon_auth_cookie() -> String {
    format!(
        "{}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",
        crate::daemon::auth::DAEMON_AUTH_COOKIE
    )
}

fn redirect_without_daemon_token(uri: &Uri) -> String {
    let path = uri.path();
    let pairs = uri
        .query()
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
        .filter(|(name, _)| name != crate::daemon::auth::DAEMON_AUTH_QUERY)
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        return path.to_string();
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in pairs {
        serializer.append_pair(&name, &value);
    }
    format!("{}?{}", path, serializer.finish())
}

async fn picker_data(state: Arc<DaemonState>, port: u16) -> PickerData {
    let mut entries = state.projects.read().await.list();
    entries.sort_by(|a, b| a.slug.cmp(&b.slug).then_with(|| a.id.cmp(&b.id)));
    let mut projects = Vec::with_capacity(entries.len());
    for entry in entries {
        let worker = state.supervisor.worker_info(&entry.id).await;
        let status = state.latest_worker_status(&entry.id).await;
        let activity = state.proxy_activity(&entry.id).await;
        let last_activity_ms = [
            entry.last_active_ms,
            status
                .as_ref()
                .map(|status| status.last_activity_ts)
                .unwrap_or_default(),
            activity.last_proxy_activity_ms,
        ]
        .into_iter()
        .max()
        .unwrap_or_default();
        projects.push(PickerProject {
            id: entry.id.clone(),
            slug: entry.slug.clone(),
            root: entry.root.to_string_lossy().to_string(),
            pinned: entry.pinned,
            worker_state: worker_state_label(worker.as_ref()),
            lsp_clients: status
                .as_ref()
                .map(|status| status.lsp_clients)
                .unwrap_or_default(),
            busy_chats: status
                .as_ref()
                .map(|status| status.busy_chats)
                .unwrap_or_default(),
            exec_running: status
                .as_ref()
                .map(|status| status.exec_running)
                .unwrap_or_default(),
            live_proxy_streams: activity.live_proxy_streams,
            cron_next_fire_ms: state.cron_pending(&entry.id).await,
            last_active_ms: last_activity_ms,
            log_path: state
                .daemon_dir
                .join("logs")
                .join(format!("worker-{}.log", entry.slug))
                .to_string_lossy()
                .to_string(),
        });
    }

    PickerData {
        daemon: PickerDaemon {
            version: state.version.clone(),
            port,
            started_at_ms: state.started_at_ms,
            uptime_secs: Duration::from_millis(now_ms().saturating_sub(state.started_at_ms))
                .as_secs(),
            log_path: state
                .daemon_dir
                .join("logs")
                .join("daemon.log")
                .to_string_lossy()
                .to_string(),
        },
        projects,
    }
}

fn worker_state_label(worker: Option<&WorkerInfo>) -> String {
    match worker.map(|worker| &worker.state) {
        Some(WorkerState::Stopped) | None => "stopped".to_string(),
        Some(WorkerState::Starting) => "starting".to_string(),
        Some(WorkerState::Ready) => "ready".to_string(),
        Some(WorkerState::Stopping) => "stopping".to_string(),
        Some(WorkerState::Crashed) => "crashed".to_string(),
        Some(WorkerState::Failed { .. }) => "failed".to_string(),
    }
}

fn render_picker_html(data: &PickerData) -> Cow<'static, [u8]> {
    let rows = picker_rows_html(&data.projects);
    let data_json = json_for_script(data);
    let meta = format!(
        "v{} · port {} · uptime {}s",
        html_escape(&data.daemon.version),
        data.daemon.port,
        data.daemon.uptime_secs
    );
    Cow::Owned(
        PICKER_TEMPLATE
            .replace("__REFACT_DAEMON_PICKER_META__", &meta)
            .replace("__REFACT_DAEMON_PICKER_ROWS__", &rows)
            .replace("__REFACT_DAEMON_PICKER_DATA__", &data_json)
            .into_bytes(),
    )
}

fn picker_rows_html(projects: &[PickerProject]) -> String {
    if projects.is_empty() {
        return "<tr><td class=\"empty\" colspan=\"6\">No projects registered yet. Run <code>refact projects open .</code> from a workspace.</td></tr>".to_string();
    }

    projects
        .iter()
        .map(|project| {
            let state = html_escape(&project.worker_state);
            let class = css_class_segment(&project.worker_state);
            format!(
                "<tr><td data-label=\"Project\"><div class=\"project\"><strong>{}</strong><span class=\"root muted\">{}</span></div></td><td data-label=\"Worker\"><span class=\"dot dot-{}\"></span>{}</td><td data-label=\"LSP clients\">{}</td><td data-label=\"Cron next fire\">{}</td><td data-label=\"Last active\">{}</td><td data-label=\"Action\"><a class=\"button\" href=\"{}\">Open GUI</a></td></tr>",
                html_escape(&project.slug),
                html_escape(&project.root),
                class,
                state,
                project.lsp_clients,
                project
                    .cron_next_fire_ms
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "—".to_string()),
                project.last_active_ms,
                project_gui_path(&project.id),
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

fn project_gui_index_body(
    body: Cow<'static, [u8]>,
    project_id: &str,
    port: u16,
) -> Cow<'static, [u8]> {
    let candidates = daemon_origin_candidates(port, project_id);
    let body = inject_gui_origin_candidates(body, &candidates);
    let Ok(html) = std::str::from_utf8(body.as_ref()) else {
        return body;
    };
    let html = inject_daemon_gui_bootstrap(html, project_id, &candidates);
    Cow::Owned(inject_gui_bootstrap_config(&html, "workspace").into_bytes())
}

fn dashboard_gui_index_body(
    body: Cow<'static, [u8]>,
    data: &PickerData,
    port: u16,
) -> Cow<'static, [u8]> {
    let candidates = daemon_root_origin_candidates(port);
    let body = inject_gui_origin_candidates(body, &candidates);
    let Ok(html) = std::str::from_utf8(body.as_ref()) else {
        return body;
    };
    let html = inject_gui_bootstrap_config(html, "dashboard");
    Cow::Owned(inject_dashboard_noscript(&html, &data.projects).into_bytes())
}

fn inject_gui_bootstrap_config(html: &str, surface: &str) -> String {
    if html.contains(GUI_BOOTSTRAP_CONFIG_ASSIGNMENT) {
        return html.to_string();
    }
    let config = serde_json::json!({"surface": surface});
    let script = format!(
        "    <script>{GUI_BOOTSTRAP_CONFIG_ASSIGNMENT}{};</script>",
        json_for_script(&config)
    );
    insert_head_script(html, &script)
}

fn inject_dashboard_noscript(html: &str, projects: &[PickerProject]) -> String {
    let project_list = if projects.is_empty() {
        "<p>No projects registered.</p>".to_string()
    } else {
        let links = projects
            .iter()
            .map(|project| {
                format!(
                    "<li><a href=\"{}\">{}</a></li>",
                    project_gui_path(&project.id),
                    html_escape(&project.slug)
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!("<ul>{links}</ul>")
    };
    let noscript = format!(
        "    <noscript><h1>Refact projects</h1>{project_list}<p><a href=\"/picker\">Open the legacy project picker</a></p></noscript>"
    );
    insert_body_html(html, &noscript)
}

fn inject_daemon_gui_bootstrap(
    html: &str,
    project_id: &str,
    candidates: &GuiPublicOriginCandidates,
) -> String {
    if html.contains(DAEMON_GUI_BOOTSTRAP_SENTINEL) {
        return html.to_string();
    }
    let script = daemon_gui_bootstrap_script(project_id, candidates);
    insert_head_script(html, &script)
}

fn daemon_gui_bootstrap_script(project_id: &str, candidates: &GuiPublicOriginCandidates) -> String {
    let prefix = project_api_prefix(project_id);
    let prefix_json = json_for_script(&prefix);
    let origins_json = json_for_script(&candidates.origins);
    r#"    <script>
      (function () {
        const daemonProjectApiPrefix = __PREFIX__;
        window.__REFACT_DAEMON_PROJECT_API_PREFIX__ = daemonProjectApiPrefix;
        const daemonOriginCandidates = __ORIGINS__;
        const currentOriginCandidates = Array.isArray(window.__REFACT_ENGINE_ORIGIN_CANDIDATES__)
          ? window.__REFACT_ENGINE_ORIGIN_CANDIDATES__
          : [];
        window.__REFACT_ENGINE_ORIGIN_CANDIDATES__ = Array.from(
          new Set(currentOriginCandidates.concat(daemonOriginCandidates)),
        );
        const origin = window.location.origin;
        const isApiPath = function (pathname) {
          return pathname === "/v1" || pathname.startsWith("/v1/");
        };
        const prefixedApiUrl = function (url) {
          return origin + daemonProjectApiPrefix + url.pathname + url.search + url.hash;
        };
        const prefixDaemonProjectApi = function (value) {
          if (typeof value === "string") {
            if (value === "/v1" || value.startsWith("/v1/")) {
              return daemonProjectApiPrefix + value;
            }
            try {
              const url = new URL(value, origin);
              if (url.origin === origin && isApiPath(url.pathname)) {
                return prefixedApiUrl(url);
              }
            } catch (_error) {}
          }
          if (typeof URL === "function" && value instanceof URL && value.origin === origin && isApiPath(value.pathname)) {
            return new URL(prefixedApiUrl(value));
          }
          if (typeof Request === "function" && value instanceof Request) {
            try {
              const url = new URL(value.url);
              if (url.origin === origin && isApiPath(url.pathname)) {
                if (value.method === "GET" || value.method === "HEAD" || value.body === null) {
                  return new Request(prefixedApiUrl(url), value);
                }
                return value.clone().arrayBuffer().then(function (buf) {
                  return new Request(prefixedApiUrl(url), {
                    method: value.method,
                    headers: value.headers,
                    body: buf,
                    mode: value.mode,
                    credentials: value.credentials,
                    cache: value.cache,
                    redirect: value.redirect,
                    referrer: value.referrer,
                    referrerPolicy: value.referrerPolicy,
                    integrity: value.integrity,
                    keepalive: value.keepalive,
                    signal: value.signal,
                  });
                });
              }
            } catch (_error) {}
          }
          return value;
        };
        if (typeof window.fetch === "function") {
          const originalFetch = window.fetch.bind(window);
          window.fetch = function (input, init) {
            const p = prefixDaemonProjectApi(input);
            if (p && typeof p.then === "function") {
              return p.then(function (value) {
                return originalFetch(value, init);
              });
            }
            return originalFetch(p, init);
          };
        }
        if (typeof window.EventSource === "function") {
          const originalEventSource = window.EventSource;
          window.EventSource = function (input, init) {
            return new originalEventSource(prefixDaemonProjectApi(input), init);
          };
          window.EventSource.prototype = originalEventSource.prototype;
          if (typeof Object.setPrototypeOf === "function") {
            Object.setPrototypeOf(window.EventSource, originalEventSource);
          }
        }
      })();
    </script>"#
        .replace("__PREFIX__", &prefix_json)
        .replace("__ORIGINS__", &origins_json)
}

fn insert_head_script(html: &str, script: &str) -> String {
    if let Some(index) = html.find("<head>") {
        let insert_at = index + "<head>".len();
        return format!("{}\n{}{}", &html[..insert_at], script, &html[insert_at..]);
    }
    if let Some(index) = html.find("<head ") {
        if let Some(offset) = html[index..].find('>') {
            let insert_at = index + offset + 1;
            return format!("{}\n{}{}", &html[..insert_at], script, &html[insert_at..]);
        }
    }
    if let Some(index) = html.find("</head>") {
        return format!("{}{}\n{}", &html[..index], script, &html[index..]);
    }
    format!("{}\n{}", script, html)
}

fn insert_body_html(html: &str, body_html: &str) -> String {
    if let Some(index) = html.find("<body>") {
        let insert_at = index + "<body>".len();
        return format!(
            "{}\n{}{}",
            &html[..insert_at],
            body_html,
            &html[insert_at..]
        );
    }
    if let Some(index) = html.find("<body ") {
        if let Some(offset) = html[index..].find('>') {
            let insert_at = index + offset + 1;
            return format!(
                "{}\n{}{}",
                &html[..insert_at],
                body_html,
                &html[insert_at..]
            );
        }
    }
    if let Some(index) = html.find("</body>") {
        return format!("{}{}\n{}", &html[..index], body_html, &html[index..]);
    }
    format!("{}\n{}", html, body_html)
}

fn daemon_root_origin_candidates(port: u16) -> GuiPublicOriginCandidates {
    let mut origins = vec![format!("http://127.0.0.1:{port}")];
    for origin in gui_public_origin_candidates(port).origins {
        if !origins.contains(&origin) {
            origins.push(origin);
        }
    }
    GuiPublicOriginCandidates { origins }
}

fn daemon_origin_candidates(port: u16, project_id: &str) -> GuiPublicOriginCandidates {
    let prefix = project_api_prefix(project_id);
    let mut origins = Vec::new();
    origins.push(format!("http://127.0.0.1:{port}{prefix}"));
    for origin in gui_public_origin_candidates(port).origins {
        let candidate = format!("{}{prefix}", origin.trim_end_matches('/'));
        if !origins.contains(&candidate) {
            origins.push(candidate);
        }
    }
    GuiPublicOriginCandidates { origins }
}

fn unknown_project_html(project_id: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"UTF-8\" /><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" /><title>Project not found</title></head><body><h1>Project not found</h1><p>No daemon project is registered for <code>{}</code>.</p><p><a href=\"/\">Back to projects</a></p></body></html>",
        html_escape(project_id)
    )
}

fn project_api_prefix(project_id: &str) -> String {
    format!("/p/{}", path_segment(project_id))
}

fn project_gui_path(project_id: &str) -> String {
    format!("{}/", project_api_prefix(project_id))
}

fn path_segment(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn json_for_script<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "null".to_string())
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn css_class_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::response::IntoResponse;
    use hyper::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    use crate::daemon::config::DaemonConfig;
    use crate::daemon::events::EventBus;
    use crate::daemon::projects::ProjectRegistry;
    use crate::daemon::state::WorkerStatusReport;

    #[derive(rust_embed::RustEmbed)]
    #[folder = "tests_gui_fixture/"]
    struct TestGuiAssets;

    fn test_gui_assets(path: &str) -> Option<EmbeddedFile> {
        TestGuiAssets::get(path)
    }

    struct TestState {
        _dir: tempfile::TempDir,
        state: Arc<DaemonState>,
    }

    async fn test_state() -> TestState {
        test_state_with_auth(None).await
    }

    async fn test_state_with_auth(token: Option<&str>) -> TestState {
        let dir = tempfile::tempdir().unwrap();
        let mut config = DaemonConfig::default();
        if let Some(token) = token {
            config.auth = crate::daemon::config::AuthConfig {
                enabled: true,
                token: Some(token.to_string()),
                ..Default::default()
            };
        }
        let state = DaemonState::new(
            config,
            EventBus::new(dir.path().join("events.jsonl")),
            token.map(str::to_string),
        );
        state.set_gui_assets(Some(test_gui_assets));
        *state.projects.write().await = ProjectRegistry::empty(dir.path().join("projects.json"));
        TestState { _dir: dir, state }
    }

    fn status_report(project_id: &str, lsp_clients: usize) -> WorkerStatusReport {
        WorkerStatusReport {
            project_id: project_id.to_string(),
            pid: 123,
            instance_token: "token".to_string(),
            lsp_clients,
            busy_chats: 2,
            exec_running: 1,
            last_activity_ts: 777,
        }
    }

    #[test]
    fn daemon_gui_index_injects_prefix_shim_and_escapes_project_id() {
        let candidates = GuiPublicOriginCandidates {
            origins: vec!["http://127.0.0.1:8488/p/abc123".to_string()],
        };
        let html = r#"<html><head><title>Refact</title></head><body></body></html>"#;
        let injected = inject_daemon_gui_bootstrap(html, "abc\"</script>", &candidates);

        assert!(injected.contains("const daemonProjectApiPrefix = \"/p/abc%22%3C%2Fscript%3E\";"));
        assert!(injected.contains("window.__REFACT_DAEMON_PROJECT_API_PREFIX__"));
        assert!(injected.contains("window.fetch = function"));
        assert!(injected.contains("value.clone().arrayBuffer().then"));
        assert!(injected.contains("body: buf"));
        assert!(injected.contains("typeof p.then === \"function\""));
        assert!(injected.contains("originalFetch(value, init)"));
        assert!(injected.contains("originalFetch(p, init)"));
        assert!(injected.contains("window.EventSource = function"));
        assert!(injected.contains("http://127.0.0.1:8488/p/abc123"));
        assert!(!injected.contains("abc\"</script>"));
    }

    #[test]
    fn daemon_origin_candidates_are_project_aware() {
        let candidates = daemon_origin_candidates(8488, "abc123");
        assert!(candidates
            .origins
            .iter()
            .any(|origin| origin == "http://127.0.0.1:8488/p/abc123"));
        assert!(candidates
            .origins
            .iter()
            .all(|origin| origin.ends_with("/p/abc123")));
    }

    #[test]
    fn daemon_gui_bootstrap_composes_with_origin_marker_injection() {
        let worker_candidates = GuiPublicOriginCandidates {
            origins: vec!["http://127.0.0.1:8488".to_string()],
        };
        let daemon_candidates = GuiPublicOriginCandidates {
            origins: vec!["http://127.0.0.1:8488/p/abc123".to_string()],
        };
        let asset = test_gui_assets(INDEX_PATH).expect("embedded index.html");
        let body =
            inject_gui_origin_candidates(asset.data, &worker_candidates);
        let html = std::str::from_utf8(body.as_ref()).unwrap();
        let injected = inject_daemon_gui_bootstrap(html, "abc123", &daemon_candidates);

        assert!(injected.contains("http://127.0.0.1:8488"));
        assert!(injected.contains("http://127.0.0.1:8488/p/abc123"));
        assert_eq!(injected.matches(DAEMON_GUI_BOOTSTRAP_SENTINEL).count(), 1);
    }

    #[test]
    fn redirect_without_daemon_token_preserves_other_query() {
        let uri: Uri = "/p/abc/?daemon_token=secret&theme=dark&q=a%20b"
            .parse()
            .unwrap();

        assert_eq!(
            redirect_without_daemon_token(&uri),
            "/p/abc/?theme=dark&q=a+b"
        );
    }

    #[test]
    fn picker_html_mentions_auth_error() {
        assert!(PICKER_TEMPLATE
            .contains("Authentication required. Open this page through the daemon launch URL."));
    }

    #[test]
    fn picker_html_contains_project_rows_and_escaped_data() {
        let data = PickerData {
            daemon: PickerDaemon {
                version: "8.1.0".to_string(),
                port: 8488,
                started_at_ms: 10,
                uptime_secs: 3,
                log_path: "/tmp/daemon/logs/daemon.log".to_string(),
            },
            projects: vec![PickerProject {
                id: "abc123".to_string(),
                slug: "my-project".to_string(),
                root: "/tmp/<workspace>".to_string(),
                pinned: false,
                worker_state: "ready".to_string(),
                lsp_clients: 2,
                busy_chats: 1,
                exec_running: 0,
                live_proxy_streams: 0,
                cron_next_fire_ms: Some(42),
                last_active_ms: 99,
                log_path: "/tmp/daemon/logs/worker-my-project.log".to_string(),
            }],
        };
        let html = String::from_utf8(render_picker_html(&data).into_owned()).unwrap();
        assert!(html.contains("my-project"));
        assert!(html.contains("/tmp/&lt;workspace&gt;"));
        assert!(html.contains("dot dot-ready"));
        assert!(html.contains("/p/abc123/"));
        assert!(html.contains("\"cron_next_fire_ms\":42"));
        assert!(!html.contains("/tmp/<workspace>"));
    }

    #[tokio::test]
    async fn picker_data_contains_registered_project_status() {
        let test = test_state().await;
        let state = test.state;
        let root = tempfile::tempdir().unwrap();
        let entry = {
            let mut registry = state.projects.write().await;
            registry.open(root.path().to_path_buf()).await.unwrap()
        };
        state.store_worker_status(status_report(&entry.id, 3)).await;
        state.set_cron_pending(&entry.id, Some(12345)).await;

        let data = picker_data(state, 8488).await;
        assert_eq!(data.projects.len(), 1);
        assert_eq!(data.projects[0].id, entry.id);
        assert_eq!(data.projects[0].lsp_clients, 3);
        assert_eq!(data.projects[0].busy_chats, 2);
        assert_eq!(data.projects[0].exec_running, 1);
        assert_eq!(data.projects[0].cron_next_fire_ms, Some(12345));
    }

    #[tokio::test]
    async fn dashboard_index_serves_spa_bootstrap_and_noscript_projects() {
        let test = test_state().await;
        let state = test.state;
        let root = tempfile::Builder::new()
            .prefix("dashboard-project")
            .tempdir()
            .unwrap();
        let entry = {
            let mut registry = state.projects.write().await;
            registry.open(root.path().to_path_buf()).await.unwrap()
        };

        let response = crate::daemon::server::make_router(state, 8488)
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("window.__REFACT_BOOTSTRAP_CONFIG__ = {\"surface\":\"dashboard\"};"));
        assert!(body.contains("<noscript>"));
        assert!(body.contains(&html_escape(&entry.slug)));
        assert!(body.contains(&format!("href=\"/p/{}/\"", entry.id)));
        assert!(body.contains("href=\"/picker\""));
        assert!(body.contains("http://127.0.0.1:8488"));
        assert!(!body.contains(DAEMON_GUI_BOOTSTRAP_SENTINEL));
    }

    #[tokio::test]
    async fn picker_route_serves_legacy_picker() {
        let test = test_state().await;
        let state = test.state;
        let root = tempfile::Builder::new()
            .prefix("picker-project")
            .tempdir()
            .unwrap();
        let entry = {
            let mut registry = state.projects.write().await;
            registry.open(root.path().to_path_buf()).await.unwrap()
        };

        let response = crate::daemon::server::make_router(state, 8488)
            .oneshot(
                Request::builder()
                    .uri("/picker")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("<title>Refact daemon projects</title>"));
        assert!(body.contains(&html_escape(&entry.slug)));
        assert!(body.contains(&format!("href=\"/p/{}/\"", entry.id)));
        assert!(!body.contains("__REFACT_BOOTSTRAP_CONFIG__"));
    }

    #[tokio::test]
    async fn unknown_project_index_returns_404_page() {
        let state = test_state().await.state;
        let response = crate::daemon::server::make_router(state, 8488)
            .oneshot(
                Request::builder()
                    .uri("/p/missing/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("Project not found"));
        assert!(body.contains("Back to projects"));
    }

    #[tokio::test]
    async fn invalid_project_index_returns_escaped_404_page() {
        let state = test_state().await.state;
        let response = crate::daemon::server::make_router(state, 8488)
            .oneshot(
                Request::builder()
                    .uri("/p/%3Cbad%3E/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("&lt;bad&gt;"));
        assert!(!body.contains("<bad>"));
    }

    #[tokio::test]
    async fn auth_enabled_picker_requires_credentials() {
        use axum::extract::ConnectInfo;
        use std::net::SocketAddr;
        let state = test_state_with_auth(Some("secret-token")).await.state;
        let mut request = Request::builder().uri("/").body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([192, 168, 1, 50], 40000))));
        let response = crate::daemon::server::make_router(state, 8488)
            .oneshot(request)
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_matrix_wrong_query_token_rejected_without_cookie_or_redirect() {
        use axum::extract::ConnectInfo;
        use std::net::SocketAddr;

        let state = test_state_with_auth(Some("secret-token")).await.state;
        let router = crate::daemon::server::make_router(state, 8488);
        for uri in [
            "/?daemon_token=wrong-token",
            "/p/abc/?daemon_token=wrong-token",
        ] {
            let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::from(([192, 168, 1, 50], 40000))));
            let response = router.clone().oneshot(request).await.unwrap();

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
            assert!(
                response.headers().get(header::SET_COOKIE).is_none(),
                "{uri}"
            );
            assert!(response.headers().get(header::LOCATION).is_none(), "{uri}");
        }
    }

    #[tokio::test]
    async fn auth_enabled_dist_chat_assets_are_public() {
        let state = test_state_with_auth(Some("secret-token")).await.state;
        let response = crate::daemon::server::make_router(state, 8488)
            .oneshot(
                Request::builder()
                    .uri("/dist/chat/missing.css")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn tokenized_picker_sets_cookie_redirects_and_cookie_allows_next_request() {
        let state = test_state_with_auth(Some("secret-token")).await.state;
        let response = crate::daemon::server::make_router(state.clone(), 8488)
            .oneshot(
                Request::builder()
                    .uri("/?daemon_token=secret-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/");
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(cookie.contains("refact_daemon_auth=secret-token"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));

        let response = crate::daemon::server::make_router(state, 8488)
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::COOKIE, "refact_daemon_auth=secret-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn tokenized_project_gui_sets_cookie_and_redirects() {
        let test = test_state_with_auth(Some("secret-token")).await;
        let state = test.state;
        let root = tempfile::tempdir().unwrap();
        let entry = {
            let mut registry = state.projects.write().await;
            registry.open(root.path().to_path_buf()).await.unwrap()
        };

        let response = crate::daemon::server::make_router(state, 8488)
            .oneshot(
                Request::builder()
                    .uri(format!("/p/{}/?daemon_token=secret-token", entry.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            &format!("/p/{}/", entry.id)
        );
        assert!(response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains(&format!(
                "refact_daemon_auth=project:{}:secret-token",
                entry.id
            )));
        assert!(response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains(&format!("Path=/p/{}/", entry.id)));
    }

    #[tokio::test]
    async fn project_gui_index_upgrades_bearer_to_project_scoped_cookie() {
        let test = test_state_with_auth(Some("secret-token")).await;
        let state = test.state;
        let root = tempfile::tempdir().unwrap();
        let entry = {
            let mut registry = state.projects.write().await;
            registry.open(root.path().to_path_buf()).await.unwrap()
        };

        let response = crate::daemon::server::make_router(state, 8488)
            .oneshot(
                Request::builder()
                    .uri(format!("/p/{}/", entry.id))
                    .header(header::AUTHORIZATION, "Bearer secret-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(cookie.contains(&format!(
            "refact_daemon_auth=project:{}:secret-token",
            entry.id
        )));
        assert!(cookie.contains(&format!("Path=/p/{}/", entry.id)));
    }

    #[tokio::test]
    async fn project_scoped_cookie_cannot_authorize_other_project_or_control_plane() {
        let test = test_state_with_auth(Some("secret-token")).await;
        let state = test.state;
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let (entry_a, entry_b) = {
            let mut registry = state.projects.write().await;
            (
                registry.open(root_a.path().to_path_buf()).await.unwrap(),
                registry.open(root_b.path().to_path_buf()).await.unwrap(),
            )
        };
        let cookie = format!(
            "{}={}",
            crate::daemon::auth::DAEMON_AUTH_COOKIE,
            crate::daemon::auth::project_cookie_value(&entry_a.id, "secret-token")
        );
        let router = crate::daemon::server::make_router(state, 8488);
        let remote =
            || axum::extract::ConnectInfo(std::net::SocketAddr::from(([192, 168, 1, 50], 40000)));

        let mut same_request = Request::builder()
            .uri(format!("/p/{}/", entry_a.id))
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap();
        same_request.extensions_mut().insert(remote());
        let same_project = router.clone().oneshot(same_request).await.unwrap();
        let mut other_request = Request::builder()
            .uri(format!("/p/{}/", entry_b.id))
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap();
        other_request.extensions_mut().insert(remote());
        let other_project = router.clone().oneshot(other_request).await.unwrap();
        let mut control_request = Request::builder()
            .uri("/daemon/v1/projects")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap();
        control_request.extensions_mut().insert(remote());
        let control_plane = router.oneshot(control_request).await.unwrap();

        assert_eq!(same_project.status(), StatusCode::OK);
        assert_eq!(other_project.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(control_plane.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn daemon_asset_route_rejects_invalid_path() {
        let response = handle_daemon_gui_asset(AxumPath("../secret".to_string()), HeaderMap::new())
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn daemon_asset_route_serves_css_and_js_with_content_types_when_embedded() {
        let state = test_state().await.state;
        for (path, content_type) in [
            ("style.css", "text/css; charset=utf-8"),
            ("index.umd.cjs", "text/javascript; charset=utf-8"),
        ] {
            if test_gui_assets(&format!("dist/chat/{path}")).is_none() {
                continue;
            }
            let response = crate::daemon::server::make_router(state.clone(), 8488)
                .oneshot(
                    Request::builder()
                        .uri(format!("/dist/chat/{path}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response
                    .headers()
                    .get(axum::http::header::CONTENT_TYPE)
                    .unwrap(),
                content_type
            );
        }
    }

    #[tokio::test]
    async fn project_index_route_injects_daemon_gui_for_registered_project() {
        let test = test_state().await;
        let state = test.state;
        let root = tempfile::tempdir().unwrap();
        let entry = {
            let mut registry = state.projects.write().await;
            registry.open(root.path().to_path_buf()).await.unwrap()
        };
        let response = crate::daemon::server::make_router(state, 8488)
            .oneshot(
                Request::builder()
                    .uri(format!("/p/{}/", entry.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("daemonProjectApiPrefix"));
        assert!(body.contains("window.__REFACT_DAEMON_PROJECT_API_PREFIX__"));
        assert!(body.contains("window.fetch = function"));
        assert!(body.contains("window.EventSource = function"));
        assert!(body.contains("window.__REFACT_BOOTSTRAP_CONFIG__ = {\"surface\":\"workspace\"};"));
        assert!(body.contains(&format!("/p/{}", entry.id)));
        assert!(body.contains(&format!("http://127.0.0.1:8488/p/{}", entry.id)));
    }

    #[test]
    fn json_for_script_escapes_html_sensitive_chars() {
        let text = json_for_script(&json!({"x": "</script><b>&"}));
        assert!(!text.contains("</script>"));
        assert!(text.contains("\\u003c/script\\u003e"));
        assert!(text.contains("\\u0026"));
    }
}

pub type GuiAssetFn = fn(&str) -> Option<EmbeddedFile>;

pub const INDEX_PATH: &str = "index.html";
pub const ASSET_PREFIX: &str = "dist/chat/";
const CACHE_CONTROL: &str = "no-cache";
const MIN_GZIP_BYTES: usize = 1024;
pub const ORIGIN_CANDIDATES_PLACEHOLDER: &str =
    "/*__REFACT_ORIGIN_CANDIDATES__*/ window.__REFACT_ENGINE_ORIGIN_CANDIDATES__ || []";

static GZIP_CACHE: OnceLock<RwLock<HashMap<String, Option<Bytes>>>> = OnceLock::new();

#[derive(Clone, Debug, Default)]
pub struct GuiPublicOriginCandidates {
    pub origins: Vec<String>,
}

// Both LAN-enumeration helpers below are called only from the non-Windows
// `local_lan_ipv4_hosts`, which returns an empty list on Windows.
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn is_candidate_lan_ipv4(ip: Ipv4Addr) -> bool {
    !(ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || ip.is_unspecified())
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
fn is_likely_virtual_interface_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("docker")
        || name.starts_with("br-")
        || name.starts_with("veth")
        || name.starts_with("virbr")
        || name.starts_with("tun")
        || name.starts_with("tap")
        || name.starts_with("tailscale")
        || name.starts_with("zt")
}

#[cfg(target_os = "windows")]
pub fn local_lan_ipv4_hosts() -> Vec<Ipv4Addr> {
    Vec::new()
}

#[cfg(not(target_os = "windows"))]
pub fn local_lan_ipv4_hosts() -> Vec<Ipv4Addr> {
    let mut preferred = Vec::new();
    let mut fallback = Vec::new();
    for iface in pnet_datalink::interfaces() {
        if iface.is_loopback() || !iface.is_up() {
            continue;
        }
        let virtual_iface = is_likely_virtual_interface_name(&iface.name);
        for network in iface.ips {
            if let std::net::IpAddr::V4(ip) = network.ip() {
                if !is_candidate_lan_ipv4(ip) || preferred.contains(&ip) || fallback.contains(&ip) {
                    continue;
                }
                if virtual_iface {
                    fallback.push(ip);
                } else {
                    preferred.push(ip);
                }
            }
        }
    }
    preferred.extend(fallback);
    preferred
}

pub fn gui_public_origin_candidates(port: u16) -> GuiPublicOriginCandidates {
    let mut origins = Vec::new();
    for ip in local_lan_ipv4_hosts() {
        origins.push(format!("http://{ip}:{port}"));
    }
    let mdns_origin =
        format!("http://{}:{port}", crate::daemon::mdns::local_mdns_browser_host());
    if !origins.contains(&mdns_origin) {
        origins.push(mdns_origin);
    }
    GuiPublicOriginCandidates { origins }
}

pub(crate) fn content_type_for_path(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

pub fn asset_response(
    path: &str,
    body: Cow<'static, [u8]>,
    status: StatusCode,
) -> Response<BoxBody> {
    response_with_body(status, content_type_for_path(path), body)
}

pub fn html_response(status: StatusCode, body: Cow<'static, [u8]>) -> Response<BoxBody> {
    response_with_body(status, "text/html; charset=utf-8", body)
}

pub fn text_response(status: StatusCode, body: String) -> Response<BoxBody> {
    response_with_body(
        status,
        "text/plain; charset=utf-8",
        body.into_bytes().into(),
    )
}

pub type BoxBody = axum::body::BoxBody;

fn response_with_body(
    status: StatusCode,
    content_type: &'static str,
    body: Cow<'static, [u8]>,
) -> Response<BoxBody> {
    response_with_bytes(status, content_type, Bytes::from(body.into_owned()))
}

fn response_with_bytes(
    status: StatusCode,
    content_type: &'static str,
    body: Bytes,
) -> Response<BoxBody> {
    let mut response = Response::new(boxed(Full::from(body)));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    response
}

pub fn missing_gui_index_html() -> &'static str {
    r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Refact GUI assets missing</title>
  </head>
  <body>
    <h1>Refact GUI assets are not bundled in this build.</h1>
    <p>Run <code>cargo build</code> from <code>refact-agent/engine</code> with Node.js and npm available, or set <code>REFACT_SKIP_GUI_BUILD=1</code> only for API-only builds.</p>
  </body>
</html>
"#
}

pub fn embedded_asset_response(
    embedded_path: &str,
    asset: EmbeddedFile,
    request_headers: &HeaderMap,
) -> Response<BoxBody> {
    let etag = asset_etag(&asset);

    if etag_matches(request_headers, &etag) {
        let mut response = Response::new(boxed(Full::from(Bytes::new())));
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        apply_validation_headers(response.headers_mut(), &etag);
        return response;
    }

    let compressed = if accepts_gzip(request_headers) {
        gzip_asset(embedded_path, asset.data.as_ref())
    } else {
        None
    };

    let content_type = content_type_for_path(embedded_path);
    let is_gzipped = compressed.is_some();
    let body = compressed.unwrap_or_else(|| Bytes::from(asset.data.into_owned()));

    let mut response = response_with_bytes(StatusCode::OK, content_type, body);
    if is_gzipped {
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    apply_validation_headers(response.headers_mut(), &etag);
    response
}

fn apply_validation_headers(headers: &mut HeaderMap, etag: &str) {
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
}

pub fn asset_etag(asset: &EmbeddedFile) -> String {
    let mut etag = String::with_capacity(66);
    etag.push('"');
    for byte in asset.metadata.sha256_hash() {
        let _ = write!(etag, "{byte:02x}");
    }
    etag.push('"');
    etag
}

pub fn etag_matches(request_headers: &HeaderMap, etag: &str) -> bool {
    request_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|candidate| {
                let candidate = candidate.trim();
                candidate == "*"
                    || candidate == etag
                    || candidate
                        .strip_prefix("W/")
                        .is_some_and(|weak| weak == etag)
            })
        })
}

pub fn accepts_gzip(request_headers: &HeaderMap) -> bool {
    request_headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|encoding| {
                encoding
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case("gzip")
            })
        })
}

fn gzip_cache() -> &'static RwLock<HashMap<String, Option<Bytes>>> {
    GZIP_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn gzip_asset(embedded_path: &str, data: &[u8]) -> Option<Bytes> {
    if data.len() < MIN_GZIP_BYTES {
        return None;
    }

    if let Ok(cache) = gzip_cache().read() {
        if let Some(cached) = cache.get(embedded_path) {
            return cached.clone();
        }
    }

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let compressed = encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .ok()
        .filter(|compressed| compressed.len() < data.len())
        .map(Bytes::from);

    if let Ok(mut cache) = gzip_cache().write() {
        cache.insert(embedded_path.to_string(), compressed.clone());
    }
    compressed
}

pub fn inject_gui_origin_candidates(
    body: Cow<'static, [u8]>,
    candidates: &GuiPublicOriginCandidates,
) -> Cow<'static, [u8]> {
    let Ok(html) = std::str::from_utf8(body.as_ref()) else {
        return body;
    };
    let Ok(json) = serde_json::to_string(&candidates.origins) else {
        return body;
    };
    if html.contains(ORIGIN_CANDIDATES_PLACEHOLDER) {
        Cow::Owned(
            html.replace(ORIGIN_CANDIDATES_PLACEHOLDER, &json)
                .into_bytes(),
        )
    } else {
        tracing::warn!("GUI origin candidates placeholder missing; serving index.html unchanged");
        body
    }
}
#[cfg(test)]
mod gui_asset_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[test]
    fn content_type_maps_common_gui_assets() {
        assert_eq!(
            content_type_for_path("index.html"),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type_for_path("index.umd.cjs"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type_for_path("style.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(
            content_type_for_path("manifest.json"),
            "application/json; charset=utf-8"
        );
        assert_eq!(content_type_for_path("font.woff2"), "font/woff2");
    }

    fn headers_with(name: header::HeaderName, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn accepts_gzip_reads_quality_values_and_ignores_other_encodings() {
        assert!(accepts_gzip(&headers_with(
            header::ACCEPT_ENCODING,
            "gzip, deflate, br"
        )));
        assert!(accepts_gzip(&headers_with(
            header::ACCEPT_ENCODING,
            "br;q=1.0, GZIP;q=0.8"
        )));
        assert!(!accepts_gzip(&headers_with(
            header::ACCEPT_ENCODING,
            "br, deflate"
        )));
        assert!(!accepts_gzip(&HeaderMap::new()));
    }

    #[test]
    fn etag_matches_handles_wildcard_weak_and_lists() {
        let etag = "\"abc123\"";
        assert!(etag_matches(
            &headers_with(header::IF_NONE_MATCH, "*"),
            etag
        ));
        assert!(etag_matches(
            &headers_with(header::IF_NONE_MATCH, "\"abc123\""),
            etag
        ));
        assert!(etag_matches(
            &headers_with(header::IF_NONE_MATCH, "W/\"abc123\""),
            etag
        ));
        assert!(etag_matches(
            &headers_with(header::IF_NONE_MATCH, "\"other\", \"abc123\""),
            etag
        ));
        assert!(!etag_matches(
            &headers_with(header::IF_NONE_MATCH, "\"other\""),
            etag
        ));
        assert!(!etag_matches(&HeaderMap::new(), etag));
    }

    #[test]
    fn gzip_asset_skips_payloads_below_threshold() {
        let small = vec![b'a'; MIN_GZIP_BYTES - 1];
        assert!(gzip_asset("test/small.js", &small).is_none());
    }

    #[test]
    fn gzip_asset_compresses_and_serves_cached_bytes() {
        let data = "console.log('refact');".repeat(500).into_bytes();
        assert!(data.len() >= MIN_GZIP_BYTES);

        let first = gzip_asset("test/compressible.js", &data).expect("compressible payload");
        assert!(first.len() < data.len());

        let second = gzip_asset("test/compressible.js", &data).expect("cached payload");
        assert_eq!(first, second);
    }

    #[test]
    fn gzip_asset_rejects_incompressible_payloads() {
        let data: Vec<u8> = (0..8192u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        let compressed = gzip_asset("test/random.bin", &data);
        if let Some(compressed) = compressed {
            assert!(compressed.len() < data.len());
        }
    }

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    struct SharedWriterGuard(Arc<Mutex<Vec<u8>>>);

    impl<'a> MakeWriter<'a> for SharedWriter {
        type Writer = SharedWriterGuard;

        fn make_writer(&'a self) -> Self::Writer {
            SharedWriterGuard(self.0.clone())
        }
    }

    impl std::io::Write for SharedWriterGuard {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn origin_candidates_missing_marker_warns_and_returns_unchanged() {
        let html = r#"<html><head></head><body></body></html>"#;
        let candidates = GuiPublicOriginCandidates {
            origins: vec!["http://127.0.0.1:8001".to_string()],
        };
        let logs = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_writer(SharedWriter(logs.clone()))
            .with_ansi(false)
            .finish();

        let injected = tracing::subscriber::with_default(subscriber, || {
            inject_gui_origin_candidates(Cow::Borrowed(html.as_bytes()), &candidates)
        });

        assert_eq!(injected.as_ref(), html.as_bytes());
        let logs = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("GUI origin candidates placeholder missing"));
    }
}
