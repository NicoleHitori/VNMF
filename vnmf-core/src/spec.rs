// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

use crate::blocksize::BlockSize;

pub const MAGIC: [u8; 4] = *b"vnmf";
// FIX: was `u16`, but was used everywhere as `usize`/`u64` via `as`. usize removes those casts.
pub const HEADER_SIZE: usize = 128;
/// 2.0 — layout incompatible with 1.x: all fields naturally aligned, `PageRecord` is 64 bytes,
/// an aux (attachments) table instead of a single `aux_chunk_idx` with a magic `0xFFFFFFFF`,
/// self-describing parity CRC table blocks. 1.x files are readable (read-only, see `legacy`).
pub const DEFAULT_VERSION: (u8, u8) = (2, 0);
/// Major version of the previous format that the reader can still read.
pub const LEGACY_MAJOR: u8 = 1;
pub const DEFAULT_BLOCK_SIZE: BlockSize = BlockSize::Page4k;

/// Signature of the parity CRC table blocks (`VNMFPAR1`) and maximum redundancy, % —
/// defined in `vnmf-parity`.
pub use vnmf_parity::{MAX_REDUNDANCY, PARITY_MAGIC};

pub const NAMESPACE: &str = "2c5ec3fc-9919-4e67-82f4-4a9555390abb";
