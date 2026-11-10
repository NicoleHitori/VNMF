// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Unpacking `.vnmf` into a folder or a ZIP — shared by `vnmfunpack` and `vnmf-server` (CBZ export).
//!
//! Two layouts: **full** (images, `aux/` with attachments and their manifest, `previews/`, `vnmf.json`,
//! `ComicInfo.xml` — `vnmfpack` packs it back losslessly) and **CBZ** (images and metadata only:
//! other readers would show covers as extra pages and don't know the attachments).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufWriter, Seek, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use vnmf_core::{AuxKind, PageFormat, VnmfReader, detect_extension};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use crate::{auxfiles, export};

/// Where files go: into a folder or into a ZIP.
pub trait Sink {
    /// `compress` — whether compression makes sense (images are already compressed, so not for them).
    fn put(&mut self, rel_path: &str, data: &[u8], compress: bool) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

/// Files in a folder.
pub struct DirSink {
    pub root: PathBuf,
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

/// The ZIP is written to `*.tmp` and renamed only on success — just like .vnmf in the builder.
pub struct ZipSink {
    zip: zip::ZipWriter<BufWriter<File>>,
    tmp: PathBuf,
    target: PathBuf,
}

impl ZipSink {
    /// Starts `<target>.tmp`; [`Sink::finish`] renames it to `target`.
    pub fn create(target: PathBuf) -> Result<Self> {
        let mut tmp = target.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let file = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        Ok(Self { zip: zip::ZipWriter::new(BufWriter::new(file)), tmp, target })
    }

    /// The temporary file (to remove it if unpacking failed).
    pub fn tmp_path(&self) -> &std::path::Path {
        &self.tmp
    }
}

/// A ZIP in any seekable writer (memory, an already open file).
pub struct ZipWriterSink<W: Write + Seek> {
    zip: zip::ZipWriter<W>,
}

impl<W: Write + Seek> ZipWriterSink<W> {
    pub fn new(writer: W) -> Self {
        Self { zip: zip::ZipWriter::new(writer) }
    }

    /// Finishes the archive and returns the writer.
    pub fn into_inner(self) -> Result<W> {
        Ok(self.zip.finish()?)
    }
}

impl<W: Write + Seek> Sink for ZipWriterSink<W> {
    fn put(&mut self, rel_path: &str, data: &[u8], compress: bool) -> Result<()> {
        let method = if compress { CompressionMethod::Deflated } else { CompressionMethod::Stored };
        self.zip.start_file(rel_path, SimpleFileOptions::default().compression_method(method))?;
        self.zip.write_all(data)?;
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<()> {
        self.zip.finish()?;
        Ok(())
    }
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

/// What was written.
#[derive(Default)]
pub struct Stats {
    pub files: usize,
    pub bytes: u64,
    /// What was left out (CBZ layout).
    pub notes: Vec<String>,
}

impl Stats {
    fn put(&mut self, sink: &mut dyn Sink, rel: &str, data: &[u8], compress: bool) -> Result<()> {
        sink.put(rel, data, compress)?;
        self.files += 1;
        self.bytes += data.len() as u64;
        Ok(())
    }
}

/// Writes the file's contents into `sink`: `full` — the full layout (with attachments and previews),
/// otherwise CBZ. `verify` — fail on a chunk with a wrong CRC32 instead of copying damaged data.
pub fn unpack(reader: &VnmfReader, sink: &mut dyn Sink, full: bool, verify: bool) -> Result<Stats> {
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
            if !full {
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
        if full {
            if verify && !reader.verify_preview_crc(i)? {
                bail!("preview #{i}: CRC32 doesn't match — data is damaged (--no-verify to ignore)");
            }
            let data = reader.get_preview_data(i)?;
            let ext = export::image_extension(data, PageFormat::from(rec.format));
            let name = format!("previews/preview_{i:02}_vol{}.{ext}", rec.volume_id.get());
            stats.put(sink, &name, data, false)?;
        }
    }
    if !full {
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
