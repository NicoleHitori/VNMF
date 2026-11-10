// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Reading a book: its structure and every kind of data the format holds.

use std::path::PathBuf;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::services::ServeFile;
use vnmf_core::{AuxFormat, InfoFormat, PageFormat};

use super::{Signed, Versioned, open_book};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::http::{Data, attachment, respond};

/// `GET /books/{id}/manifest`: everything a reader needs to start, in one request.
pub async fn manifest(State(state): State<AppState>, _: Signed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (_, book) = open_book(&state, id).await?;
    Ok(Json(json!({
        "id": id,
        "info": book.info(),
        "contents": book.contents(),
        "pages": book.pages(),
        "aux": book.aux(),
        "covers": book.covers(),
    })))
}

pub async fn contents(State(state): State<AppState>, _: Signed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (_, book) = open_book(&state, id).await?;
    Ok(Json(json!(book.contents())))
}

pub async fn pages(State(state): State<AppState>, _: Signed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (_, book) = open_book(&state, id).await?;
    Ok(Json(json!({ "pages": book.pages() })))
}

pub async fn aux_list(State(state): State<AppState>, _: Signed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (_, book) = open_book(&state, id).await?;
    Ok(Json(json!({ "aux": book.aux() })))
}

pub async fn strip_sfx(State(state): State<AppState>, _: Signed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (_, book) = open_book(&state, id).await?;
    Ok(Json(json!({ "sfx": book.strip_sfx() })))
}

pub async fn covers(State(state): State<AppState>, _: Signed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (_, book) = open_book(&state, id).await?;
    Ok(Json(json!({ "covers": book.covers() })))
}

#[derive(Deserialize)]
pub struct GroupsQuery {
    /// Spreads: pair the pages (default `true`).
    double: Option<bool>,
    /// The first page and volume covers alone (default `true`).
    cover_alone: Option<bool>,
    /// Right to left (default — as the book says).
    rtl: Option<bool>,
}

/// `GET /books/{id}/groups`: how pages are shown in paged mode.
pub async fn groups(State(state): State<AppState>, _: Signed, Path(id): Path<i64>, Query(q): Query<GroupsQuery>) -> ApiResult<Json<Value>> {
    let (_, book) = open_book(&state, id).await?;
    let rtl = q.rtl.unwrap_or_else(|| book.rtl());
    Ok(Json(json!({ "rtl": rtl, "groups": book.groups(q.double.unwrap_or(true), q.cover_alone.unwrap_or(true), rtl) })))
}

pub async fn playback(State(state): State<AppState>, _: Signed, Path((id, n)): Path<(i64, u32)>) -> ApiResult<Json<Value>> {
    let (_, book) = open_book(&state, id).await?;
    let page = book.page(n)?;
    Ok(Json(json!(book.playback(page))))
}

pub async fn text(State(state): State<AppState>, _: Signed, Path((id, n)): Path<(i64, u32)>) -> ApiResult<Json<Value>> {
    let (_, book) = open_book(&state, id).await?;
    let page = book.page(n)?;
    Ok(Json(json!(book.text(page))))
}

/// An image chunk as a response, with its CRC state.
fn chunk_response(headers: &HeaderMap, book: &std::sync::Arc<crate::book::OpenBook>, chunk: usize, immutable: bool) -> ApiResult<Response> {
    let data = book.reader.get_chunk_data(chunk).map_err(|_| ApiError::not_found(format!("chunk {chunk}")))?;
    let crc = book.reader.chunks()[chunk].crc();
    let mime = PageFormat::detect(data).mime_type();
    let mut out = Data::new(book.bytes_of(data), mime, crc);
    out.damaged = Some(!book.reader.verify_chunk_crc(chunk).unwrap_or(false));
    Ok(respond(headers, out, immutable))
}

/// `GET /books/{id}/chunks/{n}`: image bytes as stored (no recompression). `X-VNMF-CRC: bad` — damaged.
pub async fn chunk(
    State(state): State<AppState>,
    _: Signed,
    Path((id, n)): Path<(i64, u32)>,
    Query(v): Query<Versioned>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (_, book) = open_book(&state, id).await?;
    chunk_response(&headers, &book, n as usize, v.immutable(&book))
}

/// `GET /books/{id}/chunks/{n}/tiles/{t}`: a JPEG tile of a fragment taller than browsers show
/// (`tiles` in the manifest). Cut on the first request, then served from the cache.
pub async fn chunk_tile(
    State(state): State<AppState>,
    _: Signed,
    Path((id, n, t)): Path<(i64, u32, u32)>,
    Query(v): Query<Versioned>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (_, book) = open_book(&state, id).await?;
    let bytes = crate::tiles::tile(&state, &book, n as usize, t).await?;
    let crc = book.reader.chunks().get(n as usize).map_or(0, |c| c.crc()) ^ t;
    Ok(respond(&headers, Data::new(bytes, "image/jpeg", crc), v.immutable(&book)))
}

