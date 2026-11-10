// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! HTTP API, version 1: everything under `/api/v1`. See `docs/SERVER.md`.

mod account;
mod books;
mod library;
mod lists;
mod manage;
mod progress;
mod read;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderValue, Method, header};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::AppState;
use crate::auth::{CSRF_HEADER, User};
use crate::book::OpenBook;
use crate::error::{ApiError, ApiResult};
use crate::library::{BookRow, book_row};

pub fn router(state: AppState) -> Router {
    let upload_limit = usize::try_from(state.config.max_upload).unwrap_or(usize::MAX);
    let api = Router::new()
        .route("/server", get(server_info))
        // Account and users
        .route("/auth/login", post(account::login))
        .route("/auth/logout", post(account::logout))
        .route("/me", get(account::me).put(account::rename))
        .route("/me/stats", get(account::stats))
        .route("/me/sessions", get(account::sessions).delete(account::end_other_sessions))
        .route("/me/sessions/{sid}", delete(account::end_session))
        .route("/me/password", put(account::change_password))
        .route("/me/settings", get(account::get_settings).put(account::put_settings))
        .route("/users", get(account::list_users).post(account::create_user))
        .route("/users/{id}", put(account::update_user).delete(account::delete_user))
        .route("/users/{id}/avatar", get(account::avatar))
        .route("/me/avatar", put(account::put_avatar).delete(account::delete_avatar).layer(DefaultBodyLimit::max(2 << 20)))
        // Library
        .route("/library", get(library::status))
        .route("/library/scan", post(library::scan))
        .route("/library/verify", post(library::verify))
        .route("/library/duplicates", get(library::duplicates))
        .route("/books", get(books::list).post(books::upload).layer(DefaultBodyLimit::max(upload_limit)))
        .route("/books/facets", get(books::facets))
        .route("/series", get(books::series))
        .route("/books/{id}", get(books::get).delete(books::remove))
        .route("/books/{id}/copies/{other}", delete(books::remove_copy))
        // Reading
        .route("/books/{id}/manifest", get(read::manifest))
        .route("/books/{id}/contents", get(read::contents))
        .route("/books/{id}/pages", get(read::pages))
        .route("/books/{id}/groups", get(read::groups))
        .route("/books/{id}/aux", get(read::aux_list))
        .route("/books/{id}/aux/{k}", get(read::aux_data))
        .route("/books/{id}/strip-sfx", get(read::strip_sfx))
        .route("/books/{id}/chunks/{n}", get(read::chunk))
        .route("/books/{id}/chunks/{n}/tiles/{t}", get(read::chunk_tile))
        .route("/books/{id}/pages/{n}/image", get(read::page_image))
        .route("/books/{id}/pages/{n}/dark/{part}", get(read::dark))
        .route("/books/{id}/pages/{n}/text", get(read::text))
        .route("/books/{id}/pages/{n}/playback", get(read::playback))
        .route("/books/{id}/cover", get(read::main_cover))
        .route("/books/{id}/covers", get(read::covers).post(manage::add_cover).layer(DefaultBodyLimit::max(64 << 20)))
        .route("/books/{id}/covers/{i}", get(read::cover).delete(manage::delete_cover))
        .route("/books/{id}/metadata", get(read::metadata).put(manage::put_metadata).layer(DefaultBodyLimit::max(16 << 20)))
        .route("/books/{id}/info", get(read::raw_info))
        .route("/books/{id}/comicinfo.xml", get(read::comicinfo))
        .route("/books/{id}/file", get(read::file))
        .route("/books/{id}/export", get(read::export))
        // Integrity and protection
        .route("/books/{id}/health", get(manage::health))
        .route("/books/{id}/repair", post(manage::repair))
        .route("/books/{id}/protect", post(manage::protect))
        // Per user
        .route("/books/{id}/progress", get(progress::get).put(progress::put).delete(progress::remove))
        .route("/books/{id}/bookmarks", get(progress::bookmarks).post(progress::add_bookmark))
        .route("/bookmarks/{id}", delete(progress::delete_bookmark))
        .route("/progress", get(progress::recent))
        .route("/lists", get(lists::all).post(lists::create))
        .route("/lists/order", put(lists::reorder))
        .route("/lists/{id}", put(lists::update).delete(lists::remove))
        .route("/books/{id}/lists", get(lists::of_book))
        .route("/books/{id}/lists/{list}", put(lists::add_book).delete(lists::remove_book))
        .with_state(state.clone());

    let mut app =
        Router::new().nest("/api/v1", api).route("/health", get(|| async { "ok" })).fallback(get(crate::web::serve)).layer(TraceLayer::new_for_http());
    if !state.config.cors_origins.is_empty() {
        let origins: Vec<HeaderValue> = state.config.cors_origins.iter().filter_map(|o| HeaderValue::from_str(o).ok()).collect();
        app = app.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods([Method::GET, Method::HEAD, Method::POST, Method::PUT, Method::DELETE])
                .allow_headers([
                    header::AUTHORIZATION,
                    header::CONTENT_TYPE,
                    header::RANGE,
                    header::IF_NONE_MATCH,
                    header::HeaderName::from_static(CSRF_HEADER),
                ])
                .expose_headers([header::ETAG, header::CONTENT_RANGE, header::CONTENT_DISPOSITION, header::HeaderName::from_static("x-vnmf-crc")])
                .allow_credentials(true),
        );
    }
    app
}

/// What the server is and can do; no sign-in needed (clients check it before showing a login form).
async fn server_info(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let users: i64 = state.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)).await?;
    let (major, minor) = vnmf_core::spec::DEFAULT_VERSION;
    Ok(Json(json!({
        "name": "vnmf-server",
        "version": env!("CARGO_PKG_VERSION"),
        "api": 1,
        "format_version": format!("{major}.{minor}"),
        "metadata_schema": vnmf::metadata::SCHEMA,
        "has_users": users > 0,
        "max_upload": state.config.max_upload,
        "upload_formats": ["vnmf", "cbz", "zip"],
        "export_formats": ["vnmf", "cbz", "zip"],
        "features": ["progress", "bookmarks", "settings", "metadata-edit", "covers-edit", "protect", "repair", "health", "export", "upload-cbz"],
    })))
}

/// `?v=<book version>`: the URL is then immutable (see `crate::http`).
#[derive(Deserialize, Default)]
struct Versioned {
    v: Option<String>,
}

impl Versioned {
    fn immutable(&self, book: &OpenBook) -> bool {
        self.v.as_deref() == Some(book.version.as_str())
    }
}

/// The book's row and the open file. A missing file is 410.
async fn open_book(state: &AppState, id: i64) -> ApiResult<(BookRow, Arc<OpenBook>)> {
    let row = state.db.call(move |c| book_row(c, id)).await?;
    if row.missing {
        return Err(ApiError::new(axum::http::StatusCode::GONE, "missing", "the book's file is missing — rescan the library"));
    }
    let book = state.books.get(id, row.path.clone()).await?;
    Ok((row, book))
}

/// Whether the server may change this book's file.
fn writable(state: &AppState, row: &BookRow) -> bool {
    row.managed || state.config.writable_libraries
}

fn require_writable(state: &AppState, row: &BookRow) -> ApiResult<()> {
    if writable(state, row) { Ok(()) } else { Err(ApiError::read_only()) }
}

/// Any signed-in user (for handlers that need only the check).
type Signed = User;
