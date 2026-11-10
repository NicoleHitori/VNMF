// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! The book list (search, filters, sorting), facets, series, a single book, deleting and uploading.

use std::path::{Path as FsPath, PathBuf};

use axum::Json;
use axum::extract::{FromRequest, Multipart, Path, Query, Request, State};
use axum::http::{StatusCode, header};
use futures_util::StreamExt;
use rusqlite::types::Value as SqlValue;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use vnmf_core::{BlockSize, VnmfReader};

use super::{Signed, open_book, writable};
use crate::AppState;
use crate::auth::Admin;
use crate::error::{ApiError, ApiResult};
use crate::library::{self, book_row};

/// Columns of a book summary; `?1` in the join is the user id.
const SUMMARY: &str = "SELECT b.id, b.uuid, b.title, b.authors, b.kind, b.status, b.language, b.genres, b.tags, b.page_count, b.volume_count,
    b.chapter_count, b.webtoon, b.rtl, b.has_music, b.has_sfx, b.has_text, b.has_dark, b.redundancy, b.format_version, b.size, b.mtime,
    b.managed, b.missing, b.recovered, b.health, b.health_issues, b.health_full, b.health_checked_at, b.added_at, b.updated_at,
    p.page, p.chunk, p.offset, p.finished, p.updated_at, p.furthest
    FROM books b LEFT JOIN progress p ON p.book_id = b.id AND p.user_id = ?1";

/// No other copy of the book (same UUID, file present) is preferred over `b`: one with the user's
/// progress, then an uploaded one, then the oldest. Needs `p` (the user's progress of `b`) and `?1`.
const OTHER_COPY_PREFERRED: &str = "NOT EXISTS (SELECT 1 FROM books d LEFT JOIN progress dp ON dp.book_id = d.id AND dp.user_id = ?1
    WHERE d.uuid = b.uuid AND d.id <> b.id AND d.missing = 0
    AND (dp.updated_at IS NOT NULL, d.managed, -d.id) > (p.updated_at IS NOT NULL, b.managed, -b.id))";

fn list_json(text: Option<String>) -> Value {
    text.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!([]))
}

/// A row of [`SUMMARY`] as JSON.
fn summary(r: &rusqlite::Row, writable_libraries: bool) -> rusqlite::Result<Value> {
    let id: i64 = r.get(0)?;
    let (size, mtime): (i64, i64) = (r.get(20)?, r.get(21)?);
    let managed: bool = r.get(22)?;
    let version = format!("{size:x}-{mtime:x}");
    let progress = match r.get::<_, Option<i64>>(31)? {
        Some(page) => json!({ "page": page, "chunk": r.get::<_, Option<i64>>(32)?, "offset": r.get::<_, Option<f64>>(33)?,
                              "finished": r.get::<_, bool>(34)?, "updated_at": r.get::<_, i64>(35)?, "furthest": r.get::<_, i64>(36)? }),
        None => Value::Null,
    };
    let health = match r.get::<_, Option<String>>(25)? {
        Some(status) => {
            json!({ "status": status, "issues": list_json(r.get(26)?), "full": r.get::<_, Option<bool>>(27)?, "checked_at": r.get::<_, Option<i64>>(28)? })
        }
        None => Value::Null,
    };
    Ok(json!({
        "id": id,
        "uuid": r.get::<_, String>(1)?,
        "title": r.get::<_, String>(2)?,
        "authors": list_json(r.get(3)?),
        "kind": r.get::<_, Option<String>>(4)?,
        "status": r.get::<_, Option<String>>(5)?,
        "language": r.get::<_, Option<String>>(6)?,
        "genres": list_json(r.get(7)?),
        "tags": list_json(r.get(8)?),
        "page_count": r.get::<_, i64>(9)?,
        "volume_count": r.get::<_, i64>(10)?,
        "chapter_count": r.get::<_, i64>(11)?,
        "webtoon": r.get::<_, bool>(12)?,
        "rtl": r.get::<_, bool>(13)?,
        "has_music": r.get::<_, bool>(14)?,
        "has_sfx": r.get::<_, bool>(15)?,
        "has_text": r.get::<_, bool>(16)?,
        "has_dark": r.get::<_, bool>(17)?,
        "redundancy": r.get::<_, i64>(18)?,
        "format_version": r.get::<_, String>(19)?,
        "size": size,
        "version": version,
        "managed": managed,
        "writable": managed || writable_libraries,
        "missing": r.get::<_, bool>(23)?,
        "recovered": r.get::<_, bool>(24)?,
        "health": health,
        "added_at": r.get::<_, i64>(29)?,
        "updated_at": r.get::<_, i64>(30)?,
        "progress": progress,
        "cover_url": format!("/api/v1/books/{id}/cover?v={version}"),
    }))
}

