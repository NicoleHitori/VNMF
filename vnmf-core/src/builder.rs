// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

use std::borrow::Cow;
use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use imagesize::blob_size;
use log::info;
use memmap2::Mmap;
use zerocopy::{Immutable, IntoBytes};
use zerocopy::little_endian as le;

use crate::blocksize::BlockSize;
use crate::formats::{AuxFormat, PageFormat};
use crate::header::Header;
use crate::header::bit_flag::HeaderBitEnum;
use crate::parity::{self, ParityLayout};
use crate::spec;
use crate::vnmf::{AuxKind, AuxRecord, EntryRecord, PageBitFlags, PageRecord, PageRole, PreviewRecord, RecordCrc};

/// Safe narrowing of an integer type.
///
/// FIX: there used to be `as u32` / `as u16` everywhere, silently dropping the high bits
/// (a file > 4 GiB, a page > 65 535 chunks, etc.), and wrong values ended up in the tables.
/// Now an overflow is an explicit `InvalidInput` error.
fn narrow<T, U>(value: U, what: &str) -> io::Result<T>
where
    T: TryFrom<U>,
    U: Copy + Display,
{
    T::try_from(value).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{what} ({value}) doesn't fit into the format field"),
        )
    })
}

/// Size in bytes of a table of `count` records of type `T` (checked against u32).
fn table_len<T>(count: usize, what: &str) -> io::Result<u32> {
    let bytes = count.saturating_mul(std::mem::size_of::<T>());
    narrow(bytes, what)
}

/// A shared immutable buffer whose slices can be chunks (e.g. the [`crate::VnmfReader`]
/// of an open `.vnmf` — then pages are moved into the new file without copying in memory).
pub type SharedBytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// Where the chunk bytes live.
#[derive(Clone)]
pub enum ChunkData {
    /// A file on disk — streamed only during `build()`.
    File(PathBuf),
    /// Bytes in memory — e.g. an entry from a CBZ archive that we don't want to unpack to disk.
    Memory(Arc<[u8]>),
    /// A range of a shared buffer (see [`SharedBytes`]).
    Shared(SharedBytes, std::ops::Range<usize>),
}

impl ChunkData {
    /// The chunk bytes if they are already in memory (everything except a file on disk).
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            ChunkData::File(_) => None,
            ChunkData::Memory(bytes) => Some(bytes),
            ChunkData::Shared(buf, range) => (*buf).as_ref().as_ref().get(range.clone()),
        }
    }
}

impl std::fmt::Debug for ChunkData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChunkData::File(path) => f.debug_tuple("File").field(path).finish(),
            ChunkData::Memory(bytes) => write!(f, "Memory({} B)", bytes.len()),
            ChunkData::Shared(_, range) => write!(f, "Shared({range:?})"),
        }
    }
}

/// Input source of a data chunk (image, audio, text layer)
#[derive(Debug, Clone)]
pub struct ChunkSource {
    pub data: ChunkData,
    pub size: u32,
    pub crc: u32,
    /// Human-readable name for messages (file name in the archive, etc.).
    pub label: Option<String>,
}

/// How many bytes from the start of a chunk are enough to detect the format and image dimensions.
const PROBE_LEN: usize = 64 * 1024;

impl ChunkSource {
    /// Streams the metadata and computes the file's CRC32
    pub fn from_file<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;
        // FIX: it was `len() as u32` — a file over 4 GiB was silently truncated to the low 32 bits.
        let size = narrow(file.metadata()?.len(), "chunk size")?;

        let mut hasher = crc32fast::Hasher::new();
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let crc = hasher.finalize();

