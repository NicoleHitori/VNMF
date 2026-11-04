// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Automatic format detection by contents (magic bytes), not by extension.
//!
//! - [`PageFormat::detect`] — page / preview image format;
//! - [`InfoFormat::detect`] — Info section format (JSON / TOML / XML / text);
//! - [`AuxFormat::detect`] — page attachment data format (image, audio, text);
//! - [`detect_extension`] — extension for an arbitrary chunk (when unpacking).

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PageFormat {
    Avif = 0,
    WebP = 1,
    Png = 2,
    Jpeg = 3,
    Gif = 4,
    /// JPEG XL (ISO/IEC 18181): a "bare" codestream or an ISO-BMFF container.
    JpegXl = 5,
    Unknown = 255,
}

impl From<u8> for PageFormat {
    fn from(value: u8) -> Self {
        match value {
            0 => PageFormat::Avif,
            1 => PageFormat::WebP,
            2 => PageFormat::Png,
            3 => PageFormat::Jpeg,
            4 => PageFormat::Gif,
            5 => PageFormat::JpegXl,
            _ => PageFormat::Unknown, // Everything else becomes Unknown
        }
    }
}

impl PageFormat {
    /// Detects the image format by the first bytes. The first ~64 bytes are enough.
    pub fn detect(data: &[u8]) -> Self {
        if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            PageFormat::Png
        } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            PageFormat::Jpeg
        } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
            PageFormat::Gif
        } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            PageFormat::WebP
        } else if is_avif(data) {
            PageFormat::Avif
        } else if is_jxl(data) {
            PageFormat::JpegXl
        } else {
            PageFormat::Unknown
        }
    }

    /// Format by file extension (without the dot, case-insensitive).
    /// Only as a fallback — [`Self::detect`] is the one to trust.
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_ascii_lowercase().as_str() {
            "avif" => PageFormat::Avif,
            "webp" => PageFormat::WebP,
            "png" => PageFormat::Png,
            "jpg" | "jpeg" | "jfif" => PageFormat::Jpeg,
            "gif" => PageFormat::Gif,
            "jxl" => PageFormat::JpegXl,
            _ => PageFormat::Unknown,
        }
    }

    /// Canonical file extension without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            PageFormat::Avif => "avif",
            PageFormat::WebP => "webp",
            PageFormat::Png => "png",
            PageFormat::Jpeg => "jpg",
            PageFormat::Gif => "gif",
            PageFormat::JpegXl => "jxl",
            PageFormat::Unknown => "bin",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            PageFormat::Avif => "image/avif",
            PageFormat::WebP => "image/webp",
            PageFormat::Png => "image/png",
            PageFormat::Jpeg => "image/jpeg",
            PageFormat::Gif => "image/gif",
            PageFormat::JpegXl => "image/jxl",
            PageFormat::Unknown => "application/octet-stream",
        }
    }

    #[inline]
    pub fn is_known(self) -> bool {
        self != PageFormat::Unknown
    }
}

/// Image dimensions (width, height) from its header — without decoding.
/// The first few KB of data are enough; `None` if the format isn't recognized.
pub fn image_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let size = imagesize::blob_size(data).ok()?;
    Some((u32::try_from(size.width).ok()?, u32::try_from(size.height).ok()?))
}

/// AVIF is an ISO-BMFF container: `[size:4]["ftyp"][major_brand:4][minor:4][compatible_brands…]`.
/// The `avif` (still) or `avis` (animated) brand may be the major brand or among the compatible ones.
fn is_avif(data: &[u8]) -> bool {
    if data.len() < 16 || &data[4..8] != b"ftyp" {
        return false;
    }
    let box_size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let end = box_size.clamp(16, data.len());
    let is_avif_brand = |b: &[u8]| b == b"avif" || b == b"avis";

    is_avif_brand(&data[8..12]) || data[16..end].as_chunks::<4>().0.iter().any(|b| is_avif_brand(b))
}

/// Signature of a "bare" JPEG XL codestream.
pub(crate) const JXL_CODESTREAM: [u8; 2] = [0xFF, 0x0A];
/// First box of a JPEG XL container: `[size=12]["JXL "][0D 0A 87 0A]`.
pub(crate) const JXL_CONTAINER: [u8; 12] = [0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A];

fn is_jxl(data: &[u8]) -> bool {
    data.starts_with(&JXL_CODESTREAM) || data.starts_with(&JXL_CONTAINER)
}

impl fmt::Display for PageFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Avif => "AVIF",
            Self::WebP => "WebP",
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Gif => "GIF",
            Self::JpegXl => "JPEG XL",
            Self::Unknown => "Unknown",
        };
        f.write_str(s)
    }
}

/// Content format of the Info section. Not stored in the file itself — detected from contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoFormat {
    Json,
    Toml,
    Xml,
    Text,
    Binary,
}

