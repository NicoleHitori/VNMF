// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! vnmfunpack — unpacks .vnmf into a CBZ or a folder.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use vnmf_core::VnmfReader;

use vnmf::human_size;
use vnmf::unpack::{DirSink, Sink, ZipSink, unpack};

#[derive(Parser)]
#[command(name = "vnmfunpack", version, long_version = vnmf::LONG_VERSION, author = vnmf::AUTHORS, about = "Unpacks .vnmf into a CBZ or a folder")]
#[command(after_help = "Examples:\n  \
    vnmfunpack volume1.vnmf                 # → folder volume1/\n  \
    vnmfunpack volume1.vnmf -o volume1.cbz  # → CBZ (format by extension)\n  \
    vnmfunpack volume1.vnmf --format cbz\n\n\
A multi-chapter file is unpacked into folders: vol01/ch0001/… (with volumes) or ch0001/….\n\
Metadata — vnmf.json (full) and ComicInfo.xml (for other readers). vnmfpack packs such a folder back.")]
struct Args {
    /// .vnmf file
    input: PathBuf,

    /// Where to unpack: a folder or a .cbz file
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Output format. Default: cbz if -o ends with .cbz/.zip, otherwise a folder
    #[arg(long, value_enum)]
    format: Option<OutFormat>,

    /// Don't verify chunk CRC32
    #[arg(long)]
    no_verify: bool,

    /// Overwrite the output file / a non-empty folder
    #[arg(short, long)]
    force: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutFormat {
    Cbz,
    Dir,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vnmfunpack: error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<()> {
    let reader = VnmfReader::open(&args.input).with_context(|| format!("opening {}", args.input.display()))?;
    if reader.is_recovered() {
        eprintln!("warning: the primary metadata is damaged, the backup copy from the end of the file was used");
    }

    let format = args.format.unwrap_or_else(|| match &args.output {
        Some(p) if has_ext(p, &["cbz", "zip"]) => OutFormat::Cbz,
        _ => OutFormat::Dir,
    });
    let output = args.output.clone().unwrap_or_else(|| {
        let stem = vnmf::input::file_stem(&args.input);
        let name = if format == OutFormat::Cbz { format!("{stem}.cbz") } else { stem };
        args.input.with_file_name(name)
    });

    let mut sink: Box<dyn Sink> = match format {
        OutFormat::Dir => {
            if output.exists() && fs::read_dir(&output)?.next().is_some() && !args.force {
                bail!("folder {} is not empty (add --force)", output.display());
            }
            fs::create_dir_all(&output)?;
            Box::new(DirSink { root: output.clone() })
        }
        OutFormat::Cbz => {
            if output.exists() && !args.force {
                bail!("{} already exists (add --force)", output.display());
            }
            Box::new(ZipSink::create(output.clone())?)
        }
    };

    let result = unpack(&reader, sink.as_mut(), format == OutFormat::Dir, !args.no_verify);
    let finished = result.and_then(|stats| sink.finish().map(|()| stats));
    let stats = match finished {
        Ok(stats) => stats,
        Err(e) => {
            if format == OutFormat::Cbz {
                let mut tmp = output.clone().into_os_string();
                tmp.push(".tmp");
                let _ = fs::remove_file(tmp);
            }
            return Err(e);
        }
    };

    println!("{}: {} files, {}", output.display(), stats.files, human_size(stats.bytes));
    for note in stats.notes {
        println!("  note: {note}");
    }
    Ok(())
}

fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}
