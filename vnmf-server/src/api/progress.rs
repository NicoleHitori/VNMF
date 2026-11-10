// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Reading progress and bookmarks — per user, synchronized between devices.
//!
//! A position is a page; in strip reading also the fragment (chunk) and the fraction of its height at the
//! top of the screen. Offline clients send the time they read at (`updated_at`): an older position
//! than the stored one is not applied, the stored one is returned instead.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Signed;
use super::books::book_summary;
use crate::error::{ApiError, ApiResult};
use crate::library::book_row;
use crate::{AppState, now};

fn progress_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "page": r.get::<_, i64>(0)?,
        "chunk": r.get::<_, Option<i64>>(1)?,
        "offset": r.get::<_, Option<f64>>(2)?,
        "finished": r.get::<_, bool>(3)?,
        "updated_at": r.get::<_, i64>(4)?,
        "furthest": r.get::<_, i64>(5)?,
    }))
}

fn load(conn: &rusqlite::Connection, user: i64, book: i64) -> rusqlite::Result<Option<Value>> {
    conn.query_row("SELECT page, chunk, offset, finished, updated_at, furthest FROM progress WHERE user_id = ?1 AND book_id = ?2", [user, book], progress_json).optional()
}

/// `GET /books/{id}/progress` → the position, or `null`.
pub async fn get(State(state): State<AppState>, user: Signed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let value = state
        .db
        .call(move |c| {
            book_row(c, id)?;
            Ok(load(c, user.id, id)?)
        })
        .await?;
    Ok(Json(value.unwrap_or(Value::Null)))
}

#[derive(Deserialize)]
pub struct Position {
    page: u32,
    chunk: Option<u32>,
    /// 0..1 — the fraction of the chunk's height.
    offset: Option<f64>,
    /// The book is read to the end.
    #[serde(default)]
    finished: bool,
    /// When the position was reached, Unix seconds (default — now).
    updated_at: Option<i64>,
}

/// `PUT /books/{id}/progress` → the position now stored (the sent one, or a newer one from another device).
pub async fn put(State(state): State<AppState>, user: Signed, Path(id): Path<i64>, Json(body): Json<Position>) -> ApiResult<Json<Value>> {
    if body.offset.is_some_and(|o| !(0.0..=1.0).contains(&o)) {
        return Err(ApiError::bad_request("offset is a fraction 0..1"));
    }
    let value = state
        .db
        .call(move |c| {
            let pages: i64 = c
                .query_row("SELECT page_count FROM books WHERE id = ?1", [id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| ApiError::not_found(format!("book {id}")))?;
            if i64::from(body.page) >= pages {
                return Err(ApiError::bad_request(format!("page {} doesn't exist (the book has {pages})", body.page)));
            }
            let at = body.updated_at.unwrap_or_else(now).min(now());
            c.execute(
                "INSERT INTO progress (user_id, book_id, page, chunk, offset, finished, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (user_id, book_id) DO UPDATE SET page = excluded.page, chunk = excluded.chunk, offset = excluded.offset,
                 finished = excluded.finished, updated_at = excluded.updated_at WHERE excluded.updated_at >= progress.updated_at",
                params![user.id, id, body.page, body.chunk, body.offset, body.finished, at],
            )?;
            // Reaching further counts even from a position older than the stored one.
            let reached = if body.finished { pages - 1 } else { i64::from(body.page) };
            c.execute("UPDATE progress SET furthest = MAX(furthest, ?3) WHERE user_id = ?1 AND book_id = ?2", params![user.id, id, reached])?;
            let finished: bool = c.query_row("SELECT finished FROM progress WHERE user_id = ?1 AND book_id = ?2", [user.id, id], |r| r.get(0))?;
            super::lists::follow_progress(c, user.id, id, finished)?;
            Ok(load(c, user.id, id)?)
        })
        .await?;
    Ok(Json(value.unwrap_or(Value::Null)))
}

/// `DELETE /books/{id}/progress`: mark as unread (also out of the "reading" / "completed" lists).
pub async fn remove(State(state): State<AppState>, user: Signed, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    state
        .db
        .call(move |c| {
            c.execute("DELETE FROM progress WHERE user_id = ?1 AND book_id = ?2", [user.id, id])?;
            // Not being read any more: out of "reading" / "completed" (planned, on hold, dropped stay).
            c.execute(
                "DELETE FROM list_books WHERE book_id = ?2 AND list_id IN (SELECT id FROM lists WHERE user_id = ?1 AND kind IN ('reading', 'completed'))",
                [user.id, id],
            )?;
            Ok(())
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct RecentQuery {
    limit: Option<u32>,
    /// Include finished books.
    #[serde(default)]
    finished: bool,
}

/// `GET /progress`: "continue reading" — books with a position, the last read first.
pub async fn recent(State(state): State<AppState>, user: Signed, Query(q): Query<RecentQuery>) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(20).clamp(1, 200);
    let writable_libraries = state.config.writable_libraries;
    let items = state
        .db
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT p.book_id FROM progress p JOIN books b ON b.id = p.book_id
                 WHERE p.user_id = ?1 AND b.missing = 0 AND (?2 OR p.finished = 0) ORDER BY p.updated_at DESC LIMIT ?3",
            )?;
            let ids = stmt.query_map(params![user.id, q.finished, limit], |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            ids.into_iter().map(|id| book_summary(c, id, user.id, writable_libraries)).collect::<ApiResult<Vec<_>>>()
        })
        .await?;
    Ok(Json(json!({ "items": items })))
}

fn bookmark_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "book_id": r.get::<_, i64>(1)?,
        "page": r.get::<_, i64>(2)?,
        "chunk": r.get::<_, Option<i64>>(3)?,
        "offset": r.get::<_, Option<f64>>(4)?,
        "note": r.get::<_, Option<String>>(5)?,
        "created_at": r.get::<_, i64>(6)?,
    }))
}

