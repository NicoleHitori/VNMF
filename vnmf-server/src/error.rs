// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! API errors: `{"error": "<code>", "message": "<for people>"}` with a matching HTTP status.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    /// Stable machine-readable code (`not_found`, `damaged`, …).
    pub code: &'static str,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "sign in first")
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", format!("{} not found", what.into()))
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    /// The request is fine, but the file doesn't allow it (damaged, wrong kind…).
    pub fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }

    pub fn busy() -> Self {
        Self::conflict("busy", "another operation on this book is in progress")
    }

    pub fn read_only() -> Self {
        Self::forbidden("the book is in a read-only library (start the server with --writable-libraries to allow changes)")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.code, "message": self.message }))).into_response()
    }
}

/// Unexpected failures (I/O, database): 500; the details go to the server log only.
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("{e:#}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal server error (see the server log)")
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        anyhow::Error::from(e).into()
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        anyhow::Error::from(e).into()
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(e: tokio::task::JoinError) -> Self {
        anyhow::Error::from(e).into()
    }
}
