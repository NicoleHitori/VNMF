// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Reading lists of each user.
//!
//! Five basic lists always exist (created on first use): reading, planned, completed, on hold,
//! dropped — a book is in at most one of them, it's the book's state for the user. Any number of
//! the user's own lists can be added; a book can be in several of them.
//!
//! Reading moves a book between the basic lists by itself: starting it puts it in "reading" (from no
//! list or from "planned"), finishing it — in "completed" (from those or from "reading"). A book the
//! user put on hold or dropped stays there. Putting a book in "completed" by hand marks it read.
//! A book whose translation (or, if it doesn't say, the series) is still coming out — ongoing, on
//! hiatus, upcoming — stays in "reading" when finished: the reader caught up, more is to come.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Signed;
use crate::error::{ApiError, ApiResult};
use crate::library::book_row;
use crate::{AppState, now};

/// The basic lists, in their default order.
pub const BASIC: [&str; 5] = ["reading", "planned", "completed", "on_hold", "dropped"];
/// Own lists per user — a sanity limit.
const MAX_LISTS: i64 = 100;
const MAX_NAME: usize = 64;

/// Creates the basic lists the user doesn't have yet.
fn ensure_basic(c: &Connection, user: i64) -> rusqlite::Result<()> {
    for (i, kind) in BASIC.iter().enumerate() {
        c.execute(
            "INSERT OR IGNORE INTO lists (user_id, kind, name, position, created_at) VALUES (?1, ?2, NULL, ?3, ?4)",
            params![user, kind, i as i64, now()],
        )?;
    }
    Ok(())
}

/// The basic list the book is in for the user (`reading`, `planned`, …).
fn basic_of(c: &Connection, user: i64, book: i64) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT l.kind FROM list_books lb JOIN lists l ON l.id = lb.list_id WHERE l.user_id = ?1 AND lb.book_id = ?2 AND l.kind IS NOT NULL",
        [user, book],
        |r| r.get(0),
    )
    .optional()
}

/// Puts the book in the basic list `kind` (out of the other basic ones).
fn set_basic(c: &Connection, user: i64, book: i64, kind: &str) -> rusqlite::Result<()> {
    ensure_basic(c, user)?;
    c.execute(
        "DELETE FROM list_books WHERE book_id = ?1 AND list_id IN (SELECT id FROM lists WHERE user_id = ?2 AND kind IS NOT NULL AND kind != ?3)",
        params![book, user, kind],
    )?;
    c.execute(
        "INSERT OR IGNORE INTO list_books (list_id, book_id, added_at) SELECT id, ?3, ?4 FROM lists WHERE user_id = ?1 AND kind = ?2",
        params![user, kind, book, now()],
    )?;
    Ok(())
}

/// The translation (or, if it doesn't say, the series) is still coming out.
pub fn still_coming_out(c: &Connection, book: i64) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT COALESCE(translation_status, status) IN ('ongoing', 'hiatus', 'upcoming') FROM books WHERE id = ?1",
        [book],
        |r| r.get::<_, Option<bool>>(0),
    )
    .map(|v| v.unwrap_or(false))
}

/// Reading progress was saved: the book follows into "reading" or "completed" (see the module docs).
pub fn follow_progress(c: &Connection, user: i64, book: i64, finished: bool) -> rusqlite::Result<()> {
    let current = basic_of(c, user, book)?;
    let target = if finished && !still_coming_out(c, book)? { "completed" } else { "reading" };
    let moves = match current.as_deref() {
        None | Some("planned") => true,
        Some("reading") => finished,
        _ => false,
    };
    if moves && current.as_deref() != Some(target) {
        set_basic(c, user, book, target)?;
    }
    Ok(())
}

/// A list of `user`, or 404.
fn own_list(c: &Connection, user: i64, id: i64) -> ApiResult<(i64, Option<String>)> {
    c.query_row("SELECT id, kind FROM lists WHERE id = ?1 AND user_id = ?2", [id, user], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .ok_or_else(|| ApiError::not_found(format!("list {id}")))
}

fn clean_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!("a list name is 1–{MAX_NAME} characters")));
    }
    Ok(name.to_string())
}

