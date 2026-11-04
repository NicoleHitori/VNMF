// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Shared by `vnmfunpack` and `vnmfrecovery`: how to save Info and images to files.

use anyhow::{Context, Result};
use vnmf_core::{InfoFormat, PageFormat, PageRole, VnmfReader};

use crate::comicinfo;
use crate::metadata::{self, Metadata};

/// File name and contents for the Info section — lossless.
///
/// VNMF metadata (and format 1.1 Info, which reduces to it) → `vnmf.json` in the current schema;
/// ComicInfo.xml — as is; anything else — `info.json` / `info.toml` / `info.txt`.
pub fn info_file(info: &[u8]) -> Result<(String, Vec<u8>)> {
    let format = InfoFormat::detect(info);
    if format == InfoFormat::Json
        && let Some(meta) = Metadata::from_info(Some(info))
    {
        return Ok((metadata::FILE_NAME.to_string(), meta.to_bytes()));
    }
    if format == InfoFormat::Xml && String::from_utf8_lossy(info).contains("<ComicInfo") {
        return Ok((comicinfo::FILE_NAME.to_string(), info.to_vec()));
    }
    Ok((format!("info.{}", format.extension()), info.to_vec()))
}

/// `ComicInfo.xml` for other readers (Komga, Kavita, Tachiyomi…), generated from the metadata:
/// page roles go into `<Pages>`, and for a single-chapter file — its volume, number and title.
/// `None` if Info is not VNMF metadata (then `info_file` has already produced the right file).
pub fn comicinfo_file(info: &[u8], reader: &VnmfReader) -> Result<Option<(String, Vec<u8>)>> {
    if InfoFormat::detect(info) != InfoFormat::Json {
        return Ok(None);
    }
    let Some(meta) = Metadata::from_info(Some(info)) else { return Ok(None) };
    let structure = reader.structure();
    let single = match structure.as_slice() {
        [v] if v.chapters.len() == 1 => {
            let c = &v.chapters[0];
            let entry = meta.contents.chapter(c.volume_id, c.chapter_id);
            let number = entry.and_then(|e| e.number.clone()).unwrap_or_else(|| c.chapter_id.to_string());
            Some((c.volume_id, number, entry.and_then(|e| e.title.clone())))
        }
        _ => None,
    };
    let mut value = meta.to_comicinfo(single);
    let pages: Vec<(PageRole, u32, u32)> = reader
        .pages()
        .iter()
        .map(|p| (PageRole::try_from(p.role).unwrap_or(PageRole::Normal), p.width.get(), p.height.get()))
        .collect();
    // Webtoon pages are fragments, they have no roles.
    if reader.pages().iter().all(|p| p.chunk_count.get() == 1)
        && let Some(root) = value.get_mut(comicinfo::ROOT).and_then(|v| v.as_object_mut())
    {
        root.insert("PageCount".into(), pages.len().to_string().into());
        root.insert("Pages".into(), comicinfo::pages_json(&pages));
    }
    let xml = comicinfo::json_to_xml(&value).context("converting metadata → ComicInfo.xml")?;
    Ok(Some((comicinfo::FILE_NAME.to_string(), xml.into_bytes())))
}

/// The extension is determined from the actual bytes; the format from the table is only a fallback.
pub fn image_extension(data: &[u8], recorded: PageFormat) -> &'static str {
    let detected = PageFormat::detect(data);
    if detected.is_known() { detected.extension() } else { recorded.extension() }
}
