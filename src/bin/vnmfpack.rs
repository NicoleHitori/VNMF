// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! vnmfpack — creates .vnmf from CBZ, folders or individual images.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use serde_json::Value;
use vnmf_core::{BlockSize, Builder, ChunkSource, InfoFormat, PageBitFlags, PageFormat, PageRole, VnmfReader};

use vnmf::comicinfo;
use vnmf::contents::{self, Contents, Toc};
use vnmf::human_size;
use vnmf::input::{self, Chapter};
use vnmf::metadata::Metadata;

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
    let total_pages: usize = chapters.iter().map(|c| c.pages.len()).sum();
    if total_pages == 0 {
        bail!("no images found");
    }
    for ch in &chapters {
        if !ch.skipped.is_empty() {
            eprintln!("warning: {}: skipped {} file(s): {}", ch.name, ch.skipped.len(), ch.skipped.join(", "));
        }
    }

    let ids = assign_ids(&chapters, args.volume);
    let contents = chapters_contents(&chapters, &ids);

    let mut builder = Builder::new()
        .with_uuid(uuid::Uuid::new_v4().into_bytes())
        .with_block_size(args.block_size.into())
        .with_tail_duplicate(!args.no_tail_dup)
        .with_redundancy(args.redundancy)
        .with_mixed_formats(args.mixed_formats);

    // --- Metadata ---
    let info = match &args.info {
        Some(path) => load_info_file(path)?,
        None => chapters_info(&chapters, &ids),
    };
    // Direction: --rtl; otherwise as set in the metadata (it takes precedence over the chapters' ComicInfo);
    // otherwise <Manga>YesAndRightToLeft</Manga> in the ComicInfo of any chapter.
    let rtl = args.rtl
        || match &info {
            Some(InfoSource::Meta(m)) if m.reading_direction.is_some() => m.is_rtl(),
            _ => chapters.iter().any(|c| c.comic_info.as_ref().is_some_and(comicinfo::is_rtl)),
        };
    match info {
        // VNMF metadata: volume and chapter titles, reading direction and mode — all together.
        Some(InfoSource::Meta(meta)) => {
            let mut meta = *meta;
            let mut merged = meta.contents.clone();
            merged.merge(&contents);
            meta.contents = merged;
            if rtl && meta.reading_direction.is_none() {
                meta.reading_direction = Some("rtl".into());
            }
            if args.webtoon && meta.layout.is_none() {
                meta.layout = Some("strip".into());
            }
            builder.set_info(meta.to_bytes());
        }
        // TOML / text / foreign JSON — as is; chapter titles have nowhere to go then.
        Some(InfoSource::Raw(bytes)) => {
            if !contents.is_empty() {
                eprintln!("warning: Info is not VNMF metadata — volume and chapter titles were not saved");
            }
            builder.set_info(bytes);
        }
        None => {
            let mut meta = Metadata::new();
            meta.contents = contents;
            if rtl {
                meta.reading_direction = Some("rtl".into());
            }
            if args.webtoon {
                meta.layout = Some("strip".into());
            }
            if !meta.is_empty() {
                builder.set_info(meta.to_bytes());
            }
        }
    }

    // --- Cover ---
    match args.cover.as_deref() {
        Some("first") => {
            let first = &chapters.iter().find(|c| !c.pages.is_empty()).unwrap().pages[0];
            builder.add_preview(first.source.clone(), 0, 0)?;
        }
        Some(path) => builder.add_preview_file(path, 0, 0).with_context(|| format!("cover {path}"))?,
        None => {}
    }

    // Previews from `previews/` folders (after `vnmfunpack` into a folder).
    for chapter in &chapters {
        for (volume, source) in &chapter.previews {
            builder.add_preview(source.clone(), *volume, 0)?;
        }
    }

    // --- Pages ---
    let base_flags = if rtl { PageBitFlags::READING_RTL } else { 0 };
    for (chapter, (volume_id, chapter_id)) in chapters.into_iter().zip(ids) {
        if args.webtoon && !args.mixed_formats {
            check_uniform_formats(&chapter)?;
        }
        builder.set_volume(volume_id);
        add_chapter(&mut builder, chapter, chapter_id, base_flags, args.webtoon)?;
    }

    builder.build(&output).with_context(|| format!("writing {}", output.display()))?;

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

/// A clear error with a hint if webtoon fragments have different formats.
fn check_uniform_formats(chapter: &Chapter) -> Result<()> {
    let mut counts: Vec<(PageFormat, usize, &str)> = Vec::new();
    for page in &chapter.pages {
        match counts.iter_mut().find(|(f, _, _)| *f == page.format) {
            Some((_, n, _)) => *n += 1,
            None => counts.push((page.format, 1, &page.name)),
        }
    }
    if counts.len() > 1 {
        let list: Vec<String> = counts.iter().map(|(f, n, first)| format!("{f} ×{n} (e.g. {first})")).collect();
        bail!(
            "chapter {}: webtoon fragments have different formats: {}.\n\
             Add --mixed-formats to allow mixed formats in one page",
            chapter.name,
            list.join(", ")
        );
    }
    Ok(())
}