        Ok(Self { data: ChunkData::File(path), size, crc, label: None })
    }

    /// A chunk from bytes in memory.
    pub fn from_bytes(bytes: impl Into<Arc<[u8]>>) -> io::Result<Self> {
        let bytes: Arc<[u8]> = bytes.into();
        let size = narrow(bytes.len(), "chunk size")?;
        let crc = crc32fast::hash(&bytes);
        Ok(Self { data: ChunkData::Memory(bytes), size, crc, label: None })
    }

    /// A chunk that is a slice of a shared buffer, without copying. The CRC is computed over the bytes themselves.
    pub fn from_shared(buf: SharedBytes, range: std::ops::Range<usize>) -> io::Result<Self> {
        let bytes = (*buf).as_ref().as_ref().get(range.clone()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, format!("slice {range:?} is out of the buffer bounds"))
        })?;
        let size = narrow(bytes.len(), "chunk size")?;
        let crc = crc32fast::hash(bytes);
        Ok(Self { data: ChunkData::Shared(buf, range), size, crc, label: None })
    }

    /// Sets the source name for error messages (e.g. the file name in a CBZ).
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The start of the chunk for format detection (without reading the whole file).
    pub(crate) fn head(&self) -> io::Result<Cow<'_, [u8]>> {
        match &self.data {
            ChunkData::Memory(_) | ChunkData::Shared(..) => {
                let bytes = self.data.bytes().unwrap_or_default();
                Ok(Cow::Borrowed(&bytes[..bytes.len().min(PROBE_LEN)]))
            }
            ChunkData::File(path) => {
                let mut head = Vec::with_capacity(PROBE_LEN);
                File::open(path)?.take(PROBE_LEN as u64).read_to_end(&mut head)?;
                Ok(Cow::Owned(head))
            }
        }
    }

    /// Human-readable description of the source for error messages.
    fn describe(&self) -> String {
        if let Some(label) = &self.label {
            return label.clone();
        }
        match &self.data {
            ChunkData::File(path) => path.display().to_string(),
            ChunkData::Memory(_) | ChunkData::Shared(..) => "<bytes in memory>".to_string(),
        }
    }

    /// Detects the format (by magic bytes) and the image dimensions.
    pub fn probe_image(&self) -> io::Result<(u32, u32, PageFormat)> {
        let head = self.head()?;
        let format = PageFormat::detect(&head);

        // FIX: it was `.min(u16::MAX)` — the height of long pages was silently corrupted. Now u32 with a check.
        let (width, height) = match blob_size(&head) {
            Ok(d) => (narrow(d.width, "image width")?, narrow(d.height, "image height")?),
            Err(_) => (0, 0),
        };

        Ok((width, height, format))
    }
}

/// Input data for creating a page (Page)
#[derive(Debug, Clone)]
pub struct PageInput {
    pub width: u32,
    pub height: u32,
    pub format: PageFormat,
    pub role: PageRole,
    pub flags: u16,
    /// Volume: 0 — not specified. Set via [`Builder::set_volume`].
    pub volume_id: u16,
    pub chapter_id: u16,
    pub page_number_in_chapter: u16,
    pub chunks: Vec<ChunkSource>,
    pub aux: Vec<AuxInput>,
}

/// A page attachment: music, an effect, a text layer, a dark version or an arbitrary file.
#[derive(Debug, Clone)]
pub struct AuxInput {
    pub kind: AuxKind,
    pub chunk: ChunkSource,
    /// [`crate::AuxFlags`].
    pub flags: u16,
    pub delay_ms: u32,
    pub trigger_y: u32,
    /// Dark version: which page image it replaces.
    pub part: u32,
    /// How long to play, ms; 0 — unlimited.
    pub duration_ms: u32,
    /// Music: on how many pages; 0 — until the next music or the end of the chapter.
    pub pages: u16,
    /// 1..=100 %, 0 — 100 %.
    pub volume: u8,
}

impl AuxInput {
    pub fn new(kind: AuxKind, chunk: ChunkSource) -> Self {
        Self { kind, chunk, flags: 0, delay_ms: 0, trigger_y: 0, part: 0, duration_ms: 0, pages: 0, volume: 0 }
    }
}

/// Input data for a preview
#[derive(Debug, Clone)]
pub struct PreviewInput {
    pub volume_id: u16,
    pub width: u32,
    pub height: u32,
    pub format: PageFormat,
    pub flags: u8,
    pub chunk: ChunkSource,
}

pub struct Builder {
    flags: u16,
    uuid: [u8; 16],
    block_size: BlockSize,
    info_bytes: Option<Vec<u8>>,
    previews: Vec<PreviewInput>,
    pages: Vec<PageInput>,
    enable_tail_duplicate: bool,
    redundancy: u8,
    allow_mixed_formats: bool,
    /// Volume for pages added from now on.
    current_volume: u16,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    pub fn new() -> Self {
        Self {
            // FIX: `HasAlign` used to be set only in `with_block_size`, although sections are always
            // aligned (there's DEFAULT_BLOCK_SIZE). A file built without calling `with_block_size` was aligned,
            // but claimed the opposite.
            flags: HeaderBitEnum::HasAlign.mask(),
            uuid: [0u8; 16],
            block_size: spec::DEFAULT_BLOCK_SIZE,
            info_bytes: None,
            previews: Vec::new(),
            pages: Vec::new(),
            enable_tail_duplicate: false,
            redundancy: 0,
            allow_mixed_formats: false,
            current_volume: 0,
        }
    }

