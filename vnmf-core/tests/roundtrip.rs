// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Core integration tests. Self-contained: images are generated in memory
//! (a minimal PNG header + a "body" of noise), so they don't depend on samples on disk.

use std::fs;
use std::path::{Path, PathBuf};

use vnmf_core::spec;
use vnmf_core::{
    AuxFlags, AuxFormat, AuxInput, AuxKind, Builder, ChunkSource, Header, HeaderBitEnum, PageFormat, PageRole, ReaderError,
    VnmfReader,
};
use zerocopy::IntoBytes;

/// A separate temporary folder for each test (tests run in parallel).
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A PNG with a valid IHDR (enough to detect the format and dimensions) and `body` bytes of noise.
fn fake_png(width: u32, height: u32, seed: u8, body: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    v.extend_from_slice(&width.to_be_bytes());
    v.extend_from_slice(&height.to_be_bytes());
    v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    v.extend((0..body).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
    v
}

fn fake_jpeg() -> Vec<u8> {
    vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0]
}

fn page(bytes: &[u8]) -> ChunkSource {
    ChunkSource::from_bytes(bytes.to_vec()).unwrap()
}

struct Manga {
    cover: Vec<u8>,
    pages: [Vec<u8>; 2],
}

fn build_manga(out: &Path, tail_dup: bool) -> Manga {
    let m = Manga {
        cover: fake_png(300, 400, 1, 5_000),
        pages: [fake_png(1600, 2400, 2, 9_000), fake_png(1600, 70_000, 3, 7_000)],
    };
    let mut b = Builder::new().with_uuid([7; 16]).with_tail_duplicate(tail_dup);
    b.set_info(b"title = \"test\"".to_vec());
    b.add_preview(page(&m.cover), 0, 0).unwrap();
    b.add_page(page(&m.pages[0]), PageRole::FrontCover, 1, 1, 0).unwrap();
    b.add_page(page(&m.pages[1]), PageRole::Normal, 1, 2, 0).unwrap();
    b.build(out).unwrap();
    m
}

#[test]
fn roundtrip_preserves_data() {
    let dir = scratch("roundtrip");
    let out = dir.join("m.vnmf");
    let m = build_manga(&out, false);

    let r = VnmfReader::open(&out).unwrap();
    assert!(!r.is_recovered());
    assert_eq!(r.uuid(), [7; 16]);
    assert_eq!(r.info_as_str().unwrap().unwrap(), "title = \"test\"");
    assert_eq!(r.pages_count(), 2);
    assert_eq!(r.chunks_count(), 2);
    assert!(r.has_flag(HeaderBitEnum::HasAlign));
    assert!(!r.has_flag(HeaderBitEnum::HasTailDup));

    assert_eq!(r.main_preview_data().unwrap(), m.cover);
    for (i, expected) in m.pages.iter().enumerate() {
        assert_eq!(r.get_page_data(i).unwrap(), expected);
        assert!(r.verify_chunk_crc(i).unwrap());
        assert_eq!(PageFormat::from(r.pages()[i].format), PageFormat::Png);
    }
    // Height above u16::MAX is stored without truncation.
    assert_eq!(r.pages()[1].height.get(), 70_000);
    // All data is block-aligned.
    for c in r.chunks() {
        assert!(r.block_size().is_aligned(c.offset()));
    }
}

/// Opening without a path (a file descriptor on Android, bytes from a network provider) reads the same.
#[test]
fn opens_from_file_handle_and_bytes() {
    let dir = scratch("handle");
    let out = dir.join("m.vnmf");
    let m = build_manga(&out, true);
    let from_file = {
        let file = std::fs::File::open(&out).unwrap();
        VnmfReader::from_file(&file).unwrap()
        // the file is closed — the mapping lives on
    };
    let from_bytes = VnmfReader::from_bytes(&std::fs::read(&out).unwrap()).unwrap();
    for r in [&from_file, &from_bytes] {
        assert_eq!(r.pages_count(), 2);
        assert_eq!(r.main_preview_data().unwrap(), m.cover);
        assert_eq!(r.get_page_data(1).unwrap(), m.pages[1]);
    }
    assert!(matches!(VnmfReader::from_bytes(&[0; 10]), Err(ReaderError::FileTooSmall(10))));
}

