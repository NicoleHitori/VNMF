// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Sound of page attachments (music, effects) and shared helpers for showing attachments.
//!
//! Playback goes through `gtk::MediaFile` (GTK's built-in GStreamer backend), with no extra
//! dependencies. GStreamer in GTK can't play from an in-memory stream, so the bytes are written to
//! a temporary file in `$XDG_RUNTIME_DIR` (tmpfs — effectively the same memory), which is deleted
//! together with [`Sound`].

use std::cell::Cell;
use std::path::PathBuf;

use gtk::prelude::*;
use gtk::glib;
use vnmf_core::{AuxFormat, AuxKind};

/// Sound from attachment bytes.
pub struct Sound {
    media: gtk::MediaFile,
    path: PathBuf,
}

thread_local!(static NEXT: Cell<u64> = const { Cell::new(0) });

impl Sound {
    pub fn new(bytes: &[u8]) -> std::io::Result<Self> {
        let dir = glib::user_runtime_dir().join(format!("vnmf-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let n = NEXT.with(|c| c.replace(c.get() + 1));
        let path = dir.join(format!("sound-{n}.{}", AuxFormat::detect(bytes).extension()));
        std::fs::write(&path, bytes)?;
        let media = gtk::MediaFile::for_filename(&path);
        media.connect_error_notify(|m| {
            if let Some(e) = m.error() {
                eprintln!("sound: {e}");
            }
        });
        if std::env::var_os("VNMF_DEBUG_SOUND").is_some() {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            media.connect_playing_notify(move |m| eprintln!("sound {name}: playing={} volume={:.2} loop={}", m.is_playing(), m.volume(), m.is_loop()));
        }
        Ok(Self { media, path })
    }

    pub fn media(&self) -> &gtk::MediaFile {
        &self.media
    }

    pub fn play(&self) {
        self.media.play();
    }

    pub fn pause(&self) {
        self.media.pause();
    }

    /// Stop and rewind to the start.
    pub fn stop(&self) {
        self.media.pause();
        if self.media.is_seekable() {
            self.media.seek(0);
        }
    }

    pub fn set_loop(&self, repeat: bool) {
        self.media.set_loop(repeat);
    }

    /// Volume 0.0..=1.0.
    pub fn set_volume(&self, volume: f64) {
        self.media.set_volume(volume.clamp(0.0, 1.0));
    }

    pub fn set_muted(&self, muted: bool) {
        self.media.set_muted(muted);
    }

    pub fn is_playing(&self) -> bool {
        self.media.is_playing()
    }

    /// Fades out over `ms` milliseconds and stops (the sound lives until it has faded).
    pub fn fade_out(self, ms: u32) {
        let start = self.media.volume();
        let steps = (ms / 40).max(1);
        let mut step = 0;
        glib::timeout_add_local(std::time::Duration::from_millis(40), move || {
            step += 1;
            self.media.set_volume(start * f64::from(steps.saturating_sub(step)) / f64::from(steps));
            if step >= steps { glib::ControlFlow::Break } else { glib::ControlFlow::Continue }
        });
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        self.media.pause();
        self.media.clear();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Removes this process's temporary sound files (on exit).
pub fn cleanup() {
    let _ = std::fs::remove_dir_all(glib::user_runtime_dir().join(format!("vnmf-{}", std::process::id())));
}

/// Attachment type icon.
pub fn kind_icon(kind: AuxKind) -> &'static str {
    match kind {
        AuxKind::Music => "audio-x-generic-symbolic",
        AuxKind::Sfx => "audio-volume-high-symbolic",
        AuxKind::Text => "text-x-generic-symbolic",
        AuxKind::DarkImage => "weather-clear-night-symbolic",
        AuxKind::Attachment => "mail-attachment-symbolic",
    }
}
