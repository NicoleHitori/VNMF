// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! vnmfunpack — unpacks .vnmf into a CBZ or a folder.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use vnmf_core::{AuxKind, PageFormat, VnmfReader, detect_extension};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use vnmf::{auxfiles, export, human_size};

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

/// Where files go: into a folder or into a ZIP.
trait Sink {
    /// `compress` — whether compression makes sense (images are already compressed, so not for them).
    fn put(&mut self, rel_path: &str, data: &[u8], compress: bool) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

struct DirSink {
    root: PathBuf,
}

impl Sink for DirSink {
    fn put(&mut self, rel_path: &str, data: &[u8], _compress: bool) -> Result<()> {
        let path = self.root.join(rel_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, data).with_context(|| format!("writing {}", path.display()))
    }

    fn finish(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

/// The CBZ is written to `*.tmp` and renamed only on success — just like .vnmf in the builder.
struct ZipSink {
    zip: zip::ZipWriter<BufWriter<File>>,
    tmp: PathBuf,
    target: PathBuf,
}

impl Sink for ZipSink {
    fn put(&mut self, rel_path: &str, data: &[u8], compress: bool) -> Result<()> {
        let method = if compress { CompressionMethod::Deflated } else { CompressionMethod::Stored };
        self.zip.start_file(rel_path, SimpleFileOptions::default().compression_method(method))?;
        self.zip.write_all(data)?;
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<()> {
        let mut writer = self.zip.finish()?;
        writer.flush()?;
        writer.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        fs::rename(&self.tmp, &self.target)?;
        Ok(())
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
            let mut tmp = output.clone().into_os_string();
            tmp.push(".tmp");
            let tmp = PathBuf::from(tmp);
            let file = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
            Box::new(ZipSink { zip: zip::ZipWriter::new(BufWriter::new(file)), tmp, target: output.clone() })
        }
    };

    let result = unpack(&reader, sink.as_mut(), format, !args.no_verify);
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

#[derive(Default)]
struct Stats {
    files: usize,
    bytes: u64,
    notes: Vec<String>,
}

impl Stats {
    fn put(&mut self, sink: &mut dyn Sink, rel: &str, data: &[u8], compress: bool) -> Result<()> {
        sink.put(rel, data, compress)?;
        self.files += 1;
        self.bytes += data.len() as u64;
        Ok(())
    }
}

fn unpack(reader: &VnmfReader, sink: &mut dyn Sink, format: OutFormat, verify: bool) -> Result<Stats> {
    let mut stats = Stats::default();

    // Several chapters → a folder per chapter (inside a volume folder if there are volumes); one chapter — flat structure.
    let structure = reader.structure();
    let has_volumes = reader.has_volumes();
    let multi_chapter = structure.iter().map(|v| v.chapters.len()).sum::<usize>() > 1;
    let mut page_dir = vec![String::new(); reader.pages_count()];
    if multi_chapter {
        let mut used = HashSet::new();
        for volume in &structure {
            for chapter in &volume.chapters {
                let base = if has_volumes { format!("vol{:02}/ch{:04}", volume.volume_id, chapter.chapter_id) } else { format!("ch{:04}", chapter.chapter_id) };
                // Two chapters with the same number in a row (separated by a flag) — different folders.
                let mut dir = base.clone();
                let mut n = 2;
                while !used.insert(dir.clone()) {
                    dir = format!("{base}_{n}");
                    n += 1;
                }
                for p in chapter.pages.clone() {
                    page_dir[p] = format!("{dir}/");
                }
            }
        }
    }
    let width = reader.pages_count().max(reader.chunks_count()).to_string().len().max(3);

    let mut aux_count = 0;
    // The number in an attachment name is the ordinal of the page's first image (see `auxfiles`).
    let mut images_before = 0;
    let mut aux_params = Vec::new();
    for (i, page) in reader.pages().iter().enumerate() {
        let dir = &page_dir[i];
        let chunks = reader.get_page_chunks_data(i)?;
        let first = page.first_chunk_idx.get() as usize;

        for (j, data) in chunks.iter().enumerate() {
            if verify && !reader.verify_chunk_crc(first + j)? {
                bail!("page {}: CRC32 of chunk #{} doesn't match — data is damaged (--no-verify to ignore)", i + 1, first + j);
            }
            let ext = export::image_extension(data, PageFormat::from(page.format));
            let name = if chunks.len() == 1 { format!("{dir}{:0width$}.{ext}", i + 1) } else { format!("{dir}{:0width$}_{:02}.{ext}", i + 1, j + 1) };
            stats.put(sink, &name, data, false)?;
        }

        let mut seen: Vec<AuxKind> = Vec::new();
        for (k, rec) in reader.page_aux_range(i).zip(reader.page_aux(i)) {
            aux_count += 1;
            if format != OutFormat::Dir {
                continue;
            }
            if verify && !reader.verify_aux_crc(k)? {
                bail!("page {}: CRC32 of attachment #{k} doesn't match — data is damaged (--no-verify to ignore)", i + 1);
            }
            let data = reader.get_aux_data(k)?;
            seen.push(rec.kind());
            let nth = seen.iter().filter(|&&s| s == rec.kind()).count();
            let file = auxfiles::file_name(images_before + 1, width, rec.kind(), nth, detect_extension(data));
            stats.put(sink, &format!("aux/{file}"), data, true)?;
            aux_params.push((file, auxfiles::params(rec)));
        }
        images_before += chunks.len();
    }
    if let Some(manifest) = auxfiles::manifest(aux_params) {
        stats.put(sink, &format!("aux/{}", auxfiles::MANIFEST), &manifest, true)?;
    }

    // Previews don't go into a CBZ: readers would show the cover as an extra page.
    for (i, rec) in reader.previews().iter().enumerate() {
        if format == OutFormat::Dir {
            if verify && !reader.verify_preview_crc(i)? {
                bail!("preview #{i}: CRC32 doesn't match — data is damaged (--no-verify to ignore)");
            }
            let data = reader.get_preview_data(i)?;
            let ext = export::image_extension(data, PageFormat::from(rec.format));
            let name = format!("previews/preview_{i:02}_vol{}.{ext}", rec.volume_id.get());
            stats.put(sink, &name, data, false)?;
        }
    }
    if format == OutFormat::Cbz {
        if reader.previews_count() > 0 {
            stats.notes.push(format!("{} previews not included in the CBZ (unpack into a folder to get them)", reader.previews_count()));
        }
        if aux_count > 0 {
            stats.notes.push(format!("{aux_count} attachments (sound, text, dark versions) not included in the CBZ (unpack into a folder)"));
        }
    }

    if let Some(info) = reader.info_bytes() {
        // vnmf.json — full metadata (vnmfpack builds the same file from it);
        // ComicInfo.xml — for other readers.
        let (name, data) = export::info_file(info)?;
        stats.put(sink, &name, &data, true)?;
        if let Some((name, data)) = export::comicinfo_file(info, reader)? {
            stats.put(sink, &name, &data, true)?;
        }
    }
    Ok(stats)
}

fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}
