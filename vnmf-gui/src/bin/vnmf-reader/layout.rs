// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Pure reader geometry (no GTK): grouping pages into spreads, page sizes
//! in paged mode, strip row positions. Kept separate so it can be covered by tests.

use crate::settings::Fit;

// Grouping into spreads is shared with Android (`vnmf::spread`), this is just a re-export.
pub use vnmf::spread::{PageInfo, WIDE, group_pages, page_unit, visual_order};

/// Page sizes of a row (a single page or a spread) and the zoom percentage.
///
/// **100% means one ordinary page across the full width of the reading area.** A spread is two
/// pages, so at the same zoom it is twice as wide as a single page: both two halves
/// (each full width) and a wide single-image spread (height like an ordinary page,
/// width like two; `unit` is the aspect ratio of an ordinary page, see [`page_unit`]).
/// This way the zoom doesn't jump when moving between a page and a spread.
/// `Fit::Page` — fit the whole group, `Fit::Width` — exactly 100%, `Fit::Custom` — `zoom`.
pub fn row_sizes(ratios: &[f64], unit: Option<f64>, viewport: (f64, f64), fit: Fit, zoom: f64) -> (Vec<(f64, f64)>, f64) {
    let (vw, vh) = viewport;
    let total: f64 = ratios.iter().sum::<f64>().max(0.01);
    let widest = ratios.iter().copied().fold(0.01, f64::max);
    // The width that equals the window width at 100%: the page itself, or an ordinary one for a wide page.
    let reference = match unit {
        Some(unit) if widest > WIDE => unit,
        _ => widest,
    };
    let page_fit = vw / reference; // the height at which an ordinary page spans exactly the full width
    let height = match fit {
        Fit::Page => vh.min(vw / total), // the whole group is visible
        Fit::Width => page_fit,
        Fit::Custom => page_fit * zoom,
    }
    .floor()
    .max(20.0);
    let sizes = ratios.iter().map(|r| ((height * r).floor(), height)).collect();
    (sizes, height / page_fit)
}

/// Fragment sizes of a column (a webtoon page in paged mode): by width.
pub fn column_sizes(heights_per_width: &[f64], viewport_width: f64, fit: Fit, zoom: f64) -> (Vec<(f64, f64)>, f64) {
    let scale = if fit == Fit::Custom { zoom } else { 1.0 };
    let width = (viewport_width * scale).floor().max(20.0);
    (heights_per_width.iter().map(|k| (width, (width * k).floor())).collect(), width / viewport_width)
}

/// Top edges of the strip images for width `width`: exactly as GTK lays them out
/// (height = round(width × aspect), plus spacing). `before[i]` is the height of the insert above
/// row `i` (a chapter header), it is not part of the row. The last element is the total height.
pub fn strip_tops(heights_per_width: &[f64], before: &[f64], width: f64, gap: f64) -> Vec<f64> {
    let mut tops = Vec::with_capacity(heights_per_width.len() + 1);
    let mut y = 0.0;
    for (i, k) in heights_per_width.iter().enumerate() {
        y += before.get(i).copied().unwrap_or(0.0);
        tops.push(y);
        y += (width * k).round() + gap;
    }
    tops.push(y);
    tops
}

#[cfg(test)]
mod tests {
    use super::*;
    use vnmf_core::PageRole;

    fn page(role: Option<PageRole>) -> PageInfo {
        PageInfo { width: 1600, height: 2400, role, multi_chunk: false, chapter_start: false, volume_start: false }
    }

    #[test]
    fn plain_double_page_pairs_after_cover() {
        let pages = vec![page(None); 6];
        assert_eq!(group_pages(&pages, true, true), vec![vec![0], vec![1, 2], vec![3, 4], vec![5]]);
        assert_eq!(group_pages(&pages, true, false), vec![vec![0, 1], vec![2, 3], vec![4, 5]]);
        assert_eq!(group_pages(&pages, false, true), (0..6).map(|i| vec![i]).collect::<Vec<_>>());
    }

    #[test]
    fn declared_spread_always_together_and_never_split() {
        // 0 cover, 1 regular, 2+3 — spread from the file, 4 regular.
        let pages = vec![page(Some(PageRole::FrontCover)), page(None), page(Some(PageRole::SpreadLeft)), page(Some(PageRole::SpreadRight)), page(None)];
        // Without spread mode a file-defined spread is still shown together.
        assert_eq!(group_pages(&pages, false, false), vec![vec![0], vec![1], vec![2, 3], vec![4]]);
        // In spread mode page 1 doesn't "steal" the left half — it stays alone.
        assert_eq!(group_pages(&pages, true, false), vec![vec![0], vec![1], vec![2, 3], vec![4]]);
    }

    #[test]
    fn roles_from_file_keep_pages_alone() {
        let wide = PageInfo { width: 3200, height: 2400, role: None, multi_chunk: false, chapter_start: false, volume_start: false };
        let pages = vec![page(None), page(Some(PageRole::SpreadSingle)), page(None), wide, page(None), page(Some(PageRole::BackCover))];
        assert_eq!(group_pages(&pages, true, false), vec![vec![0], vec![1], vec![2], vec![3], vec![4], vec![5]]);
    }

