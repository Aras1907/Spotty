# AGENTS.md

Spotty — a Raycast-style launcher for GNOME Linux, in Rust with GTK4/libadwaita.

## Rules: no third-party extensions or CLIs, never LibreOffice

- **No GNOME Shell extensions and no third-party CLI tools, ever.** Don't add,
  rely on, or propose a GNOME Shell extension (Spotty's own included), and
  don't add a dependency on an external command-line tool that isn't already
  bundled or part of the base system. Solve window behavior (placement,
  animation, focus) inside the app with GTK/libadwaita and its own code;
  if that can't do it, say so instead of reaching for an extension.
- **No LibreOffice, ever.** Office previews and thumbnails must stay
  native/pure Rust (zip/cfb/calamine/cairo) or use the bundled tools in
  `flatpak/com.spotty.Spotty.yaml`. Never add a dependency on LibreOffice or
  `soffice`/`libreoffice` conversions.

## Build & Run

```bash
cargo build              # dev build
cargo run                # opens the search window
cargo run -- --toggle    # toggle search window (uses PID file + SIGUSR1 IPC)
cargo run -- --daemon    # start hidden in background
cargo build --release    # release build (LTO, single codegen unit, stripped)
```

No test suite, no linter config, no formatter config. `cargo build` is the only verification.

### Dev dependencies

AVIF OCR requires `dav1d` (the AV1 decoder) at build and runtime:

- Fedora: `sudo dnf install dav1d-devel`
- Debian/Ubuntu: `sudo apt install libdav1d-dev`

Flatpak bundles dav1d, so no host package is needed for the Flatpak build.

## Architecture: threading & state

- **Single GTK main thread** + one background indexer thread.
- `AppState` (root state object) is stored in a `thread_local!` as `RefCell<Option<AppState>>`. Passed around as `Rc<RefCell<…>>` on the main thread. Not `Arc`.
- The indexer's `Snapshot` is shared via `Arc<RwLock<Snapshot>>` between threads.
- Async work uses `futures` crate, **not tokio**.

## IPC: daemon toggle

`main.rs` implements a single-instance daemon using a PID file (`~/.config/spotty/spotty.pid`). When `--toggle` is passed, it sends `SIGUSR1` to the existing process instead of launching a new one; the daemon handles it with `glib::unix_signal_add_local` (no polling). The global shortcuts run `spotty-toggle` (`src/toggle.rs`, a second binary that links only libc) instead of `spotty --toggle`: same identity check and signal, ~2 ms instead of ~50 ms spent loading GTK. Each show maps a window pre-built in the background (`fresh_window` in `app.rs`).

## Flatpak & host bridging

- Primary distribution target is Flatpak (GNOME Platform runtime 50).
- When running inside a Flatpak sandbox, all external commands (gsettings, keybindings, terminal launch, file manager open, xdg-open) go through `flatpak-spawn --host`.
- `is_flatpak()` in `app.rs` detects sandbox at startup; cached via `OnceLock`.
- Dev runs natively — no sandboxing needed.
- Installing/updating **distro packages never uses `pkexec`**: it goes through PackageKit (`--system-talk-name=org.freedesktop.PackageKit`) or, for Fedora updates, dnf5daemon — both polkit-gated D-Bus services that work from the sandbox. Where neither exists the distro rows are simply not offered.

## Global shortcuts

Keybindings are registered by writing GNOME custom-keybindings schemas via `gsettings` (`app.rs:536-598`). Each trigger keyword and custom command gets its own slot. When inside Flatpak, `gsettings` is called on the host via `flatpak-spawn --host gsettings`.

## Core subsystems

| Subsystem | File | Notes |
|---|---|---|
| Indexer | `src/index.rs` | Background thread, lazy file index, inotify watcher |
| Search dispatch | `trigger-backends/src/search/mod.rs` | Routes queries to backends based on keyword trigger |
| Distro | `trigger-backends/src/features/distro.rs` | Which distro Spotty runs on (os-release, `/run/host/os-release` in Flatpak): family, package manager, image-based or not |
| System packages | `trigger-backends/src/features/packagekit.rs`, `trigger-backends/src/features/dnf5daemon.rs` | Install/update distro packages over D-Bus, no pkexec: PackageKit on every distro, dnf5daemon for Fedora updates. Updates download, then the restart installs them |
| Config | `src/config.rs` | JSON file, defines keywords, shortcuts, engines, flags |
| Key synthesis | `trigger-backends/src/features/keysynth.rs` | Auto-pastes via xdotool/wtype through flatpak-spawn |
| Preview | `trigger-backends/src/features/preview.rs` | Async: images, text, PDF (bundled pdftoppm), Office (zip/cfb — pure Rust, no external tool), video (bundled ffmpeg) |
| Clipboard | `trigger-backends/src/features/clipboard.rs` | History with auto-paste via key synthesis |
| File ops | `trigger-backends/src/features/fileops.rs` | Copy/cut/paste, state in thread-local |
| MPRIS | `src/mpris.rs` | GNOME media controls integration for the music player |
| OCR (image find) | `src/tesseract_ffi.rs` | dlopen libtesseract.so.5.5; preprocess, upscale, and run tesseract with PSM_AUTO |
| Content search | `trigger-backends/src/search/files.rs` | Text extraction (PDF, Office, images via tesseract), fuzzy matching |

## Flatpak: bundled tools

The flatpak manifest (`flatpak/com.spotty.Spotty.yaml`) bundles **poppler** (pdftoppm), **ffmpeg**, **dav1d** (AV1 decoder for AVIF), **leptonica**, and **tesseract** with **eng.traineddata** into `/app`. Spotty’s `resolve_tool()` checks `/app/bin` first before falling back to system PATH. Office file thumbnails need no external tool — extracted directly from zip/cfb.

## Key crates

- `gtk4` (0.9), `libadwaita` (0.7) — UI
- `nucleo-matcher` (0.3) — fuzzy matching
- `ignore` (0.4) — file walking, respects .gitignore
- `serde` / `serde_json` — config serialization
- `infer` — MIME detection
- `zip`, `cfb` — Office file thumbnail extraction
- `image`, `cairo-rs`, `gdk-pixbuf` — image handling
- `libheif-rs` with `embedded-libheif` — HEIC/AVIF support (builds libheif from C++ source)
