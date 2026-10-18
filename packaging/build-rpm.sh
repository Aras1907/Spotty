#!/bin/sh
# Builds Spotty's .rpm inside registry.fedoraproject.org/fedora:44 (x86_64 or
# aarch64). Run from the source root; the package lands in dist/.
set -eu
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
dnf -y -q install rust cargo gtk4-devel libadwaita-devel libdav1d-devel cmake \
  gcc-c++ pkgconf-pkg-config gettext rpm-build tar >/dev/null
OUT=$PWD/dist

mkdir -p ~/rpmbuild/SOURCES ~/rpmbuild/SPECS
tar --exclude=./dist --exclude=./target --exclude=./.git -czf ~/rpmbuild/SOURCES/spotty-$VER.tar.gz \
  --transform "s,^\.,spotty-$VER," .
cat > ~/rpmbuild/SPECS/spotty.spec <<EOF
Name:           spotty
Version:        $VER
Release:        1%{?dist}
Summary:        Raycast-style launcher for GNOME
License:        Apache-2.0
URL:            https://github.com/Aras1907/Spotty
Source0:        spotty-%{version}.tar.gz
Recommends:     tesseract tesseract-langpack-eng poppler-utils ffmpeg-free
%global debug_package %{nil}

%description
Search apps, files, clipboard history, calculations and system actions
from one keyboard-driven window.

%prep
%autosetup

%build
printf 'set(CMAKE_DISABLE_FIND_PACKAGE_AOM ON)\n' > no-aom.cmake
CMAKE_TOOLCHAIN_FILE="\$PWD/no-aom.cmake" cargo build --release --locked

%install
install -Dm755 target/release/spotty %{buildroot}%{_bindir}/spotty
install -Dm755 target/release/spotty-toggle %{buildroot}%{_bindir}/spotty-toggle
install -Dm644 data/applications/com.spotty.Spotty.desktop %{buildroot}%{_datadir}/applications/com.spotty.Spotty.desktop
install -Dm644 data/metainfo/com.spotty.Spotty.metainfo.xml %{buildroot}%{_metainfodir}/com.spotty.Spotty.metainfo.xml
install -Dm644 data/icons/hicolor/512x512/apps/com.spotty.Spotty.png %{buildroot}%{_datadir}/icons/hicolor/512x512/apps/com.spotty.Spotty.png
install -Dm644 data/icons/hicolor/scalable/actions/spotty-store-symbolic.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/actions/spotty-store-symbolic.svg
for po in po/*.po; do
  lang=\$(basename "\$po" .po)
  mkdir -p "%{buildroot}%{_datadir}/locale/\$lang/LC_MESSAGES"
  msgfmt --check -o "%{buildroot}%{_datadir}/locale/\$lang/LC_MESSAGES/spotty.mo" "\$po"
done
%find_lang spotty

%files -f spotty.lang
%license LICENSE
%{_bindir}/spotty
%{_bindir}/spotty-toggle
%{_datadir}/applications/com.spotty.Spotty.desktop
%{_metainfodir}/com.spotty.Spotty.metainfo.xml
%{_datadir}/icons/hicolor/512x512/apps/com.spotty.Spotty.png
%{_datadir}/icons/hicolor/scalable/actions/spotty-store-symbolic.svg
EOF
rpmbuild -bb ~/rpmbuild/SPECS/spotty.spec
mkdir -p "$OUT"
cp ~/rpmbuild/RPMS/*/spotty-$VER-1.*.rpm "$OUT/"
rpm -qp --requires "$OUT"/spotty-$VER-1.*.rpm | grep -v "^rpmlib\|GLIBC\|GLIBCXX\|CXXABI\|GCC_" || true
