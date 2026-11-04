// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Reed–Solomon redundancy for protection against random filesystem faults.
//!
//! A separate crate without the rest of the VNMF format: a parity section can be appended after any
//! block-aligned area of any file ([`write_parity`]), and then checked ([`check`]),
//! repaired ([`repair`]) or found without any other metadata ([`find_layout`]). Dependencies —
//! only `crc32fast` and `reed-solomon-simd`. In VNMF the protected area is everything up to the header's
//! `parity_offset`; `vnmf-core` re-exports this crate as `vnmf_core::parity`.
//!
//! # How it works
//!
//! The **protected area** is everything from the start of the file to the start of the section (in VNMF — `Header::parity_offset`:
//! the primary header, Info, tables and all data). It is split into block-sized **shards**.
//!
//! Shards are grouped into **stripes** with interleaving: stripe `s` contains blocks `s, s+S, s+2S, …`
//! (S — the number of stripes). For each stripe of K data shards, M parity shards are computed
//! (M ≈ K · redundancy%). Any M damaged shards of a stripe can be recovered.
//! Thanks to interleaving, a contiguous damaged region (a bad sector, a lost filesystem extent)
//! is spread across all stripes: at 30% a contiguous loss of up to ~30% of the file can be recovered.
//!
//! **How damaged shards are found:** a CRC32 is stored for each shard (data and parity).
//! The CRC table is written twice (at the start and at the end of the section), each of its blocks has its own CRC —
//! so damage to the table itself is survivable too.
//!
//! **The code** is a systematic Reed–Solomon over GF(2¹⁶), defined mathematically (FORMAT.md, 11.3),
//! not by a library: [`reed_solomon_simd`] is just one implementation, and the
//! `codec_matches_specification` test checks it against an independent naive implementation of the specification.
//!
//! # Parity section layout
//!
//! ```text
//! parity_offset → [CRC table A][parity shards, interleaved][CRC table B] ← end()
//! ```
//!
//! The whole layout is deterministically derived from `(parity_offset, block_size, redundancy)` in the header.
//! The same parameters are repeated in every CRC table block (the `VNMFPAR1` signature + parameters),
//! so the section can be found even without both headers — simply by signature ([`find_layout`]).
//! CRC table block: a 32-byte description + `(B/4 − 9)` u32 LE entries + CRC32 of everything before.

use std::io::{self, Seek, SeekFrom, Write};

mod blocksize;

pub use blocksize::BlockSize;

/// Signature at the start of every parity CRC table block (the first of them lies exactly at the
/// start of the section). With it the parity can be found even without any other file metadata.
pub const PARITY_MAGIC: [u8; 8] = *b"VNMFPAR1";

/// Maximum redundancy (in percent).
pub const MAX_REDUNDANCY: u8 = 30;

/// Size of the description at the start of every CRC table block.
pub const TABLE_BLOCK_HEADER: usize = 32;

/// CRC entries per table block: the block minus the description and the block's own CRC32.
fn entries_per_block(block: usize) -> usize {
    (block - TABLE_BLOCK_HEADER - 4) / 4
}

/// Which of the two CRC table copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum TableCopy {
    A = 0,
    B = 1,
}

/// Maximum data shards per stripe. More — longer decoding of a single stripe,
/// fewer — more stripes (worse parity distribution for small files isn't critical).
pub const MAX_STRIPE_DATA: usize = 1024;

/// A fully computed parity section layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParityLayout {
    /// Start of the section = length of the protected area.
    pub offset: u64,
    /// Shard size (= block size).
    pub shard_size: usize,
    pub redundancy: u8,
    /// N — number of data shards.
    pub data_shards: usize,
    /// S — number of stripes.
    pub stripes: usize,
    /// K — data shards per stripe (the last positions of incomplete stripes are virtual zeros).
    pub per_stripe: usize,
    /// M — parity shards per stripe.
    pub parity_per_stripe: usize,
    /// Blocks in one copy of the CRC table.
    pub table_blocks: usize,
}

