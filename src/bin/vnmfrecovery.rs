// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! vnmfrecovery — recovery of damaged .vnmf files.
//!
//! Automatic mode goes through three steps:
//! 1. **header** — checks the header copy at the end of the file and the primary header; restores
//!    one copy from the other. If both are lost, the redundancy section is found by the signature
//!    of its CRC table blocks (`parity::find_layout`), and the parity then restores the primary header.
//! 2. **parity** — if the file was built with redundancy, finds damaged blocks by CRC
//!    and repairs them with Reed–Solomon.
//! 3. **carve** — if something is still damaged (or there's no redundancy), extracts everything
//!    it can: using the tables, or if they're lost — by signatures (images, description).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use vnmf_core::parity::{self, ParityLayout};
use vnmf_core::recovery::{self, CarvedKind, HeaderReport};
use vnmf_core::{PageFormat, VnmfReader};

use vnmf::{export, human_size};

#[derive(Parser)]
#[command(name = "vnmfrecovery", version, long_version = vnmf::LONG_VERSION, author = vnmf::AUTHORS, about = "Recovers damaged .vnmf files")]
#[command(after_help = "Modes (--mode):\n  \
    auto    all steps in turn (default)\n  \
    header  header copies only: check and restore one from the other\n  \
    parity  Reed–Solomon redundancy repair (the file must be built with --redundancy)\n  \
    carve   only extract the data into a folder (using the tables or by signatures)\n\n\
