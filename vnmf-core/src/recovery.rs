// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Recovery of damaged files. Three levels, from the cheapest:
//!
//! 1. **Header copies** ([`inspect_headers`], [`primary_from_tail`], [`rebuild_tail`]):
//!    the primary header and tables are duplicated at the end of the file; one copy can be restored from the other.
//! 2. **Reed–Solomon parity** — see the [`crate::parity`] module (if the file was built with redundancy).
//! 3. **Signature carving** ([`carve`]): when the metadata is lost, data is searched for
//!    by magic bytes. All sections in the format are aligned to at least 4 KiB, so it's enough
//!    to check only aligned positions, and image boundaries are determined by parsing their structure.

use std::fs::OpenOptions;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use zerocopy::{FromBytes, IntoBytes};

use crate::blocksize::BlockSize;
use crate::builder::{FrontLayout, PosWriter, TABLES, write_tail};
use crate::formats::{InfoFormat, PageFormat};
use crate::header::Header;
use crate::header::bit_flag::HeaderBitEnum;
use crate::parity::ParityLayout;
use crate::reader::{HeaderCopy, ReaderError, VnmfReader, parse_header};
use crate::spec;
use crate::vnmf::{AuxRecord, EntryRecord, PageRecord, PreviewRecord, RecordCrc};

// ======================================================================
// 1. HEADER COPIES
// ======================================================================

/// State of both metadata copies.
///
/// The header and tables are checked separately: an intact header is enough to learn
/// the file parameters (block size, parity), but a copy is usable only
/// when the tables and Info it references are intact too.
#[derive(Debug)]
pub struct HeaderReport {
    pub primary: Result<Header, ReaderError>,
    /// Tables and Info of the primary copy (`Err` if damaged or the header itself is broken).
    pub primary_tables: Result<(), ReaderError>,
    /// `None` — the file is too short to have a tail copy.
    pub tail: Option<Result<Header, ReaderError>>,
    /// Tables and Info of the tail copy.
    pub tail_tables: Option<Result<(), ReaderError>>,
}

impl HeaderReport {
    /// The primary copy is fully usable: header + tables + Info.
    pub fn primary_complete(&self) -> bool {
        self.primary.is_ok() && self.primary_tables.is_ok()
    }

    /// The tail copy is fully usable.
    pub fn tail_complete(&self) -> bool {
        self.tail_ok() && matches!(self.tail_tables, Some(Ok(())))
    }

    pub fn primary_ok(&self) -> bool {
        self.primary.is_ok()
    }

    pub fn tail_ok(&self) -> bool {
        matches!(self.tail, Some(Ok(_)))
    }

    /// Whether the copies agree: if both headers are intact, the primary must match the one restored from
    /// the tail ([`primary_from_tail`]) byte for byte. Otherwise one of them was modified while keeping
    /// a valid CRC (e.g. a field rewritten together with the CRC) — such a file is not intact, even though each copy
    /// passes the check on its own. If only one header is intact, there's nothing to compare.
    pub fn headers_agree(&self) -> bool {
        match (&self.primary, &self.tail) {
            (Ok(primary), Some(Ok(tail))) => primary_from_tail(tail).is_ok_and(|p| p.as_bytes() == primary.as_bytes()),
            _ => true,
        }
    }

    /// Names of the fields in which the intact headers differ (empty — they agree or there's nothing to compare).
    pub fn header_differences(&self) -> Vec<&'static str> {
        let (Ok(primary), Some(Ok(tail))) = (&self.primary, &self.tail) else { return Vec::new() };
        let Ok(expected) = primary_from_tail(tail) else { return vec!["block_size_shift"] };
        let (a, b) = (primary.as_bytes(), expected.as_bytes());
        // Field ranges of the 2.0 header (FORMAT.md, section 3); `crc` is not compared — it's derived.
        const FIELDS: [(&str, std::ops::Range<usize>); 13] = [
            ("magic", 0..4),
            ("major_version", 4..5),
            ("minor_version", 5..6),
            ("flags", 6..8),
            ("uuid", 8..24),
            ("info", 24..40),
            ("preview_table", 40..56),
            ("page_table", 56..72),
            ("aux_table", 72..88),
            ("chunks_table", 88..104),
            ("parity_offset", 104..112),
            ("redundancy / block_size_shift", 112..114),
            ("reserved", 114..124),
        ];
        FIELDS.into_iter().filter(|(_, r)| a[r.clone()] != b[r.clone()]).map(|(n, _)| n).collect()
    }

    /// The header to trust for the file parameters (block size, parity, flags).
    /// The tail one has priority: it lies outside the protected area and doesn't depend on its state.
    pub fn best(&self) -> Option<&Header> {
        match (&self.tail, &self.primary) {
            (Some(Ok(tail)), _) => Some(tail),
            (_, Ok(primary)) => Some(primary),
            _ => None,
        }
    }
}

