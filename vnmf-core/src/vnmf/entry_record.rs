// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

use std::fmt;

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned, little_endian as le};

// FIX: `Default` is now derived instead of a hand-written impl that did the same.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, IntoBytes, FromBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C, packed)]
pub struct EntryRecord {
    /// Absolute offset of the start of the data (8 bytes).
    pub offset: le::U64,

    /// Data length in bytes (4 bytes).
    pub length: le::U32,

    /// Checksum (CRC32) of the data for integrity checking (4 bytes).
    pub crc: le::U32,
}

const _: () = assert!(std::mem::size_of::<EntryRecord>() == 16);

impl EntryRecord {
    #[inline]
    pub const fn new(offset: u64, length: u32, crc: u32) -> Self {
        Self {
            offset: le::U64::new(offset),
            length: le::U32::new(length),
            crc: le::U32::new(crc),
        }
    }

    // FIX: the getters were commented out and the code used `.offset.get()` everywhere. They are back as
    // a regular (non-const) API — `get()` in zerocopy is not a const fn.
    #[inline]
    pub fn offset(&self) -> u64 {
        self.offset.get()
    }

    #[inline]
    pub fn length(&self) -> u32 {
        self.length.get()
    }

    #[inline]
    pub fn crc(&self) -> u32 {
        self.crc.get()
    }

    /// The section is absent (encoded in the format as a zero offset or length).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.offset() == 0 || self.length() == 0
    }
}

impl fmt::Display for EntryRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Offset: {:#018x}, Len: {} B, CRC32: {:#010x}",
            self.offset(),
            self.length(),
            self.crc()
        )
    }
}