    pub fn with_uuid(mut self, uuid: [u8; 16]) -> Self {
        self.uuid = uuid;
        self
    }

    pub fn with_block_size(mut self, block_size: BlockSize) -> Self {
        self.block_size = block_size;
        self
    }

    pub fn with_tail_duplicate(mut self, enable: bool) -> Self {
        self.enable_tail_duplicate = enable;
        self
    }

    /// Reed–Solomon redundancy in percent: 0 — off, up to `spec::MAX_REDUNDANCY` (30).
    ///
    /// Protects the header, Info, tables and data from random damage: any contiguous
    /// region of up to ~`percent`% of the protected area can be recovered (`vnmfrecovery`).
    /// Automatically enables the tail copy — it's needed to find the parity when the primary
    /// header is damaged. Values above 30 are rejected in `build()`.
    /// Allow the chunks of one webtoon page to have different formats (JPEG + PNG + AVIF + …).
    ///
    /// Such a page is marked with `PageBitFlags::MIXED_FORMATS`, and the `format` field contains the format
    /// of the first chunk: the reader must detect each chunk's format by its signature.
    pub fn with_mixed_formats(mut self, allow: bool) -> Self {
        self.allow_mixed_formats = allow;
        self
    }

    pub fn with_redundancy(mut self, percent: u8) -> Self {
        self.redundancy = percent;
        self
    }

    pub fn set_flag(&mut self, flag: HeaderBitEnum) {
        self.flags |= flag.mask();
    }

    pub fn clear_flag(&mut self, flag: HeaderBitEnum) {
        self.flags &= !flag.mask();
    }