impl InfoFormat {
    pub fn detect(data: &[u8]) -> Self {
        let Ok(text) = std::str::from_utf8(data) else {
            return InfoFormat::Binary;
        };
        let text = text.trim_start_matches('\u{feff}').trim_start();

        if text.starts_with('<') {
            return InfoFormat::Xml;
        }
        if text.starts_with('{') {
            return InfoFormat::Json;
        }

        // `[` is either a JSON array or a TOML table `[section]` / `[[array]]`.
        let first_line = text.lines().next().unwrap_or("").trim();
        if first_line.starts_with('[') {
            let header = first_line.trim_start_matches('[').trim_end_matches(']');
            let is_toml_table = first_line.ends_with(']') && !header.is_empty() && header.chars().all(|c| c.is_alphanumeric() || "_-. \"'".contains(c));
            return if is_toml_table { InfoFormat::Toml } else { InfoFormat::Json };
        }

        // TOML without tables: the first meaningful line looks like `key = value`.
        let first_meaningful = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#'));
        if let Some(line) = first_meaningful
            && let Some((key, _)) = line.split_once('=')
            && !key.trim().is_empty()
            && key.trim().chars().all(|c| c.is_alphanumeric() || "_-.\"'".contains(c))
        {
            return InfoFormat::Toml;
        }

        InfoFormat::Text
    }

    pub fn extension(self) -> &'static str {
        match self {
            InfoFormat::Json => "json",
            InfoFormat::Toml => "toml",
            InfoFormat::Xml => "xml",
            InfoFormat::Text => "txt",
            InfoFormat::Binary => "bin",
        }
    }
}

impl fmt::Display for InfoFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Json => "JSON",
            Self::Toml => "TOML",
            Self::Xml => "XML",
            Self::Text => "Text",
            Self::Binary => "Binary",
        };
        f.write_str(s)
    }
}

/// Attachment data format (`AuxRecord::format`). Image codes match [`PageFormat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AuxFormat {
    Avif = 0,
    WebP = 1,
    Png = 2,
    Jpeg = 3,
    Gif = 4,
    JpegXl = 5,
    /// Ogg (Vorbis, Opus, FLAC in Ogg).
    Ogg = 16,
    Mp3 = 17,
    Flac = 18,
    Wav = 19,
    /// ISO-BMFF audio (AAC, ALAC): `.m4a`.
    M4a = 20,
    /// UTF-8 text.
    Text = 32,
    /// JSON (e.g. the `vnmf-text/1` text layer).
    Json = 33,
    Unknown = 255,
}

impl AuxFormat {
    const ALL: [AuxFormat; 14] = [
        AuxFormat::Avif,
        AuxFormat::WebP,
        AuxFormat::Png,
        AuxFormat::Jpeg,
        AuxFormat::Gif,
        AuxFormat::JpegXl,
        AuxFormat::Ogg,
        AuxFormat::Mp3,
        AuxFormat::Flac,
        AuxFormat::Wav,
        AuxFormat::M4a,
        AuxFormat::Text,
        AuxFormat::Json,
        AuxFormat::Unknown,
    ];

    /// Detects the format by contents.
    pub fn detect(data: &[u8]) -> Self {
        if let Some(image) = Self::from_image(PageFormat::detect(data)) {
            return image;
        }
        if data.starts_with(b"OggS") {
            AuxFormat::Ogg
        } else if data.starts_with(b"fLaC") {
            AuxFormat::Flac
        } else if data.starts_with(b"ID3") || (data.len() >= 2 && data[0] == 0xFF && data[1] & 0xE0 == 0xE0) {
            AuxFormat::Mp3
        } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WAVE" {
            AuxFormat::Wav
        } else if data.len() >= 12 && &data[4..8] == b"ftyp" {
            AuxFormat::M4a
        } else {
            match InfoFormat::detect(data) {
                InfoFormat::Json => AuxFormat::Json,
                InfoFormat::Binary => AuxFormat::Unknown,
                // Zero bytes and control characters are not text but binary data.
                _ if data.iter().any(|&b| b == 0 || (b < 0x20 && !b"\t\n\r".contains(&b))) => AuxFormat::Unknown,
                _ => AuxFormat::Text,
            }
        }
    }

    fn from_image(format: PageFormat) -> Option<Self> {
        Some(match format {
            PageFormat::Avif => AuxFormat::Avif,
            PageFormat::WebP => AuxFormat::WebP,
            PageFormat::Png => AuxFormat::Png,
            PageFormat::Jpeg => AuxFormat::Jpeg,
            PageFormat::Gif => AuxFormat::Gif,
            PageFormat::JpegXl => AuxFormat::JpegXl,
            PageFormat::Unknown => return None,
        })
    }

    /// Image format, if this is an image.
    pub fn image(self) -> Option<PageFormat> {
        let code = self as u8;
        (code <= 5).then(|| PageFormat::from(code))
    }

    pub fn is_audio(self) -> bool {
        matches!(self, AuxFormat::Ogg | AuxFormat::Mp3 | AuxFormat::Flac | AuxFormat::Wav | AuxFormat::M4a)
    }

    pub fn is_text(self) -> bool {
        matches!(self, AuxFormat::Text | AuxFormat::Json)
    }

    pub fn extension(self) -> &'static str {
        if let Some(image) = self.image() {
            return image.extension();
        }
        match self {
            AuxFormat::Ogg => "ogg",
            AuxFormat::Mp3 => "mp3",
            AuxFormat::Flac => "flac",
            AuxFormat::Wav => "wav",
            AuxFormat::M4a => "m4a",
            AuxFormat::Text => "txt",
            AuxFormat::Json => "json",
            _ => "bin",
        }
    }

    pub fn mime_type(self) -> &'static str {
        if let Some(image) = self.image() {
            return image.mime_type();
        }
        match self {
            AuxFormat::Ogg => "audio/ogg",
            AuxFormat::Mp3 => "audio/mpeg",
            AuxFormat::Flac => "audio/flac",
            AuxFormat::Wav => "audio/wav",
            AuxFormat::M4a => "audio/mp4",
            AuxFormat::Text => "text/plain",
            AuxFormat::Json => "application/json",
            _ => "application/octet-stream",
        }
    }
}

