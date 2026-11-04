// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Unobtrusive Reed–Solomon redundancy inside ordinary files (`vnmfguard`, `vnmf-guard`).
//!
//! `vnmf-parity` parity is embedded in the format's own structure — in places ordinary programs
//! skip (see [`formats`]): videos, audio and disk images open as usual, and in case of
//! damage the file is restored to its post-protection state byte for byte.
//!
//! # How it works
//!
//! A protected file is described by a map ([`plan::Plan`]): a sequence of pieces — protected bytes, parity,
//! auxiliary ones. The protected bytes together form a "virtual file" for which `vnmf-parity` computes
//! the parity section; its bytes are spread across `Payload` pieces. Two copies of the map are stored in the file,
//! so to check or repair it's enough to find any intact one (by the `VNMFMAP1` signature).
//!
//! Shifts (inserted or deleted bytes) are not recovered — only damaged and truncated regions,
//! just like in `vnmf-parity`.

pub mod formats;
pub mod plan;

use std::fs::{self, File};
use std::io::{self, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use vnmf_parity::{BlockSize, MAX_REDUNDANCY, ParityLayout, ParityStatus, RepairReport};

use plan::{MAP_MAGIC, Piece, Plan};
use vnmf_i18n::{tr, trf};

pub use vnmf_parity;

/// Format of a protected file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Format {
    Matroska = 1,
    Mp4 = 2,
    Ogg = 3,
    Flac = 4,
    Mp3 = 5,
    Zstd = 6,
    Gzip = 7,
    Iso = 8,
    /// Unknown format: parity simply appended at the end (only on explicit request).
    Raw = 9,
}

impl Format {
    pub const ALL: [Format; 9] = [Format::Matroska, Format::Mp4, Format::Ogg, Format::Flac, Format::Mp3, Format::Zstd, Format::Gzip, Format::Iso, Format::Raw];

    pub fn name(self) -> &'static str {
        match self {
            Format::Matroska => "Matroska / WebM",
            Format::Mp4 => "MP4 / MOV",
            Format::Ogg => "Ogg Opus / Vorbis",
            Format::Flac => "FLAC",
            Format::Mp3 => "MP3",
            Format::Zstd => "Zstandard",
            Format::Gzip => "gzip",
            Format::Iso => "ISO 9660",
            Format::Raw => tr("raw file"),
        }
    }

    /// Typical extensions (for help and dialog filters).
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Format::Matroska => &["mkv", "mka", "webm"],
            Format::Mp4 => &["mp4", "m4v", "m4a", "mov"],
            Format::Ogg => &["ogg", "oga", "opus"],
            Format::Flac => &["flac"],
            Format::Mp3 => &["mp3"],
            Format::Zstd => &["zst"],
            Format::Gzip => &["gz"],
            Format::Iso => &["iso"],
            Format::Raw => &[],
        }
    }

    /// Where exactly the parity goes.
    pub fn placement(self) -> &'static str {
        match self {
            Format::Matroska => tr("Void element at the end"),
            Format::Mp4 => tr("free box at the end"),
            Format::Ogg => tr("data after the comments in the header"),
            Format::Flac => tr("APPLICATION metadata blocks"),
            Format::Mp3 => tr("ID3v2 XVNM frame"),
            Format::Zstd => tr("skippable frames at the end"),
            Format::Gzip => tr("empty gzip members with FEXTRA at the end"),
            Format::Iso => tr("after the image"),
            Format::Raw => tr("simply at the end (the file grows — programs may notice it)"),
        }
    }
}

impl TryFrom<u8> for Format {
    type Error = u8;

    fn try_from(v: u8) -> Result<Self, u8> {
        Format::ALL.into_iter().find(|f| *f as u8 == v).ok_or(v)
    }
}

/// Protection parameters.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// 0…30 %. Zero changes nothing.
    pub redundancy: u8,
    pub block: BlockSize,
    /// Allow unknown formats (parity simply at the end).
    pub allow_raw: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { redundancy: 10, block: BlockSize::Page4k, allow_raw: false }
    }
}

/// Human-readable size: `1.4 MiB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

// ---------------------------------------------------------------------------
// Virtual file
// ---------------------------------------------------------------------------

