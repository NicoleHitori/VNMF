// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! The library index: scanning the folders, indexing files, integrity checks, background jobs.
//!
//! A file is identified by its path. A file that disappeared is marked `missing` (its reading progress
//! is kept); if a file with the same UUID shows up at another path, it's taken as moved — the book keeps
//! its id, progress and bookmarks.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use vnmf::contents::Toc;
use vnmf::health;
use vnmf::metadata::Metadata;
use vnmf_core::{AuxKind, HeaderBitEnum, PageBitFlags, VnmfReader};

use crate::error::{ApiError, ApiResult};
use crate::{AppState, now};

/// State of a background job.
#[derive(Clone, Default, Serialize)]
pub struct JobStatus {
    pub running: bool,
    pub done: u64,
    pub total: u64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// Scan: books added / updated / gone missing; verify: books found damaged.
    pub added: u64,
    pub updated: u64,
    pub missing: u64,
    pub damaged: u64,
    /// The last few errors (unreadable files, inaccessible folders).
    pub errors: Vec<String>,
}

impl JobStatus {
    fn error(&mut self, e: String) {
        tracing::warn!("{e}");
        if self.errors.len() < 50 {
            self.errors.push(e);
        }
    }
}

#[derive(Default)]
pub struct Jobs {
    pub scan: Mutex<JobStatus>,
    pub verify: Mutex<JobStatus>,
}

fn lock(m: &Mutex<JobStatus>) -> std::sync::MutexGuard<'_, JobStatus> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Marks the job as started; `false` — it's already running.
fn begin(m: &Mutex<JobStatus>) -> bool {
    let mut s = lock(m);
    if s.running {
        return false;
    }
    *s = JobStatus { running: true, started_at: Some(now()), ..Default::default() };
    true
}

fn finish(m: &Mutex<JobStatus>) {
    let mut s = lock(m);
    s.running = false;
    s.finished_at = Some(now());
}

/// A book row: where the file is and whether the server may change it.
#[derive(Clone, Debug)]
pub struct BookRow {
    pub id: i64,
    pub path: PathBuf,
    pub managed: bool,
    pub missing: bool,
}

pub fn book_row(conn: &Connection, id: i64) -> ApiResult<BookRow> {
    conn.query_row("SELECT id, path, managed, missing FROM books WHERE id = ?1", [id], |r| {
        Ok(BookRow { id: r.get(0)?, path: PathBuf::from(r.get::<_, String>(1)?), managed: r.get(2)?, missing: r.get(3)? })
    })
    .optional()?
    .ok_or_else(|| ApiError::not_found(format!("book {id}")))
}

/// What the index keeps about a file.
struct Summary {
    size: i64,
    mtime: i64,
    uuid: String,
    title: String,
    search: String,
    kind: Option<String>,
    status: Option<String>,
    language: Option<String>,
    authors: Vec<String>,
    genres: Vec<String>,
    tags: Vec<String>,
    page_count: i64,
    volume_count: i64,
    chapter_count: i64,
    webtoon: bool,
    rtl: bool,
    has: [bool; 4],
    redundancy: i64,
    format_version: String,
    recovered: bool,
    /// Header-level check (both metadata copies) — instant; data is checked by a full check.
    health_ok: bool,
    health_issues: Vec<String>,
    /// For the catalog filters.
    filters: Filters,
}

/// Metadata the catalog filters by.
#[derive(Default)]
struct Filters {
    /// From `start_date`, otherwise `release_date` (`2020`, `2020-05`, `2020-05-31`).
    year: Option<i64>,
    origin_country: Option<String>,
    age_rating: Option<String>,
    publication: Option<String>,
    demographic: Option<String>,
    color: Option<String>,
    /// `translation.type`: official, fan, machine.
    translation: Option<String>,
    /// `translation.status`: whether the translation is still coming out (the series' `status` may differ).
    translation_status: Option<String>,
}

fn year_of(date: Option<&str>) -> Option<i64> {
    let digits: String = date?.trim().chars().take(4).collect();
    (digits.len() == 4).then(|| digits.parse().ok()).flatten()
}

impl Filters {
    fn of(m: &Metadata) -> Self {
        Self {
            year: year_of(m.start_date.as_deref()).or_else(|| year_of(m.release_date.as_deref())),
            origin_country: m.origin_country.as_ref().map(|c| c.trim().to_uppercase()).filter(|c| !c.is_empty()),
            age_rating: m.age_rating.clone(),
            publication: m.publication.clone(),
            demographic: m.demographic.clone(),
            color: m.color.clone(),
            translation: m.translation.kind.clone(),
            translation_status: m.translation.status.clone(),
        }
    }
}

