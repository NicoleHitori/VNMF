// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Building a `.vnmf` from loaded inputs (CBZ, folders, images — see [`crate::input`]).
//!
//! Shared by `vnmfpack` and `vnmf-server` (uploads of CBZ archives): volume and chapter numbers,
//! the table of contents, metadata from `vnmf.json` / ComicInfo, covers, pages and attachments.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use vnmf_core::{BlockSize, Builder, ChunkSource, PageBitFlags, PageFormat, PageRole};

use crate::comicinfo;
use crate::contents::{self, Contents};
use crate::input::Chapter;
use crate::metadata::Metadata;

/// What goes into Info.
pub enum InfoSource {
    /// VNMF metadata (including that converted from ComicInfo or format 1.1 Info).
    Meta(Box<Metadata>),
    /// Another format (TOML, text, arbitrary JSON) — stored as is.
    Raw(Vec<u8>),
}

impl InfoSource {
    /// JSON that reduces to VNMF metadata (incl. Info 1.1) — metadata; anything else — as is.
    pub fn from_bytes(data: Vec<u8>) -> Self {
        match serde_json::from_slice::<Value>(&data).ok().as_ref().and_then(Metadata::from_json) {
            Some(meta) => Self::Meta(Box::new(meta)),
            None => Self::Raw(data),
        }
    }
}

/// The file's cover preview.
#[derive(Clone, Debug, Default)]
pub enum Cover {
    /// Only previews found in the inputs (`previews/` after `vnmfunpack`).
    #[default]
    None,
    /// The first page.
    First,
    /// An image file.
    File(PathBuf),
}

/// Build parameters (the `vnmfpack` options).
pub struct PackOptions {
    /// Volume for chapters that don't specify one.
    pub volume: Option<u16>,
    /// Each chapter becomes one long page made of chunks.
    pub webtoon: bool,
    /// Allow different image formats in one webtoon page.
    pub mixed_formats: bool,
    /// Right-to-left reading (also enabled by the metadata / ComicInfo).
    pub rtl: bool,
    pub block_size: BlockSize,
    /// Reed–Solomon redundancy, %.
    pub redundancy: u8,
    pub tail_duplicate: bool,
    /// Replaces the metadata from the inputs.
    pub info: Option<InfoSource>,
    pub cover: Cover,
    /// UUID of the new file (random if `None`).
    pub uuid: Option<[u8; 16]>,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            volume: None,
            webtoon: false,
            mixed_formats: false,
            rtl: false,
            block_size: vnmf_core::spec::DEFAULT_BLOCK_SIZE,
            redundancy: 0,
            tail_duplicate: true,
            info: None,
            cover: Cover::None,
            uuid: None,
        }
    }
}

/// A builder ready for [`Builder::build`] and the warnings collected on the way.
pub struct Packed {
    pub builder: Builder,
    pub warnings: Vec<String>,
    /// The title from the metadata, if there is one (to name the file).
    pub title: Option<String>,
}

