// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Opening a `.vnmf` for editing: pages, structure, roles, metadata and file parameters —
//! everything needed to build a **new** file from them (with different redundancy, block size, page order…).
//!
//! Pages are not copied into memory: each chunk is a slice of the memory-mapped file
//! ([`VnmfReader::shared_chunk`]). That's why the source file must not be overwritten while a new one
//! is being built from it.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use vnmf_core::{AuxInput, BlockSize, ChunkSource, HeaderBitEnum, PageBitFlags, PageFormat, PageRole, VnmfReader};

use crate::metadata::Metadata;

/// Page flags the builder sets itself (the rest — alpha, lossless, animation — are carried over).
const DERIVED_FLAGS: u16 =
    PageBitFlags::IS_CHAPTER_START | PageBitFlags::IS_VOLUME_START | PageBitFlags::READING_RTL | PageBitFlags::MIXED_FORMATS;

/// A single image (chunk) of a page.
pub struct OpenedImage {
    pub source: ChunkSource,
    pub format: PageFormat,
    pub width: u32,
    pub height: u32,
    /// The CRC of the bytes doesn't match the one recorded in the chunk table.
    pub damaged: bool,
}

/// Page: one image, in a webtoon — several fragments from top to bottom.
pub struct OpenedPage {
    pub images: Vec<OpenedImage>,
    pub role: PageRole,
    /// Flags describing the image itself (alpha, lossless, animation) — without the structural ones.
    pub flags: u16,
    /// Attachments (music, effects, text, dark versions) with all parameters.
    pub aux: Vec<AuxInput>,
}

/// A chapter and its pages.
pub struct OpenedChapter {
    pub volume: u16,
    /// The chapter opens a volume (only if the file has volumes).
    pub volume_start: bool,
    pub volume_title: Option<String>,
    pub volume_date: Option<String>,
    pub volume_isbn: Option<String>,
    pub chapter: u16,
    /// Number for display: from the table of contents ("10.5") or `chapter`.
    pub number: String,
    pub title: Option<String>,
    pub date: Option<String>,
    pub pages: Vec<OpenedPage>,
}

/// Contents and parameters of the opened file.
pub struct Opened {
    pub chapters: Vec<OpenedChapter>,
    pub has_volumes: bool,
    /// Metadata without the table of contents (volume and chapter titles are in `chapters`). `None` — there's no Info or it's
    /// neither VNMF metadata nor ComicInfo (see `unknown_info`).
    pub metadata: Option<Metadata>,
    /// Info exists, but it's neither VNMF metadata nor ComicInfo — it won't make it into the new file.
    pub unknown_info: bool,
    pub webtoon: bool,
    pub rtl: bool,
    pub redundancy: u8,
    pub block_size: BlockSize,
    pub tail_duplicate: bool,
    pub has_previews: bool,
    /// Custom covers — previews whose bytes are not among the page images, in table order:
    /// image and volume (0 — file cover).
    pub custom_previews: Vec<(ChunkSource, u16)>,
    /// At least one preview is a page image (covers "from pages").
    pub page_previews: bool,
    /// Metadata was read from the backup copy (the primary is damaged).
    pub recovered: bool,
    /// Pages that couldn't be read (coordinates beyond the file) — skipped.
    pub unreadable: usize,
}

impl Opened {
    /// Images with a damaged CRC.
    pub fn damaged(&self) -> usize {
        self.chapters.iter().flat_map(|c| &c.pages).flat_map(|p| &p.images).filter(|i| i.damaged).count()
    }

    pub fn pages(&self) -> usize {
        self.chapters.iter().map(|c| c.pages.len()).sum()
    }
}

/// Whether the file is VNMF (by the magic at the start).
pub fn is_vnmf(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == vnmf_core::spec::MAGIC
}