    /// Sets the info metadata (TOML / JSON / Text)
    pub fn set_info(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            self.clear_flag(HeaderBitEnum::HasInfo);
            self.info_bytes = None;
        } else {
            self.set_flag(HeaderBitEnum::HasInfo);
            self.info_bytes = Some(bytes);
        }
    }

    /// Adds a preview (a volume/chapter cover) from a file. Index 0 is the main preview.
    pub fn add_preview_file<P: AsRef<Path>>(&mut self, path: P, volume_id: u16, flags: u8) -> io::Result<()> {
        self.add_preview(ChunkSource::from_file(path)?, volume_id, flags)
    }

    /// Adds a preview from any source (file or memory).
    pub fn add_preview(&mut self, chunk: ChunkSource, volume_id: u16, flags: u8) -> io::Result<()> {
        let (width, height, format) = chunk.probe_image()?;

        self.previews.push(PreviewInput {
            volume_id,
            width,
            height,
            format,
            flags,
            chunk,
        });
        self.set_flag(HeaderBitEnum::HasPreview);
        Ok(())
    }

    /// Volume for all pages added from now on (0 — no volume).
    ///
    /// File structure — a sequence of volumes, each with chapters: boundaries are determined by a change of
    /// `volume_id` / `chapter_id` between neighbouring pages. The `IS_VOLUME_START` and
    /// `IS_CHAPTER_START` flags on the first pages are set by the builder itself.
    pub fn set_volume(&mut self, volume_id: u16) {
        self.current_volume = volume_id;
    }

    /// Adds a regular page from a single image file
    pub fn add_page_file<P: AsRef<Path>>(
        &mut self,
        path: P,
        role: PageRole,
        chapter_id: u16,
        page_number: u16,
        flags: u16,
    ) -> io::Result<()> {
        self.add_page(ChunkSource::from_file(path)?, role, chapter_id, page_number, flags)
    }

    /// Adds a regular page from any source (file or memory).
    pub fn add_page(
        &mut self,
        chunk: ChunkSource,
        role: PageRole,
        chapter_id: u16,
        page_number: u16,
        flags: u16,
    ) -> io::Result<()> {
        let (width, height, format) = chunk.probe_image()?;

        self.pages.push(PageInput {
            width,
            height,
            format,
            role,
            flags,
            volume_id: self.current_volume,
            chapter_id,
            page_number_in_chapter: page_number,
            chunks: vec![chunk],
            aux: Vec::new(),
        });
        Ok(())
    }

    /// Adds a WebToon episode (one logical page consisting of several chunks
    /// arranged from top to bottom).
    ///
    /// FIX: the width, height and format used to be passed manually — the example wrote
    /// `PageFormat::WebP` for AVIF files and guessed the height. Now they are taken from the
    /// chunks themselves: the width is the maximum, the height is the sum, the format must be the same for all.
    pub fn add_webtoon_page<P: AsRef<Path>>(
        &mut self,
        chunk_paths: &[P],
        chapter_id: u16,
        page_number: u16,
        flags: u16,
    ) -> io::Result<()> {
        let chunks = chunk_paths.iter().map(ChunkSource::from_file).collect::<io::Result<Vec<_>>>()?;
        self.add_webtoon(chunks, chapter_id, page_number, flags)
    }

    /// Same as [`Self::add_webtoon_page`], but from arbitrary sources (file or memory).
    pub fn add_webtoon(
        &mut self,
        chunks: Vec<ChunkSource>,
        chapter_id: u16,
        page_number: u16,
        flags: u16,
    ) -> io::Result<()> {
        if chunks.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a webtoon page must contain at least one chunk",
            ));
        }
        narrow::<u32, _>(chunks.len(), "number of page chunks")?;

        let mut width = 0u32;
        let mut height = 0u32;
        let mut format = None;
        let mut flags = flags;
        for chunk in &chunks {
            let (w, h, f) = chunk.probe_image()?;
            width = width.max(w);
            height = height.checked_add(h).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "the total webtoon height exceeds u32")
            })?;
            match format {
                None => format = Some(f),
                Some(first) if first != f && self.allow_mixed_formats => {
                    flags |= PageBitFlags::MIXED_FORMATS;
                }
                Some(first) if first != f => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "webtoon chunks have different formats: {first} and {f} ({})",
                            chunk.describe()
                        ),
                    ));
                }
                Some(_) => {}
            }
        }

        self.set_flag(HeaderBitEnum::IsWebToon);
        self.pages.push(PageInput {
            width,
            height,
            format: format.unwrap_or(PageFormat::Unknown),
            role: PageRole::Normal,
            flags,
            volume_id: self.current_volume,
            chapter_id,
            page_number_in_chapter: page_number,
            chunks,
            aux: Vec::new(),
        });
        Ok(())
    }

    /// Attaches a file as an attachment to the last added page (type — by contents, see
    /// [`AuxKind::guess`]).
    pub fn attach_aux_file<P: AsRef<Path>>(&mut self, path: P) -> io::Result<()> {
        let chunk = ChunkSource::from_file(path)?;
        let kind = AuxKind::guess(AuxFormat::detect(&chunk.head()?));
        self.attach_aux(AuxInput::new(kind, chunk))
    }

    /// Attaches an attachment to the last added page. A page may have several attachments
    /// (music + effects + text + dark version).
    pub fn attach_aux(&mut self, aux: AuxInput) -> io::Result<()> {
        let page = self.pages.last_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "there is no page to attach the attachment to")
        })?;
        if aux.kind == AuxKind::DarkImage && aux.part as usize >= page.chunks.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("dark version for image {} — the page has only {}", aux.part, page.chunks.len()),
            ));
        }
        if aux.volume > 100 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("volume {}% exceeds 100%", aux.volume)));
        }
        page.aux.push(aux);
        Ok(())
    }

    fn reserve(&self, cursor: &mut u64, len: u64) -> u64 {
        reserve(self.block_size, cursor, len)
    }

    /// Builds and saves the file to disk
    ///
    /// FIX: writing used to go straight into the target file, and on an error midway (an input file
    /// disappeared or changed) a half-written `.vnmf` with a valid magic was left on disk. Now the file
    /// is written next to it as `*.tmp` and atomically renamed only on success.
    pub fn build<P: AsRef<Path>>(&self, output_path: P) -> io::Result<()> {
        let output_path = output_path.as_ref();
        let mut tmp_name = output_path.as_os_str().to_owned();
        tmp_name.push(".tmp");
        let tmp_path = PathBuf::from(tmp_name);

        match self.write_file(&tmp_path) {
            Ok(()) => fs::rename(&tmp_path, output_path),
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                Err(e)
            }
        }
    }

    fn write_file(&self, path: &Path) -> io::Result<()> {
        info!("--- Starting to build a VNMF v{}.{} file ---", spec::DEFAULT_VERSION.0, spec::DEFAULT_VERSION.1);

        if self.redundancy > spec::MAX_REDUNDANCY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("redundancy {}% exceeds the maximum of {}%", self.redundancy, spec::MAX_REDUNDANCY),
            ));
        }
        let with_parity = self.redundancy > 0;

        let mut flags = self.flags;
        if self.enable_tail_duplicate || with_parity {
            flags |= HeaderBitEnum::HasTailDup.mask();
        }
        if with_parity {
            flags |= HeaderBitEnum::HasParity.mask();
        }

        // ==========================================
        // 1. SECTION LAYOUT
        // ==========================================
        // FIX: the offset computation used to consist of a chain of `if offset > 0 { … } else { … }`
        // for each table. Now there's one cursor and `reserve()` — the same layout is visible at a glance.
        let info_bytes = self.info_bytes.as_deref().unwrap_or(&[]);
        let info_len: u32 = narrow(info_bytes.len(), "Info size")?;

        let total_chunks: usize = self.pages.iter().map(|p| p.chunks.len() + p.aux.len()).sum();
        let total_aux: usize = self.pages.iter().map(|p| p.aux.len()).sum();

        let front = FrontLayout::compute(
            self.block_size,
            info_len,
            [
                table_len::<PreviewRecord>(self.previews.len(), "preview table")?,
                table_len::<PageRecord>(self.pages.len(), "page table")?,
                table_len::<AuxRecord>(total_aux, "attachments table")?,
                table_len::<EntryRecord>(total_chunks, "chunk table")?,
            ],
        );
        let (info_offset, [preview_table_offset, page_table_offset, aux_table_offset, chunks_table_offset]) =
            (front.info, front.tables);
        let mut cursor = front.end;

        // FIX: the data write order used to be reproduced by a separate loop with a manual `chunk_idx`,
        // which had to match the layout loop. Now the layout directly produces a list of
        // (source, offset), and writing simply walks it.
        let mut data_layout: Vec<(&ChunkSource, u64)> = Vec::new();

        // Preparing PreviewTable records
        let mut preview_records = Vec::with_capacity(self.previews.len());
        for prev in &self.previews {
            let offset = self.reserve(&mut cursor, prev.chunk.size.into());
            data_layout.push((&prev.chunk, offset));
            preview_records.push(PreviewRecord {
                offset: le::U64::new(offset),
                length: le::U32::new(prev.chunk.size),
                crc: le::U32::new(prev.chunk.crc),
                volume_id: le::U16::new(prev.volume_id),
                width: le::U32::new(prev.width),
                height: le::U32::new(prev.height),
                format: prev.format as u8,
                flags: prev.flags,
                record_crc: le::U32::ZERO,
            });
            preview_records.last_mut().unwrap().seal();
        }

        // Preparing ChunksTable and PageTable records
        let mut chunk_records = Vec::with_capacity(total_chunks);
        let mut page_records = Vec::with_capacity(self.pages.len());
        let mut aux_records: Vec<AuxRecord> = Vec::with_capacity(total_aux);

        let mut prev: Option<(u16, u16)> = None;
        for page in &self.pages {
            // Volume and chapter starts — by a change of numbers (flags from the caller are preserved).
            let mut page_flags = page.flags;
            if prev.is_none_or(|(v, _)| v != page.volume_id) {
                page_flags |= PageBitFlags::IS_CHAPTER_START;
                if page.volume_id != 0 {
                    page_flags |= PageBitFlags::IS_VOLUME_START;
                }
            } else if prev.is_some_and(|(_, c)| c != page.chapter_id) {
                page_flags |= PageBitFlags::IS_CHAPTER_START;
            }
            prev = Some((page.volume_id, page.chapter_id));

            let first_chunk_idx: u32 = narrow(chunk_records.len(), "chunk index")?;
            let chunk_count: u32 = narrow(page.chunks.len(), "number of page chunks")?;
            let page_idx: u32 = narrow(page_records.len(), "page index")?;

            for ch in &page.chunks {
                let offset = self.reserve(&mut cursor, ch.size.into());
                data_layout.push((ch, offset));
                chunk_records.push(EntryRecord::new(offset, ch.size, ch.crc));
            }

            // Attachments: their chunks come right after the page images, the records form a contiguous range.
            let first_aux_idx: u32 = if page.aux.is_empty() { 0 } else { narrow(aux_records.len(), "attachment index")? };
            for aux in &page.aux {
                let chunk_idx: u32 = narrow(chunk_records.len(), "chunk index")?;
                let offset = self.reserve(&mut cursor, aux.chunk.size.into());
                data_layout.push((&aux.chunk, offset));
                chunk_records.push(EntryRecord::new(offset, aux.chunk.size, aux.chunk.crc));
                let mut record = AuxRecord {
                    page_idx: le::U32::new(page_idx),
                    chunk_idx: le::U32::new(chunk_idx),
                    kind: aux.kind as u8,
                    format: AuxFormat::detect(&aux.chunk.head()?) as u8,
                    flags: le::U16::new(aux.flags),
                    delay_ms: le::U32::new(aux.delay_ms),
                    trigger_y: le::U32::new(aux.trigger_y),
                    part: le::U32::new(aux.part),
                    duration_ms: le::U32::new(aux.duration_ms),
                    pages: le::U16::new(aux.pages),
                    volume: aux.volume,
                    reserved0: 0,
                    reserved: [0; 12],
                    record_crc: le::U32::ZERO,
                };
                record.seal();
                aux_records.push(record);
                flags |= match aux.kind {
                    AuxKind::Music | AuxKind::Sfx => HeaderBitEnum::HasAudio.mask(),
                    AuxKind::Text => HeaderBitEnum::HasText.mask(),
                    AuxKind::DarkImage => HeaderBitEnum::HasDarkMode.mask(),
                    AuxKind::Attachment => 0,
                };
            }

            page_records.push(PageRecord {
                width: le::U32::new(page.width),
                height: le::U32::new(page.height),
                first_chunk_idx: le::U32::new(first_chunk_idx),
                chunk_count: le::U32::new(chunk_count),
                first_aux_idx: le::U32::new(first_aux_idx),
                aux_count: le::U32::new(narrow(page.aux.len(), "number of page attachments")?),
                volume_id: le::U16::new(page.volume_id),
                chapter_id: le::U16::new(page.chapter_id),
                page_number_in_chapter: le::U16::new(page.page_number_in_chapter),
                flags: le::U16::new(page_flags),
                format: page.format as u8,
                role: page.role as u8,
                reserved0: [0; 2],
                reserved: [0; 24],
                record_crc: le::U32::ZERO,
            });
            page_records.last_mut().unwrap().seal();
        }

        // ==========================================
        // 2. HEADER (128 bytes)
        // ==========================================
        let info_entry = if info_len > 0 {
            EntryRecord::new(info_offset, info_len, crc32fast::hash(info_bytes))
        } else {
            EntryRecord::default()
        };

        let mut header = Header::new(
            flags,
            self.uuid,
            info_entry,
            [
                section_entry(preview_table_offset, &preview_records),
                section_entry(page_table_offset, &page_records),
                section_entry(aux_table_offset, &aux_records),
                section_entry(chunks_table_offset, &chunk_records),
            ],
            self.block_size,
        );
        // The protected area ends at the aligned end of the data — that's where the parity starts.
        let parity_layout = if with_parity {
            let offset = self.block_size.align_up(cursor);
            header.parity_offset.set(offset);
            header.redundancy = self.redundancy;
            ParityLayout::new(offset, self.block_size, self.redundancy)
        } else {
            None
        };
        header.seal();

        // ==========================================
        // 3. STREAMING WRITE TO THE FILE
        // ==========================================
        // Reading is needed too: the parity is computed over the already written file via mmap.
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path)?;
        let mut writer = PosWriter::new(BufWriter::with_capacity(256 * 1024, file));

        // A. Header (128 B) and Info
        writer.write_all(header.as_bytes())?;
        writer.write_all(info_bytes)?;

        // B. Tables
        writer.write_at(preview_table_offset, preview_records.as_bytes())?;
        writer.write_at(page_table_offset, page_records.as_bytes())?;
        writer.write_at(aux_table_offset, aux_records.as_bytes())?;
        writer.write_at(chunks_table_offset, chunk_records.as_bytes())?;

        // C. Preview, page and aux chunk data
        for (src, offset) in data_layout {
            writer.pad_to(offset)?;
            writer.copy_chunk(src)?;
        }

        // D. Reed–Solomon parity over everything written above
        if let Some(layout) = &parity_layout {
            writer.pad_to(layout.offset)?;
            writer.flush()?;
            let file = writer.inner.get_mut();
            // SAFETY: the file was just created by this process and nobody else modifies it;
            // the mmap lives only until the end of the block, while we only read the protected area.
            let map = unsafe { Mmap::map(&*file)? };
            parity::write_parity(file, &map[..layout.offset as usize], layout)?;
            drop(map);
            writer.pos = layout.end();
        }

        // E. Backup copy at the end of the file (mandatory with parity)
        if header.flags.get() & HeaderBitEnum::HasTailDup.mask() != 0 {
            write_tail(
                &mut writer,
                &header,
                info_bytes,
                [preview_records.as_bytes(), page_records.as_bytes(), aux_records.as_bytes(), chunk_records.as_bytes()],
            )?;
        }

        writer.flush()?;
        // FIX: without fsync, the atomic rename could leave an empty file after a power failure.
        writer.inner.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        info!("--- VNMF file built successfully ---");
        Ok(())
    }

}

