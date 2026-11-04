// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

use std::fs::File;
use std::io;
use std::ops::Range;
use std::path::Path;

use log::warn;
use memmap2::Mmap;
use zerocopy::{FromBytes, Immutable, KnownLayout};

use crate::blocksize::BlockSize;
use crate::header::Header;
use crate::header::bit_flag::HeaderBitEnum;
use crate::parity::{self, ParityLayout, ParityStatus};
use crate::spec;
use crate::vnmf::{AuxRecord, EntryRecord, PageRecord, PreviewRecord, RecordCrc};

#[derive(thiserror::Error, Debug)]
pub enum ReaderError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Invalid file magic ID: expected 'vnmf', got {0:?}")]
    InvalidMagic([u8; 4]),

    #[error("Unsupported format version {0}.{1}")]
    UnsupportedVersion(u8, u8),

    #[error("Unknown block size (shift = {0})")]
    InvalidBlockSize(u8),

    #[error("CRC32 integrity error in {section}: expected {expected:#010x}, found {actual:#010x}")]
    CrcMismatch {
        section: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("Section size mismatch in {section}: {message}")]
    InvalidTableSize {
        section: &'static str,
        message: String,
    },

    #[error("Record #{index} in {section} is damaged: record CRC32 mismatch")]
    RecordCrc { section: &'static str, index: usize },

    #[error("Index out of range: requested {index}, but only {total} available")]
    IndexOutOfBounds { index: usize, total: usize },

    #[error("File is too small for VNMF (size: {0} bytes)")]
    FileTooSmall(u64),
}

/// Which metadata copy to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderCopy {
    /// The primary header at the start of the file and the tables after it.
    Primary,
    /// The tail header (the last 128 bytes) and the table copies before it.
    Tail,
}

/// Parsed file metadata (header + tables), independent of which
/// copy (primary or tail) it was read from.
pub(crate) struct Parsed {
    pub(crate) header: Header,
    pub(crate) block_size: BlockSize,
    pub(crate) info_range: Range<usize>,
    pub(crate) previews: Vec<PreviewRecord>,
    pub(crate) pages: Vec<PageRecord>,
    pub(crate) aux: Vec<AuxRecord>,
    pub(crate) chunks: Vec<EntryRecord>,
    /// A 1.x format file converted in memory into 2.0 records (see `legacy`).
    pub(crate) legacy: Option<(u8, u8)>,
}

/// High-performance reader for the VNMF 2.0 format (and 1.x — read-only)
///
/// Supports zero-copy memory access thanks to `memmap2`.
pub struct VnmfReader {
    mmap: Mmap,
    parsed: Parsed,
    recovered_from_tail: bool,
}

impl VnmfReader {
    /// Opens a VNMF file and parses the header and metadata tables
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, ReaderError> {
        let file = File::open(path)?;
        // SAFETY: the mmap stays valid as long as the file isn't modified externally. If another process
        // truncates or rewrites the file while it's being read, that's UB (at worst SIGBUS) — for
        // a reader of read-only containers this is an acceptable trade-off for zero-copy.
        let mmap = unsafe { Mmap::map(&file)? };
        Self::from_mmap(mmap)
    }

    /// Opens an already open file (on Android — a descriptor from `ContentResolver` when there's no path).
    /// The file can be closed right after the call: the memory mapping doesn't depend on it.
    pub fn from_file(file: &File) -> Result<Self, ReaderError> {
        // SAFETY: same as in `open`.
        let mmap = unsafe { Mmap::map(file)? };
        Self::from_mmap(mmap)
    }