/// The book's type as a vocabulary key (`manga`, `manhwa`, …): the `kind` field, else a genre or tag
/// naming a type ("Manga", "Манга"), else the country of origin, else the webtoon flag. Many files
/// come from sites that put the type among the genres and leave `kind` empty.
fn book_kind(meta: Option<&Metadata>, webtoon: bool) -> Option<String> {
    fn key(word: &str) -> Option<&'static str> {
        let word = word.trim().to_lowercase().replace(['-', ' ', '\u{2011}'], "_");
        Some(match word.as_str() {
            "manga" | "манга" => "manga",
            "manhwa" | "манхва" => "manhwa",
            "manhua" | "маньхуа" | "маньхва" | "манхуа" => "manhua",
            "webtoon" | "вебтун" => "webtoon",
            "webcomic" | "web_comic" | "вебкомікс" => "webcomic",
            "comic" | "comics" | "комікс" | "комікси" => "comic",
            "graphic_novel" | "графічний_роман" => "graphic_novel",
            "bande_dessinee" | "bande_dessinée" => "bande_dessinee",
            "doujinshi" | "доджінші" => "doujinshi",
            "artbook" | "артбук" => "artbook",
            "other" => "other",
            _ => return None,
        })
    }
    let m = meta;
    m.and_then(|m| m.kind.as_deref())
        .and_then(|k| key(k).map(str::to_owned).or_else(|| Some(k.trim().to_lowercase()).filter(|k| !k.is_empty())))
        .or_else(|| m.and_then(|m| m.genres.iter().chain(&m.tags).find_map(|g| key(g))).map(str::to_owned))
        .or_else(|| {
            let country = m?.origin_country.as_deref()?.trim().to_uppercase();
            Some(
                match country.as_str() {
                    "JP" => "manga",
                    "KR" => "manhwa",
                    "CN" | "TW" | "HK" => "manhua",
                    _ => return None,
                }
                .to_owned(),
            )
        })
        .or_else(|| webtoon.then(|| "webtoon".to_owned()))
}

fn file_stat(path: &Path) -> std::io::Result<(i64, i64)> {
    let meta = std::fs::metadata(path)?;
    let mtime = meta.modified()?.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as i64);
    Ok((meta.len() as i64, mtime))
}

fn summarize(path: &Path) -> anyhow::Result<Summary> {
    let (size, mtime) = file_stat(path)?;
    let reader = VnmfReader::open(path)?;
    let meta = Metadata::from_info(reader.info_bytes());
    let toc = Toc::from_reader(&reader);
    let stem = vnmf::input::file_stem(path);
    let title = meta.as_ref().and_then(|m| m.title.clone()).filter(|t| !t.trim().is_empty()).unwrap_or(stem.clone());

    let mut authors = Vec::new();
    let mut search = vec![title.clone(), stem];
    if let Some(m) = &meta {
        for role in ["story", "art"] {
            for name in m.credited(role) {
                if !authors.contains(&name) {
                    authors.push(name);
                }
            }
        }
        if authors.is_empty() {
            authors = m.credits.iter().map(|c| c.name.clone()).take(3).collect();
        }
        search.extend(m.original_title.clone());
        search.extend(m.romanized_title.clone());
        search.extend(m.alternative_titles.iter().map(|t| t.title.clone()));
        search.extend(m.credits.iter().flat_map(|c| std::iter::once(c.name.clone()).chain(c.original_name.clone())));
        search.extend(m.publisher.clone());
        search.extend(m.genres.iter().chain(&m.tags).chain(&m.characters).cloned());
        search.extend(m.isbn.clone());
    }
    let aux = reader.aux();
    let has = [AuxKind::Music, AuxKind::Sfx, AuxKind::Text, AuxKind::DarkImage].map(|k| vnmf::playback::has_kind(aux, k));
    let rtl = reader.pages().first().is_some_and(|p| p.flags.get() & PageBitFlags::READING_RTL != 0) || meta.as_ref().is_some_and(Metadata::is_rtl);
    let (major, minor) = reader.version();
    let health = health::assess(&reader, None);
    Ok(Summary {
        size,
        mtime,
        uuid: vnmf::format_uuid(reader.uuid()),
        title,
        search: search.join("\n").to_lowercase(),
        kind: book_kind(meta.as_ref(), reader.has_flag(HeaderBitEnum::IsWebToon)),
        status: meta.as_ref().and_then(|m| m.status.clone()),
        language: meta.as_ref().and_then(|m| m.language.clone().or_else(|| m.original_language.clone())),
        authors,
        genres: meta.as_ref().map(|m| m.genres.clone()).unwrap_or_default(),
        tags: meta.as_ref().map(|m| m.tags.clone()).unwrap_or_default(),
        page_count: reader.pages_count() as i64,
        volume_count: toc.volumes.iter().filter(|v| v.volume != 0).count() as i64,
        chapter_count: toc.chapters_count() as i64,
        webtoon: reader.has_flag(HeaderBitEnum::IsWebToon),
        rtl,
        has,
        redundancy: i64::from(reader.redundancy()),
        format_version: format!("{major}.{minor}"),
        recovered: reader.is_recovered(),
        health_ok: health.is_ok(),
        health_issues: health.issues,
        filters: meta.as_ref().map(Filters::of).unwrap_or_default(),
    })
}

