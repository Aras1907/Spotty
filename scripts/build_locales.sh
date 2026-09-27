#!/usr/bin/env bash
# Compile po/<lang>.po into spotty.mo.
#
#   scripts/build_locales.sh            → po/locale/<lang>/LC_MESSAGES/spotty.mo
#   scripts/build_locales.sh --install  → also copy into ~/.local/share/locale
#                                         (where src/i18n.rs looks on a
#                                         non-Flatpak install)
set -euo pipefail
cd "$(dirname "$0")/.."

shopt -s nullglob
for po in po/*.po; do
    lang="$(basename "$po" .po)"
    out="po/locale/$lang/LC_MESSAGES"
    mkdir -p "$out"
    msgfmt --check --statistics -o "$out/spotty.mo" "$po"
    echo "compiled po/$lang.po → $out/spotty.mo"
done

if [ "${1:-}" = "--install" ]; then
    dest="${HOME}/.local/share/locale"
    mkdir -p "$dest"
    cp -r po/locale/* "$dest/"
    echo "installed into $dest"
fi
