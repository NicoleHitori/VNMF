// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! VNMF metadata (`vnmf-metadata/1`) — our own JSON format for the Info section (file format 1.2).
//!
//! Not tied to ComicInfo: describes manga, manhwa, manhua, webtoons, Western
//! comics, BD, doujinshi, etc. equally well, together with the translation (team, translation type), the structure
//! (volumes and chapters) and catalog identifiers. All fields except `schema` are optional.
//! Full description — FORMAT.md, section 16.
//!
//! ComicInfo.xml remains the exchange format with CBZ: [`Metadata::from_comicinfo`] /
//! [`Metadata::to_comicinfo`]. ComicInfo fields without an equivalent are stored in
//! `extra.comicinfo` and returned on export. Format 1.1 Info
//! (`{"ComicInfo": …, "Contents": …}`) is read as before.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use vnmf_core::InfoFormat;

use crate::comicinfo;
use crate::contents::Contents;
use vnmf_i18n::{n_, tr, trf};

/// Schema identifier in the `schema` field.
pub const SCHEMA: &str = "vnmf-metadata/1";
/// Metadata file next to the pages (`vnmfunpack` / `vnmfpack`).
pub const FILE_NAME: &str = "vnmf.json";
/// Key in `extra` holding ComicInfo fields without an equivalent.
const COMICINFO_EXTRA: &str = "comicinfo";

// ---------------------------------------------------------------------------
// Value dictionaries: (value in JSON, display label)
// ---------------------------------------------------------------------------

/// `type` — what kind of work it is.
pub const TYPES: &[(&str, &str)] = &[
    ("manga", n_("Manga")),
    ("manhwa", n_("Manhwa")),
    ("manhua", n_("Manhua")),
    ("webtoon", n_("Webtoon")),
    ("webcomic", n_("Webcomic")),
    ("comic", n_("Comic")),
    ("graphic_novel", n_("Graphic novel")),
    ("bande_dessinee", n_("Bande dessinée (BD)")),
    ("doujinshi", n_("Doujinshi")),
    ("artbook", n_("Artbook")),
    ("other", n_("Other")),
];
/// `publication` — form of publication.
pub const PUBLICATIONS: &[(&str, &str)] =
    &[("serial", n_("Serial")), ("one_shot", n_("One-shot")), ("anthology", n_("Anthology")), ("yonkoma", n_("Yonkoma (4-koma)"))];
/// `status` — state of the series.
pub const STATUSES: &[(&str, &str)] = &[
    ("ongoing", n_("Ongoing")),
    ("completed", n_("Completed")),
    ("hiatus", n_("Hiatus")),
    ("cancelled", n_("Cancelled")),
    ("upcoming", n_("Upcoming")),
];
/// `demographic` — target audience (Japanese terms; the same for manhwa and manhua).
pub const DEMOGRAPHICS: &[(&str, &str)] = &[
    ("shounen", n_("Shōnen")),
    ("shoujo", n_("Shōjo")),
    ("seinen", n_("Seinen")),
    ("josei", n_("Josei")),
    ("kodomo", n_("Kodomo")),
];
/// `age_rating`.
pub const AGE_RATINGS: &[(&str, &str)] =
    &[("all", n_("All ages")), ("10+", "10+"), ("13+", "13+"), ("16+", "16+"), ("18+", "18+")];
/// `reading_direction`.
pub const DIRECTIONS: &[(&str, &str)] =
    &[("rtl", n_("Right to left")), ("ltr", n_("Left to right")), ("ttb", n_("Top to bottom"))];
/// `layout` — how it is meant to be read.
pub const LAYOUTS: &[(&str, &str)] = &[("paged", n_("Page by page")), ("double", n_("In spreads")), ("strip", n_("As a strip"))];
/// `color`.
pub const COLORS: &[(&str, &str)] = &[("bw", n_("Black and white")), ("color", n_("Full color")), ("mixed", n_("Partly color"))];
/// `translation.type`.
pub const TRANSLATION_TYPES: &[(&str, &str)] =
    &[("official", n_("Official")), ("fan", n_("Fan")), ("machine", n_("Machine"))];