fn json(list: &[String]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".into())
}

/// Writes the summary for `path`: updates the row with this path, or a missing row with the same UUID
/// (a moved file), or inserts a new one. Returns the book id and whether it's new.
fn upsert_main(conn: &Connection, path: &Path, managed: bool, s: &Summary) -> rusqlite::Result<(i64, bool)> {
    let path_str = path.to_string_lossy().into_owned();
    let existing: Option<i64> = conn.query_row("SELECT id FROM books WHERE path = ?1", [&path_str], |r| r.get(0)).optional()?;
    // A moved file: a row with the same UUID whose file is gone (already marked missing, or not yet —
    // the old path is noticed only at the end of the same scan). A copy (both files exist) is a separate book.
    let moved: Option<i64> = match existing {
        Some(_) => None,
        None => {
            let mut stmt = conn.prepare("SELECT id, path FROM books WHERE uuid = ?1 ORDER BY missing DESC, id")?;
            let rows = stmt.query_map([&s.uuid], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().find(|(_, p)| !Path::new(p).exists()).map(|(id, _)| id)
        }
    };
    let t = now();
    let health = if s.health_ok { "ok" } else { "damaged" };
    let values = params![
        path_str,
        managed,
        s.size,
        s.mtime,
        s.uuid,
        s.title,
        s.title.to_lowercase(),
        s.search,
        s.kind,
        s.status,
        s.language,
        json(&s.authors),
        json(&s.genres),
        json(&s.tags),
        s.page_count,
        s.volume_count,
        s.chapter_count,
        s.webtoon,
        s.rtl,
        s.has[0],
        s.has[1],
        s.has[2],
        s.has[3],
        s.redundancy,
        s.format_version,
        s.recovered,
        health,
        json(&s.health_issues),
        t,
    ];
    match existing.or(moved) {
        Some(id) => {
            conn.execute(
                "UPDATE books SET path = ?1, managed = ?2, size = ?3, mtime = ?4, uuid = ?5, title = ?6, sort_title = ?7, search = ?8,
                 kind = ?9, status = ?10, language = ?11, authors = ?12, genres = ?13, tags = ?14, page_count = ?15, volume_count = ?16,
                 chapter_count = ?17, webtoon = ?18, rtl = ?19, has_music = ?20, has_sfx = ?21, has_text = ?22, has_dark = ?23,
                 redundancy = ?24, format_version = ?25, recovered = ?26, missing = 0,
                 health = ?27, health_issues = ?28, health_full = 0, health_checked_at = ?29, updated_at = ?29
                 WHERE id = ?30",
                rusqlite::params_from_iter(values.iter().copied().chain(std::iter::once(&id as &dyn rusqlite::ToSql))),
            )?;
            Ok((id, false))
        }
        None => {
            conn.execute(
                "INSERT INTO books (path, managed, size, mtime, uuid, title, sort_title, search, kind, status, language, authors, genres, tags,
                 page_count, volume_count, chapter_count, webtoon, rtl, has_music, has_sfx, has_text, has_dark, redundancy, format_version,
                 recovered, health, health_issues, health_full, health_checked_at, added_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25,
                 ?26, ?27, ?28, 0, ?29, ?29, ?29)",
                values,
            )?;
            Ok((conn.last_insert_rowid(), true))
        }
    }
}