    /// A file already read into memory: for sources that can't be memory-mapped
    /// (a pipe, a network document provider). The bytes are copied into an anonymous mapping.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ReaderError> {
        if bytes.len() < spec::HEADER_SIZE {
            return Err(ReaderError::FileTooSmall(bytes.len() as u64));
        }
        let mut anon = memmap2::MmapMut::map_anon(bytes.len())?;
        anon.copy_from_slice(bytes);
        Self::from_mmap(anon.make_read_only()?)
    }

    /// Initializes the reader from an already open memory-mapped buffer.
    ///
    /// If the primary header or tables are damaged and the file has the `HasTailDup` flag,
    /// the metadata is read from the backup copy at the end of the file (see [`Self::is_recovered`]).
    pub fn from_mmap(mmap: Mmap) -> Result<Self, ReaderError> {
        // FIX: the size check used to be only in `open`, and the public `from_mmap` did
        // `bytes[..128]` and panicked on a short buffer.
        if mmap.len() < spec::HEADER_SIZE {
            return Err(ReaderError::FileTooSmall(mmap.len() as u64));
        }

        // FIX: the backup copy at the end of the file used to be written but never read.
        let tail_pos = mmap.len() - spec::HEADER_SIZE;
        let tail = || {
            (tail_pos >= spec::HEADER_SIZE)
                .then(|| Self::parse(&mmap, tail_pos).ok())
                .flatten()
                .filter(|p| HeaderBitEnum::HasTailDup.is_set(p.header.flags.get()))
        };

        let primary_err = match Self::parse(&mmap, 0) {
            // The primary copy is "1.x" and the tail is an intact 2.0: it's a 2.0 file with a damaged version byte
            // (see `recovery::is_legacy`), so trust the tail.
            Ok(parsed) if parsed.legacy.is_some() && tail().is_some_and(|t| t.legacy.is_none()) => {
                ReaderError::UnsupportedVersion(spec::LEGACY_MAJOR, mmap[5])
            }
            Ok(parsed) => {
                return Ok(Self { mmap, parsed, recovered_from_tail: false });
            }
            Err(e) => e,
        };

        if let Some(parsed) = tail() {
            warn!("Primary metadata is damaged ({primary_err}); using the backup copy from the end of the file");
            return Ok(Self { mmap, parsed, recovered_from_tail: true });
        }

        Err(primary_err)
    }

    /// Opens a file using only the given metadata copy (no automatic selection).
    pub fn open_with<P: AsRef<Path>>(path: P, copy: HeaderCopy) -> Result<Self, ReaderError> {
        let file = File::open(path)?;
        // SAFETY: see `open`.
        let mmap = unsafe { Mmap::map(&file)? };
        Self::from_mmap_with(mmap, copy)
    }

    /// Like [`Self::from_mmap`], but only from the given metadata copy.
    pub fn from_mmap_with(mmap: Mmap, copy: HeaderCopy) -> Result<Self, ReaderError> {
        if mmap.len() < spec::HEADER_SIZE {
            return Err(ReaderError::FileTooSmall(mmap.len() as u64));
        }
        let parsed = match copy {
            HeaderCopy::Primary => Self::parse(&mmap, 0)?,
            HeaderCopy::Tail => {
                let parsed = Self::parse(&mmap, mmap.len() - spec::HEADER_SIZE)?;
                if !HeaderBitEnum::HasTailDup.is_set(parsed.header.flags.get()) {
                    return Err(ReaderError::InvalidTableSize {
                        section: "Header",
                        message: "the tail header copy is not marked with the HasTailDup flag".into(),
                    });
                }
                parsed
            }
        };
        Ok(Self { mmap, parsed, recovered_from_tail: copy == HeaderCopy::Tail })
    }

    /// Parses the header at position `header_pos` and all the tables it references.
    pub(crate) fn parse(bytes: &[u8], header_pos: usize) -> Result<Parsed, ReaderError> {
        if bytes.get(header_pos + 4) == Some(&spec::LEGACY_MAJOR) && bytes.get(header_pos..header_pos + 4) == Some(&spec::MAGIC) {
            return crate::legacy::parse(bytes, header_pos);
        }
        let (header, block_size) = parse_header(bytes, header_pos)?;

        // 2. Info Section
        let info_range = if header.info.is_empty() {
            0..0
        } else {
            let range = checked_range(bytes.len(), header.info.offset(), header.info.length(), "Info")?;
            verify_crc(&bytes[range.clone()], header.info.crc(), "Info")?;
            range
        };

        // 3–5. Tables
        let previews = read_table::<PreviewRecord>(bytes, &header.preview_table, "PreviewTable")?;
        let pages = read_table::<PageRecord>(bytes, &header.page_table, "PageTable")?;
        let aux = read_table::<AuxRecord>(bytes, &header.aux_table, "AuxTable")?;
        // The table CRC already matched, so a damaged record here means a write error, not a media one —
        // but check anyway: the reader must not trust records that failed their own CRC.
        verify_records(&previews, "PreviewTable")?;
        verify_records(&pages, "PageTable")?;
        verify_records(&aux, "AuxTable")?;
        let chunks = read_table::<EntryRecord>(bytes, &header.chunks_table, "ChunksTable")?;
        verify_links(&pages, &aux, chunks.len())?;

        Ok(Parsed { header, block_size, info_range, previews, pages, aux, chunks, legacy: None })
    }

    // === GETTERS AND SERVICE METHODS ===

    #[inline]
    pub fn header(&self) -> &Header {
        &self.parsed.header
    }

    /// `true` if the primary metadata was damaged and was read from the backup copy.
    #[inline]
    pub fn is_recovered(&self) -> bool {
        self.recovered_from_tail
    }

    /// Raw bytes of the whole file (for integrity checks and recovery).
    #[inline]
    pub fn raw(&self) -> &[u8] {
        &self.mmap
    }

    /// Redundancy in percent (0 — none).
    #[inline]
    pub fn redundancy(&self) -> u8 {
        if self.has_flag(HeaderBitEnum::HasParity) { self.parsed.header.redundancy } else { 0 }
    }

    /// Layout of the parity section, if any (for 1.x files — `None`: their parity CRC table
    /// has a different structure, and this version can't check or recover it).
    pub fn parity_layout(&self) -> Option<ParityLayout> {
        if self.parsed.legacy.is_some() {
            return None;
        }
        ParityLayout::new(self.parsed.header.parity_offset.get(), self.parsed.block_size, self.redundancy())
    }

    /// Format version 1.x if the file is old (its records are converted in memory; such a file
    /// can't be modified or recovered — only read and rebuilt as 2.0).
    pub fn legacy_version(&self) -> Option<(u8, u8)> {
        self.parsed.legacy
    }

    /// Checks all shards of the protected area and the parity (without changing the file).
    pub fn verify_parity(&self) -> Option<ParityStatus> {
        self.parity_layout().map(|layout| parity::check(&self.mmap, &layout))
    }

    #[inline]
    pub fn version(&self) -> (u8, u8) {
        self.parsed.legacy.unwrap_or((self.parsed.header.major_version, self.parsed.header.minor_version))
    }

    #[inline]
    pub fn uuid(&self) -> [u8; 16] {
        self.parsed.header.uuid
    }

    #[inline]
    pub fn block_size(&self) -> BlockSize {
        self.parsed.block_size
    }

    #[inline]
    pub fn has_flag(&self, flag: HeaderBitEnum) -> bool {
        flag.is_set(self.parsed.header.flags.get())
    }

    /// Returns the raw bytes of the Info section (TOML / JSON / Text)
    #[inline]
    pub fn info_bytes(&self) -> Option<&[u8]> {
        if self.parsed.info_range.is_empty() {
            None
        } else {
            Some(&self.mmap[self.parsed.info_range.clone()])
        }
    }

    /// Returns Info as a UTF-8 string
    pub fn info_as_str(&self) -> Option<Result<&str, std::str::Utf8Error>> {
        self.info_bytes().map(std::str::from_utf8)
    }

    // === PREVIEWS ===

    #[inline]
    pub fn previews_count(&self) -> usize {
        self.parsed.previews.len()
    }

    #[inline]
    pub fn previews(&self) -> &[PreviewRecord] {
        &self.parsed.previews
    }

    /// Gets the bytes of a preview by index without copying
    pub fn get_preview_data(&self, index: usize) -> Result<&[u8], ReaderError> {
        let rec = self.parsed.previews.get(index).ok_or(ReaderError::IndexOutOfBounds {
            index,
            total: self.parsed.previews.len(),
        })?;

        self.get_slice_by_coords(rec.offset.get(), rec.length.get(), "Preview Data")
    }

    /// Verifies the preview's CRC32
    pub fn verify_preview_crc(&self, index: usize) -> Result<bool, ReaderError> {
        let data = self.get_preview_data(index)?;
        Ok(crc32fast::hash(data) == self.parsed.previews[index].crc.get())
    }

    /// Gets the main preview (the cover, index 0)
    pub fn main_preview_data(&self) -> Result<&[u8], ReaderError> {
        self.get_preview_data(0)
    }

    // === PAGES AND CHUNKS ===

    #[inline]
    pub fn pages_count(&self) -> usize {
        self.parsed.pages.len()
    }

    #[inline]
    pub fn pages(&self) -> &[PageRecord] {
        &self.parsed.pages
    }

    /// Volumes and chapters (see [`crate::structure`]).
    pub fn structure(&self) -> Vec<crate::structure::VolumeSpan> {
        crate::structure::structure(&self.parsed.pages)
    }

    /// Whether the file has volumes.
    pub fn has_volumes(&self) -> bool {
        crate::structure::has_volumes(&self.parsed.pages)
    }

    #[inline]
    pub fn chunks_count(&self) -> usize {
        self.parsed.chunks.len()
    }

    #[inline]
    pub fn chunks(&self) -> &[EntryRecord] {
        &self.parsed.chunks
    }

    fn page(&self, page_idx: usize) -> Result<&PageRecord, ReaderError> {
        self.parsed.pages.get(page_idx).ok_or(ReaderError::IndexOutOfBounds {
            index: page_idx,
            total: self.parsed.pages.len(),
        })
    }

    fn chunk(&self, chunk_idx: usize) -> Result<&EntryRecord, ReaderError> {
        self.parsed.chunks.get(chunk_idx).ok_or(ReaderError::IndexOutOfBounds {
            index: chunk_idx,
            total: self.parsed.chunks.len(),
        })
    }

    /// Gets the raw bytes of a specific physical chunk by its global index
    pub fn get_chunk_data(&self, chunk_idx: usize) -> Result<&[u8], ReaderError> {
        let chunk = self.chunk(chunk_idx)?;
        self.get_slice_by_coords(chunk.offset(), chunk.length(), "Chunk Data")
    }

    /// Byte range of the chunk in the file.
    pub fn chunk_range(&self, chunk_idx: usize) -> Result<Range<usize>, ReaderError> {
        let chunk = self.chunk(chunk_idx)?;
        checked_range(self.mmap.len(), chunk.offset(), chunk.length(), "Chunk Data")
    }

    /// A chunk as a source for [`crate::Builder`] — a slice of this file without copying (for
    /// rebuilding a `.vnmf` with different parameters). The CRC is recomputed from the bytes.
    pub fn shared_chunk(self: &std::sync::Arc<Self>, chunk_idx: usize) -> Result<crate::ChunkSource, ReaderError> {
        let range = self.chunk_range(chunk_idx)?;
        let shared: crate::SharedBytes = self.clone();
        Ok(crate::ChunkSource::from_shared(shared, range)?)
    }

    /// A preview as a source for [`crate::Builder`] — a slice of this file without copying.
    pub fn shared_preview(self: &std::sync::Arc<Self>, index: usize) -> Result<crate::ChunkSource, ReaderError> {
        let rec = self.parsed.previews.get(index).ok_or(ReaderError::IndexOutOfBounds {
            index,
            total: self.parsed.previews.len(),
        })?;
        let range = checked_range(self.mmap.len(), rec.offset.get(), rec.length.get(), "Preview Data")?;
        let shared: crate::SharedBytes = self.clone();
        Ok(crate::ChunkSource::from_shared(shared, range)?)
    }

    /// Verifies a specific chunk's CRC32
    pub fn verify_chunk_crc(&self, chunk_idx: usize) -> Result<bool, ReaderError> {
        let expected = self.chunk(chunk_idx)?.crc();
        let data = self.get_chunk_data(chunk_idx)?;
        Ok(crc32fast::hash(data) == expected)
    }

    /// For regular manga (where 1 page = 1 chunk) gets the bytes of the page image
    pub fn get_page_data(&self, page_idx: usize) -> Result<&[u8], ReaderError> {
        let first_idx = self.page(page_idx)?.first_chunk_idx.get() as usize;
        self.get_chunk_data(first_idx)
    }

    /// For WebToon or multi-chunk pages returns a list of byte slices of all chunks
    pub fn get_page_chunks_data(&self, page_idx: usize) -> Result<Vec<&[u8]>, ReaderError> {
        let page = self.page(page_idx)?;
        let start = page.first_chunk_idx.get() as usize;
        let count = page.chunk_count.get() as usize;

        // The range is checked element by element in `get_chunk_data`; saturating_add only
        // protects against overflow on 32-bit platforms.
        (start..start.saturating_add(count))
            .map(|idx| self.get_chunk_data(idx))
            .collect()
    }

    // === ATTACHMENTS (AUX) ===

    #[inline]
    pub fn aux(&self) -> &[AuxRecord] {
        &self.parsed.aux
    }

    /// Page attachments (music, effects, text, dark versions). The range is checked on opening.
    pub fn page_aux(&self, page_idx: usize) -> &[AuxRecord] {
        self.parsed.pages.get(page_idx).map_or(&[], |p| &self.parsed.aux[p.aux_range()])
    }

    /// Indices (in the aux table) of the page's attachments.
    pub fn page_aux_range(&self, page_idx: usize) -> std::ops::Range<usize> {
        self.parsed.pages.get(page_idx).map_or(0..0, PageRecord::aux_range)
    }

    /// Attachment bytes by its index in the aux table.
    pub fn get_aux_data(&self, aux_idx: usize) -> Result<&[u8], ReaderError> {
        let rec = self.parsed.aux.get(aux_idx).ok_or(ReaderError::IndexOutOfBounds { index: aux_idx, total: self.parsed.aux.len() })?;
        self.get_chunk_data(rec.chunk_idx.get() as usize)
    }

    /// Whether the attachment data CRC is intact.
    pub fn verify_aux_crc(&self, aux_idx: usize) -> Result<bool, ReaderError> {
        let rec = self.parsed.aux.get(aux_idx).ok_or(ReaderError::IndexOutOfBounds { index: aux_idx, total: self.parsed.aux.len() })?;
        self.verify_chunk_crc(rec.chunk_idx.get() as usize)
    }

    /// Helper for a safe slice of the memory map
    fn get_slice_by_coords(
        &self,
        offset: u64,
        length: u32,
        section_name: &'static str,
    ) -> Result<&[u8], ReaderError> {
        let range = checked_range(self.mmap.len(), offset, length, section_name)?;
        Ok(&self.mmap[range])
    }
}