/// Prepares the file from the loaded chapters. Fails if there are no images.
pub fn pack(chapters: Vec<Chapter>, options: PackOptions) -> Result<Packed> {
    let mut warnings = Vec::new();
    if chapters.iter().all(|c| c.pages.is_empty()) {
        bail!("no images found");
    }
    for ch in &chapters {
        if !ch.skipped.is_empty() {
            warnings.push(format!("{}: skipped {} file(s): {}", ch.name, ch.skipped.len(), ch.skipped.join(", ")));
        }
    }

    let ids = assign_ids(&chapters, options.volume);
    let contents = chapters_contents(&chapters, &ids);

    let uuid = options.uuid.unwrap_or_else(|| uuid::Uuid::new_v4().into_bytes());
    let mut builder = Builder::new()
        .with_uuid(uuid)
        .with_block_size(options.block_size)
        .with_tail_duplicate(options.tail_duplicate)
        .with_redundancy(options.redundancy)
        .with_mixed_formats(options.mixed_formats);

    // --- Metadata ---
    let info = match options.info {
        Some(info) => Some(info),
        None => chapters_info(&chapters, &ids),
    };
    // Direction: the option; otherwise as set in the metadata (it takes precedence over the chapters' ComicInfo);
    // otherwise <Manga>YesAndRightToLeft</Manga> in the ComicInfo of any chapter.
    let rtl = options.rtl
        || match &info {
            Some(InfoSource::Meta(m)) if m.reading_direction.is_some() => m.is_rtl(),
            _ => chapters.iter().any(|c| c.comic_info.as_ref().is_some_and(comicinfo::is_rtl)),
        };
    let mut title = None;
    match info {
        // VNMF metadata: volume and chapter titles, reading direction and mode — all together.
        Some(InfoSource::Meta(meta)) => {
            let mut meta = *meta;
            title = meta.title.clone().filter(|t| !t.trim().is_empty());
            let mut merged = meta.contents.clone();
            merged.merge(&contents);
            meta.contents = merged;
            if rtl && meta.reading_direction.is_none() {
                meta.reading_direction = Some("rtl".into());
            }
            if options.webtoon && meta.layout.is_none() {
                meta.layout = Some("strip".into());
            }
            builder.set_info(meta.to_bytes());
        }
        // TOML / text / foreign JSON — as is; chapter titles have nowhere to go then.
        Some(InfoSource::Raw(bytes)) => {
            if !contents.is_empty() {
                warnings.push("Info is not VNMF metadata — volume and chapter titles were not saved".into());
            }
            builder.set_info(bytes);
        }
        None => {
            let mut meta = Metadata::new();
            meta.contents = contents;
            if rtl {
                meta.reading_direction = Some("rtl".into());
            }
            if options.webtoon {
                meta.layout = Some("strip".into());
            }
            if !meta.is_empty() {
                builder.set_info(meta.to_bytes());
            }
        }
    }

    // --- Cover ---
    match &options.cover {
        Cover::First => {
            if let Some(first) = chapters.iter().find_map(|c| c.pages.first()) {
                builder.add_preview(first.source.clone(), 0, 0)?;
            }
        }
        Cover::File(path) => builder.add_preview_file(path, 0, 0).with_context(|| format!("cover {}", path.display()))?,
        Cover::None => {}
    }

    // Previews from `previews/` folders (after `vnmfunpack` into a folder).
    for chapter in &chapters {
        for (volume, source) in &chapter.previews {
            builder.add_preview(source.clone(), *volume, 0)?;
        }
    }

    // --- Pages ---
    let base_flags = if rtl { PageBitFlags::READING_RTL } else { 0 };
    for (chapter, (volume_id, chapter_id)) in chapters.into_iter().zip(ids) {
        if chapter.pages.is_empty() {
            continue;
        }
        if options.webtoon && !options.mixed_formats {
            check_uniform_formats(&chapter)?;
        }
        builder.set_volume(volume_id);
        add_chapter(&mut builder, chapter, chapter_id, base_flags, options.webtoon, &mut warnings)?;
    }
    Ok(Packed { builder, warnings, title })
}

/// A clear error with a hint if webtoon fragments have different formats.
fn check_uniform_formats(chapter: &Chapter) -> Result<()> {
    let mut counts: Vec<(PageFormat, usize, &str)> = Vec::new();
    for page in &chapter.pages {
        match counts.iter_mut().find(|(f, _, _)| *f == page.format) {
            Some((_, n, _)) => *n += 1,
            None => counts.push((page.format, 1, &page.name)),
        }
    }
    if counts.len() > 1 {
        let list: Vec<String> = counts.iter().map(|(f, n, first)| format!("{f} ×{n} (e.g. {first})")).collect();
        bail!(
            "chapter {}: webtoon fragments have different formats: {}.\n\
             Allow mixed formats (--mixed-formats) to keep them in one page",
            chapter.name,
            list.join(", ")
        );
    }
    Ok(())
}