/// [`upsert_main`] and the filter columns.
fn upsert(conn: &Connection, path: &Path, managed: bool, s: &Summary) -> rusqlite::Result<(i64, bool)> {
    let (id, new) = upsert_main(conn, path, managed, s)?;
    let f = &s.filters;
    conn.execute(
        "UPDATE books SET year = ?1, origin_country = ?2, age_rating = ?3, publication = ?4, demographic = ?5, color = ?6, translation = ?7,
         translation_status = ?8 WHERE id = ?9",
        params![f.year, f.origin_country, f.age_rating, f.publication, f.demographic, f.color, f.translation, f.translation_status, id],
    )?;
    reopen_grown(conn, id)?;
    Ok((id, new))
}

/// The file got new chapters: whoever had finished it hasn't any more — back in "reading" from
/// "completed" (readers keep their place; the new pages are past the furthest one they reached).
fn reopen_grown(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    let users: Vec<i64> = conn
        .prepare("SELECT p.user_id FROM progress p JOIN books b ON b.id = p.book_id WHERE p.book_id = ?1 AND p.finished = 1 AND p.furthest < b.page_count - 1")?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for user in users {
        conn.execute("UPDATE progress SET finished = 0 WHERE user_id = ?1 AND book_id = ?2", [user, id])?;
        conn.execute(
            "UPDATE list_books SET list_id = (SELECT id FROM lists WHERE user_id = ?1 AND kind = 'reading')
             WHERE book_id = ?2 AND list_id = (SELECT id FROM lists WHERE user_id = ?1 AND kind = 'completed')",
            [user, id],
        )?;
    }
    Ok(())
}

/// Indexes one file (after an upload or a rewrite). Blocking.
pub fn index_file(state: &AppState, path: &Path) -> ApiResult<i64> {
    let summary = summarize(path).map_err(|e| ApiError::unprocessable("unreadable", format!("{}: {e:#}", path.display())))?;
    let managed = state.config.is_managed(path);
    let (id, _) = state.db.blocking(|conn| Ok(upsert(conn, path, managed, &summary)?))?;
    Ok(id)
}

/// `.vnmf` files under `root`, recursively (hidden entries and temporary files skipped).
fn collect(root: &Path, out: &mut Vec<PathBuf>, status: &mut JobStatus) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                status.error(format!("{}: {e}", dir.display()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(_) if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("vnmf")) => out.push(path),
                _ => {}
            }
        }
    }
}

/// Scans all library folders. Blocking; returns `false` if a scan is already running.
pub fn scan(state: &AppState) -> bool {
    if !begin(&state.jobs.scan) {
        return false;
    }
    let mut status = JobStatus::default();
    let mut files = Vec::new();
    for root in state.config.roots() {
        if root.exists() {
            collect(&root, &mut files, &mut status);
        } else {
            status.error(format!("{}: folder not found", root.display()));
        }
    }
    files.sort();
    files.dedup();
    lock(&state.jobs.scan).total = files.len() as u64;

    let known: HashMap<String, (i64, i64, bool)> = state
        .db
        .blocking(|conn| {
            let mut stmt = conn.prepare("SELECT path, size, mtime, missing FROM books")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?;
            Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
        })
        .unwrap_or_default();

    let mut seen = HashSet::new();
    for path in &files {
        let key = path.to_string_lossy().into_owned();
        seen.insert(key.clone());
        let unchanged = matches!((known.get(&key), file_stat(path)), (Some(&(size, mtime, false)), Ok((s, m))) if size == s && mtime == m);
        if !unchanged {
            match summarize(path) {
                Ok(summary) => {
                    let managed = state.config.is_managed(path);
                    match state.db.blocking(|conn| Ok(upsert(conn, path, managed, &summary)?)) {
                        Ok((_, true)) => status.added += 1,
                        Ok((id, false)) => {
                            status.updated += 1;
                            state.books.forget(id);
                        }
                        Err(e) => status.error(format!("{}: {}", path.display(), e.message)),
                    }
                }
                Err(e) => status.error(format!("{}: {e:#}", path.display())),
            }
        }
        let mut s = lock(&state.jobs.scan);
        s.done += 1;
        (s.added, s.updated) = (status.added, status.updated);
    }

    // Everything indexed but not found now is missing (kept with its progress).
    let gone: Vec<String> = known.iter().filter(|(path, (_, _, missing))| !missing && !seen.contains(*path)).map(|(p, _)| p.clone()).collect();
    status.missing = gone.len() as u64;
    if !gone.is_empty() {
        let result = state.db.blocking(|conn| {
            let tx = conn.transaction()?;
            for path in &gone {
                tx.execute("UPDATE books SET missing = 1, updated_at = ?2 WHERE path = ?1", params![path, now()])?;
            }
            tx.commit()?;
            Ok(())
        });
        if let Err(e) = result {
            status.error(e.message);
        }
    }
    {
        let mut s = lock(&state.jobs.scan);
        (s.added, s.updated, s.missing) = (status.added, status.updated, status.missing);
        s.errors = std::mem::take(&mut status.errors);
    }
    finish(&state.jobs.scan);
    tracing::info!("library scan: {} files, {} added, {} updated, {} missing", files.len(), status.added, status.updated, status.missing);
    true
}