/// The whole file as bytes — so that `Arc<VnmfReader>` can be a [`crate::SharedBytes`] and pages can
/// be moved into a new file ([`crate::ChunkSource::from_shared`]) without copying.
impl AsRef<[u8]> for VnmfReader {
    fn as_ref(&self) -> &[u8] {
        &self.mmap
    }
}

/// Reads and validates only the header (magic, CRC, version, block size) at position `header_pos`.
/// Tables are not checked — `VnmfReader::open` does that. Also used for recovery.
pub fn parse_header(bytes: &[u8], header_pos: usize) -> Result<(Header, BlockSize), ReaderError> {
    let header_slice = header_pos
        .checked_add(spec::HEADER_SIZE)
        .and_then(|end| bytes.get(header_pos..end))
        .ok_or(ReaderError::FileTooSmall(bytes.len() as u64))?;
    let header = Header::read_from_bytes(header_slice).map_err(|_| ReaderError::InvalidTableSize {
        section: "Header",
        message: "Unable to parse the header structure".to_string(),
    })?;

    // Magic check
    if header.magic != spec::MAGIC {
        return Err(ReaderError::InvalidMagic(header.magic));
    }

    // Header CRC32 check
    let calculated_header_crc = header.calculate_crc();
    if header.crc.get() != calculated_header_crc {
        return Err(ReaderError::CrcMismatch {
            section: "Header",
            expected: header.crc.get(),
            actual: calculated_header_crc,
        });
    }

    // FIX: the version used to be unchecked, so a file with an incompatible major version was read with
    // unpredictable results. Minor versions are compatible by definition. 1.x files have a different
    // header layout — they are parsed by `legacy`, not by this function.
    if header.major_version != spec::DEFAULT_VERSION.0 {
        return Err(ReaderError::UnsupportedVersion(header.major_version, header.minor_version));
    }

    let block_size =
        BlockSize::try_from(header.block_size_shift).map_err(ReaderError::InvalidBlockSize)?;
    Ok((header, block_size))
}

