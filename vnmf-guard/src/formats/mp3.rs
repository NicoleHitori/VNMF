// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! MP3: parity in an ID3v2 `XVNM` frame at the start of the file.
//!
//! A tail after the audio won't do: decoders search it for a sync word and "play" garbage.
//! An ID3v2 tag, on the other hand, is skipped entirely by size. Frames with ids starting with X are experimental
//! (ID3v2.4, 4) and unknown to everyone, so they are skipped silently. `PRIV` doesn't fit: ffmpeg
//! turns its contents into a metadata string. An existing v2.3/v2.4 tag is extended (GStreamer
//! no longer recognizes a second tag in a row); without a tag, a v2.4 one is created.

use anyhow::{Result, bail, ensure};

use super::{Built, Extra, Seg, parity_area};
use crate::plan::Plan;

const FRAME_ID: &[u8; 4] = b"XVNM";
/// Tag size — a 28-bit syncsafe integer.
const MAX_TAG: u64 = (1 << 28) - 1;

fn syncsafe(v: u32) -> [u8; 4] {
    [(v >> 21) as u8 & 0x7F, (v >> 14) as u8 & 0x7F, (v >> 7) as u8 & 0x7F, v as u8 & 0x7F]
}

fn from_syncsafe(b: &[u8]) -> u32 {
    b.iter().take(4).fold(0, |acc, &x| acc << 7 | u32::from(x & 0x7F))
}

/// End of the ID3v2 tag at the start of the file (including the footer), if present.
pub(super) fn id3_end(d: &[u8]) -> Option<usize> {
    if !d.starts_with(b"ID3") || d.len() < 10 || d[6..10].iter().any(|&b| b & 0x80 != 0) {
        return None;
    }
    let footer = if d[3] == 4 && d[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + from_syncsafe(&d[6..10]) as usize + footer)
}

/// MPEG audio frame sync word with plausible fields (a file without a tag).
pub(super) fn is_frame_sync(d: &[u8]) -> bool {
    d.len() >= 4
        && d[0] == 0xFF
        && d[1] & 0xE0 == 0xE0
        && d[1] & 0x06 != 0 // layer
        && d[2] >> 4 != 0x0F // bitrate
        && (d[2] >> 2) & 0x03 != 0x03 // sample rate
}

/// End of the tag frames (start of padding). `syncsafe` — frame sizes in v2.4.
fn frames_end(d: &[u8], end: usize, syncsafe: bool) -> Option<usize> {
    let mut pos = 10;
    while pos + 10 <= end {
        let id = &d[pos..pos + 4];
        if id[0] == 0 {
            break;
        }
        if !id.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
            return None;
        }
        let size = if syncsafe {
            from_syncsafe(&d[pos + 4..pos + 8])
        } else {
            u32::from_be_bytes(d[pos + 4..pos + 8].try_into().ok()?)
        };
        pos = pos.checked_add(10 + size as usize)?;
    }
    (pos <= end).then_some(pos)
}

pub(super) fn build(orig: &[u8], payload: u64, map: u64) -> Result<Built> {
    let data = 2 * map + payload;
    let parity = parity_area(payload, map);

    let Some(end) = id3_end(orig) else {
        let size = 10 + data;
        ensure!(size <= MAX_TAG, "MP3: parity doesn't fit into an ID3v2 tag (up to 256 MiB) — reduce the percentage");
        let mut head = b"ID3\x04\x00\x00".to_vec();
        head.extend_from_slice(&syncsafe(size as u32));
        head.extend_from_slice(FRAME_ID);
        head.extend_from_slice(&syncsafe(data as u32));
        head.extend_from_slice(&[0, 0]);
        let mut segs = vec![Seg::Bytes(head)];
        segs.extend(parity);
        segs.push(Seg::Orig(0..orig.len()));
        return Ok(Built { segs, extra: vec![0] });
    };

    let (major, flags) = (orig[3], orig[5]);
    ensure!(major == 3 || major == 4, "MP3: ID3v2.{major} tag is not supported (only 2.3 and 2.4)");
    ensure!(flags & 0x80 == 0, "MP3: ID3v2 tag with unsynchronisation is not supported");
    ensure!(flags & 0x40 == 0, "MP3: ID3v2 tag with an extended header is not supported");
    ensure!(flags & 0x10 == 0, "MP3: ID3v2 tag with a footer is not supported");
    ensure!(end <= orig.len(), "MP3: ID3v2 tag is longer than the file");
    // Some programs write non-syncsafe frame sizes in v2.4 — try both variants.
    let insert = frames_end(orig, end, major == 4)
        .or_else(|| frames_end(orig, end, false))
        .ok_or_else(|| anyhow::anyhow!("MP3: corrupted ID3v2 frame structure"))?;
    let size = (end - 10) as u64 + 10 + data;
    ensure!(size <= MAX_TAG, "MP3: parity doesn't fit into an ID3v2 tag (up to 256 MiB) — reduce the percentage");

    let mut head = orig[..6].to_vec();
    head.extend_from_slice(&syncsafe(size as u32));
    let mut frame = FRAME_ID.to_vec();
    frame.extend_from_slice(&if major == 4 { syncsafe(data as u32) } else { (data as u32).to_be_bytes() });
    frame.extend_from_slice(&[0, 0]);

    let mut segs = vec![Seg::Bytes(head), Seg::Orig(10..insert), Seg::Bytes(frame)];
    segs.extend(parity);
    segs.push(Seg::Orig(insert..orig.len()));
    let mut extra = vec![1];
    extra.extend_from_slice(&(insert as u64).to_le_bytes());
    Ok(Built { segs, extra })
}

pub(super) fn strip(file: &[u8], plan: &Plan) -> Result<Vec<u8>> {
    let mut extra = Extra(&plan.extra);
    let had_tag = extra.u8()? == 1;
    let at = if had_tag { extra.usize()? } else { 10 };
    let Some(frame) = file.get(at..at + 10).filter(|f| f.starts_with(FRAME_ID)) else {
        bail!("MP3: parity frame not found")
    };
    let data =
        if file[3] == 4 { from_syncsafe(&frame[4..8]) } else { u32::from_be_bytes(frame[4..8].try_into()?) } as usize;
    let after = at + 10 + data;
    ensure!(after <= file.len(), "MP3: parity frame is longer than the file");

    if !had_tag {
        return Ok(file[after..].to_vec());
    }
    let mut out = file[..at].to_vec();
    let size = from_syncsafe(&file[6..10]) - 10 - data as u32;
    out[6..10].copy_from_slice(&syncsafe(size));
    out.extend_from_slice(&file[after..]);
    Ok(out)
}
