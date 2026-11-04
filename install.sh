#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist
#
# Installs VNMF and integrates it into the desktop (GNOME, KDE, …):
#   the application/x-vnmf file type, thumbnails in the file manager, app launchers,
#   "Open with", VNMF Reader as the default application.
#
#   ./install.sh                    # for yourself: ~/.local (no sudo)
#   sudo ./install.sh --system      # for everyone: /usr/local
#   ./install.sh --prefix DIR       # custom prefix
#   ./install.sh --uninstall [...]  # remove (with the same parameters)
#   ./install.sh --no-build         # don't build (use what's already in target/release)
#   ./install.sh --no-default       # don't make VNMF Reader the default application

set -euo pipefail
cd "$(dirname "$0")"

PREFIX="$HOME/.local"
SYSTEM=0
BUILD=1
UNINSTALL=0
SET_DEFAULT=1
while [[ $# -gt 0 ]]; do
    case "$1" in
        --system) PREFIX=/usr/local; SYSTEM=1 ;;
        --prefix) PREFIX="$2"; shift ;;
        --no-build) BUILD=0 ;;
        --uninstall) UNINSTALL=1 ;;
        --no-default) SET_DEFAULT=0 ;;
        -h|--help) sed -n '5,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

if [[ $SYSTEM -eq 1 && $EUID -ne 0 ]]; then
    echo "--system requires root: sudo $0 --system" >&2
    exit 1
fi

