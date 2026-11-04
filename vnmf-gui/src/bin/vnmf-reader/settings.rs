// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Reading settings: stored in `~/.config/vnmf/reader.json`.
//!
//! The file is read field by field: unknown or damaged values are replaced with defaults,
//! so an old or hand-edited config never breaks startup.

use std::path::PathBuf;

use gtk::glib;
use serde_json::{Map, Value, json};
use vnmf_i18n::tr;

/// How to fit a page in paged mode (before zoom).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    /// The page (or spread) is fully visible.
    Page,
    /// To the window width (100%), then scrolling.
    Width,
    /// Custom zoom `Settings::zoom` relative to the window width (after Ctrl+scroll, etc.).
    Custom,
}

/// Reading area background.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Background {
    Theme,
    Black,
    Gray,
    White,
}

impl Background {
    pub const ALL: [Background; 4] = [Background::Theme, Background::Black, Background::Gray, Background::White];

    pub fn label(self) -> &'static str {
        match self {
            Background::Theme => tr("Follow theme"),
            Background::Black => tr("Black"),
            Background::Gray => tr("Gray"),
            Background::White => tr("White"),
        }
    }

    pub fn css_class(self) -> Option<&'static str> {
        match self {
            Background::Theme => None,
            Background::Black => Some("reader-bg-black"),
            Background::Gray => Some("reader-bg-gray"),
            Background::White => Some("reader-bg-white"),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Background::Theme => "theme",
            Background::Black => "black",
            Background::Gray => "gray",
            Background::White => "white",
        }
    }
}

pub const ZOOM_MIN: f64 = 0.25;
pub const ZOOM_MAX: f64 = 8.0;
pub const STRIP_WIDTH_DEFAULT: i32 = 900;
pub const STRIP_WIDTH_MIN: i32 = 300;
pub const STRIP_WIDTH_MAX: i32 = 4000;

#[derive(Clone, Debug)]
pub struct Settings {
    pub fit: Fit,
    /// Paged mode zoom relative to the window width (1.0 = 100% = full width);
    /// applies when `fit == Fit::Custom`.
    pub zoom: f64,
    /// Webtoon strip width, px (strip zoom).
    pub strip_width: i32,
    /// Gap between strip fragments, px.
    pub strip_gap: i32,
    /// Two pages side by side (spread).
    pub double_page: bool,
    /// In a spread the first page (cover) is shown alone.
    pub cover_alone: bool,
    pub background: Background,
    /// A click in the left/right half of the reading area turns pages.
    pub click_zones: bool,
    /// The floating navigation bar hides when the mouse is still.
    pub autohide_bar: bool,
    /// In fullscreen the header appears only near the top edge.
    pub hide_header_fullscreen: bool,
    /// How many pages (spreads) to decode ahead of the current one and as many behind it — in both modes.
    pub preload: u32,
    /// Open a file at the page where reading stopped.
    pub remember_position: bool,
    /// Play page music and sound effects.
    pub sound: bool,
    /// Page sound volume, 0..=100 %.
    pub volume: i32,
    /// In a dark theme show dark versions of pages (if the file has them).
    pub dark_pages: bool,
    /// The "Page text" panel is open.
    pub show_text: bool,
    /// Window size (unmaximized) and whether it was maximized — it reopens the way it was closed.
    pub window_width: i32,
    pub window_height: i32,
    pub maximized: bool,
    /// File UUID → page number.
    pub positions: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fit: Fit::Page,
            zoom: 1.0,
            strip_width: STRIP_WIDTH_DEFAULT,
            strip_gap: 0,
            double_page: false,
            cover_alone: true,
            background: Background::Theme,
            click_zones: true,
            autohide_bar: true,
            hide_header_fullscreen: true,
            preload: 2,
            remember_position: true,
            sound: true,
            volume: 80,
            dark_pages: true,
            show_text: false,
            window_width: 1100,
            window_height: 900,
            maximized: false,
            positions: Map::new(),
        }
    }
}

