// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Map of a protected file: which bytes are data under parity protection, which are the parity section itself,
//! and which are auxiliary and are simply regenerated after recovery.
//!
//! A protected file is a sequence of [`Piece`]s. If only the `Keep` ones are left,
//! you get the **protected area** (padded to a block boundary with virtual zeros that aren't in the file),
//! and all `Payload`s together are the `vnmf-parity` **parity section**. `write_parity` / `check` / `repair`
//! operate on this "virtual file" `[protected area][parity]`.
//!
//! Auxiliary pieces are not protected by parity because their contents depend on it:
//! `Map` — copies of this map, `OggCrc` — CRCs of the Ogg pages that contain the parity.
//! After the data is recovered they are simply recomputed.
//!
//! Map copy: `VNMFMAP1`, u32 LE body length, body, CRC32 (of everything before), then zeros
//! to the end of the piece. There are at least two copies, so the map survives damage to one of them.

use vnmf_parity::{BlockSize, ParityLayout};

use crate::Format;

/// Map copy signature.
pub const MAP_MAGIC: [u8; 8] = *b"VNMFMAP1";
const VERSION: u8 = 1;
/// Protection against a corrupted (or deliberately malicious) map: this many pieces is enough for any format.
const MAX_PIECES: usize = 1 << 22;
const MAX_BODY: usize = 16 << 20;
/// Longest repeating piece pattern that gets compressed (an Ogg page is 4 pieces).
const MAX_PERIOD: usize = 8;

/// A piece of the protected file (in layout order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    /// Bytes of the protected area.
    Keep(u64),
    /// Bytes of the parity section (zeros beyond its length).
    Payload(u64),
    /// A map copy.
    Map(u64),
    /// Ogg page CRC field (4 bytes).
    OggCrc,
}

impl Piece {
    pub fn len(self) -> u64 {
        match self {
            Piece::Keep(n) | Piece::Payload(n) | Piece::Map(n) => n,
            Piece::OggCrc => 4,
        }
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    fn tag(self) -> u64 {
        match self {
            Piece::Keep(n) => n << 3,
            Piece::Payload(n) => n << 3 | 1,
            Piece::Map(n) => n << 3 | 2,
            Piece::OggCrc => 3,
        }
    }
}

/// Tag of the "repeat the previous `p` pieces `r` times" entry (low 3 bits).
const TAG_REPEAT: u64 = 4;

/// Full layout of a protected file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub format: Format,
    pub block: BlockSize,
    pub redundancy: u8,
    /// Length and CRC32 of the file before protection — to confirm that removing protection returned exactly it.
    pub orig_len: u64,
    pub orig_crc: u32,
    /// What the format needs in order to remove protection (see `formats::strip`).
    pub extra: Vec<u8>,
    pub pieces: Vec<Piece>,
}

impl Plan {
    fn sum(&self, pred: impl Fn(Piece) -> bool) -> u64 {
        self.pieces.iter().filter(|&&p| pred(p)).map(|p| p.len()).sum()
    }

    /// Length of the protected area (without the virtual padding to a block).
    pub fn protected_len(&self) -> u64 {
        self.sum(|p| matches!(p, Piece::Keep(_)))
    }

    pub fn payload_len(&self) -> u64 {
        self.sum(|p| matches!(p, Piece::Payload(_)))
    }

    /// Size of the protected file.
    pub fn total_len(&self) -> u64 {
        self.sum(|_| true)
    }

    /// Layout of the virtual file's parity section.
    pub fn layout(&self) -> Option<ParityLayout> {
        let protected = self.protected_len();
        ParityLayout::new(self.block.align_up(protected), self.block, self.redundancy)
    }

    /// Positions and capacity of all map copies in the file.
    pub fn map_slots(&self) -> Vec<(u64, u64)> {
        let mut pos = 0;
        let mut slots = Vec::new();
        for &piece in &self.pieces {
            if let Piece::Map(len) = piece {
                slots.push((pos, len));
            }
            pos += piece.len();
        }
        slots
    }

