// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Rewriting a `.vnmf` with a few things changed: the metadata, the covers or the protection
//! (redundancy, tail copy, block size). Everything else — UUID, pages, their order, roles, flags,
//! structure, attachments with their parameters — is carried over as is; the images are slices of the
//! memory-mapped source, nothing is decoded.
//!
//! Used by `vnmf-server`. A 1.x file comes out as 2.0.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use vnmf_core::{AuxInput, BlockSize, Builder, ChunkSource, HeaderBitEnum, PageBitFlags, PageRole, VnmfReader};

/// What to change. `None` — keep as in the source.
#[derive(Default)]
pub struct Changes {
    /// The Info section; `Some(None)` — remove it.
    pub info: Option<Option<Vec<u8>>>,
    /// All previews in order (the first is the main cover): the image and the volume (0 — the file).
    pub previews: Option<Vec<(ChunkSource, u16)>>,
    /// Reed–Solomon redundancy, %.
    pub redundancy: Option<u8>,
    pub tail_duplicate: Option<bool>,
    pub block_size: Option<BlockSize>,
    /// Reading direction of all pages (the `READING_RTL` page flag).
    pub rtl: Option<bool>,
}

/// Builds `output` from `source` with `changes`. `output` may be the same path: the new file is written
/// next to it and renamed over it only at the end (the source stays mapped until then — Unix semantics).
///
/// Refuses a file with damaged data: it would carry the damage into a file with fresh checksums.
pub fn rewrite(source: &Path, output: &Path, changes: Changes) -> Result<()> {
    let reader = Arc::new(VnmfReader::open(source).with_context(|| format!("could not open {}", source.display()))?);
    let damaged = (0..reader.chunks_count()).filter(|&i| !reader.verify_chunk_crc(i).unwrap_or(false)).count()
        + (0..reader.previews_count()).filter(|&i| !reader.verify_preview_crc(i).unwrap_or(false)).count();
    if damaged > 0 {
        bail!("{damaged} damaged data chunk(s) — repair the file first");
    }

    let tail = changes.tail_duplicate.unwrap_or_else(|| reader.has_flag(HeaderBitEnum::HasTailDup));
    let mut builder = Builder::new()
        .with_uuid(reader.uuid())
        .with_block_size(changes.block_size.unwrap_or_else(|| reader.block_size()))
        .with_tail_duplicate(tail)
        .with_redundancy(changes.redundancy.unwrap_or_else(|| reader.redundancy()))
        .with_mixed_formats(true);

    let info = match changes.info {
        Some(info) => info,
        None => reader.info_bytes().map(<[u8]>::to_vec),
    };
    if let Some(info) = info {
        builder.set_info(info);
    }

    match changes.previews {
        Some(previews) => {
            for (source, volume) in previews {
                builder.add_preview(source, volume, 0)?;
            }
        }
        None => {
            for (i, rec) in reader.previews().iter().enumerate() {
                builder.add_preview(reader.shared_preview(i)?, rec.volume_id.get(), rec.flags)?;
            }
        }
    }

    let webtoon = reader.has_flag(HeaderBitEnum::IsWebToon);
    if webtoon {
        builder.set_flag(HeaderBitEnum::IsWebToon);
    }
    for (index, page) in reader.pages().iter().enumerate() {
        builder.set_volume(page.volume_id.get());
        let chunks = page.chunk_range().map(|c| reader.shared_chunk(c)).collect::<Result<Vec<ChunkSource>, _>>()?;
        let role = PageRole::try_from(page.role).unwrap_or(PageRole::Normal);
        // The builder only adds chapter/volume start flags, so the source flags are passed as is.
        let (chapter, number, mut flags) = (page.chapter_id.get(), page.page_number_in_chapter.get(), page.flags.get());
        if let Some(rtl) = changes.rtl {
            flags = if rtl { flags | PageBitFlags::READING_RTL } else { flags & !PageBitFlags::READING_RTL };
        }
        match <[ChunkSource; 1]>::try_from(chunks) {
            Ok([chunk]) => builder.add_page(chunk, role, chapter, number, flags),
            Err(chunks) => builder.add_webtoon(chunks, chapter, number, flags),
        }
        .with_context(|| format!("page {}", index + 1))?;
        for rec in reader.page_aux(index) {
            builder.attach_aux(AuxInput {
                kind: rec.kind(),
                chunk: reader.shared_chunk(rec.chunk_idx.get() as usize)?,
                flags: rec.flags.get(),
                delay_ms: rec.delay_ms.get(),
                trigger_y: rec.trigger_y.get(),
                part: rec.part.get(),
                duration_ms: rec.duration_ms.get(),
                pages: rec.pages.get(),
                volume: rec.volume,
            })?;
        }
    }
    // `add_webtoon` marks the file as a webtoon; a paged file with a multi-image page stays paged.
    if !webtoon {
        builder.clear_flag(HeaderBitEnum::IsWebToon);
    }
    builder.build(output).with_context(|| format!("writing {}", output.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vnmf_core::{AuxFlags, AuxKind};

    fn png(w: u32, h: u32, seed: u8) -> ChunkSource {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend(w.to_be_bytes());
        bytes.extend(h.to_be_bytes());
        bytes.extend([8, 2, 0, 0, 0, seed]);
        ChunkSource::from_bytes(bytes).unwrap()
    }

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("vnmf-rewrite-{name}-{}.vnmf", std::process::id()))
    }

    #[test]
    fn keeps_everything_but_the_changes() {
        let path = temp("src");
        let mut b = Builder::new().with_uuid([7; 16]).with_redundancy(5).with_tail_duplicate(true);
        b.set_info(br#"{"schema": "vnmf-metadata/1", "title": "Old"}"#.to_vec());
        b.set_volume(1);
        b.add_page(png(10, 20, 1), PageRole::FrontCover, 1, 1, 0).unwrap();
        let mut music = AuxInput::new(AuxKind::Music, ChunkSource::from_bytes(b"OggSmusic".to_vec()).unwrap());
        music.flags = AuxFlags::LOOP;
        music.pages = 3;
        music.volume = 40;
        b.attach_aux(music).unwrap();
        b.add_page(png(10, 20, 2), PageRole::Normal, 2, 1, 0).unwrap();
        b.attach_aux(AuxInput::new(AuxKind::DarkImage, png(10, 20, 9))).unwrap();
        b.set_volume(2);
        b.add_page(png(10, 20, 3), PageRole::SpreadLeft, 1, 1, 0).unwrap();
        b.add_preview(png(3, 4, 5), 0, 0).unwrap();
        b.add_preview(png(3, 4, 6), 2, 0).unwrap();
        b.build(&path).unwrap();

        let out = temp("out");
        rewrite(&path, &out, Changes { info: Some(Some(b"new".to_vec())), redundancy: Some(0), ..Default::default() }).unwrap();
        let (old, new) = (VnmfReader::open(&path).unwrap(), VnmfReader::open(&out).unwrap());
        assert_eq!(new.uuid(), [7; 16]);
        assert_eq!(new.info_bytes(), Some(b"new".as_slice()));
        assert_eq!(new.redundancy(), 0);
        assert!(new.has_flag(HeaderBitEnum::HasTailDup));
        assert!(!new.has_flag(HeaderBitEnum::IsWebToon));
        assert_eq!(old.pages().len(), new.pages().len());
        for (a, b) in old.pages().iter().zip(new.pages()) {
            assert_eq!(
                (a.volume_id, a.chapter_id, a.page_number_in_chapter, a.flags, a.role),
                (b.volume_id, b.chapter_id, b.page_number_in_chapter, b.flags, b.role)
            );
        }
        for i in 0..old.chunks_count() {
            assert_eq!(old.get_chunk_data(i).unwrap(), new.get_chunk_data(i).unwrap());
        }
        assert_eq!(old.aux().len(), new.aux().len());
        for (a, b) in old.aux().iter().zip(new.aux()) {
            assert_eq!((a.page_idx, a.kind, a.flags, a.pages, a.volume, a.part), (b.page_idx, b.kind, b.flags, b.pages, b.volume, b.part));
        }
        assert_eq!(new.previews().iter().map(|p| p.volume_id.get()).collect::<Vec<_>>(), vec![0, 2]);

        // In place, with new covers and the Info removed.
        rewrite(&out, &out, Changes { info: Some(None), previews: Some(vec![(png(5, 5, 1), 0)]), ..Default::default() }).unwrap();
        let again = VnmfReader::open(&out).unwrap();
        assert_eq!(again.info_bytes(), None);
        assert_eq!(again.previews_count(), 1);
        assert_eq!(again.pages_count(), 3);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn webtoon_stays_a_webtoon() {
        let path = temp("wt");
        let mut b = Builder::new();
        b.add_webtoon(vec![png(8, 10, 1), png(8, 12, 2)], 1, 1, 0).unwrap();
        b.add_webtoon(vec![png(8, 10, 3)], 2, 1, 0).unwrap();
        b.build(&path).unwrap();
        rewrite(&path, &path, Changes { redundancy: Some(10), rtl: Some(true), ..Default::default() }).unwrap();
        let r = VnmfReader::open(&path).unwrap();
        assert!(r.has_flag(HeaderBitEnum::IsWebToon));
        assert!(r.pages().iter().all(|p| p.flags.get() & PageBitFlags::READING_RTL != 0));
        assert_eq!(r.redundancy(), 10);
        assert_eq!((r.pages()[0].chunk_count.get(), r.pages()[0].height.get()), (2, 22));
        let _ = std::fs::remove_file(&path);
    }
}
