// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! VNMF for Android: reading a book from Kotlin via UniFFI.
//!
//! The app opens a file ([`Book::open_fd`] — a descriptor from `ContentResolver`, no copying),
//! takes the list of pages and fragments ([`Book::pages`]), the image bytes ([`Book::chunk`]) — which
//! Android decodes itself (PNG, JPEG, WebP, GIF, AVIF since Android 12) — and the contents ([`Book::contents`]).
//! What should play and be shown is decided by the same rules as in the desktop reader
//! (`vnmf::playback`): [`Book::music_at`], [`Book::sfx_of`], [`Book::strip_sfx`], [`Book::page_text`],
//! [`Book::dark_chunk`]. Integrity check — [`Book::check`].
//!
//! Page, fragment (chunk) and attachment numbers are zero-based indices, as in the file.

use std::sync::Arc;

use vnmf::contents::Toc;
use vnmf::metadata::Metadata;
use vnmf::{health, playback};
use vnmf_core::{AuxFlags, AuxFormat, AuxKind, AuxRecord, HeaderBitEnum, PageBitFlags, PageFormat, PageRole, VnmfReader};

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum VnmfError {
    /// The file can't be opened: not VNMF, both metadata copies damaged, an I/O error.
    #[error("{reason}")]
    Open { reason: String },
    /// Part of the file couldn't be read (number out of range, data past the end of the file).
    #[error("{reason}")]
    Read { reason: String },
}

fn read_error(e: impl std::fmt::Display) -> VnmfError {
    VnmfError::Read { reason: e.to_string() }
}

/// General information about the book.
#[derive(uniffi::Record)]
pub struct BookInfo {
    pub uuid: String,
    /// Title from the metadata (if any).
    pub title: Option<String>,
    /// Webtoon: read as a strip.
    pub webtoon: bool,
    /// Right to left (manga).
    pub rtl: bool,
    pub page_count: u32,
    pub chunk_count: u32,
    pub has_music: bool,
    pub has_sfx: bool,
    pub has_text: bool,
    pub has_dark: bool,
    /// Metadata was read from the backup copy (the primary is damaged) — worth offering a check.
    pub recovered: bool,
    /// A file in the old 1.x format ("1.2"), opened read-only.
    pub legacy_version: Option<String>,
}

/// A fragment (chunk) of a page image. Dimensions — from the image header, without decoding.
#[derive(uniffi::Record)]
pub struct ChunkInfo {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    /// `png`, `jpeg`, `webp`, `gif`, `avif`, `jxl` or `unknown` — by the bytes, not by the record.
    pub format: String,
}

#[derive(uniffi::Record)]
pub struct PageInfo {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    /// Role: `Normal`, `FrontCover`, `SpreadLeft`… (see FORMAT.md, A.4).
    pub role: String,
    pub volume_id: u32,
    pub chapter_id: u32,
    pub chapter_start: bool,
    pub volume_start: bool,
    /// Fragments from top to bottom; an ordinary page has one, a webtoon page has many.
    pub chunks: Vec<ChunkInfo>,
}

#[derive(uniffi::Record)]
pub struct ChapterInfo {
    /// "Chapter 3 — Title".
    pub label: String,
    pub first_page: u32,
    pub page_count: u32,
}

#[derive(uniffi::Record)]
pub struct VolumeInfo {
    /// "Volume 1 — Title"; without volumes — empty.
    pub label: String,
    pub first_page: u32,
    pub page_count: u32,
    pub chapters: Vec<ChapterInfo>,
}

#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuxType {
    Music,
    Sfx,
    Text,
    Dark,
    Attachment,
}

impl From<AuxKind> for AuxType {
    fn from(kind: AuxKind) -> Self {
        match kind {
            AuxKind::Music => Self::Music,
            AuxKind::Sfx => Self::Sfx,
            AuxKind::Text => Self::Text,
            AuxKind::DarkImage => Self::Dark,
            AuxKind::Attachment => Self::Attachment,
        }
    }
}