#[derive(Deserialize)]
pub struct PartQuery {
    /// Which image of a multi-image (webtoon) page; 0 by default.
    part: Option<u32>,
    v: Option<String>,
}

/// `GET /books/{id}/pages/{n}/image?part=k`
pub async fn page_image(
    State(state): State<AppState>,
    _: Signed,
    Path((id, n)): Path<(i64, u32)>,
    Query(q): Query<PartQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (_, book) = open_book(&state, id).await?;
    let page = book.page(n)?;
    let range = book.reader.pages()[page].chunk_range();
    let part = q.part.unwrap_or(0) as usize;
    if part >= range.len() {
        return Err(ApiError::not_found(format!("image {part} of page {n}")));
    }
    let immutable = q.v.as_deref() == Some(book.version.as_str());
    chunk_response(&headers, &book, range.start + part, immutable)
}

/// An attachment as a response.
fn aux_response(headers: &HeaderMap, book: &std::sync::Arc<crate::book::OpenBook>, k: usize, immutable: bool) -> ApiResult<Response> {
    let rec = book.reader.aux().get(k).ok_or_else(|| ApiError::not_found(format!("attachment {k}")))?;
    let data = book.reader.get_aux_data(k).map_err(|e| ApiError::unprocessable("damaged", e.to_string()))?;
    let crc = book.reader.chunks().get(rec.chunk_idx.get() as usize).map_or(0, |c| c.crc());
    let mut out = Data::new(book.bytes_of(data), AuxFormat::detect(data).mime_type(), crc);
    out.damaged = Some(!book.reader.verify_aux_crc(k).unwrap_or(false));
    Ok(respond(headers, out, immutable))
}

/// `GET /books/{id}/pages/{n}/dark/{part}`: the dark version of a page image.
pub async fn dark(
    State(state): State<AppState>,
    _: Signed,
    Path((id, n, part)): Path<(i64, u32, u32)>,
    Query(v): Query<Versioned>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (_, book) = open_book(&state, id).await?;
    let page = book.page(n)?;
    let k = book.dark_of(page, part as usize).ok_or_else(|| ApiError::not_found(format!("dark version of image {part} of page {n}")))?;
    aux_response(&headers, &book, k, v.immutable(&book))
}

/// `GET /books/{id}/aux/{k}`: an attachment's bytes (music, an effect, a text layer, a dark image, a file).
pub async fn aux_data(
    State(state): State<AppState>,
    _: Signed,
    Path((id, k)): Path<(i64, u32)>,
    Query(v): Query<Versioned>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (_, book) = open_book(&state, id).await?;
    aux_response(&headers, &book, k as usize, v.immutable(&book))
}

/// `GET /books/{id}/cover`: the main preview, otherwise the first page image.
pub async fn main_cover(State(state): State<AppState>, _: Signed, Path(id): Path<i64>, Query(v): Query<Versioned>, headers: HeaderMap) -> ApiResult<Response> {
    let (_, book) = open_book(&state, id).await?;
    let immutable = v.immutable(&book);
    if book.reader.previews_count() > 0 {
        return preview_response(&headers, &book, 0, immutable);
    }
    if book.reader.chunks_count() == 0 {
        return Err(ApiError::not_found("cover"));
    }
    chunk_response(&headers, &book, 0, immutable)
}

fn preview_response(headers: &HeaderMap, book: &std::sync::Arc<crate::book::OpenBook>, i: usize, immutable: bool) -> ApiResult<Response> {
    let data = book.reader.get_preview_data(i).map_err(|_| ApiError::not_found(format!("cover {i}")))?;
    let crc = book.reader.previews()[i].crc.get();
    let mut out = Data::new(book.bytes_of(data), PageFormat::detect(data).mime_type(), crc);
    out.damaged = Some(!book.reader.verify_preview_crc(i).unwrap_or(false));
    Ok(respond(headers, out, immutable))
}

pub async fn cover(
    State(state): State<AppState>,
    _: Signed,
    Path((id, i)): Path<(i64, u32)>,
    Query(v): Query<Versioned>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (_, book) = open_book(&state, id).await?;
    preview_response(&headers, &book, i as usize, v.immutable(&book))
}

/// `GET /books/{id}/metadata`: `vnmf-metadata/1` (converted from ComicInfo / format 1.1 if that's what the
/// file has); `null` if the Info section is something else (see `/info`).
pub async fn metadata(State(state): State<AppState>, _: Signed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (_, book) = open_book(&state, id).await?;
    let info = book.reader.info_bytes();
    Ok(Json(json!({
        "schema": vnmf::metadata::SCHEMA,
        "info_format": info.map(|i| InfoFormat::detect(i).extension()),
        "metadata": book.metadata.as_ref().map(|m| serde_json::to_value(m).unwrap_or(Value::Null)),
    })))
}

