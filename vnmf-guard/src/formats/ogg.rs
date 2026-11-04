// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Ogg Opus / Vorbis: parity is binary data at the end of the comment packet.
//!
//! RFC 7845 §5.2 explicitly allows data after the OpusTags comment list (first byte 0 —
//! "padding, a tag editor may discard it"); the Vorbis decoder reads the comments up to the framing
//! bit and ignores the rest too. A tail after the last page won't do: ffmpeg looks for a
//! sync word only within 64 KiB and stops playback, and the duration is taken from the last
//! page.
//!
//! The comment packet becomes large, so the header packets are re-paginated (each starting on a new
//! page, as libopusenc and libvorbis do), and the numbers of all following pages are shifted.
//! Pages that hold parity or the map have a CRC that depends on the parity — these fields
//! are auxiliary (`OggCrc`). Map copies start on a new page and fit entirely within it.
//!
//! Comment packet: `[original][0][zeros up to a multiple of 255][map][parity][map]`.

use anyhow::{Result, bail, ensure};

use super::{Built, Extra, Seg};
use crate::plan::Plan;

const HEADER: usize = 27;
/// Largest page body: 255 segments of 255 bytes.
const MAX_BODY: u64 = 255 * 255;
const FLAG_CONTINUED: u8 = 1;
const FLAG_BOS: u8 = 2;

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            r = if r & 0x8000_0000 != 0 { (r << 1) ^ 0x04C1_1DB7 } else { r << 1 };
            j += 1;
        }
        t[i] = r;
        i += 1;
    }
    t
};

fn crc_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc = (crc << 8) ^ CRC_TABLE[((crc >> 24) as u8 ^ b) as usize];
    }
    crc
}

/// Page CRC (header + body), the CRC field is treated as zero.
fn crc_parts(header: &[u8], body: &[u8]) -> u32 {
    let crc = crc_update(crc_update(crc_update(0, &header[..22]), &[0; 4]), &header[26..]);
    crc_update(crc, body)
}

/// CRC of a whole page.
pub(crate) fn page_crc(page: &[u8]) -> u32 {
    let n = HEADER + page[26] as usize;
    crc_parts(&page[..n], &page[n..])
}

/// Page length from its header (needs at least 27 bytes + the segment table).
pub(crate) fn page_len(d: &[u8]) -> Option<usize> {
    let segs = *d.get(26)? as usize;
    let lacing = d.get(HEADER..HEADER + segs)?;
    Some(HEADER + segs + lacing.iter().map(|&l| l as usize).sum::<usize>())
}

#[derive(Debug, Clone, Copy)]
struct Page {
    start: usize,
    end: usize,
    flags: u8,
    granule: u64,
    serial: u32,
    seq: u32,
    segs: usize,
}

impl Page {
    fn lacing<'a>(&self, d: &'a [u8]) -> &'a [u8] {
        &d[self.start + HEADER..self.start + HEADER + self.segs]
    }

    fn body(&self) -> usize {
        self.start + HEADER + self.segs
    }
}

/// Splits the whole file into pages; any garbage or bad CRC is an error.
fn pages(d: &[u8]) -> Result<Vec<Page>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < d.len() {
        let h = &d[pos..];
        ensure!(h.starts_with(b"OggS") && h.len() >= HEADER && h[4] == 0, "Ogg: no page at {pos}");
        let len = page_len(h).filter(|&l| l <= h.len()).ok_or_else(|| anyhow::anyhow!("Ogg: truncated page at {pos}"))?;
        let page = &h[..len];
        ensure!(page_crc(page) == u32::from_le_bytes(page[22..26].try_into()?), "Ogg: wrong page CRC at {pos}");
        out.push(Page {
            start: pos,
            end: pos + len,
            flags: page[5],
            granule: u64::from_le_bytes(page[6..14].try_into()?),
            serial: u32::from_le_bytes(page[14..18].try_into()?),
            seq: u32::from_le_bytes(page[18..22].try_into()?),
            segs: page[26] as usize,
        });
        pos += len;
    }
    ensure!(!out.is_empty(), "Ogg: empty file");
    Ok(out)
}

fn page_header(flags: u8, granule: u64, serial: u32, seq: u32, lacing: &[u8]) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER + lacing.len());
    h.extend_from_slice(b"OggS\0");
    h.push(flags);
    h.extend_from_slice(&granule.to_le_bytes());
    h.extend_from_slice(&serial.to_le_bytes());
    h.extend_from_slice(&seq.to_le_bytes());
    h.extend_from_slice(&[0; 4]);
    h.push(lacing.len() as u8);
    h.extend_from_slice(lacing);
    h
}