impl ParityLayout {
    /// `None` if there's no redundancy or the parameters are invalid.
    pub fn new(offset: u64, block: BlockSize, redundancy: u8) -> Option<Self> {
        if redundancy == 0 || redundancy > MAX_REDUNDANCY || offset == 0 || !block.is_aligned(offset) {
            return None;
        }
        let shard_size = block.bytes();
        let data_shards = usize::try_from(offset / shard_size as u64).ok()?;
        let stripes = data_shards.div_ceil(MAX_STRIPE_DATA);
        let per_stripe = data_shards.div_ceil(stripes);
        let parity_per_stripe = (per_stripe * redundancy as usize).div_ceil(100).max(1);
        let entries = data_shards + stripes * parity_per_stripe;
        let table_blocks = entries.div_ceil(entries_per_block(shard_size));

        Some(Self { offset, shard_size, redundancy, data_shards, stripes, per_stripe, parity_per_stripe, table_blocks })
    }

    /// Total number of parity shards.
    pub fn parity_shards(&self) -> usize {
        self.stripes * self.parity_per_stripe
    }

    fn table_entries(&self) -> usize {
        self.data_shards + self.parity_shards()
    }

    fn entries_per_table_block(&self) -> usize {
        entries_per_block(self.shard_size)
    }

    fn table_a(&self) -> u64 {
        self.offset
    }

    fn table_b(&self) -> u64 {
        self.offset + ((self.table_blocks + self.parity_shards()) * self.shard_size) as u64
    }

    /// Global index of parity shard `j` of stripe `s` (interleaved like the data).
    pub fn parity_index(&self, stripe: usize, j: usize) -> usize {
        j * self.stripes + stripe
    }

    /// Absolute offset of the parity shard with global index `idx`.
    pub fn parity_shard_offset(&self, idx: usize) -> u64 {
        self.offset + ((self.table_blocks + idx) * self.shard_size) as u64
    }

    /// End of the parity section (block-aligned). The tail copy follows.
    pub fn end(&self) -> u64 {
        self.table_b() + (self.table_blocks * self.shard_size) as u64
    }

    /// Size of the parity section in bytes.
    pub fn len(&self) -> u64 {
        self.end() - self.offset
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Indices of the data shards in stripe `s`.
    pub fn stripe_members(&self, stripe: usize) -> impl Iterator<Item = usize> {
        (stripe..self.data_shards).step_by(self.stripes)
    }

    /// The contiguous region (in bytes) that is guaranteed to be recoverable.
    pub fn max_burst_bytes(&self) -> u64 {
        (self.parity_per_stripe * self.stripes * self.shard_size) as u64
    }

    fn data_shard<'a>(&self, file: &'a [u8], i: usize) -> &'a [u8] {
        &file[i * self.shard_size..(i + 1) * self.shard_size]
    }
}

fn rs_error(e: reed_solomon_simd::Error) -> io::Error {
    io::Error::other(format!("Reed–Solomon: {e}"))
}

/// Computes the parity of stripe `s` from the current contents of the protected area.
fn encode_stripe(protected: &[u8], layout: &ParityLayout, stripe: usize, zero: &[u8]) -> io::Result<Vec<Vec<u8>>> {
    let members: Vec<&[u8]> = layout.stripe_members(stripe).map(|i| layout.data_shard(protected, i)).collect();
    let padding = std::iter::repeat_n(zero, layout.per_stripe - members.len());
    reed_solomon_simd::encode(layout.per_stripe, layout.parity_per_stripe, members.into_iter().chain(padding)).map_err(rs_error)
}

/// Description at the start of a CRC table block: signature and layout parameters (32 bytes).
///
/// ```text
///  0  [8]  "VNMFPAR1"
///  8  u64  parity_offset
/// 16  u32  block number within the table copy (0..T)
/// 20  u8   block_size_shift
/// 21  u8   redundancy
/// 22  u8   copy: 0 — A, 1 — B
/// 23  u8   0
/// 24  u32  T — blocks in one copy
/// 28  u32  0
/// ```
fn block_header(layout: &ParityLayout, copy: TableCopy, index: usize) -> [u8; TABLE_BLOCK_HEADER] {
    let mut h = [0u8; TABLE_BLOCK_HEADER];
    h[0..8].copy_from_slice(&PARITY_MAGIC);
    h[8..16].copy_from_slice(&layout.offset.to_le_bytes());
    h[16..20].copy_from_slice(&(index as u32).to_le_bytes());
    h[20] = layout.shard_size.trailing_zeros() as u8;
    h[21] = layout.redundancy;
    h[22] = copy as u8;
    h[24..28].copy_from_slice(&(layout.table_blocks as u32).to_le_bytes());
    h
}

/// A parsed CRC table block description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockHeader {
    offset: u64,
    index: usize,
    block: BlockSize,
    redundancy: u8,
    copy: TableCopy,
    table_blocks: usize,
}

