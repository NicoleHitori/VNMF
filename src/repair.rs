// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Repairing a damaged `.vnmf` without user interaction — the first two steps of `vnmfrecovery --mode auto`:
//!
//! 1. **header copies** — restore one from the other; if both are lost, find the parity section
//!    by the signature of its CRC table blocks;
//! 2. **redundancy** — repair damaged blocks with Reed–Solomon.
//!
//! Extracting what survived (the "carve" step) is left to `vnmfrecovery`: it produces a folder, not a
//! file. Used by `vnmf-server`; the log lines are meant for people.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use vnmf_core::parity::{self, ParityLayout};
use vnmf_core::{VnmfReader, recovery};

use crate::human_size;

/// How it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    /// Nothing to repair; nothing was written.
    Intact,
    /// Fully repaired and written.
    Repaired,
    /// Repaired only partially, or the header copies were reconciled without confirmation; written if
    /// `written` is set (the file may be more readable than before), otherwise nothing could be done.
    Partial,
}

pub struct Report {
    pub outcome: Outcome,
    /// Whether `output` was written.
    pub written: bool,
    /// Damaged data chunks and previews left after the repair (in the written file, or the source).
    pub damaged_left: usize,
    pub log: Vec<String>,
}

/// Repairs `input`, writing the result to `output` (may be the same path: written next to it and renamed).
pub fn repair(input: &Path, output: &Path) -> Result<Report> {
    let original = fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let mut log = vec![format!("{} — {}", input.display(), human_size(original.len() as u64))];
    let partial = |log: Vec<String>, damaged_left| Ok(Report { outcome: Outcome::Partial, written: false, damaged_left, log });

    if recovery::is_legacy(&original) {
        log.push("format 1.x — metadata and parity recovery is not supported for it; rebuild it as 2.0 while it is readable".into());
        return match VnmfReader::open(input) {
            Ok(reader) if damaged(&reader) == 0 => Ok(Report { outcome: Outcome::Intact, written: false, damaged_left: 0, log }),
            Ok(reader) => partial(log, damaged(&reader)),
            Err(_) => partial(log, 0),
        };
    }

    // ---------- 1. Header copies ----------
    let mut bytes = original.clone();
    let report = recovery::inspect_headers(&bytes);
    log.push(format!(
        "header copies: primary {}, tail {}",
        if report.primary_complete() { "intact" } else { "damaged" },
        if report.tail_complete() { "intact" } else { "damaged" }
    ));
    let agree = report.headers_agree();
    if !agree {
        log.push(format!("both headers pass CRC but differ ({}) — one of them was modified", report.header_differences().join(", ")));
    }
    let layout = match report.best() {
        // The copies don't match, so the tail's parameters may be wrong: the parity layout is confirmed
        // by the CRC table blocks themselves.
        Some(header) if !agree => find_parity(&bytes, &mut log).or_else(|| recovery::parity_layout_of(header)),
        Some(header) => recovery::parity_layout_of(header),
        None => {
            log.push("both header copies are lost".into());
            match find_parity(&bytes, &mut log) {
                Some(layout) => Some(layout),
                None => {
                    log.push("metadata lost and no redundancy — only extracting the surviving images is possible (vnmfrecovery)".into());
                    return partial(log, 0);
                }
            }
        }
    };

    // ---------- 2. Redundancy ----------
    let parity_failed = match &layout {
        None => {
            log.push("the file was built without redundancy".into());
            false
        }
        Some(layout) => !repair_parity(&mut bytes, layout, &mut log)?,
    };

    let after = recovery::inspect_headers(&bytes);
    if report.best().is_none() {
        if !after.primary_ok() {
            log.push("the primary header was not restored by parity".into());
            return partial(log, 0);
        }
        log.push("primary header restored by parity".into());
    }
    // Both copies intact but different: the primary lies in the protected area, so intact parity has
    // already returned it to the genuine one. Without parity there's no arbiter — trust the tail.
    let primary_confirmed = layout.is_some() && !parity_failed;
    let distrust_primary = !after.headers_agree() && !primary_confirmed;
    if !after.headers_agree() && primary_confirmed {
        log.push("primary header confirmed by parity — the tail copy will be rewritten".into());
    }
    if distrust_primary {
        log.push("no redundancy (or it didn't help): it's impossible to tell which copy is genuine — the tail one was taken".into());
    }
    if (!after.primary_complete() || distrust_primary) && after.tail_complete() && recovery::restore_front_from_tail(&mut bytes)? {
        log.push("primary header and tables restored from the tail copy".into());
    } else if !after.primary_complete() && !after.tail_complete() {
        let s = recovery::salvage_metadata(&mut bytes)?;
        log.push(format!(
            "metadata assembled record by record from both copies: whole sections {}, records from the tail copy {}, unconfirmed {}{}",
            s.whole_sections,
            s.merged_records,
            s.bad_records,
            if s.exact { " — exact" } else { "" }
        ));
    }

    // ---------- Writing ----------
    let header_damage = !report.primary_complete() || !report.tail_complete() || !agree;
    let now = recovery::inspect_headers(&bytes);
    let changed = bytes != original || !now.tail_complete() || !now.headers_agree();
    if changed {
        write(output, &bytes, &mut log)?;
    }
    let checked = if changed { output } else { input };

    // ---------- 3. Data check ----------
    let damaged_left = match VnmfReader::open(checked) {
        Ok(reader) => damaged(&reader),
        Err(e) => {
            log.push(format!("the file can't be read: {e}"));
            return Ok(Report { outcome: Outcome::Partial, written: changed, damaged_left: 0, log });
        }
    };
    if damaged_left > 0 {
        log.push(format!("damaged chunks left: {damaged_left}"));
    }
    let outcome = if damaged_left > 0 || parity_failed || distrust_primary {
        Outcome::Partial
    } else if changed || header_damage {
        log.push("the file is fully recovered".into());
        Outcome::Repaired
    } else {
        log.push("the file is intact".into());
        Outcome::Intact
    };
    Ok(Report { outcome, written: changed, damaged_left, log })
}

