// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Reading format 1.x files (1.0–1.2) — **read-only**.
//!
//! The 1.x layout is incompatible with 2.0 (unaligned fields, 32-byte page record, a single
//! `aux_chunk_idx` with a magic `0xFFFFFFFF`, a different parity CRC table). The header and tables
//! are parsed using the old structures and converted in memory into 2.0 records, so the rest
//! of the code (reader, unpacking, rebuilding in Creator) handles them the same way. 1.x files
//! cannot be recovered or modified — they should be rebuilt as 2.0.

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned, little_endian as le};

use crate::blocksize::BlockSize;
use crate::formats::AuxFormat;
use crate::header::Header;
use crate::reader::{Parsed, ReaderError, checked_range, read_table, verify_crc, verify_links, verify_records};
use crate::spec;
use crate::vnmf::{AuxKind, AuxRecord, EntryRecord, PageBitFlags, PageRecord, PreviewRecord, RecordCrc};

#[derive(Debug, Clone, Copy, IntoBytes, FromBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C, packed)]
struct HeaderV1 {
    magic: [u8; 4],
    major_version: u8,
    minor_version: u8,
    flags: le::U16,
    uuid: [u8; 16],
    info: EntryRecord,
    preview_table: EntryRecord,
    page_table: EntryRecord,
    chunks_table: EntryRecord,
    parity_offset: le::U64,
    redundancy: u8,
    reserved: [u8; 26],
    block_size_shift: u8,
    crc: le::U32,
}

const _: () = assert!(std::mem::size_of::<HeaderV1>() == spec::HEADER_SIZE);

#[derive(Debug, Clone, Copy, IntoBytes, FromBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C, packed)]
struct PageRecordV1 {
    width: le::U32,
    height: le::U32,
    first_chunk_idx: le::U32,
    chunk_count: le::U16,
    aux_chunk_idx: le::U32,
    chapter_id: le::U16,
    page_number_in_chapter: le::U16,
    format: u8,
    role: u8,
    flags: le::U16,
    record_crc: le::U32,
    volume_id: le::U16,
}

const _: () = assert!(std::mem::size_of::<PageRecordV1>() == 32);

#[derive(Debug, Clone, Copy, IntoBytes, FromBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C, packed)]
struct PreviewRecordV1 {
    offset: le::U64,
    length: le::U32,
    crc: le::U32,
    volume_id: le::U16,
    width: le::U32,
    height: le::U32,
    format: u8,
    flags: u8,
    record_crc: le::U32,
}

const _: () = assert!(std::mem::size_of::<PreviewRecordV1>() == 32);

macro_rules! record_crc {
    ($t:ty) => {
        impl RecordCrc for $t {
            fn stored_crc(&self) -> u32 {
                self.record_crc.get()
            }
            fn set_stored_crc(&mut self, crc: u32) {
                self.record_crc.set(crc);
            }
        }
    };
}
record_crc!(PageRecordV1);
record_crc!(PreviewRecordV1);

/// 1.x page flags.
mod v1 {
    pub const HAS_ALPHA: u16 = 1 << 0;
    pub const IS_CHAPTER_START: u16 = 1 << 1;
    pub const HAS_AUDIO: u16 = 1 << 2;
    pub const HAS_DARK_OVERLAY: u16 = 1 << 3;
    pub const HAS_TEXT_LAYER: u16 = 1 << 4;
    pub const LOSSLESS: u16 = 1 << 5;
    pub const IS_ANIMATED: u16 = 1 << 6;
    pub const READING_RTL: u16 = 1 << 7;
    pub const MIXED_FORMATS: u16 = 1 << 8;
    pub const IS_VOLUME_START: u16 = 1 << 9;
    /// `aux_chunk_idx` = "none".
    pub const NO_AUX: u32 = u32::MAX;
}

fn convert_flags(old: u16) -> u16 {
    [
        (v1::HAS_ALPHA, PageBitFlags::HAS_ALPHA),
        (v1::IS_CHAPTER_START, PageBitFlags::IS_CHAPTER_START),
        (v1::IS_VOLUME_START, PageBitFlags::IS_VOLUME_START),
        (v1::LOSSLESS, PageBitFlags::LOSSLESS),
        (v1::IS_ANIMATED, PageBitFlags::IS_ANIMATED),
        (v1::READING_RTL, PageBitFlags::READING_RTL),
        (v1::MIXED_FORMATS, PageBitFlags::MIXED_FORMATS),
    ]
    .into_iter()
    .filter(|(from, _)| old & from != 0)
    .fold(0, |acc, (_, to)| acc | to)
}