/// Opens a `.vnmf` for rebuilding.
pub fn open(path: &Path) -> Result<Opened> {
    let reader = Arc::new(VnmfReader::open(path).with_context(|| format!("could not open {}", path.display()))?);
    let info = reader.info_bytes();
    let metadata = Metadata::from_info(info);
    let unknown_info = info.is_some_and(|i| !i.is_empty()) && metadata.is_none();
    let contents = metadata.as_ref().map(|m| m.contents.clone()).unwrap_or_default();
    let has_volumes = reader.has_volumes();
    let mut rtl = metadata.as_ref().is_some_and(Metadata::is_rtl);

    let mut chapters = Vec::new();
    let mut unreadable = 0;
    for volume in reader.structure() {
        for (k, span) in volume.chapters.iter().enumerate() {
            let entry = contents.chapter(span.volume_id, span.chapter_id);
            let vol_meta = contents.volume(span.volume_id);
            let volume_start = has_volumes && k == 0;
            let mut pages = Vec::new();
            for index in span.pages.clone() {
                let record = &reader.pages()[index];
                rtl |= record.flags.get() & PageBitFlags::READING_RTL != 0;
                match open_page(&reader, index) {
                    Ok(page) => pages.push(page),
                    Err(_) => unreadable += 1,
                }
            }
            if pages.is_empty() {
                continue;
            }
            chapters.push(OpenedChapter {
                volume: span.volume_id,
                volume_start,
                volume_title: vol_meta.and_then(|v| v.title.clone()).filter(|_| volume_start),
                volume_date: vol_meta.and_then(|v| v.date.clone()).filter(|_| volume_start),
                volume_isbn: vol_meta.and_then(|v| v.isbn.clone()).filter(|_| volume_start),
                chapter: span.chapter_id,
                number: entry.and_then(|e| e.number.clone()).unwrap_or_else(|| span.chapter_id.to_string()),
                title: entry.and_then(|e| e.title.clone()),
                date: entry.and_then(|e| e.date.clone()),
                pages,
            });
        }
    }

    // Custom covers: previews whose bytes are not among the page images.
    let page_crcs: std::collections::HashSet<u32> =
        chapters.iter().flat_map(|c| &c.pages).flat_map(|p| &p.images).map(|i| i.source.crc).collect();
    let mut custom_previews = Vec::new();
    let mut page_previews = false;
    for (i, rec) in reader.previews().iter().enumerate() {
        if page_crcs.contains(&rec.crc.get()) {
            page_previews = true;
        } else if let Ok(source) = reader.shared_preview(i)
            && source.probe_image().is_ok_and(|(w, h, _)| w > 0 && h > 0)
        {
            custom_previews.push((source, rec.volume_id.get()));
        }
    }

    // The table of contents moved into chapters; in the metadata it would only get in the way (numbers may change).
    let metadata = metadata.map(|mut m| {
        m.contents = Default::default();
        m
    });
    Ok(Opened {
        chapters,
        has_volumes,
        metadata,
        unknown_info,
        webtoon: reader.has_flag(HeaderBitEnum::IsWebToon),
        rtl,
        redundancy: reader.redundancy(),
        block_size: reader.block_size(),
        tail_duplicate: reader.has_flag(HeaderBitEnum::HasTailDup),
        has_previews: reader.previews_count() > 0,
        custom_previews,
        page_previews,
        recovered: reader.is_recovered(),
        unreadable,
    })
}