#[test]
fn page_zero_without_aux_has_no_aux_data() {
    let dir = scratch("aux");
    let out = dir.join("m.vnmf");
    let text = br#"{"schema": "vnmf-text/1", "regions": []}"#.to_vec();

    let mut b = Builder::new();
    b.add_page(page(&fake_png(10, 10, 1, 100)), PageRole::Normal, 1, 1, 0).unwrap();
    b.add_page(page(&fake_png(10, 10, 2, 100)), PageRole::Normal, 1, 2, 0).unwrap();
    b.attach_aux(AuxInput::new(AuxKind::Text, page(&text))).unwrap();
    b.build(&out).unwrap();

    let r = VnmfReader::open(&out).unwrap();
    // Presence is determined only by the record count, without magic index values.
    assert!(r.page_aux(0).is_empty());
    assert_eq!(r.pages()[0].aux_count.get(), 0);
    let aux = r.page_aux(1);
    assert_eq!(aux.len(), 1);
    assert_eq!(aux[0].kind(), AuxKind::Text);
    assert_eq!(AuxFormat::from(aux[0].format), AuxFormat::Json);
    assert_eq!(r.get_aux_data(r.page_aux_range(1).start).unwrap(), text);
    assert!(r.has_flag(HeaderBitEnum::HasText) && !r.has_flag(HeaderBitEnum::HasAudio));
}

/// Several attachments per page with playback parameters; the file flags are a summary of the table.
#[test]
fn several_aux_per_page_with_parameters() {
    let dir = scratch("aux_many");
    let out = dir.join("m.vnmf");
    let music = b"OggS\0\x02music".to_vec();
    let sfx = b"ID3\x04sfx".to_vec();
    let dark = fake_png(10, 10, 7, 50);

    let mut b = Builder::new().with_redundancy(10);
    b.add_page(page(&fake_png(10, 10, 1, 100)), PageRole::Normal, 1, 1, 0).unwrap();
    let mut m = AuxInput::new(AuxKind::Music, page(&music));
    m.flags = AuxFlags::LOOP;
    m.volume = 60;
    m.duration_ms = 90_000;
    m.pages = 3;
    b.attach_aux(m).unwrap();
    let mut s = AuxInput::new(AuxKind::Sfx, page(&sfx));
    s.delay_ms = 1500;
    s.trigger_y = 4;
    b.attach_aux(s).unwrap();
    b.attach_aux(AuxInput::new(AuxKind::DarkImage, page(&dark))).unwrap();
    // A dark version of a nonexistent page image is an error.
    let mut bad = AuxInput::new(AuxKind::DarkImage, page(&dark));
    bad.part = 1;
    assert!(b.attach_aux(bad).is_err());
    b.add_page(page(&fake_png(10, 10, 2, 100)), PageRole::Normal, 1, 2, 0).unwrap();
    b.build(&out).unwrap();

    let r = VnmfReader::open(&out).unwrap();
    let aux = r.page_aux(0);
    assert_eq!(aux.iter().map(|a| a.kind()).collect::<Vec<_>>(), [AuxKind::Music, AuxKind::Sfx, AuxKind::DarkImage]);
    assert!(aux[0].has_flag(AuxFlags::LOOP));
    assert!((aux[0].gain() - 0.6).abs() < 1e-9);
    assert_eq!((aux[0].duration_ms.get(), aux[0].pages.get()), (90_000, 3));
    assert_eq!((aux[1].delay_ms.get(), aux[1].trigger_y.get()), (1500, 4));
    assert_eq!(aux[1].gain(), 1.0);
    assert_eq!(AuxFormat::from(aux[2].format), AuxFormat::Png);
    for (k, expected) in [music, sfx, dark].iter().enumerate() {
        assert_eq!(r.get_aux_data(k).unwrap(), expected);
        assert!(r.verify_aux_crc(k).unwrap());
    }
    assert!(r.page_aux(1).is_empty());
    assert!(r.has_flag(HeaderBitEnum::HasAudio) && r.has_flag(HeaderBitEnum::HasDarkMode) && !r.has_flag(HeaderBitEnum::HasText));
    assert!(r.verify_parity().unwrap().is_clean());
}

