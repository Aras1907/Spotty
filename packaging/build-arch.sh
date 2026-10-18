#!/bin/sh
# Builds Spotty's Arch Linux package inside archlinux:base-devel (x86_64) or an
# Arch Linux ARM root filesystem (aarch64). Run from the source root as root;
# the package lands in dist/.
set -eu
VER=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
OUT=$PWD/dist

sh packaging/arch-prepare.sh base-devel rust cmake gtk4 libadwaita dav1d gettext

useradd -m builder 2>/dev/null || true
rm -rf /build; mkdir -p /build
cp -a . /build/spotty-src
rm -rf /build/spotty-src/dist /build/spotty-src/target
cat > /build/PKGBUILD <<EOF
pkgname=spotty
pkgver=$VER
pkgrel=1
pkgdesc='Raycast-style launcher for GNOME'
arch=('x86_64' 'aarch64')
url='https://github.com/Aras1907/Spotty'
license=('Apache-2.0')
depends=('gtk4' 'libadwaita' 'dav1d' 'glib2' 'cairo' 'pango' 'gdk-pixbuf2' 'graphene' 'gcc-libs' 'glibc')
makedepends=('rust' 'cmake' 'gettext')
optdepends=('tesseract: text search inside images'
            'tesseract-data-eng: English OCR data'
            'poppler: PDF previews'
            'ffmpeg: video previews')
options=('!lto' '!debug')

build() {
  cd "\$startdir/spotty-src"
  # No AOM (as in the Flatpak), and no libjpeg/OpenJPEG/sharpyuv in libheif:
  # only rare JPEG-in-HEIF images need them.
  printf 'set(CMAKE_DISABLE_FIND_PACKAGE_%s ON)\n' AOM JPEG OpenJPEG libsharpyuv > no-aom.cmake
  CMAKE_TOOLCHAIN_FILE="\$PWD/no-aom.cmake" cargo build --release --locked
}

package() {
  cd "\$startdir/spotty-src"
  install -Dm755 target/release/spotty "\$pkgdir/usr/bin/spotty"
  install -Dm755 target/release/spotty-toggle "\$pkgdir/usr/bin/spotty-toggle"
  install -Dm644 data/applications/com.spotty.Spotty.desktop "\$pkgdir/usr/share/applications/com.spotty.Spotty.desktop"
  install -Dm644 data/metainfo/com.spotty.Spotty.metainfo.xml "\$pkgdir/usr/share/metainfo/com.spotty.Spotty.metainfo.xml"
  install -Dm644 data/icons/hicolor/512x512/apps/com.spotty.Spotty.png "\$pkgdir/usr/share/icons/hicolor/512x512/apps/com.spotty.Spotty.png"
  install -Dm644 data/icons/hicolor/scalable/actions/spotty-store-symbolic.svg "\$pkgdir/usr/share/icons/hicolor/scalable/actions/spotty-store-symbolic.svg"
  for po in po/*.po; do
    lang=\$(basename "\$po" .po)
    mkdir -p "\$pkgdir/usr/share/locale/\$lang/LC_MESSAGES"
    msgfmt --check -o "\$pkgdir/usr/share/locale/\$lang/LC_MESSAGES/spotty.mo" "\$po"
  done
  install -Dm644 LICENSE "\$pkgdir/usr/share/licenses/spotty/LICENSE"
}
EOF
chown -R builder /build
# Arch Linux ARM's makepkg.conf picks another package extension.
su builder -c 'cd /build && PKGDEST=/build/out PKGEXT=.pkg.tar.zst makepkg --nodeps --noconfirm -f'
mkdir -p "$OUT"
cp /build/out/spotty-$VER-1-*.pkg.tar.zst "$OUT/"
pacman -Qip "$OUT"/spotty-$VER-1-*.pkg.tar.zst | grep -E '^(Architecture|Depends On|Installed Size)'
