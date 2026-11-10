// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! An open book: the memory-mapped file with what's derived from it (contents, metadata, image
//! dimensions, spread layout), and its JSON views. The rules — what plays where, how pages pair into
//! spreads, where an effect triggers in a strip — are the same as in the desktop reader and on Android
//! (`vnmf::playback`, `vnmf::spread`).
//!
//! Page, chunk, attachment and preview numbers are zero-based indices, as in the file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;
use vnmf::contents::Toc;
use vnmf::metadata::Metadata;
use vnmf::{playback, spread};
use vnmf_core::{AuxFlags, AuxFormat, AuxKind, HeaderBitEnum, InfoFormat, PageBitFlags, PageFormat, PageRole, VnmfReader};

use crate::error::{ApiError, ApiResult};

/// Dimensions and format of one image, from its header (no decoding).
#[derive(Clone, Copy)]
struct ChunkMeta {
    width: u32,
    height: u32,
    format: PageFormat,
}

pub struct OpenBook {
    pub reader: VnmfReader,
    pub toc: Toc,
    pub metadata: Option<Metadata>,
    pub path: PathBuf,
    /// `size-mtime` of the file: changes whenever the file does (for caching on clients).
    pub version: String,
    chunks: Vec<ChunkMeta>,
    spread: Vec<spread::PageInfo>,
}

impl OpenBook {
    pub fn open(path: &Path) -> Result<Self, vnmf_core::ReaderError> {
        let reader = VnmfReader::open(path)?;
        let version = file_version(path).unwrap_or_default();
        let mut chunks = Vec::with_capacity(reader.chunks_count());
        for page in reader.pages() {
            for chunk in page.chunk_range() {
                let data = reader.get_chunk_data(chunk).ok();
                let (width, height) = data.and_then(vnmf_core::image_dimensions).unwrap_or((page.width.get().max(1), page.height.get().max(1)));
                chunks.push(ChunkMeta { width, height, format: data.map_or(PageFormat::Unknown, PageFormat::detect) });
            }
        }
        let toc = Toc::from_reader(&reader);
        let metadata = Metadata::from_info(reader.info_bytes());
        let spread = spread::PageInfo::from_reader(&reader, &toc);
        Ok(Self { reader, toc, metadata, path: path.to_path_buf(), version, chunks, spread })
    }

    /// The size of an image chunk (from its bytes).
    pub fn chunk_size(&self, chunk: usize) -> Option<(u32, u32)> {
        self.chunks.get(chunk).map(|c| (c.width, c.height))
    }

    pub fn webtoon(&self) -> bool {
        self.reader.has_flag(HeaderBitEnum::IsWebToon)
    }

    pub fn rtl(&self) -> bool {
        self.reader.pages().first().is_some_and(|p| p.flags.get() & PageBitFlags::READING_RTL != 0) || self.metadata.as_ref().is_some_and(Metadata::is_rtl)
    }

    pub fn title(&self) -> Option<String> {
        self.metadata.as_ref().and_then(|m| m.title.clone())
    }

    pub fn page(&self, page: u32) -> ApiResult<usize> {
        let page = page as usize;
        (page < self.reader.pages_count()).then_some(page).ok_or_else(|| ApiError::not_found(format!("page {page}")))
    }

    // ------------------------------------------------------------------
    // JSON views
    // ------------------------------------------------------------------

    /// Everything about the file itself: format, flags, protection, attachments present.
    pub fn info(&self) -> BookInfo {
        let r = &self.reader;
        let aux = r.aux();
        let (major, minor) = r.version();
        let header_flags = r.header().flags.get();
        BookInfo {
            uuid: vnmf::format_uuid(r.uuid()),
            title: self.title(),
            version: self.version.clone(),
            format_version: format!("{major}.{minor}"),
            legacy: r.legacy_version().is_some(),
            webtoon: self.webtoon(),
            rtl: self.rtl(),
            page_count: r.pages_count() as u32,
            chunk_count: r.chunks_count() as u32,
            aux_count: aux.len() as u32,
            preview_count: r.previews_count() as u32,
            has_volumes: self.toc.has_volumes,
            volume_count: self.toc.volumes.iter().filter(|v| v.volume != 0).count() as u32,
            chapter_count: self.toc.chapters_count() as u32,
            has_metadata: self.metadata.is_some(),
            info_format: r.info_bytes().map(|i| InfoFormat::detect(i).extension().to_string()),
            has_music: playback::has_kind(aux, AuxKind::Music),
            has_sfx: playback::has_kind(aux, AuxKind::Sfx),
            has_text: playback::has_kind(aux, AuxKind::Text),
            has_dark: playback::has_kind(aux, AuxKind::DarkImage),
            has_attachments: playback::has_kind(aux, AuxKind::Attachment),
            flags: HeaderBitEnum::ALL.iter().filter(|f| f.is_set(header_flags)).map(|f| f.to_string()).collect(),
            block_size: r.block_size().bytes() as u32,
            redundancy: r.redundancy(),
            parity: r.parity_layout().map(|l| ParityInfo { bytes: l.len(), max_burst_bytes: l.max_burst_bytes(), shard_size: l.shard_size as u32 }),
            tail_copy: r.has_flag(HeaderBitEnum::HasTailDup),
            recovered: r.is_recovered(),
            file_size: r.raw().len() as u64,
        }
    }