/// One book's summary for `user_id`.
pub(super) fn book_summary(conn: &rusqlite::Connection, id: i64, user_id: i64, writable_libraries: bool) -> ApiResult<Value> {
    use rusqlite::OptionalExtension;
    conn.query_row(&format!("{SUMMARY} WHERE b.id = ?2"), [user_id, id], |r| summary(r, writable_libraries))
        .optional()?
        .ok_or_else(|| ApiError::not_found(format!("book {id}")))
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    /// Words to find in titles (all of them), authors, genres, tags, characters, publisher, ISBN.
    q: Option<String>,
    /// Lists are comma-separated. Genres and tags: the book has **all** of them.
    genre: Option<String>,
    tag: Option<String>,
    /// The other lists: **any** of the values.
    kind: Option<String>,
    status: Option<String>,
    language: Option<String>,
    country: Option<String>,
    age: Option<String>,
    publication: Option<String>,
    demographic: Option<String>,
    color: Option<String>,
    translation: Option<String>,
    /// Ranges (inclusive).
    chapters_min: Option<i64>,
    chapters_max: Option<i64>,
    pages_min: Option<i64>,
    pages_max: Option<i64>,
    year_min: Option<i64>,
    year_max: Option<i64>,
    /// Books with exactly this title (a series of volume files).
    series: Option<String>,
    /// Books in this list of the user (`/lists`); `any` — in any of them.
    list: Option<String>,
    webtoon: Option<bool>,
    /// `ok`, `damaged`, `unchecked`.
    health: Option<String>,
    /// `unread`, `reading`, `finished`.
    reading: Option<String>,
    /// Include books whose files are missing.
    #[serde(default)]
    missing: bool,
    /// Include every copy of a book (files with the same UUID). By default only one is listed: the one
    /// the user reads, else an uploaded one, else the first found.
    #[serde(default)]
    copies: bool,
    /// `title` (default), `added`, `updated`, `size`, `pages`, `read` (last read first).
    sort: Option<String>,
    /// `asc` / `desc`.
    order: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

/// `GET /books`
pub async fn list(State(state): State<AppState>, user: Signed, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let mut wheres: Vec<String> = Vec::new();
    let mut args: Vec<SqlValue> = vec![SqlValue::Integer(user.id)];
    let mut arg = |v: SqlValue| {
        args.push(v);
        format!("?{}", args.len())
    };
    if !q.missing {
        wheres.push("b.missing = 0".into());
    }
    if !q.copies {
        wheres.push(OTHER_COPY_PREFERRED.into());
    }
    for word in q.q.as_deref().unwrap_or("").to_lowercase().split_whitespace() {
        let escaped = word.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        wheres.push(format!("b.search LIKE {} ESCAPE '\\'", arg(SqlValue::Text(format!("%{escaped}%")))));
    }
    let values =
        |v: &Option<String>| -> Vec<String> { v.as_deref().unwrap_or("").split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned).collect() };
    for (column, value) in [("genres", &q.genre), ("tags", &q.tag)] {
        for v in values(value) {
            wheres.push(format!("EXISTS (SELECT 1 FROM json_each(b.{column}) WHERE lower(value) = lower({}))", arg(SqlValue::Text(v))));
        }
    }
    for (column, value) in [
        ("kind", &q.kind),
        ("status", &q.status),
        ("language", &q.language),
        ("origin_country", &q.country),
        ("age_rating", &q.age),
        ("publication", &q.publication),
        ("demographic", &q.demographic),
        ("color", &q.color),
        ("translation", &q.translation),
    ] {
        let list = values(value);
        if !list.is_empty() {
            let placeholders: Vec<String> = list.into_iter().map(|v| arg(SqlValue::Text(v))).collect();
            wheres.push(format!("b.{column} IN ({})", placeholders.join(", ")));
        }
    }
    for (column, min, max) in [("chapter_count", q.chapters_min, q.chapters_max), ("page_count", q.pages_min, q.pages_max), ("year", q.year_min, q.year_max)] {
        if let Some(min) = min {
            wheres.push(format!("b.{column} >= {}", arg(SqlValue::Integer(min))));
        }
        if let Some(max) = max {
            wheres.push(format!("b.{column} <= {}", arg(SqlValue::Integer(max))));
        }
    }
    match q.list.as_deref() {
        None | Some("") => {}
        Some("any") => wheres.push("EXISTS (SELECT 1 FROM list_books lb JOIN lists l ON l.id = lb.list_id WHERE lb.book_id = b.id AND l.user_id = ?1)".into()),
        Some(id) => {
            let id: i64 = id.parse().map_err(|_| ApiError::bad_request(format!("unknown list {id}")))?;
            wheres.push(format!(
                "EXISTS (SELECT 1 FROM list_books lb JOIN lists l ON l.id = lb.list_id WHERE lb.book_id = b.id AND l.user_id = ?1 AND l.id = {})",
                arg(SqlValue::Integer(id))
            ));
        }
    }
    if let Some(series) = &q.series {
        wheres.push(format!("b.sort_title = {}", arg(SqlValue::Text(series.to_lowercase()))));
    }
    if let Some(w) = q.webtoon {
        wheres.push(format!("b.webtoon = {}", arg(SqlValue::Integer(i64::from(w)))));
    }
    match q.health.as_deref() {
        None => {}
        Some("unchecked") => wheres.push("b.health_full IS NOT 1".into()),
        Some(h @ ("ok" | "damaged")) => wheres.push(format!("b.health = '{h}'")),
        Some(other) => return Err(ApiError::bad_request(format!("unknown health filter {other}"))),
    }
    match q.reading.as_deref() {
        None => {}
        Some("unread") => wheres.push("p.page IS NULL".into()),
        Some("reading") => wheres.push("p.page IS NOT NULL AND p.finished = 0".into()),
        Some("finished") => wheres.push("p.finished = 1".into()),
        Some(other) => return Err(ApiError::bad_request(format!("unknown reading filter {other}"))),
    }
    let desc = match q.order.as_deref() {
        None | Some("asc") => false,
        Some("desc") => true,
        Some(other) => return Err(ApiError::bad_request(format!("unknown order {other}"))),
    };
    let sort = match q.sort.as_deref().unwrap_or("title") {
        "title" => "b.sort_title COLLATE NATSORT",
        "added" => "b.added_at",
        "updated" => "b.updated_at",
        "size" => "b.size",
        "pages" => "b.page_count",
        "read" => "p.updated_at IS NULL, p.updated_at",
        other => return Err(ApiError::bad_request(format!("unknown sort {other}"))),
    };
    // "Last read first" means descending by default; `order` flips it.
    let desc = if q.sort.as_deref() == Some("read") { !desc } else { desc };
    let order = if desc {
        sort.split(", ").map(|s| if s.ends_with("IS NULL") { s.to_string() } else { format!("{s} DESC") }).collect::<Vec<_>>().join(", ")
    } else {
        sort.to_string()
    };
    let filter = if wheres.is_empty() { String::new() } else { format!(" WHERE {}", wheres.join(" AND ")) };
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let offset = q.offset.unwrap_or(0);
    let writable_libraries = state.config.writable_libraries;

    let (total, items) = state
        .db
        .call(move |c| {
            let from = "FROM books b LEFT JOIN progress p ON p.book_id = b.id AND p.user_id = ?1";
            let total: i64 = c.query_row(&format!("SELECT COUNT(*) {from}{filter}"), rusqlite::params_from_iter(&args), |r| r.get(0))?;
            let sql = format!("{SUMMARY}{filter} ORDER BY {order}, b.id LIMIT {limit} OFFSET {offset}");
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(&args), |r| summary(r, writable_libraries))?;
            Ok((total, rows.collect::<rusqlite::Result<Vec<_>>>()?))
        })
        .await?;
    Ok(Json(json!({ "total": total, "offset": offset, "limit": limit, "items": items })))
}