/// Parses a CRC table block at `pos`: signature, a known block size and the block's own CRC.
fn parse_block(file: &[u8], pos: usize) -> Option<BlockHeader> {
    let head = file.get(pos..pos.checked_add(TABLE_BLOCK_HEADER)?)?;
    if head[0..8] != PARITY_MAGIC {
        return None;
    }
    let block = BlockSize::try_from(head[20]).ok()?;
    let b = block.bytes();
    let body = file.get(pos..pos.checked_add(b)?)?;
    let (data, crc) = body.split_at(b - 4);
    if crc32fast::hash(data) != u32::from_le_bytes(crc.try_into().ok()?) {
        return None;
    }
    let copy = match head[22] {
        0 => TableCopy::A,
        1 => TableCopy::B,
        _ => return None,
    };
    Some(BlockHeader {
        offset: u64::from_le_bytes(head[8..16].try_into().ok()?),
        index: u32::from_le_bytes(head[16..20].try_into().ok()?) as usize,
        block,
        redundancy: head[21],
        copy,
        table_blocks: u32::from_le_bytes(head[24..28].try_into().ok()?) as usize,
    })
}

/// Serializes a CRC table copy: blocks of "description + entries + block CRC32".
fn encode_table(crcs: &[u32], layout: &ParityLayout, copy: TableCopy) -> Vec<u8> {
    let b = layout.shard_size;
    let mut out = Vec::with_capacity(layout.table_blocks * b);
    for (index, chunk) in crcs.chunks(layout.entries_per_table_block()).enumerate() {
        let start = out.len();
        out.extend_from_slice(&block_header(layout, copy, index));
        for crc in chunk {
            out.extend_from_slice(&crc.to_le_bytes());
        }
        out.resize(start + b - 4, 0);
        let block_crc = crc32fast::hash(&out[start..]);
        out.extend_from_slice(&block_crc.to_le_bytes());
    }
    out
}

/// Reads the CRC table, taking each block from the copy (A or B) whose block CRC matches and whose description
/// matches the layout. `None` — for entries whose block is damaged in both copies.
fn decode_table(file: &[u8], layout: &ParityLayout) -> (Vec<Option<u32>>, usize) {
    let b = layout.shard_size;
    let per = layout.entries_per_table_block();
    let mut entries = vec![None; layout.table_entries()];
    let mut damaged_blocks = 0;

    let read_block = |base: u64, copy: TableCopy, i: usize| -> Option<&[u8]> {
        let start = usize::try_from(base).ok()? + i * b;
        let header = parse_block(file, start)?;
        let expected =
            BlockHeader { offset: layout.offset, index: i, block: header.block, redundancy: layout.redundancy, copy, table_blocks: layout.table_blocks };
        (header == expected && header.block.bytes() == b).then(|| &file[start + TABLE_BLOCK_HEADER..start + b - 4])
    };

    for i in 0..layout.table_blocks {
        let a = read_block(layout.table_a(), TableCopy::A, i);
        let bb = read_block(layout.table_b(), TableCopy::B, i);
        damaged_blocks += usize::from(a.is_none()) + usize::from(bb.is_none());
        if let Some(body) = a.or(bb) {
            for (slot, entry) in entries.iter_mut().skip(i * per).take(per).zip(body.as_chunks::<4>().0) {
                *slot = Some(u32::from_le_bytes(*entry));
            }
        }
    }
    (entries, damaged_blocks)
}

/// Writes the full parity section for `protected` (= the file from 0 to `layout.offset`).
/// After the call the position of `out` is `layout.end()`.
pub fn write_parity<W: Write + Seek>(out: &mut W, protected: &[u8], layout: &ParityLayout) -> io::Result<()> {
    assert_eq!(protected.len() as u64, layout.offset, "the protected area doesn't match the layout");
    let zero = vec![0u8; layout.shard_size];

    let mut crcs: Vec<u32> = (0..layout.data_shards).map(|i| crc32fast::hash(layout.data_shard(protected, i))).collect();
    crcs.resize(layout.table_entries(), 0);

    for s in 0..layout.stripes {
        for (j, shard) in encode_stripe(protected, layout, s, &zero)?.iter().enumerate() {
            let idx = layout.parity_index(s, j);
            crcs[layout.data_shards + idx] = crc32fast::hash(shard);
            out.seek(SeekFrom::Start(layout.parity_shard_offset(idx)))?;
            out.write_all(shard)?;
        }
    }

    out.seek(SeekFrom::Start(layout.table_a()))?;
    out.write_all(&encode_table(&crcs, layout, TableCopy::A))?;
    out.seek(SeekFrom::Start(layout.table_b()))?;
    out.write_all(&encode_table(&crcs, layout, TableCopy::B))?;
    out.seek(SeekFrom::Start(layout.end()))?;
    Ok(())
}

