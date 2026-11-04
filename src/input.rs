// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Collecting input for `vnmfpack` and VNMF Creator: CBZ archives, folders, individual files.
//!
//! The input type and the format of each file are detected by contents (magic bytes), not by extension:
//! a `.cbz` renamed to `.zip`, or a `.jpg` that's actually WebP, are handled correctly.
//!
//! **Structure.** A folder (or CBZ) with subfolders gives several chapters: a chapter is a folder containing
//! images, a volume is the folder above it. Numbers and titles are taken from folder names
//! (`Volume 1/Chapter 3 - Title`, `vol01/ch0001`, `Vol.2 Ch.15`, also Ukrainian/Russian markers like `Том`/`Розділ`/`Глава`), and for a chapter with its own
//! `ComicInfo.xml` — from the `<Volume>`, `<Number>`, `<Title>` fields. A folder without subfolders (or a CBZ without
//! folders) is a single chapter, as before.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use vnmf_core::{AuxInput, ChunkSource, InfoFormat, PageFormat};

use crate::auxfiles;
use crate::comicinfo;
use crate::contents;
use crate::metadata::{self, Metadata};
use crate::natsort::natural_cmp;

/// What exactly was given as input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Cbz,
    Dir,
    Image(PageFormat),
    /// ComicInfo.xml, info.json, etc., given as a separate file.
    Metadata(InfoFormat),
}

/// ZIP signatures: a local file header or an empty archive.
fn is_zip(head: &[u8]) -> bool {
    head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06")
}

fn read_head(path: &Path) -> Result<Vec<u8>> {
    let mut head = Vec::with_capacity(64);
    File::open(path).with_context(|| format!("could not open {}", path.display()))?.take(64).read_to_end(&mut head)?;
    Ok(head)
}

impl InputKind {
    pub fn detect(path: &Path) -> Result<Self> {
        if path.is_dir() {
            return Ok(InputKind::Dir);
        }
        let head = read_head(path)?;
        if is_zip(&head) {
            return Ok(InputKind::Cbz);
        }
        let image = PageFormat::detect(&head);
        if image.is_known() {
            return Ok(InputKind::Image(image));
        }
        match InfoFormat::detect(&head) {
            InfoFormat::Binary => bail!("unknown file format: {}", path.display()),
            format => Ok(InputKind::Metadata(format)),
        }
    }
}

/// A single image page.
pub struct PageEntry {
    /// Name inside the archive / relative path — for sorting and messages.
    pub name: String,
    pub source: ChunkSource,
    pub format: PageFormat,
}

/// Chapter: the contents of one CBZ, one folder (or subfolder) or a group of individual files.
#[derive(Default)]
pub struct Chapter {
    pub name: String,
    pub pages: Vec<PageEntry>,
    /// ComicInfo.xml, already converted to JSON.
    pub comic_info: Option<Value>,
    /// Other metadata (info.json / info.toml / …) — as is.
    pub info: Option<Vec<u8>>,
    /// Previews from the `previews/` subfolder (as `vnmfunpack` creates it): (volume_id, source).
    pub previews: Vec<(u16, ChunkSource)>,
    /// Attachments from the `aux/` subfolder (see [`crate::auxfiles`]): (image index in the chapter, attachment).
    pub aux: Vec<(usize, AuxInput)>,
    /// Skipped files (neither images nor metadata).
    pub skipped: Vec<String>,
    /// Volume from the folder name / `<Volume>`; `None` — not specified.
    pub volume: Option<u16>,
    pub volume_title: Option<String>,
    /// Chapter number for display ("10.5") from the name / `<Number>`.
    pub number: Option<String>,
    pub title: Option<String>,
    /// Table of contents from `vnmf.json` (or format 1.1 `contents.json`), saved by `vnmfunpack`.
    pub contents: Option<contents::Contents>,
    /// VNMF metadata (`vnmf.json`) — takes precedence over ComicInfo.
    pub metadata: Option<Metadata>,
    /// Individual images passed one by one (not a CBZ or a folder): not a standalone chapter.
    pub loose: bool,
}

impl Chapter {
    fn named(name: String) -> Self {
        Self { name, ..Default::default() }
    }

    fn sort(&mut self) {
        self.pages.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    }

