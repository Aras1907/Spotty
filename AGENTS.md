# AGENTS.md

Spotty — a Raycast-style launcher for GNOME Linux, in Rust with GTK4/libadwaita.

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

`main.rs` implements a single-instance daemon using a PID file (`~/.config/spotty/spotty.pid`). When `--toggle` is passed, it sends `SIGUSR1` to the existing process instead of launching a new one. The signal handler in `app.rs:14` sets a static `AtomicBool` that a 100ms glib timer polls.

## Flatpak & host bridging

- Primary distribution target is Flatpak (GNOME Platform runtime 50).
- When running inside a Flatpak sandbox, all external commands (gsettings, keybindings, terminal launch, file manager open, xdg-open) go through `flatpak-spawn --host`.
- `is_flatpak()` in `app.rs` detects sandbox at startup; cached via `OnceLock`.
- Dev runs natively — no sandboxing needed.

## Global shortcuts

Keybindings are registered by writing GNOME custom-keybindings schemas via `gsettings` (`app.rs:536-598`). Each trigger keyword and custom command gets its own slot. When inside Flatpak, `gsettings` is called on the host via `flatpak-spawn --host gsettings`.

## Core subsystems

| Subsystem | File | Notes |
|---|---|---|
| Indexer | `src/index.rs` | Background thread, lazy file index, inotify watcher |
| Search dispatch | `src/search/mod.rs` | Routes queries to backends based on keyword trigger |
| Config | `src/config.rs` | JSON file, defines keywords, shortcuts, engines, flags |
| Key synthesis | `src/keysynth.rs` | Auto-pastes via xdotool/wtype through flatpak-spawn |
| Preview | `src/preview.rs` | Async: images, text, PDF (bundled pdftoppm), Office (zip/cfb — pure Rust, no external tool), video (bundled ffmpeg) |
| Clipboard | `src/clipboard.rs` | History with auto-paste via key synthesis |
| File ops | `src/fileops.rs` | Copy/cut/paste, state in thread-local |
| MPRIS | `src/mpris.rs` | GNOME media controls integration for the music player |
| OCR (image find) | `src/tesseract_ffi.rs` | dlopen libtesseract.so.5.5; preprocess, upscale, and run tesseract with PSM_AUTO |
| Content search | `src/search/files.rs` | Text extraction (PDF, Office, images via tesseract), fuzzy matching |

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

<!-- graft:start -->
## Graft — repo context graph

This repo is indexed in `graft/`: small linked markdown nodes that explain each
system and carry exact file:line spans, kept in sync with the code through git.

For ANY task here — understanding how something works, finding where code lives,
or scoping a change — get context from the graph before grepping or opening
source files. Re-ask freely (it's cheap) and reuse literal identifiers you
already have (symbol, error string, file name) as the query. New to this repo?
Run `graft map` first — a token-budgeted orientation (dir clusters, hubs,
hotspots), no LLM, no key.

- Run `graft ask "<your question>" --source` → ranked nodes with the relevant
  code spans inlined (each hit's ≤8-line crux by default; `--full` for whole
  definitions when the crux isn't enough). Match the tool to the task shape:
  for understanding or editing, the top node IS the answer — cite its
  `covers:` file:line spans and edit straight from `--source`. For
  exhaustive tasks ("every occurrence / every caller of this pattern"), ranked
  results are top-N, not complete — run `graft grep "<literal>"` instead
  (exhaustive over indexed files, grouped by enclosing symbol), falling back
  to raw `grep -rn` only for unindexed files.
- `graft skeleton <file>` → every definition's signature + span, ~10× cheaper
  than reading the file; use it to skim an API surface.
- `graft callers <symbol>` gives precomputed, exact edges — who calls this.
  Add `--direction out` for what it calls, or `--depth N` to walk
  transitively for the full blast radius. For structural questions, skip
  ranking and use this directly.
- Or browse: `graft/INDEX.md` lists every node; follow the links.
- Monorepos and folders of multiple repos rank fairly across sub-projects —
  hits carry `[scope/]` labels naming which one they're from. Narrow with
  `graft ask "<task>" --in <scope>/` once you know where you're working.

If a returned span is truncated ("+N more lines"), open the file at that exact
range before finalizing. Only open source files when a node genuinely lacks a
needed detail, and then at the exact file:line the node points to — never
re-read whole files.

After big code changes, refresh the graph with `graft build` (deterministic,
no API key, $0).
<!-- graft:end -->
