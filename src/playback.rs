// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Which page attachments should play and be shown — playback rules shared by
//! the desktop reader (`vnmf-reader`) and Android (via `vnmf-ffi`), so they behave the same.
//! No GUI — just rules, with tests.
//!
//! - **Music** of page `p` is the last music on the pages from the chapter start up to `p`
//!   (so it continues until another one is met; same when paging backwards). Music with
//!   `pages = N` plays only on N pages starting from its own, then silence; `duration_ms` limits
//!   the play time (the reader takes care of that).
//! - **Effects** — per page: when the page is opened; in a strip — when the trigger line
//!   (`trigger_y`) appears on screen.
//! - **Dark version** — replaces image `part` of the page.

use std::ops::Range;

use vnmf_core::{AuxKind, AuxRecord, VnmfReader};

/// Index (in the aux table) of the music that should play on page `page`; `chapter` — the chapter's pages.
pub fn music_at(reader: &VnmfReader, page: usize, chapter: Range<usize>) -> Option<usize> {
    for p in (chapter.start..=page.min(chapter.end.saturating_sub(1))).rev() {
        let found = reader.page_aux_range(p).zip(reader.page_aux(p)).rfind(|(_, a)| a.kind() == AuxKind::Music);
        if let Some((k, rec)) = found {
            let span = usize::from(rec.pages.get());
            return (span == 0 || page < p + span).then_some(k);
        }
    }
    None
}

/// Page effects: indices in the aux table.
pub fn sfx_of(reader: &VnmfReader, page: usize) -> Vec<usize> {
    reader.page_aux_range(page).zip(reader.page_aux(page)).filter(|(_, a)| a.kind() == AuxKind::Sfx).map(|(k, _)| k).collect()
}

/// Dark version of the page's image `part` (index in the aux table).
pub fn dark_of(reader: &VnmfReader, page: usize, part: usize) -> Option<usize> {
    reader.page_aux_range(page).zip(reader.page_aux(page)).find(|(_, a)| a.kind() == AuxKind::DarkImage && a.part.get() as usize == part).map(|(k, _)| k)
}

/// Line `trigger_y` (page px) → (fragment number, fraction of its height).
pub fn locate_y(heights: &[u32], y: u32) -> (usize, f64) {
    let mut top = 0u32;
    for (k, &h) in heights.iter().enumerate() {
        if y < top.saturating_add(h) || k + 1 == heights.len() {
            return (k, f64::from(y.saturating_sub(top)) / f64::from(h.max(1)));
        }
        top = top.saturating_add(h);
    }
    (0, 0.0)
}

/// Whether the file has attachments of this type.
pub fn has_kind(aux: &[AuxRecord], kind: AuxKind) -> bool {
    aux.iter().any(|a| a.kind() == kind)
}

/// Text to display from a text layer: UTF-8 as is, or region texts of `vnmf-text/1` JSON
/// (one paragraph per region, in record order).
pub fn text_layer(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return text.trim().to_string() };
    match json.get("regions").and_then(|r| r.as_array()) {
        Some(regions) => {
            regions.iter().filter_map(|r| r.get("text").and_then(|t| t.as_str())).map(str::trim).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n")
        }
        None => text.trim().to_string(),
    }
}

/// Page text: all its text layers, separated by an empty line; `None` — no layers.
pub fn page_text(reader: &VnmfReader, page: usize) -> Option<String> {
    let texts: Vec<String> = reader
        .page_aux_range(page)
        .zip(reader.page_aux(page))
        .filter(|(_, a)| a.kind() == AuxKind::Text)
        .filter_map(|(k, _)| reader.get_aux_data(k).ok().map(text_layer))
        .filter(|t| !t.is_empty())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vnmf_core::{AuxInput, Builder, ChunkSource, PageRole};

    fn png(seed: u8) -> ChunkSource {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x04\0\0\0\x06\x08\x02\0\0\0".to_vec();
        bytes.extend([0, 0, 0, 0, seed]);
        ChunkSource::from_bytes(bytes).unwrap()
    }

    fn sound(tag: &[u8]) -> ChunkSource {
        ChunkSource::from_bytes([b"OggS".as_slice(), tag].concat()).unwrap()
    }

    #[test]
    fn music_continues_until_replaced_within_chapter() {
        let path = std::env::temp_dir().join(format!("vnmf-playback-{}.vnmf", std::process::id()));
        let mut b = Builder::new();
        // Chapter 1: p. 0 (music A), 1, 2 (music B only here), 3; chapter 2: p. 4.
        b.add_page(png(0), PageRole::Normal, 1, 1, 0).unwrap();
        b.attach_aux(AuxInput::new(AuxKind::Music, sound(b"A"))).unwrap();
        b.add_page(png(1), PageRole::Normal, 1, 2, 0).unwrap();
        b.attach_aux(AuxInput::new(AuxKind::Sfx, sound(b"s"))).unwrap();
        b.add_page(png(2), PageRole::Normal, 1, 3, 0).unwrap();
        let mut only = AuxInput::new(AuxKind::Music, sound(b"B"));
        only.pages = 1;
        b.attach_aux(only).unwrap();
        b.add_page(png(3), PageRole::Normal, 1, 4, 0).unwrap();
        b.add_page(png(4), PageRole::Normal, 2, 1, 0).unwrap();
        b.build(&path).unwrap();
        let r = VnmfReader::open(&path).unwrap();
        let ch1 = 0..4;
        assert_eq!(music_at(&r, 0, ch1.clone()), Some(0));
        assert_eq!(music_at(&r, 1, ch1.clone()), Some(0));
        assert_eq!(music_at(&r, 2, ch1.clone()), Some(2));
        // After "this page only" music — silence, not a return to A.
        assert_eq!(music_at(&r, 3, ch1), None);
        assert_eq!(music_at(&r, 4, 4..5), None);
        // Music over two pages: silence on the third page from it.
        let path2 = path.with_extension("2.vnmf");
        let mut b = Builder::new();
        b.add_page(png(0), PageRole::Normal, 1, 1, 0).unwrap();
        let mut two = AuxInput::new(AuxKind::Music, sound(b"C"));
        two.pages = 2;
        b.attach_aux(two).unwrap();
        b.add_page(png(1), PageRole::Normal, 1, 2, 0).unwrap();
        b.add_page(png(2), PageRole::Normal, 1, 3, 0).unwrap();
        b.build(&path2).unwrap();
        let r2 = VnmfReader::open(&path2).unwrap();
        assert_eq!([0, 1, 2].map(|p| music_at(&r2, p, 0..3)), [Some(0), Some(0), None]);
        let _ = std::fs::remove_file(&path2);
        assert_eq!(sfx_of(&r, 1), vec![1]);
        assert!(sfx_of(&r, 0).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn text_layer_from_plain_and_json() {
        assert_eq!(text_layer("  Hello\n".as_bytes()), "Hello");
        let json = r#"{"schema": "vnmf-text/1", "regions": [{"x": 0, "y": 0, "w": 1, "h": 1, "text": "One"}, {"text": " Two "}]}"#;
        assert_eq!(text_layer(json.as_bytes()), "One\n\nTwo");
    }

    #[test]
    fn trigger_line_maps_to_fragment() {
        assert_eq!(locate_y(&[100, 200], 0), (0, 0.0));
        assert_eq!(locate_y(&[100, 200], 150), (1, 0.25));
        // Out of range — at the end of the last fragment.
        assert_eq!(locate_y(&[100], 300).0, 0);
    }
}
