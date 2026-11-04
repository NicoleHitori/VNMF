// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! File structure: volumes → chapters → pages.
//!
//! The format has no separate structure table: it follows unambiguously from the page table.
//! Pages are in reading order; a volume is the longest run of pages with the same
//! `volume_id`, a chapter — with the same `(volume_id, chapter_id)`. The `IS_VOLUME_START` /
//! `IS_CHAPTER_START` flags additionally open a new volume / chapter even with the same numbers
//! (for example, two unnumbered chapters in a row). A v1.0 file (all `volume_id = 0`) is a single
//! "volume 0" with chapters by `chapter_id`.

use std::ops::Range;

use crate::vnmf::{PageBitFlags, PageRecord};

/// Chapter: a contiguous range of pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterSpan {
    pub volume_id: u16,
    pub chapter_id: u16,
    /// Page indices in the page table.
    pub pages: Range<usize>,
}

/// Volume: a contiguous range of pages split into chapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSpan {
    /// 0 — volume not specified.
    pub volume_id: u16,
    pub pages: Range<usize>,
    pub chapters: Vec<ChapterSpan>,
}

/// Builds the structure from the page table. An empty table gives an empty list.
pub fn structure(pages: &[PageRecord]) -> Vec<VolumeSpan> {
    let mut volumes: Vec<VolumeSpan> = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let (volume_id, chapter_id, flags) = (page.volume_id.get(), page.chapter_id.get(), page.flags.get());
        let new_volume = match volumes.last() {
            None => true,
            Some(v) => v.volume_id != volume_id || flags & PageBitFlags::IS_VOLUME_START != 0,
        };
        if new_volume {
            volumes.push(VolumeSpan { volume_id, pages: i..i, chapters: Vec::new() });
        }
        let volume = volumes.last_mut().expect("volume was just added");
        let new_chapter = match volume.chapters.last() {
            None => true,
            Some(c) => c.chapter_id != chapter_id || flags & PageBitFlags::IS_CHAPTER_START != 0,
        };
        if new_chapter {
            volume.chapters.push(ChapterSpan { volume_id, chapter_id, pages: i..i });
        }
        volume.pages.end = i + 1;
        volume.chapters.last_mut().expect("chapter was just added").pages.end = i + 1;
    }
    volumes
}

/// Whether the file has volumes (at least one page with `volume_id != 0`).
pub fn has_volumes(pages: &[PageRecord]) -> bool {
    pages.iter().any(|p| p.volume_id.get() != 0)
}

/// Chapter and volume a page belongs to: (volume index, chapter index within the volume).
pub fn locate(volumes: &[VolumeSpan], page: usize) -> Option<(usize, usize)> {
    let v = volumes.iter().position(|v| v.pages.contains(&page))?;
    let c = volumes[v].chapters.iter().position(|c| c.pages.contains(&page))?;
    Some((v, c))
}
