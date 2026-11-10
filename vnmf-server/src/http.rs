// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Serving data from a book: conditional requests (`ETag`), byte ranges (audio players seek with
//! them) and caching.
//!
//! Caching: a URL with `?v=<book version>` (from the book info) is immutable — the version changes with
//! the file — so it's cached for a year; without it the client revalidates with the `ETag`.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;

/// Data to send.
pub struct Data {
    pub bytes: Bytes,
    pub mime: &'static str,
    /// Identifies the contents (a CRC from the file plus the length).
    pub etag: String,
    /// The data failed its CRC check: still sent (a damaged page is better than none), but marked.
    pub damaged: Option<bool>,
}

impl Data {
    pub fn new(bytes: Bytes, mime: &'static str, crc: u32) -> Self {
        let etag = format!("\"{crc:08x}-{:x}\"", bytes.len());
        Self { bytes, mime, etag, damaged: None }
    }
}

/// Builds the response for `data` according to the request headers. `immutable` — the URL carried the
/// current book version.
pub fn respond(headers: &HeaderMap, data: Data, immutable: bool) -> Response {
    let cache = if immutable { "private, max-age=31536000, immutable" } else { "private, no-cache" };
    let mut base = Response::builder()
        .header(header::ETAG, &data.etag)
        .header(header::CACHE_CONTROL, cache)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    if let Some(damaged) = data.damaged {
        base = base.header("x-vnmf-crc", if damaged { "bad" } else { "ok" });
    }

    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|t| t.trim() == data.etag || t.trim() == "*")) {
        return base.status(StatusCode::NOT_MODIFIED).body(Body::empty()).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    let len = data.bytes.len() as u64;
    // Units other than bytes are ignored (RFC 9110), an unsatisfiable byte range is 416.
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok()).filter(|v| v.starts_with("bytes=")).map(|v| parse_range(v, len));
    // `If-Range` with a different validator: send the whole thing.
    let range = range.filter(|_| headers.get(header::IF_RANGE).is_none_or(|v| v.to_str().is_ok_and(|v| v == data.etag)));
    let response = match range {
        Some(Some((start, end))) => base
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, data.mime)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
            .body(Body::from(data.bytes.slice(start as usize..=end as usize))),
        Some(None) => base.status(StatusCode::RANGE_NOT_SATISFIABLE).header(header::CONTENT_RANGE, format!("bytes */{len}")).body(Body::empty()),
        None => base.status(StatusCode::OK).header(header::CONTENT_TYPE, data.mime).body(Body::from(data.bytes)),
    };
    response.unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// One range `bytes=a-b`, `bytes=a-`, `bytes=-n` → inclusive `(start, end)`; `None` — unsatisfiable.
/// Several ranges aren't supported: the first one is served.
fn parse_range(value: &str, len: u64) -> Option<(u64, u64)> {
    let spec = value.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    if len == 0 {
        return None;
    }
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            if n == 0 {
                return None;
            }
            (len.saturating_sub(n), len - 1)
        }
        (a, "") => (a.parse().ok()?, len - 1),
        (a, b) => (a.parse().ok()?, b.parse::<u64>().ok()?.min(len - 1)),
    };
    (start <= end && start < len).then_some((start, end))
}

/// `Content-Disposition: attachment` with an ASCII fallback and the UTF-8 name (RFC 6266).
pub fn attachment(name: &str) -> HeaderValue {
    let ascii: String = name.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' }).collect();
    let encoded: String =
        name.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    HeaderValue::from_str(&format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")).unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 9)));
        assert_eq!(parse_range("bytes=90-", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=50-500", 100), Some((50, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=5-1", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
    }

    #[test]
    fn content_disposition() {
        assert_eq!(attachment("Том 1.cbz").to_str().unwrap(), "attachment; filename=\"___ 1.cbz\"; filename*=UTF-8''%D0%A2%D0%BE%D0%BC%201.cbz");
    }
}