/// Reserves space for a section of length `len` with block alignment and returns its offset.
/// An empty section takes no space and has offset 0 (that's how the format marks absence).
pub(crate) fn reserve(block: BlockSize, cursor: &mut u64, len: u64) -> u64 {
    if len == 0 {
        return 0;
    }
    let offset = block.align_up(*cursor);
    *cursor = offset + len;
    offset
}

/// Layout of the front block: the header, Info (right after it), four tables (aligned)
/// in [`TABLES`] order: previews, pages, attachments, chunks.
///
/// Deterministic given the section lengths — so `recovery` can restore the primary header
/// from the tail one (lengths and CRCs are the same in both, only the offsets differ).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrontLayout {
    pub info: u64,
    pub tables: [u64; 4],
    /// End of the last section (not aligned).
    pub end: u64,
}

/// Header fields with tables — in the layout order of the primary copy.
pub(crate) const TABLES: [fn(&mut Header) -> &mut EntryRecord; 4] =
    [|h| &mut h.preview_table, |h| &mut h.page_table, |h| &mut h.aux_table, |h| &mut h.chunks_table];

impl FrontLayout {
    pub fn compute(block: BlockSize, info_len: u32, table_lens: [u32; 4]) -> Self {
        let mut cursor = spec::HEADER_SIZE as u64;
        let info = if info_len > 0 { cursor } else { 0 };
        cursor += u64::from(info_len);
        let tables = table_lens.map(|len| reserve(block, &mut cursor, len.into()));
        Self { info, tables, end: cursor }
    }

