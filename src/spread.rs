// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Spreads: how manga pages are split into display groups (one page or two side by side) and in which
//! order they appear on screen. Shared by the desktop reader (`vnmf-reader`) and Android (`vnmf-ffi`),
//! so a spread looks the same. Tests are in `vnmf-reader` (layout.rs).

use std::collections::HashSet;

use vnmf_core::{PageRole, VnmfReader};

use crate::contents::Toc;

/// What needs to be known about a page for grouping.
#[derive(Clone, Copy, Debug)]
pub struct PageInfo {
    pub width: u32,
    pub height: u32,
    pub role: Option<PageRole>,
    /// A page made of several fragments (webtoon).
    pub multi_chunk: bool,
    /// First page of a chapter (not the first in the file): a spread never joins pages across the boundary.
    pub chapter_start: bool,
    /// First page of a volume (usually the volume cover).
    pub volume_start: bool,
}

/// Left and right halves of a spread explicitly set by the file (SpreadLeft / SpreadRight roles).
fn declared_pair(a: Option<PageRole>, b: Option<PageRole>) -> bool {
    matches!((a, b), (Some(PageRole::SpreadLeft), Some(PageRole::SpreadRight)) | (Some(PageRole::SpreadRight), Some(PageRole::SpreadLeft)))
}

/// Splits pages into display groups (in reading order).
///
/// - Spreads set by the file (left + right half in a row) are **always** shown together,
///   even without spread mode: it's one image cut in half.
/// - In spread mode the remaining pages go in pairs. Shown alone: covers (role in the file or
///   the "cover alone" setting), single-image spread pages (role or
///   width > height), webtoon strips. Half of a set spread is never "stolen" into a pair.
pub fn group_pages(pages: &[PageInfo], double: bool, cover_alone: bool) -> Vec<Vec<usize>> {
    let n = pages.len();
    let starts_pair = |i: usize| i + 1 < n && declared_pair(pages[i].role, pages[i + 1].role);
    let alone = |i: usize| {
        let p = &pages[i];
        p.multi_chunk
            || is_wide(p)
            || matches!(p.role, Some(PageRole::SpreadSingle | PageRole::FrontCover | PageRole::BackCover))
            || (i == 0 && cover_alone)
            // the cover of every volume — like the file cover
            || (p.volume_start && cover_alone)
            // half of a spread without a partner (the file set only one) — alone
            || matches!(p.role, Some(PageRole::SpreadLeft | PageRole::SpreadRight))
    };

    let mut groups = Vec::new();
    let mut i = 0;
    while i < n {
        // A pair: a spread set by the file, or (in spread mode) two ordinary pages.
        let auto_pair = double && !alone(i) && i + 1 < n && !alone(i + 1) && !starts_pair(i + 1) && !pages[i + 1].chapter_start;
        if starts_pair(i) || auto_pair {
            groups.push(vec![i, i + 1]);
            i += 2;
        } else {
            groups.push(vec![i]);
            i += 1;
        }
    }
    groups
}

/// Order of the group's pages on screen from left to right.
/// A file-defined spread — by roles (SpreadLeft on the left); otherwise right-to-left reverses it.
pub fn visual_order(group: &[usize], pages: &[PageInfo], rtl: bool) -> Vec<usize> {
    let mut order = group.to_vec();
    if order.len() == 2 && declared_pair(pages[order[0]].role, pages[order[1]].role) {
        if pages[order[0]].role == Some(PageRole::SpreadRight) {
            order.reverse();
        }
    } else if rtl {
        order.reverse();
    }
    order
}

/// Aspect ratio (width/height) of an "ordinary" book page — the median of narrow single pages.
/// This is the unit of scale: a wide single-image spread page takes as much space
/// as two ordinary ones side by side. `None` — there are no ordinary pages.
pub fn page_unit(pages: &[PageInfo]) -> Option<f64> {
    let mut ratios: Vec<f64> = pages.iter().filter(|p| !p.multi_chunk && !is_wide(p)).map(|p| f64::from(p.width) / f64::from(p.height.max(1))).collect();
    if ratios.is_empty() {
        return None;
    }
    ratios.sort_by(f64::total_cmp);
    Some(ratios[ratios.len() / 2])
}

pub fn is_wide(p: &PageInfo) -> bool {
    f64::from(p.width) > f64::from(p.height) * WIDE
}

/// A page wider than tall by more than 5% is a spread by itself.
pub const WIDE: f64 = 1.05;

impl PageInfo {
    /// Information about all pages of the file: sizes, roles, chapter and volume boundaries.
    /// Sizes — from the image header (if there is one fragment), otherwise from the page record.
    pub fn from_reader(reader: &VnmfReader, toc: &Toc) -> Vec<PageInfo> {
        let chapters: HashSet<usize> = toc.chapters().map(|c| c.pages.start).filter(|&p| p > 0).collect();
        let volumes: HashSet<usize> = toc.volumes.iter().filter(|v| toc.has_volumes && v.volume != 0).map(|v| v.pages.start).collect();
        reader
            .pages()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (width, height) = p
                    .chunk_range()
                    .next()
                    .and_then(|c| reader.get_chunk_data(c).ok())
                    .and_then(vnmf_core::image_dimensions)
                    .filter(|_| p.chunk_range().len() == 1)
                    .unwrap_or((p.width.get(), p.height.get()));
                PageInfo {
                    width,
                    height,
                    role: PageRole::try_from(p.role).ok(),
                    multi_chunk: p.chunk_range().len() > 1,
                    chapter_start: chapters.contains(&i),
                    volume_start: volumes.contains(&i),
                }
            })
            .collect()
    }
}
