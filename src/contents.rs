// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! File table of contents: volume and chapter titles.
//!
//! Which pages belong to which volume and chapter is determined by the page table
//! (`vnmf_core::structure`). Titles, "human" chapter numbers ("10.5") and dates are stored in
//! the VNMF metadata (`crate::metadata`) in the `contents` field:
//!
//! ```json
//! "contents": {
//!   "volumes": [
//!     { "volume": 1, "title": "Beginning",
//!       "chapters": [ { "chapter": 1, "number": "1", "title": "First meeting" } ] }
//!   ]
//! }
//! ```
//!
//! `volume` / `chapter` are the `volume_id` / `chapter_id` from the pages; all other fields are optional.

use std::ops::Range;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use vnmf_core::VnmfReader;

use crate::metadata::Metadata;
use vnmf_i18n::{tr, trf};

/// Format 1.1 table of contents file (from the previous version of `vnmfunpack`) — read-only.
pub const LEGACY_FILE_NAME: &str = "contents.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChapterMeta {
    pub chapter: u16,
    /// Number for display if it differs from `chapter` ("10.5", "Prologue").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Chapter release date (`YYYY`, `YYYY-MM` or `YYYY-MM-DD`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VolumeMeta {
    pub volume: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isbn: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<ChapterMeta>,
}

