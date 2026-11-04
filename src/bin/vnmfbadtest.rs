// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! vnmfbadtest — deliberately corrupts any file to test recovery on real data.
//!
//! Knows nothing about the VNMF format: it just overwrites, truncates or flips bits.
//! Every run prints a seed — with it the damage can be reproduced exactly.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};

use vnmf::human_size;

#[derive(Parser)]
#[command(name = "vnmfbadtest", version, long_version = vnmf::LONG_VERSION, author = vnmf::AUTHORS, about = "Deliberately corrupts a file for recovery tests")]
#[command(after_help = "SIZE: bytes (4096), with a suffix (512K, 3M, 1G) or a percentage of the file (10%).\n\n\
Examples:\n  \
    vnmfbadtest vol.vnmf                          # random damage type → vol.bad.vnmf\n  \
    vnmfbadtest vol.vnmf -m burst --at start -s 3M\n  \
    vnmfbadtest vol.vnmf -m sectors -n 300 --fill zero\n  \
    vnmfbadtest vol.vnmf -m truncate -s 5%\n  \
    vnmfbadtest vol.vnmf --seed 42                # reproduce the previous damage")]
struct Args {
    /// File to corrupt (the original is not changed without --in-place)
    input: PathBuf,

    /// Damage type
    #[arg(short, long, value_enum, default_value_t = Mode::Random)]
    mode: Mode,

    /// Where to put the contiguous damage (for burst)
    #[arg(long, value_enum, default_value_t = At::Random)]
    at: At,

    /// Damage size: for burst — the length of the region, for truncate — how much to cut off
    #[arg(short, long, value_name = "SIZE")]
    size: Option<String>,

    /// Count: sectors for sectors, bits for bitflip
    #[arg(short = 'n', long)]
    count: Option<usize>,

    /// Sector size for sectors
    #[arg(long, default_value = "512", value_name = "SIZE")]
    sector: String,

    /// What to overwrite with
    #[arg(long, value_enum, default_value_t = Fill::Random)]
    fill: Fill,

    /// Generator seed (default — from the time; printed for reproduction)
    #[arg(long)]
    seed: Option<u64>,

    /// Where to write the corrupted file (default <name>.bad.<extension>)
    #[arg(short, long, conflicts_with = "in_place")]
    output: Option<PathBuf>,

    /// Corrupt the file itself
    #[arg(long)]
    in_place: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
enum Mode {
    /// Randomly pick one of the below
    Random,
    /// One contiguous region (a bad filesystem extent)
    Burst,
    /// Many scattered bad sectors
    Sectors,
    /// Cut off the end of the file (an interrupted write / copy)
    Truncate,
    /// Flip individual bits (silent memory/media corruption)
    Bitflip,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
enum At {
    Start,
    Middle,
    End,
    Random,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
enum Fill {
    /// Zeros (like an uninitialized block)
    Zero,
    /// Random bytes (like foreign data)
    Random,
}

/// SplitMix64 — a small deterministic generator, to avoid pulling in `rand` for a test tool.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (n > 0).
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn fill(&mut self, buf: &mut [u8], how: Fill) {
        match how {
            Fill::Zero => buf.fill(0),
            Fill::Random => buf.iter_mut().for_each(|b| *b = self.next() as u8),
        }
    }
}

/// `4096`, `512K`, `3M`, `1G` or `10%` of `total`.
fn parse_size(s: &str, total: usize) -> Result<usize> {
    let s = s.trim();
    if let Some(pct) = s.strip_suffix('%') {
        let pct: f64 = pct.trim().parse().with_context(|| format!("invalid percentage: {s}"))?;
        if !(0.0..=100.0).contains(&pct) {
            bail!("percentage must be 0–100: {s}");
        }
        return Ok((total as f64 * pct / 100.0) as usize);
    }
    let (num, mult) = match s.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&s[..s.len() - 1], 1 << 10),
        Some('M') => (&s[..s.len() - 1], 1 << 20),
        Some('G') => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    let n: usize = num.trim().parse().with_context(|| format!("invalid size: {s}"))?;
    Ok(n * mult)
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vnmfbadtest: error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<()> {
    let mut data = fs::read(&args.input).with_context(|| format!("reading {}", args.input.display()))?;
    if data.is_empty() {
        bail!("the file is empty — nothing to corrupt");
    }
    let len = data.len();

    // Validate the arguments right away, even if random mode won't use them.
    if let Some(size) = &args.size {
        parse_size(size, len)?;
    }
    parse_size(&args.sector, len)?;

    let seed = args.seed.unwrap_or_else(|| {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)
    });
    let mut rng = Rng(seed);

