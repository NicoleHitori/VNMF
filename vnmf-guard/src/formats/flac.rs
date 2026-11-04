// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! FLAC: parity in `APPLICATION` metadata blocks with id `VNMF` after the existing metadata.
//!
//! libFLAC treats a tail after the audio frames as lost sync, while unknown `APPLICATION`
//! blocks are skipped by all decoders. A block holds up to 16 MiB, so parity is split into
//! several blocks: the first is a copy of the map, then the parity, the last is the map again.

use anyhow::{Result, bail, ensure};

use super::{Built, Extra, Seg, mp3};
use crate::plan::Plan;

const APP_ID: &[u8; 4] = b"VNMF";
const TYPE_APPLICATION: u8 = 2;
/// Data per block (block length is 24 bits, 4 bytes of which are the application id).
const CHUNK: u64 = (1 << 24) - 4096;

/// Parses the metadata: (header position of the last block, end of metadata).
fn metadata(d: &[u8]) -> Result<(usize, usize)> {
    let start = mp3::id3_end(d).unwrap_or(0);
    ensure!(d.get(start..start + 4) == Some(b"fLaC"), "FLAC: no fLaC signature");
    let mut pos = start + 4;
    loop {
        ensure!(pos + 4 <= d.len(), "FLAC: truncated metadata");
        let head = d[pos];
        ensure!(head & 0x7F != 0x7F, "FLAC: corrupted metadata block at {pos}");
        let len = u32::from_be_bytes([0, d[pos + 1], d[pos + 2], d[pos + 3]]) as usize;
        let next = pos + 4 + len;
        ensure!(next <= d.len(), "FLAC: metadata block is longer than the file");
        if head & 0x80 != 0 {
            return Ok((pos, next));
        }
        pos = next;
    }
}

fn block_head(len: u64, last: bool) -> Vec<u8> {
    let mut h = vec![TYPE_APPLICATION | if last { 0x80 } else { 0 }];
    h.extend_from_slice(&(len as u32 + 4).to_be_bytes()[1..]);
    h.extend_from_slice(APP_ID);
    h
}

pub(super) fn build(orig: &[u8], payload: u64, map: u64) -> Result<Built> {
    ensure!(map <= CHUNK, "FLAC: map is too large");
    let (last, insert) = metadata(orig)?;

    let mut segs = vec![Seg::Orig(0..last), Seg::Bytes(vec![orig[last] & 0x7F]), Seg::Orig(last + 1..insert)];
    segs.extend([Seg::Bytes(block_head(map, false)), Seg::Map(map)]);
    let mut blocks = 2u32;
    let mut left = payload;
    while left > 0 {
        let n = left.min(CHUNK);
        segs.extend([Seg::Bytes(block_head(n, false)), Seg::Payload(n)]);
        left -= n;
        blocks += 1;
    }
    segs.extend([Seg::Bytes(block_head(map, true)), Seg::Map(map)]);
    segs.push(Seg::Orig(insert..orig.len()));

    let mut extra = (last as u64).to_le_bytes().to_vec();
    extra.extend_from_slice(&(insert as u64).to_le_bytes());
    extra.extend_from_slice(&blocks.to_le_bytes());
    Ok(Built { segs, extra })
}

pub(super) fn strip(file: &[u8], plan: &Plan) -> Result<Vec<u8>> {
    let mut extra = Extra(&plan.extra);
    let (last, insert, blocks) = (extra.usize()?, extra.usize()?, extra.u32()?);
    ensure!(last < insert && insert <= file.len(), "FLAC: corrupted map data");
    let mut pos = insert;
    for _ in 0..blocks {
        let Some(h) = file.get(pos..pos + 8).filter(|h| h[0] & 0x7F == TYPE_APPLICATION && &h[4..8] == APP_ID) else {
            bail!("FLAC: parity block not found at {pos}")
        };
        pos += 4 + u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize;
    }
    ensure!(pos <= file.len(), "FLAC: parity block is longer than the file");
    let mut out = file[..insert].to_vec();
    out[last] |= 0x80;
    out.extend_from_slice(&file[pos..]);
    Ok(out)
}