/// `GET /books/{id}/bookmarks`
pub async fn bookmarks(State(state): State<AppState>, user: Signed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let items = state
        .db
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, book_id, page, chunk, offset, note, created_at FROM bookmarks WHERE user_id = ?1 AND book_id = ?2 ORDER BY page, chunk, offset, id",
            )?;
            Ok(stmt.query_map([user.id, id], bookmark_json)?.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct NewBookmark {
    page: u32,
    chunk: Option<u32>,
    offset: Option<f64>,
    note: Option<String>,
}

/// `POST /books/{id}/bookmarks`
pub async fn add_bookmark(
    State(state): State<AppState>,
    user: Signed,
    Path(id): Path<i64>,
    Json(body): Json<NewBookmark>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if body.note.as_ref().is_some_and(|n| n.len() > 4096) {
        return Err(ApiError::bad_request("a note is limited to 4 KiB"));
    }
    let value = state
        .db
        .call(move |c| {
            let pages: i64 = c
                .query_row("SELECT page_count FROM books WHERE id = ?1", [id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| ApiError::not_found(format!("book {id}")))?;
            if i64::from(body.page) >= pages {
                return Err(ApiError::bad_request(format!("page {} doesn't exist", body.page)));
            }
            c.execute(
                "INSERT INTO bookmarks (user_id, book_id, page, chunk, offset, note, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![user.id, id, body.page, body.chunk, body.offset, body.note, now()],
            )?;
            let new_id = c.last_insert_rowid();
            Ok(c.query_row("SELECT id, book_id, page, chunk, offset, note, created_at FROM bookmarks WHERE id = ?1", [new_id], bookmark_json)?)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(value)))
}

/// `DELETE /bookmarks/{id}`
pub async fn delete_bookmark(State(state): State<AppState>, user: Signed, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let n = state.db.call(move |c| Ok(c.execute("DELETE FROM bookmarks WHERE id = ?1 AND user_id = ?2", [id, user.id])?)).await?;
    if n == 0 {
        return Err(ApiError::not_found(format!("bookmark {id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}