/// `GET /books/{id}/info`: the Info section exactly as stored.
pub async fn raw_info(State(state): State<AppState>, _: Signed, Path(id): Path<i64>, headers: HeaderMap) -> ApiResult<Response> {
    let (_, book) = open_book(&state, id).await?;
    let info = book.reader.info_bytes().ok_or_else(|| ApiError::not_found("Info section"))?;
    let mime = match InfoFormat::detect(info) {
        InfoFormat::Json => "application/json",
        InfoFormat::Xml => "application/xml",
        InfoFormat::Toml => "application/toml",
        InfoFormat::Text => "text/plain; charset=utf-8",
        InfoFormat::Binary => "application/octet-stream",
    };
    Ok(respond(&headers, Data::new(book.bytes_of(info), mime, book.reader.header().info.crc()), false))
}

/// `GET /books/{id}/comicinfo.xml`: ComicInfo for other software, generated from the metadata.
pub async fn comicinfo(State(state): State<AppState>, _: Signed, Path(id): Path<i64>) -> ApiResult<Response> {
    let (_, book) = open_book(&state, id).await?;
    let info = book.reader.info_bytes().ok_or_else(|| ApiError::not_found("metadata"))?;
    let xml = match vnmf::export::comicinfo_file(info, &book.reader)? {
        Some((_, xml)) => xml,
        None if InfoFormat::detect(info) == InfoFormat::Xml => info.to_vec(),
        None => return Err(ApiError::not_found("VNMF metadata")),
    };
    Ok(([(header::CONTENT_TYPE, "application/xml")], xml).into_response())
}

/// A file name for downloads: the title, otherwise the file's own name.
fn download_name(book: &crate::book::OpenBook, ext: &str) -> String {
    let stem = book.title().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| vnmf::input::file_stem(&book.path));
    let clean: String = stem.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    format!("{}.{ext}", clean.trim())
}

async fn serve_file(path: PathBuf, name: &str, mime: &'static str, request: Request) -> ApiResult<Response> {
    let response = ServeFile::new(&path).try_call(request).await?;
    let mut response = response.map(Body::new);
    if response.status().is_success() {
        response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
        response.headers_mut().insert(header::CONTENT_DISPOSITION, attachment(name));
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    }
    Ok(response)
}

/// `GET /books/{id}/file`: the `.vnmf` itself (resumable: `Range`, `If-Range`).
pub async fn file(State(state): State<AppState>, _: Signed, Path(id): Path<i64>, request: Request) -> ApiResult<Response> {
    let (row, book) = open_book(&state, id).await?;
    serve_file(row.path, &download_name(&book, "vnmf"), "application/x-vnmf", request).await
}

#[derive(Deserialize)]
pub struct ExportQuery {
    /// `cbz` — images and ComicInfo.xml for other readers (default); `zip` — the full `vnmfunpack`
    /// layout with attachments, covers and `vnmf.json` (`vnmfpack` packs it back losslessly);
    /// `vnmf` — the file itself.
    format: Option<String>,
}

/// `GET /books/{id}/export?format=cbz|zip|vnmf`
pub async fn export(State(state): State<AppState>, _: Signed, Path(id): Path<i64>, Query(q): Query<ExportQuery>, request: Request) -> ApiResult<Response> {
    let (row, book) = open_book(&state, id).await?;
    let (full, ext, mime) = match q.format.as_deref().unwrap_or("cbz") {
        "cbz" => (false, "cbz", "application/vnd.comicbook+zip"),
        "zip" => (true, "zip", "application/zip"),
        "vnmf" => return serve_file(row.path, &download_name(&book, "vnmf"), "application/x-vnmf", request).await,
        other => return Err(ApiError::bad_request(format!("unknown export format {other}"))),
    };
    let dir = state.config.export_dir();
    let target = dir.join(format!("{id}-{}.{ext}", book.version));
    if !target.exists() {
        let (book2, target2) = (book.clone(), target.clone());
        tokio::task::spawn_blocking(move || build_export(&book2, &dir, id, &target2, full)).await??;
    }
    serve_file(target, &download_name(&book, ext), mime, request).await
}

/// Writes an export under a unique temporary name and renames it into place; older exports of the
/// book are removed.
fn build_export(book: &crate::book::OpenBook, dir: &std::path::Path, id: i64, target: &std::path::Path, full: bool) -> ApiResult<()> {
    use vnmf::unpack::{Sink, ZipWriterSink, unpack};
    let tmp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> ApiResult<()> {
        let file = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        let mut sink = ZipWriterSink::new(file);
        unpack(&book.reader, &mut sink as &mut dyn Sink, full, true).map_err(|e| ApiError::unprocessable("damaged", format!("{e:#}")))?;
        let mut file = sink.into_inner().map_err(ApiError::from)?;
        std::io::Write::flush(&mut file)?;
        file.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        let prefix = format!("{id}-");
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    std::fs::rename(&tmp, target)?;
    Ok(())
}
