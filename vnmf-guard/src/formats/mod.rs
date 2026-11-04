// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Where exactly parity is placed in each format so that ordinary programs don't see it.
//!
//! | format | where | why it's ignored |
//! |---|---|---|
//! | Matroska / WebM | `Void` element (0xEC) at the end | global EBML padding element |
//! | MP4 / MOV / M4A | `free` box at the end | ISO BMFF padding box |
//! | Ogg Opus / Vorbis | binary data after the comments in the comment header | RFC 7845 §5.2; Vorbis reads up to the framing bit |
//! | FLAC | `APPLICATION` metadata blocks with id `VNMF` | unknown applications are skipped |
//! | MP3 | ID3v2 `XVNM` frame | X… frames are experimental, unknown ones are skipped |
//! | Zstandard | skippable frames | part of the zstd standard |
//! | gzip | empty members with an FEXTRA field | decompress to zero bytes |
//! | ISO 9660 | right after the image | the volume size is recorded in the PVD |
//!
//! Each format stores two copies of the map (at the start and end of its area) and the parity between them.

mod flac;
mod mp3;
mod ogg;
mod tail;

use std::ops::Range;

use anyhow::Result;

use crate::Format;
use crate::plan::Plan;

pub(crate) use ogg::{page_crc, page_len};

/// A piece of the protected file during assembly.
pub(crate) enum Seg {
    /// Bytes of the original (protected).
    Orig(Range<usize>),
    /// New bytes: wrappers, headers (protected).
    Bytes(Vec<u8>),
    Payload(u64),
    Map(u64),
    OggCrc,
}

/// Assembly result: file pieces and the data needed to remove protection.
pub(crate) struct Built {
    pub segs: Vec<Seg>,
    pub extra: Vec<u8>,
}

/// Detects the format by contents. `None` — unknown (can only be protected as a raw file).
pub fn detect(d: &[u8]) -> Option<Format> {
    const MP4_BOXES: [&[u8; 4]; 8] = [b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"pnot", b"uuid"];
    if d.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(Format::Matroska);
    }
    if d.len() >= 8 && MP4_BOXES.iter().any(|b| &d[4..8] == *b) {
        return Some(Format::Mp4);
    }
    if d.starts_with(b"OggS") {
        return Some(Format::Ogg);
    }
    if d.starts_with(b"fLaC") {
        return Some(Format::Flac);
    }
    if let Some(end) = mp3::id3_end(d) {
        // FLAC can also have an ID3v2 in front.
        return Some(if d.get(end..end + 4) == Some(b"fLaC") { Format::Flac } else { Format::Mp3 });
    }
    if mp3::is_frame_sync(d) {
        return Some(Format::Mp3);
    }
    if tail::is_zstd(d) {
        return Some(Format::Zstd);
    }
    if d.starts_with(&[0x1F, 0x8B, 0x08]) {
        return Some(Format::Gzip);
    }
    if d.get(0x8001..0x8006) == Some(b"CD001") {
        return Some(Format::Iso);
    }
    None
}

/// Assembles the protected file: `payload` — at least this many bytes for parity, `map` —
/// at least this many for each map copy (the format may round up).
pub(crate) fn build(format: Format, orig: &[u8], payload: u64, map: u64) -> Result<Built> {
    match format {
        Format::Matroska => tail::matroska(orig, payload, map),
        Format::Mp4 => tail::mp4(orig, payload, map),
        Format::Ogg => ogg::build(orig, payload, map),
        Format::Flac => flac::build(orig, payload, map),
        Format::Mp3 => mp3::build(orig, payload, map),
        Format::Zstd => tail::zstd(orig, payload, map),
        Format::Gzip => tail::gzip(orig, payload, map),
        Format::Iso | Format::Raw => Ok(tail::plain(orig, payload, map)),
    }
}

/// Removes protection from an intact (already recovered) file — returns the file without parity.
pub(crate) fn strip(file: &[u8], plan: &Plan) -> Result<Vec<u8>> {
    match plan.format {
        Format::Ogg => ogg::strip(file, plan),
        Format::Flac => flac::strip(file, plan),
        Format::Mp3 => mp3::strip(file, plan),
        _ => tail::strip(file, plan),
    }
}

/// A regular format area: map copy, parity, map copy.
fn parity_area(payload: u64, map: u64) -> [Seg; 3] {
    [Seg::Map(map), Seg::Payload(payload), Seg::Map(map)]
}

/// Simple reader of `extra` fields.
struct Extra<'a>(&'a [u8]);

impl Extra<'_> {
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }

    fn usize(&mut self) -> Result<usize> {
        Ok(usize::try_from(self.u64()?)?)
    }

    fn bytes(&mut self, n: usize) -> Result<&[u8]> {
        anyhow::ensure!(n <= self.0.len(), "map: truncated format data");
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into()?)
    }
}
