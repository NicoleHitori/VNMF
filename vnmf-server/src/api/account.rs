// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Signing in and out, the user's own settings, user management (administrators).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::{self, Admin, COOKIE, User};
use crate::error::{ApiError, ApiResult};

#[derive(Deserialize)]
pub struct Login {
    username: String,
    password: String,
    /// Shown in the list of sessions: "Android", "Firefox on laptop"…
    label: Option<String>,
}

/// `POST /auth/login` → `{token, user}`; also sets the session cookie for browsers.
pub async fn login(State(state): State<AppState>, Json(body): Json<Login>) -> ApiResult<Response> {
    let secure = state.config.secure_cookies;
    let (token, user) = state
        .db
        .call(move |conn| {
            let row = conn
                .query_row("SELECT id, name, password_hash, is_admin, avatar_at, display_name FROM users WHERE name = ?1", [&body.username], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, bool>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                })
                .optional()?;
            // The hash is checked even for an unknown user, so the answer takes the same time.
            let hash = row.as_ref().map_or(auth::dummy_hash(), |r| r.2.as_str());
            let valid = auth::verify_password(&body.password, hash);
            let Some((id, name, _, is_admin, avatar_at, display_name)) = row.filter(|_| valid) else {
                return Err(ApiError::new(StatusCode::UNAUTHORIZED, "bad_credentials", "wrong user name or password"));
            };
            let label = body.label.as_deref().map(|l| l.chars().take(100).collect::<String>());
            let token = auth::create_session(conn, id, label.as_deref())?;
            Ok((token, User { id, name, display_name, is_admin, avatar: auth::avatar_url(id, avatar_at), token: String::new() }))
        })
        .await?;
    let cookie = format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000{}", if secure { "; Secure" } else { "" });
    let mut response = Json(json!({ "token": token, "user": user })).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    Ok(response)
}

/// `POST /auth/logout`: ends this session.
pub async fn logout(State(state): State<AppState>, user: User) -> ApiResult<Response> {
    state.db.call(move |conn| auth::delete_session(conn, &user.token)).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_static("vnmf_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"));
    Ok(response)
}

pub async fn me(user: User) -> Json<User> {
    Json(user)
}

#[derive(Deserialize)]
pub struct PasswordChange {
    old_password: String,
    new_password: String,
}

/// `PUT /me/password`: other sessions of the user are ended.
pub async fn change_password(State(state): State<AppState>, user: User, Json(body): Json<PasswordChange>) -> ApiResult<StatusCode> {
    auth::check_new_password(&body.new_password)?;
    state
        .db
        .call(move |conn| {
            let hash: String = conn.query_row("SELECT password_hash FROM users WHERE id = ?1", [user.id], |r| r.get(0))?;
            if !auth::verify_password(&body.old_password, &hash) {
                return Err(ApiError::forbidden("the current password is wrong"));
            }
            let new_hash = auth::hash_password(&body.new_password)?;
            let tx = conn.transaction()?;
            tx.execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", params![new_hash, user.id])?;
            tx.execute("DELETE FROM sessions WHERE user_id = ?1 AND token_hash != ?2", params![user.id, auth::token_hash(&user.token)])?;
            tx.commit()?;
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}

/// `GET /me/settings`: the reader settings saved by the user's apps (an arbitrary JSON object).
pub async fn get_settings(State(state): State<AppState>, user: User) -> ApiResult<Json<Value>> {
    let text: String = state.db.call(move |c| Ok(c.query_row("SELECT settings FROM users WHERE id = ?1", [user.id], |r| r.get(0))?)).await?;
    Ok(Json(serde_json::from_str(&text).unwrap_or_else(|_| json!({}))))
}

/// `PUT /me/settings`: replaces them (up to 64 KiB).
pub async fn put_settings(State(state): State<AppState>, user: User, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if !body.is_object() {
        return Err(ApiError::bad_request("settings must be a JSON object"));
    }
    let text = body.to_string();
    if text.len() > 64 << 10 {
        return Err(ApiError::bad_request("settings are limited to 64 KiB"));
    }
    state.db.call(move |c| Ok(c.execute("UPDATE users SET settings = ?1 WHERE id = ?2", params![text, user.id])?)).await?;
    Ok(Json(body))
}

pub async fn list_users(State(state): State<AppState>, _: Admin) -> ApiResult<Json<Value>> {
    let users = state
        .db
        .call(|c| {
            let mut stmt = c.prepare(
                "SELECT u.id, u.name, u.is_admin, u.created_at, (SELECT MAX(last_used_at) FROM sessions s WHERE s.user_id = u.id), u.avatar_at,
                     u.display_name
                 FROM users u ORDER BY u.name",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(json!({ "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "display_name": r.get::<_, Option<String>>(6)?,
                           "is_admin": r.get::<_, bool>(2)?,
                           "created_at": r.get::<_, i64>(3)?, "last_seen_at": r.get::<_, Option<i64>>(4)?,
                           "avatar": auth::avatar_url(r.get(0)?, r.get(5)?) }))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await?;
    Ok(Json(json!({ "users": users })))
}

#[derive(Deserialize)]
pub struct NewUser {
    name: String,
    password: String,
    #[serde(default)]
    is_admin: bool,
}

pub async fn create_user(State(state): State<AppState>, _: Admin, Json(body): Json<NewUser>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = body.name.clone();
    let id = state.db.call(move |c| auth::create_user(c, &body.name, &body.password, body.is_admin)).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "name": name, "is_admin": body.is_admin }))))
}