    /// Layout of the primary copy for the section lengths from header `h`.
    pub fn of(h: &Header, block: BlockSize) -> Self {
        let mut copy = *h;
        Self::compute(block, h.info.length(), TABLES.map(|f| f(&mut copy).length()))
    }
}

/// Writes backup copies of the tables, Info and the header at the end of the file.
///
/// FIX: this used to write copies of the tables and *the same* header, whose offsets point to the
/// tables at the START of the file. If the start was damaged, the tail copies were unreachable, i.e.
/// the backup gave nothing. Now the tail header has its own EntryRecords pointing to the
/// tail copies, and always occupies exactly the last 128 bytes of the file — that's where the reader looks for it.
///
/// `primary` — the primary header: flags, UUID, block size and parity parameters are taken from it
/// (they are the same in both copies).
pub(crate) fn write_tail<W: Write>(
    writer: &mut PosWriter<W>,
    primary: &Header,
    info_bytes: &[u8],
    tables: [&[u8]; 4],
) -> io::Result<()> {
    let block = BlockSize::try_from(primary.block_size_shift)
        .map_err(|v| io::Error::new(io::ErrorKind::InvalidData, format!("unknown block size {v}")))?;

    // The tail goes in reverse order: chunks, attachments, pages, previews, Info, header.
    let mut tail = *primary;
    for (field, bytes) in TABLES.iter().zip(tables).rev() {
        *field(&mut tail) = write_tail_section(writer, block, bytes)?;
    }
    tail.info = write_tail_section(writer, block, info_bytes)?;
    tail.seal();

    writer.pad_to(block.align_up(writer.pos))?;
    writer.write_all(tail.as_bytes())
}