    pub fn pages(&self) -> Vec<PageInfo> {
        let r = &self.reader;
        r.pages()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let flags = p.flags.get();
                let aux: Vec<&vnmf_core::AuxRecord> = r.page_aux(i).iter().collect();
                PageInfo {
                    index: i as u32,
                    width: p.width.get(),
                    height: p.height.get(),
                    role: role_name(p.role),
                    volume: u32::from(p.volume_id.get()),
                    chapter: u32::from(p.chapter_id.get()),
                    number_in_chapter: u32::from(p.page_number_in_chapter.get()),
                    chapter_start: flags & PageBitFlags::IS_CHAPTER_START != 0,
                    volume_start: flags & PageBitFlags::IS_VOLUME_START != 0,
                    flags: PageBitFlags::names(flags),
                    chunks: p
                        .chunk_range()
                        .filter_map(|c| {
                            let m = self.chunks.get(c)?;
                            Some(ChunkInfo {
                                index: c as u32,
                                width: m.width,
                                height: m.height,
                                format: m.format.extension(),
                                mime: m.format.mime_type(),
                                tiles: crate::tiles::count(m.height),
                            })
                        })
                        .collect(),
                    aux: r.page_aux_range(i).map(|k| k as u32).collect(),
                    has_music: aux.iter().any(|a| a.kind() == AuxKind::Music),
                    has_sfx: aux.iter().any(|a| a.kind() == AuxKind::Sfx),
                    has_text: aux.iter().any(|a| a.kind() == AuxKind::Text),
                    dark_parts: aux.iter().filter(|a| a.kind() == AuxKind::DarkImage).map(|a| a.part.get()).collect(),
                }
            })
            .collect()
    }

    /// Volumes and chapters with titles, dates and ISBN from the metadata.
    pub fn contents(&self) -> Contents {
        let meta = self.metadata.as_ref().map(|m| &m.contents);
        Contents {
            has_volumes: self.toc.has_volumes,
            structured: self.toc.is_structured(),
            volumes: self
                .toc
                .volumes
                .iter()
                .map(|v| {
                    let vm = meta.and_then(|c| c.volume(v.volume));
                    VolumeInfo {
                        number: u32::from(v.volume),
                        title: v.title.clone(),
                        date: vm.and_then(|m| m.date.clone()),
                        isbn: vm.and_then(|m| m.isbn.clone()),
                        first_page: v.pages.start as u32,
                        page_count: v.pages.len() as u32,
                        chapters: v
                            .chapters
                            .iter()
                            .map(|c| ChapterInfo {
                                id: u32::from(c.chapter),
                                number: c.display_number(),
                                title: c.title.clone(),
                                date: meta.and_then(|m| m.chapter(c.volume, c.chapter)).and_then(|m| m.date.clone()),
                                first_page: c.pages.start as u32,
                                page_count: c.pages.len() as u32,
                            })
                            .collect(),
                    }
                })
                .collect(),
        }
    }

    pub fn aux_info(&self, index: usize) -> Option<AuxInfo> {
        let rec = self.reader.aux().get(index)?;
        let format = self.reader.get_aux_data(index).map_or(AuxFormat::from(rec.format), AuxFormat::detect);
        Some(AuxInfo {
            index: index as u32,
            kind: rec.kind().name(),
            page: rec.page_idx.get(),
            format: format.extension(),
            mime: format.mime_type(),
            size: self.reader.chunks().get(rec.chunk_idx.get() as usize).map_or(0, |c| c.length()),
            looped: rec.has_flag(AuxFlags::LOOP),
            flags: AuxFlags::names(rec.flags.get()),
            gain: rec.gain(),
            delay_ms: rec.delay_ms.get(),
            duration_ms: rec.duration_ms.get(),
            pages: u32::from(rec.pages.get()),
            trigger_y: rec.trigger_y.get(),
            part: rec.part.get(),
        })
    }

    pub fn aux(&self) -> Vec<AuxInfo> {
        (0..self.reader.aux().len()).filter_map(|k| self.aux_info(k)).collect()
    }

    pub fn covers(&self) -> Vec<CoverInfo> {
        self.reader
            .previews()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let format = self.reader.get_preview_data(i).map_or(PageFormat::from(p.format), PageFormat::detect);
                CoverInfo { index: i as u32, volume: u32::from(p.volume_id.get()), width: p.width.get(), height: p.height.get(), mime: format.mime_type() }
            })
            .collect()
    }

    /// Display groups for paged mode (single pages and spreads), see `vnmf::spread`.
    pub fn groups(&self, double: bool, cover_alone: bool, rtl: bool) -> Vec<PageGroup> {
        spread::group_pages(&self.spread, double, cover_alone)
            .into_iter()
            .map(|g| PageGroup {
                visual: spread::visual_order(&g, &self.spread, rtl).into_iter().map(|p| p as u32).collect(),
                pages: g.into_iter().map(|p| p as u32).collect(),
            })
            .collect()
    }

    /// What plays and what's shown on a page in paged reading.
    pub fn playback(&self, page: usize) -> Playback {
        let total = self.reader.pages_count();
        let chapter = self.toc.locate(page).map_or(0..total, |(_, c)| c.pages.start..c.pages.end.min(total));
        Playback {
            page: page as u32,
            music: playback::music_at(&self.reader, page, chapter).and_then(|k| self.aux_info(k)),
            sfx: playback::sfx_of(&self.reader, page).into_iter().filter_map(|k| self.aux_info(k)).collect(),
            has_text: self.reader.page_aux(page).iter().any(|a| a.kind() == AuxKind::Text),
            dark_parts: self.reader.page_aux(page).iter().filter(|a| a.kind() == AuxKind::DarkImage).map(|a| a.part.get()).collect(),
        }
    }

    /// Sound effects of a strip with their trigger points: the fragment and the fraction of its height.
    pub fn strip_sfx(&self) -> Vec<StripSfx> {
        let mut out = Vec::new();
        for (k, rec) in self.reader.aux().iter().enumerate().filter(|(_, a)| a.kind() == AuxKind::Sfx) {
            let Some(record) = self.reader.pages().get(rec.page_idx.get() as usize) else { continue };
            let range = record.chunk_range();
            let heights: Vec<u32> = range.clone().filter_map(|c| self.chunks.get(c).map(|m| m.height)).collect();
            if heights.is_empty() {
                continue;
            }
            let (j, fraction) = playback::locate_y(&heights, rec.trigger_y.get());
            if let Some(aux) = self.aux_info(k) {
                out.push(StripSfx { aux, chunk: (range.start + j) as u32, fraction });
            }
        }
        out
    }

    /// Text layers of a page: the plain text (as readers show it) and each layer as stored —
    /// `vnmf-text/1` JSON (regions with coordinates, for overlays) or plain text.
    pub fn text(&self, page: usize) -> PageText {
        let layers = self
            .reader
            .page_aux_range(page)
            .zip(self.reader.page_aux(page))
            .filter(|(_, a)| a.kind() == AuxKind::Text)
            .filter_map(|(k, _)| {
                let bytes = self.reader.get_aux_data(k).ok()?;
                let content = serde_json::from_slice::<Value>(bytes)
                    .ok()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| Value::String(String::from_utf8_lossy(bytes).into_owned()));
                Some(TextLayer { aux: k as u32, content })
            })
            .collect();
        PageText { page: page as u32, text: playback::page_text(&self.reader, page), layers }
    }

    /// Dark version of a page image: index in the aux table.
    pub fn dark_of(&self, page: usize, part: usize) -> Option<usize> {
        playback::dark_of(&self.reader, page, part)
    }

    /// The bytes of a slice of the file without copying: they keep the book (and its mapping) alive.
    pub fn bytes_of(self: &Arc<Self>, data: &[u8]) -> Bytes {
        let raw = self.reader.raw();
        let start = (data.as_ptr() as usize).wrapping_sub(raw.as_ptr() as usize);
        if start <= raw.len() && data.len() <= raw.len() - start {
            Bytes::from_owner(Slice { book: self.clone(), start, len: data.len() })
        } else {
            Bytes::copy_from_slice(data)
        }
    }
}

