# SPDX-License-Identifier: GPL-3.0-or-later
# RPM for Fedora. Build: packaging/make-srpm.sh (prepares sources and Rust dependencies offline),
# then `rpmbuild --rebuild vnmf-*.src.rpm`, or in COPR / mock.

Name:           vnmf
Version:        0.1.0~beta
Release:        1%{?dist}
Summary:        VNMF manga/webtoon container: CLI tools, reader, inspector, creator, media guard
License:        GPL-3.0-or-later
Source0:        %{name}-%{version}.tar.gz
# Rust dependencies (`cargo vendor`) — offline build, as mock and COPR require.
Source1:        %{name}-%{version}-vendor.tar.gz

BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  pkgconfig(gtk4) >= 4.22
BuildRequires:  pkgconfig(libadwaita-1) >= 1.8
BuildRequires:  desktop-file-utils
BuildRequires:  gettext
BuildRequires:  libappstream-glib

Requires:       hicolor-icon-theme
Requires:       shared-mime-info
# WebP, AVIF, JPEG XL in the reader and thumbnails — via glycin.
Recommends:     glycin-loaders

%description
VNMF is a container format for manga, comics and webtoons with CRC integrity checks,
Reed-Solomon redundancy and recovery. This package provides the command-line tools
(vnmfinfo, vnmfpack, vnmfunpack, vnmfrecovery, vnmfbadtest, vnmfguard), the GTK
applications VNMF Reader, VNMF Info, VNMF Creator and VNMF Guard, a file type definition
and a thumbnailer for file managers. VNMF Guard (vnmfguard) embeds the same Reed-Solomon
redundancy invisibly into videos, music and disk images (mkv, webm, mp4, flac, ogg, opus,
mp3, zst, gz, iso) and repairs them.

%prep
%autosetup -n %{name}-%{version}
tar -xzf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<'CFG'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
CFG

%build
export RUSTFLAGS="%{build_rustflags}"
cargo build --release --frozen --workspace --bins

%install
for b in vnmfinfo vnmfpack vnmfunpack vnmfrecovery vnmfbadtest vnmfguard \
         vnmf-reader vnmf-info vnmf-creator vnmf-guard vnmf-thumbnailer; do
    install -Dpm755 target/release/$b %{buildroot}%{_bindir}/$b
done
for a in org.vnmf.Reader org.vnmf.Info org.vnmf.Creator org.vnmf.Guard; do
    desktop-file-install --dir=%{buildroot}%{_datadir}/applications data/$a.desktop
    install -Dpm644 data/icons/hicolor/scalable/apps/$a.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/$a.svg
done
install -Dpm644 data/icons/hicolor/symbolic/apps/org.vnmf.Reader-symbolic.svg \
    %{buildroot}%{_datadir}/icons/hicolor/symbolic/apps/org.vnmf.Reader-symbolic.svg
install -Dpm644 data/icons/hicolor/scalable/mimetypes/application-x-vnmf.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/mimetypes/application-x-vnmf.svg
install -Dpm644 data/vnmf-mime.xml %{buildroot}%{_datadir}/mime/packages/vnmf.xml
install -Dpm644 data/vnmf.thumbnailer %{buildroot}%{_datadir}/thumbnailers/vnmf.thumbnailer
install -Dpm644 data/org.vnmf.Reader.metainfo.xml %{buildroot}%{_metainfodir}/org.vnmf.Reader.metainfo.xml
# Interface translations: po/<lang>.po → %{_datadir}/locale/<lang>/LC_MESSAGES/vnmf.mo
po/build.sh %{buildroot}%{_datadir}/locale
%find_lang vnmf

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/*.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/*.metainfo.xml
cargo test --release --frozen --workspace

# Fedora refreshes the MIME, desktop entry and icon caches itself (file triggers) — no scriptlets needed.

%files -f vnmf.lang
%license LICENSE
%doc FORMAT.md
%{_bindir}/vnmf*
%{_datadir}/applications/org.vnmf.*.desktop
%{_datadir}/icons/hicolor/scalable/apps/org.vnmf.*.svg
%{_datadir}/icons/hicolor/symbolic/apps/org.vnmf.Reader-symbolic.svg
%{_datadir}/icons/hicolor/scalable/mimetypes/application-x-vnmf.svg
%{_datadir}/mime/packages/vnmf.xml
%{_datadir}/thumbnailers/vnmf.thumbnailer
%{_metainfodir}/org.vnmf.Reader.metainfo.xml

%changelog
* Wed Nov 04 2026 void_nicole (Nicole Hitori) - 0.1.0~beta-1
- VNMF format 2.0: aligned structures, attachments table (music, effects, text, dark versions),
  self-describing parity, recovery without both headers; 1.x files are readable
- Creator: opening and editing .vnmf, custom preview, page attachments
- Reader: music and effects playback, page text, dark versions