fn open_page(reader: &Arc<VnmfReader>, index: usize) -> Result<OpenedPage> {
    let record = &reader.pages()[index];
    let count = record.chunk_count.get() as usize;
    let mut images = Vec::with_capacity(count);
    for chunk in record.chunk_range() {
        let source = reader.shared_chunk(chunk)?;
        let damaged = source.crc != reader.chunks()[chunk].crc();
        let (width, height, format) = match source.probe_image() {
            Ok((w, h, f)) if w > 0 && h > 0 => (w, h, f),
            // Damaged image: dimensions from the page record (for a webtoon — only the width).
            _ => (record.width.get(), if count == 1 { record.height.get() } else { 0 }, PageFormat::from(record.format)),
        };
        images.push(OpenedImage { source, format, width, height, damaged });
    }
    let aux = reader
        .page_aux(index)
        .iter()
        .map(|a| {
            Ok(AuxInput {
                kind: a.kind(),
                chunk: reader.shared_chunk(a.chunk_idx.get() as usize)?,
                flags: a.flags.get(),
                delay_ms: a.delay_ms.get(),
                trigger_y: a.trigger_y.get(),
                part: a.part.get(),
                duration_ms: a.duration_ms.get(),
                pages: a.pages.get(),
                volume: a.volume,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let role = PageRole::try_from(record.role).unwrap_or(PageRole::Normal);
    Ok(OpenedPage { images, role, flags: record.flags.get() & !DERIVED_FLAGS, aux })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vnmf_core::Builder;

    /// A 4×6 PNG header (IHDR is enough for dimensions) and a byte that makes pages different.
    fn png(seed: u8) -> ChunkSource {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x04\0\0\0\x06\x08\x02\0\0\0".to_vec();
        bytes.extend([0, 0, 0, 0, seed]);
        ChunkSource::from_bytes(bytes).unwrap()
    }

    #[test]
    fn reopens_structure_roles_and_metadata() {
        let dir = std::env::temp_dir().join(format!("vnmf-edit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.vnmf");
        let mut meta = Metadata::new();
        meta.title = Some("Test".into());
        meta.contents.set_chapter(1, 2, Some("2.5".into()), Some("Second".into()));
        meta.contents.set_volume_title(1, Some("First".into()));
        let mut b = Builder::new().with_redundancy(10).with_tail_duplicate(true);
        b.set_info(meta.to_bytes());
        b.set_volume(1);
        b.add_page(png(1), PageRole::FrontCover, 1, 1, 0).unwrap();
        b.add_page(png(2), PageRole::Normal, 2, 1, 0).unwrap();
        let mut music = AuxInput::new(vnmf_core::AuxKind::Music, ChunkSource::from_bytes(b"OggS\0music".to_vec()).unwrap());
        music.flags = vnmf_core::AuxFlags::LOOP;
        music.delay_ms = 250;
        b.attach_aux(music).unwrap();
        b.set_volume(2);
        b.add_page(png(3), PageRole::Normal, 1, 1, 0).unwrap();
        b.add_preview(png(9), 0, 0).unwrap();
        b.add_preview(png(1), 1, 0).unwrap();
        b.add_preview(png(8), 2, 0).unwrap();
        b.add_preview(png(7), 2, 0).unwrap();
        b.build(&path).unwrap();

        assert!(is_vnmf(&path));
        let opened = open(&path).unwrap();
        assert!(opened.has_volumes);
        assert_eq!(opened.redundancy, 10);
        assert!(opened.tail_duplicate);
        assert_eq!(opened.chapters.len(), 3);
        assert_eq!(opened.chapters[0].pages[0].role, PageRole::FrontCover);
        assert_eq!(opened.chapters[0].volume_title.as_deref(), Some("First"));
        assert_eq!(opened.chapters[1].number, "2.5");
        assert_eq!(opened.chapters[1].title.as_deref(), Some("Second"));
        assert!(!opened.chapters[1].volume_start);
        assert!(opened.chapters[2].volume_start && opened.chapters[2].volume == 2);
        let meta = opened.metadata.as_ref().unwrap();
        assert_eq!(meta.title.as_deref(), Some("Test"));
        assert!(meta.contents.is_empty());
        assert_eq!(opened.damaged(), 0);
        // Custom — all previews not from pages, with volumes and in table order; the volume 1 cover is a page.
        let custom: Vec<(u32, u16)> = opened.custom_previews.iter().map(|(p, v)| (p.crc, *v)).collect();
        assert_eq!(custom, vec![(png(9).crc, 0), (png(8).crc, 2), (png(7).crc, 2)]);
        assert!(opened.page_previews);
        let aux = &opened.chapters[1].pages[0].aux;
        assert_eq!(aux.len(), 1);
        assert_eq!((aux[0].kind, aux[0].flags, aux[0].delay_ms), (vnmf_core::AuxKind::Music, vnmf_core::AuxFlags::LOOP, 250));
        assert!(opened.chapters[0].pages[0].aux.is_empty());

        // Rebuilding with different parameters: pages are slices of the old file.
        let out = dir.join("b.vnmf");
        let mut b = Builder::new().with_redundancy(0);
        for c in &opened.chapters {
            b.set_volume(c.volume);
            for p in &c.pages {
                b.add_page(p.images[0].source.clone(), p.role, c.chapter, 1, p.flags).unwrap();
            }
        }
        b.build(&out).unwrap();
        let reader = VnmfReader::open(&out).unwrap();
        assert_eq!(reader.redundancy(), 0);
        assert_eq!(reader.pages_count(), 3);
        let old = VnmfReader::open(&path).unwrap();
        assert_eq!(reader.get_page_data(2).unwrap(), old.get_page_data(2).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