/// A page attachment with playback parameters.
#[derive(uniffi::Record)]
pub struct AuxInfo {
    /// Index in the attachments table — for [`Book::aux_data`].
    pub index: u32,
    pub kind: AuxType,
    pub page: u32,
    /// MIME for the player (`audio/ogg`, `audio/mpeg`, …).
    pub mime: String,
    /// Plays in a loop.
    pub looped: bool,
    /// Volume 0.0..=1.0 (multiplied by the master volume).
    pub gain: f64,
    pub delay_ms: u32,
    /// How long after the start it fades out; 0 — unlimited.
    pub duration_ms: u32,
    /// Music: on how many pages starting with its own it plays; 0 — until it changes.
    pub pages: u32,
}

/// A sound effect in a strip: plays when the trigger point appears on screen.
#[derive(uniffi::Record)]
pub struct StripSfx {
    pub aux: AuxInfo,
    /// The fragment containing the trigger point, and the fraction of its height from the top (0..1).
    pub chunk: u32,
    pub fraction: f64,
}

/// A display group in paged mode: a single page or a spread.
#[derive(uniffi::Record)]
pub struct PageGroup {
    /// Pages in reading order.
    pub pages: Vec<u32>,
    /// The same pages on screen from left to right (taking reading direction and spread roles into account).
    pub visual: Vec<u32>,
}

#[derive(uniffi::Record)]
pub struct CoverInfo {
    /// Preview index — for [`Book::cover_data`].
    pub index: u32,
    /// 0 — book cover, N — cover of volume N.
    pub volume_id: u32,
}

/// Integrity check result.
#[derive(uniffi::Record)]
pub struct HealthInfo {
    pub ok: bool,
    /// What is damaged — one item per problem.
    pub issues: Vec<String>,
    /// Whether it is recoverable (empty if there are no problems).
    pub prognosis: String,
    /// Whether the data was checked (not just the metadata).
    pub data_checked: bool,
}

struct Chunk {
    width: u32,
    height: u32,
    format: PageFormat,
}

/// An open book. Thread-safe: image data can be taken from several threads at once.
#[derive(uniffi::Object)]
pub struct Book {
    reader: VnmfReader,
    toc: Toc,
    metadata: Option<Metadata>,
    chunks: Vec<Chunk>,
    spread: Vec<vnmf::spread::PageInfo>,
}

impl Book {
    fn new(reader: VnmfReader) -> Arc<Self> {
        let mut chunks = Vec::with_capacity(reader.chunks_count());
        for page in reader.pages() {
            for chunk in page.chunk_range() {
                let data = reader.get_chunk_data(chunk).ok();
                let (width, height) = data
                    .and_then(vnmf_core::image_dimensions)
                    .unwrap_or((page.width.get().max(1), page.height.get().max(1)));
                let format = data.map_or(PageFormat::Unknown, PageFormat::detect);
                chunks.push(Chunk { width, height, format });
            }
        }
        let toc = Toc::from_reader(&reader);
        let metadata = Metadata::from_info(reader.info_bytes());
        let spread = vnmf::spread::PageInfo::from_reader(&reader, &toc);
        Arc::new(Self { reader, toc, metadata, chunks, spread })
    }

    fn aux_info(&self, index: usize) -> Option<AuxInfo> {
        let rec: &AuxRecord = self.reader.aux().get(index)?;
        Some(AuxInfo {
            index: index as u32,
            kind: rec.kind().into(),
            page: rec.page_idx.get(),
            mime: mime(AuxFormat::from(rec.format)).to_string(),
            looped: rec.has_flag(AuxFlags::LOOP),
            gain: rec.gain(),
            delay_ms: rec.delay_ms.get(),
            duration_ms: rec.duration_ms.get(),
            pages: u32::from(rec.pages.get()),
        })
    }

    fn page_index(&self, page: u32) -> Result<usize, VnmfError> {
        let page = page as usize;
        (page < self.reader.pages_count())
            .then_some(page)
            .ok_or_else(|| read_error(format!("page {page} does not exist (total {})", self.reader.pages_count())))
    }
}