fn lists_json(c: &Connection, user: i64) -> ApiResult<Value> {
    ensure_basic(c, user)?;
    let mut stmt = c.prepare(
        "SELECT l.id, l.kind, l.name, l.position,
                (SELECT COUNT(*) FROM list_books lb JOIN books b ON b.id = lb.book_id WHERE lb.list_id = l.id AND b.missing = 0)
         FROM lists l WHERE l.user_id = ?1 ORDER BY l.position, l.id",
    )?;
    let items = stmt
        .query_map([user], |r| {
            Ok(json!({ "id": r.get::<_, i64>(0)?, "kind": r.get::<_, Option<String>>(1)?, "name": r.get::<_, Option<String>>(2)?,
                       "position": r.get::<_, i64>(3)?, "count": r.get::<_, i64>(4)? }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let in_lists: i64 = c.query_row(
        "SELECT COUNT(DISTINCT lb.book_id) FROM list_books lb JOIN lists l ON l.id = lb.list_id JOIN books b ON b.id = lb.book_id
         WHERE l.user_id = ?1 AND b.missing = 0",
        [user],
        |r| r.get(0),
    )?;
    let library: i64 = c.query_row("SELECT COUNT(*) FROM books WHERE missing = 0", [], |r| r.get(0))?;
    Ok(json!({ "items": items, "in_lists": in_lists, "library": library }))
}

/// `GET /lists`: the user's lists with the number of books; `in_lists` — books in any of them,
/// `library` — all books.
pub async fn all(State(state): State<AppState>, user: Signed) -> ApiResult<Json<Value>> {
    Ok(Json(state.db.call(move |c| lists_json(c, user.id)).await?))
}

#[derive(Deserialize)]
pub struct NewList {
    name: String,
}

/// `POST /lists`: a new list of the user's own, at the end.
pub async fn create(State(state): State<AppState>, user: Signed, Json(body): Json<NewList>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = clean_name(&body.name)?;
    let value = state
        .db
        .call(move |c| {
            ensure_basic(c, user.id)?;
            let own: i64 = c.query_row("SELECT COUNT(*) FROM lists WHERE user_id = ?1 AND kind IS NULL", [user.id], |r| r.get(0))?;
            if own >= MAX_LISTS {
                return Err(ApiError::conflict("too_many", format!("at most {MAX_LISTS} lists")));
            }
            let position: i64 = c.query_row("SELECT COALESCE(MAX(position), 0) + 1 FROM lists WHERE user_id = ?1", [user.id], |r| r.get(0))?;
            c.execute("INSERT INTO lists (user_id, kind, name, position, created_at) VALUES (?1, NULL, ?2, ?3, ?4)", params![user.id, name, position, now()])?;
            let id = c.last_insert_rowid();
            Ok(json!({ "id": id, "kind": null, "name": name, "position": position, "count": 0 }))
        })
        .await?;
    Ok((StatusCode::CREATED, Json(value)))
}

#[derive(Deserialize)]
pub struct ListChange {
    /// A new name; for a basic list `null` (or "") brings back the default one.
    #[serde(default, deserialize_with = "serde_with_none::deserialize")]
    name: Option<Option<String>>,
}

/// Distinguishes a missing `name` from `"name": null`.
mod serde_with_none {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
        Ok(Some(Option::deserialize(d)?))
    }
}

/// `PUT /lists/{id}`: rename.
pub async fn update(State(state): State<AppState>, user: Signed, Path(id): Path<i64>, Json(body): Json<ListChange>) -> ApiResult<Json<Value>> {
    let value = state
        .db
        .call(move |c| {
            let (_, kind) = own_list(c, user.id, id)?;
            if let Some(name) = body.name {
                let name = match name.as_deref().map(str::trim) {
                    None | Some("") if kind.is_some() => None,
                    None | Some("") => return Err(ApiError::bad_request("a list of your own needs a name")),
                    Some(n) => Some(clean_name(n)?),
                };
                c.execute("UPDATE lists SET name = ?1 WHERE id = ?2", params![name, id])?;
            }
            lists_json(c, user.id)
        })
        .await?;
    Ok(Json(value))
}

/// `DELETE /lists/{id}`: only lists of the user's own (the books stay in the library).
pub async fn remove(State(state): State<AppState>, user: Signed, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    state
        .db
        .call(move |c| {
            let (_, kind) = own_list(c, user.id, id)?;
            if kind.is_some() {
                return Err(ApiError::bad_request("a basic list can't be deleted (it can be renamed)"));
            }
            c.execute("DELETE FROM lists WHERE id = ?1", [id])?;
            Ok(())
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Order {
    ids: Vec<i64>,
}

/// `PUT /lists/order`: `{ids}` — the user's lists in the new order (missing ones go after them).
pub async fn reorder(State(state): State<AppState>, user: Signed, Json(body): Json<Order>) -> ApiResult<Json<Value>> {
    let value = state
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            ensure_basic(&tx, user.id)?;
            let rest: Vec<i64> = {
                let mut stmt = tx.prepare("SELECT id FROM lists WHERE user_id = ?1 ORDER BY position, id")?;
                stmt.query_map([user.id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let order: Vec<i64> =
                body.ids.iter().copied().filter(|id| rest.contains(id)).chain(rest.iter().copied().filter(|id| !body.ids.contains(id))).collect();
            for (i, id) in order.iter().enumerate() {
                tx.execute("UPDATE lists SET position = ?1 WHERE id = ?2", params![i as i64, id])?;
            }
            tx.commit()?;
            lists_json(c, user.id)
        })
        .await?;
    Ok(Json(value))
}

/// The lists of `user` that contain book `book`.
fn book_lists(c: &Connection, user: i64, book: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt =
        c.prepare("SELECT l.id FROM list_books lb JOIN lists l ON l.id = lb.list_id WHERE l.user_id = ?1 AND lb.book_id = ?2 ORDER BY l.position")?;
    stmt.query_map([user, book], |r| r.get(0))?.collect()
}

/// `GET /books/{id}/lists` → `{lists: [list ids]}`.
pub async fn of_book(State(state): State<AppState>, user: Signed, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let ids = state
        .db
        .call(move |c| {
            book_row(c, id)?;
            Ok(book_lists(c, user.id, id)?)
        })
        .await?;
    Ok(Json(json!({ "lists": ids })))
}

/// `PUT /books/{id}/lists/{list}`: adds the book. Into a basic list — out of the other basic ones.
pub async fn add_book(State(state): State<AppState>, user: Signed, Path((book, list)): Path<(i64, i64)>) -> ApiResult<Json<Value>> {
    let ids = state
        .db
        .call(move |c| {
            book_row(c, book)?;
            let (_, kind) = own_list(c, user.id, list)?;
            let tx = c.transaction()?;
            if kind.is_some() {
                tx.execute(
                    "DELETE FROM list_books WHERE book_id = ?1 AND list_id IN (SELECT id FROM lists WHERE user_id = ?2 AND kind IS NOT NULL AND id != ?3)",
                    params![book, user.id, list],
                )?;
            }
            tx.execute("INSERT OR IGNORE INTO list_books (list_id, book_id, added_at) VALUES (?1, ?2, ?3)", params![list, book, now()])?;
            if kind.as_deref() == Some("completed") {
                // Completed by hand: read to the end.
                tx.execute(
                    "INSERT INTO progress (user_id, book_id, page, chunk, offset, finished, updated_at, furthest)
                     SELECT ?1, id, MAX(page_count - 1, 0), NULL, NULL, 1, ?3, MAX(page_count - 1, 0) FROM books WHERE id = ?2
                     ON CONFLICT (user_id, book_id) DO UPDATE SET finished = 1, updated_at = excluded.updated_at, furthest = excluded.furthest",
                    params![user.id, book, now()],
                )?;
            }
            tx.commit()?;
            Ok(book_lists(c, user.id, book)?)
        })
        .await?;
    Ok(Json(json!({ "lists": ids })))
}

/// `DELETE /books/{id}/lists/{list}`
pub async fn remove_book(State(state): State<AppState>, user: Signed, Path((book, list)): Path<(i64, i64)>) -> ApiResult<Json<Value>> {
    let ids = state
        .db
        .call(move |c| {
            own_list(c, user.id, list)?;
            c.execute("DELETE FROM list_books WHERE list_id = ?1 AND book_id = ?2", [list, book])?;
            Ok(book_lists(c, user.id, book)?)
        })
        .await?;
    Ok(Json(json!({ "lists": ids })))
}