/// Checks both metadata copies. The tail one first (the last 128 bytes of the file).
pub fn inspect_headers(bytes: &[u8]) -> HeaderReport {
    let tail = (bytes.len() >= 2 * spec::HEADER_SIZE).then(|| {
        parse_header(bytes, bytes.len() - spec::HEADER_SIZE).and_then(|(h, _)| {
            if HeaderBitEnum::HasTailDup.is_set(h.flags.get()) {
                Ok(h)
            } else {
                Err(ReaderError::InvalidTableSize { section: "Header", message: "no HasTailDup flag".into() })
            }
        })
    });
    let tail_tables = tail.as_ref().map(|t| match t {
        Ok(_) => VnmfReader::parse(bytes, bytes.len() - spec::HEADER_SIZE).map(drop),
        Err(_) => Err(header_missing()),
    });
    let primary = parse_header(bytes, 0).map(|(h, _)| h);
    let primary_tables = match &primary {
        Ok(_) => VnmfReader::parse(bytes, 0).map(drop),
        Err(_) => Err(header_missing()),
    };
    HeaderReport { primary, primary_tables, tail, tail_tables }
}

/// Whether this is really a 1.x format file, and not a 2.0 file with a damaged version byte.
///
/// A `major_version = 1` byte isn't enough: damage could produce it too (with an accidentally or deliberately
/// recomputed CRC). A file is considered 1.x only when **neither** copy is an intact 2.0 header,
/// and at least one is marked as 1.x. An intact 2.0 copy means a 2.0 file whose other copy is damaged.
pub fn is_legacy(bytes: &[u8]) -> bool {
    let tail_pos = (bytes.len() >= 2 * spec::HEADER_SIZE).then(|| bytes.len() - spec::HEADER_SIZE);
    let positions = [Some(0), tail_pos];
    let marked = |pos: usize| bytes.get(pos..pos + 4) == Some(&spec::MAGIC) && bytes.get(pos + 4) == Some(&spec::LEGACY_MAJOR);
    let any_v2 = positions.iter().flatten().any(|&pos| parse_header(bytes, pos).is_ok());
    !any_v2 && positions.iter().flatten().any(|&pos| marked(pos))
}

fn header_missing() -> ReaderError {
    ReaderError::InvalidTableSize { section: "Header", message: "header is damaged".into() }
}

/// Restores the primary metadata copy (header, Info and tables) from an intact tail copy.
///
/// For files without redundancy this is the only way to fix a damaged start of the file:
/// the primary copy's sections have the same lengths and CRCs, and their offsets are derived from the layout.
/// Returns `true` if anything was changed.
pub fn restore_front_from_tail(bytes: &mut [u8]) -> Result<bool, ReaderError> {
    let tail_pos = bytes.len().checked_sub(spec::HEADER_SIZE).ok_or(ReaderError::FileTooSmall(bytes.len() as u64))?;
    let tail = VnmfReader::parse(bytes, tail_pos)?.header;
    let primary = primary_from_tail(&tail)?;

    let mut changed = false;
    let sections = [
        (tail.info, primary.info),
        (tail.preview_table, primary.preview_table),
        (tail.page_table, primary.page_table),
        (tail.aux_table, primary.aux_table),
        (tail.chunks_table, primary.chunks_table),
    ];
    for (from, to) in sections {
        if from.is_empty() {
            continue;
        }
        let len = from.length() as usize;
        let (src, dst) = (from.offset() as usize, to.offset() as usize);
        if dst + len > bytes.len() || src + len > bytes.len() {
            return Err(ReaderError::InvalidTableSize { section: "Front", message: "section is beyond the end of the file".into() });
        }
        if bytes[src..src + len] != bytes[dst..dst + len] {
            bytes.copy_within(src..src + len, dst);
            changed = true;
        }
    }
    let header = primary.to_bytes();
    if bytes[..spec::HEADER_SIZE] != header {
        bytes[..spec::HEADER_SIZE].copy_from_slice(&header);
        changed = true;
    }
    Ok(changed)
}

