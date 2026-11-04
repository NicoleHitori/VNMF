// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Page attachments in an unpacked folder: `aux/003.music.ogg`, `aux/003.sfx2.wav`, … and
//! `aux/aux.json` with playback parameters — so that `vnmfunpack` → `vnmfpack` yields the same file.
//!
//! Name: `<number>.<type>[<n>].<extension>`. The number is the ordinal of the page's first image
//! among all input images (from 1; for ordinary pages it's the page number), the type is
//! [`AuxKind::name`] (`music`, `sfx`, `text`, `dark`, `file`), `n` — for several attachments of the same
//! type. Without a type (`003.ogg`) it is determined by contents ([`AuxKind::guess`]).
//!
//! `aux.json`: `{"schema": "vnmf-aux/1", "files": {"003.music.ogg": {"loop": true, "volume": 60}}}` —
//! only parameters that differ from the defaults.

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use vnmf_core::{AuxFlags, AuxFormat, AuxInput, AuxKind, AuxRecord, ChunkSource};

pub const MANIFEST: &str = "aux.json";
const SCHEMA: &str = "vnmf-aux/1";

/// Attachment file name (without the `aux/` folder).
pub fn file_name(number: usize, width: usize, kind: AuxKind, nth: usize, ext: &str) -> String {
    let n = if nth > 1 { nth.to_string() } else { String::new() };
    format!("{number:0width$}.{}{n}.{ext}", kind.name())
}

/// Attachment parameters that differ from the defaults (empty object — all defaults).
pub fn params(rec: &AuxRecord) -> Map<String, Value> {
    let mut m = Map::new();
    if rec.has_flag(AuxFlags::LOOP) {
        m.insert("loop".into(), json!(true));
    }
    for (key, value) in [
        ("delay_ms", rec.delay_ms.get()),
        ("duration_ms", rec.duration_ms.get()),
        ("pages", u32::from(rec.pages.get())),
        ("trigger_y", rec.trigger_y.get()),
        ("part", rec.part.get()),
    ] {
        if value != 0 {
            m.insert(key.into(), json!(value));
        }
    }
    if rec.volume != 0 {
        m.insert("volume".into(), json!(rec.volume));
    }
    m
}

/// Contents of `aux.json` for `(file name, parameters)`; `None` — nothing to write.
pub fn manifest(entries: Vec<(String, Map<String, Value>)>) -> Option<Vec<u8>> {
    let files: Map<String, Value> = entries.into_iter().filter(|(_, p)| !p.is_empty()).map(|(n, p)| (n, Value::Object(p))).collect();
    if files.is_empty() {
        return None;
    }
    serde_json::to_vec_pretty(&json!({ "schema": SCHEMA, "files": files })).ok()
}

/// Parses `aux.json` (empty for an unknown schema).
pub fn parse_manifest(bytes: &[u8]) -> Result<Map<String, Value>> {
    let value: Value = serde_json::from_slice(bytes).context("aux/aux.json: invalid JSON")?;
    Ok(value.get("files").and_then(Value::as_object).cloned().unwrap_or_default())
}

/// Attachment from file `name` (name in the `aux/` folder): image index (from 0) and parameters.
/// `None` — the name doesn't start with a number.
pub fn parse(name: &str, chunk: ChunkSource, manifest: &Map<String, Value>, head: &[u8]) -> Option<(usize, AuxInput)> {
    let digits = name.find(|c: char| !c.is_ascii_digit()).unwrap_or(name.len());
    let index = name[..digits].parse::<usize>().ok()?.checked_sub(1)?;
    // Type is the second part of the name without trailing digits: `003.sfx2.wav` → `sfx`.
    let kind = name
        .split('.')
        .nth(1)
        .filter(|_| name.matches('.').count() >= 2)
        .map(|k| k.trim_end_matches(|c: char| c.is_ascii_digit()))
        .and_then(AuxKind::from_name)
        .unwrap_or_else(|| AuxKind::guess(AuxFormat::detect(head)));
    let mut aux = AuxInput::new(kind, chunk);
    if let Some(p) = manifest.get(name).and_then(Value::as_object) {
        let flag = |key: &str, bit: u16| if p.get(key).and_then(Value::as_bool) == Some(true) { bit } else { 0 };
        let num = |key: &str| p.get(key).and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        aux.flags = flag("loop", AuxFlags::LOOP);
        aux.delay_ms = num("delay_ms");
        aux.duration_ms = num("duration_ms");
        aux.pages = u16::try_from(num("pages")).unwrap_or(u16::MAX);
        aux.trigger_y = num("trigger_y");
        aux.part = num("part");
        aux.volume = u8::try_from(num("volume")).unwrap_or(100).min(100);
    }
    Some((index, aux))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_params_roundtrip() {
        assert_eq!(file_name(3, 3, AuxKind::Music, 1, "ogg"), "003.music.ogg");
        assert_eq!(file_name(3, 3, AuxKind::Sfx, 2, "wav"), "003.sfx2.wav");
        let manifest = parse_manifest(
            br#"{"schema": "vnmf-aux/1", "files": {"003.sfx2.wav": {"loop": true, "volume": 40, "delay_ms": 500, "duration_ms": 8000, "pages": 2}}}"#,
        )
        .unwrap();
        let src = || ChunkSource::from_bytes(b"RIFF\0\0\0\0WAVE".to_vec()).unwrap();
        let (idx, aux) = parse("003.sfx2.wav", src(), &manifest, b"RIFF\0\0\0\0WAVE").unwrap();
        assert_eq!((idx, aux.kind, aux.flags, aux.volume, aux.delay_ms), (2, AuxKind::Sfx, AuxFlags::LOOP, 40, 500));
        assert_eq!((aux.duration_ms, aux.pages), (8000, 2));
        // No type in the name — by contents; no number — not an attachment.
        assert_eq!(parse("010.ogg", src(), &manifest, b"OggS").unwrap().1.kind, AuxKind::Sfx);
        assert_eq!(parse("010.json", src(), &manifest, b"{}").unwrap().1.kind, AuxKind::Text);
        assert!(parse("readme.txt", src(), &manifest, b"hi").is_none());
    }
}