    /// Parses a metadata file by its name and contents.
    fn add_metadata(&mut self, name: &str, data: Vec<u8>) -> Result<bool> {
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let format = InfoFormat::detect(&data);

        if base.eq_ignore_ascii_case(comicinfo::FILE_NAME) || (format == InfoFormat::Xml && is_comicinfo_xml(&data)) {
            let xml = String::from_utf8(data).with_context(|| format!("{name}: not UTF-8"))?;
            self.comic_info = Some(comicinfo::xml_to_json(&xml).with_context(|| format!("{name}: invalid XML"))?);
            return Ok(true);
        }
        if base.eq_ignore_ascii_case(contents::LEGACY_FILE_NAME) && format == InfoFormat::Json {
            let json: Value = serde_json::from_slice(&data).with_context(|| format!("{name}: invalid JSON"))?;
            self.contents = Some(contents::Contents::from_legacy_json(&json));
            return Ok(true);
        }
        // VNMF metadata: vnmf.json or any JSON with "schema": "vnmf-metadata/…".
        if format == InfoFormat::Json {
            let json: Option<Value> = serde_json::from_slice(&data).ok();
            let is_vnmf = json.as_ref().and_then(|j| j.get("schema")?.as_str()).is_some_and(|s| s.starts_with("vnmf-metadata/"));
            if base.eq_ignore_ascii_case(metadata::FILE_NAME) || is_vnmf {
                let meta = json.as_ref().and_then(Metadata::from_json).with_context(|| format!("{name}: this is not VNMF metadata"))?;
                self.contents = Some(meta.contents.clone());
                self.metadata = Some(meta);
                return Ok(true);
            }
        }

        let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
        if stem.eq_ignore_ascii_case("info") && format != InfoFormat::Binary {
            self.info = Some(data);
            return Ok(true);
        }
        Ok(false)
    }

    /// Volume, number and title of the chapter from its own ComicInfo (the `<Volume>`, `<Number>`, `<Title>` fields)
    /// and, where missing, from the name (`Vol.1 Ch.5 - Title`).
    fn derive_identity(&mut self, name_has_meaning: bool) {
        let from_name = parse_name(&self.name);
        if let Some(ci) = &self.comic_info {
            self.volume = self.volume.or(comicinfo::volume_number(ci));
            self.number = self.number.take().or(comicinfo::text_field(ci, "Number"));
            // Title in ComicInfo is often "Vol.1 Ch.5 - Title" — take only the title.
            self.title = self.title.take().or_else(|| {
                comicinfo::text_field(ci, "Title").map(|t| {
                    let parsed = parse_name(&t);
                    if parsed.has_markers() { parsed.title.unwrap_or(t) } else { t }
                })
            });
        }
        self.volume = self.volume.or(from_name.volume);
        self.number = self.number.take().or(from_name.number.clone());
        if self.title.is_none() && (from_name.has_markers() || name_has_meaning) {
            self.title = from_name.title_or_whole(&self.name, name_has_meaning);
        }
    }
}

fn is_comicinfo_xml(data: &[u8]) -> bool {
    let head = &data[..data.len().min(512)];
    String::from_utf8_lossy(head).contains("<ComicInfo")
}

/// Auxiliary archive and OS files that are never pages.
fn is_junk(name: &str) -> bool {
    name.split(['/', '\\']).any(|part| part.starts_with('.') || part == "__MACOSX" || part == "Thumbs.db")
}