    let mode = match args.mode {
        Mode::Random => [Mode::Burst, Mode::Sectors, Mode::Truncate, Mode::Bitflip][rng.below(4)],
        m => m,
    };

    let size = |default_pct: f64, rng: &mut Rng| -> Result<usize> {
        match &args.size {
            Some(s) => parse_size(s, len),
            // Without --size — randomly from 1% to default_pct% of the file.
            None => Ok((len as f64 * (1.0 + rng.below(default_pct as usize) as f64) / 100.0) as usize),
        }
    };

    let description = match mode {
        Mode::Burst => {
            let n = size(10.0, &mut rng)?.clamp(1, len);
            let at = if args.at == At::Random { [At::Start, At::Middle, At::End, At::Random][rng.below(4)] } else { args.at };
            let start = match at {
                At::Start => 0,
                At::Middle => (len - n) / 2,
                At::End => len - n,
                At::Random => rng.below(len - n + 1),
            };
            rng.fill(&mut data[start..start + n], args.fill);
            format!("contiguous region {} ({n} B) at offset {start:#x} ({:?})", human_size(n as u64), at)
        }
        Mode::Sectors => {
            let sector = parse_size(&args.sector, len)?.clamp(1, len);
            let count = args.count.unwrap_or_else(|| 10 + rng.below(300));
            let mut offsets = Vec::with_capacity(count);
            for _ in 0..count {
                // Sectors are aligned, as on a real medium.
                let start = rng.below(len.div_ceil(sector)) * sector;
                let end = (start + sector).min(len);
                rng.fill(&mut data[start..end], args.fill);
                offsets.push(start);
            }
            offsets.sort_unstable();
            offsets.dedup();
            let preview: Vec<String> = offsets.iter().take(8).map(|o| format!("{o:#x}")).collect();
            format!(
                "{} bad sectors of {} (total {}): {}{}",
                offsets.len(),
                human_size(sector as u64),
                human_size((offsets.len() * sector) as u64),
                preview.join(", "),
                if offsets.len() > 8 { ", …" } else { "" }
            )
        }
        Mode::Truncate => {
            let n = size(10.0, &mut rng)?.clamp(1, len - 1);
            data.truncate(len - n);
            format!("cut off {} ({n} B) from the end: {len} → {} B", human_size(n as u64), data.len())
        }
        Mode::Bitflip => {
            let count = args.count.unwrap_or_else(|| 1 + rng.below(64));
            let mut flips = Vec::with_capacity(count);
            for _ in 0..count {
                let pos = rng.below(len);
                let bit = rng.below(8);
                data[pos] ^= 1 << bit;
                flips.push(format!("{pos:#x}:{bit}"));
            }
            let shown = flips.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
            format!("flipped {count} bit(s): {shown}{}", if count > 8 { ", …" } else { "" })
        }
        Mode::Random => unreachable!(),
    };

    let output = if args.in_place {
        args.input.clone()
    } else {
        args.output.clone().unwrap_or_else(|| {
            let stem = args.input.file_stem().map_or("file".into(), |s| s.to_string_lossy().into_owned());
            let name = match args.input.extension() {
                Some(ext) => format!("{stem}.bad.{}", ext.to_string_lossy()),
                None => format!("{stem}.bad"),
            };
            args.input.with_file_name(name)
        })
    };
    fs::write(&output, &data).with_context(|| format!("writing {}", output.display()))?;

    println!("{} → {}", args.input.display(), output.display());
    println!("  {mode:?}: {description}");
    println!("  seed: {seed}  (repeat: --seed {seed} -m {})", format!("{mode:?}").to_lowercase());
    Ok(())
}
