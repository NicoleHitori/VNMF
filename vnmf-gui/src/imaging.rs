// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Decoding images in the background.
//!
//! GTK decodes PNG/JPEG itself, and WebP, AVIF, JPEG XL, GIF via gdk-pixbuf and glycin loaders,
//! so all VNMF formats are shown without extra libraries.

use std::collections::{HashMap, VecDeque};

use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, gio, glib};
use vnmf_i18n::tr;

/// Full decode (for reading). Runs on a background thread.
pub async fn decode(bytes: Vec<u8>) -> Result<gdk::Texture, String> {
    gio::spawn_blocking(move || gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).map_err(|e| e.to_string()))
        .await
        .map_err(|_| tr("decoding crashed").to_string())?
}

/// Downscaled image of width `width` px (thumbnails, previews).
///
/// Scaling happens during decoding — no full-size RGBA buffer sits in memory
/// for every thumbnail (a 2000×3000 page is 24 MB).
pub async fn decode_scaled(bytes: Vec<u8>, width: i32) -> Result<gdk::Texture, String> {
    gio::spawn_blocking(move || {
        let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
        let pixbuf = gdk_pixbuf::Pixbuf::from_stream_at_scale(&stream, width, -1, true, gio::Cancellable::NONE)
            .map_err(|e| e.to_string())?;
        let format = if pixbuf.has_alpha() { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
        let texture = gdk::MemoryTexture::new(
            pixbuf.width(),
            pixbuf.height(),
            format,
            &pixbuf.read_pixel_bytes(),
            pixbuf.rowstride() as usize,
        );
        Ok(texture.upcast::<gdk::Texture>())
    })
    .await
    .map_err(|_| tr("decoding crashed").to_string())?
}

/// An empty paintable with the right proportions — a placeholder while the image decodes.
/// Thanks to it the layout doesn't "jump" when the picture appears.
pub fn placeholder(width: u32, height: u32) -> gdk::Paintable {
    gdk::Paintable::new_empty(width.max(1) as i32, height.max(1) as i32)
}

/// A small LRU cache of decoded textures by key (chunk number, etc.).
///
/// Limited both by the number of textures and by their total size in memory (`max_bytes`,
/// RGBA: width × height × 4), whichever is reached first. The most recently inserted texture is
/// always kept, even if it alone exceeds the budget.
pub struct TextureCache {
    capacity: usize,
    max_bytes: usize,
    used: usize,
    map: HashMap<usize, gdk::Texture>,
    order: VecDeque<usize>,
}

/// Memory taken by a decoded texture (RGBA).
pub fn texture_bytes(texture: &gdk::Texture) -> usize {
    texture.width().max(0) as usize * texture.height().max(0) as usize * 4
}

impl TextureCache {
    pub fn new(capacity: usize) -> Self {
        Self::with_budget(capacity, usize::MAX)
    }

    /// At most `capacity` textures and `max_bytes` bytes.
    pub fn with_budget(capacity: usize, max_bytes: usize) -> Self {
        Self { capacity, max_bytes, used: 0, map: HashMap::new(), order: VecDeque::new() }
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub fn contains(&self, key: usize) -> bool {
        self.map.contains_key(&key)
    }

    pub fn get(&mut self, key: usize) -> Option<gdk::Texture> {
        let texture = self.map.get(&key)?.clone();
        self.touch(key);
        Some(texture)
    }

    /// Marks a texture as recently used (protects it from eviction) without returning it.
    pub fn touch(&mut self, key: usize) -> bool {
        if !self.map.contains_key(&key) {
            return false;
        }
        if self.order.back() != Some(&key) {
            self.order.retain(|&k| k != key);
            self.order.push_back(key);
        }
        true
    }

    pub fn insert(&mut self, key: usize, texture: gdk::Texture) {
        self.used += texture_bytes(&texture);
        match self.map.insert(key, texture) {
            Some(old) => {
                self.used -= texture_bytes(&old);
                self.touch(key);
            }
            None => self.order.push_back(key),
        }
        while self.order.len() > 1 && (self.order.len() > self.capacity || self.used > self.max_bytes) {
            let Some(old) = self.order.pop_front() else { break };
            if let Some(texture) = self.map.remove(&old) {
                self.used -= texture_bytes(&texture);
            }
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.used = 0;
    }
}

/// Thumbnail for a page source (a file on disk or bytes in memory, e.g. from a CBZ).
pub async fn thumbnail(source: vnmf_core::ChunkSource, width: i32) -> Result<gdk::Texture, String> {
    let bytes = gio::spawn_blocking(move || match &source.data {
        vnmf_core::ChunkData::File(path) => std::fs::read(path).map_err(|e| e.to_string()),
        data => Ok(data.bytes().unwrap_or_default().to_vec()),
    })
    .await
    .map_err(|_| tr("reading crashed").to_string())??;
    decode_scaled(bytes, width).await
}
