// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Server settings and the layout of the data folder:
//!
//! ```text
//! <data>/vnmf-server.db   database
//! <data>/library/         uploaded books (the server's own library — it may change and delete them)
//! <data>/incoming/        uploads in progress
//! <data>/cache/export/    CBZ / ZIP exports
//! <data>/cache/tiles/     JPEG tiles of strip fragments too tall for browsers
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub data_dir: PathBuf,
    /// Additional folders with `.vnmf` files, scanned recursively. Read-only unless
    /// [`Self::writable_libraries`].
    pub libraries: Vec<PathBuf>,
    /// Allow rewriting (metadata, covers, protection, repair) and deleting files in [`Self::libraries`].
    pub writable_libraries: bool,
    /// Maximum size of one upload, bytes.
    pub max_upload: u64,
    /// Rescan the libraries periodically (`None` — only at startup and on request).
    pub scan_interval: Option<Duration>,
    /// Every book gets a full integrity check at least this often, in the background (`None` — only
    /// on request). Silent disk damage is found while the redundancy can still repair it.
    pub verify_interval: Option<Duration>,
    /// Origins allowed to call the API from a browser (CORS); empty — same origin only.
    pub cors_origins: Vec<String>,
    /// Session cookies only over HTTPS (`Secure`); enable behind a TLS proxy.
    pub secure_cookies: bool,
}

impl Config {
    /// Defaults for a data folder (used by tests and as the base for the command line).
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            libraries: Vec::new(),
            writable_libraries: false,
            max_upload: 4 << 30,
            scan_interval: None,
            verify_interval: None,
            cors_origins: Vec::new(),
            secure_cookies: false,
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("vnmf-server.db")
    }

    /// The server's own library: uploads go here.
    pub fn library_dir(&self) -> PathBuf {
        self.data_dir.join("library")
    }

    pub fn incoming_dir(&self) -> PathBuf {
        self.data_dir.join("incoming")
    }

    pub fn export_dir(&self) -> PathBuf {
        self.data_dir.join("cache").join("export")
    }

    pub fn tiles_dir(&self) -> PathBuf {
        self.data_dir.join("cache").join("tiles")
    }

    /// All folders that are scanned: the own library first.
    pub fn roots(&self) -> Vec<PathBuf> {
        std::iter::once(self.library_dir()).chain(self.libraries.iter().cloned()).collect()
    }

    /// Whether the file is in the server's own library.
    pub fn is_managed(&self, path: &Path) -> bool {
        path.starts_with(self.library_dir())
    }

    pub fn prepare_dirs(&self) -> std::io::Result<()> {
        for dir in [self.data_dir.clone(), self.library_dir(), self.incoming_dir(), self.export_dir(), self.tiles_dir()] {
            std::fs::create_dir_all(dir)?;
        }
        // Leftovers of uploads interrupted by a restart.
        if let Ok(entries) = std::fs::read_dir(self.incoming_dir()) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        Ok(())
    }
}