#[derive(Deserialize)]
pub struct UserChange {
    password: Option<String>,
    is_admin: Option<bool>,
}

/// `PUT /users/{id}`: a new password (ends the user's sessions) and/or the admin flag.
pub async fn update_user(State(state): State<AppState>, _: Admin, Path(id): Path<i64>, Json(body): Json<UserChange>) -> ApiResult<StatusCode> {
    if let Some(p) = &body.password {
        auth::check_new_password(p)?;
    }
    state
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if tx.query_row("SELECT 1 FROM users WHERE id = ?1", [id], |_| Ok(())).optional()?.is_none() {
                return Err(ApiError::not_found(format!("user {id}")));
            }
            if let Some(p) = &body.password {
                tx.execute("UPDATE users SET password_hash = ?1 WHERE id = ?2", params![auth::hash_password(p)?, id])?;
                tx.execute("DELETE FROM sessions WHERE user_id = ?1", [id])?;
            }
            if let Some(a) = body.is_admin {
                // Someone must stay able to manage the server: the last administrator stays one
                // (one's own rights can be given up while there's another).
                let others: i64 = tx.query_row("SELECT COUNT(*) FROM users WHERE is_admin = 1 AND id != ?1", [id], |r| r.get(0))?;
                if !a && others == 0 {
                    return Err(ApiError::conflict("last_admin", "the last administrator can't lose the rights: make someone else an administrator first"));
                }
                tx.execute("UPDATE users SET is_admin = ?1 WHERE id = ?2", params![a, id])?;
            }
            tx.commit()?;
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}

