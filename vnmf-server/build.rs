// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Embeds the web interface (`web/`, built from `vnmf-web/`) into the binary: the server needs no
//! files next to it, and building it needs no JavaScript tooling — the built files are in the repository.

use std::fmt::Write;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, root, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push((rel.to_string_lossy().replace('\\', "/"), path));
        }
    }
}

fn main() {
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("web");
    // A directory: Cargo rescans everything inside it for changes.
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();
    let mut code = String::from("/// Files of the web interface: (path, contents).\npub static WEB_FILES: &[(&str, &[u8])] = &[\n");
    for (rel, path) in files {
        let _ = writeln!(code, "    ({rel:?}, include_bytes!({:?})),", path.display().to_string());
    }
    code.push_str("];\n");
    let out = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("web_files.rs");
    std::fs::write(out, code).expect("writing web_files.rs");
}
