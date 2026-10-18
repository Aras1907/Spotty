#!/bin/sh
# Installs the package just built into dist/ in a throwaway container, then
# runs smoke.sh against it. Usage: smoke-in-container.sh deb|rpm|arch
set -eu
case $1 in
  deb)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq ./dist/spotty_*.deb xvfb xauth x11-utils dbus >/dev/null ;;
  rpm)
    dnf -y -q install ./dist/spotty-*.rpm xorg-x11-server-Xvfb xorg-x11-xauth xwininfo dbus-daemon >/dev/null ;;
  arch)
    sh packaging/arch-prepare.sh xorg-server-xvfb xorg-xauth xorg-xwininfo dbus
    pacman -U --noconfirm dist/spotty-*.pkg.tar.zst >/dev/null ;;
esac
echo "== missing libraries:"; ldd /usr/bin/spotty | grep 'not found' || echo none
export HOME=/tmp/smoke-home; mkdir -p "$HOME"
sh packaging/smoke.sh spotty
