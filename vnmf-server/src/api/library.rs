// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! The library as a whole: folders, counts, background scan and integrity check.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::Signed;
use crate::AppState;
use crate::auth::Admin;
use crate::error::ApiResult;
use crate::library;

/// `GET /library`: folders, numbers of books, the state of the background jobs.
pub async fn status(State(state): State<AppState>, user: Signed) -> ApiResult<Json<Value>> {
    let counts = state
        .db
        .call(|c| {
            Ok(c.query_row(
                "SELECT COUNT(DISTINCT uuid) FILTER (WHERE missing = 0), COUNT(*) FILTER (WHERE missing = 1), COALESCE(SUM(size) FILTER (WHERE missing = 0), 0),
                 COALESCE(SUM(page_count) FILTER (WHERE missing = 0), 0), COUNT(*) FILTER (WHERE missing = 0 AND health = 'damaged'),
                 COUNT(*) FILTER (WHERE missing = 0 AND health_full IS NOT 1), COUNT(*) FILTER (WHERE missing = 0 AND redundancy > 0),
                 (SELECT COUNT(*) FROM (SELECT uuid FROM books GROUP BY uuid HAVING COUNT(*) > 1)),
                 (SELECT COALESCE(SUM(size), 0) FROM books b WHERE missing = 0
                     AND EXISTS (SELECT 1 FROM books o WHERE o.uuid = b.uuid AND o.missing = 0 AND o.id < b.id)),
                 MIN(health_checked_at) FILTER (WHERE missing = 0 AND health_full = 1)
                 FROM books",
                [],
                |r| {
                    Ok(json!({ "books": r.get::<_, i64>(0)?, "missing": r.get::<_, i64>(1)?, "bytes": r.get::<_, i64>(2)?, "pages": r.get::<_, i64>(3)?,
                               "damaged": r.get::<_, i64>(4)?, "not_fully_checked": r.get::<_, i64>(5)?, "protected": r.get::<_, i64>(6)?,
                               "duplicates": r.get::<_, i64>(7)?, "duplicate_bytes": r.get::<_, i64>(8)?,
                               "oldest_full_check": r.get::<_, Option<i64>>(9)? }))
                },
            )?)
        })
        .await?;
    let scan = state.jobs.scan.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let verify = state.jobs.verify.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let verify_days = state.config.verify_interval.map(|d| d.as_secs() / 86400);
    let mut out = json!({ "counts": counts, "scan": scan, "verify": verify, "verify_interval_days": verify_days });
    // Paths on the server's disk are for administrators only.
    if user.is_admin {
        out["folders"] = json!({
            "library": state.config.library_dir(),
            "extra": state.config.libraries,
            "writable_extra": state.config.writable_libraries,
        });
    }
    Ok(Json(out))
}

/// `GET /library/duplicates`: books that are in several files (the same UUID), with every copy —
/// to delete identical ones with `DELETE /books/{id}/copies/{other}`.
pub async fn duplicates(State(state): State<AppState>, _: Admin) -> ApiResult<Json<Value>> {
    let writable_libraries = state.config.writable_libraries;
    let groups = state
        .db
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT b.uuid, b.id, b.title, b.path, b.size, b.managed, b.missing, b.added_at,
                     (SELECT COUNT(*) FROM progress p WHERE p.book_id = b.id)
                 FROM books b WHERE b.uuid IN (SELECT uuid FROM books GROUP BY uuid HAVING COUNT(*) > 1)
                 ORDER BY b.sort_title COLLATE NATSORT, b.uuid, b.id",
            )?;
            let mut groups: Vec<Value> = Vec::new();
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let uuid: String = r.get(0)?;
                let managed: bool = r.get(5)?;
                let copy = json!({
                    "id": r.get::<_, i64>(1)?, "path": r.get::<_, String>(3)?, "size": r.get::<_, i64>(4)?,
                    "managed": managed, "writable": managed || writable_libraries, "missing": r.get::<_, bool>(6)?,
                    "added_at": r.get::<_, i64>(7)?, "readers": r.get::<_, i64>(8)?,
                });
                match groups.last_mut() {
                    Some(g) if g["uuid"] == uuid.as_str() => g["copies"].as_array_mut().expect("an array").push(copy),
                    _ => groups.push(json!({ "uuid": uuid, "title": r.get::<_, String>(2)?, "copies": [copy] })),
                }
            }
            Ok(groups)
        })
        .await?;
    Ok(Json(json!({ "groups": groups })))
}

/// `POST /library/scan`: 202 — started, 409 — already running.
pub async fn scan(State(state): State<AppState>, _: Admin) -> StatusCode {
    if library::start_scan(&state) { StatusCode::ACCEPTED } else { StatusCode::CONFLICT }
}

/// `POST /library/verify`: a full integrity check of every book in the background.
pub async fn verify(State(state): State<AppState>, _: Admin) -> StatusCode {
    if library::start_verify(&state) { StatusCode::ACCEPTED } else { StatusCode::CONFLICT }
}