#[uniffi::export]
impl Book {
    /// Opens a file by descriptor and **takes ownership of it** (on Android —
    /// `ParcelFileDescriptor.detachFd()`): the descriptor is closed here, after memory mapping.
    /// If the source can't be mapped (a pipe, a network provider), the file is read into memory.
    #[uniffi::constructor]
    pub fn open_fd(fd: i32) -> Result<Arc<Self>, VnmfError> {
        #[cfg(unix)]
        {
            use std::io::Read;
            use std::os::fd::FromRawFd;
            // SAFETY: ownership of the descriptor is transferred by the caller (detachFd); only we close it.
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            let reader = match VnmfReader::from_file(&file) {
                Ok(reader) => reader,
                Err(vnmf_core::ReaderError::Io(_)) => {
                    let mut bytes = Vec::new();
                    file.read_to_end(&mut bytes).map_err(|e| VnmfError::Open { reason: e.to_string() })?;
                    VnmfReader::from_bytes(&bytes).map_err(|e| VnmfError::Open { reason: e.to_string() })?
                }
                Err(e) => return Err(VnmfError::Open { reason: e.to_string() }),
            };
            Ok(Self::new(reader))
        }
        #[cfg(not(unix))]
        {
            let _ = fd;
            Err(VnmfError::Open { reason: "file descriptors are supported only on Unix/Android".into() })
        }
    }

    #[uniffi::constructor]
    pub fn open_path(path: String) -> Result<Arc<Self>, VnmfError> {
        VnmfReader::open(&path).map(Self::new).map_err(|e| VnmfError::Open { reason: e.to_string() })
    }

    #[uniffi::constructor]
    pub fn open_bytes(bytes: Vec<u8>) -> Result<Arc<Self>, VnmfError> {
        VnmfReader::from_bytes(&bytes).map(Self::new).map_err(|e| VnmfError::Open { reason: e.to_string() })
    }

    pub fn info(&self) -> BookInfo {
        let r = &self.reader;
        let aux = r.aux();
        BookInfo {
            uuid: vnmf::format_uuid(r.uuid()),
            title: self.metadata.as_ref().and_then(|m| m.title.clone()),
            webtoon: r.has_flag(HeaderBitEnum::IsWebToon),
            rtl: r.pages().first().is_some_and(|p| p.flags.get() & PageBitFlags::READING_RTL != 0),
            page_count: r.pages_count() as u32,
            chunk_count: r.chunks_count() as u32,
            has_music: playback::has_kind(aux, AuxKind::Music),
            has_sfx: playback::has_kind(aux, AuxKind::Sfx),
            has_text: playback::has_kind(aux, AuxKind::Text),
            has_dark: playback::has_kind(aux, AuxKind::DarkImage),
            recovered: r.is_recovered(),
            legacy_version: r.legacy_version().map(|(major, minor)| format!("{major}.{minor}")),
        }
    }

    /// Metadata (`vnmf-metadata/1`, JSON): authors, description, genres, links… `None` — there is none.
    pub fn metadata_json(&self) -> Option<String> {
        self.metadata.as_ref().map(|m| String::from_utf8_lossy(&m.to_bytes()).into_owned())
    }

    pub fn pages(&self) -> Vec<PageInfo> {
        self.reader
            .pages()
            .iter()
            .enumerate()
            .map(|(i, p)| PageInfo {
                index: i as u32,
                width: p.width.get(),
                height: p.height.get(),
                role: PageRole::try_from(p.role).map_or_else(|r| format!("Unknown{r}"), |r| r.name().to_string()),
                volume_id: u32::from(p.volume_id.get()),
                chapter_id: u32::from(p.chapter_id.get()),
                chapter_start: p.flags.get() & PageBitFlags::IS_CHAPTER_START != 0,
                volume_start: p.flags.get() & PageBitFlags::IS_VOLUME_START != 0,
                chunks: p
                    .chunk_range()
                    .filter_map(|c| {
                        let meta = self.chunks.get(c)?;
                        Some(ChunkInfo { index: c as u32, width: meta.width, height: meta.height, format: meta.format.extension().to_string() })
                    })
                    .collect(),
            })
            .collect()
    }