/// Starts a scan in the background; `false` — one is already running.
pub fn start_scan(state: &AppState) -> bool {
    if lock(&state.jobs.scan).running {
        return false;
    }
    let state = state.clone();
    tokio::task::spawn_blocking(move || scan(&state));
    true
}

/// Result of an integrity check.
#[derive(Serialize)]
pub struct HealthReport {
    pub ok: bool,
    /// What is damaged — one item per problem.
    pub issues: Vec<String>,
    /// Whether it's recoverable (empty when there are no problems).
    pub prognosis: String,
    /// The data was checked too (CRC of every chunk and the redundancy), not just the metadata.
    pub full: bool,
    /// Damaged chunks (full check).
    pub damaged_chunks: Vec<u32>,
    /// Redundancy state (full check of a file with parity): whether all damage is repairable.
    pub repairable: Option<bool>,
    pub checked_at: i64,
}

/// Checks a file: `full` — reads all of it (seconds for a big file). Blocking.
pub fn check(reader: &VnmfReader, full: bool) -> HealthReport {
    let verification = full.then(|| health::verify(reader).ok()).flatten();
    let h = health::assess(reader, verification.as_ref());
    HealthReport {
        ok: h.is_ok(),
        prognosis: if h.is_ok() { String::new() } else { h.prognosis.to_string() },
        issues: h.issues,
        full: h.data_checked,
        damaged_chunks: verification.as_ref().map(|v| v.bad_chunks.iter().map(|&c| c as u32).collect()).unwrap_or_default(),
        repairable: verification.as_ref().and_then(|v| v.parity.as_ref()).map(|p| p.is_recoverable()),
        checked_at: now(),
    }
}

pub fn store_health(conn: &Connection, id: i64, report: &HealthReport) -> ApiResult<()> {
    conn.execute(
        "UPDATE books SET health = ?1, health_issues = ?2, health_full = ?3, health_checked_at = ?4 WHERE id = ?5",
        params![if report.ok { "ok" } else { "damaged" }, json(&report.issues), report.full, report.checked_at, id],
    )?;
    Ok(())
}

/// Full check of every book, in the background (a "scrub"); `false` — already running.
pub fn start_verify(state: &AppState) -> bool {
    run_verify(state, None)
}

/// The periodic scrub: books not fully checked within `max_age` (or never, or changed since).
/// Nothing is started when there are none. `false` — nothing to do or already running.
pub fn start_verify_due(state: &AppState, max_age: std::time::Duration) -> bool {
    run_verify(state, Some(crate::now() - max_age.as_secs() as i64))
}

fn run_verify(state: &AppState, checked_before: Option<i64>) -> bool {
    let due = move |conn: &Connection| -> rusqlite::Result<Vec<(i64, String)>> {
        let mut stmt = conn.prepare(
            "SELECT id, path FROM books WHERE missing = 0
             AND (?1 IS NULL OR health_full IS NOT 1 OR health_checked_at IS NULL OR health_checked_at < ?1)
             ORDER BY health_full IS 1, health_checked_at, id",
        )?;
        let rows = stmt.query_map([checked_before], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    };
    if checked_before.is_some() && state.db.blocking(|conn| Ok(due(conn)?)).is_ok_and(|books| books.is_empty()) {
        return false;
    }
    if !begin(&state.jobs.verify) {
        return false;
    }
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        let books = state.db.blocking(|conn| Ok(due(conn)?)).unwrap_or_default();
        lock(&state.jobs.verify).total = books.len() as u64;
        for (id, path) in books {
            match VnmfReader::open(&path) {
                Ok(reader) => {
                    let report = check(&reader, true);
                    if !report.ok {
                        lock(&state.jobs.verify).damaged += 1;
                    }
                    if let Err(e) = state.db.blocking(|conn| store_health(conn, id, &report)) {
                        lock(&state.jobs.verify).error(e.message);
                    }
                }
                Err(e) => {
                    let mut s = lock(&state.jobs.verify);
                    s.damaged += 1;
                    s.error(format!("{path}: {e}"));
                }
            }
            lock(&state.jobs.verify).done += 1;
        }
        finish(&state.jobs.verify);
    });
    true
}