/// `GET /books/facets`: values to filter by, with the number of books.
pub async fn facets(State(state): State<AppState>, _: Signed) -> ApiResult<Json<Value>> {
    let facets = state
        .db
        .call(|c| {
            let count = |sql: &str| -> rusqlite::Result<Vec<Value>> {
                let mut stmt = c.prepare(sql)?;
                let rows = stmt.query_map([], |r| Ok(json!({ "value": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)? })))?;
                rows.collect()
            };
            let list = |column: &str| count(&format!("SELECT j.value, COUNT(DISTINCT b.uuid) FROM books b, json_each(b.{column}) j WHERE b.missing = 0 GROUP BY lower(j.value) ORDER BY COUNT(DISTINCT b.uuid) DESC, j.value COLLATE NATSORT"));
            let plain = |column: &str| count(&format!("SELECT {column}, COUNT(DISTINCT uuid) FROM books WHERE missing = 0 AND {column} IS NOT NULL GROUP BY {column} ORDER BY COUNT(DISTINCT uuid) DESC"));
            let range = |column: &str| -> rusqlite::Result<Value> {
                c.query_row(&format!("SELECT MIN({column}), MAX({column}) FROM books WHERE missing = 0 AND {column} IS NOT NULL"), [], |r| {
                    Ok(json!({ "min": r.get::<_, Option<i64>>(0)?, "max": r.get::<_, Option<i64>>(1)? }))
                })
            };
            Ok(json!({
                "genres": list("genres")?, "tags": list("tags")?, "authors": list("authors")?,
                "kinds": plain("kind")?, "statuses": plain("status")?, "languages": plain("language")?,
                "countries": plain("origin_country")?, "age_ratings": plain("age_rating")?, "publications": plain("publication")?,
                "demographics": plain("demographic")?, "colors": plain("color")?, "translations": plain("translation")?,
                "ranges": { "chapters": range("chapter_count")?, "pages": range("page_count")?, "year": range("year")? },
            }))
        })
        .await?;
    Ok(Json(facets))
}

