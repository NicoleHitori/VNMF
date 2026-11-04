// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Page attachments (aux table, 2.0): music, sound effects, a text layer, a dark version.
//!
//! A page references a contiguous range of records (`PageRecord::first_aux_idx`,
//! `aux_count`); each record is a type, playback parameters and a chunk with data.

use std::fmt;

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned, little_endian as le};

use crate::vnmf::RecordCrc;
use vnmf_i18n::tr;

/// 48 bytes, all fields naturally aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoBytes, FromBytes, Unaligned, Immutable, KnownLayout)]
#[repr(C, packed)]
pub struct AuxRecord {
    /// Page the attachment belongs to (duplicates the reference from the page — for checking and recovery).
    pub page_idx: le::U32,
    /// Data chunk in the chunk table.
    pub chunk_idx: le::U32,
    /// [`AuxKind`].
    pub kind: u8,
    /// [`crate::AuxFormat`] — a hint; the real format is detected by signature.
    pub format: u8,
    /// [`AuxFlags`].
    pub flags: le::U16,
    /// Music / effect: start delay after the page is shown (ms).
    pub delay_ms: le::U32,
    /// Effect in a strip: the page line (px from its top) at which it plays; 0 — from the start of the page.
    pub trigger_y: le::U32,
    /// Dark version: which page image (0..chunk_count) it replaces.
    pub part: le::U32,
    /// Music / effect: how long to play from the start, ms (with looping — the same); 0 — unlimited.
    pub duration_ms: le::U32,
    /// Music: on how many pages, starting with its own; 0 — until the next music or the end of the chapter.
    pub pages: le::U16,
    /// Volume in percent, 1..=100; 0 — 100 %.
    pub volume: u8,
    pub reserved0: u8,
    pub reserved: [u8; 12],
    /// CRC32 of this record with the `record_crc` field zeroed.
    pub record_crc: le::U32,
}

const _: () = assert!(std::mem::size_of::<AuxRecord>() == 48);

impl RecordCrc for AuxRecord {
    fn stored_crc(&self) -> u32 {
        self.record_crc.get()
    }

    fn set_stored_crc(&mut self, crc: u32) {
        self.record_crc.set(crc);
    }
}

impl AuxRecord {
    pub fn kind(&self) -> AuxKind {
        AuxKind::from(self.kind)
    }

    /// Volume 0.0..=1.0.
    pub fn gain(&self) -> f64 {
        match self.volume {
            0 => 1.0,
            v => f64::from(v.min(100)) / 100.0,
        }
    }

    pub fn has_flag(&self, flag: u16) -> bool {
        self.flags.get() & flag != 0
    }
}

/// Attachment type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum AuxKind {
    /// Arbitrary file: the reader doesn't show it, unpacking keeps it.
    Attachment = 0,
    /// Background music: plays from this page until the next music or the end of the chapter
    /// (or for `pages` pages, or `duration_ms`).
    Music = 1,
    /// Sound effect: once when the page is shown (in a strip — when `trigger_y` is reached).
    Sfx = 2,
    /// Page text: UTF-8 or `vnmf-text/1` JSON (regions with text).
    Text = 3,
    /// Dark version of the page image (for a dark theme).
    DarkImage = 4,
}

impl AuxKind {
    pub const ALL: [AuxKind; 5] = [AuxKind::Attachment, AuxKind::Music, AuxKind::Sfx, AuxKind::Text, AuxKind::DarkImage];

    /// Short Latin name (for file names when unpacking: `aux/003.music.ogg`).
    pub fn name(self) -> &'static str {
        match self {
            AuxKind::Attachment => "file",
            AuxKind::Music => "music",
            AuxKind::Sfx => "sfx",
            AuxKind::Text => "text",
            AuxKind::DarkImage => "dark",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name().eq_ignore_ascii_case(name))
    }

    /// Human-readable name.
    pub fn label(self) -> &'static str {
        match self {
            AuxKind::Attachment => tr("File"),
            AuxKind::Music => tr("Music"),
            AuxKind::Sfx => tr("Sound effect"),
            AuxKind::Text => tr("Text"),
            AuxKind::DarkImage => tr("Dark version"),
        }
    }

    pub fn is_audio(self) -> bool {
        matches!(self, AuxKind::Music | AuxKind::Sfx)
    }

    /// Type by format when not set explicitly: sound — effect, text — text, image —
    /// dark version, everything else — file.
    pub fn guess(format: crate::AuxFormat) -> Self {
        if format.is_audio() {
            AuxKind::Sfx
        } else if format.is_text() {
            AuxKind::Text
        } else if format.image().is_some() {
            AuxKind::DarkImage
        } else {
            AuxKind::Attachment
        }
    }

    /// Whether the format suits this type.
    pub fn accepts(self, format: crate::AuxFormat) -> bool {
        match self {
            AuxKind::Attachment => true,
            AuxKind::Music | AuxKind::Sfx => format.is_audio(),
            AuxKind::Text => format.is_text(),
            AuxKind::DarkImage => format.image().is_some(),
        }
    }
}

impl From<u8> for AuxKind {
    /// Unknown code (from a newer version) — treated as `Attachment`: the reader skips it.
    fn from(value: u8) -> Self {
        Self::ALL.get(value as usize).copied().unwrap_or(AuxKind::Attachment)
    }
}

impl fmt::Display for AuxKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Attachment flags.
pub struct AuxFlags;

impl AuxFlags {
    /// Music / effect: loop (an effect — while the page is visible), up to `duration_ms` if set.
    pub const LOOP: u16 = 1 << 0;

    pub fn names(flags: u16) -> Vec<&'static str> {
        [(Self::LOOP, "LOOP")]
            .into_iter()
            .filter(|(bit, _)| flags & bit != 0)
            .map(|(_, n)| n)
            .collect()
    }
}
