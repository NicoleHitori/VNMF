#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Compiles po/<lang>.po into <dest>/<lang>/LC_MESSAGES/vnmf.mo (default dest: target/locale).
# The programs find the catalogs in <prefix>/share/locale; for a development run:
#   po/build.sh && VNMF_LOCALEDIR=target/locale cargo run --bin vnmf-reader
set -euo pipefail
cd "$(dirname "$0")/.."
dest=${1:-target/locale}
for po in po/*.po; do
    [[ -e "$po" ]] || continue
    lang=$(basename "$po" .po)
    mkdir -p "$dest/$lang/LC_MESSAGES"
    msgfmt --check -o "$dest/$lang/LC_MESSAGES/vnmf.mo" "$po"
done
