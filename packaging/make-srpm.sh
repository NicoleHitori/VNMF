#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Prepares RPM sources: an archive of the working tree (without target/ and .git) + Rust dependencies (cargo vendor),
# and builds a src.rpm. Requires: rpm-build (sudo dnf install rpm-build rpmdevtools).
#
#   packaging/make-srpm.sh            # → ~/rpmbuild/SRPMS/vnmf-*.src.rpm
#   packaging/make-srpm.sh --rpm      # also build the binary RPM right away

set -euo pipefail
cd "$(dirname "$0")/.."
NAME=vnmf
VERSION=$(sed -n 's/^Version: *//p' packaging/vnmf.spec)
TOP=$(rpm --eval '%{_topdir}')
mkdir -p "$TOP"/{SOURCES,SPECS,SRPMS}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# The working tree as is (including uncommitted changes), without build artifacts and test samples.
tar --exclude=./target --exclude=./.git --exclude=./vendor --exclude=./tests \
    --transform "s|^\.|$NAME-$VERSION|" -czf "$TOP/SOURCES/$NAME-$VERSION.tar.gz" .

cargo vendor --locked --versioned-dirs "$WORK/vendor" >/dev/null
tar -C "$WORK" -czf "$TOP/SOURCES/$NAME-$VERSION-vendor.tar.gz" vendor

cp packaging/vnmf.spec "$TOP/SPECS/"
if [[ "${1:-}" == "--rpm" ]]; then
    rpmbuild -ba "$TOP/SPECS/vnmf.spec"
    echo "RPM: $TOP/RPMS/$(uname -m)/"
else
    rpmbuild -bs "$TOP/SPECS/vnmf.spec"
fi