/// Collects chapters from all inputs. Each CBZ and each folder is a chapter (or several, if there are
/// subfolders inside); consecutive individual files are merged into one chapter (in the order they were given).
pub fn load_inputs(paths: &[PathBuf]) -> Result<Vec<Chapter>> {
    let mut chapters = Vec::new();
    let new_loose = || Chapter { loose: true, ..Chapter::named("files".into()) };
    let mut loose = new_loose();

    let flush = |loose: &mut Chapter, chapters: &mut Vec<Chapter>| {
        if !loose.pages.is_empty() {
            chapters.push(std::mem::replace(loose, new_loose()));
        }
    };

    for path in paths {
        match InputKind::detect(path)? {
            InputKind::Cbz => {
                flush(&mut loose, &mut chapters);
                chapters.extend(load_cbz(path)?);
            }
            InputKind::Dir => {
                flush(&mut loose, &mut chapters);
                chapters.extend(load_dir(path)?);
            }
            InputKind::Image(format) => loose.pages.push(PageEntry { name: path.display().to_string(), source: ChunkSource::from_file(path)?, format }),
            InputKind::Metadata(_) => {
                let data = fs::read(path)?;
                let name = path.display().to_string();
                if !loose.add_metadata(&name, data)? {
                    bail!("{name}: expected ComicInfo.xml, contents.json or info.json/toml/txt");
                }
            }
        }
    }
    flush(&mut loose, &mut chapters);
    // Metadata without pages (`vnmfpack ComicInfo.xml`) — don't drop it silently.
    if loose.comic_info.is_some() || loose.info.is_some() || loose.contents.is_some() {
        match chapters.last_mut() {
            Some(last) => {
                last.comic_info = last.comic_info.take().or(loose.comic_info);
                last.info = last.info.take().or(loose.info);
                last.contents = last.contents.take().or(loose.contents);
                last.metadata = last.metadata.take().or(loose.metadata);
            }
            None => bail!("metadata was given, but no images"),
        }
    }
    Ok(chapters)
}

/// Contents of a CBZ or a folder before splitting into chapters.
struct Collected {
    name: String,
    /// Images (name — path relative to the root).
    pages: Vec<PageEntry>,
    /// Metadata: (relative path, bytes).
    metadata: Vec<(String, Vec<u8>)>,
    previews: Vec<(u16, ChunkSource)>,
    /// Attachments from `aux/NNN.*`: (image number in the input from 0, attachment).
    aux: Vec<(usize, AuxInput)>,
    skipped: Vec<String>,
}

impl Collected {
    fn new(name: String) -> Self {
        Self { name, pages: Vec::new(), metadata: Vec::new(), previews: Vec::new(), aux: Vec::new(), skipped: Vec::new() }
    }
}

/// Reads a CBZ into memory: pages become `ChunkSource::from_bytes`, ComicInfo.xml → JSON.
pub fn load_cbz(path: &Path) -> Result<Vec<Chapter>> {
    let file = File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).with_context(|| format!("{}: damaged ZIP", path.display()))?;
    let mut collected = Collected::new(file_stem(path));

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name()?.into_owned();
        if is_junk(&name) {
            continue;
        }
        // Zip bomb protection: a chunk in the format can't exceed u32::MAX.
        if entry.size() > u64::from(u32::MAX) {
            bail!("{name}: entry is too large ({} bytes)", entry.size());
        }
        let mut data = Vec::with_capacity(entry.size() as usize);
        (&mut entry).take(u64::from(u32::MAX) + 1).read_to_end(&mut data)?;
        drop(entry);

        let format = PageFormat::detect(&data);
        if format.is_known() {
            let source = ChunkSource::from_bytes(data)?.with_label(name.clone());
            collected.pages.push(PageEntry { name, source, format });
        } else {
            collected.metadata.push((name, data));
        }
    }
    split_chapters(collected)
}

/// Walks a folder recursively. Images are read from disk only during the build.
pub fn load_dir(root: &Path) -> Result<Vec<Chapter>> {
    let mut collected = Collected::new(file_stem(root));
    let mut stack = vec![root.to_path_buf()];
    let mut aux_files: Vec<(String, PathBuf)> = Vec::new();
    let mut aux_manifest = serde_json::Map::new();

    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("could not read {}", dir.display()))? {
            let path = entry?.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if is_junk(&rel) {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            // Auxiliary subfolders created by `vnmfunpack` — so the folder packs back symmetrically.
            if let Some(name) = rel.strip_prefix("previews/") {
                collected.previews.push((preview_volume(name), ChunkSource::from_file(&path)?));
                continue;
            }
            if let Some(name) = rel.strip_prefix("aux/") {
                if name == auxfiles::MANIFEST {
                    aux_manifest = auxfiles::parse_manifest(&fs::read(&path)?)?;
                } else {
                    aux_files.push((name.to_string(), path));
                }
                continue;
            }

            let format = PageFormat::detect(&read_head(&path)?);
            if format.is_known() {
                collected.pages.push(PageEntry { name: rel, source: ChunkSource::from_file(&path)?, format });
            } else {
                collected.metadata.push((rel, fs::read(&path)?));
            }
        }
    }
    // Attachments — after the walk: parameters from `aux.json` may have come later than the files themselves.
    aux_files.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    for (name, path) in aux_files {
        let head = read_head(&path)?;
        match auxfiles::parse(&name, ChunkSource::from_file(&path)?.with_label(name.clone()), &aux_manifest, &head) {
            Some(entry) => collected.aux.push(entry),
            None => collected.skipped.push(format!("aux/{name}")),
        }
    }
    split_chapters(collected)
}