/// All record and header fields sit at naturally aligned offsets.
#[test]
fn records_are_naturally_aligned() {
    use std::mem::{offset_of, size_of};
    use vnmf_core::{AuxRecord, EntryRecord, PageRecord, PreviewRecord};
    assert_eq!(size_of::<PageRecord>(), 64);
    assert_eq!(size_of::<AuxRecord>(), 48);
    assert_eq!(size_of::<PreviewRecord>(), 32);
    macro_rules! aligned {
        ($t:ty: $($f:ident: $n:expr),*) => {$(assert_eq!(offset_of!($t, $f) % $n, 0, concat!(stringify!($t), ".", stringify!($f)));)*};
    }
    aligned!(PageRecord: width: 4, height: 4, first_chunk_idx: 4, chunk_count: 4, first_aux_idx: 4, aux_count: 4,
        volume_id: 2, chapter_id: 2, page_number_in_chapter: 2, flags: 2, record_crc: 4);
    aligned!(AuxRecord: page_idx: 4, chunk_idx: 4, flags: 2, delay_ms: 4, trigger_y: 4, part: 4, duration_ms: 4, pages: 2, record_crc: 4);
    aligned!(PreviewRecord: offset: 8, length: 4, crc: 4, width: 4, height: 4, volume_id: 2, record_crc: 4);
    aligned!(EntryRecord: offset: 8, length: 4, crc: 4);
    aligned!(Header: flags: 2, info: 8, preview_table: 8, page_table: 8, aux_table: 8, chunks_table: 8, parity_offset: 8, crc: 4);
}

#[test]
fn webtoon_dimensions_are_probed_from_chunks() {
    let dir = scratch("webtoon");
    let out = dir.join("w.vnmf");
    let parts = [fake_png(800, 5000, 1, 300), fake_png(720, 6000, 2, 300), fake_png(800, 4000, 3, 300)];

    let mut b = Builder::new();
    b.add_webtoon(parts.iter().map(|p| page(p)).collect(), 1, 1, 0).unwrap();
    b.build(&out).unwrap();

    let r = VnmfReader::open(&out).unwrap();
    let rec = r.pages()[0];
    assert!(r.has_flag(HeaderBitEnum::IsWebToon));
    assert_eq!(rec.chunk_count.get(), 3);
    assert_eq!((rec.width.get(), rec.height.get()), (800, 15_000));
    assert_eq!(PageFormat::from(rec.format), PageFormat::Png);
    for (data, expected) in r.get_page_chunks_data(0).unwrap().iter().zip(&parts) {
        assert_eq!(data, expected);
    }
}