/// State of the protected area and parity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParityStatus {
    /// Indices of damaged (or missing — the file is truncated) data shards.
    pub bad_data: Vec<usize>,
    /// Global indices of damaged parity shards.
    pub bad_parity: Vec<usize>,
    /// Shards whose CRC is unknown (the table block is damaged in both copies).
    pub unknown: usize,
    /// Damaged CRC table blocks (total in A and B).
    pub damaged_table_blocks: usize,
    /// Stripes where more shards are damaged than can be recovered.
    pub unrecoverable_stripes: Vec<usize>,
}

impl ParityStatus {
    /// Nothing is damaged (including the tables).
    pub fn is_clean(&self) -> bool {
        self.bad_data.is_empty() && self.bad_parity.is_empty() && self.unknown == 0 && self.damaged_table_blocks == 0
    }

    /// All damage is within the parity's capabilities.
    pub fn is_recoverable(&self) -> bool {
        self.unrecoverable_stripes.is_empty()
    }
}

/// State of the shards of a single stripe: which positions (within the stripe) must be treated as lost.
struct StripeErasures {
    /// Data positions in the stripe (0..count).
    data: Vec<usize>,
    /// Parity positions in the stripe (0..M).
    parity: Vec<usize>,
    /// More than M lost.
    unrecoverable: bool,
}

/// Analyzes all shards. `file` — the whole file (may be shorter than `layout.end()` if truncated).
fn analyze(file: &[u8], layout: &ParityLayout) -> (ParityStatus, Vec<Option<u32>>, Vec<StripeErasures>) {
    let (table, damaged_table_blocks) = decode_table(file, layout);
    let b = layout.shard_size;

    // Some(true) — damaged, Some(false) — intact, None — CRC unknown.
    let shard_state = |start: u64, expected: Option<u32>| -> Option<bool> {
        let start = usize::try_from(start).ok()?;
        match file.get(start..start + b) {
            None => Some(true),
            Some(data) => expected.map(|crc| crc32fast::hash(data) != crc),
        }
    };

    let mut status = ParityStatus { damaged_table_blocks, ..Default::default() };
    let mut stripes = Vec::with_capacity(layout.stripes);

    for s in 0..layout.stripes {
        let mut bad = StripeErasures { data: Vec::new(), parity: Vec::new(), unrecoverable: false };
        let mut unknown = StripeErasures { data: Vec::new(), parity: Vec::new(), unrecoverable: false };

        for (pos, i) in layout.stripe_members(s).enumerate() {
            match shard_state((i * b) as u64, table[i]) {
                Some(true) => {
                    bad.data.push(pos);
                    status.bad_data.push(i);
                }
                Some(false) => {}
                None => unknown.data.push(pos),
            }
        }
        for j in 0..layout.parity_per_stripe {
            let idx = layout.parity_index(s, j);
            match shard_state(layout.parity_shard_offset(idx), table[layout.data_shards + idx]) {
                Some(true) => {
                    bad.parity.push(j);
                    status.bad_parity.push(idx);
                }
                Some(false) => {}
                None => unknown.parity.push(j),
            }
        }
        status.unknown += unknown.data.len() + unknown.parity.len();

        // Shards with unknown CRC are cautiously treated as lost if there's enough parity for that;
        // otherwise — best effort: assume they're intact.
        let m = layout.parity_per_stripe;
        let with_unknown = bad.data.len() + bad.parity.len() + unknown.data.len() + unknown.parity.len();
        if with_unknown <= m {
            bad.data.extend(unknown.data);
            bad.parity.extend(unknown.parity);
        }
        bad.unrecoverable = bad.data.len() + bad.parity.len() > m;
        if bad.unrecoverable {
            status.unrecoverable_stripes.push(s);
        }
        stripes.push(bad);
    }
    status.bad_data.sort_unstable();
    status.bad_parity.sort_unstable();
    (status, table, stripes)
}

/// Checks the integrity of the protected area and parity.
pub fn check(file: &[u8], layout: &ParityLayout) -> ParityStatus {
    analyze(file, layout).0
}