struct Slice {
    book: Arc<OpenBook>,
    start: usize,
    len: usize,
}

impl AsRef<[u8]> for Slice {
    fn as_ref(&self) -> &[u8] {
        &self.book.reader.raw()[self.start..self.start + self.len]
    }
}

pub fn role_name(role: u8) -> String {
    PageRole::try_from(role).map_or_else(|r| format!("Unknown{r}"), |r| r.name().to_string())
}

/// `size-mtime_ns` in hex.
pub fn file_version(path: &Path) -> std::io::Result<String> {
    let meta = std::fs::metadata(path)?;
    let mtime = meta.modified()?.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    Ok(format!("{:x}-{:x}", meta.len(), mtime))
}

#[derive(Serialize)]
pub struct ParityInfo {
    /// Size of the parity section.
    pub bytes: u64,
    /// A contiguous damaged region of up to this size is recoverable.
    pub max_burst_bytes: u64,
    pub shard_size: u32,
}

#[derive(Serialize)]
pub struct BookInfo {
    pub uuid: String,
    pub title: Option<String>,
    /// Changes whenever the file does: add `?v=<version>` to data URLs to let clients cache them forever.
    pub version: String,
    pub format_version: String,
    /// A 1.x file (read-only; rewriting converts it to 2.0).
    pub legacy: bool,
    pub webtoon: bool,
    pub rtl: bool,
    pub page_count: u32,
    pub chunk_count: u32,
    pub aux_count: u32,
    pub preview_count: u32,
    pub has_volumes: bool,
    pub volume_count: u32,
    pub chapter_count: u32,
    pub has_metadata: bool,
    /// Format of the Info section (`json`, `xml`, `toml`, `txt`), if there is one.
    pub info_format: Option<String>,
    pub has_music: bool,
    pub has_sfx: bool,
    pub has_text: bool,
    pub has_dark: bool,
    pub has_attachments: bool,
    /// Header flags (`HasInfo`, `HasParity`, `IsWebToon`…).
    pub flags: Vec<String>,
    pub block_size: u32,
    /// Reed–Solomon redundancy, %.
    pub redundancy: u8,
    pub parity: Option<ParityInfo>,
    /// The backup copy of the metadata at the end of the file.
    pub tail_copy: bool,
    /// The primary metadata is damaged; the backup copy was used.
    pub recovered: bool,
    pub file_size: u64,
}