/// `credits[].roles`.
pub const ROLES: &[(&str, &str)] = &[
    ("author", n_("Author")),
    ("story", n_("Writer")),
    ("art", n_("Artist")),
    ("original_creator", n_("Original creator")),
    ("penciller", n_("Penciller")),
    ("inker", n_("Inker")),
    ("colorist", n_("Colorist")),
    ("letterer", n_("Letterer")),
    ("cover_artist", n_("Cover artist")),
    ("assistant", n_("Assistant")),
    ("editor", n_("Editor")),
    ("translator", n_("Translator")),
    ("proofreader", n_("Proofreader")),
    ("typesetter", n_("Typesetter")),
    ("cleaner", n_("Cleaner")),
    ("redrawer", n_("Redrawer")),
    ("quality_checker", n_("Quality checker")),
    ("raw_provider", n_("Raw provider")),
];
/// Known `identifiers` keys (others are allowed too).
pub const IDENTIFIERS: &[(&str, &str)] = &[
    ("anilist", "AniList"),
    ("mal", "MyAnimeList"),
    ("mangadex", "MangaDex"),
    ("mangaupdates", "MangaUpdates"),
    ("kitsu", "Kitsu"),
    ("shikimori", "Shikimori"),
    ("comicvine", "Comic Vine"),
    ("webtoons", "WEBTOON"),
    ("naver", "Naver Webtoon"),
    ("kakao", "KakaoPage"),
    ("bilibili", "Bilibili Comics"),
    ("kuaikan", "Kuaikan"),
    ("tapas", "Tapas"),
];

