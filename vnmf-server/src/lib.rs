// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! VNMF library server.
//!
//! The `.vnmf` files are the source of truth: the database (SQLite) holds only what can't be read from
//! them — users, reading progress, bookmarks — and an index of the library for listing and search,
//! which a rescan can always rebuild. Images, sound and text are served straight from the
//! memory-mapped files, without copying or decoding.
//!
//! The HTTP API is described in `docs/SERVER.md`.

pub mod api;
pub mod auth;
pub mod book;
pub mod config;
pub mod db;
pub mod error;
pub mod http;
pub mod library;
pub mod tiles;
pub mod web;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub use config::Config;

/// Shared state of all request handlers.
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub config: Config,
    pub db: db::Db,
    pub books: book::BookCache,
    pub jobs: library::Jobs,
    /// Books being rewritten (metadata, covers, protection, repair) — one operation per book at a time.
    busy: Mutex<HashSet<i64>>,
    /// Cutting tiles: one fragment at a time (decoding takes hundreds of megabytes).
    pub tiling: tokio::sync::Mutex<()>,
}

impl std::ops::Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl AppState {
    /// Opens (creating if needed) the data folder and the database.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        config.prepare_dirs()?;
        let db = db::Db::open(&config.db_path())?;
        Ok(Self { inner: Arc::new(Inner { config, db, books: book::BookCache::new(64), jobs: library::Jobs::default(), busy: Mutex::new(HashSet::new()), tiling: tokio::sync::Mutex::new(()) }) })
    }

    /// Marks a book as being rewritten; `None` — it already is. Released when the guard is dropped.
    pub fn lock_book(&self, id: i64) -> Option<BusyGuard> {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        busy.insert(id).then(|| BusyGuard { state: self.clone(), id })
    }
}

pub struct BusyGuard {
    state: AppState,
    id: i64,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.state.busy.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
    }
}

/// The whole application: API routes with their middleware.
pub fn app(state: AppState) -> axum::Router {
    api::router(state)
}

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