/// `GET /series`: books grouped by title (a series kept as one file per volume), in natural order.
pub async fn series(State(state): State<AppState>, _: Signed) -> ApiResult<Json<Value>> {
    let items = state
        .db
        .call(|c| {
            let mut stmt = c.prepare(
                "SELECT MIN(title), COUNT(*), json_group_array(id), SUM(page_count), MAX(added_at) FROM
                 (SELECT * FROM books WHERE missing = 0 ORDER BY path COLLATE NATSORT)
                 GROUP BY sort_title ORDER BY sort_title COLLATE NATSORT",
            )?;
            let rows = stmt.query_map([], |r| {
                let ids: Value = serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_else(|_| json!([]));
                Ok(json!({ "title": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?, "book_ids": ids,
                           "page_count": r.get::<_, i64>(3)?, "last_added_at": r.get::<_, i64>(4)? }))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await?;
    Ok(Json(json!({ "items": items })))
}

/// `GET /books/{id}`: the summary plus everything about the file (`info`; `null` if it can't be opened).
pub async fn get(State(state): State<AppState>, user: Signed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let writable_libraries = state.config.writable_libraries;
    let mut book = state.db.call(move |c| book_summary(c, id, user.id, writable_libraries)).await?;
    let info = match open_book(&state, id).await {
        Ok((_, opened)) => serde_json::to_value(opened.info()).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };
    book["info"] = info;
    let show_paths = user.is_admin;
    book["copies"] = state
        .db
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT d.id, d.path, d.size, d.managed, d.missing, b.size FROM books d JOIN books b ON b.uuid = d.uuid AND b.id = ?1
                 WHERE d.id <> ?1 ORDER BY d.id",
            )?;
            let rows = stmt.query_map([id], |r| {
                let (managed, size): (bool, i64) = (r.get(3)?, r.get(2)?);
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "path": if show_paths { Some(r.get::<_, String>(1)?) } else { None },
                    "size": size,
                    "managed": managed,
                    "writable": managed || writable_libraries,
                    "missing": r.get::<_, bool>(4)?,
                    "same_size": size == r.get::<_, i64>(5)?,
                }))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await?
        .into();
    Ok(Json(book))
}

/// Whether two files have the same bytes.
fn same_bytes(a: &FsPath, b: &FsPath) -> std::io::Result<bool> {
    use std::io::Read;
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = fa.read(&mut ba)?;
        if n == 0 {
            return Ok(true);
        }
        fb.read_exact(&mut bb[..n])?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
    }
}

/// `DELETE /books/{id}/copies/{other}`: deletes a copy of the book — a file with the same UUID and
/// the same bytes (refused with `different_copy` otherwise; a missing copy is just forgotten). Progress,
/// bookmarks and lists of every user move to the book that stays (the newer progress wins).
pub async fn remove_copy(State(state): State<AppState>, _: Admin, Path((id, other)): Path<(i64, i64)>) -> ApiResult<StatusCode> {
    if id == other {
        return Err(ApiError::bad_request("a book is not a copy of itself"));
    }
    let (keep, drop, same_uuid) = state
        .db
        .call(move |c| {
            let same: bool = c.query_row("SELECT (SELECT uuid FROM books WHERE id = ?1) = (SELECT uuid FROM books WHERE id = ?2)", [id, other], |r| {
                Ok(r.get::<_, Option<bool>>(0)?.unwrap_or(false))
            })?;
            Ok((book_row(c, id)?, book_row(c, other)?, same))
        })
        .await?;
    if !same_uuid {
        return Err(ApiError::bad_request(format!("book {other} is not a copy of book {id}")));
    }
    if keep.missing {
        return Err(ApiError::bad_request("the book's own file is missing: keep the copy instead"));
    }
    if !drop.missing && !writable(&state, &drop) {
        return Err(ApiError::read_only());
    }
    let _busy_keep = state.lock_book(id).ok_or_else(ApiError::busy)?;
    let _busy_drop = state.lock_book(other).ok_or_else(ApiError::busy)?;
    if !drop.missing {
        let (a, b) = (keep.path.clone(), drop.path.clone());
        let same = tokio::task::spawn_blocking(move || same_bytes(&a, &b)).await.map_err(anyhow::Error::from)??;
        if !same {
            return Err(ApiError::conflict("different_copy", "the files have the same UUID but different contents (another version?): delete it as a book instead"));
        }
    }
    state
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO progress (user_id, book_id, page, chunk, offset, finished, updated_at, furthest)
                 SELECT user_id, ?1, page, chunk, offset, finished, updated_at, furthest FROM progress WHERE book_id = ?2
                 ON CONFLICT (user_id, book_id) DO UPDATE SET page = excluded.page, chunk = excluded.chunk, offset = excluded.offset,
                     finished = excluded.finished, updated_at = excluded.updated_at
                 WHERE excluded.updated_at > progress.updated_at",
                [id, other],
            )?;
            tx.execute(
                "UPDATE progress SET furthest = MAX(furthest, (SELECT o.furthest FROM progress o WHERE o.book_id = ?2 AND o.user_id = progress.user_id))
                 WHERE book_id = ?1 AND user_id IN (SELECT user_id FROM progress WHERE book_id = ?2)",
                [id, other],
            )?;
            tx.execute("UPDATE bookmarks SET book_id = ?1 WHERE book_id = ?2", [id, other])?;
            // A book is in at most one basic list of a user: the kept book's one wins.
            tx.execute(
                "INSERT OR IGNORE INTO list_books (list_id, book_id, added_at)
                 SELECT lb.list_id, ?1, lb.added_at FROM list_books lb JOIN lists l ON l.id = lb.list_id
                 WHERE lb.book_id = ?2 AND (l.kind IS NULL OR NOT EXISTS (
                     SELECT 1 FROM list_books k JOIN lists kl ON kl.id = k.list_id
                     WHERE k.book_id = ?1 AND kl.user_id = l.user_id AND kl.kind IS NOT NULL))",
                [id, other],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    if !drop.missing {
        match tokio::fs::remove_file(&drop.path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    state.books.forget(other);
    state.db.call(move |c| Ok(c.execute("DELETE FROM books WHERE id = ?1", [other])?)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct RemoveQuery {
    /// Delete the file too (default). `false` — only forget a missing book.
    delete_file: Option<bool>,
}

/// `DELETE /books/{id}`: the file (in the own library, or anywhere with `--writable-libraries`) and the
/// book with its progress and bookmarks. A missing book is just forgotten.
pub async fn remove(State(state): State<AppState>, _: Admin, Path(id): Path<i64>, Query(q): Query<RemoveQuery>) -> ApiResult<StatusCode> {
    let row = state.db.call(move |c| book_row(c, id)).await?;
    let delete_file = q.delete_file.unwrap_or(true) && !row.missing;
    if delete_file && !writable(&state, &row) {
        return Err(ApiError::read_only());
    }
    if !delete_file && !row.missing {
        return Err(ApiError::bad_request("the file exists: it would be found again by the next scan — delete it, or remove it from the library folder"));
    }
    let _busy = state.lock_book(id).ok_or_else(ApiError::busy)?;
    if delete_file {
        match tokio::fs::remove_file(&row.path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    state.books.forget(id);
    state.db.call(move |c| Ok(c.execute("DELETE FROM books WHERE id = ?1", [id])?)).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----------------------------------------------------------------------
// Uploads
// ----------------------------------------------------------------------

#[derive(Deserialize, Default, Clone)]
pub struct UploadOptions {
    /// Name of the uploaded file (raw body uploads). Volume and chapter numbers are taken from names
    /// (`Vol.2 Ch.10.cbz`), as `vnmfpack` does.
    filename: Option<String>,
    /// All uploaded files (CBZ, images, ComicInfo.xml, vnmf.json) become one book, each CBZ a chapter.
    #[serde(default)]
    combine: bool,
    /// CBZ conversion: each chapter becomes one long strip.
    #[serde(default)]
    webtoon: bool,
    #[serde(default)]
    mixed_formats: bool,
    #[serde(default)]
    rtl: bool,
    /// Reed–Solomon redundancy for converted books, % (0–30).
    #[serde(default)]
    redundancy: u8,
    /// Volume for chapters whose names don't say.
    volume: Option<u16>,
    /// Cover of converted books: `first` (default) or `none`.
    cover: Option<String>,
    /// A `.vnmf` with the UUID of a book already in the library replaces its file (keeping progress).
    #[serde(default)]
    replace: bool,
}

/// Removes the upload's temporary folder whatever happens.
struct Incoming(PathBuf);

impl Drop for Incoming {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A file name from a client: the last path component, without control characters.
fn client_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    let clean: String = base.chars().filter(|c| !c.is_control()).take(150).collect();
    if clean.is_empty() || clean == "." || clean == ".." { "upload".into() } else { clean }
}

/// A file name for the library made from a title.
fn library_name(title: &str) -> String {
    let clean: String = title.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).take(120).collect();
    let clean = clean.trim().trim_matches('.').trim().to_string();
    if clean.is_empty() { "book".into() } else { clean }
}

/// `<library>/<name>.vnmf`, or `<name> (2).vnmf`… if taken.
fn free_path(dir: &FsPath, name: &str) -> PathBuf {
    let mut path = dir.join(format!("{name}.vnmf"));
    let mut n = 2;
    while path.exists() || path.with_extension("vnmf.tmp").exists() {
        path = dir.join(format!("{name} ({n}).vnmf"));
        n += 1;
    }
    path
}

/// `POST /books`: a raw body (`?filename=`) or `multipart/form-data` with one or more files.
/// `.vnmf` files are added as they are; CBZ/ZIP archives are converted (see [`UploadOptions`]).
pub async fn upload(
    State(state): State<AppState>,
    Admin(_): Admin,
    Query(options): Query<UploadOptions>,
    request: Request,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if options.redundancy > vnmf_core::spec::MAX_REDUNDANCY {
        return Err(ApiError::bad_request(format!("redundancy is at most {}%", vnmf_core::spec::MAX_REDUNDANCY)));
    }
    let dir = state.config.incoming_dir().join(uuid::Uuid::new_v4().to_string());
    tokio::fs::create_dir_all(&dir).await?;
    let incoming = Incoming(dir);
    let limit = state.config.max_upload;
    let multipart = request.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("multipart/form-data"));

    let mut files: Vec<PathBuf> = Vec::new();
    let mut total = 0u64;
    let too_big = || ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", format!("uploads are limited to {limit} bytes"));
    if multipart {
        let mut form = Multipart::from_request(request, &state).await.map_err(|e| ApiError::bad_request(e.to_string()))?;
        while let Some(mut field) = form.next_field().await.map_err(|e| ApiError::bad_request(e.to_string()))? {
            let Some(name) = field.file_name().map(client_name) else { continue };
            let path = incoming.0.join(files.len().to_string()).join(name);
            tokio::fs::create_dir_all(path.parent().unwrap_or(&incoming.0)).await?;
            let mut out = tokio::fs::File::create(&path).await?;
            while let Some(chunk) = field.chunk().await.map_err(|e| ApiError::bad_request(e.to_string()))? {
                total += chunk.len() as u64;
                if total > limit {
                    return Err(too_big());
                }
                out.write_all(&chunk).await?;
            }
            out.flush().await?;
            files.push(path);
        }
    } else {
        let name = client_name(options.filename.as_deref().unwrap_or("upload"));
        let path = incoming.0.join("0").join(name);
        tokio::fs::create_dir_all(path.parent().unwrap_or(&incoming.0)).await?;
        let mut out = tokio::fs::File::create(&path).await?;
        let mut body = request.into_body().into_data_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| ApiError::bad_request(format!("upload interrupted: {e}")))?;
            total += chunk.len() as u64;
            if total > limit {
                return Err(too_big());
            }
            out.write_all(&chunk).await?;
        }
        out.flush().await?;
        files.push(path);
    }
    if files.is_empty() || total == 0 {
        return Err(ApiError::bad_request("nothing was uploaded"));
    }

    let st = state.clone();
    let result = tokio::task::spawn_blocking(move || import(&st, files, &options)).await?;
    drop(incoming);
    let (ids, errors, warnings) = result;
    if ids.is_empty() {
        let first = errors.into_iter().next().map(|(_, e)| e).unwrap_or_else(|| ApiError::bad_request("nothing was imported"));
        return Err(first);
    }
    let writable_libraries = state.config.writable_libraries;
    let books = state.db.call(move |c| ids.iter().map(|&id| book_summary(c, id, -1, writable_libraries)).collect::<ApiResult<Vec<_>>>()).await?;
    let errors: Vec<Value> = errors.into_iter().map(|(file, e)| json!({ "file": file, "error": e.code, "message": e.message })).collect();
    Ok((StatusCode::CREATED, Json(json!({ "books": books, "errors": errors, "warnings": warnings }))))
}

type Imported = (Vec<i64>, Vec<(String, ApiError)>, Vec<String>);

/// What the uploaded file is, by its first bytes.
enum Kind {
    Vnmf,
    Zip,
    Other,
}

fn kind(path: &FsPath) -> Kind {
    use std::io::Read;
    let mut magic = [0u8; 4];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut magic)).unwrap_or(0);
    match &magic[..n] {
        m if m == vnmf_core::spec::MAGIC => Kind::Vnmf,
        b"PK\x03\x04" => Kind::Zip,
        _ => Kind::Other,
    }
}

fn display_name(path: &FsPath) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Adds the uploaded files to the library. Blocking.
fn import(state: &AppState, files: Vec<PathBuf>, options: &UploadOptions) -> Imported {
    let mut ids = Vec::new();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    if options.combine {
        let name = files.first().map(|f| display_name(f)).unwrap_or_default();
        match vnmf::input::load_inputs(&files)
            .map_err(|e| ApiError::unprocessable("unsupported", format!("{e:#}")))
            .and_then(|chapters| convert(state, chapters, options, &files[0]))
        {
            Ok((id, w)) => {
                ids.push(id);
                warnings.extend(w);
            }
            Err(e) => errors.push((name, e)),
        }
        return (ids, errors, warnings);
    }
    for file in files {
        let name = display_name(&file);
        let result = match kind(&file) {
            Kind::Vnmf => add_vnmf(state, &file, options).map(|id| (id, Vec::new())),
            Kind::Zip => vnmf::input::load_cbz(&file)
                .map_err(|e| ApiError::unprocessable("unsupported", format!("{e:#}")))
                .and_then(|chapters| convert(state, chapters, options, &file)),
            Kind::Other => Err(ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported", "not a .vnmf file or a CBZ/ZIP archive")),
        };
        match result {
            Ok((id, w)) => {
                ids.push(id);
                warnings.extend(w.into_iter().map(|w| format!("{name}: {w}")));
            }
            Err(e) => errors.push((name, e)),
        }
    }
    (ids, errors, warnings)
}

/// A ready `.vnmf`: checked, then moved into the library (or over the book with the same UUID).
fn add_vnmf(state: &AppState, file: &FsPath, options: &UploadOptions) -> ApiResult<i64> {
    let reader = VnmfReader::open(file).map_err(|e| ApiError::unprocessable("unreadable", format!("the file can't be read: {e}")))?;
    let uuid = vnmf::format_uuid(reader.uuid());
    let title = vnmf::metadata::Metadata::from_info(reader.info_bytes()).and_then(|m| m.title).filter(|t| !t.trim().is_empty());
    drop(reader);
    let existing = state.db.blocking(|c| {
        let mut stmt = c.prepare("SELECT id FROM books WHERE uuid = ?1 AND missing = 0 ORDER BY id LIMIT 1")?;
        let id: Option<i64> = stmt.query_row([&uuid], |r| r.get(0)).ok();
        id.map(|id| book_row(c, id)).transpose()
    })?;
    let target = match existing {
        Some(row) if options.replace => {
            if !writable(state, &row) {
                return Err(ApiError::read_only());
            }
            let _busy = state.lock_book(row.id).ok_or_else(ApiError::busy)?;
            std::fs::copy(file, row.path.with_extension("vnmf.tmp"))?;
            std::fs::rename(row.path.with_extension("vnmf.tmp"), &row.path)?;
            state.books.forget(row.id);
            return library::index_file(state, &row.path);
        }
        Some(row) => {
            return Err(ApiError::conflict("exists", format!("this book is already in the library (id {}); add ?replace=true to replace its file", row.id)));
        }
        None => free_path(&state.config.library_dir(), &library_name(&title.unwrap_or_else(|| vnmf::input::file_stem(file)))),
    };
    // The incoming folder is inside the data folder, so this is a rename, not a copy.
    if std::fs::rename(file, &target).is_err() {
        std::fs::copy(file, &target)?;
    }
    library::index_file(state, &target)
}

/// CBZ / images → a new `.vnmf` in the library.
fn convert(state: &AppState, chapters: Vec<vnmf::input::Chapter>, options: &UploadOptions, first: &FsPath) -> ApiResult<(i64, Vec<String>)> {
    use vnmf::pack::{Cover, PackOptions};
    let cover = match options.cover.as_deref() {
        None | Some("first") => Cover::First,
        Some("none") => Cover::None,
        Some(other) => return Err(ApiError::bad_request(format!("unknown cover option {other}"))),
    };
    let packed = vnmf::pack::pack(
        chapters,
        PackOptions {
            volume: options.volume,
            webtoon: options.webtoon,
            mixed_formats: options.mixed_formats,
            rtl: options.rtl,
            block_size: BlockSize::Page4k,
            redundancy: options.redundancy,
            tail_duplicate: true,
            cover,
            ..Default::default()
        },
    )
    .map_err(|e| ApiError::unprocessable("unsupported", format!("{e:#}")))?;
    // The title from the metadata, otherwise the (first) file name.
    let title = packed.title.clone().unwrap_or_else(|| vnmf::input::file_stem(first));
    let target = free_path(&state.config.library_dir(), &library_name(&title));
    packed.builder.build(&target).map_err(|e| ApiError::unprocessable("unsupported", format!("{e:#}")))?;
    Ok((library::index_file(state, &target)?, packed.warnings))
}