fn set_crc(header: &mut [u8], body: &[u8]) {
    let crc = crc_parts(header, body);
    header[22..26].copy_from_slice(&crc.to_le_bytes());
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Codec {
    Opus = 1,
    Vorbis = 2,
}

impl Codec {
    fn header_packets(self) -> usize {
        match self {
            Codec::Opus => 2,
            Codec::Vorbis => 3,
        }
    }
}

/// Header packets and how many pages they occupy.
struct Headers {
    codec: Codec,
    packets: Vec<Vec<u8>>,
    pages: usize,
}

fn headers(d: &[u8], pages: &[Page]) -> Result<Headers> {
    let serial = pages[0].serial;
    ensure!(pages[0].flags & FLAG_BOS != 0, "Ogg: the first page is not the beginning of a stream");
    ensure!(pages.iter().all(|p| p.serial == serial), "Ogg: multiple logical streams (multiplexed or chained file) are not supported");

    let mut codec = None;
    let mut packets = Vec::new();
    let mut current = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let mut pos = page.body();
        let lacing = page.lacing(d);
        for (k, &l) in lacing.iter().enumerate() {
            current.extend_from_slice(&d[pos..pos + l as usize]);
            pos += l as usize;
            if l == 255 {
                continue;
            }
            let packet = std::mem::take(&mut current);
            if packets.is_empty() {
                codec = Some(if packet.starts_with(b"OpusHead") {
                    Codec::Opus
                } else if packet.starts_with(b"\x01vorbis") {
                    Codec::Vorbis
                } else {
                    bail!("Ogg: only Opus and Vorbis are supported")
                });
            }
            packets.push(packet);
            let codec = codec.expect("codec is determined by the first packet");
            if packets.len() == codec.header_packets() {
                ensure!(k + 1 == lacing.len(), "Ogg: audio data on the same page as the headers");
                let comment = &packets[1];
                let expected: &[u8] = if codec == Codec::Opus { b"OpusTags" } else { b"\x03vorbis" };
                ensure!(comment.starts_with(expected), "Ogg: the second packet is not the comments");
                return Ok(Headers { codec, packets, pages: i + 1 });
            }
        }
    }
    bail!("Ogg: file ended in the middle of the headers")
}

/// Part of a packet during pagination.
enum Area<'a> {
    Keep(&'a [u8]),
    Map(u64),
    Payload(u64),
}

impl Area<'_> {
    fn len(&self) -> u64 {
        match self {
            Area::Keep(b) => b.len() as u64,
            Area::Map(n) | Area::Payload(n) => *n,
        }
    }
}

/// Paginates one packet starting on a new page. `breaks` — offsets in the packet (multiples of 255) where
/// a new page is forced to start. The page on which the packet ends has granule 0,
/// the rest — −1 (as in libogg).
fn paginate(areas: &[Area], breaks: &[u64], serial: u32, seq: &mut u32, bos: bool, segs: &mut Vec<Seg>) -> Result<()> {
    let total: u64 = areas.iter().map(Area::len).sum();
    let full = total / 255;
    let count = full + 1;
    let mut first = 0;
    while first < count {
        let mut last = first + 1;
        while last < count && last - first < 255 && !breaks.contains(&(last * 255)) {
            last += 1;
        }
        let lacing: Vec<u8> = (first..last).map(|i| if i < full { 255 } else { (total % 255) as u8 }).collect();
        let (from, to) = (first * 255, if last == count { total } else { last * 255 });
        let flags = if first > 0 { FLAG_CONTINUED } else { 0 } | if bos && first == 0 { FLAG_BOS } else { 0 };
        let granule = if last == count { 0 } else { u64::MAX };
        let mut header = page_header(flags, granule, serial, *seq, &lacing);

        let mut body = Vec::new();
        let mut known = Vec::new();
        let mut offset = 0;
        for area in areas {
            let (a, b) = (offset, offset + area.len());
            offset = b;
            let (s, e) = (a.max(from), b.min(to));
            if s >= e {
                continue;
            }
            match area {
                Area::Keep(bytes) => {
                    let part = &bytes[(s - a) as usize..(e - a) as usize];
                    known.extend_from_slice(part);
                    body.push(Seg::Bytes(part.to_vec()));
                }
                Area::Map(n) => {
                    ensure!(s == a && e == b, "Ogg: a map copy doesn't fit into a single page");
                    body.push(Seg::Map(*n));
                }
                Area::Payload(_) => body.push(Seg::Payload(e - s)),
            }
        }
        if known.len() as u64 == to - from {
            set_crc(&mut header, &known);
            header.extend_from_slice(&known);
            segs.push(Seg::Bytes(header));
        } else {
            segs.extend([Seg::Bytes(header[..22].to_vec()), Seg::OggCrc, Seg::Bytes(header[26..].to_vec())]);
            segs.extend(body);
        }
        *seq = seq.wrapping_add(1);
        first = last;
    }
    Ok(())
}