/// Writes the parity section into a separate buffer: `write_parity` addresses it with absolute offsets.
struct Section {
    buf: Vec<u8>,
    base: u64,
    pos: u64,
}

impl Write for Section {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let at = (self.pos - self.base) as usize;
        if self.buf.len() < at + data.len() {
            self.buf.resize(at + data.len(), 0);
        }
        self.buf[at..at + data.len()].copy_from_slice(data);
        self.pos += data.len() as u64;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for Section {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        match to {
            SeekFrom::Start(p) if p >= self.base => self.pos = p,
            _ => {
                return Err(io::Error::other(tr("parity section: unsupported seek")));
            }
        }
        Ok(self.pos)
    }
}

/// Assembles the map for the original: the space for parity and the map grows until everything fits
/// (wrapper headers depend on the parity size, and that depends on the size of the protected area).
fn prepare(orig: &[u8], format: Format, opts: &Options) -> Result<(Plan, Vec<u8>)> {
    ensure!(!orig.is_empty(), tr("empty file"));
    let (mut payload, mut map) = (0u64, 256u64);
    for _ in 0..64 {
        let built = formats::build(format, orig, payload, map)?;
        let mut pieces: Vec<Piece> = Vec::new();
        for seg in &built.segs {
            let piece = match seg {
                formats::Seg::Orig(r) => Piece::Keep(r.len() as u64),
                formats::Seg::Bytes(b) => Piece::Keep(b.len() as u64),
                formats::Seg::Payload(n) => Piece::Payload(*n),
                formats::Seg::Map(n) => Piece::Map(*n),
                formats::Seg::OggCrc => Piece::OggCrc,
            };
            match (pieces.last_mut(), piece) {
                (_, Piece::Keep(0) | Piece::Payload(0)) => {}
                (Some(Piece::Keep(a)), Piece::Keep(b)) | (Some(Piece::Payload(a)), Piece::Payload(b)) => *a += b,
                _ => pieces.push(piece),
            }
        }
        let plan = Plan {
            format,
            block: opts.block,
            redundancy: opts.redundancy,
            orig_len: orig.len() as u64,
            orig_crc: crc32fast::hash(orig),
            extra: built.extra,
            pieces,
        };
        let layout = plan.layout().context(tr("invalid redundancy parameters"))?;
        let need_map = plan.encode().len() as u64;
        if plan.payload_len() < layout.len() {
            payload = layout.len();
            continue;
        }
        if plan.map_slots().iter().any(|&(_, cap)| cap < need_map) {
            map = need_map.next_multiple_of(64);
            continue;
        }

        let mut v = Vec::with_capacity(layout.end() as usize);
        for seg in &built.segs {
            match seg {
                formats::Seg::Orig(r) => v.extend_from_slice(&orig[r.clone()]),
                formats::Seg::Bytes(b) => v.extend_from_slice(b),
                _ => {}
            }
        }
        v.resize(layout.offset as usize, 0);
        let mut section = Section { buf: Vec::with_capacity(layout.len() as usize), base: layout.offset, pos: layout.offset };
        vnmf_parity::write_parity(&mut section, &v, &layout)?;
        v.extend_from_slice(&section.buf);
        return Ok((plan, v));
    }
    bail!(tr("could not find a layout"))
}

/// The virtual file from the physical one: protected bytes + zeros up to a block + the parity section.
/// Missing (truncated) bytes are zeros; the CRC check will find them.
fn gather(file: &[u8], plan: &Plan, layout: &ParityLayout) -> Vec<u8> {
    let mut v = vec![0u8; layout.end() as usize];
    let (mut pos, mut keep, mut pay) = (0usize, 0usize, 0usize);
    let section = layout.offset as usize;
    let section_len = layout.len() as usize;
    for &piece in &plan.pieces {
        let len = piece.len() as usize;
        let src = file.get(pos.min(file.len())..(pos + len).min(file.len())).unwrap_or(&[]);
        match piece {
            Piece::Keep(_) => {
                v[keep..keep + src.len()].copy_from_slice(src);
                keep += len;
            }
            Piece::Payload(_) => {
                let n = src.len().min(section_len.saturating_sub(pay));
                v[section + pay..section + pay + n].copy_from_slice(&src[..n]);
                pay += len;
            }
            Piece::Map(_) | Piece::OggCrc => {}
        }
        pos += len;
    }
    v
}

