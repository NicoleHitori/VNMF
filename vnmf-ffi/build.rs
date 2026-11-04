// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! The AV1 decoder assembly (`rav1d`, via `avif-decode`) on x86 references Rust tables
//! through PC-relative addresses, while global symbols in a shared library can be interposed from outside —
//! so the linker refuses ("relocation R_X86_64_PC32 … recompile with -fPIC"). `-Bsymbolic` binds
//! the symbols of `libvnmf_ffi.so` to the library itself: nobody interposes them from outside anyway.

fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os == "linux" || os == "android" {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-Bsymbolic");
    }
}