/// Pages after the headers — with numbers shifted by `delta`.
fn renumbered(d: &[u8], pages: &[Page], delta: u32, segs: &mut Vec<Seg>) {
    for page in pages {
        if delta == 0 {
            segs.push(Seg::Orig(page.start..page.end));
            continue;
        }
        let mut header = d[page.start..page.body()].to_vec();
        header[18..22].copy_from_slice(&page.seq.wrapping_add(delta).to_le_bytes());
        set_crc(&mut header, &d[page.body()..page.end]);
        segs.extend([Seg::Bytes(header), Seg::Orig(page.body()..page.end)]);
    }
}

pub(super) fn build(orig: &[u8], payload: u64, map: u64) -> Result<Built> {
    let pages = pages(orig)?;
    let h = headers(orig, &pages)?;
    let serial = pages[0].serial;
    let first_seq = pages[0].seq;

    let comment = &h.packets[1];
    let prefix_len = (comment.len() + 1).next_multiple_of(255);
    let mut prefix = comment.clone();
    prefix.resize(prefix_len, 0);
    let map = map.next_multiple_of(255);
    ensure!(map <= MAX_BODY, "Ogg: the map doesn't fit into a single page");
    let payload = payload.next_multiple_of(255);
    let areas = [Area::Keep(&prefix), Area::Map(map), Area::Payload(payload), Area::Map(map)];
    let breaks = [prefix_len as u64, prefix_len as u64 + map + payload];

    let mut segs = Vec::new();
    let mut seq = first_seq;
    paginate(&[Area::Keep(&h.packets[0])], &[], serial, &mut seq, true, &mut segs)?;
    paginate(&areas, &breaks, serial, &mut seq, false, &mut segs)?;
    for packet in &h.packets[2..] {
        paginate(&[Area::Keep(packet)], &[], serial, &mut seq, false, &mut segs)?;
    }
    let new_pages = seq.wrapping_sub(first_seq);
    renumbered(orig, &pages[h.pages..], new_pages.wrapping_sub(h.pages as u32), &mut segs);

    // For removing protection: the comment length and the layout of the original header pages.
    let mut extra = vec![h.codec as u8];
    extra.extend_from_slice(&(comment.len() as u64).to_le_bytes());
    extra.extend_from_slice(&new_pages.to_le_bytes());
    extra.extend_from_slice(&(h.pages as u32).to_le_bytes());
    for page in &pages[..h.pages] {
        extra.push(page.flags);
        extra.extend_from_slice(&page.granule.to_le_bytes());
        extra.push(page.segs as u8);
        extra.extend_from_slice(page.lacing(orig));
    }
    Ok(Built { segs, extra })
}

pub(super) fn strip(file: &[u8], plan: &Plan) -> Result<Vec<u8>> {
    let mut extra = Extra(&plan.extra);
    let _codec = extra.u8()?;
    let comment_len = extra.usize()?;
    let (new_pages, old_pages) = (extra.u32()?, extra.u32()? as usize);

    let pages = pages(file)?;
    let mut h = headers(file, &pages)?;
    ensure!(h.pages == new_pages as usize && comment_len <= h.packets[1].len(), "Ogg: headers don't match the map");
    h.packets[1].truncate(comment_len);
    let stream = h.packets.concat();

    let (serial, first_seq) = (pages[0].serial, pages[0].seq);
    let mut out = Vec::with_capacity(file.len());
    let mut pos = 0;
    for i in 0..old_pages {
        let flags = extra.u8()?;
        let granule = extra.u64()?;
        let segs = extra.u8()? as usize;
        let lacing = extra.bytes(segs)?;
        let len: usize = lacing.iter().map(|&l| l as usize).sum();
        let Some(body) = stream.get(pos..pos + len) else { bail!("Ogg: headers are shorter than the original pages") };
        let mut header = page_header(flags, granule, serial, first_seq.wrapping_add(i as u32), lacing);
        set_crc(&mut header, body);
        out.extend_from_slice(&header);
        out.extend_from_slice(body);
        pos += len;
    }
    ensure!(pos == stream.len(), "Ogg: headers are longer than the original pages");

    let mut segs = Vec::new();
    renumbered(file, &pages[h.pages..], (old_pages as u32).wrapping_sub(new_pages), &mut segs);
    for seg in segs {
        match seg {
            Seg::Bytes(b) => out.extend_from_slice(&b),
            Seg::Orig(r) => out.extend_from_slice(&file[r]),
            _ => unreachable!("renumbering yields only bytes"),
        }
    }
    Ok(out)
}
