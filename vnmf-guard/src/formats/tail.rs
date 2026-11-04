// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Formats where parity is appended at the end in an "empty" wrapper: Matroska/WebM, MP4,
//! Zstandard, gzip, ISO (and a raw file). The original stays an unchanged prefix — so removing
//! protection simply means truncating the file (for MP4 also restoring the size of the last box).

use anyhow::{Result, bail, ensure};

use super::{Built, Extra, Seg, parity_area};
use crate::plan::Plan;

fn with_prefix(orig: &[u8], segs: impl IntoIterator<Item = Seg>) -> Vec<Seg> {
    std::iter::once(Seg::Orig(0..orig.len())).chain(segs).collect()
}

/// Variable-length EBML number at `pos`: (value without the marker, length, "unknown size").
fn vint(d: &[u8], pos: usize) -> Option<(u64, usize, bool)> {
    let first = *d.get(pos)?;
    let len = first.leading_zeros() as usize + 1;
    let bytes = d.get(pos..pos + len).filter(|_| len <= 8)?;
    let value = bytes[1..].iter().fold(u64::from(first) & (0xFF >> len), |acc, &b| acc << 8 | u64::from(b));
    Some((value, len, value == (1 << (7 * len)) - 1))
}

/// Matroska/WebM: a `Void` element with an 8-byte size at the end of the segment; the segment size
/// is increased so the `Void` ends up inside it (otherwise ffmpeg warns about data after the
/// segment). A segment of unknown size (a browser recording) swallows the `Void` anyway.
pub(super) fn matroska(orig: &[u8], payload: u64, map: u64) -> Result<Built> {
    let body = 2 * map + payload;
    ensure!(body < (1 << 56) - 1, "file too large for a Matroska Void");
    let void_len = 9 + body;
    let mut head = vec![0xEC, 0x01];
    head.extend_from_slice(&body.to_be_bytes()[1..]);

    let mut segs = Vec::new();
    let mut extra = Vec::new();
    // EBML header, then Segment (id 0x18538067).
    let segment_size = vint(orig, 4)
        .map(|(size, len, _)| 4 + len + size as usize)
        .filter(|&at| orig.get(at..at + 4) == Some(&[0x18, 0x53, 0x80, 0x67]))
        .and_then(|at| Some((at + 4, vint(orig, at + 4)?)));
    match segment_size {
        Some((at, (size, len, false))) if at + len + size as usize == orig.len() && size + void_len < (1 << (7 * len)) - 1 => {
            let grown = (size + void_len) | 1 << (7 * len);
            segs.push(Seg::Orig(0..at));
            segs.push(Seg::Bytes(grown.to_be_bytes()[8 - len..].to_vec()));
            segs.push(Seg::Orig(at + len..orig.len()));
            extra.extend_from_slice(&(at as u64).to_le_bytes());
            extra.extend_from_slice(&orig[at..at + len]);
        }
        _ => segs.push(Seg::Orig(0..orig.len())),
    }
    segs.push(Seg::Bytes(head));
    segs.extend(parity_area(payload, map));
    Ok(Built { segs, extra })
}

/// MP4/MOV: a `free` box with a 64-bit size. The last box with size 0 ("to the end of file")
/// gets an explicit size — otherwise it would swallow our box.
pub(super) fn mp4(orig: &[u8], payload: u64, map: u64) -> Result<Built> {
    let len = orig.len() as u64;
    let mut pos = 0u64;
    let mut open_ended = None;
    while pos < len {
        let p = pos as usize;
        ensure!(pos + 8 <= len, "MP4: truncated box at {pos}");
        let size32 = u32::from_be_bytes(orig[p..p + 4].try_into()?);
        let (size, header) = match size32 {
            0 => {
                open_ended = Some(pos);
                (len - pos, 8)
            }
            1 => {
                ensure!(pos + 16 <= len, "MP4: truncated box at {pos}");
                (u64::from_be_bytes(orig[p + 8..p + 16].try_into()?), 16)
            }
            n => (u64::from(n), 8),
        };
        ensure!(size >= header && pos + size <= len, "MP4: corrupted box structure at {pos} (garbage after it or the file is truncated)");
        pos += size;
    }

    let mut segs = Vec::new();
    let mut extra = Vec::new();
    if let Some(at) = open_ended {
        let size = u32::try_from(len - at).map_err(|_| anyhow::anyhow!("MP4: the last box without a size is larger than 4 GiB"))?;
        segs.push(Seg::Orig(0..at as usize));
        segs.push(Seg::Bytes(size.to_be_bytes().to_vec()));
        segs.push(Seg::Orig(at as usize + 4..orig.len()));
        extra.extend_from_slice(&at.to_le_bytes());
        extra.extend_from_slice(&[0; 4]);
    } else {
        segs.push(Seg::Orig(0..orig.len()));
    }
    let body = 2 * map + payload;
    let mut head = 1u32.to_be_bytes().to_vec();
    head.extend_from_slice(b"free");
    head.extend_from_slice(&(16 + body).to_be_bytes());
    segs.push(Seg::Bytes(head));
    segs.extend(parity_area(payload, map));
    Ok(Built { segs, extra })
}

