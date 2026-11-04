# VNMF 2.0 Format Specification

VNMF is a container for manga, comics and webtoons: page images, cover previews,
page attachments (music, sound effects, text, dark versions), metadata and (optionally)
Reed–Solomon redundancy for protection against media faults.

This document describes the format at the level of bytes and mathematics — enough to write a compatible
reader or writer without the `vnmf-core` code and without any particular library.
Reference implementation: `vnmf-core` (Rust); the redundancy section ([11](#11-parity-section-reedsolomon)) is a separate crate,
`vnmf-parity`, which can be used without the rest of the format.

**Status:** 2.0, stable (October 2026). The layout is fixed: incompatible changes only
with a new major version, compatible ones via minor ([18](#18-compatibility-and-versions)). 2.0 is incompatible with 1.x: all fields are naturally aligned, a page record is 64 bytes,
a separate attachments table instead of a single `aux_chunk_idx`, self-describing parity blocks, an exact
mathematical definition of the code; see [18](#18-compatibility-and-versions).

---

## Contents

1. [General](#1-general)
2. [Overall file structure](#2-overall-file-structure)
3. [Header](#3-header)
4. [EntryRecord](#4-entryrecord)
5. [Info section](#5-info-section)
6. [Preview table](#6-preview-table)
7. [Page table](#7-page-table)
8. [Attachments table (aux)](#8-attachments-table-aux)
9. [Chunk table and data](#9-chunk-table-and-data)
10. [Tail copy of the metadata](#10-tail-copy-of-the-metadata)
11. [Parity section (Reed–Solomon)](#11-parity-section-reedsolomon)
12. [Integrity control: summary](#12-integrity-control-summary)
13. [Write algorithm](#13-write-algorithm)
14. [Read algorithm](#14-read-algorithm)
15. [Recovery](#15-recovery)
16. [VNMF metadata](#16-vnmf-metadata)
17. [Limits](#17-limits)
18. [Compatibility and versions](#18-compatibility-and-versions)
19. [Appendix A: codes and flags](#appendix-a-codes-and-flags)
20. [Appendix B: tools](#appendix-b-tools)

---

## 1. General

| What | How |
|---|---|
| Byte order | **little-endian** for all multi-byte integers |
| Structures | no padding (`packed`), but **every field sits at a naturally aligned offset**: `u64` — multiple of 8, `u32` — 4, `u16` — 2 (from the start of the structure; the tables themselves start at an offset that is a multiple of `B`). So a structure can be overlaid on an mmap as an ordinary aligned C struct — without unaligned accesses on ARM, MIPS, RISC-V |
| Offsets | absolute, from the start of the file, in bytes |
| "Section absent" | an `EntryRecord` with `offset = 0` **or** `length = 0` |
| Reserved bytes | the writer fills them with zeros, the reader ignores them |
| Strings | UTF-8 (only in the Info section) |

### 1.1. CRC32

**CRC-32/ISO-HDLC** is used everywhere (the same as in ZIP, PNG, Ethernet):

| Parameter | Value |
|---|---|
| Polynomial | `0x04C11DB7` (reflected `0xEDB88320`) |
| Initial value | `0xFFFFFFFF` |
| Input / output reflected | yes / yes |
| Final XOR | `0xFFFFFFFF` |
| Check value for `"123456789"` | `0xCBF43926` |

**Self-checking structures** (the header, page, preview and attachment records, parity
CRC table blocks) contain their own CRC.
It is computed over **all bytes of the structure with the CRC field temporarily zeroed**.

### 1.2. Block size and alignment

The file has a block size `B = 2^shift`, where `shift` is in the header:

| `shift` | `B` |
|---|---|
| 12 | 4 KiB (default) |
| 13 | 8 KiB |
| 14 | 16 KiB |
| 15 | 32 KiB |

Other values are invalid — the reader must reject the file.

All sections except the header and Info in the primary copy start at an offset that is a **multiple of `B`**.
Gaps between sections are filled with zeros. Alignment gives three benefits:
sections line up with memory pages (mmap, zero-copy), parity shards line up with blocks,
and when the metadata is lost, data only has to be searched for at aligned positions.

`align_up(x) = ⌈x / B⌉ · B`.

---

## 2. Overall file structure

```text
offset 0
┌──────────────────────────────────────────────┐ ┐
│ Header (128 B)                               │ │
│ Info (any length, right after the header)    │ │
│ ··· zeros up to align_up ···                 │ │
│ Preview table         (N × 32 B)             │ │  primary
│ Page table            (N × 64 B)             │ │  metadata
│ Attachments table     (N × 48 B)             │ │  copy
│ Chunk table           (N × 16 B)             │ ┘  (each at align_up)
│ ··· zeros ···                                │ ┐
│ Preview data                                 │ │  data
│ Chunk data: page 0, its attachments, p. 1…   │ │  (every chunk
│ ··· zeros up to align_up ···                 │ ┘  aligned)
├──────────────────────────────────────────────┤ ← parity_offset   ┐
│ CRC table A  (blocks with VNMFPAR1 signature)│                   │ only with
│ Parity shards                                │                   │ redundancy
│ CRC table B  (blocks with VNMFPAR1 signature)│                   │ > 0
├──────────────────────────────────────────────┤ ← end of parity   ┘
│ Copy of the chunk table                      │ ┐
│ Copy of the attachments table                │ │
│ Copy of the page table                       │ │  tail copy
│ Copy of the preview table                    │ │  (with HasTailDup)
│ Copy of Info                                 │ │
│ Tail header (exactly the last 128 B)         │ ┘
└──────────────────────────────────────────────┘ end of file
```

Everything from offset 0 to `parity_offset` is called the **protected area**: that is what the
Reed–Solomon parity covers.

---

## 3. Header

Size — exactly **128 bytes**. The primary header occupies offset 0; the tail one (if present) —
the last 128 bytes of the file.

| Offset | Size | Type | Field | Description |
|---:|---:|---|---|---|
| 0 | 4 | `u8[4]` | `magic` | ASCII `"vnmf"` (`76 6E 6D 66`) |
| 4 | 1 | `u8` | `major_version` | `2` |
| 5 | 1 | `u8` | `minor_version` | `0` |
| 6 | 2 | `u16` | `flags` | file flags, [see A.1](#a1-file-flags-flags) |
| 8 | 16 | `u8[16]` | `uuid` | volume identifier (UUID v4 recommended), binary, RFC 4122 byte order |
| 24 | 16 | `EntryRecord` | `info` | Info section |
| 40 | 16 | `EntryRecord` | `preview_table` | preview table |
| 56 | 16 | `EntryRecord` | `page_table` | page table |
| 72 | 16 | `EntryRecord` | `aux_table` | attachments table ([8](#8-attachments-table-aux)) |
| 88 | 16 | `EntryRecord` | `chunks_table` | chunk table |
| 104 | 8 | `u64` | `parity_offset` | start of the parity section = end of the protected area; `0` — none |
| 112 | 1 | `u8` | `redundancy` | redundancy in percent: `0` — none, `1..=30` |
| 113 | 1 | `u8` | `block_size_shift` | `12..=15`, see [1.2](#12-block-size-and-alignment) |
| 114 | 10 | `u8[10]` | `reserved` | zeros |
| 124 | 4 | `u32` | `crc` | CRC32 of the 128 header bytes with this field zeroed |

The primary and tail headers differ **only** in the offsets in the five `EntryRecord`s
(and, as a consequence, in the `crc` field): in the tail one they point to the tail copies of the sections.
Section lengths and CRCs, flags, UUID and parity parameters are the same.

**Header validation** (in this order):
1. `magic == "vnmf"`;
2. `crc` matches;
3. `major_version == 2` (a different major version is incompatible; a reader may separately support 1.x,
   see [18](#18-compatibility-and-versions));
4. `block_size_shift ∈ 12..=15`.

---

## 4. EntryRecord

Description of a section or a chunk. **16 bytes.**

| Offset | Size | Type | Field | Description |
|---:|---:|---|---|---|
| 0 | 8 | `u64` | `offset` | absolute offset of the start of the data |
| 8 | 4 | `u32` | `length` | length in bytes |
| 12 | 4 | `u32` | `crc` | CRC32 of the data (`length` bytes from `offset`) |

The record has no CRC of its own: in the chunk table it is confirmed by the CRC of the data it points to
(see [15.3](#153-assembling-metadata-record-by-record)).

---

## 5. Info section

Arbitrary text metadata of the volume. The format is **not fixed** — the reader detects it by contents:

| Start of the contents (after the BOM and whitespace) | Format |
|---|---|
| `<` | XML |
| `{` | JSON |
| `[` + name + `]` on the first line (`[meta]`, `[[x]]`) | TOML |
| `[` otherwise | JSON (array) |
| the first meaningful line is `key = value` | TOML |
| other valid UTF-8 | text |
| invalid UTF-8 | binary data |

The recommended format is **JSON following the VNMF metadata schema** ([section 16](#16-vnmf-metadata)).

In the primary copy, Info lies **right after the header** (offset 128) without alignment.
When Info is present, the `HasInfo` flag is set.

---

## 6. Preview table

An array of `PreviewRecord` records of **32 bytes** each. Count = `preview_table.length / 32`
(a length that is not a multiple of 32 is an error). The preview with index 0 is the main cover of the file.

| Offset | Size | Type | Field | Description |
|---:|---:|---|---|---|
| 0 | 8 | `u64` | `offset` | offset of the image data |
| 8 | 4 | `u32` | `length` | data length |
| 12 | 4 | `u32` | `crc` | CRC32 of the image **data** |
| 16 | 4 | `u32` | `width` | width, px (0 — unknown) |
| 20 | 4 | `u32` | `height` | height, px (0 — unknown) |
| 24 | 2 | `u16` | `volume_id` | 0 — file cover, 1 — volume 1, … |
| 26 | 1 | `u8` | `format` | image format, [A.3](#a3-image-formats-format) |
| 27 | 1 | `u8` | `flags` | reserved, `0` |
| 28 | 4 | `u32` | `record_crc` | CRC32 of **this record** with `record_crc` zeroed |

The main preview (`volume_id = 0`) may be a separate image that is not among the pages.

The number of previews is unlimited, and `volume_id` may repeat: several previews of the same volume
(or file) are its covers, **the main one is the first in the table**, the rest are additional (cover
variants, a back cover, etc.). The writer puts the file covers (`volume_id = 0`)
first, so record 0 is the main cover of the file. A preview of a volume that isn't among the pages
is not an error — the reader may skip it.

Preview data is stored outside the chunk table; each is aligned to `B`.

---

## 7. Page table

An array of `PageRecord` records of **64 bytes** each, in reading order. Count =
`page_table.length / 64`. 26 reserved bytes — for future fields without changing the record size.

| Offset | Size | Type | Field | Description |
|---:|---:|---|---|---|
| 0 | 4 | `u32` | `width` | page width, px |
| 4 | 4 | `u32` | `height` | height, px (for a webtoon — the sum of the chunk heights) |
| 8 | 4 | `u32` | `first_chunk_idx` | index of the image's first chunk in the chunk table |
| 12 | 4 | `u32` | `chunk_count` | number of image chunks (≥ 1) |
| 16 | 4 | `u32` | `first_aux_idx` | index of the first record in the attachments table (`0` if `aux_count = 0`) |
| 20 | 4 | `u32` | `aux_count` | number of page attachments, [7.2](#72-page-attachments) |
| 24 | 2 | `u16` | `volume_id` | volume number, `0` — volume not specified |
| 26 | 2 | `u16` | `chapter_id` | chapter number, [7.4](#74-volumes-and-chapters) |
| 28 | 2 | `u16` | `page_number_in_chapter` | page number within the chapter, from 1 |
| 30 | 2 | `u16` | `flags` | page flags, [A.2](#a2-page-flags-pagerecordflags) |
| 32 | 1 | `u8` | `format` | image format, [A.3](#a3-image-formats-format) |
| 33 | 1 | `u8` | `role` | page role, [A.4](#a4-page-roles-role) |
| 34 | 26 | `u8[26]` | `reserved` | zeros |
| 60 | 4 | `u32` | `record_crc` | CRC32 of **this record** with `record_crc` zeroed |

### 7.1. Page image

The image consists of chunks `first_chunk_idx .. first_chunk_idx + chunk_count` of the chunk table.

- **Regular page:** one chunk = one image file.
- **Webtoon** (the `IsWebToon` file flag): several chunks — fragments of a long canvas
  **from top to bottom**, each a standalone image file. `width` is the maximum
  fragment width, `height` is the sum of their heights.
- **Mixed formats** (the `MIXED_FORMATS` page flag): fragments of a single page have
  different formats (e.g. JPEG and PNG). The `format` field then contains the format of the **first** chunk,
  and the reader **must** detect each chunk's format by its signature ([A.3](#a3-image-formats-format)).
  Without this flag all chunks of the page have the `format` format.

### 7.2. Page attachments

Attachments are the attachments table records `first_aux_idx .. first_aux_idx + aux_count`
([section 8](#8-attachments-table-aux)). **Presence is determined only by `aux_count`**: there are no magic
index values, and what exactly the page has (sound, text, a dark version) is visible from the type
of each record, not from page flags. So this information can't contradict itself.

### 7.3. Record integrity

`record_crc` makes it possible to check each page independently of the CRC of the whole table: if
the table is damaged, you can see *which* records survived. This is used in recovery
([15.3](#153-assembling-metadata-record-by-record)). The reader **must** reject a record with a wrong
`record_crc`, even if the table CRC matched.

### 7.4. Volumes and chapters

A file may contain several volumes, each with several chapters. There's no separate structure table:
it follows unambiguously from the page table (pages are in reading order).

- **Volume** — the longest run of pages with the same `volume_id`. A new volume also starts
  on a page with the `IS_VOLUME_START` flag, even with the same number.
- **Chapter** — the longest run of a volume's pages with the same `chapter_id`; a new chapter
  also starts on a page with `IS_CHAPTER_START`. This makes it possible to have two chapters with
  the same number in a row.
- `volume_id = 0` on all pages — a file without volumes: a single
  "volume 0" with chapters.
- Numbers don't have to increase and don't have to be unique across volumes (chapter 1 may
  be in every volume). Within a volume, chapters with the same number are not recommended.
- The writer **must** set `IS_CHAPTER_START` on the first page of every chapter and
  `IS_VOLUME_START` on the first page of every volume with `volume_id ≠ 0` (the reference
  writer does this itself based on number changes).
- Webtoon: a chapter (episode) is usually a single page made of several chunks.
- `chapter_id` is an integer. The "human" number ("10.5", "Prologue") and volume and chapter titles
  are stored in the metadata, in the `contents` field ([16.4](#164-contents-volumes-and-chapters)).
- A preview with `volume_id = N` is the cover of volume N ([6](#6-preview-table)).

---

## 8. Attachments table (aux)

An array of `AuxRecord` records of **48 bytes** each. Count = `aux_table.length / 48`. The records of one
page are contiguous, pages are in the order of the page table.

| Offset | Size | Type | Field | Description |
|---:|---:|---|---|---|
| 0 | 4 | `u32` | `page_idx` | the page the attachment belongs to (duplicates the reference from the page) |
| 4 | 4 | `u32` | `chunk_idx` | the data chunk in the chunk table |
| 8 | 1 | `u8` | `kind` | type, [A.5](#a5-attachment-types-auxrecordkind) |
| 9 | 1 | `u8` | `format` | data format, [A.7](#a7-attachment-data-formats-auxrecordformat) — a hint, the real one is detected by signature |
| 10 | 2 | `u16` | `flags` | flags, [A.6](#a6-attachment-flags-auxrecordflags) |
| 12 | 4 | `u32` | `delay_ms` | music and effects: start delay, ms |
| 16 | 4 | `u32` | `trigger_y` | an effect in a strip: the page line (px from its top) at which it plays; `0` — from the start |
| 20 | 4 | `u32` | `part` | dark version: which page image (`0 .. chunk_count`) it replaces |
| 24 | 4 | `u32` | `duration_ms` | music and effects: how long to play from the start, ms (with `LOOP` — repeat for that long); `0` — unlimited |
| 28 | 2 | `u16` | `pages` | music: on how many pages, starting with its own; `0` — until the next music or the end of the chapter |
| 30 | 1 | `u8` | `volume` | volume `1..=100` %; `0` — 100 % |
| 31 | 13 | `u8[13]` | `reserved` | zeros |
| 44 | 4 | `u32` | `record_crc` | CRC32 of **this record** with `record_crc` zeroed |

**Reference check** (the reader **must** do it): for every page `i` the range
`first_aux_idx .. first_aux_idx + aux_count` lies within the table, and every record in it has
`page_idx = i`; the `chunk_idx` of every record is within the chunk table.

### 8.1. How to play back

| Type | Reader behaviour |
|---|---|
| `Music` | The music of page `p` is the **last** music on the pages from the start of the chapter up to `p` (so it continues until another one is met; the same when paging backwards). If it has `pages = N > 0`, it plays only on pages `s .. s+N−1` (`s` is its page), then silence. `duration_ms > 0` — it plays that long from the start and fades out; on the same pages it doesn't start again until the reader moves to where different music or silence plays. At a chapter boundary the music stops. `LOOP` — repeat (within `duration_ms`, if set) |
| (limits) | Both limits apply together: whichever comes first |
| `Sfx` | In paged mode — once, when the page is opened. In a strip — when the `trigger_y` line appears on screen. `LOOP` — repeat while the page (line) is visible; `duration_ms` cuts the effect off earlier |
| `Text` | Page text: UTF-8 or `vnmf-text/1` JSON (8.2). Shown on demand (a separate panel, search, text-to-speech) |
| `DarkImage` | In a dark theme it's shown instead of the page's image `part`. The dimensions must match the original |
| `Attachment` | An arbitrary file: the reader doesn't show it, unpacking keeps it |

An unknown type code (from a newer version) is treated as `Attachment`. The `delay_ms` delay applies to music and
effects; the volume is a multiplier of the reader's master volume.

### 8.2. Text layer `vnmf-text/1`

```json
{
  "schema": "vnmf-text/1",
  "language": "uk",
  "regions": [
    { "x": 120, "y": 340, "w": 300, "h": 90, "text": "A line in a speech bubble" }
  ]
}
```

Coordinates are in pixels of the page image (for a webtoon — of the whole long page). Regions
are in reading order. Text without this schema is plain text for the whole page.

---

## 9. Chunk table and data

An array of `EntryRecord`s (16 bytes). Count = `chunks_table.length / 16`. Each record
describes one **chunk** — a contiguous piece of data: a page image, a webtoon fragment
or attachment data.

**Order of data in the file** (this is how the reference writer does it; a reader must not rely on it):

1. the data of all previews in preview table order;
2. for each page in turn: its chunks, then the chunks of its attachments in record order.

Each fragment starts at an aligned offset; the tail of the block is filled with zeros.

The **image format** in a page or preview record is a hint. The real format is determined
by the data signature (see [A.3](#a3-image-formats-format)); if they differ, the data should
be trusted.

---

## 10. Tail copy of the metadata

Enabled by the **`HasTailDup`** flag (mandatory if there is parity).

After the end of the data (or the end of the parity section) the following are written, each at `align_up`:

1. a copy of the chunk table;
2. a copy of the attachments table;
3. a copy of the page table;
4. a copy of the preview table;
5. a copy of Info;
6. the **tail header** — at `align_up`, and it always occupies **exactly the last 128 bytes of the file**.

Empty sections are skipped (their `EntryRecord` in the tail header is zero).
The tail copy is **not covered** by parity: it is itself a duplicate and is deterministically
reproduced from the primary metadata.

### 10.1. Restoring the primary header from the tail one

The primary copy has a deterministic layout that depends only on the section lengths:

```text
cursor = 128
info.offset          = (info.length > 0) ? 128 : 0;      cursor += info.length
preview_table.offset = reserve(preview_table.length)
page_table.offset    = reserve(page_table.length)
aux_table.offset     = reserve(aux_table.length)
chunks_table.offset  = reserve(chunks_table.length)

reserve(len) = len == 0 ? 0 : { off = align_up(cursor); cursor = off + len; off }
```

So the primary header = the tail one with these offsets substituted and the `crc` recomputed.

---

## 11. Parity section (Reed–Solomon)

Present if the **`HasParity`** flag is set and `redundancy ∈ 1..=30`.
Allows recovering damaged blocks of the protected area (header, Info, tables, data).

### 11.1. Parameters

The whole layout is **deterministically derived** from three header fields —
`parity_offset`, `block_size_shift`, `redundancy`. The same three parameters are **repeated in every
CRC table block** ([11.5](#115-crc-table)), so the section can be found even without both headers.
`parity_offset` must be a multiple of `B`.

```text
B   = 2^block_size_shift                 shard size
N   = parity_offset / B                  data shards (protected area)
S   = ⌈N / 1024⌉                         stripes
K   = ⌈N / S⌉                            data shards per stripe (≤ 1024)
M   = max(1, ⌈K · redundancy / 100⌉)     parity shards per stripe
P   = S · M                              parity shards in total
E   = (B − 36) / 4 = B/4 − 9             entries per CRC table block
T   = ⌈(N + P) / E⌉                      blocks in one copy of the CRC table
```

### 11.2. Shards and interleaved stripes

Data shard `i` is the bytes `[i·B, (i+1)·B)` of the protected area.

Stripe `s` (`0 ≤ s < S`) contains shards `s, s+S, s+2S, …` (while `< N`). Shard `i` occupies
position `⌊i / S⌋` in the stripe. If a stripe has fewer than `K` shards, the remaining positions are **virtual
zero shards** (not stored in the file).

**Why interleave:** real damage is usually contiguous (a bad sector, a lost filesystem
extent). A contiguous region of `L` blocks hits each stripe roughly `L/S` times,
so with `M` parity shards per stripe any contiguous loss of up to `M · S · B`
bytes can be recovered — roughly `redundancy`% of the protected area.

### 11.3. Codec (mathematical definition)

The code is defined mathematically; a particular library is just one of the possible implementations.

**Field.** GF(2¹⁶) = GF(2)[x] / (p(x)), p(x) = x¹⁶ + x⁵ + x³ + x² + 1 (`0x1002D`).
A field element in this representation is a 16-bit number, bit `k` is the coefficient of xᵏ.

**Symbols.** A shard is a sequence of 16-bit symbols. A shard is split into 64-byte blocks; in
each block the first 32 bytes are the low bytes of 32 symbols, the next 32 are the high bytes:

```text
symbol e of a shard = byte[64·⌊e/32⌋ + (e mod 32)] | byte[64·⌊e/32⌋ + 32 + (e mod 32)] << 8
```

(`B` is a multiple of 64, so a shard contains exactly `B/2` symbols.) Symbols with the same number `e` in
different shards are encoded independently — everything below is for a single `e`.

**Cantor basis.** Symbol `s` denotes the field element φ(s) = ⊕ βₖ over all set bits
`k` of the number `s`, where

```text
β₀..β₁₅ = 0001 ACCA 3C0E 163E C582 ED2E 914C 4012 6C98 10D8 6A72 B900 FDB8 FB34 FF38 991E  (hex)
```

φ is a linear bijection, so adding symbols is XOR, and multiplication happens over φ(·).

**Code.** For a stripe with `K` data shards (with virtual zeros up to `K`) and `M` parity
shards: `m = 2^⌈log₂ M⌉` (M rounded up to a power of 2). The codeword `c` has
length 2¹⁶ and points ωᵢ = φ(i), `i = 0 .. 2¹⁶−1`:

```text
c[j]     = φ(parity j),   j = 0 .. m−1   (only j < M are stored)
c[m + i] = φ(data i),     i = 0 .. K−1
c[other] = 0
```

The parity is the unique solution of the conditions

```text
Σᵢ c[i] · ωᵢᵗ = 0   for t = 0 .. m−1
```

i.e. of an m×m system with a Vandermonde matrix at the points ω₀..ωₘ₋₁ (it is non-singular). Equivalently:
`c` is the values at all field points of a polynomial of degree `< 2¹⁶ − m`, which makes this a systematic
Reed–Solomon code, and any `M` lost shards of a stripe (data or parity) can be recovered
by any erasure decoder for this code.

The reference implementation uses a fast algorithm (additive FFT, O(n log n)) from the
`reed-solomon-simd` library; the `codec_matches_specification` test in `vnmf-parity` checks its result against
a naive solution of the system above — a change of field, basis or order in a new version of the library
will break the test before incompatible files appear.

### 11.4. Section layout

```text
parity_offset ┬ CRC table A                  T blocks
              ├ parity shard #0              B bytes
              ├ parity shard #1
              ├ …
              ├ parity shard #(P−1)
              ├ CRC table B                  T blocks (like A, except for the copy byte)
end           ┘ = parity_offset + (2T + P) · B
```

Parity shard `j` of stripe `s` has the **global index** `g = j·S + s` (interleaved as well) and
lies at offset `parity_offset + (T + g) · B`.

### 11.5. CRC table

Contains `N + P` `u32` entries: first the CRC32 of each data shard `0..N`, then of each
parity shard by global index `0..P`.

Every table block (`B` bytes) is **self-describing**:

| Offset | Size | Type | Field |
|---:|---:|---|---|
| 0 | 8 | `u8[8]` | ASCII signature `"VNMFPAR1"` |
| 8 | 8 | `u64` | `parity_offset` |
| 16 | 4 | `u32` | block number within the table copy, `0 .. T−1` |
| 20 | 1 | `u8` | `block_size_shift` |
| 21 | 1 | `u8` | `redundancy` |
| 22 | 1 | `u8` | copy: `0` — A, `1` — B |
| 23 | 1 | `u8` | `0` |
| 24 | 4 | `u32` | `T` — blocks in one copy |
| 28 | 4 | `u32` | `0` |
| 32 | 4·E | `u32[E]` | entries (the last block is padded with zeros) |
| B−4 | 4 | `u32` | CRC32 of the first `B − 4` bytes of the block |

The first block of copy A lies exactly at `parity_offset` — the signature marks the start of the section.
The two copies (A and B) are placed on different sides of the parity shards, so that a single contiguous
damage can't hit both; they differ only in the copy byte (and the block CRCs).

When reading, each table block is taken from the copy where the block CRC matches and the description
matches the layout (parameters, block number, copy).

### 11.6. Example

A 30.9 MiB file, `B = 4 KiB`, `redundancy = 20`:
`N = 6577`, `S = 7`, `K = 940`, `M = 188`, `P = 1316`, `E = 1015`, `T = 8`.
The parity section is `(2·8 + 1316) · 4 KiB ≈ 5.2 MiB`; a contiguous region of up to
`188 · 7 · 4 KiB ≈ 5.1 MiB` is guaranteed to be recoverable.

---

## 12. Integrity control: summary

| What is protected | By what | Where it's stored |
|---|---|---|
| Header | CRC32 | `Header.crc` |
| Info, each table | section CRC32 | `EntryRecord.crc` in the header |
| Each page / preview / attachment record | record CRC32 | `record_crc` in the record itself |
| Page ↔ attachments references | mutual indices | `first_aux_idx`/`aux_count` and `AuxRecord.page_idx` |
| Data of each chunk | CRC32 | `EntryRecord.crc` in the chunk table |
| Data of each preview | CRC32 | `PreviewRecord.crc` |
| The whole protected area | Reed–Solomon + CRC32 of every block | parity section |
| Parity parameters | a copy in every CRC table block + block CRC | parity section |
| Primary metadata | a full copy | tail copy |

---

## 13. Write algorithm

1. Determine the sizes of all sections and data; check the limits ([section 17](#17-limits)).
2. Lay out the primary copy (section [10.1](#101-restoring-the-primary-header-from-the-tail-one)),
   then the data with `align_up` for each fragment.
3. Build the preview, page and attachment records, **computing `record_crc` for each**;
   the chunk records; the CRCs of all sections. The `HasAudio`, `HasText`, `HasDarkMode` flags — by
   attachment types.
4. If `redundancy > 0`: `parity_offset = align_up(end of data)`, set
   `HasParity` and `HasTailDup`.
5. Build the primary header, compute `crc`.
6. Write the header, Info, tables, data; pad with zeros up to `parity_offset`.
7. If there is parity — compute it over the already written protected area and write the section
   ([section 11](#11-parity-section-reedsolomon)).
8. If `HasTailDup` — write the tail copy ([section 10](#10-tail-copy-of-the-metadata)).

The reference writer writes to a temporary file, calls `fsync` and renames atomically, so
a half-written file never appears under the target name.

---

## 14. Read algorithm

1. A file shorter than 128 bytes is an error.
2. Try the **primary copy**: validate the header ([section 3](#3-header)); for Info and
   each table — the file bounds, the section CRC; the table length is a multiple of the record size;
   the `record_crc` of every preview, page and attachment record; the references between pages and
   attachments ([8](#8-attachments-table-aux)).
3. If step 2 failed and the file is ≥ 256 bytes — try the **tail copy** (the header in the
   last 128 bytes) the same way; it is usable only with the `HasTailDup` flag.
4. Both failed — return the error of the primary copy (and offer recovery).

The reader must check that the `offset + length` of every section or chunk doesn't overflow and doesn't
go beyond the end of the file. The CRC of chunk data is checked on demand (when reading a page), not
when opening the file.

---

## 15. Recovery

The reference tool `vnmfrecovery` performs three steps, from the cheapest to the most expensive.

### 15.1. Metadata copies

Both copies are checked **in full** (header + tables + Info), the tail one first.

- The primary is damaged, the tail is intact → restore the primary from the tail: the header per
  [10.1](#101-restoring-the-primary-header-from-the-tail-one), copy the sections.
- The tail is damaged, the primary is intact → rewrite the tail from the primary.
- Both are damaged → [15.3](#153-assembling-metadata-record-by-record).
- **Both are intact, but the headers differ** (the primary doesn't match the one restored from the tail per
  [10.1](#101-restoring-the-primary-header-from-the-tail-one); this happens when a field was changed
  together with the CRC) — the file is **not intact**. The primary header lies in the protected area, so with
  parity it is the arbiter: after 15.2 the primary header is genuine, and the tail copy
  is rewritten from it. The parity layout is then better taken from the CRC table blocks
  ([11.5](#115-crc-table)) rather than from either header. Without parity it's impossible to tell the genuine
  copy: the reference tool takes the tail one but reports that the result is unconfirmed
  (exit code 1).

The file parameters (block size, parity) are taken from any intact **header** —
even if that copy's tables are damaged.

**Both headers are lost.** The primary header lies in the protected area, so it is
restored by the parity — the parity section just needs to be found. Every CRC table block starts with
the `VNMFPAR1` signature and contains all the layout parameters ([11.5](#115-crc-table)):

1. Scan the file positions that are multiples of 4 KiB (the minimum `B`).
2. At a position with the signature, read `block_size_shift` and check the block CRC.
3. Compute the layout from the block's `(parity_offset, B, redundancy)` and check that the block lies
   exactly where it should (block number, copy A or B, `T`). Such a block is a vote for the layout.
4. The layout with the most votes wins.

A single intact block out of `2T` is enough. Then — the usual parity recovery (15.2); if shard 0
is recovered, the primary header is intact again, and the tail copy is rewritten from it.

### 15.2. Parity

1. Pad a truncated file with zeros up to the end of the parity section — the lost shards simply won't
   pass the CRC.
2. Read the CRC table (blocks from A or B).
3. A shard with a wrong CRC is **erased**. A shard whose CRC is unknown (the table block is damaged in
   both copies) is considered erased if the stripe has enough parity for that; otherwise —
   intact (best effort).
4. For every stripe with ≤ `M` erasures: decode the data shards, then re-encode the
   parity and rewrite the damaged parity shards.
5. Rewrite both CRC tables. For **unrecovered** stripes the old CRCs are kept —
   otherwise damaged data would be "legalized" and no longer detected.

After that 15.1 is applied, and the tail copy is rewritten entirely.

### 15.3. Assembling metadata record by record

When neither copy is intact but at least one header is, the primary copy is assembled from the
two copies **section by section**:

1. a section whose CRC matches in either copy is taken whole;
2. otherwise — **record by record**: for each index the record is taken from the copy where it
   is confirmed:
   - a preview / page / attachment record — by its own `record_crc`;
   - a chunk record — by the CRC of the **data** it points to;
3. Info has no internal structure — the primary copy is taken.

If after assembly the CRCs of all sections match the header, the metadata has been restored exactly.

### 15.4. Data extraction

If the data couldn't be fully recovered:

- **Using the tables** (the metadata is readable): each chunk is saved as a separate file; chunks with
  a wrong CRC are marked `_damaged`.
- **By signatures** (the metadata is lost): positions that are multiples of 4 KiB (the minimum
  `B`) are checked. At a position with an image signature the boundaries are determined by parsing the structure:

  | Format | How the end is determined |
  |---|---|
  | PNG | the chain of chunks up to `IEND`; the CRC of every PNG chunk is verified |
  | JPEG | segments with lengths; after `SOS` — entropy-coded data up to the next marker; the end is `EOI` |
  | GIF | palettes, image and extension blocks with sub-blocks, the `0x3B` trailer |
  | WebP | the RIFF container length (+ padding to even) |
  | AVIF | a sequence of top-level ISO-BMFF boxes up to the first "non-box" |
  | JPEG XL (container) | the same as AVIF — it's ISO-BMFF too |
  | JPEG XL (codestream) | the length isn't encoded — boundaries are estimated, the fragment is marked incomplete |

  If the structure is damaged, the fragment is taken up to the next signature or zero block
  and marked incomplete. Text (Info) is searched for at offset 128 and at aligned
  positions: a run of valid UTF-8 without control characters up to the first zero.

---

## 16. VNMF metadata

The Info section in JSON following the **`vnmf-metadata/1`** schema — VNMF's own format, equally suited
to manga, manhwa, manhua, webtoons, Western comics, BD, doujinshi, etc. It is identified by the
`"schema": "vnmf-metadata/1"` field at the top level. Keys are `snake_case`.

**Rules:**
- All fields except `schema` are **optional**; empty ones are not written.
- Dictionary values (`type`, `status`, …) are strings from the tables below. An unknown value
  is shown by the reader as is (dictionaries can be extended without breaking compatibility).
- Dates — `YYYY`, `YYYY-MM` or `YYYY-MM-DD`. Languages — BCP 47 (`uk`, `ja`, `zh-Hans`),
  countries — ISO 3166-1 alpha-2 (`JP`, `KR`, `CN`).
- **Unknown keys** (from newer schema versions) are kept by the reader and written back unchanged.
  Third-party application data goes in `extra` under its own key.

```json
{
  "schema": "vnmf-metadata/1",
  "title": "Solo Leveling",
  "original_title": "나 혼자만 레벨업",
  "alternative_titles": [{ "title": "Solo Leveling", "language": "en" }],
  "type": "manhwa",
  "status": "completed",
  "original_language": "ko",
  "origin_country": "KR",
  "credits": [
    { "name": "Chugong", "original_name": "추공", "roles": ["original_creator"] },
    { "name": "DUBU", "roles": ["art"] },
    { "name": "Vera", "roles": ["translator", "typesetter"] }
  ],
  "language": "uk",
  "translation": { "type": "fan", "groups": ["Team"] },
  "genres": ["Action", "Fantasy"],
  "age_rating": "16+",
  "reading_direction": "ttb",
  "layout": "strip",
  "identifiers": { "anilist": "105398" },
  "contents": {
    "volumes": [{ "volume": 1, "title": "Beginning",
                  "chapters": [{ "chapter": 1, "title": "Awakening" }] }]
  }
}
```

### 16.1. Fields

**The work (series):**

| Field | Type | Meaning |
|---|---|---|
| `title` | string | title (in the edition's language) |
| `original_title` | string | title in the original language |
| `romanized_title` | string | romanized transliteration |
| `alternative_titles` | array of `{title, language?}` | other titles |
| `type` | dictionary | what kind of work it is, [16.2](#162-dictionaries) |
| `publication` | dictionary | form of publication |
| `status` | dictionary | state of the series |
| `demographic` | dictionary | target audience |
| `original_language` | string | original language (BCP 47) |
| `origin_country` | string | country of origin (ISO 3166-1) |
| `summary` | string | description |
| `genres`, `tags`, `characters` | arrays of strings | genres, tags, characters |
| `age_rating` | dictionary | age rating |
| `content_warnings` | array of strings | content warnings |
| `start_date`, `end_date` | date | years the series ran |
| `volume_count`, `chapter_count` | integer | total volumes / chapters **in the series** (not in the file) |
| `credits` | array of `{name, roles[], original_name?}` | authors and team; one person — one entry with several roles |
| `serialization` | string | magazine or platform of first publication (Weekly Shōnen Jump, Naver Webtoon, KakaoPage) |
| `original_publisher` | string | original publisher |

**This edition (the file):**

| Field | Type | Meaning |
|---|---|---|
| `language` | string | language of the text in this file (BCP 47) |
| `translation` | `{type?, groups[], notes?}` | translation type, translation/scanlation teams, notes |
| `publisher`, `imprint` | string | publisher and imprint of this edition |
| `edition` | string | edition (Deluxe, collector's, digital) |
| `release_date` | date | release date of this edition |
| `isbn` | string | ISBN / GTIN |

**Reading, catalogs, the rest:**

| Field | Type | Meaning |
|---|---|---|
| `reading_direction` | dictionary | reading direction (pages also have the `READING_RTL` flag) |
| `layout` | dictionary | how it's meant to be read |
| `color` | dictionary | coloring |
| `identifiers` | object `key → string` | catalog identifiers, [16.2](#162-dictionaries) |
| `links` | array of `{url, title?}` | links |
| `contents` | object | volume and chapter titles, [16.4](#164-contents-volumes-and-chapters) |
| `extra` | object | application data; `extra.comicinfo` — ComicInfo fields without an equivalent |

### 16.2. Dictionaries

| Field | Values |
|---|---|
| `type` | `manga`, `manhwa`, `manhua`, `webtoon`, `webcomic`, `comic`, `graphic_novel`, `bande_dessinee`, `doujinshi`, `artbook`, `other` |
| `publication` | `serial`, `one_shot`, `anthology`, `yonkoma` |
| `status` | `ongoing`, `completed`, `hiatus`, `cancelled`, `upcoming` |
| `demographic` | `shounen`, `shoujo`, `seinen`, `josei`, `kodomo` (also for manhwa/manhua) |
| `age_rating` | `all`, `10+`, `13+`, `16+`, `18+` |
| `reading_direction` | `rtl` (manga), `ltr` (comics, manhwa), `ttb` (webtoon) |
| `layout` | `paged`, `double` (in spreads), `strip` (as a strip) |
| `color` | `bw`, `color`, `mixed` |
| `translation.type` | `official`, `fan`, `machine` |
| `credits[].roles` | `author` (story and art), `story`, `art`, `original_creator` (author of the source novel), `penciller`, `inker`, `colorist`, `letterer`, `cover_artist`, `assistant`, `editor`, `translator`, `proofreader`, `typesetter`, `cleaner`, `redrawer`, `quality_checker`, `raw_provider` |
| `identifiers` (keys) | `anilist`, `mal`, `mangadex`, `mangaupdates`, `kitsu`, `shikimori`, `comicvine`, `webtoons`, `naver`, `kakao`, `bilibili`, `kuaikan`, `tapas` (others are allowed) |

### 16.3. ComicInfo.xml

ComicInfo (ComicRack/Komga/Kavita/Tachiyomi) is the exchange format with CBZ. When **packing** it
is reduced to VNMF metadata, when **unpacking** it is generated from it (`<Pages>` — from the roles in
the page table; for a single-chapter file — its `Title`, `Number`, `Volume`).

| ComicInfo | VNMF metadata |
|---|---|
| `Series` (or `Title` without `Series`) | `title` |
| `Title` when `Series` is present | chapter title in `contents` |
| `Number`, `Volume` | numbers in the page table (`chapter_id`, `volume_id`) and `contents` |
| `LocalizedSeries` | `alternative_titles` |
| `Summary` | `summary` |
| `Year`, `Month`, `Day` | `release_date` |
| `Writer` / `Penciller` / `Inker` / `Colorist` / `Letterer` / `CoverArtist` / `Editor` / `Translator` | `credits` with the roles `story` / `art` / `inker` / `colorist` / `letterer` / `cover_artist` / `editor` / `translator` |
| `Publisher`, `Imprint` | `publisher`, `imprint` |
| `Genre`, `Tags`, `Characters` | `genres`, `tags`, `characters` (comma-separated) |
| `Web` | `links` |
| `LanguageISO` | `language` |
| `GTIN` | `isbn` |
| `ScanInformation` | `translation.notes` |
| `BlackAndWhite` | `color` (`Yes` → `bw`) |
| `Manga` | `YesAndRightToLeft` → `type: manga`, `reading_direction: rtl`; `Yes` → `type: manga` |
| `AgeRating` | `age_rating` (`Everyone` → `all`, `Teen` → `13+`, `Mature 17+` → `16+`, `Adults Only 18+` → `18+`) |
| `Pages` | page roles in the page table: `FrontCover` → `FrontCover`, `BackCover` → `BackCover`, `InnerCover` → `TitlePage`, `Advertisement` → `Advertisement`, `Editorial`/`Letters` → `Credits`, `DoublePage="True"` → `SpreadSingle` |
| the rest (`StoryArc`, `Notes`, `Teams`, …) | `extra.comicinfo` as is — returned on export |

ComicInfo in JSON (the internal representation for conversion): the root element is an object with
a single key `{"ComicInfo": {…}}`, attributes are keys with `@`, text next to attributes is
`#text`, repeated elements are an array, all values are strings.

### 16.4. Contents: volumes and chapters

Titles, display numbers and dates of volumes and chapters are in the `contents` field. The structure (which pages
go where) is determined by the page table ([7.4](#74-volumes-and-chapters)); the contents only **label** it.

```json
"contents": {
  "volumes": [
    { "volume": 1, "title": "Beginning", "date": "2019-05", "isbn": "…",
      "chapters": [
        { "chapter": 1, "title": "First meeting", "date": "2018-03-04" },
        { "chapter": 2, "number": "1.5", "title": "Extra" }
      ] }
  ]
}
```

| Field | Meaning |
|---|---|
| `volume` | the volume's `volume_id` (`0` — chapters of a file without volumes) |
| `title`, `date`, `isbn` (volume) | title, date, ISBN of the volume |
| `chapter` | the chapter's `chapter_id` within this volume |
| `number` | display number, if it differs from `chapter` (a string: `"10.5"`) |
| `title`, `date` (chapter) | title and release date of the chapter |

Volumes and chapters without data don't have to be listed; entries without a counterpart in the page table
are ignored.

### 16.5. Files outside VNMF and compatibility

**Unpacking into a folder or CBZ** (`vnmfunpack`): a multi-chapter file is laid out in folders
`vol01/ch0001/…` (with volumes) or `ch0001/…`; the metadata — `vnmf.json` (full, in the current schema)
and `ComicInfo.xml` (for other readers). **Packing** (`vnmfpack`, VNMF Creator) reads
`vnmf.json` (takes precedence), otherwise the ComicInfo of each chapter, as well as folders like
`Volume 1 - Title/Chapter 3 - Title/` and names like `Vol.1 Ch.5 - Title.cbz`.

**Format 1.1 Info** — `{"ComicInfo": {…}, "Contents": {"Volumes": [{"Volume", "Title",
"Chapters": [{"Chapter", "Number", "Title"}]}]}}` — the reader (since 1.2) reduces it to VNMF metadata using
table [16.3](#163-comicinfoxml); the writer writes only `vnmf-metadata/1`. Info in other
formats (TOML, text, arbitrary JSON) is kept as is.

---

## 17. Limits

| Quantity | Limit | Due to |
|---|---|---|
| Size of a single chunk, preview, Info, table | 4 GiB − 1 | `u32 length` |
| File size | 2⁶⁴ − 1 | `u64 offset` |
| Number of pages | ≈ 67 million | table ≤ 4 GiB / 64 B |
| Number of attachments | ≈ 89 million | table ≤ 4 GiB / 48 B |
| Number of chunks | ≈ 268 million | table ≤ 4 GiB / 16 B; `u32` index |
| Chunks / attachments per page | 2³² − 1 | `u32 chunk_count`, `u32 aux_count` (in practice — the table limits) |
| Width / height | 4 294 967 295 px | `u32` |
| Volume, chapter, page-in-chapter number | 65 535 | `u16` |
| Redundancy | 0–30 % | the `redundancy` field, [3](#3-header) |
| Block size | 4–32 KiB | `block_size_shift ∈ 12..=15` |

---

## 18. Compatibility and versions

- The **major** version changes with incompatible changes; the reader rejects a file with a different major.
- **Minor** — compatible extensions: new flags, use of reserved bytes with
  the value "0 = default behaviour". The reader accepts any minor.
- The reader ignores unknown flag bits.
- Reserved fields: the writer writes zeros, the reader ignores them.

| Version | Changes |
|---|---|
| 1.0 | initial |
| 1.1 | `PageRecord.volume_id`, the `IS_VOLUME_START` flag, `Contents` table of contents in Info |
| 1.2 | own metadata schema `vnmf-metadata/1` instead of ComicInfo in Info ([16](#16-vnmf-metadata)) |
| **2.0** | stable (2026-10). **Incompatible** layout: all fields naturally aligned (header, 64 B `PageRecord`, `PreviewRecord`); an attachments table (`aux_table` in the header, [8](#8-attachments-table-aux)) instead of a single `aux_chunk_idx` with a magic `0xFFFFFFFF` — several attachments per page with a type and playback parameters; page flags only describe the image, while `HasDarkMode` / `HasText` / `HasAudio` are a summary of the attachments table; parity CRC table blocks are self-describing (`VNMFPAR1` + parameters, [11.5](#115-crc-table)); the Reed–Solomon code is defined mathematically ([11.3](#113-codec-mathematical-definition)) |

**1.x files.** A `major_version = 1` byte isn't enough to consider a file 1.x: damage to a
2.0 header with a recomputed CRC could produce it too. A file is considered 1.x only when **neither** of the two
copies (offset 0 and the last 128 bytes) is an intact 2.0 header. If there is an intact 2.0 copy, it's a
2.0 file whose other copy is damaged. A 2.0 reader may (the reference one does) read 1.x files, converting their
records in memory: the 1.x header (fields at 1.x offsets, `block_size_shift` at 123), 32 B page
records with `aux_chunk_idx` (`0xFFFFFFFF` — none; the attachment type comes from the 1.x flags
`HAS_AUDIO` → `Sfx`, `HAS_TEXT_LAYER` → `Text`, `HAS_DARK_OVERLAY` → `DarkImage`, otherwise by
contents). 1.x parity (CRC table blocks without a description, `E = B/4 − 1`) is neither checked nor
recovered; such a file should be rebuilt as 2.0 (VNMF Creator opens 1.x and saves 2.0).

---

## Appendix A: codes and flags

### A.1. File flags (`flags`)

| Bit | Mask | Name | Meaning |
|---:|---|---|---|
| 0 | `0x0001` | `HasInfo` | there is an Info section |
| 1 | `0x0002` | `HasPreview` | there is at least one preview |
| 2 | `0x0004` | `HasAlign` | sections are aligned to `B` (the reference writer always does this) |
| 3 | `0x0008` | `HasEncrypted` | reserved for encryption (not implemented) |
| 4 | `0x0010` | `HasDarkMode` | there are `DarkImage` attachments (a summary of the attachments table) |
| 5 | `0x0020` | `HasText` | there are `Text` attachments (summary) |
| 6 | `0x0040` | `HasAudio` | there are `Music` or `Sfx` attachments (summary) |
| 7 | `0x0080` | `HasTailDup` | there is a tail copy of the metadata ([10](#10-tail-copy-of-the-metadata)) |
| 8 | `0x0100` | `HasParity` | there is a parity section ([11](#11-parity-section-reedsolomon)) |
| 9–14 | | | reserved |
| 15 | `0x8000` | `IsWebToon` | webtoon: pages may consist of several chunks |

Bits 4–6 are set by the writer itself; a reader that needs precision looks into the attachments table.

### A.2. Page flags (`PageRecord.flags`)

Only properties of the image and the structure — which attachments exist is visible from the attachments table.

| Bit | Mask | Name | Meaning |
|---:|---|---|---|
| 0 | `0x0001` | `HAS_ALPHA` | the image has an alpha channel |
| 1 | `0x0002` | `IS_CHAPTER_START` | the first page of a chapter ([7.4](#74-volumes-and-chapters)) |
| 2 | `0x0004` | `IS_VOLUME_START` | the first page of a volume ([7.4](#74-volumes-and-chapters)) |
| 3 | `0x0008` | `LOSSLESS` | lossless image |
| 4 | `0x0010` | `IS_ANIMATED` | animated image |
| 5 | `0x0020` | `READING_RTL` | right-to-left reading |
| 6 | `0x0040` | `MIXED_FORMATS` | the page's chunks have different formats — detect each by signature ([7.1](#71-page-image)) |
| 7–15 | | | reserved |

### A.3. Image formats (`format`)

| Code | Format | Signature (detection by contents) | Extension |
|---:|---|---|---|
| 0 | AVIF | `[size:4] "ftyp"`, brand `avif` or `avis` as the major or among the compatible brands | `.avif` |
| 1 | WebP | `"RIFF" [size:4] "WEBP"` | `.webp` |
| 2 | PNG | `89 50 4E 47 0D 0A 1A 0A` | `.png` |
| 3 | JPEG | `FF D8 FF` | `.jpg` |
| 4 | GIF | `"GIF87a"` or `"GIF89a"` | `.gif` |
| 5 | JPEG XL | codestream `FF 0A` or container `00 00 00 0C 4A 58 4C 20 0D 0A 87 0A` (`"JXL "`) | `.jxl` |
| 255 | unknown | — | `.bin` |

Codes 6–254 are reserved; the reader treats them as an unknown format.

### A.4. Page roles (`role`)

| Code | Role | Description |
|---:|---|---|
| 0 | `Normal` | regular page |
| 1 | `FrontCover` | front cover |
| 2 | `BackCover` | back cover |
| 3 | `TitlePage` | title page |
| 4 | `SpreadLeft` | left half of a spread |
| 5 | `SpreadRight` | right half of a spread |
| 6 | `SpreadSingle` | a whole spread as a single image |
| 7 | `Credits` | authors / translators |
| 8 | `TableOfContents` | table of contents |
| 9 | `OmakeBonus` | bonus material |
| 10 | `Advertisement` | advertisement |

Other codes are an unknown role; the reader may show the page as `Normal`.

### A.5. Attachment types (`AuxRecord.kind`)

| Code | Type | Data | Name when unpacked |
|---:|---|---|---|
| 0 | `Attachment` | arbitrary | `file` |
| 1 | `Music` | sound | `music` |
| 2 | `Sfx` | sound | `sfx` |
| 3 | `Text` | UTF-8 or `vnmf-text/1` JSON | `text` |
| 4 | `DarkImage` | image | `dark` |

Other codes are treated as `Attachment`.

### A.6. Attachment flags (`AuxRecord.flags`)

| Bit | Mask | Name | Meaning |
|---:|---|---|---|
| 0 | `0x0001` | `LOOP` | music / effect: repeat (an effect — while the page is visible; both — within `duration_ms`) |
| 1–15 | | | reserved |

### A.7. Attachment data formats (`AuxRecord.format`)

Codes 0–5 are images, as in [A.3](#a3-image-formats-format).

| Code | Format | Signature | Extension |
|---:|---|---|---|
| 16 | Ogg (Vorbis, Opus, FLAC in Ogg) | `"OggS"` | `.ogg` |
| 17 | MP3 | `"ID3"` or the `FF Ex` sync word | `.mp3` |
| 18 | FLAC | `"fLaC"` | `.flac` |
| 19 | WAV | `"RIFF" [size:4] "WAVE"` | `.wav` |
| 20 | M4A (AAC, ALAC) | `[size:4] "ftyp"` (not AVIF / JPEG XL) | `.m4a` |
| 32 | UTF-8 text | valid UTF-8 without control characters | `.txt` |
| 33 | JSON | UTF-8 starting with `{` | `.json` |
| 255 | unknown | — | `.bin` |

---

## Appendix B: tools

| Tool | Purpose |
|---|---|
| `vnmfpack` | creates `.vnmf` from CBZ, folders or individual images (PNG, JPEG, WebP, AVIF, JPEG XL, GIF); volumes and chapters — from folders, names, `vnmf.json` and ComicInfo ([16.5](#165-files-outside-vnmf-and-compatibility)); `--volume N`, `--redundancy 0-30`, `--webtoon`, `--mixed-formats`, `--cover`; attachments from the `aux/` folder (as after `vnmfunpack`; without a type in the name — by contents) |
| `vnmfunpack` | unpacks into a folder or CBZ; metadata — `vnmf.json` and a generated `ComicInfo.xml`; volumes and chapters — `vol01/ch0001/` folders; attachments — `aux/003.music.ogg`, `aux/003.sfx2.wav`… (the number is the page's first image among all, the type — [A.5](#a5-attachment-types-auxrecordkind)) and `aux/aux.json` with parameters (`loop`, `delay_ms`, `duration_ms`, `pages`, `trigger_y`, `part`, `volume`), so `vnmfpack` builds the same file |
| `vnmfinfo` | full file information, contents (volumes and chapters) and integrity check (both metadata copies, CRC of all data, parity); on damage — a list of problems, a prognosis and a `vnmfrecovery` command; `--quick` — metadata only; `--json`; exit code 1 — the file is damaged |
| `vnmfrecovery` | recovery: metadata copies (both headers lost — the parity section is found by the `VNMFPAR1` signature) → parity → data extraction; for 1.x files — extraction only |
| `vnmfbadtest` | deliberately corrupts a file for tests (`burst`, `sectors`, `truncate`, `bitflip`) |

Every tool has `--help` with examples.

Graphical applications (GTK4/libadwaita, the `vnmf-gui` crate):

| Application | Purpose |
|---|---|
| `vnmf-info` | the same as `vnmfinfo`, + a cover and page viewer; contents by volumes and chapters; a full integrity check in the background |
| `vnmf-reader` | reader: page by page (manga, right to left) or as a strip (webtoon) + short information like `vnmfinfo --quick`; contents in the sidebar, a "Volume · Chapter" card at the start of a chapter, chapter headers in the strip, jumping between chapters (Ctrl+←/→, Ctrl+PgUp/PgDn); attachments: music and effects (per the rules of [8.1](#81-how-to-play-back); M — mute), the "Page text" panel (T), dark page versions in a dark theme |
| `vnmf-creator` | creating and editing a file: dragging in images/folders/CBZ, reordering pages, chapters and volumes (by dragging headers), page roles, volumes and chapters with titles, VNMF metadata (importing `vnmf.json` / ComicInfo.xml), redundancy, covers of the file and of each volume — any number: all pages with the "Cover" role and custom images from files; page attachments (page menu → "Sound, text, dark version"): music, effects, text (from a file or typed in), a dark version, an arbitrary file — with previewing and parameters (looping, on how many pages and for how many seconds the music plays, delay, volume, in a webtoon — the trigger line from the top of the fragment). An opened `.vnmf` (including 1.x) is saved only into a **new** 2.0 file (the source is not overwritten): pages and attachments are carried over byte for byte, and the parameters (redundancy, block size, backup copy, covers) can be changed; the file's custom covers are recognized and carried over into the list |
| `vnmf-thumbnailer` | thumbnails for file managers: the main preview, otherwise the first page (the top of a webtoon strip) |

---

## Appendix C: system integration (Linux)

| What | Value |
|---|---|
| MIME type | `application/x-vnmf` |
| Detection | the `vnmf` signature at offset 0 (priority 80) and the `*.vnmf` extension — the file is recognized even without the extension |
| Type icon | `application-x-vnmf` |
| Thumbnails | `vnmf-thumbnailer -s %s %i %o` (freedesktop thumbnailer specification) |
| Opening | VNMF Reader (default), VNMF Info |

The files are in the `data/` folder. Installation:

```sh
sudo ./install.sh --system     # /usr/local — for everyone, with thumbnails (recommended)
./install.sh                   # ~/.local — no sudo, but no thumbnails in GNOME
./install.sh --uninstall       # removal (with the same parameters)
packaging/make-srpm.sh --rpm   # RPM package for Fedora (requires rpm-build)
```

GNOME runs thumbnailers in a bubblewrap sandbox where only `/usr` is visible, so
`vnmf-thumbnailer` has to live under `/usr` (`/usr/local/bin`, or `/usr/bin` from the RPM).
If the user hasn't chosen an application for `.vnmf`, VNMF Reader and VNMF Info make
VNMF Reader the default on first launch. An explicit user choice is not changed.

---

## License and authors

© 2026 void_nicole (Nicole Hitori) + AI-assist.

The specification, the `vnmf-core` and `vnmf-parity` libraries, the console tools and the `vnmf-gui` graphical applications are
free software under the **GNU General Public License version 3 or later**
(`GPL-3.0-or-later`). The full text is in the [`LICENSE`](LICENSE) file.
