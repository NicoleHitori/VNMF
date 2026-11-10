// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! SQLite: one connection behind a mutex, used from blocking threads. For a home library that's plenty
//! (WAL; the heavy part — images — never touches the database).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use rusqlite::Connection;

use crate::error::{ApiError, ApiResult};

/// Schema migrations in order; `PRAGMA user_version` = how many have been applied.
const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    -- reader settings of the user's apps, an arbitrary JSON object
    settings      TEXT NOT NULL DEFAULT '{}',
    created_at    INTEGER NOT NULL
);

CREATE TABLE sessions (
    token_hash   TEXT PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    label        TEXT,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL
);

-- Index of the library: everything here is read from the files and rebuilt by a rescan.
CREATE TABLE books (
    id            INTEGER PRIMARY KEY,
    path          TEXT NOT NULL UNIQUE,
    managed       INTEGER NOT NULL,
    size          INTEGER NOT NULL,
    mtime         INTEGER NOT NULL,
    uuid          TEXT NOT NULL,
    title         TEXT NOT NULL,
    sort_title    TEXT NOT NULL,
    search        TEXT NOT NULL,
    kind          TEXT,
    status        TEXT,
    language      TEXT,
    authors       TEXT NOT NULL DEFAULT '[]',
    genres        TEXT NOT NULL DEFAULT '[]',
    tags          TEXT NOT NULL DEFAULT '[]',
    page_count    INTEGER NOT NULL,
    volume_count  INTEGER NOT NULL,
    chapter_count INTEGER NOT NULL,
    webtoon       INTEGER NOT NULL,
    rtl           INTEGER NOT NULL,
    has_music     INTEGER NOT NULL,
    has_sfx       INTEGER NOT NULL,
    has_text      INTEGER NOT NULL,
    has_dark      INTEGER NOT NULL,
    redundancy    INTEGER NOT NULL,
    format_version TEXT NOT NULL,
    recovered     INTEGER NOT NULL,
    missing       INTEGER NOT NULL DEFAULT 0,
    -- last integrity check: NULL — never; 'ok' / 'damaged'
    health        TEXT,
    health_issues TEXT,
    health_full   INTEGER,
    health_checked_at INTEGER,
    added_at      INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
CREATE INDEX books_uuid ON books(uuid);
CREATE INDEX books_sort ON books(sort_title);

CREATE TABLE progress (
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    book_id    INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    page       INTEGER NOT NULL,
    -- strip reading: the fragment and the fraction of its height at the top of the screen
    chunk      INTEGER,
    offset     REAL,
    finished   INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, book_id)
);
CREATE INDEX progress_recent ON progress(user_id, updated_at);

