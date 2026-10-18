#!/bin/sh
# Builds Spotty's .deb inside docker.io/library/rust:1-trixie (amd64 or arm64).
# Run from the source root; the package lands in dist/.
set -eu
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  libgtk-4-dev libadwaita-1-dev libdav1d-dev cmake g++ pkg-config gettext \
  dpkg-dev file >/dev/null
ARCH=$(dpkg --print-architecture)
MULTIARCH=$(dpkg-architecture -qDEB_HOST_MULTIARCH)
OUT=$PWD/dist

rm -rf /build; cp -a . /build
cd /build
# No AOM (as in the Flatpak). No libjpeg/OpenJPEG/sharpyuv either: Debian's
# libjpeg.so.62 is libjpeg.so.8 on Ubuntu, and libheif only needs them for
# rare JPEG/JPEG 2000-in-HEIF images.
printf 'set(CMAKE_DISABLE_FIND_PACKAGE_%s ON)\n' AOM JPEG OpenJPEG libsharpyuv > no-aom.cmake
# Debian names tesseract libtesseract.so.5, but Spotty dlopens
# libtesseract.so.5.5: a private symlink in /usr/lib/spotty (found through the
# binary's RUNPATH) bridges that without touching the system libdir.
CMAKE_TOOLCHAIN_FILE="$PWD/no-aom.cmake" \
RUSTFLAGS="-C link-arg=-Wl,-rpath,/usr/lib/spotty" \
  cargo build --release --locked

P=/pkg
rm -rf $P; mkdir -p $P/DEBIAN
install -Dm755 target/release/spotty $P/usr/bin/spotty
install -Dm755 target/release/spotty-toggle $P/usr/bin/spotty-toggle
install -Dm644 data/applications/com.spotty.Spotty.desktop $P/usr/share/applications/com.spotty.Spotty.desktop
install -Dm644 data/metainfo/com.spotty.Spotty.metainfo.xml $P/usr/share/metainfo/com.spotty.Spotty.metainfo.xml
install -Dm644 data/icons/hicolor/512x512/apps/com.spotty.Spotty.png $P/usr/share/icons/hicolor/512x512/apps/com.spotty.Spotty.png
install -Dm644 data/icons/hicolor/scalable/actions/spotty-store-symbolic.svg $P/usr/share/icons/hicolor/scalable/actions/spotty-store-symbolic.svg
for po in po/*.po; do
  lang=$(basename "$po" .po)
  mkdir -p "$P/usr/share/locale/$lang/LC_MESSAGES"
  msgfmt --check -o "$P/usr/share/locale/$lang/LC_MESSAGES/spotty.mo" "$po"
done
mkdir -p $P/usr/lib/spotty
ln -s /usr/lib/$MULTIARCH/libtesseract.so.5 $P/usr/lib/spotty/libtesseract.so.5.5
# Spotty looks for tessdata in the Fedora/Arch/Flatpak locations; point it at
# Debian's for the whole session.
install -Dm644 /dev/stdin $P/usr/lib/environment.d/60-spotty.conf <<'EOF'
TESSDATA_PREFIX=/usr/share/tesseract-ocr/5/tessdata
EOF
install -Dm644 LICENSE $P/usr/share/doc/spotty/copyright

# Shared-library dependencies, computed rather than hand-written.
rm -rf /shl; mkdir -p /shl/debian; cd /shl
printf 'Source: spotty\n\nPackage: spotty\nArchitecture: %s\n' "$ARCH" > debian/control
DEPS=$(dpkg-shlibdeps -O $P/usr/bin/spotty $P/usr/bin/spotty-toggle | sed 's/^shlibs:Depends=//')
SIZE=$(du -sk --exclude=DEBIAN $P | cut -f1)
cat > $P/DEBIAN/control <<EOF
Package: spotty
Version: $VER
Architecture: $ARCH
Maintainer: Aras1907 <45166762+Aras1907@users.noreply.github.com>
Installed-Size: $SIZE
Depends: $DEPS
Recommends: libglib2.0-bin, libtesseract5, tesseract-ocr-eng, poppler-utils, ffmpeg
Section: utils
Priority: optional
Homepage: https://github.com/Aras1907/Spotty
Description: Raycast-style launcher for GNOME
 Search apps, files, clipboard history, calculations and system actions
 from one keyboard-driven window.
EOF
mkdir -p "$OUT"
dpkg-deb --root-owner-group -Zxz --build $P "$OUT/spotty_${VER}_${ARCH}.deb"
dpkg-deb --info "$OUT/spotty_${VER}_${ARCH}.deb"
