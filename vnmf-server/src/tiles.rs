// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Tiles of strip fragments too tall for browsers.
//!
//! Browsers don't decode images taller than 32767 px (Firefox and Chrome alike): such a fragment
//! stays blank. Webtoons downloaded from sites sometimes have them (a whole chapter in a few
//! images). The server cuts such a fragment into JPEG tiles once and keeps them in
//! `<data>/cache/tiles/<uuid>-<version>/`; the file itself is not changed. Native readers decode
//! the original.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use image::RgbImage;
use vnmf_core::PageFormat;

use crate::AppState;
use crate::book::OpenBook;
use crate::error::{ApiError, ApiResult};

/// The tallest image browsers show.
pub const BROWSER_LIMIT: u32 = 32767;
/// The height of a tile (the last one is shorter).
pub const TILE_HEIGHT: u32 = 8192;
const JPEG_QUALITY: u8 = 90;

/// How many tiles a fragment `height` px tall is shown as; 0 — it is shown as is.
pub fn count(height: u32) -> u32 {
    if height > BROWSER_LIMIT { height.div_ceil(TILE_HEIGHT) } else { 0 }
}

fn book_dir(state: &AppState, book: &OpenBook) -> PathBuf {
    state.config.tiles_dir().join(format!("{}-{}", vnmf::format_uuid(book.reader.uuid()), book.version))
}

fn tile_path(dir: &Path, chunk: usize, tile: u32) -> PathBuf {
    dir.join(format!("{chunk}-{tile}.jpg"))
}

/// The JPEG of tile `tile` of `chunk`, cutting the fragment's tiles first if they aren't cached.
pub async fn tile(state: &AppState, book: &Arc<OpenBook>, chunk: usize, tile: u32) -> ApiResult<Bytes> {
    let height = book.chunk_size(chunk).map_or(0, |(_, h)| h);
    if tile >= count(height) {
        return Err(ApiError::not_found(format!("tile {tile} of chunk {chunk}")));
    }
    let dir = book_dir(state, book);
    let path = tile_path(&dir, chunk, tile);
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return Ok(bytes.into());
    }
    let _one_at_a_time = state.tiling.lock().await;
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return Ok(bytes.into());
    }
    let book = book.clone();
    let tiles_dir = state.config.tiles_dir();
    tokio::task::spawn_blocking(move || cut(&book, chunk, &dir, &tiles_dir)).await.map_err(anyhow::Error::from)??;
    Ok(tokio::fs::read(&path).await?.into())
}

/// Decodes the fragment and writes all its tiles. Tiles of older versions of the book are removed.
fn cut(book: &OpenBook, chunk: usize, dir: &Path, tiles_dir: &Path) -> ApiResult<()> {
    let data = book.reader.get_chunk_data(chunk).map_err(|e| ApiError::unprocessable("damaged", e.to_string()))?;
    let image = decode(data)?;
    let prefix = format!("{}-", vnmf::format_uuid(book.reader.uuid()));
    if let Ok(entries) = std::fs::read_dir(tiles_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && entry.path() != dir {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    std::fs::create_dir_all(dir)?;
    let (width, height) = image.dimensions();
    for t in 0..count(height) {
        let top = t * TILE_HEIGHT;
        let part = image::imageops::crop_imm(&image, 0, top, width, TILE_HEIGHT.min(height - top)).to_image();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
            .encode_image(&part)
            .map_err(|e| ApiError::from(anyhow::anyhow!("encoding a tile: {e}")))?;
        // Written whole or not at all: a reader may ask for it meanwhile.
        let path = tile_path(dir, chunk, t);
        let temp = path.with_extension("part");
        std::fs::File::create(&temp)?.write_all(&jpeg)?;
        std::fs::rename(&temp, &path)?;
    }
    Ok(())
}

fn decode(data: &[u8]) -> ApiResult<RgbImage> {
    let unsupported = |e: String| ApiError::unprocessable("unsupported", format!("can't decode the image: {e}"));
    if PageFormat::detect(data) == PageFormat::Avif {
        let image = avif_decode::Decoder::from_avif(data).and_then(|d| d.to_image()).map_err(|e| unsupported(e.to_string()))?;
        return Ok(avif_to_rgb(image));
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format().map_err(|e| unsupported(e.to_string()))?;
    reader.no_limits(); // the image is known to be huge; one at a time
    Ok(reader.decode().map_err(|e| unsupported(e.to_string()))?.to_rgb8())
}

fn avif_to_rgb(image: avif_decode::Image) -> RgbImage {
    use avif_decode::Image;
    fn collect<P: Copy>(img: &imgref::ImgVec<P>, px: impl Fn(P) -> [u8; 3]) -> RgbImage {
        let mut out = Vec::with_capacity(img.width() * img.height() * 3);
        for row in img.rows() {
            for &p in row {
                out.extend_from_slice(&px(p));
            }
        }
        RgbImage::from_raw(img.width() as u32, img.height() as u32, out).expect("the buffer matches the size")
    }
    let hi = |v: u16| (v >> 8) as u8;
    match image {
        // The usual case, copied whole (pixel by pixel takes seconds in a debug build).
        Image::Rgb8(img) => {
            use rgb::ComponentBytes;
            let (buf, width, height) = img.into_contiguous_buf();
            RgbImage::from_raw(width as u32, height as u32, buf.as_bytes().to_vec()).expect("the buffer matches the size")
        }
        Image::Rgba8(img) => collect(&img, |p| [p.r, p.g, p.b]),
        Image::Rgb16(img) => collect(&img, |p| [hi(p.r), hi(p.g), hi(p.b)]),
        Image::Rgba16(img) => collect(&img, |p| [hi(p.r), hi(p.g), hi(p.b)]),
        Image::Gray8(img) => collect(&img, |p| [p.value(); 3]),
        Image::Gray16(img) => collect(&img, |p| [hi(p.value()); 3]),
    }
}
