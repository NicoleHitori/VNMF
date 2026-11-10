// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! vnmfpack — creates .vnmf from CBZ, folders or individual images.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use vnmf_core::{BlockSize, InfoFormat, VnmfReader};

use vnmf::comicinfo;
use vnmf::contents::Toc;
use vnmf::human_size;
use vnmf::input;
use vnmf::metadata::Metadata;
use vnmf::pack::{self, Cover, InfoSource, PackOptions};

#[derive(Parser)]
#[command(name = "vnmfpack", version, long_version = vnmf::LONG_VERSION, author = vnmf::AUTHORS, about = "Creates .vnmf from CBZ, folders or individual images")]
#[command(after_help = "Examples:\n  \
    vnmfpack chapter01.cbz\n  \
    vnmfpack ch1.cbz ch2.cbz ch3/ -o volume1.vnmf --cover first\n  \
    vnmfpack series/ -o series.vnmf          # series/Volume 1/Chapter 1/*.png → volumes and chapters\n  \
    vnmfpack \"Vol.1 Ch.1.cbz\" \"Vol.1 Ch.2.cbz\" \"Vol.2 Ch.3.cbz\"\n  \
    vnmfpack ch*.cbz --volume 3              # all chapters — volume 3\n  \
    vnmfpack scans/*.png --webtoon -o episode.vnmf\n  \
    vnmfpack episode.cbz --webtoon --mixed-formats  # JPEG + PNG in one episode\n  \
    vnmfpack volume1/ --redundancy 10        # protection against filesystem faults\n\n\
Page attachments (sound, text, dark versions) go in the input's aux/ folder:\n  \
    aux/003.music.ogg   music from page 3 (until the next music or the end of the chapter)\n  \
    aux/003.sfx.wav     sound effect of page 3 (another one — 003.sfx2.wav)\n  \
    aux/003.text.txt    page text (UTF-8 or vnmf-text/1 JSON)\n  \
    aux/003.dark.png    dark version of the page\n  \
    aux/aux.json        parameters: {\"files\": {\"003.music.ogg\": {\"loop\": true, \"pages\": 4, \"duration_ms\": 60000}}}\n\
The number is the ordinal of the image among all images in the folder; that's how vnmfunpack creates them.")]
struct Args {
    /// Input: .cbz, folders or images. Each CBZ and each folder is a separate chapter,
    /// consecutive individual files form one chapter. A folder with subfolders gives several chapters (subfolders) and
    /// volumes (the folders above them). The type is detected by contents, not by extension.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Output file. Default — the name of the first input with the .vnmf extension
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Volume for chapters that don't specify one (in the name `Vol.N`/`Volume N`, the folder or ComicInfo <Volume>)
    #[arg(long, value_name = "N")]
    volume: Option<u16>,

    /// Webtoon: each chapter becomes one long page made of chunks
    #[arg(long)]
    webtoon: bool,

    /// Allow different formats (JPEG, PNG, WebP, AVIF, JPEG XL, GIF) in one webtoon page.
    /// Without this flag all fragments of an episode must have the same format
    #[arg(long, requires = "webtoon")]
    mixed_formats: bool,

    /// Right-to-left reading (also enabled automatically by ComicInfo <Manga>YesAndRightToLeft)
    #[arg(long)]
    rtl: bool,

    /// Alignment block size
    #[arg(long, value_enum, default_value_t = BlockArg::K4)]
    block_size: BlockArg,

    /// Metadata file: vnmf.json, ComicInfo.xml (converted to VNMF metadata), JSON, TOML
    /// or text; replaces the metadata from the archives
    #[arg(long)]
    info: Option<PathBuf>,

    /// Cover preview: a path to an image or `first` (the first page)
    #[arg(long, value_name = "PATH|first")]
    cover: Option<String>,

    /// Reed–Solomon redundancy in % (0 — off, maximum 30). Allows recovering
    /// a damaged contiguous region of the file of roughly the same size (`vnmfrecovery`)
    #[arg(short, long, default_value_t = 0, value_name = "0-30",
          value_parser = clap::value_parser!(u8).range(0..=vnmf_core::spec::MAX_REDUNDANCY as i64))]
    redundancy: u8,

    /// Don't write the backup copy of the tables at the end of the file (ignored with --redundancy > 0)
    #[arg(long)]
    no_tail_dup: bool,

    /// Overwrite the output file if it exists
    #[arg(short, long)]
    force: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum BlockArg {
    #[value(name = "4k")]
    K4,
    #[value(name = "8k")]
    K8,
    #[value(name = "16k")]
    K16,
    #[value(name = "32k")]
    K32,
}

