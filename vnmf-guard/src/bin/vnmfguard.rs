// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! vnmfguard — unobtrusive Reed–Solomon redundancy in video, audio and disk images.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use vnmf_guard::vnmf_parity::{BlockSize, MAX_REDUNDANCY};
use vnmf_guard::{Format, Inspection, Options, ProtectOutcome, RepairOutcome, StripOutcome, human_size};

#[derive(Parser)]
#[command(name = "vnmfguard", version, author = "void_nicole (Nicole Hitori) + AI-assist")]
#[command(about = "Unobtrusive Reed–Solomon redundancy in video, audio and disk images")]
#[command(after_help = "Parity is placed inside the format's structure, so ordinary programs open \
the file as usual. Damaged regions (bad sectors, a truncated end) are restored to their state right after protection.\n\n\
Exit codes: 0 — all good; 1 — the file is damaged (check), not fully repaired (repair) or not protected; 2 — error.")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add parity (an already protected file is re-protected with the new parameters)
    Protect {
        files: Vec<PathBuf>,
        /// Redundancy, % (0 — change nothing)
        #[arg(short, long, default_value_t = 10, value_parser = clap::value_parser!(u8).range(0..=MAX_REDUNDANCY as i64))]
        redundancy: u8,
        /// Block size (parity shard)
        #[arg(long, value_enum, default_value_t = Block::K4)]
        block: Block,
        /// Allow an unknown format: parity simply at the end (programs may notice it)
        #[arg(long)]
        raw: bool,
        /// Write the result here (only for a single file); otherwise in place
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Check integrity
    Check { files: Vec<PathBuf> },
    /// Repair damaged regions
    Repair {
        files: Vec<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Remove protection (return the file without parity)
    Strip {
        files: Vec<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Supported formats and where the parity goes in them
    Formats,
}

#[derive(Clone, Copy, ValueEnum)]
enum Block {
    #[value(name = "4k")]
    K4,
    #[value(name = "8k")]
    K8,
    #[value(name = "16k")]
    K16,
    #[value(name = "32k")]
    K32,
}

impl From<Block> for BlockSize {
    fn from(b: Block) -> Self {
        match b {
            Block::K4 => BlockSize::Page4k,
            Block::K8 => BlockSize::Page8k,
            Block::K16 => BlockSize::Page16k,
            Block::K32 => BlockSize::Ext32k,
        }
    }
}

/// Summary for a single file (for the exit code).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    Ok,
    Problem,
    Error,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let (files, output) = match &args.command {
        Command::Protect { files, output, .. }
        | Command::Repair { files, output }
        | Command::Strip { files, output } => (files.clone(), output.clone()),
        Command::Check { files } => (files.clone(), None),
        Command::Formats => {
            print_formats();
            return ExitCode::SUCCESS;
        }
    };
    if files.is_empty() {
        eprintln!("vnmfguard: no files given");
        return ExitCode::from(2);
    }
    if output.is_some() && files.len() > 1 {
        eprintln!("vnmfguard: --output can only be given for a single file");
        return ExitCode::from(2);
    }

    let mut worst = Outcome::Ok;
    for file in &files {
        let result = match &args.command {
            Command::Protect { redundancy, block, raw, .. } => {
                let opts = Options { redundancy: *redundancy, block: (*block).into(), allow_raw: *raw };
                protect(file, output.as_deref(), &opts)
            }
            Command::Check { .. } => check(file),
            Command::Repair { .. } => repair(file, output.as_deref()),
            Command::Strip { .. } => strip(file, output.as_deref()),
            Command::Formats => unreachable!(),
        };
        let outcome = result.unwrap_or_else(|e| {
            eprintln!("{}: error: {e:#}", file.display());
            Outcome::Error
        });
        worst = worst.max(outcome);
    }
    match worst {
        Outcome::Ok => ExitCode::SUCCESS,
        Outcome::Problem => ExitCode::from(1),
        Outcome::Error => ExitCode::from(2),
    }
}

fn print_formats() {
    println!("Format               Extensions        Where the parity goes");
    for f in Format::ALL {
        let ext = if f.extensions().is_empty() { "(--raw)".to_string() } else { f.extensions().join(", ") };
        println!("{:<20} {:<17} {}", f.name(), ext, f.placement());
    }
    println!("\n.img images are better compressed to .zst or .gz; .xz has no room for foreign data.");
}

fn protect(file: &Path, output: Option<&Path>, opts: &Options) -> Result<Outcome> {
    match vnmf_guard::protect(file, output, opts)? {
        ProtectOutcome::Nothing => println!("{}: 0% — nothing changed", file.display()),
        ProtectOutcome::AlreadyProtected(r) => println!("{}: already protected ({r}%)", file.display()),
        ProtectOutcome::Protected { plan, original_len, new_len, previous } => {
            let layout = plan.layout().expect("layout of a protected file");
            let again = previous.map(|p| format!(" (was {p}%)")).unwrap_or_default();
            println!("{}: protected {}%{again} — {}", file.display(), plan.redundancy, plan.format.name());
            println!(
                "  {} → {} (+{}), recovers a contiguous region of up to {}",
                human_size(original_len),
                human_size(new_len),
                human_size(new_len.saturating_sub(original_len)),
                human_size(layout.max_burst_bytes())
            );
            if let Some(out) = output {
                println!("  written to {}", out.display());
            }
        }
    }
    Ok(Outcome::Ok)
}

fn print_inspection(i: &Inspection) {
    let total = i.layout.data_shards;
    println!(
        "  data blocks: {total} of {}, damaged {}; parity: {}, damaged {}",
        human_size(i.layout.shard_size as u64),
        i.status.bad_data.len(),
        i.layout.parity_shards(),
        i.status.bad_parity.len()
    );
    if i.maps_intact < i.maps_total {
        println!("  intact map copies: {} of {}", i.maps_intact, i.maps_total);
    }
    if i.bad_page_crcs > 0 {
        println!("  wrong Ogg page CRCs: {}", i.bad_page_crcs);
    }
    if i.status.unknown > 0 {
        println!("  blocks with unknown CRC (table damaged): {}", i.status.unknown);
    }
    if !i.is_recoverable() {
        println!(
            "  ⚠ {} stripe(s) of {} can't be recovered: more damage than parity",
            i.status.unrecoverable_stripes.len(),
            i.layout.stripes
        );
    }
}

fn check(file: &Path) -> Result<Outcome> {
    let Some(i) = vnmf_guard::inspect(file)? else {
        println!("{}: not protected", file.display());
        return Ok(Outcome::Problem);
    };
    println!("{}: {}", file.display(), i.summary());
    if i.is_clean() {
        return Ok(Outcome::Ok);
    }
    print_inspection(&i);
    Ok(Outcome::Problem)
}

fn repair(file: &Path, output: Option<&Path>) -> Result<Outcome> {
    match vnmf_guard::repair(file, output)? {
        RepairOutcome::Unprotected => {
            println!("{}: not protected — nothing to recover with", file.display());
            Ok(Outcome::Problem)
        }
        RepairOutcome::Intact(i) => {
            println!("{}: {} — no repair needed", file.display(), i.summary());
            Ok(Outcome::Ok)
        }
        RepairOutcome::Repaired { before, report, target } => {
            println!("{}: {}", file.display(), before.summary());
            print_inspection(&before);
            let complete = report.as_ref().is_none_or(|r| r.is_complete());
            match &report {
                Some(r) => println!("  blocks repaired: data {}, parity {}", r.repaired_data, r.repaired_parity),
                None => println!("  data intact — only auxiliary fields were rewritten"),
            }
            if complete {
                println!("  ✓ fully repaired → {}", target.display());
                Ok(Outcome::Ok)
            } else {
                println!("  ⚠ partially repaired → {}", target.display());
                Ok(Outcome::Problem)
            }
        }
    }
}

fn strip(file: &Path, output: Option<&Path>) -> Result<Outcome> {
    match vnmf_guard::strip(file, output)? {
        StripOutcome::Unprotected => {
            println!("{}: not protected", file.display());
            Ok(Outcome::Problem)
        }
        StripOutcome::Stripped { identical, len } => {
            let note = if identical {
                "identical to the original byte for byte"
            } else {
                "the contents are the same, but the bytes differ from the original"
            };
            println!("{}: protection removed — {} ({note})", file.display(), human_size(len));
            Ok(if identical { Outcome::Ok } else { Outcome::Problem })
        }
    }
}
