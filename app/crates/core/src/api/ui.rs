//! Local Vue assets and history-mode routes. Unknown API paths never become HTML.
use super::ApiError;
use axum::{
    body::Body,
    extract::Request,
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

fn content_type(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()? {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        _ => return None,
    })
}

pub async fn serve(dist: Option<PathBuf>, request: Request) -> Response {
    let missing = || ApiError::new(404, "Not Found").into_response();
    let Some(dist) = dist else {
        return missing();
    };
    let path = request.uri().path();
    if path == "/api" || path.starts_with("/api/") {
        return missing();
    }
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "GET, HEAD")]).into_response();
    }
    // Reject encoded separators/dot segments rather than decoding them ambiguously. Vue routes
    // only need their query string; no file path needs a percent escape, colon or backslash.
    if path.contains(['%', '\\', ':', '\0']) || path.split('/').any(|part| part.starts_with('.')) {
        return missing();
    }
    let relative = path.trim_start_matches('/');
    let candidate = dist.join(relative);
    if crate::paths::linked(&candidate).is_some() {
        return missing();
    }
    let is_asset = path.starts_with("/assets/");
    let file = if !relative.is_empty() && candidate.is_file() {
        candidate
    } else if is_asset || Path::new(relative).extension().is_some() {
        return missing();
    } else {
        dist.join("index.html")
    };
    if crate::paths::linked(&file).is_some() {
        return missing();
    }
    let Some(mime) = content_type(&file) else {
        return missing();
    };
    // A bounded read completes before responding, so Content-Length and HEAD match exactly.
    let Ok(file) = tokio::fs::File::open(&file).await else {
        return missing();
    };
    let Ok(metadata) = file.metadata().await else {
        return missing();
    };
    if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
        return missing();
    }
    let mut bytes = Vec::new();
    if file.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes).await.is_err() || bytes.len() > 32 * 1024 * 1024 {
        return missing();
    }
    let length = bytes.len();
    let body = if request.method() == Method::HEAD { Body::empty() } else { Body::from(bytes) };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, length)
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(body)
        .unwrap()
}