/// Folder of a path: `"vol01/ch0001/001.png"` → `"vol01/ch0001"`, `"001.png"` → `""`.
fn parent_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn last_component(dir: &str) -> &str {
    dir.rsplit('/').next().unwrap_or(dir)
}

/// Splits the input's contents into chapters by folders (see the module description).
fn split_chapters(mut c: Collected) -> Result<Vec<Chapter>> {
    // Chapters: folders that directly contain images, in natural order.
    let mut by_dir: BTreeMap<String, Vec<PageEntry>> = BTreeMap::new();
    for page in std::mem::take(&mut c.pages) {
        by_dir.entry(parent_of(&page.name).to_string()).or_default().push(page);
    }
    let mut dirs: Vec<String> = by_dir.keys().cloned().collect();
    dirs.sort_by(|a, b| natural_cmp(a, b));

    // Metadata: a chapter's — in its folder; the rest (root, higher folders) — for the whole file.
    let mut root_meta = Chapter::named(c.name.clone());
    let mut chapter_meta: BTreeMap<String, Chapter> = BTreeMap::new();
    for (rel, data) in c.metadata {
        let dir = parent_of(&rel);
        let target = if dirs.len() > 1 && by_dir.contains_key(dir) {
            chapter_meta.entry(dir.to_string()).or_insert_with(|| Chapter::named(String::new()))
        } else {
            &mut root_meta
        };
        if !target.add_metadata(&rel, data)? {
            root_meta.skipped.push(rel);
        }
    }

    let single = dirs.len() <= 1;
    // Volumes are the folders above chapters; the number from the name, otherwise ordinal.
    let mut volume_dirs: Vec<String> = dirs.iter().map(|d| parent_of(d).to_string()).filter(|d| !d.is_empty()).collect();
    volume_dirs.dedup();
    let mut chapters = Vec::new();
    for (i, dir) in dirs.iter().enumerate() {
        let mut ch = chapter_meta.remove(dir).unwrap_or_default();
        ch.name = if single { c.name.clone() } else { format!("{}/{}", c.name, dir) };
        ch.pages = by_dir.remove(dir).unwrap_or_default();
        ch.sort();
        if single {
            // A single chapter: the input's metadata is its own.
            ch.comic_info = root_meta.comic_info.take();
            ch.info = root_meta.info.take();
            ch.contents = root_meta.contents.take();
            ch.metadata = root_meta.metadata.take();
            ch.name = c.name.clone();
            ch.derive_identity(false);
        } else {
            let chapter_dir = last_component(dir).to_string();
            let saved_name = std::mem::replace(&mut ch.name, chapter_dir);
            ch.derive_identity(!dir.is_empty());
            ch.name = saved_name;
            let vol_dir = parent_of(dir);
            if !vol_dir.is_empty() && ch.volume.is_none() {
                let parsed = parse_name(last_component(vol_dir));
                let ordinal = volume_dirs.iter().position(|d| d == vol_dir).map_or(1, |p| p + 1);
                ch.volume = parsed.volume.or_else(|| parsed.number.as_deref().and_then(|n| n.parse().ok())).or(u16::try_from(ordinal).ok());
                ch.volume_title = parsed.title_or_whole(last_component(vol_dir), true);
            }
            if i == 0 {
                // Metadata of the whole file — to the first chapter (that's where vnmfpack / Creator will find it).
                ch.comic_info = ch.comic_info.take().or(root_meta.comic_info.take());
                ch.info = ch.info.take().or(root_meta.info.take());
                ch.contents = root_meta.contents.take();
                ch.metadata = root_meta.metadata.take();
            }
        }
        chapters.push(ch);
    }

    let Some(first) = chapters.first_mut() else {
        // Empty input — a "chapter" without pages, so the caller sees the skipped files.
        let mut empty = Chapter::named(c.name);
        empty.skipped = c.skipped;
        empty.skipped.append(&mut root_meta.skipped);
        return Ok(vec![empty]);
    };
    first.previews = c.previews;
    first.skipped = c.skipped;
    first.skipped.append(&mut root_meta.skipped);

    // Aux from `aux/` is numbered across all pages of the input — distribute it among the chapters.
    let mut starts = Vec::with_capacity(chapters.len());
    let mut total = 0;
    for ch in &chapters {
        starts.push(total);
        total += ch.pages.len();
    }
    for (global, src) in c.aux {
        let k = starts.iter().rposition(|&s| s <= global).unwrap_or(0);
        chapters[k].aux.push((global - starts[k], src));
    }
    Ok(chapters)
}

