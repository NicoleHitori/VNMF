// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Changing a book's file (administrators; files in the own library, or anywhere with
//! `--writable-libraries`): metadata, covers, protection, repair. And the integrity check.
//!
//! A change rewrites the file next to itself and renames it into place (`vnmf::rewrite`): UUID, pages,
//! attachments and structure stay exactly as they were, so reading progress stays valid.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, header};
use serde::Deserialize;
use serde_json::{Value, json};
use vnmf::metadata::Metadata;
use vnmf::rewrite::{self, Changes};
use vnmf_core::{BlockSize, ChunkSource, PageFormat};

use super::books::book_summary;
use super::{Signed, open_book, require_writable};
use crate::AppState;
use crate::auth::Admin;
use crate::book::OpenBook;
use crate::error::{ApiError, ApiResult};
use crate::library::{self, book_row};

/// Rewrites book `id` with the changes `make` computes from the open book; returns the new summary.
async fn rewrite_book<F>(state: &AppState, user_id: i64, id: i64, make: F) -> ApiResult<Value>
where
    F: FnOnce(&OpenBook) -> ApiResult<Changes> + Send + 'static,
{
    let (row, book) = open_book(state, id).await?;
    require_writable(state, &row)?;
    let _busy = state.lock_book(id).ok_or_else(ApiError::busy)?;
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        let changes = make(&book)?;
        drop(book);
        rewrite::rewrite(&row.path, &row.path, changes).map_err(|e| ApiError::unprocessable("rewrite_failed", format!("{e:#}")))?;
        st.books.forget(id);
        library::index_file(&st, &row.path)
    })
    .await??;
    let writable_libraries = state.config.writable_libraries;
    state.db.call(move |c| book_summary(c, id, user_id, writable_libraries)).await
}

/// `PUT /books/{id}/metadata`: `vnmf-metadata/1` JSON, or ComicInfo (XML, or its JSON form) which is
/// converted. Without a `contents` field, the volume and chapter titles already in the file are kept.
/// `reading_direction` also sets the direction flag of every page.
pub async fn put_metadata(State(state): State<AppState>, Admin(admin): Admin, Path(id): Path<i64>, headers: HeaderMap, body: Bytes) -> ApiResult<Json<Value>> {
    let xml = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.contains("xml")) || body.trim_ascii_start().starts_with(b"<");
    let (mut meta, has_contents) = if xml {
        (Metadata::from_info(Some(&body)).ok_or_else(|| ApiError::bad_request("not a ComicInfo.xml"))?, false)
    } else {
        let value: Value = serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
        let mut value = value;
        // A client that edits the fields may leave the schema out.
        if let Some(obj) = value.as_object_mut()
            && !obj.contains_key("schema")
            && !obj.contains_key("ComicInfo")
        {
            obj.insert("schema".into(), json!(vnmf::metadata::SCHEMA));
        }
        let has_contents = value.get("contents").is_some();
        (Metadata::from_json(&value).ok_or_else(|| ApiError::bad_request("not vnmf-metadata/1 or ComicInfo"))?, has_contents)
    };
    let rtl = meta.reading_direction.as_deref().map(|d| d == "rtl");
    let summary = rewrite_book(&state, admin.id, id, move |book| {
        if !has_contents && let Some(old) = &book.metadata {
            meta.contents = old.contents.clone();
        }
        let info = (!meta.is_empty()).then(|| meta.to_bytes());
        Ok(Changes { info: Some(info), rtl, ..Default::default() })
    })
    .await?;
    Ok(Json(summary))
}

/// The current previews as sources (copied: they're small) with their volumes.
fn current_previews(book: &OpenBook) -> ApiResult<Vec<(ChunkSource, u16)>> {
    book.reader
        .previews()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let data = book.reader.get_preview_data(i).map_err(|e| ApiError::unprocessable("damaged", e.to_string()))?;
            Ok((ChunkSource::from_bytes(data.to_vec())?, p.volume_id.get()))
        })
        .collect()
}

#[derive(Deserialize)]
pub struct CoverQuery {
    /// 0 — the book cover (default), N — cover of volume N.
    volume: Option<u16>,
    /// Replace the existing covers of this volume (default `true`).
    replace: Option<bool>,
}

/// `POST /books/{id}/covers`: the body is an image (any format the format allows). A book cover goes
/// first — it becomes the main one.
pub async fn add_cover(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(id): Path<i64>,
    Query(q): Query<CoverQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let source = ChunkSource::from_bytes(body.to_vec())?;
    match source.probe_image() {
        Ok((w, h, f)) if w > 0 && h > 0 && f != PageFormat::Unknown => {}
        _ => return Err(ApiError::bad_request("the body is not a supported image (AVIF, JPEG, PNG, WebP, GIF, JPEG XL)")),
    }
    let volume = q.volume.unwrap_or(0);
    let replace = q.replace.unwrap_or(true);
    let summary = rewrite_book(&state, admin.id, id, move |book| {
        let mut previews = current_previews(book)?;
        if replace {
            previews.retain(|(_, v)| *v != volume);
        }
        if volume == 0 {
            previews.insert(0, (source, 0));
        } else {
            previews.push((source, volume));
        }
        Ok(Changes { previews: Some(previews), ..Default::default() })
    })
    .await?;
    Ok(Json(summary))
}