/// Repair result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairReport {
    /// State before repair.
    pub before: ParityStatus,
    pub repaired_data: usize,
    pub repaired_parity: usize,
    /// Stripes that couldn't be recovered (more damage than parity).
    pub failed_stripes: Vec<usize>,
}

impl RepairReport {
    pub fn is_complete(&self) -> bool {
        self.failed_stripes.is_empty()
    }
}

/// Repairs damaged data and parity shards in place and rewrites both CRC tables.
///
/// `file` must be no shorter than `layout.end()` — a truncated file must be padded with zeros
/// before the call (the lost shards then simply won't pass the CRC check).
pub fn repair(file: &mut [u8], layout: &ParityLayout) -> io::Result<RepairReport> {
    if (file.len() as u64) < layout.end() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "the file is shorter than the parity section — pad it"));
    }
    let (before, stored, stripes) = analyze(file, layout);
    let b = layout.shard_size;
    let zero = vec![0u8; b];
    let mut report = RepairReport { before: before.clone(), repaired_data: 0, repaired_parity: 0, failed_stripes: Vec::new() };

    for (s, erasures) in stripes.iter().enumerate() {
        if erasures.data.is_empty() && erasures.parity.is_empty() {
            continue;
        }
        if erasures.unrecoverable {
            report.failed_stripes.push(s);
            continue;
        }

        let members: Vec<usize> = layout.stripe_members(s).collect();

        // 1. Data: decode the lost positions from the surviving data + parity.
        if !erasures.data.is_empty() {
            let restored = {
                let originals = (0..layout.per_stripe).filter(|pos| !erasures.data.contains(pos)).map(|pos| {
                    let shard = members.get(pos).map_or(zero.as_slice(), |&i| layout.data_shard(file, i));
                    (pos, shard)
                });
                let recovery = (0..layout.parity_per_stripe).filter(|j| !erasures.parity.contains(j)).map(|j| {
                    let start = layout.parity_shard_offset(layout.parity_index(s, j)) as usize;
                    (j, &file[start..start + b])
                });
                reed_solomon_simd::decode(layout.per_stripe, layout.parity_per_stripe, originals, recovery).map_err(rs_error)?
            };
            for (pos, data) in restored {
                if let Some(&i) = members.get(pos) {
                    file[i * b..(i + 1) * b].copy_from_slice(&data);
                    report.repaired_data += 1;
                }
            }
        }

        // 2. Parity: recompute from the (already recovered) data and rewrite the damaged shards.
        if !erasures.parity.is_empty() {
            let parity = encode_stripe(&file[..layout.offset as usize], layout, s, &zero)?;
            for &j in &erasures.parity {
                let start = layout.parity_shard_offset(layout.parity_index(s, j)) as usize;
                file[start..start + b].copy_from_slice(&parity[j]);
                report.repaired_parity += 1;
            }
        }
    }

    // 3. CRC tables: recompute for the recovered stripes. For the unrecovered ones keep
    //    the stored CRCs — otherwise damaged data would be "legalized" and no longer detected.
    let failed: Vec<bool> = (0..layout.stripes).map(|s| report.failed_stripes.contains(&s)).collect();
    let mut crcs = Vec::with_capacity(layout.table_entries());
    for i in 0..layout.data_shards {
        let actual = crc32fast::hash(layout.data_shard(file, i));
        crcs.push(if failed[i % layout.stripes] { stored[i].unwrap_or(actual) } else { actual });
    }
    for idx in 0..layout.parity_shards() {
        let start = layout.parity_shard_offset(idx) as usize;
        let actual = crc32fast::hash(&file[start..start + b]);
        let stripe = idx % layout.stripes;
        crcs.push(if failed[stripe] { stored[layout.data_shards + idx].unwrap_or(actual) } else { actual });
    }
    for (base, copy) in [(layout.table_a(), TableCopy::A), (layout.table_b(), TableCopy::B)] {
        let table = encode_table(&crcs, layout, copy);
        let start = base as usize;
        file[start..start + table.len()].copy_from_slice(&table);
    }

    Ok(report)
}

// ---------------------------------------------------------------------------
// Searching for the parity section without a header
// ---------------------------------------------------------------------------

/// A layout found by the signature of the CRC table blocks ([`find_layout`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoundLayout {
    pub layout: ParityLayout,
    /// Intact CRC table blocks confirming this layout (out of `2 · table_blocks`).
    pub blocks: usize,
}