/// Writes the physical file from the virtual one: auxiliary pieces are recomputed.
fn emit<W: Read + Write + Seek>(out: &mut W, plan: &Plan, v: &[u8], layout: &ParityLayout) -> io::Result<()> {
    let map = plan.encode();
    let zeros = vec![0u8; 1 << 16];
    let mut crcs = Vec::new();
    {
        let mut w = BufWriter::with_capacity(1 << 20, &mut *out);
        let write_zeros = |w: &mut BufWriter<_>, mut n: usize| -> io::Result<()> {
            while n > 0 {
                let k = n.min(zeros.len());
                w.write_all(&zeros[..k])?;
                n -= k;
            }
            Ok(())
        };
        let (mut pos, mut keep, mut pay) = (0u64, 0usize, 0usize);
        let section = layout.offset as usize;
        let section_len = layout.len() as usize;
        for &piece in &plan.pieces {
            let len = piece.len() as usize;
            match piece {
                Piece::Keep(_) => {
                    w.write_all(&v[keep..keep + len])?;
                    keep += len;
                }
                Piece::Payload(_) => {
                    let n = len.min(section_len.saturating_sub(pay));
                    w.write_all(&v[section + pay..section + pay + n])?;
                    write_zeros(&mut w, len - n)?;
                    pay += len;
                }
                Piece::Map(_) => {
                    w.write_all(&map)?;
                    write_zeros(&mut w, len - map.len())?;
                }
                Piece::OggCrc => {
                    crcs.push(pos);
                    w.write_all(&[0; 4])?;
                }
            }
            pos += len as u64;
        }
        w.flush()?;
    }
    // Ogg page CRCs — once the pages are fully written.
    let mut page = Vec::new();
    for at in crcs {
        out.seek(SeekFrom::Start(at - 22))?;
        page.resize(27, 0);
        out.read_exact(&mut page)?;
        let head = 27 + page[26] as usize;
        page.resize(head, 0);
        out.read_exact(&mut page[27..])?;
        let len = formats::page_len(&page).ok_or_else(|| io::Error::other(tr("Ogg: corrupted page")))?;
        page.resize(len, 0);
        out.read_exact(&mut page[head..])?;
        let crc = formats::page_crc(&page);
        out.seek(SeekFrom::Start(at))?;
        out.write_all(&crc.to_le_bytes())?;
    }
    out.seek(SeekFrom::End(0))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Search and check
// ---------------------------------------------------------------------------

/// Finds the map by signature. A copy counts only if it lies where its own map says it should
/// (this rejects maps of nested protected files). The map with the most copies wins.
fn find_plan(file: &[u8]) -> Option<(Plan, usize)> {
    let mut found: Vec<(Plan, usize)> = Vec::new();
    for pos in memchr::memmem::find_iter(file, &MAP_MAGIC) {
        let Some(plan) = Plan::decode(&file[pos..]) else {
            continue;
        };
        if !plan.map_slots().iter().any(|&(at, _)| at == pos as u64) {
            continue;
        }
        match found.iter_mut().find(|(p, _)| *p == plan) {
            Some((_, n)) => *n += 1,
            None => found.push((plan, 1)),
        }
    }
    found.into_iter().max_by_key(|(plan, n)| (*n, plan.total_len() == file.len() as u64))
}

/// State of a protected file.
#[derive(Debug, Clone)]
pub struct Inspection {
    pub plan: Plan,
    pub layout: ParityLayout,
    /// State of the protected area and parity (`vnmf-parity`).
    pub status: ParityStatus,
    pub file_len: u64,
    pub maps_total: usize,
    pub maps_intact: usize,
    /// Wrong CRCs of Ogg pages carrying parity (regenerated during repair).
    pub bad_page_crcs: usize,
}

impl Inspection {
    pub fn format(&self) -> Format {
        self.plan.format
    }

    /// Nothing is damaged: neither data, nor parity, nor auxiliary fields, nor the length.
    pub fn is_clean(&self) -> bool {
        self.status.is_clean() && !self.service_damaged()
    }

    pub fn is_recoverable(&self) -> bool {
        self.status.is_recoverable()
    }

    /// Only auxiliary things are damaged (map copies, page CRCs, file length).
    pub fn service_damaged(&self) -> bool {
        self.maps_intact < self.maps_total || self.bad_page_crcs > 0 || self.file_len != self.plan.total_len()
    }

    /// How many bytes of protected data are damaged (to block precision).
    pub fn damaged_bytes(&self) -> u64 {
        (self.status.bad_data.len() * self.layout.shard_size) as u64
    }

    /// A short one-line description of the state.
    pub fn summary(&self) -> String {
        let head = format!("{}, {}%", self.plan.format.name(), self.plan.redundancy);
        if self.is_clean() {
            return trf!("{0}: intact", head);
        }
        let mut parts = Vec::new();
        if !self.status.bad_data.is_empty() {
            parts.push(trf!("damaged data ≈{0}", human_size(self.damaged_bytes())));
        }
        if !self.status.bad_parity.is_empty() {
            parts.push(trf!("parity blocks: {0}", self.status.bad_parity.len()));
        }
        if self.file_len < self.plan.total_len() {
            parts.push(trf!("truncated by {0}", human_size(self.plan.total_len() - self.file_len)));
        } else if self.file_len > self.plan.total_len() {
            parts.push(trf!("extra tail {0}", human_size(self.file_len - self.plan.total_len())));
        }
        if parts.is_empty() {
            parts.push(tr("auxiliary fields damaged").into());
        }
        let verdict = if self.is_recoverable() { tr("recoverable") } else { tr("cannot be fully recovered") };
        format!("{head}: {} — {verdict}", parts.join(", "))
    }
}

/// Checks the file bytes. `None` — no protection (map not found).
fn inspect_bytes(file: &[u8]) -> Option<(Inspection, Vec<u8>)> {
    let (plan, _) = find_plan(file)?;
    let layout = plan.layout()?;
    let v = gather(file, &plan, &layout);
    let status = vnmf_parity::check(&v, &layout);

    // Map copies that passed CRC but have damaged padding also need to be rewritten.
    let map = plan.encode();
    let slots = plan.map_slots();
    let intact = slots
        .iter()
        .filter(|&&(at, cap)| {
            let (at, cap) = (at as usize, cap as usize);
            file.get(at..at + cap).is_some_and(|s| s.starts_with(&map) && s[map.len()..].iter().all(|&b| b == 0))
        })
        .count();
    let mut bad_page_crcs = 0;
    let mut pos = 0usize;
    for &piece in &plan.pieces {
        if piece == Piece::OggCrc {
            let page = file.get(pos - 22..).and_then(|p| Some(&p[..formats::page_len(p).filter(|&l| l <= p.len())?]));
            if page.is_none_or(|p| formats::page_crc(p).to_le_bytes() != p[22..26]) {
                bad_page_crcs += 1;
            }
        }
        pos += piece.len() as usize;
    }
    let inspection = Inspection { layout, status, file_len: file.len() as u64, maps_total: slots.len(), maps_intact: intact, bad_page_crcs, plan };
    Some((inspection, v))
}

// ---------------------------------------------------------------------------
// Operations on bytes (in memory)
// ---------------------------------------------------------------------------

/// Protects the file bytes. `Ok(None)` — redundancy 0 %, nothing changed.
pub fn protect_bytes(orig: &[u8], opts: &Options) -> Result<Option<(Vec<u8>, Plan)>> {
    if opts.redundancy == 0 {
        return Ok(None);
    }
    ensure!(opts.redundancy <= MAX_REDUNDANCY, "redundancy must be from 0 to {MAX_REDUNDANCY}%");
    let format = match formats::detect(orig) {
        Some(f) => f,
        None if opts.allow_raw => Format::Raw,
        None => bail!("unknown format (supported: {})", supported_list()),
    };
    let (plan, v) = prepare(orig, format, opts)?;
    let layout = plan.layout().expect("layout was checked in prepare");
    let mut out = Cursor::new(Vec::with_capacity(plan.total_len() as usize));
    emit(&mut out, &plan, &v, &layout)?;
    Ok(Some((out.into_inner(), plan)))
}

/// Checks the file bytes; `None` — the file is not protected.
pub fn inspect_slice(file: &[u8]) -> Option<Inspection> {
    inspect_bytes(file).map(|(i, _)| i)
}

/// There's no map, but there are traces of protection (map or parity CRC table signatures) — meaning the file
/// was protected, and the damage destroyed both map copies. That's an error, not "not protected".
fn ensure_no_traces(file: &[u8]) -> Result<()> {
    let traces = [&MAP_MAGIC, &vnmf_parity::PARITY_MAGIC].iter().any(|m| memchr::memmem::find(file, *m).is_some());
    ensure!(!traces, tr("the file was protected, but both map copies are destroyed — recovery is impossible"));
    Ok(())
}

/// Repairs the file bytes. `None` — the file is not protected; otherwise — the state before repair, the
/// `vnmf-parity` report (`None` if the data was intact) and the new contents (if anything changed).
pub fn repair_bytes(file: &[u8]) -> Result<Option<Repaired>> {
    let Some((inspection, mut v)) = inspect_bytes(file) else {
        return Ok(None);
    };
    if inspection.is_clean() {
        return Ok(Some(Repaired { inspection, report: None, bytes: None }));
    }
    let report = if inspection.status.is_clean() { None } else { Some(vnmf_parity::repair(&mut v, &inspection.layout)?) };
    let mut out = Cursor::new(Vec::with_capacity(inspection.plan.total_len() as usize));
    emit(&mut out, &inspection.plan, &v, &inspection.layout)?;
    Ok(Some(Repaired { inspection, report, bytes: Some(out.into_inner()) }))
}

/// Result of [`repair_bytes`].
pub struct Repaired {
    pub inspection: Inspection,
    pub report: Option<RepairReport>,
    pub bytes: Option<Vec<u8>>,
}

impl Repaired {
    /// Whether everything could be recovered.
    pub fn is_complete(&self) -> bool {
        self.report.as_ref().is_none_or(RepairReport::is_complete)
    }
}

/// Removes protection: returns the file without parity (repairing it first if needed).
/// The second element — whether the result matched the original before protection (length and CRC32).
pub fn strip_bytes(file: &[u8]) -> Result<Option<(Vec<u8>, bool)>> {
    let Some(repaired) = repair_bytes(file)? else {
        return Ok(None);
    };
    ensure!(repaired.is_complete(), tr("the file is damaged more than the parity can recover — protection not removed"));
    let plan = &repaired.inspection.plan;
    let out = formats::strip(repaired.bytes.as_deref().unwrap_or(file), plan)?;
    let same = out.len() as u64 == plan.orig_len && crc32fast::hash(&out) == plan.orig_crc;
    Ok(Some((out, same)))
}

fn supported_list() -> String {
    Format::ALL.iter().flat_map(|f| f.extensions()).copied().collect::<Vec<_>>().join(", ")
}

// ---------------------------------------------------------------------------
// Operations on files
// ---------------------------------------------------------------------------

/// Checks a file. `Ok(None)` — the file is not protected.
pub fn inspect(path: &Path) -> Result<Option<Inspection>> {
    let file = fs::read(path).with_context(|| trf!("reading {0}", path.display()))?;
    let inspection = inspect_slice(&file);
    if inspection.is_none() {
        ensure_no_traces(&file)?;
    }
    Ok(inspection)
}

/// Writes a file atomically: to a temporary file next to it, then renames. Permissions are copied from `like`.
fn write_atomic(target: &Path, like: &Path, write: impl FnOnce(&mut File) -> Result<()>) -> Result<()> {
    let name = target.file_name().context(tr("path without a file name"))?.to_string_lossy();
    let tmp = target.with_file_name(format!(".{name}.vnmfguard-{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = File::options().read(true).write(true).create_new(true).open(&tmp)?;
        write(&mut file)?;
        file.sync_all()?;
        if let Ok(meta) = fs::metadata(like) {
            fs::set_permissions(&tmp, meta.permissions())?;
        }
        fs::rename(&tmp, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| trf!("writing {0}", target.display()))
}

/// Result of protecting a file.
#[derive(Debug, Clone)]
pub enum ProtectOutcome {
    /// Protected (`previous` — the percentage of the previous protection, if the file was re-protected).
    Protected { plan: Plan, original_len: u64, new_len: u64, previous: Option<u8> },
    /// The file is already protected with the same parameters.
    AlreadyProtected(u8),
    /// Redundancy 0 % — nothing changed.
    Nothing,
}

/// Protects a file (in place or into `output`). An already protected file with different parameters
/// is re-protected (the old protection is removed first).
pub fn protect(input: &Path, output: Option<&Path>, opts: &Options) -> Result<ProtectOutcome> {
    if opts.redundancy == 0 {
        return Ok(ProtectOutcome::Nothing);
    }
    let mut orig = fs::read(input).with_context(|| trf!("reading {0}", input.display()))?;
    let mut previous = None;
    if let Some(inspection) = inspect_slice(&orig) {
        if inspection.plan.redundancy == opts.redundancy && inspection.plan.block == opts.block && inspection.is_clean() {
            if let Some(out) = output.filter(|o| *o != input) {
                fs::copy(input, out).with_context(|| trf!("copying to {0}", out.display()))?;
            }
            return Ok(ProtectOutcome::AlreadyProtected(inspection.plan.redundancy));
        }
        previous = Some(inspection.plan.redundancy);
        orig = strip_bytes(&orig)?.expect("protection was just found").0;
    }
    let original_len = orig.len() as u64;
    let format = match formats::detect(&orig) {
        Some(f) => f,
        None if opts.allow_raw => Format::Raw,
        None => bail!("unknown format (supported: {})", supported_list()),
    };
    ensure!(opts.redundancy <= MAX_REDUNDANCY, "redundancy must be from 0 to {MAX_REDUNDANCY}%");
    let (plan, v) = prepare(&orig, format, opts)?;
    drop(orig);
    let layout = plan.layout().expect("layout was checked in prepare");
    write_atomic(output.unwrap_or(input), input, |f| Ok(emit(f, &plan, &v, &layout)?))?;
    Ok(ProtectOutcome::Protected { new_len: plan.total_len(), plan, original_len, previous })
}

/// Result of repairing a file.
#[derive(Debug, Clone)]
pub enum RepairOutcome {
    Unprotected,
    /// Nothing needs repairing (if `output` is set — the file was copied there).
    Intact(Inspection),
    /// The file was rewritten. `report` — `None` if only auxiliary things were damaged.
    Repaired {
        before: Inspection,
        report: Option<RepairReport>,
        target: PathBuf,
    },
}

/// Repairs a file (in place or into `output`).
pub fn repair(input: &Path, output: Option<&Path>) -> Result<RepairOutcome> {
    let file = fs::read(input).with_context(|| trf!("reading {0}", input.display()))?;
    let Some((before, mut v)) = inspect_bytes(&file) else {
        ensure_no_traces(&file)?;
        return Ok(RepairOutcome::Unprotected);
    };
    drop(file);
    if before.is_clean() {
        if let Some(out) = output.filter(|o| *o != input) {
            fs::copy(input, out).with_context(|| trf!("copying to {0}", out.display()))?;
        }
        return Ok(RepairOutcome::Intact(before));
    }
    let report = if before.status.is_clean() { None } else { Some(vnmf_parity::repair(&mut v, &before.layout)?) };
    let target = output.unwrap_or(input).to_path_buf();
    write_atomic(&target, input, |f| Ok(emit(f, &before.plan, &v, &before.layout)?))?;
    Ok(RepairOutcome::Repaired { before, report, target })
}

/// Result of removing protection.
#[derive(Debug, Clone, Copy)]
pub enum StripOutcome {
    Unprotected,
    /// `identical` — the result matched the file before protection byte for byte.
    Stripped {
        identical: bool,
        len: u64,
    },
}

/// Removes protection (in place or into `output`).
pub fn strip(input: &Path, output: Option<&Path>) -> Result<StripOutcome> {
    let file = fs::read(input).with_context(|| trf!("reading {0}", input.display()))?;
    let Some((bytes, identical)) = strip_bytes(&file)? else {
        ensure_no_traces(&file)?;
        return Ok(StripOutcome::Unprotected);
    };
    drop(file);
    write_atomic(output.unwrap_or(input), input, |f| Ok(f.write_all(&bytes)?))?;
    Ok(StripOutcome::Stripped { identical, len: bytes.len() as u64 })
}