/// Parses 1.x metadata at header position `header_pos` and converts it into 2.0 records.
pub(crate) fn parse(bytes: &[u8], header_pos: usize) -> Result<Parsed, ReaderError> {
    let slice = header_pos
        .checked_add(spec::HEADER_SIZE)
        .and_then(|end| bytes.get(header_pos..end))
        .ok_or(ReaderError::FileTooSmall(bytes.len() as u64))?;
    let old = HeaderV1::read_from_bytes(slice).map_err(|_| ReaderError::FileTooSmall(bytes.len() as u64))?;
    if old.magic != spec::MAGIC {
        return Err(ReaderError::InvalidMagic(old.magic));
    }
    let mut zeroed = old;
    zeroed.crc.set(0);
    let actual = crc32fast::hash(zeroed.as_bytes());
    if actual != old.crc.get() {
        return Err(ReaderError::CrcMismatch { section: "Header", expected: old.crc.get(), actual });
    }
    let block_size = BlockSize::try_from(old.block_size_shift).map_err(ReaderError::InvalidBlockSize)?;

    let info_range = if old.info.is_empty() {
        0..0
    } else {
        let range = checked_range(bytes.len(), old.info.offset(), old.info.length(), "Info")?;
        verify_crc(&bytes[range.clone()], old.info.crc(), "Info")?;
        range
    };
    let old_previews = read_table::<PreviewRecordV1>(bytes, &old.preview_table, "PreviewTable")?;
    let old_pages = read_table::<PageRecordV1>(bytes, &old.page_table, "PageTable")?;
    verify_records(&old_previews, "PreviewTable")?;
    verify_records(&old_pages, "PageTable")?;
    let chunks = read_table::<EntryRecord>(bytes, &old.chunks_table, "ChunksTable")?;

    let previews = old_previews
        .iter()
        .map(|p| {
            let mut rec = PreviewRecord {
                offset: p.offset,
                length: p.length,
                crc: p.crc,
                width: p.width,
                height: p.height,
                volume_id: p.volume_id,
                format: p.format,
                flags: p.flags,
                record_crc: le::U32::ZERO,
            };
            rec.seal();
            rec
        })
        .collect();

    let mut aux = Vec::new();
    let mut pages = Vec::with_capacity(old_pages.len());
    for (i, p) in old_pages.iter().enumerate() {
        let flags = p.flags.get();
        let aux_idx = p.aux_chunk_idx.get();
        let first_aux = aux.len() as u32;
        if aux_idx != v1::NO_AUX && (aux_idx as usize) < chunks.len() {
            let entry = &chunks[aux_idx as usize];
            let format = checked_range(bytes.len(), entry.offset(), entry.length(), "Chunk Data")
                .map_or(AuxFormat::Unknown, |r| AuxFormat::detect(&bytes[r.start..r.end.min(r.start + 4096)]));
            // In 1.x the aux type was set by page flags; without them — by contents.
            let kind = if flags & v1::HAS_AUDIO != 0 {
                AuxKind::Sfx
            } else if flags & v1::HAS_TEXT_LAYER != 0 {
                AuxKind::Text
            } else if flags & v1::HAS_DARK_OVERLAY != 0 {
                AuxKind::DarkImage
            } else {
                AuxKind::guess(format)
            };
            let mut rec = AuxRecord {
                page_idx: le::U32::new(i as u32),
                chunk_idx: le::U32::new(aux_idx),
                kind: kind as u8,
                format: format as u8,
                flags: le::U16::ZERO,
                delay_ms: le::U32::ZERO,
                trigger_y: le::U32::ZERO,
                part: le::U32::ZERO,
                duration_ms: le::U32::ZERO,
                pages: le::U16::ZERO,
                volume: 0,
                reserved0: 0,
                reserved: [0; 12],
                record_crc: le::U32::ZERO,
            };
            rec.seal();
            aux.push(rec);
        }
        let aux_count = aux.len() as u32 - first_aux;
        let mut rec = PageRecord {
            width: p.width,
            height: p.height,
            first_chunk_idx: p.first_chunk_idx,
            chunk_count: le::U32::new(p.chunk_count.get().into()),
            first_aux_idx: le::U32::new(if aux_count > 0 { first_aux } else { 0 }),
            aux_count: le::U32::new(aux_count),
            volume_id: p.volume_id,
            chapter_id: p.chapter_id,
            page_number_in_chapter: p.page_number_in_chapter,
            flags: le::U16::new(convert_flags(flags)),
            format: p.format,
            role: p.role,
            reserved0: [0; 2],
            reserved: [0; 24],
            record_crc: le::U32::ZERO,
        };
        rec.seal();
        pages.push(rec);
    }
    verify_links(&pages, &aux, chunks.len())?;

    let mut header = Header::new(
        old.flags.get(),
        old.uuid,
        old.info,
        [old.preview_table, old.page_table, EntryRecord::default(), old.chunks_table],
        block_size,
    );
    header.major_version = old.major_version;
    header.minor_version = old.minor_version;
    header.parity_offset = old.parity_offset;
    header.redundancy = old.redundancy;
    header.seal();

    Ok(Parsed {
        header,
        block_size,
        info_range,
        previews,
        pages,
        aux,
        chunks,
        legacy: Some((old.major_version, old.minor_version)),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Minimal 1.2 file: two pages, the second with aux audio, no parity (as the old builder wrote it).
    pub(crate) fn v1_file() -> Vec<u8> {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x04\0\0\0\x06\x08\x02\0\0\0".to_vec();
        let ogg = b"OggS\0\x02sound".to_vec();
        let block = 4096usize;
        let page_table = block as u64;
        let chunks_table = 2 * block as u64;
        let data = [png.clone(), [png.clone(), vec![1]].concat(), ogg.clone()];
        let offsets = [3 * block as u64, 4 * block as u64, 5 * block as u64];
        let chunks: Vec<EntryRecord> =
            data.iter().zip(offsets).map(|(d, o)| EntryRecord::new(o, d.len() as u32, crc32fast::hash(d))).collect();
        let page = |first: u32, aux: u32, chapter: u16, flags: u16| {
            let mut r = PageRecordV1 {
                width: le::U32::new(4),
                height: le::U32::new(6),
                first_chunk_idx: le::U32::new(first),
                chunk_count: le::U16::new(1),
                aux_chunk_idx: le::U32::new(aux),
                chapter_id: le::U16::new(chapter),
                page_number_in_chapter: le::U16::new(1),
                format: 2,
                role: 0,
                flags: le::U16::new(flags),
                record_crc: le::U32::ZERO,
                volume_id: le::U16::new(0),
            };
            r.seal();
            r
        };
        let pages = [page(0, v1::NO_AUX, 1, v1::IS_CHAPTER_START), page(1, 2, 2, v1::IS_CHAPTER_START | v1::HAS_AUDIO | v1::READING_RTL)];
        let mut header = HeaderV1 {
            magic: spec::MAGIC,
            major_version: 1,
            minor_version: 2,
            flags: le::U16::new(1 << 2),
            uuid: [7; 16],
            info: EntryRecord::default(),
            preview_table: EntryRecord::default(),
            page_table: EntryRecord::new(page_table, 64, crc32fast::hash(pages.as_bytes())),
            chunks_table: EntryRecord::new(chunks_table, 48, crc32fast::hash(chunks.as_bytes())),
            parity_offset: le::U64::ZERO,
            redundancy: 0,
            reserved: [0; 26],
            block_size_shift: 12,
            crc: le::U32::ZERO,
        };
        header.crc.set(crc32fast::hash(header.as_bytes()));
        let mut file = vec![0u8; 6 * block];
        file[..128].copy_from_slice(header.as_bytes());
        file[block..block + 64].copy_from_slice(pages.as_bytes());
        file[2 * block..2 * block + 48].copy_from_slice(chunks.as_bytes());
        for (d, o) in data.iter().zip(offsets) {
            file[o as usize..o as usize + d.len()].copy_from_slice(d);
        }
        file
    }

    #[test]
    fn reads_v1_file_as_2_0_records() {
        let file = v1_file();
        let parsed = crate::VnmfReader::parse(&file, 0).unwrap();
        assert_eq!(parsed.legacy, Some((1, 2)));
        assert_eq!(parsed.pages.len(), 2);
        assert_eq!(parsed.pages[0].aux_count.get(), 0);
        let p = &parsed.pages[1];
        assert_eq!(p.aux_range(), 0..1);
        assert_eq!(p.flags.get(), PageBitFlags::IS_CHAPTER_START | PageBitFlags::READING_RTL);
        assert!(p.is_intact());
        assert_eq!(parsed.aux[0].kind(), AuxKind::Sfx);
        assert_eq!(AuxFormat::from(parsed.aux[0].format), AuxFormat::Ogg);
        assert_eq!(parsed.aux[0].chunk_idx.get(), 2);
        assert!(crate::recovery::is_legacy(&file));
    }
}
