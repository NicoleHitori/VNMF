// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

use std::fmt;

use zerocopy::byteorder::little_endian as le;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::blocksize::BlockSize;
use crate::header::bit_flag::HeaderFlagsFormatter;
use crate::spec;
use crate::vnmf::EntryRecord;

/// Main header of a VNMF 2.0 file.
///
/// Total structure size: **128 bytes**; every field sits at a naturally aligned
/// offset (EntryRecord and u64 — multiple of 8, u32 — 4, u16 — 2).
///
/// If `HasTailDup` is set, a copy of the header occupies the last 128 bytes of the file,
/// and its `EntryRecord`s point to the tail copies of the tables.
#[derive(Debug, Clone, Copy, IntoBytes, FromBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C, packed)]
pub struct Header {
    pub magic: [u8; 4],          // "vnmf" (4B)
    pub major_version: u8,       // 1 (1B)
    pub minor_version: u8,       // 0 (1B)
    pub flags: le::U16,          // General file flags (HasInfo, HasTailDup, IsWebToon...) (2B)
    pub uuid: [u8; 16],          // Unique UUID of the volume/manga (16B)

    pub info: EntryRecord,          // 24: Info section (JSON / TOML / text)
    pub preview_table: EntryRecord, // 40: Preview table
    pub page_table: EntryRecord,    // 56: Page table
    pub aux_table: EntryRecord,     // 72: Page attachments table (sound, text, dark versions)
    pub chunks_table: EntryRecord,  // 88: Chunk table

    /// 104: Start of the parity section = end of the protected area (0 — no redundancy).
    /// The section layout is derived from this field, `redundancy` and `block_size_shift`; the same
    /// parameters are repeated in every parity CRC table block (see `parity`).
    pub parity_offset: le::U64,
    /// 112: Reed–Solomon redundancy in percent: 0 — none, 1..=30.
    pub redundancy: u8,
    /// 113: 12 (4 KiB) … 15 (32 KiB).
    pub block_size_shift: u8,

    pub reserved: [u8; 10],      // 114: Reserved (zeros)
    pub crc: le::U32,            // 124: Header CRC32
}

// Compile-time size check
const _: () = assert!(std::mem::size_of::<Header>() == spec::HEADER_SIZE);
const _: () = assert!(std::mem::offset_of!(Header, aux_table) == 72);
const _: () = assert!(std::mem::offset_of!(Header, parity_offset) == 104);
const _: () = assert!(std::mem::offset_of!(Header, crc) == 124);

impl Header {
    pub fn new(
        flags: u16,
        uuid: [u8; 16],
        info: EntryRecord,
        [preview, page_table, aux_table, chunks_table]: [EntryRecord; 4],
        block_size: BlockSize,
    ) -> Self {
        Self {
            magic: spec::MAGIC,
            major_version: spec::DEFAULT_VERSION.0,
            minor_version: spec::DEFAULT_VERSION.1,
            flags: le::U16::new(flags),
            uuid,
            info,
            preview_table: preview,
            page_table,
            aux_table,
            chunks_table,
            parity_offset: le::U64::ZERO,
            redundancy: 0,
            block_size_shift: block_size.shift(),
            reserved: [0u8; 10],
            crc: le::U32::ZERO,
        }
    }

    /// Computes the CRC32 of the whole header.
    ///
    /// During computation the `crc` field is ignored (treated as zero), so
    /// the result is deterministic regardless of the field's previous state.
    pub fn calculate_crc(&self) -> u32 {
        let mut header_copy = *self;
        header_copy.crc.set(0);
        crc32fast::hash(header_copy.as_bytes())
    }

    /// Raw 128 bytes of the header.
    pub fn to_bytes(&self) -> [u8; spec::HEADER_SIZE] {
        let mut out = [0u8; spec::HEADER_SIZE];
        out.copy_from_slice(self.as_bytes());
        out
    }

    /// Writes the current checksum into the `crc` field.
    pub fn seal(&mut self) {
        let crc = self.calculate_crc();
        self.crc.set(crc);
    }
}

impl fmt::Display for Header {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let magic = String::from_utf8_lossy(&self.magic);
        let u = self.uuid;

        write!(
            f,
            "Header v{}.{} [Magic: \"{}\"]\n\
             UUID: {:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}\n\
             Flags: {:#06x} {} | Block Size Code: {}\n\
             Preview: {}\n\
             Info: {}\n\
             Page Table: {}\n\
             Aux Table: {}\n\
             Chunks Table: {}\n\
             Parity: offset {:#x}, redundancy {}%\n\
             CRC32: {:#010x}",
            self.major_version,
            self.minor_version,
            magic,
            u[0], u[1], u[2], u[3], u[4], u[5], u[6], u[7],
            u[8], u[9], u[10], u[11], u[12], u[13], u[14], u[15],
            self.flags.get(),
            HeaderFlagsFormatter(self.flags.get()),
            self.block_size_shift,
            self.preview_table,
            self.info,
            self.page_table,
            self.aux_table,
            self.chunks_table,
            self.parity_offset.get(),
            self.redundancy,
            self.crc.get(),
        )
    }
}