CREATE TABLE bookmarks (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    book_id    INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    page       INTEGER NOT NULL,
    chunk      INTEGER,
    offset     REAL,
    note       TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX bookmarks_book ON bookmarks(user_id, book_id);
"#,
    r#"
-- Avatars: a small image (the web interface sends a 256×256 square), kept in the database.
ALTER TABLE users ADD COLUMN avatar BLOB;
ALTER TABLE users ADD COLUMN avatar_mime TEXT;
ALTER TABLE users ADD COLUMN avatar_at INTEGER;
"#,
    r#"
-- Catalog filters: more metadata in the index.
ALTER TABLE books ADD COLUMN year INTEGER;
ALTER TABLE books ADD COLUMN origin_country TEXT;
ALTER TABLE books ADD COLUMN age_rating TEXT;
ALTER TABLE books ADD COLUMN publication TEXT;
ALTER TABLE books ADD COLUMN demographic TEXT;
ALTER TABLE books ADD COLUMN color TEXT;
ALTER TABLE books ADD COLUMN translation TEXT;
CREATE INDEX books_year ON books(year);
-- The next scan reindexes every book (it compares the modification time).
UPDATE books SET mtime = 0;
"#,
    r#"
-- Reading lists of each user: five basic ones (`kind`; a book is in at most one of them) and the
-- user's own. A basic list's `name` is NULL until renamed: clients show it in their language.
CREATE TABLE lists (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind       TEXT,
    name       TEXT,
    position   INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (user_id, kind)
);
CREATE TABLE list_books (
    list_id  INTEGER NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    book_id  INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    added_at INTEGER NOT NULL,
    PRIMARY KEY (list_id, book_id)
);
CREATE INDEX list_books_book ON list_books(book_id);
"#,
    r#"
-- The type is now also inferred (from genres, tags, the country, the webtoon flag): re-read every file.
UPDATE books SET mtime = 0;
"#,
    r#"
-- The furthest page the user reached: chapters before it are read, even when the position goes back.
ALTER TABLE progress ADD COLUMN furthest INTEGER NOT NULL DEFAULT 0;
UPDATE progress SET furthest = CASE WHEN finished THEN (SELECT MAX(page_count - 1, 0) FROM books WHERE id = book_id) ELSE page END;
"#,
    r#"
-- Books being read go to the basic lists by themselves: started — "reading", finished — "completed"
-- (only books in no basic list yet; from now on the server keeps them in step).
INSERT OR IGNORE INTO lists (user_id, kind, name, position, created_at)
SELECT u.user_id, k.kind, NULL, k.position, CAST(strftime('%s', 'now') AS INTEGER)
FROM (SELECT DISTINCT user_id FROM progress) u,
     (SELECT 'reading' AS kind, 0 AS position UNION ALL SELECT 'planned', 1 UNION ALL SELECT 'completed', 2
      UNION ALL SELECT 'on_hold', 3 UNION ALL SELECT 'dropped', 4) k;
INSERT OR IGNORE INTO list_books (list_id, book_id, added_at)
SELECT l.id, p.book_id, p.updated_at FROM progress p
JOIN lists l ON l.user_id = p.user_id AND l.kind = CASE WHEN p.finished THEN 'completed' ELSE 'reading' END
WHERE NOT EXISTS (SELECT 1 FROM list_books lb JOIN lists bl ON bl.id = lb.list_id
                  WHERE lb.book_id = p.book_id AND bl.user_id = p.user_id AND bl.kind IS NOT NULL);
-- And a book put in "completed" by hand is read to the end.
INSERT INTO progress (user_id, book_id, page, chunk, offset, finished, updated_at, furthest)
SELECT l.user_id, lb.book_id, MAX(b.page_count - 1, 0), NULL, NULL, 1, lb.added_at, MAX(b.page_count - 1, 0)
FROM list_books lb JOIN lists l ON l.id = lb.list_id JOIN books b ON b.id = lb.book_id WHERE l.kind = 'completed'
ON CONFLICT (user_id, book_id) DO UPDATE SET finished = 1, furthest = excluded.furthest;
"#,
    r#"
-- Whether the translation is still coming out (`translation.status`): finishing an ongoing book is
-- catching up, not completing it. Re-read every file to fill it.
ALTER TABLE books ADD COLUMN translation_status TEXT;
UPDATE books SET mtime = 0;
"#,
    r#"
-- A nickname, apart from the login (`name`, fixed): what others see.
ALTER TABLE users ADD COLUMN display_name TEXT;
"#,
];

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::setup(conn)
    }

    fn setup(mut conn: Connection) -> anyhow::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Titles sorted like file managers do: "Vol. 2" before "Vol. 10".
        conn.create_collation("NATSORT", vnmf::natsort::natural_cmp)?;
        let applied: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let applied = usize::try_from(applied).unwrap_or(usize::MAX);
        if applied > MIGRATIONS.len() {
            anyhow::bail!("the database was created by a newer vnmf-server (schema {applied}, this one knows {})", MIGRATIONS.len());
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql).with_context(|| format!("database migration {}", i + 1))?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Runs `f` with the connection on a blocking thread.
    pub async fn call<R, F>(&self, f: F) -> ApiResult<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> ApiResult<R> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.blocking(f)).await?
    }

    /// The same from a thread that may block (the command line, background jobs).
    pub fn blocking<R>(&self, f: impl FnOnce(&mut Connection) -> ApiResult<R>) -> ApiResult<R> {
        let mut conn = self.conn.lock().map_err(|_| ApiError::from(anyhow::anyhow!("database connection poisoned")))?;
        f(&mut conn)
    }
}