/// Restores the primary header from the tail one.
///
/// Section lengths and CRCs are the same in both copies, and the offsets of the primary sections are
/// deterministically derived from the lengths (the same layout as in the builder). The result is a sealed header.
pub fn primary_from_tail(tail: &Header) -> Result<Header, ReaderError> {
    let block = BlockSize::try_from(tail.block_size_shift).map_err(ReaderError::InvalidBlockSize)?;
    let front = FrontLayout::of(tail, block);
    let mut primary = *tail;
    primary.info.offset.set(front.info);
    for (field, offset) in TABLES.iter().zip(front.tables) {
        field(&mut primary).offset.set(offset);
    }
    primary.seal();
    Ok(primary)
}

/// End of the data, after which the tail copy starts (without it — the end of the file).
fn tail_start(reader: &VnmfReader) -> u64 {
    if let Some(layout) = reader.parity_layout() {
        return layout.end();
    }
    let h = reader.header();
    let sections = [h.info, h.preview_table, h.page_table, h.aux_table, h.chunks_table];
    let data_end = sections
        .iter()
        .map(|e| e.offset() + u64::from(e.length()))
        .chain(reader.chunks().iter().map(|c| c.offset() + u64::from(c.length())))
        .chain(reader.previews().iter().map(|p| p.offset.get() + u64::from(p.length.get())))
        .max()
        .unwrap_or(spec::HEADER_SIZE as u64);
    reader.block_size().align_up(data_end)
}

/// Rewrites the tail copy from the primary metadata (the primary header and tables must be intact).
/// The file is truncated/extended to the required length.
pub fn rebuild_tail(path: &Path) -> Result<(), ReaderError> {
    let reader = VnmfReader::open_with(path, HeaderCopy::Primary)?;
    if let Some((major, minor)) = reader.legacy_version() {
        return Err(ReaderError::UnsupportedVersion(major, minor));
    }
    if !HeaderBitEnum::HasTailDup.is_set(reader.header().flags.get()) {
        return Err(ReaderError::InvalidTableSize { section: "Header", message: "the file has no tail copy".into() });
    }
    let start = tail_start(&reader);
    let header = *reader.header();
    let info = reader.info_bytes().unwrap_or(&[]).to_vec();
    let previews = reader.previews().as_bytes().to_vec();
    let pages = reader.pages().as_bytes().to_vec();
    let aux = reader.aux().as_bytes().to_vec();
    let chunks = reader.chunks().as_bytes().to_vec();
    drop(reader);

    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.set_len(start)?;
    file.seek(SeekFrom::Start(start))?;
    let mut writer = PosWriter::at(BufWriter::new(file), start);
    write_tail(&mut writer, &header, &info, [&previews, &pages, &aux, &chunks])?;
    writer.flush()?;
    writer.inner.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(())
}

/// Summary of assembling the metadata record by record ([`salvage_metadata`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SalvageReport {
    /// Sections that survived entirely in at least one copy.
    pub whole_sections: usize,
    /// Records taken from the tail copy because they are damaged in the primary.
    pub merged_records: usize,
    /// Records (and Info, if both copies are broken) that couldn't be confirmed in either copy.
    pub bad_records: usize,
    /// After assembly the CRCs of all sections match the header — the metadata has been restored exactly.
    pub exact: bool,
}

