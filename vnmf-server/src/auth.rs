// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Users and sessions.
//!
//! Passwords are hashed with Argon2id. A session is a random token; the database keeps only its
//! SHA-256, so a leaked database doesn't let anyone in. Apps send the token as
//! `Authorization: Bearer <token>`; a browser gets it in an `HttpOnly` cookie as well. With the cookie,
//! changing requests must also carry the `X-VNMF-CSRF: 1` header — a form on another site can't set it.

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{Method, header};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{ApiError, ApiResult};
use crate::{AppState, now};

pub const COOKIE: &str = "vnmf_session";
pub const CSRF_HEADER: &str = "x-vnmf-csrf";
/// `last_used_at` is updated at most this often (seconds) — not a write on every request.
const TOUCH_INTERVAL: i64 = 3600;

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| anyhow::anyhow!("random: {e}"))?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| anyhow::anyhow!("salt: {e}"))?;
    Ok(Argon2::default().hash_password(password.as_bytes(), &salt).map_err(|e| anyhow::anyhow!("hashing: {e}"))?.to_string())
}

/// A real hash of a random password: checked when the user doesn't exist, so a login attempt takes the
/// same time either way.
pub fn dummy_hash() -> &'static str {
    static HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let mut raw = [0u8; 16];
        let _ = getrandom::fill(&mut raw);
        hash_password(&hex::encode(raw)).unwrap_or_default()
    });
    &HASH
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

pub fn check_new_password(password: &str) -> ApiResult<()> {
    if password.chars().count() < 8 {
        return Err(ApiError::bad_request("the password must be at least 8 characters"));
    }
    Ok(())
}

pub fn check_user_name(name: &str) -> ApiResult<()> {
    let ok = !name.is_empty() && name.chars().count() <= 64 && name.chars().all(|c| c.is_alphanumeric() || "._-".contains(c));
    if !ok {
        return Err(ApiError::bad_request("a user name is 1–64 letters, digits, '.', '_' or '-'"));
    }
    Ok(())
}

pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Creates a session; returns the token (shown once).
pub fn create_session(conn: &rusqlite::Connection, user_id: i64, label: Option<&str>) -> ApiResult<String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|e| anyhow::anyhow!("random: {e}"))?;
    let token = hex::encode(raw);
    let t = now();
    conn.execute(
        "INSERT INTO sessions (token_hash, user_id, label, created_at, last_used_at) VALUES (?1, ?2, ?3, ?4, ?4)",
        params![token_hash(&token), user_id, label, t],
    )?;
    Ok(token)
}

pub fn delete_session(conn: &rusqlite::Connection, token: &str) -> ApiResult<()> {
    conn.execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash(token)])?;
    Ok(())
}

/// Creates a user (the command line and the admin API).
pub fn create_user(conn: &rusqlite::Connection, name: &str, password: &str, admin: bool) -> ApiResult<i64> {
    check_user_name(name)?;
    check_new_password(password)?;
    let hash = hash_password(password)?;
    match conn.execute("INSERT INTO users (name, password_hash, is_admin, created_at) VALUES (?1, ?2, ?3, ?4)", params![name, hash, admin, now()]) {
        Ok(_) => Ok(conn.last_insert_rowid()),
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
            Err(ApiError::conflict("exists", format!("user {name} already exists")))
        }
        Err(e) => Err(e.into()),
    }
}

/// The signed-in user. As an extractor it rejects anonymous requests with 401.
#[derive(Clone, Debug, Serialize)]
pub struct User {
    pub id: i64,
    /// The login: fixed, what the user signs in with.
    pub name: String,
    /// What others see; changed freely (`null` — the login is shown).
    pub display_name: Option<String>,
    pub is_admin: bool,
    /// The avatar's address (it changes with the avatar), `null` — none.
    pub avatar: Option<String>,
    /// The session token of this request (to sign out).
    #[serde(skip)]
    pub token: String,
}

/// The address of user `id`'s avatar set at `at`.
pub fn avatar_url(id: i64, at: Option<i64>) -> Option<String> {
    at.map(|at| format!("/api/v1/users/{id}/avatar?v={at}"))
}

/// A signed-in administrator (403 for others).
pub struct Admin(pub User);

/// The token from `Authorization: Bearer`, or from the cookie; `bool` — it came from the cookie.
fn request_token(parts: &Parts) -> Option<(String, bool)> {
    if let Some(token) = parts.headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")) {
        return Some((token.trim().to_string(), false));
    }
    parts
        .headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(COOKIE).and_then(|rest| rest.strip_prefix('=')))
        .map(|t| (t.to_string(), true))
}

impl FromRequestParts<AppState> for User {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let (token, from_cookie) = request_token(parts).ok_or_else(ApiError::unauthorized)?;
        let safe = matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS);
        if from_cookie && !safe && !parts.headers.contains_key(CSRF_HEADER) {
            return Err(ApiError::forbidden(format!("a request authorized by the cookie needs the {CSRF_HEADER} header")));
        }
        let hash = token_hash(&token);
        let user = state
            .db
            .call(move |conn| {
                let row = conn
                    .query_row(
                        "SELECT u.id, u.name, u.is_admin, s.last_used_at, u.avatar_at, u.display_name FROM sessions s JOIN users u ON u.id = s.user_id
                         WHERE s.token_hash = ?1",
                        [&hash],
                        |r| {
                            Ok((
                                r.get::<_, i64>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, bool>(2)?,
                                r.get::<_, i64>(3)?,
                                r.get::<_, Option<i64>>(4)?,
                                r.get::<_, Option<String>>(5)?,
                            ))
                        },
                    )
                    .optional()?;
                if let Some((_, _, _, used, _, _)) = &row
                    && now() - used > TOUCH_INTERVAL
                {
                    conn.execute("UPDATE sessions SET last_used_at = ?1 WHERE token_hash = ?2", params![now(), hash])?;
                }
                Ok(row)
            })
            .await?;
        let (id, name, is_admin, _, avatar_at, display_name) = user.ok_or_else(ApiError::unauthorized)?;
        Ok(User { id, name, display_name, is_admin, avatar: avatar_url(id, avatar_at), token })
    }
}

impl FromRequestParts<AppState> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let user = User::from_request_parts(parts, state).await?;
        if !user.is_admin {
            return Err(ApiError::forbidden("only an administrator can do this"));
        }
        Ok(Admin(user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords() {
        let hash = hash_password("correct horse").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong horse", &hash));
        assert!(!verify_password("x", "not a hash"));
    }

    #[test]
    fn names() {
        assert!(check_user_name("nicole_h.2").is_ok());
        assert!(check_user_name("Ніколь").is_ok());
        assert!(check_user_name("a b").is_err());
        assert!(check_user_name("").is_err());
    }
}