fn find_parity(bytes: &[u8], log: &mut Vec<String>) -> Option<ParityLayout> {
    match parity::find_layout(bytes) {
        Some(found) => {
            log.push(format!(
                "redundancy section found by signature: {}%, at {:#x}, confirmed by {} of {} CRC table blocks",
                found.layout.redundancy,
                found.layout.offset,
                found.blocks,
                2 * found.layout.table_blocks
            ));
            Some(found.layout)
        }
        None => {
            log.push("redundancy section not found".into());
            None
        }
    }
}

/// `Ok(false)` — some stripes were not recovered.
fn repair_parity(bytes: &mut Vec<u8>, layout: &ParityLayout, log: &mut Vec<String>) -> Result<bool> {
    // A truncated file is padded with zeros — the lost shards simply won't pass the CRC.
    let end = usize::try_from(layout.end()).context("parity section beyond the address space")?;
    if bytes.len() < end {
        log.push(format!("the file is truncated by {} — padded for recovery", human_size((end - bytes.len()) as u64)));
        bytes.resize(end, 0);
    }
    let status = parity::check(bytes, layout);
    if status.is_clean() {
        log.push(format!("redundancy {}%: all shards are intact", layout.redundancy));
        return Ok(true);
    }
    log.push(format!(
        "redundancy {}%: damaged data shards {}, parity {}, CRC table blocks {}",
        layout.redundancy,
        status.bad_data.len(),
        status.bad_parity.len(),
        status.damaged_table_blocks
    ));
    let report = parity::repair(bytes, layout)?;
    log.push(format!("repaired: data shards {}, parity {}", report.repaired_data, report.repaired_parity));
    if !report.is_complete() {
        log.push(format!("could not recover {} stripe(s) of {}: more damage than redundancy", report.failed_stripes.len(), layout.stripes));
    }
    Ok(report.is_complete())
}

/// Writes atomically and rebuilds the tail copy from the primary metadata (it's deterministic; this also
/// clears garbage in the alignment padding, which no CRC covers).
fn write(target: &Path, bytes: &[u8], log: &mut Vec<String>) -> Result<()> {
    let mut tmp = target.as_os_str().to_owned();
    tmp.push(".repair.tmp");
    let tmp = std::path::PathBuf::from(tmp);
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = recovery::rebuild_tail(&tmp) {
        log.push(format!("tail copy not rebuilt: {e}"));
    }
    fs::rename(&tmp, target).with_context(|| format!("renaming to {}", target.display()))?;
    Ok(())
}

fn damaged(reader: &VnmfReader) -> usize {
    (0..reader.chunks_count()).filter(|&i| !reader.verify_chunk_crc(i).unwrap_or(false)).count()
        + (0..reader.previews_count()).filter(|&i| !reader.verify_preview_crc(i).unwrap_or(false)).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vnmf_core::{Builder, ChunkSource, PageRole};

    fn png(seed: u8) -> ChunkSource {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x04\0\0\0\x06\x08\x02\0\0\0".to_vec();
        bytes.extend(std::iter::repeat_n(seed, 6000));
        ChunkSource::from_bytes(bytes).unwrap()
    }

    fn build(name: &str, redundancy: u8) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("vnmf-repair-{name}-{}.vnmf", std::process::id()));
        let mut b = Builder::new().with_redundancy(redundancy).with_tail_duplicate(true);
        for i in 0..4 {
            b.add_page(png(i), PageRole::Normal, 1, u16::from(i) + 1, 0).unwrap();
        }
        b.build(&path).unwrap();
        path
    }

    #[test]
    fn intact_file_is_left_alone() {
        let path = build("intact", 10);
        let out = path.with_extension("out.vnmf");
        let r = repair(&path, &out).unwrap();
        assert_eq!(r.outcome, Outcome::Intact);
        assert!(!r.written && !out.exists());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn parity_repairs_damaged_data_in_place() {
        let path = build("data", 20);
        let good = fs::read(&path).unwrap();
        let reader = VnmfReader::open(&path).unwrap();
        let offset = reader.chunks()[1].offset() as usize;
        drop(reader);
        let mut bad = good.clone();
        for b in &mut bad[offset + 100..offset + 600] {
            *b ^= 0xA5;
        }
        fs::write(&path, &bad).unwrap();
        let r = repair(&path, &path).unwrap();
        assert_eq!(r.outcome, Outcome::Repaired, "{:?}", r.log);
        assert_eq!(fs::read(&path).unwrap(), good);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn damage_without_parity_is_partial() {
        let path = build("noparity", 0);
        let reader = VnmfReader::open(&path).unwrap();
        let offset = reader.chunks()[2].offset() as usize;
        drop(reader);
        let mut bad = fs::read(&path).unwrap();
        bad[offset + 50] ^= 0xFF;
        fs::write(&path, &bad).unwrap();
        let r = repair(&path, &path).unwrap();
        assert_eq!(r.outcome, Outcome::Partial);
        assert_eq!(r.damaged_left, 1);
        let _ = fs::remove_file(&path);
    }
}