#[derive(Serialize)]
pub struct ChunkInfo {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    /// By the bytes, not by the record: `png`, `jpg`, `webp`, `gif`, `avif`, `jxl`, `bin`.
    pub format: &'static str,
    pub mime: &'static str,
    /// Taller than browsers decode: shown as this many JPEG tiles (`/chunks/{n}/tiles/{t}`), each
    /// [`crate::tiles::TILE_HEIGHT`] px tall but the last.
    #[serde(skip_serializing_if = "is_zero")]
    pub tiles: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Serialize)]
pub struct PageInfo {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    /// `Normal`, `FrontCover`, `SpreadLeft`… (FORMAT.md, A.4).
    pub role: String,
    pub volume: u32,
    pub chapter: u32,
    pub number_in_chapter: u32,
    pub chapter_start: bool,
    pub volume_start: bool,
    pub flags: Vec<&'static str>,
    /// Images from top to bottom: one for an ordinary page, many for a webtoon episode.
    pub chunks: Vec<ChunkInfo>,
    /// The page's attachments: indices in the aux table.
    pub aux: Vec<u32>,
    pub has_music: bool,
    pub has_sfx: bool,
    pub has_text: bool,
    /// Images that have a dark version.
    pub dark_parts: Vec<u32>,
}

#[derive(Serialize)]
pub struct ChapterInfo {
    /// Chapter id in the file (`chapter_id`).
    pub id: u32,
    /// For display: `"3"`, `"10.5"`.
    pub number: String,
    pub title: Option<String>,
    pub date: Option<String>,
    pub first_page: u32,
    pub page_count: u32,
}

#[derive(Serialize)]
pub struct VolumeInfo {
    /// 0 — chapters outside volumes.
    pub number: u32,
    pub title: Option<String>,
    pub date: Option<String>,
    pub isbn: Option<String>,
    pub first_page: u32,
    pub page_count: u32,
    pub chapters: Vec<ChapterInfo>,
}