/// Assembles the primary metadata copy (header, Info, tables) from the primary and tail copies
/// **record by record** — when neither copy is fully intact, but different places are damaged.
///
/// - a section that survived in either copy is taken whole;
/// - page and preview records are checked by their own CRC (`RecordCrc`);
/// - chunk records have no CRC of their own — they are confirmed by the CRC of the data they point to.
///
/// The result is written into the primary copy in `bytes`; the tail copy should be rebuilt afterwards
/// ([`rebuild_tail`]).
pub fn salvage_metadata(bytes: &mut [u8]) -> Result<SalvageReport, ReaderError> {
    let primary = parse_header(bytes, 0).ok().map(|(h, _)| h);
    let tail_pos = bytes.len().checked_sub(spec::HEADER_SIZE).filter(|&p| p >= spec::HEADER_SIZE);
    let tail = tail_pos.and_then(|p| parse_header(bytes, p).ok()).map(|(h, _)| h).filter(|h| HeaderBitEnum::HasTailDup.is_set(h.flags.get()));

    let target = match (primary, &tail) {
        (Some(p), _) => p,
        (None, Some(t)) => primary_from_tail(t)?,
        (None, None) => return Err(header_missing()),
    };

    let copy_of = |entry: &EntryRecord, len: usize| -> Option<Vec<u8>> {
        let start = usize::try_from(entry.offset()).ok()?;
        bytes.get(start..start.checked_add(len)?).map(<[u8]>::to_vec)
    };
    // Section candidates: the primary copy, then the tail one.
    let candidates = |pick: fn(&Header) -> EntryRecord| -> Vec<Vec<u8>> {
        let len = pick(&target).length() as usize;
        [Some(pick(&target)), tail.as_ref().map(pick)].into_iter().flatten().filter_map(|e| copy_of(&e, len)).collect()
    };

    let chunk_data_ok = |rec: &[u8]| -> bool {
        EntryRecord::read_from_bytes(rec).is_ok_and(|e| {
            let start = e.offset() as usize;
            start.checked_add(e.length() as usize).and_then(|end| bytes.get(start..end)).is_some_and(|data| crc32fast::hash(data) == e.crc())
        })
    };

    let mut report = SalvageReport::default();
    let mut restored: Vec<(EntryRecord, Vec<u8>)> = Vec::new();

    type Pick = fn(&Header) -> EntryRecord;
    let sections: [(Pick, Option<usize>); 5] = [
        (|h| h.info, None),
        (|h| h.preview_table, Some(size_of::<PreviewRecord>())),
        (|h| h.page_table, Some(size_of::<PageRecord>())),
        (|h| h.aux_table, Some(size_of::<AuxRecord>())),
        (|h| h.chunks_table, Some(size_of::<EntryRecord>())),
    ];

    for (idx, (pick, record_size)) in sections.into_iter().enumerate() {
        let entry = pick(&target);
        if entry.is_empty() {
            continue;
        }
        let cands = candidates(pick);
        if let Some(whole) = cands.iter().find(|c| crc32fast::hash(c) == entry.crc()) {
            report.whole_sections += 1;
            restored.push((entry, whole.clone()));
            continue;
        }
        let Some(first) = cands.first() else {
            report.bad_records += 1;
            continue;
        };
        let Some(size) = record_size else {
            // Info has no internal structure — take the primary copy as is.
            report.bad_records += 1;
            restored.push((entry, first.clone()));
            continue;
        };

        let valid = |rec: &[u8]| -> bool {
            match idx {
                1 => PreviewRecord::read_from_bytes(rec).is_ok_and(|r| r.is_intact()),
                2 => PageRecord::read_from_bytes(rec).is_ok_and(|r| r.is_intact()),
                3 => AuxRecord::read_from_bytes(rec).is_ok_and(|r| r.is_intact()),
                _ => chunk_data_ok(rec),
            }
        };
        let mut merged = first.clone();
        for (i, slot) in merged.chunks_mut(size).enumerate() {
            if valid(slot) {
                continue;
            }
            let other = cands[1..].iter().filter_map(|c| c.get(i * size..(i + 1) * size)).find(|rec| valid(rec));
            match other {
                Some(rec) => {
                    slot.copy_from_slice(rec);
                    report.merged_records += 1;
                }
                // Both copies are identical — most likely the record is intact, and the check failed
                // because of damaged chunk data (which doesn't affect it).
                None if cands.iter().all(|c| c.get(i * size..(i + 1) * size) == Some(&*slot)) => {}
                None => report.bad_records += 1,
            }
        }
        restored.push((entry, merged));
    }

    for (entry, data) in &restored {
        let start = entry.offset() as usize;
        bytes[start..start + data.len()].copy_from_slice(data);
    }
    bytes[..spec::HEADER_SIZE].copy_from_slice(&target.to_bytes());

    report.exact = restored.iter().all(|(e, d)| crc32fast::hash(d) == e.crc())
        && restored.len()
            == [target.info, target.preview_table, target.page_table, target.aux_table, target.chunks_table].iter().filter(|e| !e.is_empty()).count();
    Ok(report)
}

