// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! vnmf-thumbnailer — .vnmf thumbnails for file managers (Nautilus, Nemo, Thunar…).
//!
//! Invoked per the freedesktop specification (see `data/vnmf.thumbnailer`):
//! `vnmf-thumbnailer -s SIZE INPUT OUTPUT.png`.
//!
//! Source — the main preview (with an intact CRC), otherwise the first page. A webtoon (a very tall
//! strip) is cropped from the top to page proportions — otherwise the thumbnail would be a thin sliver.
//! Decoding — gdk-pixbuf (on Fedora via glycin): the same formats as in the reader,
//! including WebP, AVIF and JPEG XL. GTK is not initialized — no display needed.

use std::process::ExitCode;

use gtk::{gdk_pixbuf, gio, glib};
use vnmf_core::{VnmfReader, image_dimensions};

/// Height/width above which an image is considered a webtoon strip and cropped.
const STRIP_RATIO: f64 = 1.6;
/// Proportions to crop the strip to (height/width, as for an ordinary page).
const CROP_RATIO: f64 = 1.5;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vnmf-thumbnailer: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut size = 256;
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-s" | "--size" => size = args.next().and_then(|s| s.parse().ok()).ok_or("-s: a number is required")?,
            "-h" | "--help" => {
                println!("Usage: vnmf-thumbnailer [-s SIZE] INPUT.vnmf OUTPUT.png");
                return Ok(());
            }
            _ => paths.push(arg),
        }
    }
    let [input, output] = paths.as_slice() else {
        return Err("required: [-s SIZE] INPUT OUTPUT (see --help)".into());
    };
    let size: i32 = size.clamp(16, 2048);

    let reader = VnmfReader::open(input).map_err(|e| format!("{input}: {e}"))?;
    let mut last_error = String::from("the file has neither a preview nor pages");
    for data in candidates(&reader) {
        match thumbnail(data, size) {
            Ok(pixbuf) => return pixbuf.savev(output, "png", &[]).map_err(|e| format!("{output}: {e}")),
            Err(e) => last_error = e,
        }
    }
    Err(last_error)
}

/// Images a thumbnail can be made from, in order of preference.
fn candidates(reader: &VnmfReader) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if reader.previews_count() > 0
        && reader.verify_preview_crc(0).unwrap_or(false)
        && let Ok(data) = reader.get_preview_data(0)
    {
        out.push(data);
    }
    // First page with an intact CRC (the first fragment — for a webtoon that's the top of the strip).
    for page in reader.pages().iter().take(3) {
        let chunk = page.first_chunk_idx.get() as usize;
        if reader.verify_chunk_crc(chunk).unwrap_or(false)
            && let Ok(data) = reader.get_chunk_data(chunk)
        {
            out.push(data);
            break;
        }
    }
    out
}

fn thumbnail(data: &[u8], size: i32) -> Result<gdk_pixbuf::Pixbuf, String> {
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(data));
    let strip = image_dimensions(data).is_some_and(|(w, h)| f64::from(h) > f64::from(w.max(1)) * STRIP_RATIO);
    if !strip {
        // Fit into a size×size square keeping proportions (scaling already happens during decoding).
        return gdk_pixbuf::Pixbuf::from_stream_at_scale(&stream, size, size, true, gio::Cancellable::NONE)
            .map_err(|e| e.to_string());
    }
    // Strip: width such that the cropped top fits into the square, then take the top part.
    let width = (f64::from(size) / CROP_RATIO).round() as i32;
    let pixbuf = gdk_pixbuf::Pixbuf::from_stream_at_scale(&stream, width, -1, true, gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    let height = pixbuf.height().min((f64::from(pixbuf.width()) * CROP_RATIO).round() as i32);
    Ok(pixbuf.new_subpixbuf(0, 0, pixbuf.width(), height).copy().unwrap_or(pixbuf))
}