fn path() -> PathBuf {
    glib::user_config_dir().join("vnmf").join("reader.json")
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Self::default();
        let Some(v) = std::fs::read(path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
            return s;
        };
        let get_bool = |k: &str, d: bool| v.get(k).and_then(Value::as_bool).unwrap_or(d);
        let get_i32 = |k: &str, d: i32| v.get(k).and_then(Value::as_i64).map_or(d, |n| n as i32);

        s.fit = match v.get("fit").and_then(Value::as_str) {
            Some("width") => Fit::Width,
            Some("custom") => Fit::Custom,
            _ => Fit::Page,
        };
        s.zoom = v.get("zoom").and_then(Value::as_f64).unwrap_or(1.0).clamp(ZOOM_MIN, ZOOM_MAX);
        s.strip_width = get_i32("strip_width", s.strip_width).clamp(STRIP_WIDTH_MIN, STRIP_WIDTH_MAX);
        s.strip_gap = get_i32("strip_gap", s.strip_gap).clamp(0, 64);
        s.double_page = get_bool("double_page", s.double_page);
        s.cover_alone = get_bool("cover_alone", s.cover_alone);
        s.background = Background::ALL.into_iter().find(|b| v.get("background").and_then(Value::as_str) == Some(b.key())).unwrap_or(Background::Theme);
        s.click_zones = get_bool("click_zones", s.click_zones);
        s.autohide_bar = get_bool("autohide_bar", s.autohide_bar);
        s.hide_header_fullscreen = get_bool("hide_header_fullscreen", s.hide_header_fullscreen);
        s.preload = get_i32("preload", s.preload as i32).clamp(0, 6) as u32;
        s.remember_position = get_bool("remember_position", s.remember_position);
        s.sound = get_bool("sound", s.sound);
        s.volume = get_i32("volume", s.volume).clamp(0, 100);
        s.dark_pages = get_bool("dark_pages", s.dark_pages);
        s.show_text = get_bool("show_text", s.show_text);
        s.window_width = get_i32("window_width", s.window_width).clamp(360, 16384);
        s.window_height = get_i32("window_height", s.window_height).clamp(300, 16384);
        s.maximized = get_bool("maximized", s.maximized);
        if let Some(map) = v.get("positions").and_then(Value::as_object) {
            s.positions = map.clone();
        }
        s
    }

    /// Atomic write (temporary file + rename). Errors are only logged:
    /// settings aren't worth interrupting reading for.
    pub fn save(&self) {
        let value = json!({
            "fit": match self.fit {
                Fit::Page => "page",
                Fit::Width => "width",
                Fit::Custom => "custom",
            },
            "zoom": self.zoom,
            "strip_width": self.strip_width,
            "strip_gap": self.strip_gap,
            "double_page": self.double_page,
            "cover_alone": self.cover_alone,
            "background": self.background.key(),
            "click_zones": self.click_zones,
            "autohide_bar": self.autohide_bar,
            "hide_header_fullscreen": self.hide_header_fullscreen,
            "preload": self.preload,
            "remember_position": self.remember_position,
            "sound": self.sound,
            "volume": self.volume,
            "dark_pages": self.dark_pages,
            "show_text": self.show_text,
            "window_width": self.window_width,
            "window_height": self.window_height,
            "maximized": self.maximized,
            "positions": self.positions,
        });
        let path = path();
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&value).unwrap_or_default())?;
            std::fs::rename(tmp, &path)
        })();
        if let Err(e) = result {
            eprintln!("vnmf-reader: could not save settings {}: {e}", path.display());
        }
    }

    /// Saved position. Old versions wrote only the page number — it is read as a position
    /// at the start of the page.
    pub fn position(&self, uuid: &str) -> Option<Position> {
        match self.positions.get(uuid)? {
            Value::Object(o) => Some(Position {
                page: o.get("page")?.as_u64()? as usize,
                chunk: o.get("chunk").and_then(Value::as_u64).unwrap_or(0) as usize,
                offset: o.get("offset").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 1.0),
            }),
            v => v.as_u64().map(|page| Position::page(page as usize)),
        }
    }

    pub fn set_position(&mut self, uuid: &str, pos: Position) {
        let value = if pos.chunk == 0 && pos.offset == 0.0 {
            Value::from(pos.page as u64)
        } else {
            json!({ "page": pos.page, "chunk": pos.chunk, "offset": pos.offset })
        };
        self.positions.insert(uuid.to_string(), value);
    }
}

/// Where reading stopped: the page, and in a strip also the fragment of that page and the point within it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Position {
    pub page: usize,
    /// Fragment (chunk) from the start of the page: a long webtoon page consists of many.
    pub chunk: usize,
    /// Fraction of the fragment's height at the top edge of the window, 0..=1.
    pub offset: f64,
}

impl Position {
    pub fn page(page: usize) -> Self {
        Self { page, chunk: 0, offset: 0.0 }
    }

    /// The fraction is rounded (to 0.01 px on a typical fragment) so the file isn't rewritten
    /// on every subpixel shift.
    pub fn new(page: usize, chunk: usize, offset: f64) -> Self {
        Self { page, chunk, offset: (offset.clamp(0.0, 1.0) * 10_000.0).round() / 10_000.0 }
    }

    pub fn is_start(&self) -> bool {
        self.page == 0 && self.chunk == 0 && self.offset == 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_roundtrip_and_old_format() {
        let mut s = Settings::default();
        s.positions.insert("old".into(), Value::from(7u64));
        assert_eq!(s.position("old"), Some(Position::page(7)));
        let pos = Position::new(0, 41, 0.123456);
        s.set_position("strip", pos);
        assert_eq!(s.position("strip"), Some(Position { page: 0, chunk: 41, offset: 0.1235 }));
        // The start of a page is written the old way — as a number, compatible with previous versions.
        s.set_position("page", Position::page(3));
        assert_eq!(s.positions["page"], Value::from(3u64));
        assert_eq!(s.position("missing"), None);
    }
}
