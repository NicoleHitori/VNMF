// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! vnmfinfo — full information about a .vnmf file.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use serde_json::{Value, json};
use vnmf_core::header::bit_flag::HeaderFlagsFormatter;
use vnmf_core::parity::ParityStatus;
use vnmf_core::{AuxFlags, AuxFormat, AuxKind, HeaderBitEnum, InfoFormat, PageBitFlags, PageFormat, VnmfReader, recovery, spec};

use vnmf::health::{Health, Verify, assess, aux_summary, chunk_formats, is_mixed, page_format, problems_word, role_name, verify};
use vnmf::contents::Toc;
use vnmf::metadata::{self, Metadata};
use vnmf::{format_uuid, human_size};

#[derive(Parser)]
#[command(name = "vnmfinfo", version, long_version = vnmf::LONG_VERSION, author = vnmf::AUTHORS, about = "Shows full information about a .vnmf file and checks its integrity")]
#[command(after_help = "Exit codes: 0 — the file is intact; 1 — the file is damaged (see vnmfrecovery); 2 — error.")]
struct Args {
    /// .vnmf file
    input: PathBuf,

    /// Quick: check only the metadata (both copies), without reading all the data and parity
    #[arg(long)]
    quick: bool,

    /// Deprecated: a full check is now performed by default
    #[arg(long, hide = true)]
    verify: bool,

    /// Output in JSON format
    #[arg(long)]
    json: bool,

    /// Don't print the page table
    #[arg(long)]
    brief: bool,