/// `DELETE /books/{id}/covers/{i}`
pub async fn delete_cover(State(state): State<AppState>, Admin(admin): Admin, Path((id, i)): Path<(i64, u32)>) -> ApiResult<Json<Value>> {
    let summary = rewrite_book(&state, admin.id, id, move |book| {
        let mut previews = current_previews(book)?;
        if i as usize >= previews.len() {
            return Err(ApiError::not_found(format!("cover {i}")));
        }
        previews.remove(i as usize);
        Ok(Changes { previews: Some(previews), ..Default::default() })
    })
    .await?;
    Ok(Json(summary))
}

#[derive(Deserialize)]
pub struct Protect {
    /// Reed–Solomon redundancy, % (0 — off, up to 30).
    redundancy: Option<u8>,
    /// The backup copy of the metadata at the end of the file (always on with redundancy).
    tail_copy: Option<bool>,
    /// Alignment block, bytes: 4096, 8192, 16384, 32768.
    block_size: Option<u32>,
}

/// `POST /books/{id}/protect`: changes the protection against storage faults.
pub async fn protect(State(state): State<AppState>, Admin(admin): Admin, Path(id): Path<i64>, Json(body): Json<Protect>) -> ApiResult<Json<Value>> {
    if body.redundancy.is_some_and(|r| r > vnmf_core::spec::MAX_REDUNDANCY) {
        return Err(ApiError::bad_request(format!("redundancy is at most {}%", vnmf_core::spec::MAX_REDUNDANCY)));
    }
    let block_size = match body.block_size {
        None => None,
        Some(4096) => Some(BlockSize::Page4k),
        Some(8192) => Some(BlockSize::Page8k),
        Some(16384) => Some(BlockSize::Page16k),
        Some(32768) => Some(BlockSize::Ext32k),
        Some(other) => return Err(ApiError::bad_request(format!("block size {other} — expected 4096, 8192, 16384 or 32768"))),
    };
    let changes = Changes { redundancy: body.redundancy, tail_duplicate: body.tail_copy, block_size, ..Default::default() };
    Ok(Json(rewrite_book(&state, admin.id, id, move |_| Ok(changes)).await?))
}

#[derive(Deserialize)]
pub struct HealthQuery {
    /// Read the whole file: CRC of every chunk and the redundancy (default — only both metadata copies).
    #[serde(default)]
    full: bool,
}

/// `GET /books/{id}/health?full=true`: the result is also stored in the library index.
pub async fn health(State(state): State<AppState>, _: Signed, Path(id): Path<i64>, Query(q): Query<HealthQuery>) -> ApiResult<Json<library::HealthReport>> {
    let (_, book) = open_book(&state, id).await?;
    let st = state.clone();
    let report = tokio::task::spawn_blocking(move || -> ApiResult<_> {
        let report = library::check(&book.reader, q.full);
        st.db.blocking(|c| library::store_health(c, id, &report))?;
        Ok(report)
    })
    .await??;
    Ok(Json(report))
}

#[derive(Deserialize)]
pub struct RepairQuery {
    /// Replace the file even if it was repaired only partially (it may be more readable than before,
    /// but the original is gone). By default a partial result is discarded.
    #[serde(default)]
    accept_partial: bool,
}

/// `POST /books/{id}/repair`: restores the metadata copies and repairs damaged data with the
/// redundancy (like `vnmfrecovery`). Extracting what survived of a hopeless file is left to `vnmfrecovery`.
pub async fn repair(State(state): State<AppState>, Admin(admin): Admin, Path(id): Path<i64>, Query(q): Query<RepairQuery>) -> ApiResult<Json<Value>> {
    let row = state.db.call(move |c| book_row(c, id)).await?;
    if row.missing {
        return Err(ApiError::new(axum::http::StatusCode::GONE, "missing", "the book's file is missing"));
    }
    require_writable(&state, &row)?;
    let _busy = state.lock_book(id).ok_or_else(ApiError::busy)?;
    let st = state.clone();
    let (outcome, replaced, damaged_left, log) = tokio::task::spawn_blocking(move || -> ApiResult<_> {
        // Repaired next to the file under a hidden name (scans skip it), moved over it only if accepted.
        let name = row.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = row.path.with_file_name(format!(".{name}.repair"));
        let report = vnmf::repair::repair(&row.path, &tmp).map_err(|e| ApiError::unprocessable("repair_failed", format!("{e:#}")))?;
        use vnmf::repair::Outcome;
        let accept = report.written && (report.outcome == Outcome::Repaired || (report.outcome == Outcome::Partial && q.accept_partial));
        if accept {
            std::fs::rename(&tmp, &row.path)?;
            st.books.forget(id);
            library::index_file(&st, &row.path)?;
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
        let outcome = match report.outcome {
            Outcome::Intact => "intact",
            Outcome::Repaired => "repaired",
            Outcome::Partial => "partial",
        };
        Ok((outcome, accept, report.damaged_left, report.log))
    })
    .await??;
    let writable_libraries = state.config.writable_libraries;
    let book = state.db.call(move |c| book_summary(c, id, admin.id, writable_libraries)).await?;
    Ok(Json(json!({ "outcome": outcome, "replaced": replaced, "damaged_left": damaged_left, "log": log, "book": book })))
}