/// Volume and chapter titles.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Contents {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<VolumeMeta>,
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl Contents {
    pub fn is_empty(&self) -> bool {
        self.volumes.iter().all(|v| {
            v.title.is_none() && v.date.is_none() && v.isbn.is_none() && v.chapters.iter().all(|c| c.number.is_none() && c.title.is_none() && c.date.is_none())
        })
    }

    /// Format 1.1 table of contents: `{"Volumes": [{"Volume", "Title", "Chapters": [{"Chapter", "Number", "Title"}]}]}`.
    pub fn from_legacy_json(value: &Value) -> Self {
        fn text(v: &Value, key: &str) -> Option<String> {
            match v.get(key)? {
                Value::String(s) => non_empty(Some(s.clone())),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            }
        }
        fn id(v: &Value, key: &str) -> Option<u16> {
            match v.get(key)? {
                Value::Number(n) => n.as_u64().and_then(|n| u16::try_from(n).ok()),
                Value::String(s) => s.trim().parse().ok(),
                _ => None,
            }
        }
        let volumes = value
            .get("Volumes")
            .and_then(Value::as_array)
            .map(|vols| {
                vols.iter()
                    .filter_map(|v| {
                        Some(VolumeMeta {
                            volume: id(v, "Volume")?,
                            title: text(v, "Title"),
                            chapters: v
                                .get("Chapters")
                                .and_then(Value::as_array)
                                .map(|chs| {
                                    chs.iter()
                                        .filter_map(|c| {
                                            Some(ChapterMeta { chapter: id(c, "Chapter")?, number: text(c, "Number"), title: text(c, "Title"), date: None })
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                            ..Default::default()
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { volumes }
    }

    pub fn volume(&self, volume: u16) -> Option<&VolumeMeta> {
        self.volumes.iter().find(|v| v.volume == volume)
    }

    pub fn chapter(&self, volume: u16, chapter: u16) -> Option<&ChapterMeta> {
        self.volume(volume)?.chapters.iter().find(|c| c.chapter == chapter)
    }

    pub fn volume_mut(&mut self, volume: u16) -> &mut VolumeMeta {
        match self.volumes.iter().position(|v| v.volume == volume) {
            Some(i) => &mut self.volumes[i],
            None => {
                self.volumes.push(VolumeMeta { volume, ..Default::default() });
                self.volumes.last_mut().unwrap()
            }
        }
    }

    pub fn set_volume_title(&mut self, volume: u16, title: Option<String>) {
        self.volume_mut(volume).title = non_empty(title);
    }

    /// Sets a chapter's number and title (empty values don't overwrite existing ones).
    pub fn set_chapter(&mut self, volume: u16, chapter: u16, number: Option<String>, title: Option<String>) {
        let entry = self.chapter_mut(volume, chapter);
        // A number equal to chapter_id is not stored — it's already known.
        if let Some(n) = non_empty(number).filter(|n| *n != chapter.to_string()) {
            entry.number = Some(n);
        }
        if let Some(t) = non_empty(title) {
            entry.title = Some(t);
        }
    }

    pub fn chapter_mut(&mut self, volume: u16, chapter: u16) -> &mut ChapterMeta {
        let vol = self.volume_mut(volume);
        match vol.chapters.iter().position(|c| c.chapter == chapter) {
            Some(i) => &mut vol.chapters[i],
            None => {
                vol.chapters.push(ChapterMeta { chapter, ..Default::default() });
                vol.chapters.last_mut().unwrap()
            }
        }
    }

    /// Merges another table of contents into this one: non-empty values from `other` take precedence.
    pub fn merge(&mut self, other: &Contents) {
        for v in &other.volumes {
            let vol = self.volume_mut(v.volume);
            for (dst, src) in [(&mut vol.title, &v.title), (&mut vol.date, &v.date), (&mut vol.isbn, &v.isbn)] {
                if src.is_some() {
                    dst.clone_from(src);
                }
            }
            for c in &v.chapters {
                self.set_chapter(v.volume, c.chapter, c.number.clone(), c.title.clone());
                if c.date.is_some() {
                    self.chapter_mut(v.volume, c.chapter).date.clone_from(&c.date);
                }
            }
        }
    }

    /// Removes entries with no data at all.
    pub fn prune(&mut self) {
        for v in &mut self.volumes {
            v.chapters.retain(|c| c.number.is_some() || c.title.is_some() || c.date.is_some());
        }
        self.volumes.retain(|v| v.title.is_some() || v.date.is_some() || v.isbn.is_some() || !v.chapters.is_empty());
    }
}

/// Chapter numbers (`chapter_id`) for a sequence of chapters.
///
/// `volumes[i]` is the volume of chapter `i`, `numbers[i]` is its number as a string ("5", "10.5") or `None`.
/// If all chapters have integer numbers that don't repeat within a volume, they become
/// the `chapter_id`; otherwise ordinals 1, 2, 3… are used (the real numbers then go into the contents).
pub fn assign_chapter_ids(volumes: &[u16], numbers: &[Option<String>]) -> Vec<u16> {
    let parsed: Option<Vec<u16>> = numbers.iter().map(|n| n.as_deref()?.trim().parse().ok()).collect();
    if let Some(ids) = parsed {
        let mut pairs: Vec<(u16, u16)> = volumes.iter().copied().zip(ids.iter().copied()).collect();
        pairs.sort_unstable();
        if pairs.windows(2).all(|w| w[0] != w[1]) {
            return ids;
        }
    }
    (1..=numbers.len()).map(|i| u16::try_from(i).unwrap_or(u16::MAX)).collect()
}

// ---------------------------------------------------------------------------
// Contents for display: structure from the pages + titles from the metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TocChapter {
    pub volume: u16,
    pub chapter: u16,
    pub number: Option<String>,
    pub title: Option<String>,
    pub pages: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct TocVolume {
    pub volume: u16,
    pub title: Option<String>,
    pub pages: Range<usize>,
    pub chapters: Vec<TocChapter>,
}

/// The full table of contents of a file.
#[derive(Debug, Clone, Default)]
pub struct Toc {
    pub volumes: Vec<TocVolume>,
    /// Whether the file has volumes (otherwise only chapters are shown).
    pub has_volumes: bool,
}

impl TocVolume {
    /// "Volume 2 — Title" / "Volume 2" / "No volume".
    pub fn label(&self) -> String {
        let base = if self.volume == 0 { tr("No volume").to_string() } else { trf!("Volume {0}", self.volume) };
        match &self.title {
            Some(t) => format!("{base} — {t}"),
            None => base,
        }
    }
}

impl TocChapter {
    /// Number for display: the one set in the contents, or `chapter_id`.
    pub fn display_number(&self) -> String {
        self.number.clone().unwrap_or_else(|| self.chapter.to_string())
    }

    /// "Chapter 10.5 — Title" / "Chapter 3".
    pub fn label(&self) -> String {
        let base = trf!("Chapter {0}", self.display_number());
        match &self.title {
            Some(t) => format!("{base} — {t}"),
            None => base,
        }
    }
}

impl Toc {
    pub fn from_reader(reader: &VnmfReader) -> Self {
        let contents = Metadata::from_info(reader.info_bytes()).map(|m| m.contents).unwrap_or_default();
        let volumes = reader
            .structure()
            .into_iter()
            .map(|v| TocVolume {
                volume: v.volume_id,
                title: contents.volume(v.volume_id).and_then(|m| m.title.clone()),
                pages: v.pages,
                chapters: v
                    .chapters
                    .into_iter()
                    .map(|c| {
                        let meta = contents.chapter(c.volume_id, c.chapter_id);
                        TocChapter {
                            volume: c.volume_id,
                            chapter: c.chapter_id,
                            number: meta.and_then(|m| m.number.clone()),
                            title: meta.and_then(|m| m.title.clone()),
                            pages: c.pages,
                        }
                    })
                    .collect(),
            })
            .collect();
        Self { volumes, has_volumes: reader.has_volumes() }
    }

    pub fn chapters(&self) -> impl Iterator<Item = &TocChapter> {
        self.volumes.iter().flat_map(|v| v.chapters.iter())
    }

    pub fn chapters_count(&self) -> usize {
        self.chapters().count()
    }

    /// Whether the structure is worth showing: several chapters or there are volumes.
    pub fn is_structured(&self) -> bool {
        self.has_volumes || self.chapters_count() > 1
    }

    /// Volume and chapter the page belongs to.
    pub fn locate(&self, page: usize) -> Option<(&TocVolume, &TocChapter)> {
        let v = self.volumes.iter().find(|v| v.pages.contains(&page))?;
        let c = v.chapters.iter().find(|c| c.pages.contains(&page))?;
        Some((v, c))
    }

    /// Position label: "Volume 1 · Chapter 3 — Title" (without the volume if there are no volumes).
    pub fn position_label(&self, page: usize) -> Option<String> {
        let (v, c) = self.locate(page)?;
        Some(if self.has_volumes { format!("{} · {}", short_volume(v), c.label()) } else { c.label() })
    }
}

fn short_volume(v: &TocVolume) -> String {
    if v.volume == 0 { tr("No volume").into() } else { trf!("Volume {0}", v.volume) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapter_ids_use_numbers_when_possible() {
        let some = |v: &[&str]| v.iter().map(|s| Some(s.to_string())).collect::<Vec<_>>();
        assert_eq!(assign_chapter_ids(&[1, 1, 2], &some(&["5", "6", "7"])), vec![5, 6, 7]);
        // The same number in different volumes is fine.
        assert_eq!(assign_chapter_ids(&[1, 2], &some(&["1", "1"])), vec![1, 1]);
        // Fractional, repeated within a volume, or missing number — ordinals.
        assert_eq!(assign_chapter_ids(&[0, 0], &some(&["10", "10.5"])), vec![1, 2]);
        assert_eq!(assign_chapter_ids(&[0, 0], &some(&["3", "3"])), vec![1, 2]);
        assert_eq!(assign_chapter_ids(&[0, 0], &[Some("3".into()), None]), vec![1, 2]);
    }

    #[test]
    fn serde_roundtrip_and_legacy() {
        let mut c = Contents::default();
        c.set_volume_title(1, Some("Beginning".into()));
        c.set_chapter(1, 1, Some("1".into()), Some("First meeting".into()));
        c.set_chapter(1, 2, Some("1.5".into()), None);
        c.set_chapter(2, 3, None, None); // no data — removed
        c.prune();
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["volumes"][0]["chapters"][0], serde_json::json!({"chapter": 1, "title": "First meeting"}));
        assert_eq!(json["volumes"].as_array().unwrap().len(), 1);
        let back: Contents = serde_json::from_value(json).unwrap();
        assert_eq!(back, c);

        let legacy = serde_json::json!({"Volumes": [{"Volume": 1, "Title": "V", "Chapters": [{"Chapter": "2", "Number": "1.5"}]}]});
        let old = Contents::from_legacy_json(&legacy);
        assert_eq!(old.volume(1).unwrap().title.as_deref(), Some("V"));
        assert_eq!(old.chapter(1, 2).unwrap().number.as_deref(), Some("1.5"));
    }
}
