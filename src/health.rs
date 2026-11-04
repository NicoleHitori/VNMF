// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! File integrity check and page description — shared by `vnmfinfo` and the GUI (`vnmf-gui`).

use anyhow::Result;
use vnmf_core::parity::ParityStatus;
use vnmf_core::{HeaderBitEnum, PageBitFlags, PageFormat, PageRecord, PageRole, VnmfReader, recovery};
use vnmf_i18n::{ntr, tr, trf};

/// Data check result: CRC of all chunks and previews, parity state.
pub struct Verify {
    pub checked: usize,
    pub bad: Vec<String>,
    pub bad_chunks: Vec<usize>,
    pub bad_previews: usize,
    pub parity: Option<ParityStatus>,
}

pub fn verify(reader: &VnmfReader) -> Result<Verify> {
    let mut bad = Vec::new();
    let mut bad_chunks = Vec::new();
    let mut bad_previews = 0;
    for i in 0..reader.chunks_count() {
        // A chunk beyond the end of the file (a truncated file) is also damage, not a program error.
        if !reader.verify_chunk_crc(i).unwrap_or(false) {
            bad.push(trf!("chunk #{0}", i));
            bad_chunks.push(i);
        }
    }
    for i in 0..reader.previews_count() {
        if !reader.verify_preview_crc(i).unwrap_or(false) {
            bad.push(trf!("preview #{0}", i));
            bad_previews += 1;
        }
    }
    Ok(Verify { checked: reader.chunks_count() + reader.previews_count(), bad, bad_chunks, bad_previews, parity: reader.verify_parity() })
}

pub fn role_name(page: &PageRecord) -> String {
    PageRole::try_from(page.role).map_or_else(|r| format!("?({r})"), |r| r.name().to_string())
}

/// Actual formats of all of a page's chunks (no duplicates, in order of appearance).
pub fn chunk_formats(reader: &VnmfReader, i: usize) -> Vec<PageFormat> {
    let mut out = Vec::new();
    for data in reader.get_page_chunks_data(i).unwrap_or_default() {
        let f = PageFormat::detect(data);
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// Page attachment types in short: `music+sfx+text`; `-` — none.
pub fn aux_summary(reader: &VnmfReader, page: usize) -> String {
    let aux = reader.page_aux(page);
    if aux.is_empty() {
        return "-".into();
    }
    aux.iter().map(|a| a.kind().name()).collect::<Vec<_>>().join("+")
}

pub fn is_mixed(page: &PageRecord) -> bool {
    page.flags.get() & PageBitFlags::MIXED_FORMATS != 0
}

/// The format from the table and the actual one (from the first chunk's bytes), if they differ.
pub fn page_format(reader: &VnmfReader, i: usize, page: &PageRecord) -> (PageFormat, Option<PageFormat>) {
    let recorded = PageFormat::from(page.format);
    let detected = reader.get_page_data(i).map(PageFormat::detect).unwrap_or(PageFormat::Unknown);
    (recorded, (detected.is_known() && detected != recorded).then_some(detected))
}

/// Integrity check summary: what is damaged and whether it can be recovered.
pub struct Health {
    /// Description of each problem found.
    pub issues: Vec<String>,
    /// Prognosis for `vnmfrecovery` (only if there are problems).
    pub prognosis: &'static str,
    /// Whether the data was checked (without `--quick`).
    pub data_checked: bool,
}

impl Health {
    pub fn is_ok(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Page number (from 1) the chunk belongs to.
pub fn chunk_owner(reader: &VnmfReader, chunk: usize) -> Option<usize> {
    let from_aux = reader.aux().iter().find(|a| a.chunk_idx.get() as usize == chunk).map(|a| a.page_idx.get() as usize);
    from_aux.or_else(|| reader.pages().iter().position(|p| p.chunk_range().contains(&chunk))).map(|i| i + 1)
}

pub fn assess(reader: &VnmfReader, verification: Option<&Verify>) -> Health {
    let mut issues = Vec::new();

    // Both metadata copies — in full (header, tables, Info). A 1.x file has nothing to be
    // checked against: the reader has already read it, and this version can't recover the old format.
    let copies = recovery::inspect_headers(reader.raw());
    let legacy = reader.legacy_version().is_some();
    let primary_bad = !legacy && !copies.primary_complete();
    let tail_bad = !legacy && reader.has_flag(HeaderBitEnum::HasTailDup) && !copies.tail_complete();
    if primary_bad {
        issues.push(tr("the primary metadata copy (start of file) is damaged — the backup was used").to_string());
    }
    if tail_bad {
        issues.push(tr("the backup metadata copy (end of file) is damaged").to_string());
    }
    // The same check as in `vnmfrecovery`: both headers pass CRC but differ.
    let disagree = !legacy && !primary_bad && !tail_bad && !copies.headers_agree();
    if disagree {
        issues.push(trf!("the primary and backup headers are intact but differ ({0}) — one of them was modified", copies.header_differences().join(", ")));
    }

    let mut data_bad = false;
    let mut parity_fixes_all = false;
    if let Some(v) = verification {
        if !v.bad.is_empty() {
            data_bad = true;
            let mut pages: Vec<usize> = v.bad_chunks.iter().filter_map(|&c| chunk_owner(reader, c)).collect();
            pages.dedup();
            let shown: Vec<String> = pages.iter().take(12).map(usize::to_string).collect();
            let mut text = trf!("damaged data: {0} of {1}", v.bad.len(), v.checked);
            if !pages.is_empty() {
                text += &trf!(" — pages {0}{1}", shown.join(", "), if pages.len() > 12 { ", …" } else { "" });
            }
            if v.bad_previews > 0 {
                text += &trf!("; previews: {0}", v.bad_previews);
            }
            issues.push(text);
        }
        if let Some(p) = &v.parity {
            if !p.is_clean() {
                issues.push(trf!(
                    "redundancy: damaged data blocks {0}, parity {1}, CRC tables {2}",
                    p.bad_data.len(),
                    p.bad_parity.len(),
                    p.damaged_table_blocks
                ));
            }
            parity_fixes_all = p.is_recoverable();
        }
    }

    let has_parity = reader.parity_layout().is_some();
    let prognosis = if disagree && !data_bad && has_parity {
        // The primary header is in the protected area: parity will show whether it was modified.
        tr("fully recoverable: redundancy will confirm the genuine header copy")
    } else if disagree && !data_bad {
        tr("the copies agree with the backup, but without redundancy there's no way to verify which one is genuine")
    } else if !data_bad && !(verification.is_some_and(|v| v.parity.as_ref().is_some_and(|p| !p.is_clean()))) {
        // Only the metadata is damaged, and one of the copies is intact.
        tr("fully recoverable from the backup metadata copy")
    } else if verification.is_some_and(|v| v.parity.is_some()) && parity_fixes_all {
        tr("fully recoverable thanks to redundancy")
    } else if verification.is_some_and(|v| v.parity.is_some()) {
        tr("more damage than redundancy: some pages will be lost, the surviving ones can be extracted")
    } else {
        tr("no redundancy: damaged pages can't be recovered, the surviving ones can be extracted")
    };

    Health { issues, prognosis, data_checked: verification.is_some() }
}

/// "1 problem", "2 problems".
pub fn problems_word(n: usize) -> &'static str {
    ntr("problem", "problems", n as u64)
}