/// Numbers and a title recognized in a file or folder name.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NameParts {
    pub volume: Option<u16>,
    pub number: Option<String>,
    pub title: Option<String>,
    markers: bool,
}

impl NameParts {
    pub fn has_markers(&self) -> bool {
        self.markers
    }

    /// The title, and if the name has neither markers nor a number — the whole name (for folders like "Prologue").
    fn title_or_whole(&self, whole: &str, whole_allowed: bool) -> Option<String> {
        if self.title.is_some() || self.markers || self.number.is_some() {
            return self.title.clone();
        }
        whole_allowed.then(|| whole.trim().to_string()).filter(|t| !t.is_empty())
    }
}

const VOLUME_MARKERS: [&str; 5] = ["volume", "vol", "том", "tome", "v"];
const CHAPTER_MARKERS: [&str; 8] = ["chapter", "розділ", "глава", "episode", "ch", "ep", "гл", "c"];

/// Parses `Vol.1 Ch.5 - Title`, `vol01`, `ch0001`, `Том 2 Розділ 10.5: Назва`, `005`.
/// Short markers (`v`, `c`) must come right before a digit: `v01`, `c5`.
pub fn parse_name(name: &str) -> NameParts {
    let lower = name.to_lowercase();
    let mut parts = NameParts::default();
    let mut end = 0;
    if let Some((num, stop)) = find_marked_number(&lower, &VOLUME_MARKERS) {
        parts.volume = num.split('.').next().and_then(|n| n.parse().ok());
        parts.markers = true;
        end = end.max(stop);
    }
    if let Some((num, stop)) = find_marked_number(&lower, &CHAPTER_MARKERS) {
        parts.number = Some(trim_number(&num));
        parts.markers = true;
        end = end.max(stop);
    }
    if !parts.markers {
        // A name starting with a number: "005", "12 - Title".
        let digits = lower.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(lower.len());
        if digits > 0 && lower[..digits].chars().any(|c| c.is_ascii_digit()) {
            parts.number = Some(trim_number(lower[..digits].trim_end_matches('.')));
            end = digits;
        }
    }
    // The title — after the last number, without separators. `end` is a byte boundary in `lower`;
    // for ASCII letters and Cyrillic the lowercase length is the same.
    let rest = name.get(end..).unwrap_or("").trim_start_matches(|c: char| " -_.:—–)]".contains(c)).trim();
    if end > 0 && !rest.is_empty() {
        parts.title = Some(rest.to_string());
    }
    parts
}

/// `"0005"` → `"5"`, `"010.50"` → `"10.50"`.
fn trim_number(n: &str) -> String {
    let (int, frac) = n.split_once('.').unwrap_or((n, ""));
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    if frac.is_empty() { int.to_string() } else { format!("{int}.{frac}") }
}

/// First occurrence of a marker (at a word boundary) followed by a number. Returns the number and the end of the match.
fn find_marked_number(s: &str, markers: &[&str]) -> Option<(String, usize)> {
    let mut best: Option<(usize, String, usize)> = None;
    for marker in markers {
        let mut from = 0;
        while let Some(pos) = s[from..].find(marker) {
            let start = from + pos;
            from = start + marker.len();
            let before_ok = s[..start].chars().next_back().is_none_or(|c| !c.is_alphabetic());
            if !before_ok {
                continue;
            }
            let after = &s[start + marker.len()..];
            // Short markers — only right next to a digit (otherwise "c" would be found in any word).
            let short = marker.chars().count() == 1;
            let skipped = if short { 0 } else { after.len() - after.trim_start_matches(['.', ' ', '_', '-', '#']).len() };
            let num_part = &after[skipped..];
            let digits = num_part.find(|c: char| !c.is_ascii_digit()).unwrap_or(num_part.len());
            if digits == 0 {
                continue;
            }
            // Fractional part: "10.5".
            let mut len = digits;
            if num_part[digits..].starts_with('.') {
                let frac = num_part[digits + 1..].find(|c: char| !c.is_ascii_digit()).unwrap_or(num_part.len() - digits - 1);
                if frac > 0 {
                    len = digits + 1 + frac;
                }
            }
            let stop = start + marker.len() + skipped + len;
            if best.as_ref().is_none_or(|(s0, _, _)| start < *s0) {
                best = Some((start, num_part[..len].to_string(), stop));
            }
            break;
        }
    }
    best.map(|(_, n, stop)| (n, stop))
}