/// Writes the primary header into the first 128 bytes of the file.
pub fn write_primary_header(path: &Path, header: &Header) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.write_all(header.as_bytes())?;
    file.sync_all()
}

/// Parity layout from the header (if the file has redundancy).
pub fn parity_layout_of(header: &Header) -> Option<ParityLayout> {
    if !HeaderBitEnum::HasParity.is_set(header.flags.get()) {
        return None;
    }
    let block = BlockSize::try_from(header.block_size_shift).ok()?;
    ParityLayout::new(header.parity_offset.get(), block, header.redundancy)
}

// ======================================================================
// 3. SIGNATURE CARVING
// ======================================================================

/// Minimum section alignment in the format (BlockSize::Page4k).
const SCAN_STEP: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarvedKind {
    Image(PageFormat),
    /// Text metadata (Info: JSON / TOML / XML / text).
    Text(InfoFormat),
}

/// A found data fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carved {
    pub offset: u64,
    pub len: usize,
    pub kind: CarvedKind,
    /// `true` — boundaries determined by parsing the structure (for PNG — also all CRCs matched);
    /// `false` — the structure is damaged, boundaries estimated up to the next signature / zero block.
    pub complete: bool,
}

impl Carved {
    pub fn data<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        &bytes[self.offset as usize..self.offset as usize + self.len]
    }
}

/// Searches the whole file for images and text metadata by signatures.
pub fn carve(bytes: &[u8]) -> Vec<Carved> {
    let mut found = Vec::new();
    let mut seen_texts: Vec<u32> = Vec::new();

    let mut push_text = |found: &mut Vec<Carved>, offset: usize, len: usize, format: InfoFormat| {
        let crc = crc32fast::hash(&bytes[offset..offset + len]);
        if !seen_texts.contains(&crc) {
            seen_texts.push(crc);
            found.push(Carved { offset: offset as u64, len, kind: CarvedKind::Text(format), complete: true });
        }
    };

    // Info in the primary copy comes right after the 128-byte header (without alignment).
    if let Some((len, format)) = bytes.get(spec::HEADER_SIZE..).and_then(text_extent) {
        push_text(&mut found, spec::HEADER_SIZE, len, format);
    }

    let mut pos = SCAN_STEP;
    while pos < bytes.len() {
        let rest = &bytes[pos..];
        let format = PageFormat::detect(rest);

        if format.is_known() {
            let (len, complete) = match image_extent(rest, format) {
                Some((len, verified)) => (len, verified),
                None => (fallback_extent(rest), false),
            };
            found.push(Carved { offset: pos as u64, len, kind: CarvedKind::Image(format), complete });
            pos = align_up(pos + len.max(1), SCAN_STEP);
            continue;
        }
        if let Some((len, format)) = text_extent(rest) {
            push_text(&mut found, pos, len, format);
            pos = align_up(pos + len, SCAN_STEP);
            continue;
        }
        pos += SCAN_STEP;
    }
    found
}

fn align_up(v: usize, a: usize) -> usize {
    v.div_ceil(a) * a
}

/// Boundaries of a damaged image: up to the next aligned signature or a fully zero block,
/// without trailing zeros.
fn fallback_extent(rest: &[u8]) -> usize {
    let mut end = rest.len();
    let mut p = SCAN_STEP;
    while p < rest.len() {
        let block = &rest[p..(p + SCAN_STEP).min(rest.len())];
        if PageFormat::detect(block).is_known() || block.iter().all(|&b| b == 0) || text_extent(block).is_some() {
            end = p;
            break;
        }
        p += SCAN_STEP;
    }
    let trimmed = rest[..end].iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    trimmed.max(1)
}

/// A text fragment at the start of `rest`: up to the first NUL, invalid UTF-8 or a control character.
fn text_extent(rest: &[u8]) -> Option<(usize, InfoFormat)> {
    let first = *rest.first()?;
    if !(first.is_ascii_graphic() || first.is_ascii_whitespace() || first == 0xEF) {
        return None;
    }
    let limit = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    let candidate = &rest[..limit];
    let valid = match std::str::from_utf8(candidate) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&candidate[..e.valid_up_to()]).ok()?,
    };
    let end = valid.char_indices().find(|&(_, c)| c.is_control() && !matches!(c, '\n' | '\r' | '\t')).map_or(valid.len(), |(i, _)| i);
    let text = valid[..end].trim_end();
    if text.trim().len() < 16 {
        return None;
    }
    match InfoFormat::detect(text.as_bytes()) {
        InfoFormat::Binary => None,
        format => Some((text.len(), format)),
    }
}