#[derive(Serialize)]
pub struct Contents {
    pub has_volumes: bool,
    /// More than one chapter or any volumes — worth showing a table of contents.
    pub structured: bool,
    pub volumes: Vec<VolumeInfo>,
}

#[derive(Serialize)]
pub struct AuxInfo {
    pub index: u32,
    /// `music`, `sfx`, `text`, `dark`, `attachment`.
    pub kind: &'static str,
    pub page: u32,
    pub format: &'static str,
    pub mime: &'static str,
    pub size: u32,
    pub looped: bool,
    pub flags: Vec<&'static str>,
    /// 0.0..=1.0, multiplied by the master volume.
    pub gain: f64,
    pub delay_ms: u32,
    /// 0 — unlimited.
    pub duration_ms: u32,
    /// Music: on how many pages from its own it plays; 0 — until it changes.
    pub pages: u32,
    /// Strip effect: the page line (px from its top) where it plays.
    pub trigger_y: u32,
    /// Dark version: which page image it replaces.
    pub part: u32,
}

#[derive(Serialize)]
pub struct CoverInfo {
    pub index: u32,
    /// 0 — the book cover, N — cover of volume N.
    pub volume: u32,
    pub width: u32,
    pub height: u32,
    pub mime: &'static str,
}

#[derive(Serialize)]
pub struct PageGroup {
    /// Reading order.
    pub pages: Vec<u32>,
    /// Left to right on screen.
    pub visual: Vec<u32>,
}

#[derive(Serialize)]
pub struct Playback {
    pub page: u32,
    /// Music that should be playing on this page (it continues until it changes; the boundary is the chapter).
    pub music: Option<AuxInfo>,
    /// Effects that play when the page is opened.
    pub sfx: Vec<AuxInfo>,
    pub has_text: bool,
    pub dark_parts: Vec<u32>,
}

#[derive(Serialize)]
pub struct StripSfx {
    pub aux: AuxInfo,
    pub chunk: u32,
    /// 0..1 from the top of the fragment.
    pub fraction: f64,
}

#[derive(Serialize)]
pub struct TextLayer {
    pub aux: u32,
    /// `vnmf-text/1` JSON object, or a string.
    pub content: Value,
}

#[derive(Serialize)]
pub struct PageText {
    pub page: u32,
    /// All layers as plain text; `null` — none.
    pub text: Option<String>,
    pub layers: Vec<TextLayer>,
}

/// Open books by id, most recently used kept. A changed file (other `version`) is reopened.
pub struct BookCache {
    capacity: usize,
    inner: Mutex<CacheInner>,
}

#[derive(Default)]
struct CacheInner {
    tick: u64,
    books: HashMap<i64, (Arc<OpenBook>, u64)>,
}

impl BookCache {
    pub fn new(capacity: usize) -> Self {
        Self { capacity, inner: Mutex::new(CacheInner::default()) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The book `id` at `path`, opened if needed (on a blocking thread).
    pub async fn get(&self, id: i64, path: PathBuf) -> ApiResult<Arc<OpenBook>> {
        let current = {
            let p = path.clone();
            tokio::task::spawn_blocking(move || file_version(&p)).await?
        };
        let current = current.map_err(|_| ApiError::new(axum::http::StatusCode::GONE, "missing", "the book's file is missing — rescan the library"))?;
        {
            let mut inner = self.lock();
            inner.tick += 1;
            let tick = inner.tick;
            if let Some((book, used)) = inner.books.get_mut(&id)
                && book.path == path
                && book.version == current
            {
                *used = tick;
                return Ok(book.clone());
            }
        }
        let opened = tokio::task::spawn_blocking(move || OpenBook::open(&path)).await?;
        let book = Arc::new(opened.map_err(|e| ApiError::unprocessable("unreadable", format!("the file can't be read: {e}")))?);
        let mut inner = self.lock();
        let tick = inner.tick;
        if inner.books.len() >= self.capacity
            && let Some(oldest) = inner.books.iter().min_by_key(|(_, (_, used))| *used).map(|(id, _)| *id)
        {
            inner.books.remove(&oldest);
        }
        inner.books.insert(id, (book.clone(), tick));
        Ok(book)
    }

    pub fn forget(&self, id: i64) {
        self.lock().books.remove(&id);
    }
}