/// Converts (offset, length) from the file into a range inside a buffer of length `total`.
///
/// FIX: it used to be `offset as usize + length` without checks. On 32-bit platforms
/// `as usize` truncated the offset, and for an "evil" offset near u64::MAX the addition overflowed:
/// in debug — a panic, in release — wrapping to a small number that passed the bounds check.
/// A damaged file must not crash the application, so only checked arithmetic here.
pub(crate) fn checked_range(total: usize, offset: u64, length: u32, section: &'static str) -> Result<Range<usize>, ReaderError> {
    let start = usize::try_from(offset).ok();
    let end = start.and_then(|s| s.checked_add(length as usize));
    match (start, end) {
        (Some(start), Some(end)) if end <= total => Ok(start..end),
        _ => Err(ReaderError::InvalidTableSize {
            section,
            message: format!("Coordinates {offset}+{length} are beyond the end of the file ({total})"),
        }),
    }
}

/// Cross-table references: page attachment ranges are within the aux table and each aux record
/// belongs to exactly the page that references it; chunks are within the chunk table.
pub(crate) fn verify_links(pages: &[PageRecord], aux: &[AuxRecord], chunks: usize) -> Result<(), ReaderError> {
    let bad = |message: String| ReaderError::InvalidTableSize { section: "AuxTable", message };
    for (i, page) in pages.iter().enumerate() {
        let range = page.aux_range();
        if range.end > aux.len() {
            return Err(bad(format!("page {i}: attachments {range:?} are beyond the table ({})", aux.len())));
        }
        if let Some(rec) = aux[range].iter().find(|a| a.page_idx.get() as usize != i) {
            return Err(bad(format!("page {i}: the attachment record belongs to page {}", rec.page_idx.get())));
        }
    }
    if let Some(rec) = aux.iter().find(|a| a.chunk_idx.get() as usize >= chunks) {
        return Err(bad(format!("attachment chunk {} is beyond the chunk table ({chunks})", rec.chunk_idx.get())));
    }
    Ok(())
}

