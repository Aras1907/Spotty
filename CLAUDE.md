# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
# Development build
cargo build

# Release build (optimized: LTO, single codegen unit, stripped binary)
cargo release

# Run directly
cargo run -- [--toggle] [--daemon] [--files]

# Flatpak build
#flatpak-builder --user --install --force-clean build-dir flatpak/com.spotty.Spotty.yaml
```

There are no test suites or lint configurations in this project.

### CLI Flags

`src/main.rs` defines: `--toggle`/`-t` (show/hide the search window), `--quit`/`-q` (exit the daemon), `--daemon`/`-d` (start hidden, in the background), `--settings`/`-s` (open settings), `--clipboard`/`-c` (open directly in clipboard mode).

## Architecture Overview

Spotty is a Raycast-style launcher for GNOME Linux, built in Rust with GTK4/libadwaita. It runs as a persistent daemon and shows a search window on demand.

### Core Data Flow

User types in search box → `ui/search_window.rs` debounces input → `search/mod.rs` dispatches to backends → results rendered via `ui/result_row.rs`. The search dispatcher selects backends based on active **trigger mode** (a keyword like "files", "pdf", "clip") or falls back to universal search.

### Major Subsystems

**Indexer** ([src/index.rs](src/index.rs)) — Runs on a background thread. Maintains a `Snapshot` (RwLock-protected) of indexed apps and files. App index refreshes every 300s; file index is lazy (built on first file-mode activation). A filesystem watcher monitors Downloads, Documents, Desktop, etc. for incremental updates.

**Search backends** ([src/search/](src/search/)) — Each backend is independent:
- `apps.rs` — fuzzy app matching via `nucleo-matcher`
- `files.rs` — fuzzy file matching against the index
- `browse.rs` — live path exploration (e.g., `~/`, `/usr/`)
- `calculator.rs` — math expression evaluation
- `clipboard.rs` — clipboard history search
- `web.rs` — generates search URLs for configured engines
- `system.rs` / `settings_panels.rs` — GNOME system actions and control center panels
- `typo.rs` — typo/fuzzy fallback

**Preview** ([src/preview.rs](src/preview.rs)) — Async previews: images (GTK Picture), text (monospace TextView), PDFs (bundled `pdftoppm`), Office files (zip-extracted embedded thumbnails — no external tool), video (bundled FFmpeg/ffmpegthumbnailer).

**Key synthesis** ([src/keysynth.rs](src/keysynth.rs)) — Synthesizes keyboard accelerators (e.g. Ctrl+V) on the focused window via `xdotool`/`wtype`, used to auto-paste after a clipboard-history pick. Runs on the host through `flatpak-spawn --host` when sandboxed.

**Global shortcuts** ([src/app.rs](src/app.rs)) — `register_all_keybindings`/`register_slot` write GNOME custom-keybindings via `gsettings` (through `flatpak-spawn --host` when sandboxed) so each trigger keyword can carry its own global shortcut (e.g. Super+Ctrl+F to open in files mode).

**Config** ([src/config.rs](src/config.rs)) — JSON file. Defines trigger keywords (with associated file extensions, icons, shortcuts), custom terminal commands, search engines (7 built-in + custom URL support), and feature flags.

**Background operations** ([src/operations.rs](src/operations.rs)) — Tracks long-running install/uninstall jobs in a global registry so they keep running and reporting progress (as live rows) even while the search window is hidden.

**File operations** ([src/fileops.rs](src/fileops.rs)) — Copy/cut/paste for selected results, with pending state kept in a thread-local so it survives the window being hidden.

**History** ([src/history.rs](src/history.rs)) — Tracks per-query selection counts to power autocompletion suggestions.

### Threading Model

Single GTK main thread + one background indexer thread. Data shared via `Arc<RwLock<Snapshot>>`. Uses the `futures` crate for async work (not tokio).

### Key Technologies

| Concern | Crate/Tool |
|---|---|
| GUI | `gtk4`, `libadwaita` |
| Fuzzy matching | `nucleo-matcher` |
| File walking | `ignore` (respects .gitignore) |
| Serialization | `serde` + `serde_json` |
| Image handling | `image`, `cairo-rs`, `gdk-pixbuf` |
| Office file previews | `zip`, `cfb` |
| File type detection | `infer` |
| Distribution | Flatpak (GNOME Platform runtime 50) |

### Application State

`AppState` in [src/app.rs](src/app.rs) is the root object holding config, the indexer handle, clipboard manager, and window references. It is passed around via `Rc<RefCell<AppState>>` on the main thread.