impl From<u8> for AuxFormat {
    fn from(value: u8) -> Self {
        Self::ALL.into_iter().find(|f| *f as u8 == value).unwrap_or(AuxFormat::Unknown)
    }
}

impl fmt::Display for AuxFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(image) = self.image() {
            return image.fmt(f);
        }
        let s = match self {
            AuxFormat::Ogg => "Ogg",
            AuxFormat::Mp3 => "MP3",
            AuxFormat::Flac => "FLAC",
            AuxFormat::Wav => "WAV",
            AuxFormat::M4a => "M4A",
            AuxFormat::Text => "Text",
            AuxFormat::Json => "JSON",
            _ => "Unknown",
        };
        f.write_str(s)
    }
}

/// Extension (without the dot) for an arbitrary chunk: an image, audio or a text layer.
pub fn detect_extension(data: &[u8]) -> &'static str {
    AuxFormat::detect(data).extension()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_images() {
        assert_eq!(PageFormat::detect(b"\x89PNG\r\n\x1a\n...."), PageFormat::Png);
        assert_eq!(PageFormat::detect(&[0xFF, 0xD8, 0xFF, 0xE0]), PageFormat::Jpeg);
        assert_eq!(PageFormat::detect(b"GIF89a"), PageFormat::Gif);
        assert_eq!(PageFormat::detect(b"RIFF\0\0\0\0WEBPVP8 "), PageFormat::WebP);
        assert_eq!(PageFormat::detect(b"\0\0\0\x1cftypavif\0\0\0\0avifmif1"), PageFormat::Avif);
        // AVIF only among compatible brands
        assert_eq!(PageFormat::detect(b"\0\0\0\x1cftypmif1\0\0\0\0mif1avif"), PageFormat::Avif);
        // HEIC is also ftyp, but not AVIF
        assert_eq!(PageFormat::detect(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"), PageFormat::Unknown);
        assert_eq!(PageFormat::detect(&[0xFF, 0x0A, 0xFA, 0x7F]), PageFormat::JpegXl);
        assert_eq!(PageFormat::detect(b"\0\0\0\x0cJXL \r\n\x87\n\0\0\0\x14ftypjxl "), PageFormat::JpegXl);
        assert_eq!(PageFormat::from(5), PageFormat::JpegXl);
        assert_eq!(PageFormat::detect(b"hello"), PageFormat::Unknown);
    }

    #[test]
    fn detects_info() {
        assert_eq!(InfoFormat::detect(br#"{"a": 1}"#), InfoFormat::Json);
        assert_eq!(InfoFormat::detect(b"[1, 2]"), InfoFormat::Json);
        assert_eq!(InfoFormat::detect(b"\n[meta]\ntitle = \"x\""), InfoFormat::Toml);
        assert_eq!(InfoFormat::detect(b"# c\ntitle = \"x\""), InfoFormat::Toml);
        assert_eq!(InfoFormat::detect(b"<?xml version='1.0'?><a/>"), InfoFormat::Xml);
        assert_eq!(InfoFormat::detect("Just a description, привіт".as_bytes()), InfoFormat::Text);
        assert_eq!(InfoFormat::detect(&[0xFF, 0xFE, 0x00]), InfoFormat::Binary);
    }

    #[test]
    fn detects_aux_extensions() {
        assert_eq!(detect_extension(b"OggS...."), "ogg");
        assert_eq!(detect_extension(b"ID3\x04"), "mp3");
        assert_eq!(detect_extension(br#"{"ocr": []}"#), "json");
        assert_eq!(detect_extension(&[0, 1, 2, 0xFF]), "bin");
        assert_eq!(detect_extension("Page text, привіт".as_bytes()), "txt");
        assert_eq!(AuxFormat::detect(b"fLaC\0\0"), AuxFormat::Flac);
        assert_eq!(AuxFormat::from(AuxFormat::Wav as u8), AuxFormat::Wav);
        assert_eq!(AuxFormat::Png.image(), Some(PageFormat::Png));
        assert!(AuxFormat::Mp3.is_audio() && AuxFormat::Mp3.image().is_none());
    }
}