/// First zstd frame: regular (`28 B5 2F FD`) or skippable (`5? 2A 4D 18`).
pub(super) fn is_zstd(d: &[u8]) -> bool {
    d.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) || (d.len() >= 4 && d[0] & 0xF0 == 0x50 && d[1..4] == [0x2A, 0x4D, 0x18])
}

const ZSTD_SKIPPABLE: u32 = 0x184D_2A5E;
const ZSTD_CHUNK: u64 = 1 << 31;

/// Zstandard: skippable frames (up to 4 GiB each): map, parity in parts, map.
pub(super) fn zstd(orig: &[u8], payload: u64, map: u64) -> Result<Built> {
    ensure!(map <= u64::from(u32::MAX), "zstd: map is too large");
    let frame = |len: u64| {
        let mut h = ZSTD_SKIPPABLE.to_le_bytes().to_vec();
        h.extend_from_slice(&(len as u32).to_le_bytes());
        Seg::Bytes(h)
    };
    let mut segs = vec![frame(map), Seg::Map(map)];
    for chunk in chunks(payload, ZSTD_CHUNK) {
        segs.extend([frame(chunk), Seg::Payload(chunk)]);
    }
    segs.extend([frame(map), Seg::Map(map)]);
    Ok(Built { segs: with_prefix(orig, segs), extra: Vec::new() })
}

/// Maximum data in a single FEXTRA field (XLEN ≤ 65535, minus the 4-byte subfield header).
const GZIP_CHUNK: u64 = 65531;

/// gzip: empty members (decompress to zero bytes), data in the `VG` subfield of the FEXTRA field.
/// `gzip -d`, `zcat` and zlib read many members in a row, so the output doesn't change.
pub(super) fn gzip(orig: &[u8], payload: u64, map: u64) -> Result<Built> {
    ensure!(map <= GZIP_CHUNK, "gzip: map doesn't fit into a single FEXTRA field");
    let member = |len: u64, data: Seg, segs: &mut Vec<Seg>| {
        let mut head = vec![0x1F, 0x8B, 0x08, 0x04, 0, 0, 0, 0, 0, 0xFF];
        head.extend_from_slice(&(len as u16 + 4).to_le_bytes());
        head.extend_from_slice(b"VG");
        head.extend_from_slice(&(len as u16).to_le_bytes());
        // Empty deflate block, CRC32 = 0, ISIZE = 0.
        segs.extend([Seg::Bytes(head), data, Seg::Bytes(vec![0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0])]);
    };
    let mut segs = vec![Seg::Orig(0..orig.len())];
    member(map, Seg::Map(map), &mut segs);
    for chunk in chunks(payload, GZIP_CHUNK) {
        member(chunk, Seg::Payload(chunk), &mut segs);
    }
    member(map, Seg::Map(map), &mut segs);
    Ok(Built { segs, extra: Vec::new() })
}

/// ISO 9660 (and a raw file): no wrapper.
pub(super) fn plain(orig: &[u8], payload: u64, map: u64) -> Built {
    Built { segs: with_prefix(orig, parity_area(payload, map)), extra: Vec::new() }
}

fn chunks(total: u64, size: u64) -> impl Iterator<Item = u64> {
    (0..total.div_ceil(size)).map(move |i| size.min(total - i * size))
}

/// Tail formats: the original is a prefix of the file; `extra` (if present) is the offset and original bytes
/// of the field changed during protection (Matroska segment size or the last MP4 box size).
pub(super) fn strip(file: &[u8], plan: &Plan) -> Result<Vec<u8>> {
    let len = usize::try_from(plan.orig_len)?;
    let Some(orig) = file.get(..len) else { bail!("file is shorter than the original") };
    let mut out = orig.to_vec();
    if !plan.extra.is_empty() {
        let mut extra = Extra(&plan.extra);
        let at = extra.usize()?;
        let bytes = extra.bytes(plan.extra.len() - 8)?;
        ensure!(at + bytes.len() <= out.len(), "map: corrupted format data");
        out[at..at + bytes.len()].copy_from_slice(bytes);
    }
    Ok(out)
}
