---
name: flatpak-build-deploy
description: How to build/compile-check Spotty from this sandboxed session via flatpak-spawn --host
metadata:
  type: reference
---

This session runs inside the `com.vscodium.codium` flatpak, so the host has no `gtk4.pc` — a plain `cargo build` fails at `gdk4-sys` (pkg-config can't find gtk4). Build through the GNOME SDK 50 + rust-stable extension instead.

Fast compile-check (verified, ~3s incremental; reuses the local `./target` and the existing cargo cache):

```sh
cd /home/aras/development/spotty-v6-src
flatpak-spawn --host flatpak run --user --filesystem=home \
  --env=CARGO_HOME=/home/aras/.var/app/com.vscodium.codium/data/cargo \
  --command=sh org.gnome.Sdk/x86_64/50 -c \
  'export PATH=/usr/lib/sdk/rust-stable/bin:$PATH; cargo build'
```

Notes: pass `--user` and the fully-qualified ref `org.gnome.Sdk/x86_64/50` (both user+system copies are installed, so a bare `org.gnome.Sdk//50` prompts interactively and aborts). Add `--share=network` if crates need downloading.

Full Flatpak build/install (from [CLAUDE.md](../CLAUDE.md)):

```sh
flatpak-builder --user --install --force-clean build-dir flatpak/com.spotty.Spotty.yaml
```