    /// Show the Info section in full (default — the first 20 lines)
    #[arg(long)]
    full_info: bool,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("vnmfinfo: error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Args) -> Result<bool> {
    let reader = match VnmfReader::open(&args.input) {
        Ok(reader) => reader,
        Err(open_err) => return report_unreadable(&args, &open_err.to_string()),
    };
    let file_size = fs::metadata(&args.input)?.len();
    let verification = if args.quick { None } else { Some(verify(&reader)?) };
    let health = assess(&reader, verification.as_ref());

    if args.json {
        print_json(&args, &reader, file_size, verification.as_ref(), &health)?;
    } else {
        print_text(&args, &reader, file_size, verification.as_ref(), &health);
    }
    Ok(health.is_ok())
}

/// The file can't be opened with either metadata copy: show what exactly is damaged and what to do.
fn report_unreadable(args: &Args, open_err: &str) -> Result<bool> {
    let bytes = fs::read(&args.input).with_context(|| format!("reading {}", args.input.display()))?;
    let copies = recovery::inspect_headers(&bytes);
    let has_signature = bytes.starts_with(&spec::MAGIC)
        || bytes.len().checked_sub(spec::HEADER_SIZE).is_some_and(|tail| bytes[tail..].starts_with(&spec::MAGIC));
    // Without signatures it's either a foreign file or a VNMF with both headers destroyed. Tell them apart by
    // contents: in VNMF images sit at positions that are multiples of 4 KiB — that's what recovery looks for.
    let carvable = if has_signature { 0 } else {
        recovery::carve(&bytes).iter().filter(|c| matches!(c.kind, recovery::CarvedKind::Image(_))).count()
    };
    if !has_signature && carvable == 0 {
        if args.json {
            let out = json!({ "file": args.input.display().to_string(), "readable": false, "vnmf": false, "error": open_err });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        anyhow::bail!("{}: not a VNMF file ({open_err})", args.input.display());
    }

    let describe = |header_ok: bool, tables: Option<&Result<(), vnmf_core::ReaderError>>| -> String {
        match (header_ok, tables) {
            (false, _) => "header damaged".into(),
            (true, Some(Err(e))) => format!("header intact, tables damaged ({e})"),
            (true, _) => "intact".into(),
        }
    };
    let primary = describe(copies.primary.is_ok(), Some(&copies.primary_tables));
    let tail = match &copies.tail {
        None => "none".to_string(),
        Some(t) => describe(t.is_ok(), copies.tail_tables.as_ref()),
    };
    let prognosis = if copies.primary.is_ok() || matches!(copies.tail, Some(Ok(_))) {
        "a header survived: the metadata can be assembled record by record or recovered with redundancy"
    } else {
        "both headers are lost: data can only be extracted by signatures"
    };

    if args.json {
        let out = json!({
            "file": args.input.display().to_string(),
            "file_size": bytes.len(),
            "readable": false,
            "error": open_err,
            "health": {
                "damaged": true,
                "issues": [format!("primary metadata copy: {primary}"), format!("backup metadata copy: {tail}")],
                "prognosis": prognosis,
                "recommendation": recommendation(args),
            },
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(false);
    }

    println!("File:           {} ({})", args.input.display(), human_size(bytes.len() as u64));
    if !has_signature {
        println!(
            "Format:         the \"vnmf\" signature was found neither at the start nor at the end, but {carvable} \
             images were found at aligned positions — looks like a VNMF with destroyed headers"
        );
    }
    println!("Error:          {open_err}");
    print_damage_box(&[
        format!("primary metadata copy: {primary}"),
        format!("backup metadata copy: {tail}"),
    ], prognosis, args);
    Ok(false)
}

fn recommendation(args: &Args) -> String {
    format!("vnmfrecovery {}", shell_quote(&args.input.display().to_string()))
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_alphanumeric() || "/._-+".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn print_damage_box(issues: &[String], prognosis: &str, args: &Args) {
    println!();
    println!("╔══ ⚠ FILE IS DAMAGED ═══════════════════════════════════════════");
    for issue in issues {
        println!("║  • {issue}");
    }
    println!("║");
    println!("║  Prognosis: {prognosis}");
    println!("║  Run recovery:  {}", recommendation(args));
    println!("╚════════════════════════════════════════════════════════════════");
}

fn print_text(args: &Args, r: &VnmfReader, file_size: u64, verification: Option<&Verify>, health: &Health) {
    let h = r.header();
    let flags = h.flags.get();
    let (major, minor) = r.version();
    let data_bytes: u64 = r.chunks().iter().map(|c| u64::from(c.length())).sum::<u64>()
        + r.previews().iter().map(|p| u64::from(p.length.get())).sum::<u64>();

    println!("File:           {} ({})", args.input.display(), human_size(file_size));
    match r.legacy_version() {
        Some(_) => println!("Format:         VNMF {major}.{minor} — old format, read-only (rebuild as {}.{} in VNMF Creator)", spec::DEFAULT_VERSION.0, spec::DEFAULT_VERSION.1),
        None => println!("Format:         VNMF {major}.{minor}"),
    }
    println!("UUID:           {}", format_uuid(r.uuid()));
    println!("Block size:     {}", r.block_size());
    println!("Flags:          {flags:#06x} {}", HeaderFlagsFormatter(flags));
    println!(
        "Status:         {}",
        match (health.is_ok(), health.data_checked) {
            (false, _) => format!(
                "⚠ DAMAGED ({} {}) — run {}",
                health.issues.len(),
                problems_word(health.issues.len()),
                recommendation(args)
            ),
            (true, true) => "✔ intact (metadata, data and parity checked)".to_string(),
            (true, false) => "✔ metadata intact (data not checked: --quick)".to_string(),
        }
    );
    println!("Backup copy:    {}", if r.has_flag(HeaderBitEnum::HasTailDup) { "yes" } else { "no" });
    match r.parity_layout() {
        Some(l) => println!(
            "Redundancy:     {}% Reed–Solomon — {} of parity; {} shards of {} in {} stripes ({}+{}); \
             recovers contiguous damage of up to {}",
            l.redundancy,
            human_size(l.len()),
            l.data_shards,
            human_size(l.shard_size as u64),
            l.stripes,
            l.per_stripe,
            l.parity_per_stripe,
            human_size(l.max_burst_bytes())
        ),
        None if r.legacy_version().is_some() && r.has_flag(HeaderBitEnum::HasParity) => {
            println!("Redundancy:     {}% (format 1.x — check and recovery not supported)", h.redundancy)
        }
        None => println!("Redundancy:     none"),
    }
    println!(
        "Data:           {} in {} chunks, overhead {} ({:.1}%)",
        human_size(data_bytes),
        r.chunks_count(),
        human_size(file_size.saturating_sub(data_bytes)),
        100.0 * file_size.saturating_sub(data_bytes) as f64 / file_size.max(1) as f64
    );

    // Page formats
    let mut formats: BTreeMap<String, usize> = BTreeMap::new();
    for page in r.pages() {
        *formats.entry(PageFormat::from(page.format).to_string()).or_default() += 1;
    }
    let formats: Vec<String> = formats.iter().map(|(f, n)| format!("{f} ×{n}")).collect();
    println!("Pages:          {} ({})", r.pages_count(), formats.join(", "));
    if !r.aux().is_empty() {
        let mut kinds: BTreeMap<AuxKind, usize> = BTreeMap::new();
        for a in r.aux() {
            *kinds.entry(a.kind()).or_default() += 1;
        }
        let pages = (0..r.pages_count()).filter(|&i| !r.page_aux(i).is_empty()).count();
        let kinds: Vec<String> = kinds.iter().map(|(k, n)| format!("{} ×{n}", k.label().to_lowercase())).collect();
        println!("Attachments:    {} on {pages} pages ({})", r.aux().len(), kinds.join(", "));
    }
    let toc = Toc::from_reader(r);
    if toc.is_structured() {
        let volumes = toc.volumes.iter().filter(|v| v.volume != 0).count();
        if volumes > 0 {
            println!("Structure:      volumes {volumes}, chapters {}", toc.chapters_count());
        } else {
            println!("Structure:      chapters {}", toc.chapters_count());
        }
    }

    // Info
    println!();
    match r.info_bytes() {
        None => println!("Info: none"),
        Some(info) => {
            let format = InfoFormat::detect(info);
            let meta = Metadata::from_info(Some(info));
            let kind = match (&meta, format) {
                (Some(_), InfoFormat::Json) if is_current_schema(info) => format!("VNMF metadata ({})", metadata::SCHEMA),
                (Some(_), InfoFormat::Json) => "ComicInfo in JSON (format 1.1, read as VNMF metadata)".to_string(),
                (Some(_), _) => "ComicInfo.xml".to_string(),
                (None, f) => f.to_string(),
            };
            println!("Info: {kind}, {}", human_size(info.len() as u64));
            if let Some(meta) = &meta {
                let width = meta.display_rows().iter().map(|(l, _)| l.chars().count()).max().unwrap_or(0);
                let mut last = "";
                for (label, value) in meta.display_rows() {
                    // Repeated labels (several people, identifiers) — without repeating the name.
                    let shown = if label == last { "" } else { label };
                    last = label;
                    let pad = " ".repeat(width.saturating_sub(shown.chars().count()));
                    let value = if label == "Description" && !args.full_info { shorten(&value, 160) } else { value };
                    println!("  {shown}{pad}  {value}");
                }
            }
            // Raw contents: for metadata — only with --full-info, for other formats — the beginning.
            if meta.is_none() || args.full_info {
                let text = String::from_utf8_lossy(info);
                let lines: Vec<&str> = text.trim().lines().collect();
                let shown = if args.full_info { lines.len() } else { lines.len().min(20) };
                for line in &lines[..shown] {
                    println!("  │ {line}");
                }
                if shown < lines.len() {
                    println!("  │ … {} more lines (--full-info)", lines.len() - shown);
                }
            }
        }
    }

    // Table of contents
    if toc.is_structured() {
        println!("\nContents:");
        let pages = |range: &std::ops::Range<usize>| {
            if range.len() == 1 { format!("p. {}", range.start + 1) } else { format!("pp. {}–{}", range.start + 1, range.end) }
        };
        for volume in &toc.volumes {
            let indent = if toc.has_volumes {
                println!("  {} ({})", volume.label(), pages(&volume.pages));
                "    "
            } else {
                "  "
            };
            for chapter in &volume.chapters {
                println!("{indent}{} ({})", chapter.label(), pages(&chapter.pages));
            }
        }
    }

    // Previews
    if r.previews_count() > 0 {
        println!("\nPreviews ({}):", r.previews_count());
        println!("  {:>3}  {:>4}  {:>11}  {:<7}  {:>10}", "#", "vol", "size", "format", "bytes");
        for (i, p) in r.previews().iter().enumerate() {
            println!(
                "  {:>3}  {:>4}  {:>11}  {:<7}  {:>10}",
                i,
                p.volume_id.get(),
                format!("{}x{}", p.width.get(), p.height.get()),
                PageFormat::from(p.format).to_string(),
                p.length.get()
            );
        }
    }

    // Pages
    if !args.brief && r.pages_count() > 0 {
        println!("\nPages ({}):", r.pages_count());
        println!(
            "  {:>4}  {:>3}  {:>6}  {:>4}  {:>11}  {:<7}  {:<14}  {:>5}  {:>10}  {:<13}  flags",
            "#", "vol", "chap", "page", "size", "format", "role", "chunk", "bytes", "attachments"
        );
        for (i, page) in r.pages().iter().enumerate() {
            let count = page.chunk_count.get() as usize;
            let bytes: u64 = r.chunks().get(page.chunk_range()).unwrap_or_default().iter().map(|c| u64::from(c.length())).sum();
            let (recorded, mismatch) = page_format(r, i, page);
            let format = if is_mixed(page) {
                chunk_formats(r, i).iter().map(ToString::to_string).collect::<Vec<_>>().join("+")
            } else {
                match mismatch {
                    Some(actual) => format!("{recorded}≠{actual}"),
                    None => recorded.to_string(),
                }
            };
            println!(
                "  {:>4}  {:>3}  {:>6}  {:>4}  {:>11}  {:<7}  {:<14}  {:>5}  {:>10}  {:<13}  {}",
                i + 1,
                if page.volume_id.get() == 0 { "-".to_string() } else { page.volume_id.get().to_string() },
                page.chapter_id.get(),
                page.page_number_in_chapter.get(),
                format!("{}x{}", page.width.get(), page.height.get()),
                format,
                role_name(page),
                count,
                bytes,
                aux_summary(r, i),
                PageBitFlags::names(page.flags.get()).join(",")
            );
        }
        if r.pages().iter().enumerate().any(|(i, p)| page_format(r, i, p).1.is_some()) {
            println!("  ≠ — the format in the table doesn't match the actual chunk contents");
        }
    }

    if let Some(v) = verification {
        println!();
        if v.bad.is_empty() {
            println!("Data CRC32: {}/{} OK", v.checked, v.checked);
        }
        if v.parity.as_ref().is_some_and(ParityStatus::is_clean) {
            println!("Redundancy: all blocks intact");
        }
    }

    if !health.is_ok() {
        print_damage_box(&health.issues, health.prognosis, args);
    }
}

fn print_json(args: &Args, r: &VnmfReader, file_size: u64, verification: Option<&Verify>, health: &Health) -> Result<()> {
    let flags = r.header().flags.get();
    let flag_names: Vec<String> =
        HeaderBitEnum::ALL.iter().filter(|b| b.is_set(flags)).map(|b| b.to_string()).collect();

    let info = r.info_bytes().map(|bytes| {
        let format = InfoFormat::detect(bytes);
        let content = match format {
            // Format 1.1 Info (ComicInfo + Contents) is shown already in the current schema.
            InfoFormat::Json => Metadata::from_info(Some(bytes))
                .and_then(|m| serde_json::to_value(&m).ok())
                .unwrap_or_else(|| serde_json::from_slice(bytes).unwrap_or(Value::Null)),
            InfoFormat::Binary => Value::Null,
            _ => Value::String(String::from_utf8_lossy(bytes).into_owned()),
        };
        json!({ "format": format.to_string(), "size": bytes.len(), "content": content })
    });

    let previews: Vec<Value> = r
        .previews()
        .iter()
        .map(|p| {
            json!({
                "volume_id": p.volume_id.get(),
                "width": p.width.get(),
                "height": p.height.get(),
                "format": PageFormat::from(p.format).to_string(),
                "offset": p.offset.get(),
                "length": p.length.get(),
                "crc32": format!("{:08x}", p.crc.get()),
            })
        })
        .collect();

    let pages: Vec<Value> = if args.brief {
        Vec::new()
    } else {
        r.pages()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (recorded, mismatch) = page_format(r, i, p);
                json!({
                    "index": i,
                    "volume_id": p.volume_id.get(),
                    "chapter_id": p.chapter_id.get(),
                    "page_number": p.page_number_in_chapter.get(),
                    "width": p.width.get(),
                    "height": p.height.get(),
                    "format": recorded.to_string(),
                    "detected_format": mismatch.map(|f| f.to_string()),
                    "chunk_formats": is_mixed(p).then(|| {
                        chunk_formats(r, i).iter().map(ToString::to_string).collect::<Vec<_>>()
                    }),
                    "role": role_name(p),
                    "flags": PageBitFlags::names(p.flags.get()),
                    "first_chunk": p.first_chunk_idx.get(),
                    "chunk_count": p.chunk_count.get(),
                    "aux": r.page_aux(i).iter().map(|a| json!({
                        "kind": a.kind().name(),
                        "format": AuxFormat::from(a.format).to_string(),
                        "chunk": a.chunk_idx.get(),
                        "bytes": r.chunks().get(a.chunk_idx.get() as usize).map(|c| c.length()),
                        "flags": AuxFlags::names(a.flags.get()),
                        "delay_ms": a.delay_ms.get(),
                        "trigger_y": a.trigger_y.get(),
                        "part": a.part.get(),
                        "duration_ms": a.duration_ms.get(),
                        "pages": a.pages.get(),
                        "volume": if a.volume == 0 { 100 } else { a.volume },
                    })).collect::<Vec<_>>(),
                })
            })
            .collect()
    };

    let out = json!({
        "file": args.input.display().to_string(),
        "file_size": file_size,
        "readable": true,
        "health": {
            "damaged": !health.is_ok(),
            "data_checked": health.data_checked,
            "issues": health.issues,
            "prognosis": (!health.is_ok()).then_some(health.prognosis),
            "recommendation": (!health.is_ok()).then(|| recommendation(args)),
        },
        "version": format!("{}.{}", r.version().0, r.version().1),
        "uuid": format_uuid(r.uuid()),
        "block_size": r.block_size().bytes(),
        "flags": flag_names,
        "recovered_from_tail": r.is_recovered(),
        "info": info,
        "previews": previews,
        "pages_count": r.pages_count(),
        "contents": toc_json(r),
        "chunks_count": r.chunks_count(),
        "pages": pages,
        "redundancy": r.redundancy(),
        "parity": r.parity_layout().map(|l| json!({
            "offset": l.offset,
            "size": l.len(),
            "shard_size": l.shard_size,
            "data_shards": l.data_shards,
            "stripes": l.stripes,
            "data_per_stripe": l.per_stripe,
            "parity_per_stripe": l.parity_per_stripe,
            "max_burst_bytes": l.max_burst_bytes(),
        })),
        "verify": verification.map(|v| json!({
            "checked": v.checked,
            "bad": v.bad,
            "parity": v.parity.as_ref().map(|p| json!({
                "clean": p.is_clean(),
                "recoverable": p.is_recoverable(),
                "bad_data_shards": p.bad_data.len(),
                "bad_parity_shards": p.bad_parity.len(),
                "damaged_table_blocks": p.damaged_table_blocks,
                "unknown": p.unknown,
            })),
        })),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// Contents for `--json`: volumes with chapters, page ranges (from 0, end exclusive).
fn toc_json(r: &VnmfReader) -> Value {
    let toc = Toc::from_reader(r);
    let volumes: Vec<Value> = toc
        .volumes
        .iter()
        .map(|v| {
            json!({
                "volume_id": v.volume,
                "title": v.title,
                "pages": [v.pages.start, v.pages.end],
                "chapters": v.chapters.iter().map(|c| json!({
                    "chapter_id": c.chapter,
                    "number": c.display_number(),
                    "title": c.title,
                    "pages": [c.pages.start, c.pages.end],
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "has_volumes": toc.has_volumes, "volumes": volumes })
}

fn is_current_schema(info: &[u8]) -> bool {
    serde_json::from_slice::<Value>(info)
        .ok()
        .and_then(|v| v.get("schema")?.as_str().map(|s| s.starts_with("vnmf-metadata/")))
        .unwrap_or(false)
}

/// The first `max` characters on a single line.
fn shorten(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}… (--full-info)", flat.chars().take(max).collect::<String>())
    }
}