/// Exact image boundaries from its internal structure.
/// Returns `(length, verified)`; `None` — the structure is damaged or truncated.
pub fn image_extent(data: &[u8], format: PageFormat) -> Option<(usize, bool)> {
    match format {
        PageFormat::Png => png_extent(data),
        PageFormat::Jpeg => jpeg_extent(data).map(|n| (n, true)),
        PageFormat::Gif => gif_extent(data).map(|n| (n, true)),
        PageFormat::WebP => webp_extent(data).map(|n| (n, true)),
        PageFormat::Avif => bmff_extent(data).map(|n| (n, true)),
        // The JPEG XL container is ISO-BMFF too. The length of a "bare" codestream can't be determined without
        // full decoding — then boundaries are estimated up to the next signature / zero block.
        PageFormat::JpegXl if data.starts_with(&crate::formats::JXL_CONTAINER) => bmff_extent(data).map(|n| (n, true)),
        PageFormat::JpegXl => None,
        PageFormat::Unknown => None,
    }
}

fn be_u32(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

/// PNG: a chain of `[len][type][data][crc]` chunks up to IEND; the CRC of each chunk is verified.
fn png_extent(d: &[u8]) -> Option<(usize, bool)> {
    let mut i = 8;
    let mut crc_ok = true;
    loop {
        let len = be_u32(d, i)? as usize;
        let body = d.get(i + 4..(i + 8).checked_add(len)?)?;
        let crc = be_u32(d, i + 8 + len)?;
        crc_ok &= crc32fast::hash(body) == crc;
        let ty = &body[..4];
        i += 12 + len;
        if ty == b"IEND" {
            return Some((i, crc_ok));
        }
        if !ty.iter().all(u8::is_ascii_alphabetic) {
            return None;
        }
    }
}

/// JPEG: segments with lengths, after SOS — entropy-coded data up to the next marker, the end is EOI.
fn jpeg_extent(d: &[u8]) -> Option<usize> {
    if !d.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    loop {
        if *d.get(i)? != 0xFF {
            return None;
        }
        while *d.get(i)? == 0xFF {
            i += 1;
        }
        let marker = d[i];
        i += 1;
        match marker {
            0xD9 => return Some(i),
            0xD0..=0xD7 | 0x01 => continue,
            _ => {
                let len = u16::from_be_bytes([*d.get(i)?, *d.get(i + 1)?]) as usize;
                if len < 2 {
                    return None;
                }
                i += len;
                if marker == 0xDA {
                    // Entropy-coded data: 0xFF00 and RSTn are part of the stream, any other 0xFFxx is a marker.
                    loop {
                        if *d.get(i)? == 0xFF {
                            let next = *d.get(i + 1)?;
                            if next == 0x00 || (0xD0..=0xD7).contains(&next) {
                                i += 2;
                                continue;
                            }
                            break;
                        }
                        i += 1;
                    }
                }
            }
        }
    }
}

/// GIF: header, palettes, image/extension blocks with sub-blocks, trailer 0x3B.
fn gif_extent(d: &[u8]) -> Option<usize> {
    fn sub_blocks(d: &[u8], mut i: usize) -> Option<usize> {
        loop {
            let n = *d.get(i)? as usize;
            i += 1;
            if n == 0 {
                return Some(i);
            }
            i += n;
        }
    }

    let flags = *d.get(10)?;
    let mut i = 13;
    if flags & 0x80 != 0 {
        i += 3 << ((flags & 7) + 1);
    }
    loop {
        match *d.get(i)? {
            0x3B => return Some(i + 1),
            0x21 => i = sub_blocks(d, i + 2)?,
            0x2C => {
                let local = *d.get(i + 9)?;
                i += 10;
                if local & 0x80 != 0 {
                    i += 3 << ((local & 7) + 1);
                }
                i = sub_blocks(d, i + 1)?; // +1 — LZW minimum code size
            }
            _ => return None,
        }
    }
}

/// WebP: a RIFF container, the length in the header (+ padding to even).
fn webp_extent(d: &[u8]) -> Option<usize> {
    let size = u32::from_le_bytes(d.get(4..8)?.try_into().ok()?) as usize;
    let total = 8 + size + (size & 1);
    (total <= d.len()).then_some(total)
}

/// AVIF (ISO-BMFF): a sequence of top-level boxes (ftyp, meta, mdat…) up to the first
/// "non-box" — in the format the image is followed by zero padding.
fn bmff_extent(d: &[u8]) -> Option<usize> {
    let mut i = 0;
    let mut boxes = 0;
    while i + 8 <= d.len() {
        let ty = &d[i + 4..i + 8];
        if !ty.iter().all(|c| c.is_ascii_alphanumeric() || *c == b' ') {
            break;
        }
        let size = match be_u32(d, i)? {
            0 => return None, // "to the end of file" — boundaries can't be determined
            1 => usize::try_from(u64::from_be_bytes(d.get(i + 8..i + 16)?.try_into().ok()?)).ok()?,
            n => n as usize,
        };
        if size < 8 || i + size > d.len() {
            return None;
        }
        i += size;
        boxes += 1;
    }
    (boxes >= 2).then_some(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(body: &[u8]) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        for (ty, data) in [(&b"IHDR"[..], &[0u8; 13][..]), (b"IDAT", body), (b"IEND", &[])] {
            v.extend((data.len() as u32).to_be_bytes());
            let mut chunk = ty.to_vec();
            chunk.extend_from_slice(data);
            v.extend_from_slice(&chunk);
            v.extend(crc32fast::hash(&chunk).to_be_bytes());
        }
        v
    }

    #[test]
    fn extents() {
        let p = png(b"hello");
        assert_eq!(png_extent(&[p.clone(), vec![0; 50]].concat()), Some((p.len(), true)));
        let mut broken = p.clone();
        broken[45] ^= 1;
        assert_eq!(png_extent(&broken), Some((p.len(), false)));

        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 1, 2, 0xFF, 0xDA, 0, 2, 9, 0xFF, 0x00, 8, 0xFF, 0xD0, 7, 0xFF, 0xD9];
        assert_eq!(jpeg_extent(&[&jpeg[..], &[0; 10]].concat()), Some(jpeg.len()));

        let webp = [b"RIFF".as_slice(), &5u32.to_le_bytes(), b"WEBPX"].concat();
        assert_eq!(webp_extent(&[webp.clone(), vec![0; 9]].concat()), Some(14));

        let mut avif = [0, 0, 0, 16].to_vec();
        avif.extend_from_slice(b"ftypavif\0\0\0\0");
        avif.extend_from_slice(&[0, 0, 0, 12]);
        avif.extend_from_slice(b"mdat1234");
        assert_eq!(bmff_extent(&[avif.clone(), vec![0; 100]].concat()), Some(28));

        let gif = [b"GIF89a".as_slice(), &[1, 0, 1, 0, 0, 0, 0], &[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 1, 2, 0], &[0x3B]].concat();
        assert_eq!(gif_extent(&gif), Some(gif.len()));
    }

    #[test]
    fn record_crc_detects_single_bit() {
        let mut page = PageRecord::read_from_bytes(&[7u8; 64]).unwrap();
        page.seal();
        assert!(page.is_intact());
        page.width.set(page.width.get() ^ 1);
        assert!(!page.is_intact());
    }

    #[test]
    fn carves_aligned_images_and_text() {
        let mut file = vec![0u8; 4096 * 6];
        file[..4].copy_from_slice(b"junk");
        let info = br#"{"ComicInfo": {"Title": "Test title"}}"#;
        file[128..128 + info.len()].copy_from_slice(info);
        let p = png(&[7u8; 5000]);
        file[4096..4096 + p.len()].copy_from_slice(&p); // takes 2 blocks
        let p2 = png(b"second");
        file[4096 * 4..4096 * 4 + p2.len()].copy_from_slice(&p2);

        let found = carve(&file);
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(found[0].kind, CarvedKind::Text(InfoFormat::Json));
        assert_eq!(found[0].data(&file), info);
        assert_eq!(found[1].data(&file), p);
        assert!(found[1].complete);
        assert_eq!(found[2].offset, 4096 * 4);
    }
}
