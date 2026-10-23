#!/bin/sh
# Builds the Flatpak repository that users add with `flatpak remote-add`:
# every *.flatpak bundle in BUNDLE_DIR (one per architecture) is imported into
# a fresh, signed OSTree repository, next to the .flatpakrepo and .flatpakref
# files that point at it and a small page explaining how to install.
#
# Usage: build-flatpak-repo.sh BUNDLE_DIR OUT_DIR GPG_KEY_ID
# GNUPGHOME must hold the secret key. Publish OUT_DIR at $SPOTTY_REPO_URL
# (default: the project's GitHub Pages site).
#
# The release workflow runs this on every v* tag. One-time GitHub setup:
#   - Settings -> Pages -> Source: GitHub Actions
#   - the signing key as the FLATPAK_GPG_PRIVATE_KEY secret:
#       gpg --armor --export-secret-keys <key id> | gh secret set FLATPAK_GPG_PRIVATE_KEY
#     Its public half is flatpak-repo/spotty-repo.asc. Keep the key: users'
#     remotes only accept updates signed with it.
set -eu
BUNDLES=$1
OUT=$2
KEY=$3
URL=${SPOTTY_REPO_URL:-https://aras1907.github.io/Spotty}
HOME_PAGE=https://github.com/Aras1907/Spotty
HERE=$(dirname "$0")

rm -rf "$OUT"
mkdir -p "$OUT"
ostree init --mode=archive-z2 --repo="$OUT/repo"
found=0
for bundle in "$BUNDLES"/*.flatpak; do
  [ -e "$bundle" ] || continue
  echo "== importing $bundle"
  flatpak build-import-bundle --no-update-summary --gpg-sign="$KEY" "$OUT/repo" "$bundle"
  found=$((found + 1))
done
[ "$found" -gt 0 ] || { echo "no .flatpak bundles in $BUNDLES" >&2; exit 1; }

# Summary, AppStream branches (so GNOME Software can show Spotty) and
# from-scratch deltas (fewer, larger downloads on install).
flatpak build-update-repo \
  --title="Spotty" \
  --comment="Fast launcher for apps, files, clipboard history and system actions" \
  --homepage="$HOME_PAGE" \
  --icon="$URL/com.spotty.Spotty.png" \
  --default-branch=master \
  --gpg-sign="$KEY" \
  --generate-static-deltas \
  --prune \
  "$OUT/repo"
rm -rf "$OUT/repo/tmp"
ostree summary --view --repo="$OUT/repo" | grep -E '^\* app/' || true

GPGKEY=$(gpg --export "$KEY" | base64 -w0)
cat > "$OUT/spotty.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Spotty
Url=$URL/repo/
Homepage=$HOME_PAGE
Comment=Fast launcher for apps, files, clipboard history and system actions
Description=Updates for Spotty, straight from its developer.
Icon=$URL/com.spotty.Spotty.png
GPGKey=$GPGKEY
EOF
cat > "$OUT/com.spotty.Spotty.flatpakref" <<EOF
[Flatpak Ref]
Name=com.spotty.Spotty
Branch=master
Title=Spotty
Url=$URL/repo/
SuggestRemoteName=spotty
RuntimeRepo=https://dl.flathub.org/repo/flathub.flatpakrepo
IsRuntime=false
Homepage=$HOME_PAGE
Icon=$URL/com.spotty.Spotty.png
GPGKey=$GPGKEY
EOF
gpg --armor --export "$KEY" > "$OUT/spotty-repo.asc"
cp "$HERE/../data/icons/hicolor/512x512/apps/com.spotty.Spotty.png" "$OUT/"
sed "s|@URL@|$URL|g" "$HERE/flatpak-repo/index.html" > "$OUT/index.html"
# Served as-is: no Jekyll processing of the repository files.
touch "$OUT/.nojekyll"
echo "== repository in $OUT"