/// `DELETE /users/{id}`: with their sessions, progress and bookmarks.
pub async fn delete_user(State(state): State<AppState>, Admin(admin): Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    if id == admin.id {
        return Err(ApiError::bad_request("you can't delete yourself"));
    }
    let deleted = state.db.call(move |c| Ok(c.execute("DELETE FROM users WHERE id = ?1", [id])?)).await?;
    if deleted == 0 {
        return Err(ApiError::not_found(format!("user {id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Rename {
    /// The nickname; empty or `null` — show the login.
    display_name: Option<String>,
}

const MAX_DISPLAY_NAME: usize = 64;

/// `PUT /me`: a new nickname (`{display_name}`). The login (`name`) doesn't change: it's what the
/// user signs in with.
pub async fn rename(State(state): State<AppState>, user: User, Json(body): Json<Rename>) -> ApiResult<Json<User>> {
    let display_name = body.display_name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    if let Some(n) = &display_name {
        if n.chars().count() > MAX_DISPLAY_NAME {
            return Err(ApiError::bad_request(format!("a nickname is up to {MAX_DISPLAY_NAME} characters")));
        }
        if n.chars().any(char::is_control) {
            return Err(ApiError::bad_request("a nickname can't contain control characters"));
        }
    }
    let value = display_name.clone();
    state.db.call(move |c| Ok(c.execute("UPDATE users SET display_name = ?1 WHERE id = ?2", params![value, user.id])?)).await?;
    Ok(Json(User { display_name, ..user }))
}

/// A session's public id: the start of the token's hash (neither the token nor enough to find it).
fn session_id(hash: &str) -> String {
    hash.chars().take(16).collect()
}

/// `GET /me/sessions`: where the user is signed in.
pub async fn sessions(State(state): State<AppState>, user: User) -> ApiResult<Json<Value>> {
    let current = auth::token_hash(&user.token);
    let items = state
        .db
        .call(move |c| {
            let mut stmt = c.prepare("SELECT token_hash, label, created_at, last_used_at FROM sessions WHERE user_id = ?1 ORDER BY last_used_at DESC")?;
            let rows = stmt.query_map([user.id], |r| {
                let hash: String = r.get(0)?;
                Ok(json!({ "id": session_id(&hash), "current": hash == current, "label": r.get::<_, Option<String>>(1)?,
                           "created_at": r.get::<_, i64>(2)?, "last_used_at": r.get::<_, i64>(3)? }))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await?;
    Ok(Json(json!({ "items": items })))
}

/// `DELETE /me/sessions/{id}`: signs that device out.
pub async fn end_session(State(state): State<AppState>, user: User, Path(sid): Path<String>) -> ApiResult<StatusCode> {
    if sid.len() != 16 || !sid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::not_found("session"));
    }
    let n = state
        .db
        .call(move |c| Ok(c.execute("DELETE FROM sessions WHERE user_id = ?1 AND substr(token_hash, 1, 16) = ?2", params![user.id, sid.to_ascii_lowercase()])?))
        .await?;
    if n == 0 {
        return Err(ApiError::not_found("session"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /me/sessions`: signs out everywhere except here.
pub async fn end_other_sessions(State(state): State<AppState>, user: User) -> ApiResult<Json<Value>> {
    let current = auth::token_hash(&user.token);
    let n = state.db.call(move |c| Ok(c.execute("DELETE FROM sessions WHERE user_id = ?1 AND token_hash != ?2", params![user.id, current])?)).await?;
    Ok(Json(json!({ "ended": n })))
}

/// `GET /me/stats`: the user's reading in numbers. `reading` and `finished` are the books in the
/// "reading" and "completed" lists — the same numbers the lists show.
pub async fn stats(State(state): State<AppState>, user: User) -> ApiResult<Json<Value>> {
    let value = state
        .db
        .call(move |c| {
            Ok(c.query_row(
                "SELECT
                 (SELECT COUNT(*) FROM list_books lb JOIN lists l ON l.id = lb.list_id JOIN books bb ON bb.id = lb.book_id
                     WHERE l.user_id = ?1 AND l.kind = 'reading' AND bb.missing = 0),
                 (SELECT COUNT(*) FROM list_books lb JOIN lists l ON l.id = lb.list_id JOIN books bb ON bb.id = lb.book_id
                     WHERE l.user_id = ?1 AND l.kind = 'completed' AND bb.missing = 0),
                 COALESCE(SUM(CASE WHEN p.finished = 1 THEN b.page_count ELSE MAX(p.page, p.furthest) + 1 END), 0),
                 (SELECT COUNT(*) FROM bookmarks WHERE user_id = ?1), MAX(p.updated_at)
                 FROM progress p JOIN books b ON b.id = p.book_id WHERE p.user_id = ?1",
                [user.id],
                |r| {
                    Ok(json!({ "reading": r.get::<_, i64>(0)?, "finished": r.get::<_, i64>(1)?, "pages_read": r.get::<_, i64>(2)?,
                               "bookmarks": r.get::<_, i64>(3)?, "last_read_at": r.get::<_, Option<i64>>(4)? }))
                },
            )?)
        })
        .await?;
    Ok(Json(value))
}

/// The largest avatar accepted: the web interface sends a small square, other clients should too.
const AVATAR_MAX: usize = 1 << 20;

/// `PUT /me/avatar`: the body is an image (PNG, JPEG, WebP, GIF, AVIF), up to 1 MiB.
pub async fn put_avatar(State(state): State<AppState>, user: User, body: axum::body::Bytes) -> ApiResult<Json<User>> {
    use vnmf_core::PageFormat;
    if body.len() > AVATAR_MAX {
        return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "an avatar is limited to 1 MiB"));
    }
    let format = PageFormat::detect(&body);
    if !matches!(format, PageFormat::Png | PageFormat::Jpeg | PageFormat::WebP | PageFormat::Gif | PageFormat::Avif) {
        return Err(ApiError::bad_request("the avatar must be a PNG, JPEG, WebP, GIF or AVIF image"));
    }
    let at = crate::now();
    let id = user.id;
    state
        .db
        .call(move |c| {
            Ok(c.execute("UPDATE users SET avatar = ?1, avatar_mime = ?2, avatar_at = ?3 WHERE id = ?4", params![body.to_vec(), format.mime_type(), at, id])?)
        })
        .await?;
    Ok(Json(User { avatar: auth::avatar_url(user.id, Some(at)), ..user }))
}

/// `DELETE /me/avatar`
pub async fn delete_avatar(State(state): State<AppState>, user: User) -> ApiResult<Json<User>> {
    let id = user.id;
    state.db.call(move |c| Ok(c.execute("UPDATE users SET avatar = NULL, avatar_mime = NULL, avatar_at = NULL WHERE id = ?1", [id])?)).await?;
    Ok(Json(User { avatar: None, ..user }))
}

/// `GET /users/{id}/avatar`: for any signed-in user (avatars are shown next to names).
pub async fn avatar(
    State(state): State<AppState>,
    _: User,
    Path(id): Path<i64>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    let row = state
        .db
        .call(move |c| {
            Ok(c.query_row("SELECT avatar, avatar_mime, avatar_at FROM users WHERE id = ?1 AND avatar IS NOT NULL", [id], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })
            .optional()?)
        })
        .await?;
    let (bytes, mime, at) = row.ok_or_else(|| ApiError::not_found("avatar"))?;
    let mime: &'static str = match mime.as_str() {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/webp" => "image/webp",
        "image/gif" => "image/gif",
        _ => "image/avif",
    };
    let data = crate::http::Data { etag: format!("\"av-{id}-{at:x}\""), bytes: bytes.into(), mime, damaged: None };
    // A URL with the current version (`?v=`, as in `User::avatar`) can be cached for good.
    let immutable = q.get("v").is_some_and(|v| *v == at.to_string());
    Ok(crate::http::respond(&headers, data, immutable))
}
