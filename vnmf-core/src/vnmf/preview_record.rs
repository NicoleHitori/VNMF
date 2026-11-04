// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned, little_endian as le};

use crate::vnmf::RecordCrc;

/// Preview table record (2.0): 32 bytes, all fields naturally aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoBytes, FromBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C, packed)]
pub struct PreviewRecord {
    /// Absolute offset of the start of the data.
    pub offset: le::U64,
    /// Data length in bytes.
    pub length: le::U32,
    /// CRC32 of the image data (not to be confused with `record_crc`).
    pub crc: le::U32,
    /// Width in px (0 — unknown).
    pub width: le::U32,
    /// Height in px (0 — unknown).
    pub height: le::U32,
    /// 0 — main cover of the file, N — cover of volume N.
    pub volume_id: le::U16,
    /// Preview format.
    pub format: u8,
    /// Flags (reserved, 0).
    pub flags: u8,
    /// CRC32 of this record with the `record_crc` field zeroed.
    pub record_crc: le::U32,
}

const _: () = assert!(std::mem::size_of::<PreviewRecord>() == 32);

impl RecordCrc for PreviewRecord {
    fn stored_crc(&self) -> u32 {
        self.record_crc.get()
    }

    fn set_stored_crc(&mut self, crc: u32) {
        self.record_crc.set(crc);
    }
}
