#!/bin/sh
# Readies a fresh Arch Linux (archlinux:base-devel) or Arch Linux ARM container
# and installs the packages given as arguments.
set -eu
# pacman's download sandbox needs kernel features containers often lack.
grep -q '^DisableSandbox' /etc/pacman.conf || sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf
if [ "$(uname -m)" = aarch64 ]; then
  pacman-key --init >/dev/null
  pacman-key --populate archlinuxarm >/dev/null
  # The ARM root filesystem ships a kernel; a container doesn't need one.
  pacman -Rdd --noconfirm linux-aarch64 linux-firmware >/dev/null 2>&1 || true
fi
pacman -Syu --noconfirm --needed "$@" >/dev/null
