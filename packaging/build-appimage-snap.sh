#!/bin/sh
# Builds Spotty's AppImage and its classic snap inside
# docker.io/library/rust:1-trixie (x86_64 or aarch64). Both carry the same
# linuxdeploy AppDir. Run from the source root; they land in dist/.
set -eu
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
export DEBIAN_FRONTEND=noninteractive APPIMAGE_EXTRACT_AND_RUN=1
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  libgtk-4-dev libadwaita-1-dev libdav1d-dev cmake g++ pkg-config gettext \
  file wget ca-certificates libtesseract5 tesseract-ocr-eng poppler-utils \
  librsvg2-common libgdk-pixbuf2.0-bin desktop-file-utils patchelf squashfs-tools \
  snapd >/dev/null
ARCH=$(uname -m)
MULTIARCH=$(dpkg-architecture -qDEB_HOST_MULTIARCH 2>/dev/null || echo "$ARCH-linux-gnu")
SNAP_ARCH=$(dpkg --print-architecture)
OUT=$PWD/dist

rm -rf /build; cp -a . /build
cd /build
printf 'set(CMAKE_DISABLE_FIND_PACKAGE_%s ON)\n' AOM > no-aom.cmake
CMAKE_TOOLCHAIN_FILE="$PWD/no-aom.cmake" cargo build --release --locked

# Downloaded tools live apart from the source and the AppDir.
rm -rf /tools; mkdir -p /tools && cd /tools
wget -q https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-$ARCH.AppImage
wget -q https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh
chmod +x linuxdeploy-$ARCH.AppImage linuxdeploy-plugin-gtk.sh

cd /build
A=/build/AppDir
for po in po/*.po; do
  lang=$(basename "$po" .po)
  mkdir -p "$A/usr/share/locale/$lang/LC_MESSAGES"
  msgfmt --check -o "$A/usr/share/locale/$lang/LC_MESSAGES/spotty.mo" "$po"
done
install -Dm644 data/metainfo/com.spotty.Spotty.metainfo.xml $A/usr/share/metainfo/com.spotty.Spotty.appdata.xml
install -Dm644 data/icons/hicolor/scalable/actions/spotty-store-symbolic.svg $A/usr/share/icons/hicolor/scalable/actions/spotty-store-symbolic.svg
install -Dm644 /usr/share/tesseract-ocr/5/tessdata/eng.traineddata $A/usr/share/tessdata/eng.traineddata
# Spotty dlopens libtesseract.so.5.5; Debian's soname is libtesseract.so.5.
mkdir -p $A/usr/lib
cp -L /usr/lib/$MULTIARCH/libtesseract.so.5 $A/usr/lib/libtesseract.so.5.5

# Runs after linuxdeploy-plugin-gtk's hook (hooks are sourced in name order).
mkdir -p $A/apprun-hooks
cat > $A/apprun-hooks/zz-spotty.sh <<'EOF'
export TESSDATA_PREFIX="$APPDIR/usr/share/tessdata"
export PATH="$APPDIR/usr/bin:$PATH"
# Spotty runs the host's gsettings and apps: a schema dir holding only the
# bundled schemas would hide GNOME's keybinding schemas from them. The bundled
# schemas stay reachable through XDG_DATA_DIRS.
unset GSETTINGS_SCHEMA_DIR
# Keep GNOME's native Wayland session.
unset GDK_BACKEND
# Opening the AppImage (or the snap's launcher) shows Settings, like the
# desktop entry does.
if [ $# -eq 0 ]; then
  set -- --settings
fi
EOF

export DEPLOY_GTK_VERSION=4 LINUXDEPLOY_OUTPUT_VERSION=$VER OUTPUT=Spotty-$VER-$ARCH.AppImage
PATH=/tools:$PATH /tools/linuxdeploy-$ARCH.AppImage --appdir $A \
  --executable target/release/spotty \
  --executable target/release/spotty-toggle \
  --executable /usr/bin/pdftoppm \
  --executable /usr/bin/pdftotext \
  --library $A/usr/lib/libtesseract.so.5.5 \
  --desktop-file data/applications/com.spotty.Spotty.desktop \
  --icon-file data/icons/hicolor/512x512/apps/com.spotty.Spotty.png \
  --plugin gtk \
  --output appimage
grep -q 'zz-spotty.sh' $A/AppRun
mkdir -p "$OUT"
cp Spotty-$VER-$ARCH.AppImage "$OUT/"

# ── Snap: the same AppDir, classic confinement (Spotty launches host apps,
# edits GNOME shortcuts and reads the whole home folder, which a strictly
# confined snap can't do).
S=/build/snap-root
rm -rf $S; cp -a $A $S
mkdir -p $S/meta/gui
cp data/icons/hicolor/512x512/apps/com.spotty.Spotty.png $S/meta/gui/icon.png
sed 's|^Icon=.*|Icon=${SNAP}/meta/gui/icon.png|' data/applications/com.spotty.Spotty.desktop > $S/meta/gui/spotty.desktop
cat > $S/meta/snap.yaml <<EOF
name: spotty
version: '$VER'
summary: Raycast-style launcher for GNOME
description: |
  Search apps, files, clipboard history, calculations and system actions
  from one keyboard-driven window.
license: Apache-2.0
architectures: [$SNAP_ARCH]
base: core24
confinement: classic
grade: stable
apps:
  spotty:
    command: AppRun
EOF
snap pack $S "$OUT"
ls -la "$OUT"