    /// Serializes the map (a single copy, without zero padding).
    pub fn encode(&self) -> Vec<u8> {
        let mut body = vec![VERSION, self.format as u8, self.block.shift(), self.redundancy];
        body.extend_from_slice(&self.orig_len.to_le_bytes());
        body.extend_from_slice(&self.orig_crc.to_le_bytes());
        put_varint(&mut body, self.extra.len() as u64);
        body.extend_from_slice(&self.extra);
        encode_pieces(&self.pieces, &mut body);

        let mut out = Vec::with_capacity(body.len() + 16);
        out.extend_from_slice(&MAP_MAGIC);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        let crc = crc32fast::hash(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// Parses a map copy at the start of `data`. `None` — not a map or damaged.
    pub fn decode(data: &[u8]) -> Option<Plan> {
        if data.get(..8)? != MAP_MAGIC {
            return None;
        }
        let len = u32::from_le_bytes(data.get(8..12)?.try_into().ok()?) as usize;
        if len > MAX_BODY {
            return None;
        }
        let record = data.get(..12 + len + 4)?;
        let (signed, crc) = record.split_at(12 + len);
        if crc32fast::hash(signed) != u32::from_le_bytes(crc.try_into().ok()?) {
            return None;
        }

        let mut r = Reader(&signed[12..]);
        let [version, format, shift, redundancy] = r.take(4)?.try_into().ok()?;
        if version != VERSION {
            return None;
        }
        let format = Format::try_from(format).ok()?;
        let block = BlockSize::try_from(shift).ok()?;
        let orig_len = u64::from_le_bytes(r.take(8)?.try_into().ok()?);
        let orig_crc = u32::from_le_bytes(r.take(4)?.try_into().ok()?);
        let extra_len = usize::try_from(r.varint()?).ok()?;
        let extra = r.take(extra_len)?.to_vec();
        let pieces = decode_pieces(&mut r)?;
        let plan = Plan { format, block, redundancy, orig_len, orig_crc, extra, pieces };
        // The size must be plausible and the layout must exist.
        (plan.total_len() < 1 << 52 && plan.layout().is_some()).then_some(plan)
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.0.len() {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }

    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.take(1)?.first()?;
            v |= u64::from(b & 0x7f) << shift;
            if b < 0x80 {
                return Some(v);
            }
        }
        None
    }
}

/// Pieces with simple repeat compression: Ogg pages, gzip members, FLAC blocks follow identical
/// patterns, so thousands of pieces take a few dozen bytes.
fn encode_pieces(pieces: &[Piece], out: &mut Vec<u8>) {
    let mut entries: Vec<u64> = Vec::new();
    let mut i = 0;
    while i < pieces.len() {
        let (mut period, mut repeats) = (1, 0);
        for p in 1..=MAX_PERIOD.min(pieces.len() - i) {
            let pattern = &pieces[i..i + p];
            let mut r = 0;
            while i + p * (r + 2) <= pieces.len() && &pieces[i + p * (r + 1)..i + p * (r + 2)] == pattern {
                r += 1;
            }
            if r * p > repeats * period {
                (period, repeats) = (p, r);
            }
        }
        entries.extend(pieces[i..i + period].iter().map(|p| p.tag()));
        if repeats > 0 {
            entries.push((period as u64) << 3 | TAG_REPEAT);
            entries.push(repeats as u64);
        }
        i += period * (repeats + 1);
    }
    put_varint(out, entries.len() as u64);
    for e in entries {
        put_varint(out, e);
    }
}

fn decode_pieces(r: &mut Reader) -> Option<Vec<Piece>> {
    let count = r.varint()?;
    let mut pieces = Vec::new();
    let mut read = 0;
    while read < count {
        let tag = r.varint()?;
        read += 1;
        let n = tag >> 3;
        let piece = match tag & 7 {
            0 => Piece::Keep(n),
            1 => Piece::Payload(n),
            2 => Piece::Map(n),
            3 => Piece::OggCrc,
            TAG_REPEAT => {
                let times = r.varint()?;
                read += 1;
                let period = usize::try_from(n).ok().filter(|&p| (1..=MAX_PERIOD).contains(&p) && p <= pieces.len())?;
                let total = usize::try_from(times).ok()?.checked_mul(period)?;
                if pieces.len() + total > MAX_PIECES {
                    return None;
                }
                let start = pieces.len() - period;
                for _ in 0..times {
                    pieces.extend_from_within(start..start + period);
                }
                continue;
            }
            _ => return None,
        };
        if pieces.len() >= MAX_PIECES {
            return None;
        }
        pieces.push(piece);
    }
    Some(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pieces: Vec<Piece>) -> Plan {
        Plan { format: Format::Ogg, block: BlockSize::Page4k, redundancy: 10, orig_len: 12345, orig_crc: 0xDEAD_BEEF, extra: vec![1, 2, 3], pieces }
    }

    #[test]
    fn roundtrip_with_repeats() {
        let mut pieces = vec![Piece::Keep(100), Piece::Map(510)];
        for _ in 0..1000 {
            pieces.extend([Piece::Keep(22), Piece::OggCrc, Piece::Keep(256), Piece::Payload(65025)]);
        }
        pieces.extend([Piece::Payload(7), Piece::Map(510), Piece::Keep(1 << 40)]);
        let plan = sample(pieces);
        let bytes = plan.encode();
        assert!(bytes.len() < 100, "repeats were not compressed: {} bytes", bytes.len());
        let mut padded = bytes.clone();
        padded.resize(bytes.len() + 50, 0);
        assert_eq!(Plan::decode(&padded), Some(plan));
    }

    #[test]
    fn damaged_map_is_rejected() {
        let mut bytes = sample(vec![Piece::Keep(5000), Piece::Map(300), Piece::Payload(4096)]).encode();
        bytes[20] ^= 1;
        assert_eq!(Plan::decode(&bytes), None);
    }
}
