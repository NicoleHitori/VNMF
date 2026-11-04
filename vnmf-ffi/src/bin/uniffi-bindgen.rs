// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! UniFFI binding generator (Kotlin) working from an already built library:
//! `cargo run -p vnmf-ffi --features cli --bin uniffi-bindgen -- generate --library <libvnmf_ffi.so> --language kotlin --out-dir <dir>`.

fn main() {
    uniffi::uniffi_bindgen_main()
}
