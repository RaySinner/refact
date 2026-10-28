use axum::extract::Path;
use axum::http::{header, HeaderMap, StatusCode};
use axum::Extension;
use axum::response::IntoResponse;
use rust_embed::{EmbeddedFile, RustEmbed};

use crate::daemon::web::{
    asset_etag, asset_response, embedded_asset_response, gzip_asset, html_response,
    inject_gui_origin_candidates, missing_gui_index_html, text_response, ASSET_PREFIX, INDEX_PATH,
    MIN_GZIP_BYTES, ORIGIN_CANDIDATES_PLACEHOLDER,
};
use crate::http::GuiPublicOriginCandidates;

#[derive(RustEmbed)]
#[folder = "assets/chat/"]
#[exclude = "*.map"]
pub(crate) struct ChatGuiAsset;

pub(crate) fn chat_gui_asset_get(path: &str) -> Option<EmbeddedFile> {
    ChatGuiAsset::get(path)
}

pub async fn handle_gui_index(
    Extension(candidates): Extension<GuiPublicOriginCandidates>,
) -> impl IntoResponse {
    match ChatGuiAsset::get(INDEX_PATH) {
        Some(asset) => {
            let body = inject_gui_origin_candidates(asset.data, &candidates);
            asset_response(INDEX_PATH, body, StatusCode::OK)
        }
        None => html_response(
            StatusCode::SERVICE_UNAVAILABLE,
            missing_gui_index_html().as_bytes().to_vec().into(),
        ),
    }
}

pub async fn handle_gui_asset(
    Path(path): Path<String>,
    request_headers: HeaderMap,
) -> impl IntoResponse {
    if path.is_empty() || path.split('/').any(|part| part == ".." || part.is_empty()) {
        return text_response(
            StatusCode::BAD_REQUEST,
            "invalid GUI asset path".to_string(),
        );
    }

    let embedded_path = format!("{ASSET_PREFIX}{path}");
    match ChatGuiAsset::get(&embedded_path) {
        Some(asset) => embedded_asset_response(&embedded_path, asset, &request_headers),
        None => text_response(
            StatusCode::NOT_FOUND,
            format!("GUI asset not found: {path}"),
        ),
    }
}

pub async fn handle_favicon() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with(name: header::HeaderName, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_str(value).unwrap());
        headers
    }

    fn largest_embedded_js_asset() -> String {
        ChatGuiAsset::iter()
            .filter(|path| {
                path.ends_with(".js") || path.ends_with(".cjs") || path.ends_with(".css")
            })
            .filter_map(|path| {
                let size = ChatGuiAsset::get(path.as_ref())?.data.len();
                (size >= MIN_GZIP_BYTES).then(|| (size, path.to_string()))
            })
            .max()
            .map(|(_, path)| path)
            .expect("embedded GUI assets are required to verify asset negotiation")
    }

    #[test]
    fn embedded_asset_response_negotiates_gzip_and_revalidation() {
        let path = largest_embedded_js_asset();
        let asset = ChatGuiAsset::get(&path).expect("asset exists");
        let raw_len = asset.data.len();
        let etag = asset_etag(&asset);
        assert!(raw_len >= MIN_GZIP_BYTES);

        let plain = embedded_asset_response(&path, asset, &HeaderMap::new());
        assert_eq!(plain.status(), StatusCode::OK);
        assert!(!plain.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(plain.headers().get(header::ETAG).unwrap(), etag.as_str());
        assert_eq!(
            plain.headers().get(header::VARY).unwrap(),
            "Accept-Encoding"
        );

        let asset = ChatGuiAsset::get(&path).expect("asset exists");
        let gzipped =
            embedded_asset_response(&path, asset, &headers_with(header::ACCEPT_ENCODING, "gzip"));
        assert_eq!(gzipped.status(), StatusCode::OK);
        assert_eq!(
            gzipped.headers().get(header::CONTENT_ENCODING).unwrap(),
            "gzip"
        );

        let compressed = ChatGuiAsset::get(&path)
            .and_then(|asset| gzip_asset(&path, asset.data.as_ref()))
            .expect("text asset compresses");
        assert!(compressed.len() < raw_len);

        let asset = ChatGuiAsset::get(&path).expect("asset exists");
        let not_modified =
            embedded_asset_response(&path, asset, &headers_with(header::IF_NONE_MATCH, &etag));
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            not_modified.headers().get(header::ETAG).unwrap(),
            etag.as_str()
        );
        assert_ne!(etag, "\"\"");
    }

    #[test]
    fn origin_candidates_inject_into_real_embedded_index() {
        let asset = ChatGuiAsset::get(INDEX_PATH).expect("embedded index.html");
        let candidates = GuiPublicOriginCandidates {
            origins: vec![
                "http://127.0.0.1:8001".to_string(),
                "http://workstation.local:8001".to_string(),
            ],
        };

        let injected = inject_gui_origin_candidates(asset.data, &candidates);
        let html = std::str::from_utf8(injected.as_ref()).unwrap();

        assert!(html.contains("http://127.0.0.1:8001"));
        assert!(html.contains("http://workstation.local:8001"));
        assert!(!html.contains(ORIGIN_CANDIDATES_PLACEHOLDER));
        assert!(!html.contains("window.__REFACT_ENGINE_ORIGIN_CANDIDATES__ || []"));

        let reinjected = inject_gui_origin_candidates(injected.clone(), &candidates);
        assert_eq!(reinjected.as_ref(), injected.as_ref());
    }

}