/// `preview_00_vol3.avif` → 3; without `vol` → 0 (main preview).
fn preview_volume(name: &str) -> u16 {
    name.split_once("vol").and_then(|(_, rest)| leading_number(rest)).and_then(|n| u16::try_from(n).ok()).unwrap_or(0)
}

/// Number at the start of a string: `"005.json"` → 5.
fn leading_number(s: &str) -> Option<usize> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s[..end].parse().ok()
}

pub fn file_stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "vnmf".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: Option<u16>, n: Option<&str>, t: Option<&str>) -> (Option<u16>, Option<String>, Option<String>) {
        (v, n.map(String::from), t.map(String::from))
    }

    fn parsed(name: &str) -> (Option<u16>, Option<String>, Option<String>) {
        let x = parse_name(name);
        (x.volume, x.number, x.title)
    }

    #[test]
    fn parses_common_names() {
        assert_eq!(parsed("Vol.1 Ch.5 - The Beginning"), p(Some(1), Some("5"), Some("The Beginning")));
        assert_eq!(parsed("vol01"), p(Some(1), None, None));
        assert_eq!(parsed("ch0012"), p(None, Some("12"), None));
        assert_eq!(parsed("Том 2 Розділ 10.5: Назва"), p(Some(2), Some("10.5"), Some("Назва")));
        assert_eq!(parsed("v03 c007"), p(Some(3), Some("7"), None));
        assert_eq!(parsed("005"), p(None, Some("5"), None));
        assert_eq!(parsed("12 - Зустріч"), p(None, Some("12"), Some("Зустріч")));
        // No markers and no number — nothing (the folder name is taken separately).
        assert_eq!(parsed("Пролог"), p(None, None, None));
        // "c" inside a word is not a marker.
        assert_eq!(parsed("comic"), p(None, None, None));
    }

    fn write_png(path: &Path, seed: u8) {
        let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        v.extend_from_slice(&10u32.to_be_bytes());
        v.extend_from_slice(&20u32.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, seed]);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, v).unwrap();
    }

    #[test]
    fn nested_dirs_become_volumes_and_chapters() {
        let root = std::env::temp_dir().join(format!("vnmf-input-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (rel, seed) in [
            ("Том 1 - Початок/Розділ 1 - Знайомство/01.png", 1),
            ("Том 1 - Початок/Розділ 1 - Знайомство/02.png", 2),
            ("Том 1 - Початок/Розділ 2/01.png", 3),
            ("Том 2/Розділ 10/01.png", 4),
            ("Том 2/Розділ 9/01.png", 5),
        ] {
            write_png(&root.join(rel), seed);
        }
        fs::write(root.join("ComicInfo.xml"), "<ComicInfo><Series>S</Series></ComicInfo>").unwrap();

        let chapters = load_dir(&root).unwrap();
        let ids: Vec<_> = chapters.iter().map(|c| (c.volume, c.number.clone(), c.title.clone(), c.pages.len())).collect();
        assert_eq!(
            ids,
            vec![
                (Some(1), Some("1".into()), Some("Знайомство".into()), 2),
                (Some(1), Some("2".into()), None, 1),
                // Natural order: 9 before 10.
                (Some(2), Some("9".into()), None, 1),
                (Some(2), Some("10".into()), None, 1),
            ]
        );
        assert_eq!(chapters[0].volume_title.as_deref(), Some("Початок"));
        assert_eq!(chapters[2].volume_title, None);
        assert!(chapters[0].comic_info.is_some(), "the root metadata goes to the first chapter");
        let _ = fs::remove_dir_all(&root);
    }
}