/// Finds the parity section when both header copies are lost.
///
/// Every CRC table block (of both copies) starts with the `VNMFPAR1` signature and the layout
/// parameters and has its own CRC. So it's enough to scan positions that are multiples of 4 KiB (the minimum
/// block): every intact block lying exactly where its own layout says it should is a vote for that layout.
/// The layout with the most votes wins. `None` — no intact block at all.
pub fn find_layout(file: &[u8]) -> Option<FoundLayout> {
    use std::collections::HashMap;

    const STEP: usize = 4096;
    let mut votes: HashMap<(u64, u8, u8), usize> = HashMap::new();
    for pos in (0..file.len()).step_by(STEP) {
        let Some(h) = parse_block(file, pos) else { continue };
        let Some(layout) = ParityLayout::new(h.offset, h.block, h.redundancy) else { continue };
        if h.table_blocks != layout.table_blocks || h.index >= layout.table_blocks {
            continue;
        }
        let base = match h.copy {
            TableCopy::A => layout.table_a(),
            TableCopy::B => layout.table_b(),
        };
        if base + (h.index * layout.shard_size) as u64 == pos as u64 {
            *votes.entry((h.offset, h.block.shift(), h.redundancy)).or_default() += 1;
        }
    }
    let ((offset, shift, redundancy), blocks) = votes.into_iter().max_by_key(|&(key, n)| (n, std::cmp::Reverse(key)))?;
    let layout = ParityLayout::new(offset, BlockSize::try_from(shift).ok()?, redundancy)?;
    Some(FoundLayout { layout, blocks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn layout_for(blocks: usize, redundancy: u8) -> ParityLayout {
        ParityLayout::new((blocks * 4096) as u64, BlockSize::Page4k, redundancy).unwrap()
    }

    fn build(blocks: usize, redundancy: u8) -> (Vec<u8>, ParityLayout) {
        let layout = layout_for(blocks, redundancy);
        let protected: Vec<u8> = (0..blocks * 4096).map(|i| (i * 7 + i / 4096) as u8).collect();
        let mut cur = Cursor::new(protected.clone());
        write_parity(&mut cur, &protected, &layout).unwrap();
        let file = cur.into_inner();
        assert_eq!(file.len() as u64, layout.end());
        (file, layout)
    }

    #[test]
    fn layout_math() {
        let l = layout_for(3000, 30);
        assert_eq!(l.stripes, 3);
        assert_eq!(l.per_stripe, 1000);
        assert_eq!(l.parity_per_stripe, 300);
        assert!(ParityLayout::new(4096, BlockSize::Page4k, 0).is_none());
        assert!(ParityLayout::new(4096, BlockSize::Page4k, 31).is_none());
        assert!(ParityLayout::new(4097, BlockSize::Page4k, 10).is_none());
        // at least one parity shard even for tiny files
        assert_eq!(layout_for(1, 1).parity_per_stripe, 1);
    }

    #[test]
    fn clean_file_is_clean() {
        let (file, layout) = build(50, 10);
        assert!(check(&file, &layout).is_clean());
    }

    #[test]
    fn repairs_burst_within_budget() {
        let (mut file, layout) = build(2500, 20); // 3 stripes
        let original = file.clone();
        // A contiguous region: 400 blocks (≈16% of the protected area) — spread across the stripes.
        for byte in &mut file[100 * 4096..500 * 4096] {
            *byte = 0xAA;
        }
        let report = repair(&mut file, &layout).unwrap();
        assert!(report.is_complete(), "{report:?}");
        assert_eq!(report.repaired_data, 400);
        assert_eq!(file, original);
        assert!(check(&file, &layout).is_clean());
    }

    #[test]
    fn repairs_parity_and_one_table_copy() {
        let (mut file, layout) = build(100, 10);
        let original = file.clone();
        let p = layout.parity_shard_offset(0) as usize;
        file[p] ^= 1;
        file[layout.table_a() as usize + 5] ^= 1; // a table A block is damaged — B is intact
        file[4096 * 7] ^= 1;
        let report = repair(&mut file, &layout).unwrap();
        assert!(report.is_complete());
        assert_eq!((report.repaired_data, report.repaired_parity), (1, 1));
        assert_eq!(report.before.damaged_table_blocks, 1);
        assert_eq!(file, original);
    }

    #[test]
    fn reports_unrecoverable() {
        let (mut file, layout) = build(100, 5);
        for byte in &mut file[..60 * 4096] {
            *byte = 0;
        }
        let report = repair(&mut file, &layout).unwrap();
        assert!(!report.is_complete());
        // Unrecovered data is not "legalized": after the repair it is still detected.
        assert!(!check(&file, &layout).bad_data.is_empty());
    }

    #[test]
    fn truncated_file_is_recoverable_after_padding() {
        let (mut file, layout) = build(200, 30);
        let original = file.clone();
        // The end is lost: table B and part of the parity.
        let cut = layout.table_b() as usize - 4096 * 5;
        file.truncate(cut);
        file.resize(layout.end() as usize, 0);
        let report = repair(&mut file, &layout).unwrap();
        assert!(report.is_complete());
        assert_eq!(file, original);
    }
    /// Both header copies are lost: the layout is found by the signature of the table blocks, and
    /// the parity repairs the damaged start of the file (that's where the primary header is).
    #[test]
    fn finds_layout_without_header() {
        for (blocks, redundancy, block) in
            [(300usize, 10u8, BlockSize::Page4k), (120, 25, BlockSize::Page8k), (2500, 3, BlockSize::Page4k), (40, 30, BlockSize::Ext32k)]
        {
            let b = block.bytes();
            let layout = ParityLayout::new((blocks * b) as u64, block, redundancy).unwrap();
            let protected: Vec<u8> = (0..blocks * b).map(|i| (i * 31 + i / b * 7 + i / 977) as u8).collect();
            let mut cur = Cursor::new(protected.clone());
            write_parity(&mut cur, &protected, &layout).unwrap();
            let mut file = cur.into_inner();
            // The "tail copy" after the parity is lost too (garbage).
            file.extend(std::iter::repeat_n(0x5Au8, 3 * b + 128));
            let original = file.clone();
            // The header (the first block), a piece of data and the first block of table A are destroyed.
            for byte in &mut file[..b] {
                *byte = 0;
            }
            file[5 * b + 10] ^= 0xFF;
            file[layout.offset as usize + 3] ^= 0xFF;

            let found = find_layout(&file).expect("layout found");
            assert_eq!(found.layout, layout, "{blocks} {redundancy}");
            assert_eq!(found.blocks, 2 * layout.table_blocks - 1);
            let report = repair(&mut file, &found.layout).unwrap();
            assert!(report.is_complete());
            assert_eq!(file, original);
        }
    }

    #[test]
    fn finds_layout_from_table_b_when_table_a_is_lost() {
        let (mut file, layout) = build(400, 15);
        let original = file.clone();
        let a = layout.table_a() as usize;
        for byte in &mut file[a..a + layout.table_blocks * 4096] {
            *byte = 0;
        }
        for byte in &mut file[..4096] {
            *byte = 0xFF;
        }
        let found = find_layout(&file).unwrap();
        assert_eq!(found.layout, layout);
        repair(&mut file, &found.layout).unwrap();
        assert_eq!(file, original);
    }

    #[test]
    fn nothing_found_in_random_bytes() {
        let file: Vec<u8> = (0..200 * 4096).map(|i: usize| (i.wrapping_mul(2654435761) >> 7) as u8).collect();
        assert!(find_layout(&file).is_none());
    }

    /// The end of the file is truncated (copy B and part of the parity are lost), the start is destroyed.
    #[test]
    fn finds_layout_in_truncated_file() {
        let (mut file, layout) = build(1500, 10);
        let original = file.clone();
        file.truncate(layout.end() as usize - 60 * 4096);
        for byte in &mut file[..8192] {
            *byte = 0;
        }
        assert_eq!(find_layout(&file).unwrap().layout, layout);
        file.resize(layout.end() as usize, 0);
        assert!(repair(&mut file, &layout).unwrap().is_complete());
        assert_eq!(file, original);
    }

    /// A table block that ended up in the wrong place (e.g. copied from another file) doesn't
    /// vote: its description doesn't match its position.
    #[test]
    fn misplaced_table_block_is_ignored() {
        let (mut file, layout) = build(50, 10);
        let a = layout.table_a() as usize;
        file.copy_within(a..a + 4096, 4096 * 3);
        let found = find_layout(&file).unwrap();
        assert_eq!(found.layout, layout);
        assert_eq!(found.blocks, 2 * layout.table_blocks);
    }

    // --- Codec conformance to the specification (FORMAT.md, 11.3) -------------------------------

    /// GF(2¹⁶) = GF(2)[x] / (x¹⁶ + x⁵ + x³ + x² + 1).
    const GF_POLY: u32 = 0x1002D;
    /// Cantor basis: symbol s (16 bits) denotes the element ⊕ βₖ over the set bits k.
    const CANTOR: [u16; 16] = [0x0001, 0xACCA, 0x3C0E, 0x163E, 0xC582, 0xED2E, 0x914C, 0x4012, 0x6C98, 0x10D8, 0x6A72, 0xB900, 0xFDB8, 0xFB34, 0xFF38, 0x991E];

    fn gf_mul(a: u16, b: u16) -> u16 {
        let (mut a, mut b, mut r) = (u32::from(a), u32::from(b), 0u32);
        while b != 0 {
            if b & 1 != 0 {
                r ^= a;
            }
            b >>= 1;
            a <<= 1;
            if a & 0x10000 != 0 {
                a ^= GF_POLY;
            }
        }
        r as u16
    }

    fn gf_pow(a: u16, mut e: u32) -> u16 {
        let (mut r, mut base) = (1u16, a);
        while e > 0 {
            if e & 1 == 1 {
                r = gf_mul(r, base);
            }
            base = gf_mul(base, base);
            e >>= 1;
        }
        r
    }

    fn phi(s: u16) -> u16 {
        (0..16).filter(|k| s >> k & 1 == 1).fold(0, |acc, k| acc ^ CANTOR[k])
    }

    /// Symbol `e` of a shard: in every 64-byte block the first 32 bytes are the low bytes of 32 symbols,
    /// the next 32 are the high bytes.
    fn symbol(shard: &[u8], e: usize) -> u16 {
        let (block, i) = (e / 32, e % 32);
        u16::from(shard[block * 64 + i]) | u16::from(shard[block * 64 + 32 + i]) << 8
    }

    /// Reference per the specification: m = M rounded up to a power of 2; a codeword c of length 2¹⁶ —
    /// parity at positions 0..m, data at m..m+K, the rest zeros; Σᵢ cᵢ·ωᵢᵗ = 0 for t < m, ωᵢ = φ(i).
    /// Solving the m×m system (Vandermonde in ω₀..ωₘ₋₁) gives m parity symbols, the first M are stored.
    fn reference_parity(data: &[u16], parity: usize) -> Vec<u16> {
        let m = parity.next_power_of_two();
        let w = |i: usize| phi(i as u16);
        let mut a = vec![vec![0u16; m + 1]; m];
        for (t, row) in a.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().take(m).enumerate() {
                *cell = gf_pow(w(j), t as u32);
            }
            row[m] = data.iter().enumerate().fold(0, |acc, (i, &d)| acc ^ gf_mul(d, gf_pow(w(m + i), t as u32)));
        }
        for col in 0..m {
            let pivot = (col..m).find(|&r| a[r][col] != 0).expect("Vandermonde is non-singular");
            a.swap(col, pivot);
            let inv = gf_pow(a[col][col], 65534);
            for v in &mut a[col] {
                *v = gf_mul(*v, inv);
            }
            let pivot_row = a[col].clone();
            for (r, row) in a.iter_mut().enumerate() {
                let f = row[col];
                if r != col && f != 0 {
                    for (x, &p) in row.iter_mut().zip(&pivot_row) {
                        *x ^= gf_mul(f, p);
                    }
                }
            }
        }
        a.iter().take(parity).map(|row| row[m]).collect()
    }

    /// The library must produce exactly what the specification describes. If a new version of
    /// `reed-solomon-simd` changes the field, basis or order — this test will fail before
    /// incompatible files appear.
    #[test]
    fn codec_matches_specification() {
        let mut phi_inv = vec![0u16; 65536];
        for s in 0..=u16::MAX {
            phi_inv[phi(s) as usize] = s;
        }
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        };
        // Various K and M, including several "chunks" (K > m) and the format's maximum redundancy.
        for (k, parity) in [(1usize, 1usize), (2, 1), (5, 2), (7, 3), (17, 6), (40, 12), (300, 90)] {
            let shards: Vec<Vec<u8>> = (0..k).map(|_| (0..128).map(|_| rnd()).collect()).collect();
            let recovery = reed_solomon_simd::encode(k, parity, &shards).unwrap();
            for e in [0, 7, 31, 32, 63] {
                let data: Vec<u16> = shards.iter().map(|s| phi(symbol(s, e))).collect();
                let expected = reference_parity(&data, parity);
                for (j, &value) in expected.iter().enumerate() {
                    assert_eq!(phi_inv[value as usize], symbol(&recovery[j], e), "K={k} M={parity} symbol {e} parity {j}");
                }
            }
        }
    }
}