BINS=(vnmfinfo vnmfpack vnmfunpack vnmfrecovery vnmfbadtest vnmfguard vnmf-reader vnmf-info vnmf-creator vnmf-guard vnmf-thumbnailer)
APPS=(org.vnmf.Reader org.vnmf.Info org.vnmf.Creator org.vnmf.Guard)
# Install just for yourself (into the home folder) or system-wide.
USER_MODE=0
[[ "$PREFIX" == "$HOME"/* ]] && USER_MODE=1
BIN="$PREFIX/bin"
SHARE="$PREFIX/share"
ICONS="$SHARE/icons/hicolor"

# Refresh caches after changing files (each utility is optional).
refresh() {
    command -v update-mime-database >/dev/null && update-mime-database "$SHARE/mime" || true
    command -v update-desktop-database >/dev/null && update-desktop-database -q "$SHARE/applications" || true
    if command -v gtk4-update-icon-cache >/dev/null; then
        gtk4-update-icon-cache -q -t -f "$ICONS" || true
    elif command -v gtk-update-icon-cache >/dev/null; then
        gtk-update-icon-cache -q -t -f "$ICONS" || true
    fi
}

if [[ $UNINSTALL -eq 1 ]]; then
    for b in "${BINS[@]}"; do rm -f "$BIN/$b"; done
    for a in "${APPS[@]}"; do
        rm -f "$SHARE/applications/$a.desktop" "$ICONS/scalable/apps/$a.svg" "$ICONS/symbolic/apps/$a-symbolic.svg"
    done
    rm -f "$SHARE/metainfo/org.vnmf.Reader.metainfo.xml" \
          "$SHARE/mime/packages/vnmf.xml" \
          "$ICONS/scalable/mimetypes/application-x-vnmf.svg" \
          "$SHARE/thumbnailers/vnmf.thumbnailer" \
          "$SHARE"/locale/*/LC_MESSAGES/vnmf.mo
    if [[ -f "$SHARE/applications/mimeapps.list" ]]; then
        sed -i '/^application\/x-vnmf=/d' "$SHARE/applications/mimeapps.list"
        # Only the header is left — we created the file, remove it.
        if ! grep -qv '^\[Default Applications\]$' "$SHARE/applications/mimeapps.list"; then
            rm -f "$SHARE/applications/mimeapps.list"
        fi
    fi
    refresh
    echo "VNMF removed from $PREFIX"
    exit 0
fi

if [[ $BUILD -eq 1 ]]; then
    if [[ $EUID -eq 0 && -n "${SUDO_USER:-}" ]]; then
        # Build as the user: otherwise target/ and ~/.cargo would become owned by root.
        sudo -u "$SUDO_USER" -- bash -lc "cd '$PWD' && cargo build --release --workspace --bins"
    else
        cargo build --release --workspace --bins
    fi
fi

for b in "${BINS[@]}"; do
    [[ -x "target/release/$b" ]] || { echo "target/release/$b is missing — build without --no-build" >&2; exit 1; }
    install -Dm755 "target/release/$b" "$BIN/$b"
done
# Absolute paths in Exec: the desktop session may not have $PREFIX/bin in PATH (especially ~/.local/bin).
install_exec() { # source target
    install -Dm644 "$1" "$2"
    sed -i -E "s|^(TryExec=)(vnmf[-a-z]*)|\1$BIN/\2|; s|^(Exec=)(vnmf[-a-z]*)|\1$BIN/\2|" "$2"
}
for a in "${APPS[@]}"; do
    install_exec "data/$a.desktop" "$SHARE/applications/$a.desktop"
    install -Dm644 "data/icons/hicolor/scalable/apps/$a.svg" "$ICONS/scalable/apps/$a.svg"
done
install -Dm644 data/icons/hicolor/symbolic/apps/org.vnmf.Reader-symbolic.svg "$ICONS/symbolic/apps/org.vnmf.Reader-symbolic.svg"
install -Dm644 data/icons/hicolor/scalable/mimetypes/application-x-vnmf.svg "$ICONS/scalable/mimetypes/application-x-vnmf.svg"
install -Dm644 data/org.vnmf.Reader.metainfo.xml "$SHARE/metainfo/org.vnmf.Reader.metainfo.xml"
install -Dm644 data/vnmf-mime.xml "$SHARE/mime/packages/vnmf.xml"
install_exec data/vnmf.thumbnailer "$SHARE/thumbnailers/vnmf.thumbnailer"
# Interface translations (po/<lang>.po → share/locale/<lang>/LC_MESSAGES/vnmf.mo).
if command -v msgfmt >/dev/null; then
    po/build.sh "$(realpath -m "$SHARE/locale")"
else
    echo "warning: msgfmt (gettext) not found — the interface stays in English" >&2
fi
refresh

# Default application. Without this GNOME picks the alphabetically first one — VNMF Info.
# For yourself — ~/.config/mimeapps.list; for the system — the prefix's mimeapps.list (the default
# for all users, which everyone can change). The applications themselves also make VNMF Reader
# the default on first launch if the user hasn't chosen anything.
if [[ $SET_DEFAULT -eq 1 ]]; then
    if [[ $USER_MODE -eq 1 ]]; then
        command -v xdg-mime >/dev/null && xdg-mime default org.vnmf.Reader.desktop application/x-vnmf
    else
        list="$SHARE/applications/mimeapps.list"
        line="application/x-vnmf=org.vnmf.Reader.desktop;org.vnmf.Info.desktop;"
        if [[ -f "$list" ]] && grep -q '^\[Default Applications\]' "$list"; then
            sed -i '/^application\/x-vnmf=/d' "$list"
            sed -i "/^\[Default Applications\]/a $line" "$list"
        else
            printf '[Default Applications]\n%s\n' "$line" >> "$list"
        fi
    fi
fi

# File managers remember failed thumbnails — reset them so .vnmf ones are regenerated.
owner_home="$HOME"
[[ $EUID -eq 0 && -n "${SUDO_USER:-}" ]] && owner_home="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
rm -rf "$owner_home/.cache/thumbnails/fail"

echo "VNMF installed to $PREFIX"
if [[ $SYSTEM -eq 0 && ":$PATH:" != *":$BIN:"* ]]; then
    echo "warning: $BIN is not in PATH — add it to run the console tools"
fi
if [[ "$BIN" != /usr/* ]]; then
    # gnome-desktop runs thumbnailers in a bwrap sandbox where only /usr is visible.
    echo "warning: in GNOME (Nautilus) thumbnails only work with the program under /usr —"
    echo "         for thumbnails install system-wide: sudo $0 --system"
fi
echo "Thumbnails: restart the file manager (nautilus -q)."