impl From<BlockArg> for BlockSize {
    fn from(b: BlockArg) -> Self {
        match b {
            BlockArg::K4 => BlockSize::Page4k,
            BlockArg::K8 => BlockSize::Page8k,
            BlockArg::K16 => BlockSize::Page16k,
            BlockArg::K32 => BlockSize::Ext32k,
        }
    }
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vnmfpack: error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<()> {
    let output = args.output.clone().unwrap_or_else(|| default_output(&args.inputs[0]));
    if output.exists() && !args.force {
        bail!("{} already exists (add --force to overwrite)", output.display());
    }

    let chapters = input::load_inputs(&args.inputs)?;
    let info = args.info.as_deref().map(load_info_file).transpose()?;
    let cover = match args.cover.as_deref() {
        Some("first") => Cover::First,
        Some(path) => Cover::File(PathBuf::from(path)),
        None => Cover::None,
    };
    let options = PackOptions {
        volume: args.volume,
        webtoon: args.webtoon,
        mixed_formats: args.mixed_formats,
        rtl: args.rtl,
        block_size: args.block_size.into(),
        redundancy: args.redundancy,
        tail_duplicate: !args.no_tail_dup,
        info,
        cover,
        uuid: None,
    };
    let packed = pack::pack(chapters, options)?;
    for warning in &packed.warnings {
        eprintln!("warning: {warning}");
    }
    packed.builder.build(&output).with_context(|| format!("writing {}", output.display()))?;

    // A short summary — re-read the finished file to show the actual result.
    let reader = VnmfReader::open(&output)?;
    let size = fs::metadata(&output)?.len();
    println!(
        "{}: {} pages, {} chunks, {} previews, {}",
        output.display(),
        reader.pages_count(),
        reader.chunks_count(),
        reader.previews_count(),
        human_size(size)
    );
    let toc = Toc::from_reader(&reader);
    if toc.is_structured() {
        let volumes = toc.volumes.iter().filter(|v| v.volume != 0).count();
        let mut line = format!("  chapters: {}", toc.chapters_count());
        if volumes > 0 {
            line = format!("  volumes: {volumes}, chapters: {}", toc.chapters_count());
        }
        println!("{line}");
    }
    if let Some(layout) = reader.parity_layout() {
        println!(
            "  redundancy {}%: +{} of parity, recovers contiguous damage of up to {}",
            layout.redundancy,
            human_size(layout.len()),
            human_size(layout.max_burst_bytes())
        );
    }
    Ok(())
}

/// File from --info: VNMF metadata (`vnmf.json`), ComicInfo.xml, format 1.1 JSON — reduced to
/// VNMF metadata; TOML, text and arbitrary JSON are stored as is.
fn load_info_file(path: &Path) -> Result<InfoSource> {
    let data = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    Ok(match InfoFormat::detect(&data) {
        InfoFormat::Xml => {
            let xml = String::from_utf8(data).context("XML is not UTF-8")?;
            let json = comicinfo::xml_to_json(&xml).with_context(|| format!("{}: invalid XML", path.display()))?;
            if !comicinfo::is_comicinfo(&json) {
                bail!("{}: XML without a <ComicInfo> root — ComicInfo.xml was expected", path.display());
            }
            InfoSource::Meta(Box::new(Metadata::from_comicinfo(&json)))
        }
        InfoFormat::Json => {
            serde_json::from_slice::<serde_json::Value>(&data).with_context(|| format!("{}: invalid JSON", path.display()))?;
            InfoSource::from_bytes(data)
        }
        InfoFormat::Binary => bail!("{}: metadata must be text", path.display()),
        InfoFormat::Toml | InfoFormat::Text => InfoSource::Raw(data),
    })
}

fn default_output(first_input: &Path) -> PathBuf {
    let name = format!("{}.vnmf", input::file_stem(first_input));
    match first_input.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}