fn write_tail_section<W: Write>(writer: &mut PosWriter<W>, block: BlockSize, bytes: &[u8]) -> io::Result<EntryRecord> {
    if bytes.is_empty() {
        return Ok(EntryRecord::default());
    }
    let offset = block.align_up(writer.pos);
    writer.write_at(offset, bytes)?;
    // The length was already checked against u32 while laying out the primary sections.
    Ok(EntryRecord::new(offset, bytes.len() as u32, crc32fast::hash(bytes)))
}

/// EntryRecord for a records table (an empty table → a zero record).
fn section_entry<T: IntoBytes + Immutable>(offset: u64, records: &[T]) -> EntryRecord {
    let bytes = records.as_bytes();
    if bytes.is_empty() {
        EntryRecord::default()
    } else {
        EntryRecord::new(offset, bytes.len() as u32, crc32fast::hash(bytes))
    }
}

/// A writer that tracks its current position itself.
///
/// FIX: the position used to be taken via `stream_position()` on a `BufWriter`, which is a `seek` that
/// flushes the buffer to disk on EVERY call (i.e. before every chunk) — the 256 KB buffer
/// effectively didn't work. Also, `Seek` is no longer needed — any `Write` can be used.
pub(crate) struct PosWriter<W> {
    pub inner: W,
    pub pos: u64,
}

