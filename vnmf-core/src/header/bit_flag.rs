// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HeaderBitEnum {
    HasInfo = 0,
    HasPreview = 1,
    HasAlign = 2,
    HasEncrypted = 3,
    /// Dark versions of pages exist (`DarkImage` attachments). Like the next two, only a summary of the
    /// aux table: the writer sets it itself; a reader may not trust it and look into the table.
    HasDarkMode = 4,
    /// A text layer exists (`Text` attachments).
    HasText = 5,
    /// Sound exists: music or effects (`Music` / `Sfx` attachments).
    HasAudio = 6,
    /// FIX: new flag. The backup copy of tables and header at the end of the file used to be
    /// unmarked, so the reader couldn't know whether to look for it.
    HasTailDup = 7,
    /// The file has a Reed–Solomon parity section (see `Header::redundancy`).
    HasParity = 8,
    IsWebToon = 15, // last bit
}

impl HeaderBitEnum {
    pub const ALL: [HeaderBitEnum; 10] = [
        HeaderBitEnum::HasInfo,
        HeaderBitEnum::HasPreview,
        HeaderBitEnum::HasAlign,
        HeaderBitEnum::HasEncrypted,
        HeaderBitEnum::HasDarkMode,
        HeaderBitEnum::HasText,
        HeaderBitEnum::HasAudio,
        HeaderBitEnum::HasTailDup,
        HeaderBitEnum::HasParity,
        HeaderBitEnum::IsWebToon,
    ];

    pub const fn mask(self) -> u16 {
        1 << (self as u8)
    }

    /// Checks whether the corresponding bit is set in the `flags` mask.
    #[inline]
    pub const fn is_set(self, flags: u16) -> bool {
        (flags & self.mask()) != 0
    }
}

impl fmt::Display for HeaderBitEnum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::HasInfo => "HasInfo",
            Self::HasPreview => "HasPreview",
            Self::HasAlign => "HasAlign",
            Self::HasEncrypted => "HasEncrypted",
            Self::HasDarkMode => "HasDarkMode",
            Self::HasText => "HasText",
            Self::HasAudio => "HasAudio",
            Self::HasTailDup => "HasTailDup",
            Self::HasParity => "HasParity",
            Self::IsWebToon => "IsWebToon",
        };
        f.write_str(s)
    }
}

pub struct HeaderFlagsFormatter(pub u16);

impl fmt::Display for HeaderFlagsFormatter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        f.write_str("[")?;
        for bit in HeaderBitEnum::ALL {
            if bit.is_set(self.0) {
                if !first {
                    f.write_str(", ")?;
                }
                write!(f, "{bit}")?;
                first = false;
            }
        }
        f.write_str("]")
    }
}