    #[test]
    fn declared_spread_sides_follow_roles_not_reading_direction() {
        let pages = vec![page(Some(PageRole::SpreadRight)), page(Some(PageRole::SpreadLeft)), page(None), page(None)];
        // The right half comes first in reading order (manga) — but is still on the right on screen.
        assert_eq!(visual_order(&[0, 1], &pages, true), vec![1, 0]);
        assert_eq!(visual_order(&[0, 1], &pages, false), vec![1, 0]);
        // A regular pair — in reading direction.
        assert_eq!(visual_order(&[2, 3], &pages, true), vec![3, 2]);
        assert_eq!(visual_order(&[2, 3], &pages, false), vec![2, 3]);
    }

    #[test]
    fn hundred_percent_is_page_width() {
        // A tall 1:4 page in a 1000×800 window.
        let (sizes, pct) = row_sizes(&[0.25], None, (1000.0, 800.0), Fit::Width, 1.0);
        assert_eq!(sizes[0].0, 1000.0);
        assert!((pct - 1.0).abs() < 0.01);
        // "Fit page" for it is 20% (height 800 → width 200).
        let (sizes, pct) = row_sizes(&[0.25], None, (1000.0, 800.0), Fit::Page, 1.0);
        assert_eq!(sizes[0], (200.0, 800.0));
        assert!((pct - 0.2).abs() < 0.01);
        // Custom zoom is relative to width: 50% = half the window width.
        let (sizes, _) = row_sizes(&[0.25], None, (1000.0, 800.0), Fit::Custom, 0.5);
        assert_eq!(sizes[0].0, 500.0);
    }

    #[test]
    fn spread_counts_as_two_pages() {
        // A spread of two 2:3 pages in a 1000×800 window.
        let ratios = [2.0 / 3.0, 2.0 / 3.0];
        // 100%: each half spans the full window width — the spread is twice as wide, scrolling.
        let (sizes, pct) = row_sizes(&ratios, None, (1000.0, 800.0), Fit::Width, 1.0);
        assert_eq!(sizes, vec![(1000.0, 1500.0), (1000.0, 1500.0)]);
        assert!((pct - 1.0).abs() < 0.01);
        // The same zoom as for a single page of the same size.
        let (single, _) = row_sizes(&ratios[..1], None, (1000.0, 800.0), Fit::Custom, 0.5);
        let (pair, _) = row_sizes(&ratios, None, (1000.0, 800.0), Fit::Custom, 0.5);
        assert_eq!(single[0], pair[0]);
        // "Fit" — the whole spread in the window: height 750 (width 2×500), that's 50%.
        let (sizes, pct) = row_sizes(&ratios, None, (1000.0, 800.0), Fit::Page, 1.0);
        assert_eq!(sizes, vec![(500.0, 750.0), (500.0, 750.0)]);
        assert!((pct - 0.5).abs() < 0.01);
    }

    #[test]
    fn wide_spread_image_takes_two_pages() {
        // A book of 2:3 pages and one single-image 4:3 spread page.
        let mut pages = vec![page(None); 5];
        pages.push(PageInfo { width: 3200, height: 2400, role: None, multi_chunk: false, chapter_start: false, volume_start: false });
        let unit = page_unit(&pages);
        assert!((unit.unwrap() - 2.0 / 3.0).abs() < 1e-9);
        // At the same zoom the height is equal, and the spread is twice as wide as a page.
        let (single, _) = row_sizes(&[2.0 / 3.0], unit, (1000.0, 800.0), Fit::Custom, 0.5);
        let (wide, pct) = row_sizes(&[4.0 / 3.0], unit, (1000.0, 800.0), Fit::Custom, 0.5);
        assert_eq!(single[0], (500.0, 750.0));
        assert_eq!(wide[0], (1000.0, 750.0));
        assert!((pct - 0.5).abs() < 0.01);
        // 100%: the spread is two window widths.
        let (wide, _) = row_sizes(&[4.0 / 3.0], unit, (1000.0, 800.0), Fit::Width, 1.0);
        assert_eq!(wide[0].0, 2000.0);
        // "Fit": the whole spread in the window.
        let (wide, _) = row_sizes(&[4.0 / 3.0], unit, (1000.0, 800.0), Fit::Page, 1.0);
        assert_eq!(wide[0], (1000.0, 750.0));
        // A book of only wide pages — there's no unit, 100% = full width.
        assert_eq!(page_unit(&pages[5..]), None);
        let (wide, _) = row_sizes(&[4.0 / 3.0], None, (1000.0, 800.0), Fit::Width, 1.0);
        assert_eq!(wide[0].0, 1000.0);
    }

    #[test]
    fn strip_tops_include_gaps() {
        let tops = strip_tops(&[1.0, 0.5, 2.0], &[], 100.0, 10.0);
        assert_eq!(tops, vec![0.0, 110.0, 170.0, 380.0]);
        // A 40 px chapter header above the first and third rows.
        let tops = strip_tops(&[1.0, 0.5, 2.0], &[40.0, 0.0, 40.0], 100.0, 10.0);
        assert_eq!(tops, vec![40.0, 150.0, 250.0, 460.0]);
    }

    #[test]
    fn pairs_never_cross_chapters_and_volume_covers_stand_alone() {
        let mut pages = vec![page(None); 7];
        // Chapter 2 — from page 3; volume 2 (with a cover) — from page 5.
        pages[3].chapter_start = true;
        pages[5].chapter_start = true;
        pages[5].volume_start = true;
        assert_eq!(group_pages(&pages, true, true), vec![vec![0], vec![1, 2], vec![3, 4], vec![5], vec![6]]);
        assert_eq!(group_pages(&pages, true, false), vec![vec![0, 1], vec![2], vec![3, 4], vec![5, 6]]);
    }
}
