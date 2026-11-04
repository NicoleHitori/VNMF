// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned, little_endian as le};

use crate::vnmf::RecordCrc;

/// Page table record (2.0): **64 bytes**, every field at a naturally aligned offset
/// (u32 — multiple of 4, u16 — multiple of 2), so a C reader can overlay the struct directly onto an mmap
/// without unaligned accesses. 24 reserved bytes — for future fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoBytes, FromBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C, packed)]
pub struct PageRecord {
    // 0: geometry
    pub width: le::U32,
    pub height: le::U32,

    // 8: image — chunks first_chunk_idx .. first_chunk_idx + chunk_count
    pub first_chunk_idx: le::U32,
    pub chunk_count: le::U32,

    // 16: attachments — aux table records first_aux_idx .. first_aux_idx + aux_count.
    //     Presence is determined only by `aux_count` (no magic values); at 0 the writer stores 0.
    pub first_aux_idx: le::U32,
    pub aux_count: le::U32,

    // 24: structure
    pub volume_id: le::U16,
    pub chapter_id: le::U16,
    pub page_number_in_chapter: le::U16,
    pub flags: le::U16,

    // 32: types
    pub format: u8,
    pub role: u8,
    pub reserved0: [u8; 2],

    // 36: reserved (zeros)
    pub reserved: [u8; 24],

    // 60: CRC32 of this record with the `record_crc` field zeroed
    pub record_crc: le::U32,
}

const _: () = assert!(std::mem::size_of::<PageRecord>() == 64);

impl RecordCrc for PageRecord {
    fn stored_crc(&self) -> u32 {
        self.record_crc.get()
    }

    fn set_stored_crc(&mut self, crc: u32) {
        self.record_crc.set(crc);
    }
}

impl PageRecord {
    /// Indices of this page's aux table records.
    #[inline]
    pub fn aux_range(&self) -> std::ops::Range<usize> {
        let first = self.first_aux_idx.get() as usize;
        first..first.saturating_add(self.aux_count.get() as usize)
    }

    /// Indices of the page image's chunks.
    #[inline]
    pub fn chunk_range(&self) -> std::ops::Range<usize> {
        let first = self.first_chunk_idx.get() as usize;
        first..first.saturating_add(self.chunk_count.get() as usize)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageRole {
    Normal = 0,             // Regular page
    FrontCover = 1,         // Front cover
    BackCover = 2,          // Back cover
    TitlePage = 3,          // Title page
    SpreadLeft = 4,         // Left half of a spread
    SpreadRight = 5,        // Right half of a spread
    SpreadSingle = 6,       // A single continuous wide spread page
    Credits = 7,            // Authors/translators page
    TableOfContents = 8,    // Table of contents
    OmakeBonus = 9,         // Bonus / extra
    Advertisement = 10,     // Advertisement
}

impl PageRole {
    pub const ALL: [PageRole; 11] = [
        PageRole::Normal,
        PageRole::FrontCover,
        PageRole::BackCover,
        PageRole::TitlePage,
        PageRole::SpreadLeft,
        PageRole::SpreadRight,
        PageRole::SpreadSingle,
        PageRole::Credits,
        PageRole::TableOfContents,
        PageRole::OmakeBonus,
        PageRole::Advertisement,
    ];

    pub fn name(self) -> &'static str {
        match self {
            PageRole::Normal => "Normal",
            PageRole::FrontCover => "FrontCover",
            PageRole::BackCover => "BackCover",
            PageRole::TitlePage => "TitlePage",
            PageRole::SpreadLeft => "SpreadLeft",
            PageRole::SpreadRight => "SpreadRight",
            PageRole::SpreadSingle => "SpreadSingle",
            PageRole::Credits => "Credits",
            PageRole::TableOfContents => "TableOfContents",
            PageRole::OmakeBonus => "OmakeBonus",
            PageRole::Advertisement => "Advertisement",
        }
    }
}

impl TryFrom<u8> for PageRole {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        PageRole::ALL.get(value as usize).copied().ok_or(value)
    }
}

impl std::fmt::Display for PageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Page flags (2.0). Only properties of the image itself and the structure: which attachments the page has
/// (sound, text, dark version) is visible from its aux table records, not from flags —
/// so these two things can't contradict each other.
pub struct PageBitFlags;

impl PageBitFlags {
    const NAMES: [(u16, &'static str); 7] = [
        (Self::HAS_ALPHA, "ALPHA"),
        (Self::IS_CHAPTER_START, "CHAPTER_START"),
        (Self::IS_VOLUME_START, "VOLUME_START"),
        (Self::LOSSLESS, "LOSSLESS"),
        (Self::IS_ANIMATED, "ANIMATED"),
        (Self::READING_RTL, "RTL"),
        (Self::MIXED_FORMATS, "MIXED_FORMATS"),
    ];

    /// Names of the set bits (unknown bits are ignored).
    pub fn names(flags: u16) -> Vec<&'static str> {
        Self::NAMES.iter().filter(|(bit, _)| flags & bit != 0).map(|(_, n)| *n).collect()
    }

    pub const HAS_ALPHA: u16        = 1 << 0; // Has an alpha channel
    pub const IS_CHAPTER_START: u16 = 1 << 1; // Start of a new chapter
    pub const IS_VOLUME_START: u16  = 1 << 2; // Start of a new volume
    pub const LOSSLESS: u16         = 1 << 3; // Lossless image
    pub const IS_ANIMATED: u16      = 1 << 4; // Animated page (APNG / Animated WebP / GIF)
    pub const READING_RTL: u16      = 1 << 5; // Right-to-left reading direction
    pub const MIXED_FORMATS: u16    = 1 << 6; // The page's chunks have different formats — detect each by signature
}