#[test]
fn webtoon_rejects_mixed_formats() {
    let mut b = Builder::new();
    let err = b
        .add_webtoon(vec![page(&fake_png(10, 10, 1, 10)), page(&fake_jpeg())], 1, 1, 0)
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn file_and_memory_sources_mix() {
    let dir = scratch("mixed_sources");
    let on_disk = dir.join("p1.png");
    let p1 = fake_png(100, 200, 9, 1000);
    let p2 = fake_png(100, 200, 8, 1000);
    fs::write(&on_disk, &p1).unwrap();
    let out = dir.join("m.vnmf");

    let mut b = Builder::new();
    b.add_page_file(&on_disk, PageRole::Normal, 1, 1, 0).unwrap();
    b.add_page(page(&p2), PageRole::Normal, 1, 2, 0).unwrap();
    b.build(&out).unwrap();

    let r = VnmfReader::open(&out).unwrap();
    assert_eq!(r.get_page_data(0).unwrap(), p1);
    assert_eq!(r.get_page_data(1).unwrap(), p2);
}

#[test]
fn corrupted_tables_recover_from_tail_copy() {
    let dir = scratch("tail");
    let out = dir.join("m.vnmf");
    let m = build_manga(&out, true);

    // Corrupt the primary page table.
    let mut bytes = fs::read(&out).unwrap();
    let table = VnmfReader::open(&out).unwrap().header().page_table;
    bytes[table.offset() as usize] ^= 0xFF;
    fs::write(&out, &bytes).unwrap();

    let r = VnmfReader::open(&out).unwrap();
    assert!(r.is_recovered());
    assert_eq!(r.pages_count(), 2);
    assert_eq!(r.get_page_data(1).unwrap(), m.pages[1]);
    assert_eq!(r.info_as_str().unwrap().unwrap(), "title = \"test\"");
}

#[test]
fn corrupted_tables_without_tail_copy_fail() {
    let dir = scratch("notail");
    let out = dir.join("m.vnmf");
    build_manga(&out, false);

    let mut bytes = fs::read(&out).unwrap();
    let table = VnmfReader::open(&out).unwrap().header().page_table;
    bytes[table.offset() as usize] ^= 0xFF;
    fs::write(&out, &bytes).unwrap();

    match VnmfReader::open(&out) {
        Err(ReaderError::CrcMismatch { section: "PageTable", .. }) => {}
        other => panic!("expected CrcMismatch, got {:?}", other.err()),
    }
}

#[test]
fn hostile_offsets_return_error_not_panic() {
    let dir = scratch("hostile");
    let out = dir.join("m.vnmf");
    build_manga(&out, false);

    // A header valid by CRC, but the table offset is near u64::MAX.
    let mut bytes = fs::read(&out).unwrap();
    let mut header = *VnmfReader::open(&out).unwrap().header();
    header.page_table.offset.set(u64::MAX - 4);
    header.seal();
    bytes[..spec::HEADER_SIZE].copy_from_slice(header.as_bytes());
    fs::write(&out, &bytes).unwrap();

    assert!(matches!(
        VnmfReader::open(&out),
        Err(ReaderError::InvalidTableSize { section: "PageTable", .. })
    ));
}

#[test]
fn unknown_block_size_and_major_version_are_rejected() {
    let dir = scratch("version");
    let out = dir.join("m.vnmf");
    build_manga(&out, false);
    let original = *VnmfReader::open(&out).unwrap().header();
    let bytes = fs::read(&out).unwrap();

    let patch = |f: &dyn Fn(&mut Header)| {
        let mut h = original;
        f(&mut h);
        h.seal();
        let mut b = bytes.clone();
        b[..spec::HEADER_SIZE].copy_from_slice(h.as_bytes());
        fs::write(&out, &b).unwrap();
        VnmfReader::open(&out)
    };

    assert!(matches!(patch(&|h| h.block_size_shift = 99), Err(ReaderError::InvalidBlockSize(99))));
    assert!(matches!(patch(&|h| h.major_version = 3), Err(ReaderError::UnsupportedVersion(3, 0))));
}

#[test]
fn source_changed_after_add_fails_build_and_leaves_no_file() {
    let dir = scratch("changed");
    let src = dir.join("page.png");
    fs::write(&src, fake_png(10, 10, 1, 500)).unwrap();
    let out = dir.join("m.vnmf");

    let mut b = Builder::new();
    b.add_page_file(&src, PageRole::Normal, 1, 1, 0).unwrap();
    // The file grew between add and build.
    fs::write(&src, fake_png(10, 10, 1, 600)).unwrap();

    assert_eq!(b.build(&out).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    assert!(!out.exists());
    assert!(!dir.join("m.vnmf.tmp").exists());
}

#[test]
fn too_small_buffer_is_error() {
    let dir = scratch("small");
    let out = dir.join("tiny.vnmf");
    fs::write(&out, b"vnmf").unwrap();
    assert!(matches!(VnmfReader::open(&out), Err(ReaderError::FileTooSmall(4))));
}

// ======================================================================
// Redundancy and recovery
// ======================================================================

use vnmf_core::{parity, recovery};

fn build_with_redundancy(out: &Path, redundancy: u8) -> Manga {
    let m = Manga {
        cover: fake_png(300, 400, 1, 20_000),
        pages: [fake_png(1600, 2400, 2, 90_000), fake_png(1600, 2400, 3, 70_000)],
    };
    let mut b = Builder::new().with_uuid([9; 16]).with_redundancy(redundancy);
    b.set_info(br#"{"ComicInfo": {"Title": "parity"}}"#.to_vec());
    b.add_preview(page(&m.cover), 0, 0).unwrap();
    b.add_page(page(&m.pages[0]), PageRole::Normal, 1, 1, 0).unwrap();
    b.add_page(page(&m.pages[1]), PageRole::Normal, 1, 2, 0).unwrap();
    b.build(out).unwrap();
    m
}

#[test]
fn redundancy_is_recorded_and_verifies_clean() {
    let dir = scratch("parity_clean");
    let out = dir.join("p.vnmf");
    build_with_redundancy(&out, 25);

    let r = VnmfReader::open(&out).unwrap();
    assert_eq!(r.redundancy(), 25);
    assert!(r.has_flag(HeaderBitEnum::HasParity));
    assert!(r.has_flag(HeaderBitEnum::HasTailDup), "parity enables the tail copy");
    let layout = r.parity_layout().unwrap();
    assert_eq!(layout.offset % 4096, 0);
    assert!(r.verify_parity().unwrap().is_clean());
    // The tail header comes right after the parity section.
    assert!(fs::metadata(&out).unwrap().len() - spec::HEADER_SIZE as u64 >= layout.end());
}

#[test]
fn redundancy_above_max_is_rejected() {
    let dir = scratch("parity_max");
    let mut b = Builder::new().with_redundancy(31);
    b.add_page(page(&fake_png(10, 10, 1, 10)), PageRole::Normal, 1, 1, 0).unwrap();
    assert_eq!(b.build(dir.join("x.vnmf")).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn parity_repairs_damaged_header_tables_and_data() {
    let dir = scratch("parity_repair");
    let out = dir.join("p.vnmf");
    let m = build_with_redundancy(&out, 30);
    let pristine = fs::read(&out).unwrap();
    let layout = VnmfReader::open(&out).unwrap().parity_layout().unwrap();

    // Wipe the start of the file contiguously: the primary header, Info, tables and part of the data.
    let mut bytes = pristine.clone();
    let burst = (layout.max_burst_bytes() as usize).min(layout.offset as usize) * 9 / 10;
    bytes[..burst].fill(0xEE);

    let report = recovery::inspect_headers(&bytes);
    assert!(!report.primary_ok() && report.tail_ok());
    let header = recovery::parity_layout_of(report.best().unwrap()).unwrap();
    assert_eq!(header, layout);

    let result = parity::repair(&mut bytes, &layout).unwrap();
    assert!(result.is_complete(), "{result:?}");
    assert_eq!(bytes, pristine);

    fs::write(&out, &bytes).unwrap();
    let r = VnmfReader::open(&out).unwrap();
    assert!(!r.is_recovered());
    assert_eq!(r.get_page_data(1).unwrap(), m.pages[1]);
}

#[test]
fn primary_header_is_rebuilt_from_tail() {
    let dir = scratch("rebuild_primary");
    let out = dir.join("m.vnmf");
    build_manga(&out, true);
    let original = *VnmfReader::open(&out).unwrap().header();

    let mut bytes = fs::read(&out).unwrap();
    bytes[..spec::HEADER_SIZE].fill(0);
    fs::write(&out, &bytes).unwrap();

    let report = recovery::inspect_headers(&bytes);
    let rebuilt = recovery::primary_from_tail(report.tail.unwrap().as_ref().unwrap()).unwrap();
    assert_eq!(rebuilt.as_bytes(), original.as_bytes());

    recovery::write_primary_header(&out, &rebuilt).unwrap();
    assert!(!VnmfReader::open(&out).unwrap().is_recovered());
}

#[test]
fn tail_is_rebuilt_from_primary() {
    let dir = scratch("rebuild_tail");
    for redundancy in [0, 10] {
        let out = dir.join(format!("m{redundancy}.vnmf"));
        if redundancy == 0 { build_manga(&out, true); } else { build_with_redundancy(&out, redundancy); }
        let pristine = fs::read(&out).unwrap();

        // The end of the file is lost together with the tail header.
        let truncated = pristine.len() - 6000;
        fs::write(&out, &pristine[..truncated]).unwrap();
        assert!(!recovery::inspect_headers(&fs::read(&out).unwrap()).tail_ok());

        recovery::rebuild_tail(&out).unwrap();
        assert_eq!(fs::read(&out).unwrap(), pristine, "redundancy {redundancy}");
    }
}

#[test]
fn carving_finds_pages_when_metadata_is_gone() {
    let dir = scratch("carve");
    let out = dir.join("m.vnmf");
    let m = build_manga(&out, false);

    let mut bytes = fs::read(&out).unwrap();
    bytes[..4096].fill(0); // header + Info
    let found = recovery::carve(&bytes);
    let images: Vec<&[u8]> = found
        .iter()
        .filter(|c| matches!(c.kind, recovery::CarvedKind::Image(_)))
        .map(|c| c.data(&bytes))
        .collect();
    assert_eq!(images, [m.cover.as_slice(), &m.pages[0], &m.pages[1]]);
}

// ======================================================================
// Integrity of page and preview records
// ======================================================================

use vnmf_core::{HeaderCopy, RecordCrc};

#[test]
fn every_record_is_sealed() {
    let dir = scratch("records_sealed");
    let out = dir.join("m.vnmf");
    build_manga(&out, false);
    let r = VnmfReader::open(&out).unwrap();
    assert!(r.pages().iter().all(RecordCrc::is_intact));
    assert!(r.previews().iter().all(RecordCrc::is_intact));
}

#[test]
fn salvage_merges_tables_damaged_in_both_copies() {
    let dir = scratch("salvage");
    let out = dir.join("m.vnmf");
    let m = build_manga(&out, true);
    let pristine = fs::read(&out).unwrap();

    let front = *VnmfReader::open_with(&out, HeaderCopy::Primary).unwrap().header();
    let tail = *VnmfReader::open_with(&out, HeaderCopy::Tail).unwrap().header();

    let mut bytes = pristine.clone();
    // Primary copy: page 0 and chunk record 1 are damaged.
    // Tail copy: page 1, chunk 0 and the preview. No page/chunk table is intact in both copies.
    bytes[front.page_table.offset() as usize + 3] ^= 0xFF;
    bytes[front.chunks_table.offset() as usize + 16 + 2] ^= 0xFF;
    bytes[tail.page_table.offset() as usize + 64 + 5] ^= 0xFF;
    bytes[tail.chunks_table.offset() as usize + 9] ^= 0xFF;
    bytes[tail.preview_table.offset() as usize + 1] ^= 0xFF;

    let report = recovery::inspect_headers(&bytes);
    assert!(!report.primary_complete() && !report.tail_complete(), "neither copy is usable on its own");

    let salvage = recovery::salvage_metadata(&mut bytes).unwrap();
    assert!(salvage.exact, "{salvage:?}");
    // Page 0 (by the record CRC) and chunk 1 (by the CRC of the data it points to) — from the tail;
    // the preview is intact in the primary copy, so it's taken as a whole section.
    assert_eq!(salvage.merged_records, 2);
    assert_eq!(salvage.whole_sections, 2); // Info and previews
    assert_eq!(salvage.bad_records, 0);

    fs::write(&out, &bytes).unwrap();
    recovery::rebuild_tail(&out).unwrap();
    assert_eq!(fs::read(&out).unwrap(), pristine);
    assert_eq!(VnmfReader::open(&out).unwrap().get_page_data(1).unwrap(), m.pages[1]);
}

#[test]
fn damaged_record_is_reported_by_index() {
    let dir = scratch("record_index");
    let out = dir.join("m.vnmf");
    build_manga(&out, false);
    let mut bytes = fs::read(&out).unwrap();
    let mut header = *VnmfReader::open(&out).unwrap().header();
    // Corrupt page record 1 and "legalize" the table CRC — only the record CRC can detect it.
    let table = header.page_table.offset() as usize;
    let len = header.page_table.length() as usize;
    bytes[table + 64 + 8] ^= 1;
    header.page_table.crc.set(crc32fast_hash(&bytes[table..table + len]));
    header.seal();
    bytes[..spec::HEADER_SIZE].copy_from_slice(header.as_bytes());
    fs::write(&out, &bytes).unwrap();

    assert!(matches!(
        VnmfReader::open(&out),
        Err(ReaderError::RecordCrc { section: "PageTable", index: 1 })
    ));
}

fn crc32fast_hash(data: &[u8]) -> u32 {
    // The same CRC-32 (IEEE) as in the format; without a separate dev dependency.
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

// ======================================================================
// Mixed formats in a webtoon
// ======================================================================

use vnmf_core::PageBitFlags;

#[test]
fn mixed_formats_webtoon_when_allowed() {
    let dir = scratch("mixed_webtoon");
    let out = dir.join("w.vnmf");
    let parts = [fake_png(800, 1000, 1, 300), fake_jpeg(), fake_png(800, 1000, 2, 300)];

    let mut b = Builder::new().with_mixed_formats(true);
    b.add_webtoon(parts.iter().map(|p| page(p)).collect(), 1, 1, 0).unwrap();
    b.build(&out).unwrap();

    let r = VnmfReader::open(&out).unwrap();
    let rec = r.pages()[0];
    assert_ne!(rec.flags.get() & PageBitFlags::MIXED_FORMATS, 0);
    assert_eq!(PageFormat::from(rec.format), PageFormat::Png, "the page format is the format of the first chunk");
    let detected: Vec<PageFormat> = r.get_page_chunks_data(0).unwrap().iter().map(|d| PageFormat::detect(d)).collect();
    assert_eq!(detected, [PageFormat::Png, PageFormat::Jpeg, PageFormat::Png]);
}

#[test]
fn uniform_webtoon_has_no_mixed_flag_and_error_names_the_chunk() {
    let dir = scratch("uniform_webtoon");
    let out = dir.join("w.vnmf");
    let mut b = Builder::new().with_mixed_formats(true);
    b.add_webtoon(vec![page(&fake_png(10, 10, 1, 10)), page(&fake_png(10, 10, 2, 10))], 1, 1, 0).unwrap();
    b.build(&out).unwrap();
    let flags = VnmfReader::open(&out).unwrap().pages()[0].flags.get();
    assert_eq!(flags & PageBitFlags::MIXED_FORMATS, 0, "allowing doesn't mean the flag is always set");

    let mut strict = Builder::new();
    let err = strict
        .add_webtoon(vec![page(&fake_png(10, 10, 1, 10)), page(&fake_jpeg()).with_label("002.jpeg")], 1, 1, 0)
        .unwrap_err();
    assert!(err.to_string().contains("002.jpeg"), "{err}");
}

// --- Volumes and chapters (v1.1) ---

#[test]
fn volumes_and_chapters_roundtrip() {
    let out = scratch("structure").join("s.vnmf");
    let mut b = Builder::new();
    // Volume 1: chapters 1 (2 pages) and 2 (1 page); volume 2: chapter 3 (2 pages).
    b.set_volume(1);
    for (ch, n) in [(1u16, 1u16), (1, 2), (2, 1)] {
        b.add_page(page(&fake_png(10, 20, n as u8, 32)), PageRole::Normal, ch, n, 0).unwrap();
    }
    b.set_volume(2);
    for n in 1..=2u16 {
        b.add_page(page(&fake_png(10, 20, 9 + n as u8, 32)), PageRole::Normal, 3, n, 0).unwrap();
    }
    b.build(&out).unwrap();

    let r = VnmfReader::open(&out).unwrap();
    assert_eq!(r.version(), spec::DEFAULT_VERSION);
    assert!(r.has_volumes());
    let s = r.structure();
    assert_eq!(s.len(), 2);
    assert_eq!((s[0].volume_id, s[0].pages.clone()), (1, 0..3));
    assert_eq!(s[0].chapters.iter().map(|c| (c.chapter_id, c.pages.clone())).collect::<Vec<_>>(), vec![(1, 0..2), (2, 2..3)]);
    assert_eq!((s[1].volume_id, s[1].pages.clone(), s[1].chapters.len()), (2, 3..5, 1));

    // The builder sets the start flags itself.
    let starts: Vec<(bool, bool)> = r
        .pages()
        .iter()
        .map(|p| (p.flags.get() & PageBitFlags::IS_VOLUME_START != 0, p.flags.get() & PageBitFlags::IS_CHAPTER_START != 0))
        .collect();
    assert_eq!(starts, vec![(true, true), (false, false), (false, true), (true, true), (false, false)]);
    assert_eq!(vnmf_core::structure::locate(&s, 4), Some((1, 0)));
}

#[test]
fn file_without_volumes_is_one_unnamed_volume() {
    let out = scratch("novolumes").join("n.vnmf");
    let mut b = Builder::new();
    for ch in [1u16, 1, 2] {
        b.add_page(page(&fake_png(10, 20, ch as u8, 16)), PageRole::Normal, ch, 1, 0).unwrap();
    }
    b.build(&out).unwrap();
    let r = VnmfReader::open(&out).unwrap();
    assert!(!r.has_volumes());
    let s = r.structure();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].volume_id, 0);
    assert_eq!(s[0].chapters.len(), 2);
    assert!(r.pages().iter().all(|p| p.flags.get() & PageBitFlags::IS_VOLUME_START == 0));
}

/// A primary header with `major = 1` and a recomputed CRC is a damaged 2.0 file, not 1.x:
/// the tail copy is intact, so the reader uses it, and recovery doesn't treat the file as "old format".
#[test]
fn major_one_with_valid_crc_is_damage_not_legacy() {
    let dir = scratch("major_one");
    let out = dir.join("m.vnmf");
    let m = build_with_redundancy(&out, 20);
    let pristine = fs::read(&out).unwrap();
    let mut header = *VnmfReader::open(&out).unwrap().header();
    header.major_version = 1;
    header.seal();
    let mut bytes = pristine.clone();
    bytes[..spec::HEADER_SIZE].copy_from_slice(header.as_bytes());
    fs::write(&out, &bytes).unwrap();

    assert!(!recovery::is_legacy(&bytes));
    let r = VnmfReader::open(&out).unwrap();
    assert!(r.is_recovered() && r.legacy_version().is_none());
    assert_eq!(r.get_page_data(1).unwrap(), m.pages[1]);
    let report = recovery::inspect_headers(&bytes);
    assert!(!report.primary_ok() && report.tail_complete());

    // Parity restores the file byte for byte.
    let layout = recovery::parity_layout_of(report.best().unwrap()).unwrap();
    assert!(parity::repair(&mut bytes, &layout).unwrap().is_complete());
    assert_eq!(bytes, pristine);
}

/// Both headers pass CRC but differ (a field changed together with the CRC) — the copies disagree.
#[test]
fn headers_with_valid_crc_but_different_content_disagree() {
    let dir = scratch("disagree");
    let out = dir.join("m.vnmf");
    build_manga(&out, true);
    let pristine = fs::read(&out).unwrap();
    assert!(recovery::inspect_headers(&pristine).headers_agree());

    let tail_pos = pristine.len() - spec::HEADER_SIZE;
    for pos in [0, tail_pos] {
        let mut bytes = pristine.clone();
        let mut h = *zerocopy::FromBytes::ref_from_bytes(&bytes[pos..pos + spec::HEADER_SIZE]).map(|h: &Header| h).unwrap();
        h.uuid[0] ^= 0xFF;
        h.seal();
        bytes[pos..pos + spec::HEADER_SIZE].copy_from_slice(h.as_bytes());
        let report = recovery::inspect_headers(&bytes);
        assert!(report.primary_complete() && report.tail_complete(), "each copy is intact on its own");
        assert!(!report.headers_agree(), "header changed at {pos}");
        assert_eq!(report.header_differences(), ["uuid"]);

        // Without parity there's no arbiter — trust the tail: the primary is restored from it.
        assert!(recovery::restore_front_from_tail(&mut bytes).unwrap());
        assert!(recovery::inspect_headers(&bytes).headers_agree());
    }
}
