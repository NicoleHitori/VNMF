// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Block size: the file alignment unit and the parity shard size (2¹²…2¹⁵ bytes).

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BlockSize {
    Page4k = 12,
    Page8k = 13,
    Page16k = 14,
    Ext32k = 15,
}

// FIX: there used to be a `From<u8>` here that silently turned any unknown value into 4 KiB.
// For a damaged or newer file that produced wrong data instead of an error. Now an unknown
// shift is returned as `Err(value)`, and the reader turns it into `ReaderError::InvalidBlockSize`.
impl TryFrom<u8> for BlockSize {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            12 => Ok(BlockSize::Page4k),
            13 => Ok(BlockSize::Page8k),
            14 => Ok(BlockSize::Page16k),
            15 => Ok(BlockSize::Ext32k),
            other => Err(other),
        }
    }
}

impl BlockSize {
    /// Returns the block size in bytes (e.g. 4096 for `Page4k`).
    #[inline]
    pub const fn bytes(self) -> usize {
        1 << (self as usize)
    }

    /// Returns the exponent / bit shift (12, 13, 14, 15).
    #[inline]
    pub const fn shift(self) -> u8 {
        self as u8
    }

    pub fn as_u32(self) -> u32 {
        1 << (self as u8)
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn align_up(&self, pos: u64) -> u64 {
        let mask = (self.as_u32() - 1) as u64;
        (pos + mask) & !mask
    }

    pub fn is_aligned(&self, pos: u64) -> bool {
        pos & (self.as_u32() - 1) as u64 == 0
    }
}

impl fmt::Display for BlockSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Page4k => "4 KiB",
            Self::Page8k => "8 KiB",
            Self::Page16k => "16 KiB",
            Self::Ext32k => "32 KiB",
        };
        f.write_str(s)
    }
}

impl From<BlockSize> for u8 {
    #[inline]
    fn from(size: BlockSize) -> Self {
        size as u8
    }
}