    /// Display groups for paged mode — the same rules as in the desktop reader: a spread
    /// set by the file is always together; in spread mode (`double`) the rest go in pairs, shown alone are
    /// covers (`cover_alone` — both the first page and volume covers), wide pages,
    /// long pages made of several fragments; pairs never join chapters.
    pub fn page_groups(&self, double: bool, cover_alone: bool, rtl: bool) -> Vec<PageGroup> {
        vnmf::spread::group_pages(&self.spread, double, cover_alone)
            .into_iter()
            .map(|g| PageGroup {
                visual: vnmf::spread::visual_order(&g, &self.spread, rtl).into_iter().map(|p| p as u32).collect(),
                pages: g.into_iter().map(|p| p as u32).collect(),
            })
            .collect()
    }

    /// Image bytes of a fragment (for `ImageDecoder` / `BitmapFactory`).
    pub fn chunk(&self, chunk: u32) -> Result<Vec<u8>, VnmfError> {
        self.reader.get_chunk_data(chunk as usize).map(<[u8]>::to_vec).map_err(read_error)
    }

    /// The fragment's CRC matches (`false` — damaged: show it as is and offer a check).
    pub fn chunk_ok(&self, chunk: u32) -> bool {
        self.reader.verify_chunk_crc(chunk as usize).unwrap_or(false)
    }

    /// Dark version of the page's fragment `part` (if any).
    pub fn dark_chunk(&self, page: u32, part: u32) -> Option<Vec<u8>> {
        let k = playback::dark_of(&self.reader, page as usize, part as usize)?;
        self.reader.get_aux_data(k).ok().map(<[u8]>::to_vec)
    }

    /// Covers (previews) in file order: the first is the main book cover.
    pub fn covers(&self) -> Vec<CoverInfo> {
        self.reader.previews().iter().enumerate().map(|(i, p)| CoverInfo { index: i as u32, volume_id: u32::from(p.volume_id.get()) }).collect()
    }

    pub fn cover_data(&self, index: u32) -> Result<Vec<u8>, VnmfError> {
        self.reader.get_preview_data(index as usize).map(<[u8]>::to_vec).map_err(read_error)
    }

    /// Cover for the library: the main preview, otherwise the first fragment of the first page.
    pub fn main_cover(&self) -> Option<Vec<u8>> {
        self.reader.main_preview_data().ok().or_else(|| self.reader.get_chunk_data(0).ok()).map(<[u8]>::to_vec)
    }

    /// Volumes and chapters. Without volumes — one volume with an empty title.
    pub fn contents(&self) -> Vec<VolumeInfo> {
        self.toc
            .volumes
            .iter()
            .map(|v| VolumeInfo {
                label: if self.toc.has_volumes { v.label() } else { String::new() },
                first_page: v.pages.start as u32,
                page_count: v.pages.len() as u32,
                chapters: v
                    .chapters
                    .iter()
                    .map(|c| ChapterInfo { label: c.label(), first_page: c.pages.start as u32, page_count: c.pages.len() as u32 })
                    .collect(),
            })
            .collect()
    }

    /// "Volume 1 · Chapter 3 — Title" for a page; `None` — the book has a single chapter without volumes.
    pub fn position_label(&self, page: u32) -> Option<String> {
        self.toc.is_structured().then(|| self.toc.position_label(page as usize)).flatten()
    }

    /// Music that should play on the page (it continues until it changes; the boundary is the chapter).
    pub fn music_at(&self, page: u32) -> Option<AuxInfo> {
        let page = page as usize;
        let chapter = self.toc.locate(page).map_or(0..self.reader.pages_count(), |(_, c)| c.pages.clone());
        let k = playback::music_at(&self.reader, page, chapter.start..chapter.end.min(self.reader.pages_count()))?;
        self.aux_info(k)
    }

    /// Page sound effects (paged reading: they play when the page is opened).
    pub fn sfx_of(&self, page: u32) -> Vec<AuxInfo> {
        playback::sfx_of(&self.reader, page as usize).into_iter().filter_map(|k| self.aux_info(k)).collect()
    }