fn add_chapter(builder: &mut Builder, chapter: Chapter, chapter_id: u16, flags: u16, webtoon: bool) -> Result<()> {
    let name = chapter.name.clone();
    let start_flags = flags | PageBitFlags::IS_CHAPTER_START;

    if webtoon {
        // A chapter is one long page: all its attachments belong to it.
        let sources: Vec<ChunkSource> = chapter.pages.into_iter().map(|p| p.source).collect();
        builder.add_webtoon(sources, chapter_id, 1, start_flags).with_context(|| format!("chapter {name}"))?;
        for (_, aux) in chapter.aux {
            builder.attach_aux(aux).with_context(|| format!("chapter {name}: attachment"))?;
        }
        return Ok(());
    }

    let roles = chapter.comic_info.as_ref().map(comicinfo::page_roles).unwrap_or_default();
    let mut aux = chapter.aux;
    for (i, page) in chapter.pages.into_iter().enumerate() {
        let role = roles.iter().find(|(idx, _)| *idx == i).map_or(PageRole::Normal, |(_, r)| *r);
        let page_number = u16::try_from(i + 1).context("too many pages in a chapter")?;
        let page_flags = if i == 0 { start_flags } else { flags };
        builder.add_page(page.source, role, chapter_id, page_number, page_flags).with_context(|| format!("{name}: {}", page.name))?;
        for (_, entry) in aux.extract_if(.., |(idx, _)| *idx == i) {
            builder.attach_aux(entry).with_context(|| format!("{name}: attachment of page {}", i + 1))?;
        }
    }
    for (idx, _) in aux {
        eprintln!("warning: {name}: attachment for a nonexistent page {} skipped", idx + 1);
    }
    Ok(())
}

/// Volume and number of each chapter.
///
/// Volume — found in the name / folder / ComicInfo, otherwise `--volume`, otherwise the previous
/// chapter's volume (0 — no volume). Number — see [`contents::assign_chapter_ids`].
fn assign_ids(chapters: &[Chapter], default_volume: Option<u16>) -> Vec<(u16, u16)> {
    let mut volume = default_volume.unwrap_or(0);
    let volumes: Vec<u16> = chapters
        .iter()
        .map(|c| {
            volume = c.volume.or(default_volume).unwrap_or(volume);
            volume
        })
        .collect();
    let numbers: Vec<Option<String>> = chapters.iter().map(|c| c.number.clone()).collect();
    let ids = contents::assign_chapter_ids(&volumes, &numbers);
    volumes.into_iter().zip(ids).collect()
}

/// Volume and chapter titles: from folder names / ComicInfo, overridden by `contents.json` (after vnmfunpack).
fn chapters_contents(chapters: &[Chapter], ids: &[(u16, u16)]) -> Contents {
    let mut contents = Contents::default();
    for (ch, &(volume, chapter)) in chapters.iter().zip(ids) {
        contents.set_chapter(volume, chapter, ch.number.clone(), ch.title.clone());
        if ch.volume_title.is_some() {
            contents.set_volume_title(volume, ch.volume_title.clone());
        }
    }
    for saved in chapters.iter().filter_map(|c| c.contents.as_ref()) {
        contents.merge(saved);
    }
    contents
}

/// What goes into Info.
enum InfoSource {
    /// VNMF metadata (including that converted from ComicInfo or format 1.1 Info).
    Meta(Box<Metadata>),
    /// Another format (TOML, text, arbitrary JSON) — stored as is.
    Raw(Vec<u8>),
}

/// Info from the chapters: `vnmf.json`, otherwise ComicInfo, otherwise an info file — from the first chapter that has one.
///
/// For a multi-chapter file ComicInfo describes the whole series: single-chapter fields (`Title`,
/// `Number`, `Pages`…) are removed — chapter numbers and titles go into the table of contents.
fn chapters_info(chapters: &[Chapter], ids: &[(u16, u16)]) -> Option<InfoSource> {
    if let Some(meta) = chapters.iter().find_map(|c| c.metadata.clone()) {
        return Some(InfoSource::Meta(Box::new(meta)));
    }
    let first = chapters.iter().find(|c| c.comic_info.is_some() || c.info.is_some())?;
    Some(match &first.comic_info {
        Some(json) => {
            let mut json = json.clone();
            if chapters.len() > 1 {
                let one_volume = ids.windows(2).all(|w| w[0].0 == w[1].0);
                comicinfo::strip_chapter_fields(&mut json, one_volume);
            }
            InfoSource::Meta(Box::new(Metadata::from_comicinfo(&json)))
        }
        None => raw_or_meta(first.info.clone().unwrap()),
    })
}

/// JSON that reduces to VNMF metadata (incl. Info 1.1) — metadata; anything else — as is.
fn raw_or_meta(data: Vec<u8>) -> InfoSource {
    match serde_json::from_slice::<Value>(&data).ok().as_ref().and_then(Metadata::from_json) {
        Some(meta) => InfoSource::Meta(Box::new(meta)),
        None => InfoSource::Raw(data),
    }
}

/// File from --info: VNMF metadata (`vnmf.json`), ComicInfo.xml, format 1.1 JSON — reduced to
/// VNMF metadata; TOML, text and arbitrary JSON are stored as is.
fn load_info_file(path: &Path) -> Result<Option<InfoSource>> {
    let data = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    Ok(Some(match InfoFormat::detect(&data) {
        InfoFormat::Xml => {
            let xml = String::from_utf8(data).context("XML is not UTF-8")?;
            let json = comicinfo::xml_to_json(&xml).with_context(|| format!("{}: invalid XML", path.display()))?;
            if !comicinfo::is_comicinfo(&json) {
                bail!("{}: XML without a <ComicInfo> root — ComicInfo.xml was expected", path.display());
            }
            InfoSource::Meta(Box::new(Metadata::from_comicinfo(&json)))
        }
        InfoFormat::Json => {
            serde_json::from_slice::<Value>(&data).with_context(|| format!("{}: invalid JSON", path.display()))?;
            raw_or_meta(data)
        }
        InfoFormat::Binary => bail!("{}: metadata must be text", path.display()),
        InfoFormat::Toml | InfoFormat::Text => InfoSource::Raw(data),
    }))
}

fn default_output(first_input: &Path) -> PathBuf {
    let name = format!("{}.vnmf", input::file_stem(first_input));
    match first_input.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}
