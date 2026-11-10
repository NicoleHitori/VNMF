// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Shared code of the console tools `vnmfinfo`, `vnmfpack`, `vnmfunpack`, `vnmfrecovery`.
//!
//! The format itself lives in `vnmf-core`; here is only what the tools need:
//! reading CBZ/folders, VNMF metadata (and conversion from/to ComicInfo.xml), natural name sorting.

/// Authors (for `--version` and "About" dialogs).
pub const AUTHORS: &str = "void_nicole (Nicole Hitori) + AI-assist";
/// License (SPDX).
pub const LICENSE: &str = "GPL-3.0-or-later";
/// Extended `--version` for all console tools.
pub const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\nAuthors: void_nicole (Nicole Hitori) + AI-assist",
    "\nLicense: GNU GPL version 3 or later <https://www.gnu.org/licenses/gpl-3.0.html>.",
    "\nThis is free software: you are free to change and redistribute it.",
    "\nThere is NO WARRANTY, to the extent permitted by law."
);

pub mod auxfiles;
pub mod comicinfo;
pub mod contents;
pub mod edit;
pub mod export;
pub mod health;
pub mod input;
pub mod metadata;
pub mod natsort;
pub mod pack;
pub mod playback;
pub mod repair;
pub mod rewrite;
pub mod spread;
pub mod unpack;

/// Human-readable size: `1.4 MiB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// UUID in canonical form `xxxxxxxx-xxxx-…`.
pub fn format_uuid(bytes: [u8; 16]) -> String {
    uuid::Uuid::from_bytes(bytes).hyphenated().to_string()
}
