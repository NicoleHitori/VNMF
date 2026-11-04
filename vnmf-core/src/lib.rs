// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Core of the VNMF format: binary structures, builder and reader.
//!
//! FIX: moved out of the binary into a separate crate without GUI dependencies (eframe/egui/image),
//! so the format can be used from any application (CLI, server, another viewer)
//! and tested independently of the interface.

/// Block size — from `vnmf-parity` (it is also the parity shard size).
pub mod blocksize {
    pub use vnmf_parity::BlockSize;
}
pub mod builder;
pub mod formats;
pub mod header;
mod legacy;
/// Reed–Solomon redundancy lives in the separate `vnmf-parity` crate (usable without the format).
pub use vnmf_parity as parity;
pub mod reader;
pub mod recovery;
pub mod spec;
pub mod structure;
pub mod vnmf;

pub use blocksize::BlockSize;
pub use builder::{AuxInput, Builder, ChunkData, ChunkSource, SharedBytes};
pub use formats::{AuxFormat, InfoFormat, PageFormat, detect_extension, image_dimensions};
pub use header::Header;
pub use header::bit_flag::HeaderBitEnum;
pub use reader::{HeaderCopy, ReaderError, VnmfReader};
pub use structure::{ChapterSpan, VolumeSpan};
pub use vnmf::{AuxFlags, AuxKind, AuxRecord, EntryRecord, PageBitFlags, PageRecord, PageRole, PreviewRecord, RecordCrc};