/// Label of a dictionary value (an unknown value — as is).
pub fn label(dict: &[(&str, &'static str)], value: &str) -> String {
    dict.iter().find(|(v, _)| *v == value).map_or_else(|| value.to_string(), |(_, l)| tr(l).to_string())
}

// ---------------------------------------------------------------------------
// Structure
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AltTitle {
    pub title: String,
    /// Title language (BCP 47: `en`, `ja-Latn`, `ko`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Credit {
    pub name: String,
    /// Roles from [`ROLES`] (others are allowed too).
    pub roles: Vec<String>,
    /// Name in the original language (尾田栄一郎).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Translation {
    /// [`TRANSLATION_TYPES`].
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Translation / scanlation teams.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Translation {
    fn is_empty(&self) -> bool {
        self.kind.is_none() && self.groups.is_empty() && self.notes.is_none()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Link {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// VNMF metadata. Empty fields are not serialized; unknown keys (from newer schema versions)
/// are kept in `unknown` and written back.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    pub schema: String,

    // --- The work ---
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub romanized_title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternative_titles: Vec<AltTitle>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demographic: Option<String>,
    /// Original language (BCP 47: `ja`, `ko`, `zh-Hans`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_language: Option<String>,
    /// Country of origin (ISO 3166-1: `JP`, `KR`, `CN`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub characters: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_rating: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub content_warnings: Vec<String>,
    /// Series dates: `YYYY`, `YYYY-MM` or `YYYY-MM-DD`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_date: Option<String>,
    /// Total volumes / chapters in the series (not in this file).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapter_count: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub credits: Vec<Credit>,
    /// Magazine or platform of first publication (Weekly Shōnen Jump, Naver Webtoon, KakaoPage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serialization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_publisher: Option<String>,

    // --- This edition ---
    /// Language of the text in this file (BCP 47).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imprint: Option<String>,
    /// Edition: "Deluxe", "Collector's", "Digital".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isbn: Option<String>,
    #[serde(skip_serializing_if = "Translation::is_empty")]
    pub translation: Translation,

    // --- Reading ---
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading_direction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,

    // --- Catalogs ---
    /// Catalog identifiers: `{"anilist": "30013", "mangadex": "…"}` (see [`IDENTIFIERS`]).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub identifiers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,

    // --- Structure and the rest ---
    #[serde(skip_serializing_if = "Contents::is_empty")]
    pub contents: Contents,
    /// Application data and fields without an equivalent (`extra.comicinfo` — from ComicInfo).
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
    /// Unknown keys (from newer schema versions) — kept as is.
    #[serde(flatten)]
    pub unknown: Map<String, Value>,
}

fn text(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// "A, B; C" → ["A", "B", "C"].
pub fn split_list(s: &str) -> Vec<String> {
    s.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

impl Metadata {
    pub fn new() -> Self {
        Self { schema: SCHEMA.to_string(), ..Default::default() }
    }

    /// Empty — only the schema (not worth writing).
    pub fn is_empty(&self) -> bool {
        let mut probe = self.clone();
        probe.schema = SCHEMA.into();
        probe.contents.prune();
        probe == Self::new()
    }

    /// JSON for the Info section.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut m = self.clone();
        m.schema = SCHEMA.into();
        m.contents.prune();
        serde_json::to_vec_pretty(&m).unwrap_or_default()
    }

    /// Metadata from any JSON that contains it: `vnmf-metadata`, format 1.1 Info
    /// (`ComicInfo` + `Contents`) or ComicInfo converted to JSON. Otherwise — `None`.
    pub fn from_json(value: &Value) -> Option<Self> {
        let schema = value.get("schema").and_then(Value::as_str);
        if schema.is_some_and(|s| s.starts_with("vnmf-metadata/")) {
            return serde_json::from_value(value.clone()).ok();
        }
        if comicinfo::is_comicinfo(value) {
            let mut m = Self::from_comicinfo(value);
            if let Some(legacy) = value.get("Contents") {
                m.contents.merge(&Contents::from_legacy_json(legacy));
            }
            return Some(m);
        }
        None
    }

    /// Metadata from the Info section: JSON (see [`Self::from_json`]) or ComicInfo.xml.
    pub fn from_info(info: Option<&[u8]>) -> Option<Self> {
        let info = info?;
        match InfoFormat::detect(info) {
            InfoFormat::Json => Self::from_json(&serde_json::from_slice(info).ok()?),
            InfoFormat::Xml => {
                let json = comicinfo::xml_to_json(std::str::from_utf8(info).ok()?).ok()?;
                comicinfo::is_comicinfo(&json).then(|| Self::from_comicinfo(&json))
            }
            _ => None,
        }
    }

    pub fn is_rtl(&self) -> bool {
        self.reading_direction.as_deref() == Some("rtl")
    }

    /// Names with role `role`.
    pub fn credited(&self, role: &str) -> Vec<String> {
        self.credits.iter().filter(|c| c.roles.iter().any(|r| r == role)).map(|c| c.name.clone()).collect()
    }

    /// Sets the people with role `role`: the role is removed from everyone, then added to the given ones
    /// (a person with several roles stays a single entry).
    pub fn set_credited(&mut self, role: &str, names: &[String]) {
        for c in &mut self.credits {
            c.roles.retain(|r| r != role);
        }
        for name in names {
            match self.credits.iter_mut().find(|c| c.name == *name) {
                Some(c) => c.roles.push(role.to_string()),
                None => self.credits.push(Credit { name: name.clone(), roles: vec![role.to_string()], original_name: None }),
            }
        }
        self.credits.retain(|c| !c.roles.is_empty());
    }

    fn add_credit(&mut self, names: &str, role: &str) {
        for name in split_list(names) {
            match self.credits.iter_mut().find(|c| c.name == name) {
                Some(c) if !c.roles.iter().any(|r| r == role) => c.roles.push(role.into()),
                Some(_) => {}
                None => self.credits.push(Credit { name, roles: vec![role.into()], original_name: None }),
            }
        }
    }

    // -----------------------------------------------------------------------
    // ComicInfo
    // -----------------------------------------------------------------------

    /// From ComicInfo (the `crate::comicinfo` JSON convention, `{"ComicInfo": {…}}` or flat).
    ///
    /// Single-chapter fields (`Number`, `Volume`, `Pages`, `PageCount`, and `Title` when
    /// `Series` is present) describe the structure — they're parsed by whoever knows the chapters (`input`, `vnmfpack`).
    pub fn from_comicinfo(value: &Value) -> Self {
        let root = value.get(comicinfo::ROOT).unwrap_or(value);
        let mut m = Self::new();
        let Some(fields) = root.as_object() else { return m };
        let get = |k: &str| text(fields.get(k).and_then(Value::as_str));
        let mut leftover = Map::new();

        m.title = get("Series").or_else(|| get("Title"));
        for (key, value) in fields {
            let Some(s) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                if !matches!(key.as_str(), "Pages") {
                    leftover.insert(key.clone(), value.clone());
                }
                continue;
            };
            match key.as_str() {
                "Series" | "Year" | "Month" | "Day" | "Number" | "Volume" | "PageCount" => {}
                "Title" if get("Series").is_none() => {}
                "LocalizedSeries" => m.alternative_titles.push(AltTitle { title: s.into(), language: None }),
                "Summary" => m.summary = Some(s.into()),
                "Writer" => m.add_credit(s, "story"),
                "Penciller" => m.add_credit(s, "art"),
                "Inker" => m.add_credit(s, "inker"),
                "Colorist" => m.add_credit(s, "colorist"),
                "Letterer" => m.add_credit(s, "letterer"),
                "CoverArtist" => m.add_credit(s, "cover_artist"),
                "Editor" => m.add_credit(s, "editor"),
                "Translator" => m.add_credit(s, "translator"),
                "Publisher" => m.publisher = Some(s.into()),
                "Imprint" => m.imprint = Some(s.into()),
                "Genre" => m.genres = split_list(s),
                "Tags" => m.tags = split_list(s),
                "Characters" => m.characters = split_list(s),
                "Web" => m.links = s.split_whitespace().map(|u| Link { url: u.into(), title: None }).collect(),
                "LanguageISO" => m.language = Some(s.into()),
                "GTIN" => m.isbn = Some(s.into()),
                "ScanInformation" => m.translation.notes = Some(s.into()),
                "BlackAndWhite" => match s {
                    "Yes" => m.color = Some("bw".into()),
                    "No" => m.color = Some("color".into()),
                    _ => {}
                },
                "Manga" => match s {
                    "YesAndRightToLeft" => {
                        m.kind = Some("manga".into());
                        m.reading_direction = Some("rtl".into());
                    }
                    "Yes" => m.kind = Some("manga".into()),
                    _ => {}
                },
                "AgeRating" => m.age_rating = age_from_comicinfo(s).map(str::to_owned),
                // No equivalent — keep it for the reverse export.
                _ => {
                    leftover.insert(key.clone(), value.clone());
                }
            }
        }
        let date: Vec<String> = ["Year", "Month", "Day"]
            .iter()
            .map_while(|k| get(k).and_then(|v| v.parse::<u16>().ok()))
            .enumerate()
            .map(|(i, n)| if i == 0 { format!("{n:04}") } else { format!("{n:02}") })
            .collect();
        if !date.is_empty() {
            m.release_date = Some(date.join("-"));
        }
        // The chapter title (Title next to Series) — into the contents, if the integer number is known.
        if let (Some(_), Some(title)) = (get("Series"), get("Title")) {
            let volume = get("Volume").and_then(|v| v.parse().ok()).unwrap_or(0);
            match get("Number").and_then(|n| n.parse().ok()) {
                Some(chapter) => m.contents.set_chapter(volume, chapter, None, Some(title)),
                None => {
                    leftover.insert("Title".into(), Value::String(title));
                }
            }
        }
        if !leftover.is_empty() {
            m.extra.insert(COMICINFO_EXTRA.into(), Value::Object(leftover));
        }
        m
    }

    /// ComicInfo (`{"ComicInfo": {…}}`) for a CBZ. `chapter` — the volume, number and title if the file is
    /// a single chapter (then they become `Volume`, `Number`, `Title`). `Pages` is added by the caller.
    pub fn to_comicinfo(&self, chapter: Option<(u16, String, Option<String>)>) -> Value {
        let mut ci = Map::new();
        let mut put = |k: &str, v: Option<String>| {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                ci.insert(k.into(), Value::String(v));
            }
        };
        let names = |roles: &[&str]| {
            let list: Vec<String> = self
                .credits
                .iter()
                .filter(|c| c.roles.iter().any(|r| roles.contains(&r.as_str())))
                .map(|c| c.name.clone())
                .collect();
            (!list.is_empty()).then(|| list.join(", "))
        };
        let (volume, number, chapter_title) = match chapter {
            Some((v, n, t)) => (Some(v).filter(|v| *v != 0), Some(n), t),
            None => (None, None, None),
        };
        put("Title", chapter_title.or_else(|| self.title.clone()));
        put("Series", self.title.clone());
        put("Number", number);
        put("Count", self.chapter_count.map(|n| n.to_string()));
        put("Volume", volume.map(|v| v.to_string()));
        put("LocalizedSeries", self.alternative_titles.first().map(|a| a.title.clone()));
        put("Summary", self.summary.clone());
        if let Some(date) = &self.release_date {
            let mut parts = date.split('-');
            put("Year", parts.next().map(str::to_owned));
            put("Month", parts.next().and_then(|m| m.parse::<u8>().ok()).map(|m| m.to_string()));
            put("Day", parts.next().and_then(|d| d.parse::<u8>().ok()).map(|d| d.to_string()));
        }
        put("Writer", names(&["author", "story", "original_creator"]));
        put("Penciller", names(&["author", "art", "penciller"]));
        put("Inker", names(&["inker"]));
        put("Colorist", names(&["colorist"]));
        put("Letterer", names(&["letterer"]));
        put("CoverArtist", names(&["cover_artist"]));
        put("Editor", names(&["editor"]));
        put("Translator", names(&["translator"]));
        put("Publisher", self.publisher.clone());
        put("Imprint", self.imprint.clone());
        put("Genre", (!self.genres.is_empty()).then(|| self.genres.join(", ")));
        put("Tags", (!self.tags.is_empty()).then(|| self.tags.join(", ")));
        put("Web", (!self.links.is_empty()).then(|| self.links.iter().map(|l| l.url.as_str()).collect::<Vec<_>>().join(" ")));
        put("LanguageISO", self.language.clone());
        put("BlackAndWhite", self.color.as_deref().map(|c| if c == "bw" { "Yes" } else { "No" }.to_string()));
        let manga = if self.is_rtl() {
            Some("YesAndRightToLeft")
        } else if matches!(self.kind.as_deref(), Some("manga" | "manhwa" | "manhua")) {
            Some("Yes")
        } else {
            None
        };
        put("Manga", manga.map(str::to_owned));
        put("Characters", (!self.characters.is_empty()).then(|| self.characters.join(", ")));
        let scan = match (&self.translation.notes, self.translation.groups.is_empty()) {
            (Some(n), _) => Some(n.clone()),
            (None, false) => Some(self.translation.groups.join(", ")),
            _ => None,
        };
        put("ScanInformation", scan);
        put("GTIN", self.isbn.clone());
        put("AgeRating", self.age_rating.as_deref().and_then(age_to_comicinfo).map(str::to_owned));
        // Fields that came from ComicInfo without an equivalent — back.
        if let Some(Value::Object(saved)) = self.extra.get(COMICINFO_EXTRA) {
            for (k, v) in saved {
                ci.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        json!({ comicinfo::ROOT: ci })
    }

    // -----------------------------------------------------------------------
    // Display
    // -----------------------------------------------------------------------

    /// Fields for display (label, value) in a sensible order; empty ones are skipped.
    pub fn display_rows(&self) -> Vec<(&'static str, String)> {
        let mut rows: Vec<(&'static str, String)> = Vec::new();
        let mut add = |label: &'static str, value: Option<String>| {
            if let Some(v) = value.filter(|v| !v.is_empty()) {
                rows.push((label, v));
            }
        };
        let list = |v: &[String]| (!v.is_empty()).then(|| v.join(", "));
        let lbl = |dict, v: &Option<String>| v.as_deref().map(|v| label(dict, v));

        add(tr("Title"), self.title.clone());
        add(tr("Original title"), self.original_title.clone());
        add(tr("Romanized"), self.romanized_title.clone());
        add(
            tr("Other titles"),
            list(
                &self
                    .alternative_titles
                    .iter()
                    .map(|a| match &a.language {
                        Some(l) => format!("{} ({l})", a.title),
                        None => a.title.clone(),
                    })
                    .collect::<Vec<_>>(),
            ),
        );
        add(tr("Type"), lbl(TYPES, &self.kind));
        add(tr("Publication"), lbl(PUBLICATIONS, &self.publication));
        add(tr("Status"), lbl(STATUSES, &self.status));
        add(tr("Audience"), lbl(DEMOGRAPHICS, &self.demographic));
        add(
            tr("Origin"),
            match (&self.original_language, &self.origin_country) {
                (Some(l), Some(c)) => Some(format!("{l}, {c}")),
                (l, c) => l.clone().or(c.clone()),
            },
        );
        for c in &self.credits {
            let roles: Vec<String> = c.roles.iter().map(|r| label(ROLES, r)).collect();
            let name = match &c.original_name {
                Some(o) => format!("{} ({o})", c.name),
                None => c.name.clone(),
            };
            rows.push((tr("Authors and team"), format!("{name} — {}", roles.join(", "))));
        }
        let mut add = |label: &'static str, value: Option<String>| {
            if let Some(v) = value.filter(|v| !v.is_empty()) {
                rows.push((label, v));
            }
        };
        add(tr("Serialized in"), self.serialization.clone());
        add(tr("Original publisher"), self.original_publisher.clone());
        add(
            tr("Years"),
            match (&self.start_date, &self.end_date) {
                (Some(s), Some(e)) => Some(format!("{s} — {e}")),
                (Some(s), None) => Some(trf!("since {0}", s)),
                (None, e) => e.clone(),
            },
        );
        add(
            tr("In the series"),
            match (self.volume_count, self.chapter_count) {
                (Some(v), Some(c)) => Some(trf!("volumes {0}, chapters {1}", v, c)),
                (Some(v), None) => Some(trf!("volumes {0}", v)),
                (None, Some(c)) => Some(trf!("chapters {0}", c)),
                _ => None,
            },
        );
        add(tr("Language"), self.language.clone());
        let t = &self.translation;
        add(
            tr("Translation"),
            (!t.is_empty()).then(|| {
                let mut parts = Vec::new();
                if let Some(k) = &t.kind {
                    parts.push(label(TRANSLATION_TYPES, k));
                }
                if !t.groups.is_empty() {
                    parts.push(t.groups.join(", "));
                }
                if let Some(n) = &t.notes {
                    parts.push(n.clone());
                }
                parts.join(" · ")
            }),
        );
        add(tr("Publisher"), self.publisher.clone());
        add(tr("Imprint"), self.imprint.clone());
        add(tr("Edition"), self.edition.clone());
        add(tr("Release date"), self.release_date.clone());
        add("ISBN", self.isbn.clone());
        add(tr("Genres"), list(&self.genres));
        add(tr("Tags"), list(&self.tags));
        add(tr("Characters"), list(&self.characters));
        add(tr("Age rating"), lbl(AGE_RATINGS, &self.age_rating));
        add(tr("Content warnings"), list(&self.content_warnings));
        add(tr("Reading direction"), lbl(DIRECTIONS, &self.reading_direction));
        add(tr("How to read"), lbl(LAYOUTS, &self.layout));
        add(tr("Color"), lbl(COLORS, &self.color));
        for (key, id) in &self.identifiers {
            let name = IDENTIFIERS.iter().find(|(k, _)| k == key).map_or(key.as_str(), |(_, l)| *l);
            rows.push((tr("Catalogs"), format!("{name}: {id}")));
        }
        for link in &self.links {
            rows.push((tr("Links"), link.title.as_ref().map_or_else(|| link.url.clone(), |t| format!("{t}: {}", link.url))));
        }
        if let Some(s) = &self.summary {
            rows.push((tr("Description"), s.clone()));
        }
        rows
    }
}

/// ComicInfo age ratings → the VNMF scale.
fn age_from_comicinfo(s: &str) -> Option<&'static str> {
    Some(match s {
        "Everyone" | "G" | "Early Childhood" | "Kids to Adults" => "all",
        "Everyone 10+" => "10+",
        "Teen" | "PG" => "13+",
        "Mature 17+" | "MA15+" | "M" => "16+",
        "Adults Only 18+" | "R18+" | "X18+" => "18+",
        _ => return None,
    })
}

fn age_to_comicinfo(s: &str) -> Option<&'static str> {
    Some(match s {
        "all" => "Everyone",
        "10+" => "Everyone 10+",
        "13+" => "Teen",
        "16+" => "Mature 17+",
        "18+" => "Adults Only 18+",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comicinfo() -> Value {
        crate::comicinfo::xml_to_json(
            r#"<ComicInfo xmlns:xsd="http://www.w3.org/2001/XMLSchema">
                 <Title>First meeting</Title><Series>Witches</Series><Number>3</Number><Volume>1</Volume>
                 <Summary>Description</Summary><Year>2024</Year><Month>5</Month>
                 <Writer>Anna, Bob</Writer><Penciller>Anna</Penciller><Translator>Vera</Translator>
                 <Genre>Fantasy, Comedy</Genre><Manga>YesAndRightToLeft</Manga><AgeRating>Teen</AgeRating>
                 <LanguageISO>uk</LanguageISO><Web>https://a.example https://b.example</Web>
                 <StoryArc>Arc</StoryArc><Notes>note</Notes>
                 <Pages><Page Image="0" Type="FrontCover"/></Pages>
               </ComicInfo>"#,
        )
        .unwrap()
    }

    #[test]
    fn comicinfo_maps_to_own_fields() {
        let m = Metadata::from_comicinfo(&comicinfo());
        assert_eq!(m.title.as_deref(), Some("Witches"));
        assert_eq!(m.kind.as_deref(), Some("manga"));
        assert!(m.is_rtl());
        assert_eq!(m.age_rating.as_deref(), Some("13+"));
        assert_eq!(m.release_date.as_deref(), Some("2024-05"));
        assert_eq!(m.genres, vec!["Fantasy", "Comedy"]);
        assert_eq!(m.links.len(), 2);
        // Anna is one person with two roles.
        assert_eq!(m.credits[0], Credit { name: "Anna".into(), roles: vec!["story".into(), "art".into()], original_name: None });
        assert_eq!(m.credited("translator"), vec!["Vera"]);
        // The chapter title — in the contents by (volume, number); without an equivalent — in extra.comicinfo.
        assert_eq!(m.contents.chapter(1, 3).unwrap().title.as_deref(), Some("First meeting"));
        let extra = &m.extra[COMICINFO_EXTRA];
        assert_eq!(extra["StoryArc"], "Arc");
        assert!(extra.get("Pages").is_none() && extra.get("Number").is_none());
    }

    #[test]
    fn json_roundtrip_is_lossless_and_back_to_comicinfo() {
        let m = Metadata::from_comicinfo(&comicinfo());
        let bytes = m.to_bytes();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["schema"], SCHEMA);
        assert!(v.get("ComicInfo").is_none());
        assert_eq!(Metadata::from_info(Some(&bytes)).unwrap(), Metadata::from_json(&v).unwrap());

        let back = m.to_comicinfo(Some((1, "3".into(), Some("First meeting".into()))));
        let ci = &back[comicinfo::ROOT];
        assert_eq!(ci["Series"], "Witches");
        assert_eq!(ci["Title"], "First meeting");
        assert_eq!(ci["Writer"], "Anna, Bob");
        assert_eq!(ci["Penciller"], "Anna");
        assert_eq!(ci["Manga"], "YesAndRightToLeft");
        assert_eq!(ci["AgeRating"], "Teen");
        assert_eq!(ci["Month"], "5");
        assert_eq!(ci["StoryArc"], "Arc", "fields without an equivalent are returned");
    }

    #[test]
    fn unknown_keys_survive_and_legacy_info_is_read() {
        let v = json!({"schema": "vnmf-metadata/1", "title": "T", "future_field": {"x": 1}});
        let m = Metadata::from_json(&v).unwrap();
        let out: Value = serde_json::from_slice(&m.to_bytes()).unwrap();
        assert_eq!(out["future_field"]["x"], 1);

        let legacy = json!({"ComicInfo": {"Series": "S"}, "Contents": {"Volumes": [{"Volume": 2, "Title": "Storm"}]}});
        let m = Metadata::from_json(&legacy).unwrap();
        assert_eq!(m.title.as_deref(), Some("S"));
        assert_eq!(m.contents.volume(2).unwrap().title.as_deref(), Some("Storm"));
        assert!(Metadata::from_json(&json!({"random": 1})).is_none());
    }

    #[test]
    fn credits_are_editable_by_role() {
        let mut m = Metadata::new();
        m.set_credited("story", &["Anna".into()]);
        m.set_credited("art", &["Anna".into(), "Bob".into()]);
        m.set_credited("story", &[]);
        assert_eq!(m.credits.len(), 2);
        assert_eq!(m.credited("art"), vec!["Anna", "Bob"]);
        assert!(m.credited("story").is_empty());
        assert!(Metadata::new().is_empty());
    }
}