    /// All sound effects of the strip with their trigger points (fragment and fraction of its height).
    pub fn strip_sfx(&self) -> Vec<StripSfx> {
        let mut out = Vec::new();
        for (k, rec) in self.reader.aux().iter().enumerate().filter(|(_, a)| a.kind() == AuxKind::Sfx) {
            let page = rec.page_idx.get() as usize;
            let Some(record) = self.reader.pages().get(page) else { continue };
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

    /// Attachment bytes (sound for the player, a file attachment).
    pub fn aux_data(&self, index: u32) -> Result<Vec<u8>, VnmfError> {
        self.reader.get_aux_data(index as usize).map(<[u8]>::to_vec).map_err(read_error)
    }

    /// Page text (all its text layers); `None` — no text.
    pub fn page_text(&self, page: u32) -> Result<Option<String>, VnmfError> {
        Ok(playback::page_text(&self.reader, self.page_index(page)?))
    }

    /// Integrity check: `full = false` — metadata only (instant), `true` — also the CRC of all
    /// data and the redundancy (reads the whole file — don't call on the main thread).
    pub fn check(&self, full: bool) -> HealthInfo {
        let verification = full.then(|| health::verify(&self.reader).ok()).flatten();
        let h = health::assess(&self.reader, verification.as_ref());
        HealthInfo {
            ok: h.is_ok(),
            prognosis: if h.is_ok() { String::new() } else { h.prognosis.to_string() },
            issues: h.issues,
            data_checked: h.data_checked,
        }
    }
}

/// A decoded image: RGBA, 8 bits per channel, row by row (like `Bitmap.Config.ARGB_8888`
/// in memory — for `copyPixelsFromBuffer`).
#[derive(uniffi::Record)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Decodes AVIF with our own decoder (pure Rust, `rav1d`) — a fallback when the system
/// `ImageDecoder` fails: on Android 12–13 it often doesn't support 4:4:4 (AV1 High profile),
/// 10/12-bit, etc. `target_width` > 0 — downscale to that width right away (never upscales), so as not
/// to pass tens of megabytes of full-resolution pixels to Kotlin.
#[uniffi::export]
pub fn decode_avif(data: Vec<u8>, target_width: u32) -> Result<RgbaImage, VnmfError> {
    let image = avif_decode::Decoder::from_avif(&data).and_then(|d| d.to_image()).map_err(read_error)?;
    let (width, height, rgba) = to_rgba8(image);
    Ok(downscale(width, height, rgba, target_width))
}

fn to_rgba8(image: avif_decode::Image) -> (usize, usize, Vec<u8>) {
    use avif_decode::Image;
    fn collect<P: Copy>(img: &imgref::ImgVec<P>, px: impl Fn(P) -> [u8; 4]) -> (usize, usize, Vec<u8>) {
        let mut out = Vec::with_capacity(img.width() * img.height() * 4);
        for row in img.rows() {
            for &p in row {
                out.extend_from_slice(&px(p));
            }
        }
        (img.width(), img.height(), out)
    }
    let hi = |v: u16| (v >> 8) as u8;
    match image {
        Image::Rgb8(img) => collect(&img, |p| [p.r, p.g, p.b, 255]),
        Image::Rgba8(img) => collect(&img, |p| [p.r, p.g, p.b, p.a]),
        Image::Rgb16(img) => collect(&img, |p| [hi(p.r), hi(p.g), hi(p.b), 255]),
        Image::Rgba16(img) => collect(&img, |p| [hi(p.r), hi(p.g), hi(p.b), hi(p.a)]),
        Image::Gray8(img) => collect(&img, |p| {
            let v = p.value();
            [v, v, v, 255]
        }),
        Image::Gray16(img) => collect(&img, |p| {
            let v = hi(p.value());
            [v, v, v, 255]
        }),
    }
}

/// Downscaling by area averaging (no upscaling): clean, without jagged text.
fn downscale(width: usize, height: usize, rgba: Vec<u8>, target_width: u32) -> RgbaImage {
    let target = target_width as usize;
    if target == 0 || target >= width {
        return RgbaImage { width: width as u32, height: height as u32, pixels: rgba };
    }
    let (tw, th) = (target, ((height * target) / width).max(1));
    let mut out = vec![0u8; tw * th * 4];
    for ty in 0..th {
        let (y0, y1) = (ty * height / th, ((ty + 1) * height / th).max(ty * height / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * width / tw, ((tx + 1) * width / tw).max(tx * width / tw + 1));
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                let row = &rgba[(y * width + x0) * 4..(y * width + x1) * 4];
                for px in row.as_chunks::<4>().0 {
                    for c in 0..4 {
                        sum[c] += u32::from(px[c]);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = (ty * tw + tx) * 4;
            for c in 0..4 {
                out[o + c] = ((sum[c] + n / 2) / n) as u8;
            }
        }
    }
    RgbaImage { width: tw as u32, height: th as u32, pixels: out }
}

/// Attachment MIME for a player or "Share".
fn mime(format: AuxFormat) -> &'static str {
    match format {
        AuxFormat::Ogg => "audio/ogg",
        AuxFormat::Mp3 => "audio/mpeg",
        AuxFormat::Flac => "audio/flac",
        AuxFormat::Wav => "audio/wav",
        AuxFormat::M4a => "audio/mp4",
        AuxFormat::Avif => "image/avif",
        AuxFormat::WebP => "image/webp",
        AuxFormat::Png => "image/png",
        AuxFormat::Jpeg => "image/jpeg",
        AuxFormat::Gif => "image/gif",
        AuxFormat::JpegXl => "image/jxl",
        AuxFormat::Text => "text/plain",
        AuxFormat::Json => "application/json",
        AuxFormat::Unknown => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vnmf_core::{AuxInput, Builder, ChunkSource};

    fn png_bytes(width: u32, height: u32, seed: u8) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 2, 0, 0, 0, seed]);
        bytes
    }

    fn png(width: u32, height: u32, seed: u8) -> ChunkSource {
        ChunkSource::from_bytes(png_bytes(width, height, seed)).unwrap()
    }

    fn ogg(tag: &[u8]) -> ChunkSource {
        ChunkSource::from_bytes([b"OggS".as_slice(), tag].concat()).unwrap()
    }

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("vnmf-ffi-{name}-{}.vnmf", std::process::id()))
    }

    #[test]
    fn paged_book_with_volumes_sound_text_and_dark() {
        let path = temp("paged");
        let mut b = Builder::new().with_redundancy(10);
        b.set_volume(1);
        b.add_page(png(800, 1200, 1), PageRole::FrontCover, 1, 1, 0).unwrap();
        let mut music = AuxInput::new(AuxKind::Music, ogg(b"rain"));
        music.flags = AuxFlags::LOOP;
        music.duration_ms = 9000;
        b.attach_aux(music).unwrap();
        b.add_page(png(800, 1200, 2), PageRole::Normal, 1, 2, 0).unwrap();
        b.attach_aux(AuxInput::new(AuxKind::Sfx, ogg(b"thunder"))).unwrap();
        b.attach_aux(AuxInput::new(AuxKind::Text, ChunkSource::from_bytes(br#"{"regions": [{"text": "Grr"}]}"#.to_vec()).unwrap())).unwrap();
        b.attach_aux(AuxInput::new(AuxKind::DarkImage, png(800, 1200, 9))).unwrap();
        b.set_volume(2);
        b.add_page(png(800, 1200, 3), PageRole::Normal, 1, 1, 0).unwrap();
        b.add_preview(png(300, 400, 7), 0, 0).unwrap();
        b.build(&path).unwrap();

        let fd = {
            use std::os::fd::IntoRawFd;
            std::fs::File::open(&path).unwrap().into_raw_fd()
        };
        let book = Book::open_fd(fd).unwrap();
        let info = book.info();
        assert_eq!((info.page_count, info.webtoon, info.has_music, info.has_sfx, info.has_text, info.has_dark), (3, false, true, true, true, true));
        let pages = book.pages();
        assert_eq!(pages[0].role, "FrontCover");
        assert!(pages[2].volume_start);
        assert_eq!((pages[1].chunks[0].width, pages[1].chunks[0].height, pages[1].chunks[0].format.as_str()), (800, 1200, "png"));
        assert_eq!(book.chunk(pages[1].chunks[0].index).unwrap(), png_bytes(800, 1200, 2));
        assert!(book.chunk_ok(0));
        assert!(book.chunk(99).is_err());

        let contents = book.contents();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[1].first_page, 2);
        assert!(book.position_label(2).is_some());

        // Music continues on page 1 (the effect doesn't interrupt it), volume 2 is a different chapter, silence.
        let music = book.music_at(1).unwrap();
        assert_eq!((music.kind, music.looped, music.duration_ms, music.mime.as_str()), (AuxType::Music, true, 9000, "audio/ogg"));
        assert!(book.music_at(2).is_none());
        let sfx = book.sfx_of(1);
        assert_eq!(sfx.len(), 1);
        assert_eq!(book.aux_data(sfx[0].index).unwrap(), b"OggSthunder");
        assert_eq!(book.page_text(1).unwrap().as_deref(), Some("Grr"));
        assert_eq!(book.page_text(0).unwrap(), None);
        assert!(book.page_text(42).is_err());
        assert!(book.dark_chunk(1, 0).is_some());
        assert!(book.dark_chunk(0, 0).is_none());

        // Spread: the cover (role) alone, and a pair doesn't cross the chapter boundary (volume 2).
        let groups: Vec<Vec<u32>> = book.page_groups(true, false, false).into_iter().map(|g| g.pages).collect();
        assert_eq!(groups, vec![vec![0], vec![1], vec![2]]);
        assert_eq!(book.covers().len(), 1);
        assert_eq!(book.main_cover().unwrap(), book.cover_data(0).unwrap());
        assert!(book.check(true).ok);
        assert!(book.metadata_json().is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn spreads_pair_pages_like_desktop() {
        let path = temp("spread");
        let mut b = Builder::new();
        for i in 0..4 {
            b.add_page(png(800, 1200, i), PageRole::Normal, 1, u16::from(i) + 1, 0).unwrap();
        }
        b.build(&path).unwrap();
        let book = Book::open_path(path.to_string_lossy().into_owned()).unwrap();
        assert_eq!(book.page_groups(false, true, false).len(), 4);
        let groups: Vec<Vec<u32>> = book.page_groups(true, true, false).into_iter().map(|g| g.pages).collect();
        assert_eq!(groups, vec![vec![0], vec![1, 2], vec![3]]);
        // Right to left: the second page of a pair is on the left.
        assert_eq!(book.page_groups(true, true, true)[1].visual, vec![2, 1]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn webtoon_strip_sfx_points_to_fragment() {
        let path = temp("webtoon");
        let mut b = Builder::new();
        b.add_webtoon(vec![png(800, 1000, 1), png(800, 1000, 2), png(800, 1000, 3)], 1, 1, 0).unwrap();
        let mut sfx = AuxInput::new(AuxKind::Sfx, ogg(b"door"));
        sfx.trigger_y = 1500; // middle of the second fragment
        b.attach_aux(sfx).unwrap();
        b.build(&path).unwrap();
        let book = Book::open_bytes(std::fs::read(&path).unwrap()).unwrap();
        assert!(book.info().webtoon);
        assert_eq!(book.pages()[0].chunks.len(), 3);
        let strip = book.strip_sfx();
        assert_eq!(strip.len(), 1);
        assert_eq!((strip[0].chunk, strip[0].fraction), (1, 0.5));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_non_vnmf() {
        assert!(matches!(Book::open_bytes(vec![0; 4096]), Err(VnmfError::Open { .. })));
        assert!(matches!(Book::open_path("/nonexistent.vnmf".into()), Err(VnmfError::Open { .. })));
    }
}