pub(crate) fn verify_records<T: RecordCrc>(records: &[T], section: &'static str) -> Result<(), ReaderError> {
    match records.iter().position(|r| !r.is_intact()) {
        Some(index) => Err(ReaderError::RecordCrc { section, index }),
        None => Ok(()),
    }
}

pub(crate) fn verify_crc(data: &[u8], expected: u32, section: &'static str) -> Result<(), ReaderError> {
    let actual = crc32fast::hash(data);
    if actual != expected {
        return Err(ReaderError::CrcMismatch { section, expected, actual });
    }
    Ok(())
}

/// A generic method for reading typed tables with Zerocopy
pub(crate) fn read_table<T: FromBytes + Immutable + KnownLayout + Copy>(
    bytes: &[u8],
    entry: &EntryRecord,
    section: &'static str,
) -> Result<Vec<T>, ReaderError> {
    if entry.is_empty() {
        return Ok(Vec::new());
    }

    let range = checked_range(bytes.len(), entry.offset(), entry.length(), section)?;
    let slice = &bytes[range];
    verify_crc(slice, entry.crc(), section)?;

    // `ref_from_bytes` checks the record size multiple for a slice by itself.
    let records = <[T]>::ref_from_bytes(slice).map_err(|_| ReaderError::InvalidTableSize {
        section,
        message: format!(
            "Size {} is not a multiple of the record size {}",
            slice.len(),
            std::mem::size_of::<T>()
        ),
    })?;

    Ok(records.to_vec())
}