fn add_chapter(builder: &mut Builder, chapter: Chapter, chapter_id: u16, flags: u16, webtoon: bool, warnings: &mut Vec<String>) -> Result<()> {
    let name = chapter.name.clone();
    let start_flags = flags | PageBitFlags::IS_CHAPTER_START;

    if webtoon {
        // A chapter is one long page: all its attachments belong to it.
        let sources: Vec<ChunkSource> = chapter.pages.into_iter().map(|p| p.source).collect();
        builder.add_webtoon(sources, chapter_id, 1, start_flags).with_context(|| format!("chapter {name}"))?;
        for (_, aux) in chapter.aux {
            builder.attach_aux(aux).with_context(|| format!("chapter {name}: attachment"))?;
        }
        return Ok(());
    }

    let roles = chapter.comic_info.as_ref().map(comicinfo::page_roles).unwrap_or_default();
    let mut aux = chapter.aux;
    for (i, page) in chapter.pages.into_iter().enumerate() {
        let role = roles.iter().find(|(idx, _)| *idx == i).map_or(PageRole::Normal, |(_, r)| *r);
        let page_number = u16::try_from(i + 1).context("too many pages in a chapter")?;
        let page_flags = if i == 0 { start_flags } else { flags };
        builder.add_page(page.source, role, chapter_id, page_number, page_flags).with_context(|| format!("{name}: {}", page.name))?;
        for (_, entry) in aux.extract_if(.., |(idx, _)| *idx == i) {
            builder.attach_aux(entry).with_context(|| format!("{name}: attachment of page {}", i + 1))?;
        }
    }
    for (idx, _) in aux {
        warnings.push(format!("{name}: attachment for a nonexistent page {} skipped", idx + 1));
    }
    Ok(())
}

/// Volume and number of each chapter.
///
/// Volume — found in the name / folder / ComicInfo, otherwise `default_volume`, otherwise the previous
/// chapter's volume (0 — no volume). Number — see [`contents::assign_chapter_ids`].
fn assign_ids(chapters: &[Chapter], default_volume: Option<u16>) -> Vec<(u16, u16)> {
    let mut volume = default_volume.unwrap_or(0);
    let volumes: Vec<u16> = chapters
        .iter()
        .map(|c| {
            volume = c.volume.or(default_volume).unwrap_or(volume);
            volume
        })
        .collect();
    let numbers: Vec<Option<String>> = chapters.iter().map(|c| c.number.clone()).collect();
    let ids = contents::assign_chapter_ids(&volumes, &numbers);
    volumes.into_iter().zip(ids).collect()
}

/// Volume and chapter titles: from folder names / ComicInfo, overridden by `contents.json` (after vnmfunpack).
fn chapters_contents(chapters: &[Chapter], ids: &[(u16, u16)]) -> Contents {
    let mut contents = Contents::default();
    for (ch, &(volume, chapter)) in chapters.iter().zip(ids) {
        contents.set_chapter(volume, chapter, ch.number.clone(), ch.title.clone());
        if ch.volume_title.is_some() {
            contents.set_volume_title(volume, ch.volume_title.clone());
        }
    }
    for saved in chapters.iter().filter_map(|c| c.contents.as_ref()) {
        contents.merge(saved);
    }
    contents
}

/// Info from the chapters: `vnmf.json`, otherwise ComicInfo, otherwise an info file — from the first chapter that has one.
///
/// For a multi-chapter file ComicInfo describes the whole series: single-chapter fields (`Title`,
/// `Number`, `Pages`…) are removed — chapter numbers and titles go into the table of contents.
fn chapters_info(chapters: &[Chapter], ids: &[(u16, u16)]) -> Option<InfoSource> {
    if let Some(meta) = chapters.iter().find_map(|c| c.metadata.clone()) {
        return Some(InfoSource::Meta(Box::new(meta)));
    }
    let first = chapters.iter().find(|c| c.comic_info.is_some() || c.info.is_some())?;
    Some(match (&first.comic_info, &first.info) {
        (Some(json), _) => {
            let mut json = json.clone();
            if chapters.len() > 1 {
                let one_volume = ids.windows(2).all(|w| w[0].0 == w[1].0);
                comicinfo::strip_chapter_fields(&mut json, one_volume);
            }
            InfoSource::Meta(Box::new(Metadata::from_comicinfo(&json)))
        }
        (None, Some(info)) => InfoSource::from_bytes(info.clone()),
        (None, None) => return None,
    })
}
