// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! The web interface, embedded at build time (see `build.rs`): every path that isn't the API gets a
//! file of it, and unknown paths get `index.html` (the interface routes on the client).

use std::sync::LazyLock;

use axum::http::{HeaderMap, HeaderValue, Uri, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;

use crate::error::ApiError;
use crate::http::{Data, respond};

include!(concat!(env!("OUT_DIR"), "/web_files.rs"));

/// The interface talks only to this server; images, sound and styles come from it too.
const CSP: &str = "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self' 'unsafe-inline'; \
                   script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// Content hashes (FNV-1a) for the `ETag`s, computed once.
static HASHES: LazyLock<Vec<u32>> =
    LazyLock::new(|| WEB_FILES.iter().map(|(_, data)| data.iter().fold(0x811c_9dc5u32, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))).collect());

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// `GET` of anything outside `/api` and `/health`.
pub async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return ApiError::not_found(format!("API endpoint /{path}")).into_response();
    }
    let found = WEB_FILES.iter().position(|(name, _)| *name == path).filter(|_| !path.is_empty());
    let Some(index) = found.or_else(|| WEB_FILES.iter().position(|(name, _)| *name == "index.html")) else {
        return ApiError::not_found("web interface").into_response();
    };
    let (name, data) = WEB_FILES[index];
    let data = Data { bytes: Bytes::from_static(data), mime: mime(name), etag: format!("\"{:08x}\"", HASHES[index]), damaged: None };
    // Built assets have content hashes in their names — cache them for good; the rest is revalidated.
    let mut response = respond(&headers, data, name.starts_with("assets/"));
    if name.ends_with(".html") {
        let h = response.headers_mut();
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
        h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    }
    response
}
