#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Extracts translatable strings from the sources into po/vnmf.pot and merges them into every po/*.po.
# Requires gettext ≥ 0.24 (xgettext with Rust support). Run from anywhere: po/update.sh
set -euo pipefail
cd "$(dirname "$0")/.."

mapfile -t files < <(grep -rlE '\b(tr|n_|ntr)\(|\b(trf|ntrf)!' --include='*.rs' src vnmf-core/src vnmf-guard/src vnmf-gui/src | sort)

xgettext --language=Rust --from-code=UTF-8 \
    --keyword=tr --keyword=n_ --keyword='trf!' --keyword='ntr:1,2' --keyword='ntrf!:1,2' \
    --add-comments=TRANSLATORS: --sort-by-file \
    --package-name=vnmf --package-version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)" \
    --copyright-holder='void_nicole (Nicole Hitori)' \
    -o po/vnmf.pot "${files[@]}"

for po in po/*.po; do
    [[ -e "$po" ]] || continue
    msgmerge --quiet --update --backup=none --previous "$po" po/vnmf.pot
    printf '%s: ' "$po"; msgfmt --check --statistics -o /dev/null "$po"
done