impl<W: Write> PosWriter<W> {
    pub fn new(inner: W) -> Self {
        Self::at(inner, 0)
    }

    /// A writer that continues writing from position `pos` (inner must already be there).
    pub fn at(inner: W, pos: u64) -> Self {
        Self { inner, pos }
    }

    /// Writes alignment zeros up to the given position (without heap allocation)
    ///
    /// FIX: if the current position was already PAST `target`, this used to be silently ignored and the next
    /// section was written at the wrong offset. Now it's a layout error.
    pub fn pad_to(&mut self, target: u64) -> io::Result<()> {
        const ZEROES: [u8; 4096] = [0u8; 4096];
        if target < self.pos {
            return Err(io::Error::other(format!(
                "layout error: position {} is already past the section boundary {target}",
                self.pos
            )));
        }
        while self.pos < target {
            let to_write = (target - self.pos).min(ZEROES.len() as u64) as usize;
            self.write_all(&ZEROES[..to_write])?;
        }
        Ok(())
    }

    /// Writes a section at its offset; an empty section is skipped.
    pub fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.pad_to(offset)?;
        self.write_all(bytes)
    }

    /// Copies exactly `src.size` bytes of the file and checks the CRC against what's recorded in the table.
    ///
    /// FIX: this used to be an `io::copy` of the whole file. The size and CRC are computed in `add_*`, while
    /// copying happens in `build()`; if the file changed in between, a wrong CRC ended up in the table,
    /// and if it grew — it "ran over" the next chunk and silently corrupted the file.
    fn copy_chunk(&mut self, src: &ChunkSource) -> io::Result<()> {
        let changed = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the file changed after being added to the builder: {}", src.describe()),
            )
        };

        let path = match &src.data {
            // Bytes in memory are immutable — their CRC was already computed in `from_bytes` / `from_shared`.
            ChunkData::Memory(_) | ChunkData::Shared(..) => {
                let bytes = src.data.bytes().ok_or_else(changed)?;
                return self.write_all(bytes);
            }
            ChunkData::File(path) => path,
        };

        let mut reader = BufReader::with_capacity(64 * 1024, File::open(path)?);
        let mut hasher = crc32fast::Hasher::new();
        let mut remaining = u64::from(src.size);
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if n as u64 > remaining {
                return Err(changed());
            }
            hasher.update(&buf[..n]);
            self.write_all(&buf[..n])?;
            remaining -= n as u64;
        }

        if remaining != 0 || hasher.finalize() != src.crc {
            return Err(changed());
        }
        Ok(())
    }
}

impl<W: Write> Write for PosWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.pos += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