Exit codes: 0 — the file is intact or fully recovered; 1 — recovered partially or without confirmation (header copies differ and there's no redundancy); 2 — error.")]
struct Args {
    /// Damaged .vnmf file
    input: PathBuf,

    #[arg(long, value_enum, default_value_t = Mode::Auto)]
    mode: Mode,

    /// Where to write the recovered file (default <name>.repaired.vnmf next to it)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Fix the file in place (atomically, via a temporary file)
    #[arg(long, conflicts_with = "output")]
    in_place: bool,

    /// Folder for extracted data (default <name>.recovered/ next to it)
    #[arg(long)]
    carve_dir: Option<PathBuf>,

    /// Extract only by signatures, ignoring the tables
    #[arg(long)]
    raw: bool,

    /// Overwrite existing output files/folders
    #[arg(short, long)]
    force: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Mode {
    Auto,
    Header,
    Parity,
    Carve,
}

/// Summary for the exit code.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    Intact,
    Repaired,
    Partial,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(Outcome::Intact | Outcome::Repaired) => ExitCode::SUCCESS,
        Ok(Outcome::Partial) => ExitCode::from(1),
        Err(e) => {
            eprintln!("vnmfrecovery: error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn step(n: u8, title: &str) {
    println!("\n[{n}/3] {title}");
}

fn run(args: Args) -> Result<Outcome> {
    let original = fs::read(&args.input).with_context(|| format!("reading {}", args.input.display()))?;
    println!("{} — {}", args.input.display(), human_size(original.len() as u64));

    if args.mode == Mode::Carve {
        return carve(&args, &args.input, None);
    }

    // Format 1.x: a different header and parity layout — only data extraction is left.
    // The version byte isn't enough: a 2.0 file with a damaged `major` also looks like 1.x, but has an intact
    // 2.0 header in the other copy — that one is recovered as usual.
    if recovery::is_legacy(&original) {
        println!("  format 1.x file — metadata and parity recovery is not supported for it");
        println!("  tip: while the file is still readable, rebuild it as 2.0 (VNMF Creator → \"Save\")");
        if args.mode != Mode::Auto {
            bail!("format 1.x is not supported by this mode");
        }
        // An intact 1.x file is left alone; otherwise extract what survived, but that doesn't make the file
        // recovered, so the exit code is "partial".
        if let Ok(reader) = VnmfReader::open(&args.input)
            && damaged_chunks(&reader)?.is_empty()
        {
            println!("  metadata and CRC of all {} chunks are intact — no extraction needed", reader.chunks_count());
            return Ok(Outcome::Intact);
        }
        carve(&args, &args.input, Some("format 1.x"))?;
        return Ok(Outcome::Partial);
    }

    // ---------- 1. Header copies ----------
    step(1, "Header copies");
    let mut bytes = original.clone();
    let report = recovery::inspect_headers(&bytes);
    print_headers(&report);
    let agree = report.headers_agree();
    if !agree {
        println!("  ⚠ both headers pass CRC but differ ({}) — one of them was modified", report.header_differences().join(", "));
    }
    let layout = match report.best() {
        // The copies don't match, so the tail's parameters may be wrong: the parity layout
        // is confirmed by the CRC table blocks themselves (the VNMFPAR1 signature with parameters).
        Some(header) if !agree && args.mode != Mode::Header => find_parity(&bytes).or_else(|| recovery::parity_layout_of(header)),
        Some(header) => recovery::parity_layout_of(header),
        None => {
            println!("  both header copies are lost — the file parameters are unknown");
            // The primary header lies in the protected area: if the parity section is found,
            // Reed–Solomon will restore it too.
            let guessed = if matches!(args.mode, Mode::Auto | Mode::Parity) { find_parity(&bytes) } else { None };
            match guessed {
                Some(layout) => Some(layout),
                None if args.mode == Mode::Auto => return carve(&args, &args.input, Some("metadata lost")),
                None => bail!("cannot continue without a single intact header copy"),
            }
        }
    };

    // ---------- 2. Redundancy ----------
    let mut parity_failed = false;
    if args.mode != Mode::Header {
        step(2, "Redundancy (Reed–Solomon)");
        match &layout {
            None if args.mode == Mode::Parity => bail!("the file was built without redundancy (vnmfpack --redundancy)"),
            None => println!("  the file was built without redundancy — skipped"),
            Some(layout) => parity_failed = !repair_parity(&mut bytes, layout)?,
        }
    }

    // The primary metadata copy after parity: if it's still damaged — restore it from the tail.
    let after = recovery::inspect_headers(&bytes);
    if report.best().is_none() {
        if !after.primary_ok() {
            println!("  ⚠ the primary header was not restored by parity");
            if args.mode == Mode::Auto {
                return carve(&args, &args.input, Some("metadata lost"));
            }
            bail!("header not restored");
        }
        println!("  primary header restored by parity");
    }
    // Both copies are intact but different. The primary lies in the protected area: if parity exists and is
    // intact, it has already returned the primary to the genuine one — then the tail is rewritten from it. Without
    // parity there's no arbiter, so trust the tail (as for the file parameters).
    let primary_confirmed = layout.is_some() && args.mode != Mode::Header && !parity_failed;
    let after_agree = after.headers_agree();
    if !after_agree && primary_confirmed {
        println!("  primary header confirmed by parity — the tail copy will be rewritten");
    }
    let distrust_primary = !after_agree && !primary_confirmed;
    if distrust_primary {
        println!("  ⚠ no redundancy (or it didn't help): it's impossible to tell which copy is genuine — the tail one was taken");
    }
    if (!after.primary_complete() || distrust_primary) && after.tail_complete() && recovery::restore_front_from_tail(&mut bytes)? {
        println!("  primary header and tables restored from the tail copy");
    } else if !after.primary_complete() && !after.tail_complete() {
        // Neither copy is intact — assemble the metadata record by record from both (CRC of each record).
        let s = recovery::salvage_metadata(&mut bytes)?;
        println!(
            "  metadata assembled record by record from both copies: whole sections {}, records from the tail copy {}, unconfirmed {}{}",
            s.whole_sections,
            s.merged_records,
            s.bad_records,
            if s.exact { " — exact" } else { "" }
        );
    }

    // ---------- Writing the fixed file ----------
    let header_damage = !report.primary_complete() || !report.tail_complete() || !agree;
    let now = recovery::inspect_headers(&bytes);
    let needs_tail = !now.tail_complete() || !now.headers_agree();
    let changed = bytes != original || needs_tail;
    // The tail is deterministic, so on any repair it's rewritten entirely from the primary
    // metadata — this also removes garbage in the alignment padding between sections, which no CRC covers.
    let target = if changed { Some(write_repaired(&args, &bytes, true)?) } else { None };
    let checked_path = target.clone().unwrap_or_else(|| args.input.clone());

    // ---------- 3. Data check and, if needed, extraction ----------
    step(3, "Data check");
    let damaged = match VnmfReader::open(&checked_path) {
        Ok(reader) => {
            let bad = damaged_chunks(&reader)?;
            if bad.is_empty() {
                println!("  all {} chunks and {} previews are intact", reader.chunks_count(), reader.previews_count());
            } else {
                println!("  damaged chunks: {}", bad.len());
            }
            !bad.is_empty()
        }
        Err(e) => {
            println!("  the file can't be read: {e}");
            true
        }
    };

    // The copies were reconciled by guess — the file is readable, but the result is unconfirmed.
    if distrust_primary && !damaged && !parity_failed {
        println!("\nThe file was reconciled using the tail copy, but without confirmation → {}", checked_path.display());
        println!("Check the differing fields (vnmfinfo); if the primary copy was the genuine one — restore from the original.");
        return Ok(Outcome::Partial);
    }

    if !damaged && !parity_failed {
        return Ok(match &target {
            Some(path) => {
                println!("\nDone: the file is fully recovered → {}", path.display());
                Outcome::Repaired
            }
            None if header_damage => Outcome::Repaired,
            None => {
                println!("\nThe file is intact, no recovery needed.");
                Outcome::Intact
            }
        });
    }

    if args.mode != Mode::Auto {
        println!("\nNot everything could be recovered. Try `--mode carve` to extract the surviving data.");
        return Ok(Outcome::Partial);
    }
    carve(&args, &checked_path, Some("part of the data was not recovered"))?;
    Ok(Outcome::Partial)
}

fn print_headers(report: &HeaderReport) {
    match (&report.tail, &report.tail_tables) {
        (None, _) => println!("  tail copy: none (the file is too short)"),
        (Some(Err(e)), _) => println!("  tail copy (end of file): header damaged — {e}"),
        (Some(Ok(_)), Some(Err(e))) => println!("  tail copy (end of file): header intact, tables damaged — {e}"),
        (Some(Ok(_)), _) => println!("  tail copy (end of file): intact"),
    }
    match (&report.primary, &report.primary_tables) {
        (Err(e), _) => println!("  primary copy: header damaged — {e}"),
        (Ok(_), Err(e)) => println!("  primary copy: header intact, tables damaged — {e}"),
        (Ok(_), Ok(())) => println!("  primary copy: intact"),
    }
}

/// Searching for the parity section without a header — by the signature of the CRC table blocks
/// (see [`parity::find_layout`]).
fn find_parity(bytes: &[u8]) -> Option<ParityLayout> {
    println!("  searching for the redundancy section by the {} signature…", String::from_utf8_lossy(&vnmf_core::spec::PARITY_MAGIC));
    match parity::find_layout(bytes) {
        Some(found) => {
            println!(
                "  found: block {}, {}%, section at {:#x} — confirmed by {} of {} CRC table blocks",
                human_size(found.layout.shard_size as u64),
                found.layout.redundancy,
                found.layout.offset,
                found.blocks,
                2 * found.layout.table_blocks
            );
            Some(found.layout)
        }
        None => {
            println!("  parity section not found (the file has no redundancy or it was lost)");
            None
        }
    }
}

/// Checks and, if needed, repairs with parity. `Ok(false)` — some stripes were not recovered.
fn repair_parity(bytes: &mut Vec<u8>, layout: &ParityLayout) -> Result<bool> {
    println!(
        "  {}%: {} data shards of {}, {} stripes of {}+{}",
        layout.redundancy,
        layout.data_shards,
        human_size(layout.shard_size as u64),
        layout.stripes,
        layout.per_stripe,
        layout.parity_per_stripe
    );

    // A truncated file is padded with zeros — the lost shards simply won't pass the CRC.
    let end = layout.end() as usize;
    if bytes.len() < end {
        println!("  the file is truncated by {} — padded for recovery", human_size((end - bytes.len()) as u64));
        bytes.resize(end, 0);
    }

    let status = parity::check(bytes, layout);
    if status.is_clean() {
        println!("  all shards are intact");
        return Ok(true);
    }
    println!("  damaged: data shards {}, parity {}, CRC table blocks {}", status.bad_data.len(), status.bad_parity.len(), status.damaged_table_blocks);

    let report = parity::repair(bytes, layout)?;
    println!("  repaired: data shards {}, parity {}", report.repaired_data, report.repaired_parity);
    if !report.is_complete() {
        println!("  ⚠ could not recover {} stripe(s) of {}: more damage than redundancy", report.failed_stripes.len(), layout.stripes);
    }
    Ok(report.is_complete())
}

/// Writes the recovered bytes (atomically) and, if needed, rebuilds the tail copy.
fn write_repaired(args: &Args, bytes: &[u8], rebuild_tail: bool) -> Result<PathBuf> {
    let target = if args.in_place {
        args.input.clone()
    } else {
        let path = args.output.clone().unwrap_or_else(|| sibling(&args.input, ".repaired.vnmf"));
        if path.exists() && !args.force {
            bail!("{} already exists (add --force)", path.display());
        }
        path
    };

    let mut tmp = target.clone().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes)?;

    if rebuild_tail {
        match recovery::rebuild_tail(&tmp) {
            Ok(()) => println!("  tail copy rewritten from the primary metadata"),
            Err(e) => println!("  ⚠ tail copy not rebuilt: {e}"),
        }
    }
    fs::rename(&tmp, &target)?;
    Ok(target)
}

fn damaged_chunks(reader: &VnmfReader) -> Result<Vec<usize>> {
    let mut bad = Vec::new();
    for i in 0..reader.chunks_count() {
        if !reader.verify_chunk_crc(i).unwrap_or(false) {
            bad.push(i);
        }
    }
    for i in 0..reader.previews_count() {
        if !reader.verify_preview_crc(i).unwrap_or(false) {
            bad.push(usize::MAX - i);
        }
    }
    Ok(bad)
}

fn sibling(input: &Path, suffix: &str) -> PathBuf {
    input.with_file_name(format!("{}{suffix}", vnmf::input::file_stem(input)))
}

// ======================================================================
// Data extraction
// ======================================================================

/// Extracts data from file `source` (the original or an already partially recovered copy).
fn carve(args: &Args, source: &Path, reason: Option<&str>) -> Result<Outcome> {
    let dir = args.carve_dir.clone().unwrap_or_else(|| sibling(&args.input, ".recovered"));
    if dir.exists() && fs::read_dir(&dir)?.next().is_some() && !args.force {
        bail!("folder {} is not empty (add --force)", dir.display());
    }
    fs::create_dir_all(&dir)?;

    match reason {
        Some(r) => println!("\nExtracting data ({r}) → {}", dir.display()),
        None => println!("\nExtracting data → {}", dir.display()),
    }

    // First — using the tables (exact boundaries, page order, roles); otherwise — by signatures.
    if !args.raw {
        match VnmfReader::open(source) {
            Ok(reader) => return extract_by_tables(&reader, &dir),
            Err(e) => println!("  the tables can't be read ({e}) — searching by signatures"),
        }
    }
    extract_by_signatures(&fs::read(source)?, &dir)
}

fn extract_by_tables(reader: &VnmfReader, dir: &Path) -> Result<Outcome> {
    let mut good = 0;
    let mut damaged = 0;
    let width = reader.pages_count().to_string().len().max(3);

    for (i, page) in reader.pages().iter().enumerate() {
        let first = page.first_chunk_idx.get() as usize;
        let count = page.chunk_count.get() as usize;
        for j in 0..count {
            let Ok(data) = reader.get_chunk_data(first + j) else {
                damaged += 1;
                continue;
            };
            let ok = reader.verify_chunk_crc(first + j).unwrap_or(false);
            let ext = export::image_extension(data, PageFormat::from(page.format));
            let part = if count > 1 { format!("_{:02}", j + 1) } else { String::new() };
            let mark = if ok { "" } else { "_damaged" };
            fs::write(dir.join(format!("{:0width$}{part}{mark}.{ext}", i + 1)), data)?;
            if ok { good += 1 } else { damaged += 1 }
        }
    }
    for i in 0..reader.previews_count() {
        if let Ok(data) = reader.get_preview_data(i) {
            let ok = reader.verify_preview_crc(i).unwrap_or(false);
            let ext = export::image_extension(data, PageFormat::from(reader.previews()[i].format));
            fs::create_dir_all(dir.join("previews"))?;
            let mark = if ok { "" } else { "_damaged" };
            fs::write(dir.join(format!("previews/preview_{i:02}_vol{}{mark}.{ext}", reader.previews()[i].volume_id.get())), data)?;
        }
    }
    if let Some(info) = reader.info_bytes() {
        let (name, data) = export::info_file(info)?;
        fs::write(dir.join(name), data)?;
    }

    println!("  using the tables: intact fragments {good}, damaged {damaged} (marked _damaged)");
    Ok(if damaged == 0 { Outcome::Repaired } else { Outcome::Partial })
}

fn extract_by_signatures(bytes: &[u8], dir: &Path) -> Result<Outcome> {
    let found = recovery::carve(bytes);
    let mut log = String::from("# offset\tsize\tkind\tcomplete\tfile\n");
    let (mut images, mut partial, mut texts) = (0, 0, 0);

    for item in &found {
        let data = item.data(bytes);
        let name = match item.kind {
            CarvedKind::Image(format) => {
                images += 1;
                let mark = if item.complete { "" } else { "_partial" };
                partial += usize::from(!item.complete);
                format!("{images:03}{mark}.{}", format.extension())
            }
            CarvedKind::Text(_) => {
                texts += 1;
                let (name, _) = export::info_file(data)?;
                if texts == 1 { name } else { format!("{texts}_{name}") }
            }
        };
        let content = match item.kind {
            CarvedKind::Text(_) => export::info_file(data)?.1,
            CarvedKind::Image(_) => data.to_vec(),
        };
        fs::write(dir.join(&name), content)?;
        log.push_str(&format!("{:#x}\t{}\t{:?}\t{}\t{name}\n", item.offset, item.len, item.kind, item.complete));
    }
    fs::write(dir.join("recovery.tsv"), log)?;

    println!("  by signatures: images {images} (incomplete among them {partial}), descriptions {texts}; log — recovery.tsv");
    if images > 0 {
        println!("  note: a preview cover may come first; the order is as in the file. To pack back: vnmfpack {}", dir.display());
    }
    if images + texts == 0 {
        bail!("no data found");
    }
    Ok(Outcome::Partial)
}
