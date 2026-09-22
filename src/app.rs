//! Application lifecycle and window plumbing.
//!
//! # Architecture
//!
//! Single GTK main thread. Root state lives in the [`STATE`] thread-local
//! (`RefCell<Option<AppState>>`); nothing here is `Arc`-shared across
//! threads. The only cross-thread state is the indexer's `Snapshot`
//! (`Arc<RwLock<…>>` in `crate::index`).
//!
//! # Why every show recreates the window
//!
//! On GNOME/Wayland, an unmapped toplevel can never regain keyboard focus:
//! re-showing a hidden `SearchWindow` maps it but leaves it input-dead
//! (verified empirically, see /tmp/opencode/passthrough_test.py). The only
//! reliable way to focus is to create a fresh window. All show paths in this
//! file therefore construct a new `SearchWindow` and swap it into state;
//! hiding unmaps + drops the old one. Consequences:
//!
//! - Window construction runs on every show (~10-30ms) — acceptable, and it
//!   is *not* prebuilt at startup, because a prebuilt window would be
//!   discarded on first show anyway (see `on_startup`).
//! - GTK's `is_visible()` is not the toggle source of truth — the window's
//!   `shown` flag is, because a fade-out is still "visible" to GTK while the
//!   toggle handler must treat it as hidden.
//!
//! # Daemon IPC
//!
//! The running daemon is summoned with SIGUSR1 (PID file + `kill -USR1`),
//! installed in [`on_startup`]. The signal handler runs on the GLib main
//! thread and routes to `toggle_search` / `show_keyword_search` depending on
//! whether a keyword request file is present.
use crate::clipboard::ClipboardHistory;
use crate::config::Config;
use crate::index::Indexer;
use crate::ui::triggers_window::TriggersWindow;
use crate::ui::search_window::SearchWindow;
use crate::ui::settings_window::SettingsWindow;
use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;


thread_local! {
    static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
    static HOLD: RefCell<Option<gtk::gio::ApplicationHoldGuard>> = const { RefCell::new(None) };
    /// Timestamp + majflt at the moment the SIGUSR1 handler fires.
    /// Consumed by present_and_focus (once per toggle).
    static SIGNAL_MARK: RefCell<Option<(Instant, libc::c_long)>> = const { RefCell::new(None) };
}
/// Application-wide root state, stored once in the [`STATE`] thread-local by
/// [`on_startup`]. `Rc` clones are handed to windows and subsystems; nothing
/// is `Send`/`Sync` — this all stays on the GTK main thread.
pub struct AppState {
    pub config: Rc<RefCell<Config>>,
    pub indexer: Rc<Indexer>,
    pub clipboard: Rc<RefCell<ClipboardHistory>>,
    pub music_player: std::sync::Arc<crate::music::MusicPlayer>,
    pub search_win: RefCell<Option<SearchWindow>>,
    pub settings_win: RefCell<Option<SettingsWindow>>,
    pub triggers_win: RefCell<Option<Rc<TriggersWindow>>>,
}

const APP_ID: &str = "com.spotty.Spotty";

/// Lazily-evaluated, cached result of whether we're inside a Flatpak sandbox.
/// Uses `std::sync::OnceLock` so the env var is checked at most once per process
/// lifetime.
pub(crate) fn is_flatpak() -> bool {
    static FLATPAK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLATPAK.get_or_init(|| {
        std::env::var("FLATPAK_ID").is_ok()
            || std::path::Path::new("/.flatpak-info").exists()
    })
}

fn host_shell(cmd: &str) -> std::process::Command {
    let mut c = if is_flatpak() {
        let mut c = std::process::Command::new("flatpak-spawn");
        c.args(["--host", "sh", "-lc"]);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.args(["-lc"]);
        c
    };
    c.arg(cmd);
    c
}

pub fn spawn_host_shell_command(command: &str) -> std::io::Result<std::process::Child> {
    host_shell(command).spawn()
}

pub fn run_host_shell_command(command: &str) -> std::io::Result<std::process::Output> {
    host_shell(command).output()
}

/// A terminal emulator discovered on the host via its `.desktop` file
/// (`Categories` containing `TerminalEmulator`). This covers distro-packaged
/// AND Flatpak terminals automatically, without a hardcoded list.
#[derive(Debug, Clone)]
pub struct TerminalEntry {
    /// Stable identifier (the `.desktop` file's basename, e.g.
    /// "org.gnome.Ptyxis" or "alacritty").
    pub id: String,
    /// `Exec=` value with desktop field codes (%u, %F, etc.) stripped.
    exec: String,
}

/// Known per-terminal "open at directory" argument styles, keyed by the
/// terminal's own command name (lowercase) — e.g. "ptyxis", "contour",
/// "konsole". Matched against either the entry's id or the binary name
/// extracted from its Exec= line (and, for Flatpak apps, the `--command=`
/// target).
fn known_open_args(command_name: &str, escaped: &str) -> Option<String> {
    Some(match command_name {
        "ptyxis" => format!("--new-window --working-directory='{escaped}'"),
        "gnome-terminal" | "gnome-terminal.real" => format!("--working-directory='{escaped}'"),
        "kgx" | "gnome-console" => format!("--working-directory='{escaped}'"),
        "konsole" => format!("--workdir '{escaped}'"),
        "xfce4-terminal" => format!("--working-directory='{escaped}'"),
        "tilix" => format!("--working-directory='{escaped}'"),
        "alacritty" => format!("--working-directory '{escaped}'"),
        "contour" => format!("terminal working-directory '{escaped}'"),
        "blackbox" | "com.raggesilver.blackbox" => format!("--working-directory='{escaped}'"),
        "io.elementary.terminal" => format!("--working-directory='{escaped}'"),
        _ => return None,
    })
}

/// Extract the terminal's own command name from its `Exec=` line: for
/// `flatpak run ... --command=foo ...` this is "foo"; otherwise it's the
/// basename of the first token.
fn exec_command_name(exec: &str) -> String {
    let tokens: Vec<&str> = exec.split_whitespace().collect();
    for t in &tokens {
        if let Some(cmd) = t.strip_prefix("--command=") {
            return cmd.to_lowercase();
        }
    }
    tokens
        .first()
        .map(|t| {
            std::path::Path::new(t)
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or(t)
                .to_lowercase()
        })
        .unwrap_or_default()
}

/// Discover installed terminal emulators by scanning `.desktop` files (both
/// distro-packaged and Flatpak exports) for `Categories=...TerminalEmulator`.
pub fn detect_installed_terminals() -> Vec<TerminalEntry> {
    const SEP: &str = "\x01";
    let script = format!(
        "for dir in /usr/share/applications /usr/local/share/applications \
             /var/lib/flatpak/exports/share/applications \
             \"$HOME/.local/share/flatpak/exports/share/applications\" \
             \"$HOME/.local/share/applications\"; do \
             [ -d \"$dir\" ] || continue; \
             for f in \"$dir\"/*.desktop; do \
                 [ -f \"$f\" ] || continue; \
                 grep -q '^Categories=.*TerminalEmulator' \"$f\" || continue; \
                 name=$(grep -m1 '^Name=' \"$f\" | head -1 | cut -d= -f2-); \
                 exec=$(grep -m1 '^Exec=' \"$f\" | head -1 | cut -d= -f2-); \
                 base=$(basename \"$f\" .desktop); \
                 printf '%s{sep}%s{sep}%s\\n' \"$base\" \"$name\" \"$exec\"; \
             done; \
         done",
        sep = SEP
    );
    let out = run_host_shell_command(&script)
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    let mut entries: Vec<TerminalEntry> = Vec::new();
    for line in out.lines() {
        let mut parts = line.splitn(3, SEP);
        let (Some(id), Some(_name), Some(exec)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if entries.iter().any(|e| e.id == id) {
            continue;
        }
        // Strip desktop field codes (%u, %U, %f, %F, %i, %c, %k, ...).
        let exec: String = exec
            .split_whitespace()
            .filter(|t| !(t.starts_with('%') && t.len() == 2))
            .collect::<Vec<_>>()
            .join(" ");
        if exec.is_empty() {
            continue;
        }
        entries.push(TerminalEntry {
            id: id.to_string(),
            exec,
        });
    }
    entries
}

/// Build the shell snippet that opens `entry` at `escaped` (a
/// shell-single-quote-escaped path) as the working directory. Falls back to
/// `cd '<path>' && exec <Exec...>` for terminals whose flag syntax we don't
/// know — works for most standalone (non-D-Bus-service) emulators.
fn terminal_open_cmd(entry: &TerminalEntry, escaped: &str) -> String {
    let cmd_name = exec_command_name(&entry.exec);
    let id_lower = entry.id.to_lowercase();
    let args = known_open_args(&cmd_name, escaped).or_else(|| known_open_args(&id_lower, escaped));
    match args {
        Some(args) => format!("exec {} {args}", entry.exec),
        None => format!("cd '{escaped}' && exec {}", entry.exec),
    }
}

/// Open a terminal emulator and run `command` in it (interactive).
/// If `command` is empty, just opens a terminal window.
/// Tries gnome-terminal, kgx (GNOME Console), and xterm in order.
pub fn run_in_terminal(command: &str) {
    let inner_cmd = if command.is_empty() {
        String::new()
    } else {
        // Keep the window open after the command so the user can read output.
        format!(
            "{}; echo '---'; echo 'Press Enter to close...'; read",
            command
        )
    };

    // Build a shell script that tries each terminal in order.
    let script = if inner_cmd.is_empty() {
        // Just open a terminal
        "for T in gnome-terminal kgx xterm; do \
            command -v $T >/dev/null 2>&1 && exec $T; \
         done"
            .to_string()
    } else {
        // Run command inside the terminal
        let escaped = inner_cmd.replace('\'', "'\\''");
        format!(
            "CMD='{escaped}'; \
             if command -v gnome-terminal >/dev/null 2>&1; then \
                 gnome-terminal -- bash -c \"$CMD\"; \
             elif command -v kgx >/dev/null 2>&1; then \
                 kgx -e bash -c \"$CMD\"; \
             elif command -v xterm >/dev/null 2>&1; then \
                 xterm -e bash -c \"$CMD\"; \
             fi"
        )
    };

    let _ = spawn_host_shell_command(&script);
}

/// Open `path` (a folder) in the user's file manager — whatever it is. Tries
/// the default handler first (gio, which works via portal when sandboxed),
/// then `xdg-open` on the host, then falls back to launching common file
/// managers directly so this works even if no default is configured.
pub fn open_in_file_manager(path: &std::path::Path) {
    let _ = gtk::gio::AppInfo::launch_default_for_uri(
        &gtk::gio::File::for_path(path).uri(),
        gtk::gio::AppLaunchContext::NONE,
    );

    if is_flatpak() {
        let dir = path.display().to_string();
        let escaped = dir.replace('\'', "'\\''");
        let script = format!(
            "xdg-open '{escaped}' >/dev/null 2>&1 && exit 0; \
             for FM in nautilus dolphin nemo pcmanfm thunar 'gio open'; do \
                 command -v ${{FM%% *}} >/dev/null 2>&1 && exec $FM '{escaped}'; \
             done"
        );
        let _ = spawn_host_shell_command(&script);
    }
}

/// Open a terminal emulator with its working directory set to `path`.
/// Always prefers the desktop's configured default terminal (gsettings /
/// xdg-settings / x-terminal-emulator), falling back through whatever
/// terminals were discovered via .desktop files.
pub fn open_terminal_at(path: &std::path::Path) {
    let dir = path.display().to_string();
    let escaped = dir.replace('\'', "'\\''");

    // Move detection + spawn off the main thread so the UI never blocks.
    std::thread::spawn(move || {
        let (mut entries, default) = cached_terminal_info();
        if let Some(default) = default {
            if let Some(pos) = entries.iter().position(|e| e.id == default.id) {
                let e = entries.remove(pos);
                entries.insert(0, e);
            }
        }

        let mut script = String::new();
        for entry in &entries {
            let Some(bin) = entry.exec.split_whitespace().next() else {
                continue;
            };
            script.push_str(&format!(
                "command -v '{bin}' >/dev/null 2>&1 && {{ {}; }}\n",
                terminal_open_cmd(entry, &escaped)
            ));
        }

        log::info!("open_terminal_at: spawning script: {script}");
        match spawn_host_shell_command(&script) {
            Ok(_) => log::info!("open_terminal_at: spawn succeeded"),
            Err(e) => log::warn!("open_terminal_at: spawn failed: {e}"),
        }
    });
}

/// TTL for the terminal detection cache. Terminals change rarely;
/// 5 minutes is plenty for a daemon process.
const TERMINAL_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Cached terminal detection result: (time, entries, default).
static TERMINAL_CACHE: OnceLock<Mutex<Option<(Instant, Vec<TerminalEntry>, Option<TerminalEntry>)>>> =
    OnceLock::new();

/// Return detected terminals and the default, using a TTL cache.
fn cached_terminal_info() -> (Vec<TerminalEntry>, Option<TerminalEntry>) {
    let m = TERMINAL_CACHE.get_or_init(|| Mutex::new(None));
    let now = Instant::now();
    {
        let mut guard = m.lock().unwrap();
        if let Some((at, entries, default)) = guard.as_ref() {
            if now.duration_since(*at) < TERMINAL_CACHE_TTL {
                return (entries.clone(), default.clone());
            }
        }
    }
    let entries = detect_installed_terminals();
    let default = detect_default_terminal(&entries);
    *m.lock().unwrap() = Some((now, entries.clone(), default.clone()));
    (entries, default)
}

/// Detect the desktop's default terminal emulator, matching one of the
/// installed `.desktop` terminals. GNOME and Budgie both publish it via
/// gsettings; other desktops set it through xdg-settings or the Debian
/// `x-terminal-emulator` alternative.
///
/// Takes a pre-scanned `entries` list to avoid re-scanning `.desktop` files.
fn detect_default_terminal(entries: &[TerminalEntry]) -> Option<TerminalEntry> {
    for probe in [
        "gsettings get org.gnome.desktop.default-applications.terminal exec 2>/dev/null",
        "xdg-settings get default-terminal-emulator 2>/dev/null",
        "readlink -f \"$(command -v x-terminal-emulator 2>/dev/null)\" 2>/dev/null",
    ] {
        let out = run_host_shell_command(probe)
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let id = normalize_terminal_id(&out);
        if id.is_empty() {
            continue;
        }
        for entry in entries {
            if entry.id.eq_ignore_ascii_case(&id)
                || exec_command_name(&entry.exec)
                    .replace('-', "")
                    .contains(&id.replace('-', ""))
            {
                return Some(entry.clone());
            }
        }
    }
    None
}

/// Normalize a gsettings/xdg-settings identifier into a `.desktop` basename or
/// command name: strips quotes and the trailing multi-character suffix after
/// the last dash for `foo.desktop` / `'foo'` / `org.x.y.desktop`-style ids.
fn normalize_terminal_id(raw: &str) -> String {
    let s = raw.trim().trim_matches('\'').to_string();
    let mut s = s.split_whitespace().last().unwrap_or(&s).to_string();
    if s.ends_with(".desktop") {
        s.truncate(s.len() - ".desktop".len());
    }
    s
}

fn is_own_flatpak() -> bool {
    std::env::var("FLATPAK_ID").ok().as_deref() == Some(APP_ID)
}

fn host_command(extra_arg: &str) -> String {
    // Fast shell signal (~5ms) to the running daemon.  If the daemon isn't
    // running (no PID file / dead process) the fallback starts one via --toggle.
    // Keyword slots skip the fallback because they need an already-running daemon.
    if extra_arg.is_empty() {
        let bin = host_command_for_launch("--toggle").replace('\'', "'\\''");
        return format!("sh -c 'kill -USR1 \"$(cat \"$HOME/.config/spotty/spotty.pid\" 2>/dev/null)\" 2>/dev/null || {bin}'");
    }
    if extra_arg == "--clipboard" {
        return host_signal_command("clipboard");
    }
    if let Some(kw) = extra_arg.strip_prefix("--keyword=") {
        return host_signal_command(kw);
    }
    host_command_for_launch(extra_arg)
}

pub(crate) fn host_signal_command(keyword: &str) -> String {
    // GNOME's g_spawn_command_line_async uses g_shell_parse_argv which
    // handles quotes but NOT $VAR or $(sub) expansion.  Wrap in sh -c so
    // a real shell evaluates $HOME and reads the pid file.
    //
    // Do NOT use `kill ... || fallback`: with an empty operand POSIX-mode
    // sh's kill prints an error but exits 0, silently swallowing the fallback.
    // Guard the pid explicitly with `[ -n "$p" ]` instead.
    // Use `read` (builtin) instead of `$(cat …)` to avoid an extra fork+exec.
    let base = spotty_base_command();
    if keyword.is_empty() {
        format!(
            "sh -c 'read -r p < \"$HOME/.config/spotty/spotty.pid\" 2>/dev/null; \
             if [ -n \"$p\" ] && kill -USR1 \"$p\" 2>/dev/null; \
             then exit 0; fi; exec \"{}\" --toggle'",
            base,
        )
    } else {
        format!(
            "sh -c 'echo \"{}\" > \"$HOME/.config/spotty/spotty_keyword.txt\"; \
             read -r p < \"$HOME/.config/spotty/spotty.pid\" 2>/dev/null; \
             if [ -n \"$p\" ] && kill -USR1 \"$p\" 2>/dev/null; \
             then exit 0; fi; exec \"{}\" --keyword={}'",
            keyword, base, keyword,
        )
    }
}

/// The base invocation for Spotty itself (no CLI args): the absolute binary
/// path, or the flatpak run command when running sandboxed. Spawnable on the
/// host by the GNOME Shell extension.
pub(crate) fn spotty_base_command() -> String {
    if is_own_flatpak() {
        format!("/usr/bin/flatpak run --user {}", APP_ID)
    } else {
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "spotty".into())
    }
}

/// Build the raw launch command (binary + CLI args) for the host keybinding.
/// This always returns a direct binary invocation that GNOME Shell can run.
pub(crate) fn host_command_for_launch(extra_arg: &str) -> String {
    let base = spotty_base_command();

    if extra_arg.is_empty() {
        format!("{} --toggle", base)
    } else {
        format!("{} {}", base, extra_arg)
    }
}

fn install_actions(app: &adw::Application, cfg: &Config) {
    let app_weak = app.downgrade();

    let toggle = gtk::gio::SimpleAction::new("toggle", None);
    toggle.connect_activate(move |_, _| {
        if let Some(app) = app_weak.upgrade() {
            glib::idle_add_local_once(move || toggle_search(&app));
        }
    });
    app.add_action(&toggle);

    let app_weak = app.downgrade();
    let clipboard = gtk::gio::SimpleAction::new("clipboard", None);
    clipboard.connect_activate(move |_, _| {
        if let Some(app) = app_weak.upgrade() {
            glib::idle_add_local_once(move || show_clipboard_search(&app));
        }
    });
    app.add_action(&clipboard);

    let refresh = gtk::gio::SimpleAction::new("refresh", None);
    refresh.connect_activate(move |_, _| {
        glib::idle_add_local_once(refresh_search_window);
    });
    app.add_action(&refresh);

    let app_weak = app.downgrade();
    let triggers = gtk::gio::SimpleAction::new("triggers", None);
    triggers.connect_activate(move |_, _| {
        if let Some(app) = app_weak.upgrade() {
            glib::idle_add_local_once(move || open_triggers_window(&app));
        }
    });
    app.add_action(&triggers);

    let trigger_kws = crate::triggers::keywords();
    for kw in cfg.command_keywords.iter().chain(trigger_kws.iter()) {
        if kw.id == "clipboard" || !kw.enabled {
            continue;
        }
        let action = gtk::gio::SimpleAction::new(&format!("keyword-{}", kw.id), None);
        let id = kw.id.clone();
        let app_weak = app.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(app) = app_weak.upgrade() {
                let id = id.clone();
                glib::idle_add_local_once(move || show_keyword_search(&app, &id));
            }
        });
        app.add_action(&action);
    }
}

/// One-time startup hook, called by the GTK application before its main loop
/// starts dispatching. Everything here runs synchronously on the main thread,
/// so it must stay lean — a slow `on_startup` blocks the compositor's frame
/// clock and manifests as a one-shot video freeze on first launch (the
/// `spotty --toggle` fallback pays this cost on the very first Super+Space).
///
/// Heavy work is deliberately offloaded or skipped here:
/// - index cache load runs on the indexer's background thread (`Indexer::new`
///   only constructs; `start_background_indexing` does the walking)
/// - OCR availability probe runs on a spawned thread (main.rs)
/// - keybinding registration + autostart run on a spawned thread
/// - the search window is NOT prebuilt — it is created fresh on first show,
///   see the module docs for why
pub fn on_startup(app: &adw::Application) {
    log::info!("=== SPOTTY v6 STARTING ===");
    let css = gtk::CssProvider::new();
    css.load_from_string(include_str!("../data/style.css"));
    if let Some(d) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &d,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let theme = gtk::IconTheme::for_display(&d);
        let repo_icons = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/icons");
        if repo_icons.exists() {
            theme.add_search_path(repo_icons);
        }
    }
    let config = Rc::new(RefCell::new(Config::load()));
    crate::search::cmd::preload_install_cache_async(config.borrow().package_manager);
    let indexer = Rc::new(Indexer::new(config.clone()));
    indexer.start_background_indexing();
    let clipboard = Rc::new(RefCell::new(ClipboardHistory::load(
        config.borrow().clipboard_history_limit,
        config.borrow().clipboard_retention_days,
    )));
    clipboard.borrow_mut().start_watching();
    // Backfill OCR text for pre-existing clipboard images: capture-time OCR
    // only covers new copies, and the background indexer skips ~/.cache.
    // Delayed so startup indexing finishes first; nudges the UI when done.
    let snapshot_arc = indexer.snapshot();
    {
        let image_paths: Vec<std::path::PathBuf> = clipboard
            .borrow()
            .entries()
            .iter()
            .filter_map(|e| match e {
                crate::clipboard::ClipboardEntry::Image(p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        if !image_paths.is_empty() {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(5));
                if crate::ocr::scan_paths(&image_paths) > 0 {
                    glib::idle_add_once(crate::app::refresh_search_window);
                }
            });
        }
    }
    // One-time indexed image + PDF sweep: refresh OCR text for files whose
    // cache entry predates the current pipeline version. Images via scan_paths,
    // PDFs via pdf_text_for (pdftotext-first). Runs once at startup, chunked
    // with pauses so interactive find/preview OCR is never starved.
    {
        let snap = snapshot_arc;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(8));
            let indexed_images: Vec<std::path::PathBuf> = {
                let Ok(g) = snap.read() else { return };
                g.files
                    .iter()
                    .filter(|f| !f.is_dir)
                    .filter(|f| {
                        f.path
                            .extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| {
                                matches!(
                                    &*e.to_ascii_lowercase(),
                                    "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "bmp"
                                        | "avif" | "gif" | "ico" | "pnm" | "pgm" | "ppm"
                                        | "pbm" | "qoi" | "tga" | "heic" | "heif" | "svg"
                                )
                            })
                    })
                    .map(|f| f.path.clone())
                    .collect()
            };
            let indexed_pdfs: Vec<std::path::PathBuf> = {
                let Ok(g) = snap.read() else { return };
                g.files
                    .iter()
                    .filter(|f| !f.is_dir)
                    .filter(|f| {
                        f.path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                            e.eq_ignore_ascii_case("pdf")
                        })
                    })
                    .map(|f| f.path.clone())
                    .collect()
            };
            // Images first (background thread, fast per file via scan_paths).
            if !indexed_images.is_empty() {
                log::info!("ocr-sweep: {} indexed images to refresh", indexed_images.len());
                for chunk in indexed_images.chunks(25) {
                    if crate::ocr::scan_paths(chunk) > 0 {
                        glib::idle_add_once(crate::app::refresh_search_window);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
            // PDFs next: pdftotext is fast for text PDFs; scanned PDFs
            // take longer (page OCR) but still chunked with pauses.
            if !indexed_pdfs.is_empty() {
                log::info!("ocr-sweep: {} indexed PDFs to refresh", indexed_pdfs.len());
                for chunk in indexed_pdfs.chunks(3) {
                    for p in chunk {
                        if crate::ocr::pdf_text_for(p).is_some() {
                            glib::idle_add_once(crate::app::refresh_search_window);
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
            }
            log::info!("ocr-sweep: done");
        });
    }
    let music_player = std::sync::Arc::new(crate::music::MusicPlayer::default());
    STATE.with(|s| {
        *s.borrow_mut() = Some(AppState {
            config: config.clone(),
            indexer,
            clipboard,
            music_player,
            search_win: RefCell::new(None),
            settings_win: RefCell::new(None),
            triggers_win: RefCell::new(None),
        })
    });
    // Load installed triggers (trigger keywords from the marketplace) before
    // anything wires up keyword actions/keybindings.
    crate::triggers::load_all();
    // ponytail: build once at startup and keep alive for the daemon's lifetime.
    // Toggle = present/hide the same window — avoids ~20-50ms per-toggle widget
    // construction that stalls the compositor frame clock (video freeze).
    // If Wayland focus ever breaks after hide, set REUSE_WINDOW=false in
    // toggle_search to fall back to fresh-window-per-show.
    {
        STATE.with(|s| {
            if let Some(st) = s.borrow().as_ref() {
                let win = SearchWindow::new(
                    app,
                    st.config.clone(),
                    st.indexer.clone(),
                    st.clipboard.clone(),
                );
                // Prewarm: realize the window at startup to create the GDK
                // surface without mapping it (no focus steal at login).  The
                // first toggle's present() then only maps the surface instead
                // of allocating + mapping, shaving the first-frame latency.
                gtk::prelude::WidgetExt::realize(&win.window);
                // Log the GSK renderer type for diagnostics.
                if let Some(renderer) = win.window.native()
                    .and_then(|n| n.renderer())
                {
                    log::info!("GSK renderer: {}", renderer.type_().name());
                }
                *st.search_win.borrow_mut() = Some(win);
            }
        });
    }
    HOLD.with(|h| *h.borrow_mut() = Some(app.hold()));
    // Expose now-playing media to GNOME (MPRIS) for the system media controls.
    crate::mpris::init(app);
    install_actions(app, &config.borrow());

    // Install SIGUSR1 handler for fast keyboard-shortcut toggle
    let app_weak = app.downgrade();
    glib::unix_signal_add_local(libc::SIGUSR1, move || {
        // Record mark: timestamp + major page-fault count at signal arrival.
        SIGNAL_MARK.with(|m| {
            let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
            unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru); }
            *m.borrow_mut() = Some((Instant::now(), ru.ru_majflt));
        });
        if let Some(app) = app_weak.upgrade() {
            let kw_path = keyword_signal_file();
            if let Ok(kw) = std::fs::read_to_string(&kw_path) {
                let _ = std::fs::remove_file(&kw_path);
                show_keyword_search(&app, kw.trim());
            } else {
                toggle_search(&app);
            }
        }
        glib::ControlFlow::Continue
    });

    std::thread::spawn(move || {
        setup_autostart();
        crate::keybindings::uninstall_old_extension();
        crate::keybindings::register_all();
    });
}

fn keyword_signal_file() -> std::path::PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    p.push("spotty/spotty_keyword.txt");
    p
}

fn local_autostart_file() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap()
        .join("autostart/com.spotty.Spotty.desktop")
}

fn autostart_desktop_entry() -> String {
    format!(
        "[Desktop Entry]
Type=Application
Name=Spotty
Comment=Start Spotty hidden in the background when you log in
Exec={}
Icon=com.spotty.Spotty
Terminal=false
NoDisplay=true
StartupNotify=false
X-GNOME-Autostart-enabled=true
",
        host_command("--daemon")
    )
}

fn host_autostart_script(script: &str) -> bool {
    let status = if is_flatpak() {
        std::process::Command::new("flatpak-spawn")
            .args(["--host", "sh", "-lc", script])
            .status()
    } else {
        std::process::Command::new("sh")
            .args(["-lc", script])
            .status()
    };
    status.map(|s| s.success()).unwrap_or(false)
}

fn host_autostart_output(script: &str) -> Option<String> {
    let out = if is_flatpak() {
        std::process::Command::new("flatpak-spawn")
            .args(["--host", "sh", "-lc", script])
            .output()
            .ok()?
    } else {
        std::process::Command::new("sh")
            .args(["-lc", script])
            .output()
            .ok()?
    };
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn autostart_enabled() -> bool {
    if is_flatpak() {
        host_autostart_script(r#"test -f "$HOME/.config/autostart/com.spotty.Spotty.desktop""#)
    } else {
        local_autostart_file().exists()
    }
}

fn autostart_contents() -> Option<String> {
    if is_flatpak() {
        host_autostart_output(
            r#"cat "$HOME/.config/autostart/com.spotty.Spotty.desktop" 2>/dev/null"#,
        )
    } else {
        std::fs::read_to_string(local_autostart_file()).ok()
    }
}

fn autostart_needs_refresh() -> bool {
    autostart_contents()
        .map(|current| current != autostart_desktop_entry())
        .unwrap_or(false)
}

pub fn set_autostart_enabled(enabled: bool) -> bool {
    if is_flatpak() {
        if enabled {
            let script = format!(
                r#"mkdir -p "$HOME/.config/autostart" && cat <<'EOF' > "$HOME/.config/autostart/com.spotty.Spotty.desktop"
{}
EOF"#,
                autostart_desktop_entry()
            );
            host_autostart_script(&script)
        } else {
            host_autostart_script(r#"rm -f "$HOME/.config/autostart/com.spotty.Spotty.desktop""#)
        }
    } else {
        let file = local_autostart_file();
        if enabled {
            if let Some(parent) = file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(file, autostart_desktop_entry()).is_ok()
        } else {
            std::fs::remove_file(file)
                .map(|_| true)
                .or_else(|err| {
                    if err.kind() == std::io::ErrorKind::NotFound {
                        Ok(true)
                    } else {
                        Err(err)
                    }
                })
                .unwrap_or(false)
        }
    }
}

fn setup_autostart() {
    if !autostart_enabled() || autostart_needs_refresh() {
        if set_autostart_enabled(true) {
            log::info!("autostart written/refreshed");
        }
    }
}

/// Show the search window (or focus it if already visible).
///
/// Wayland focus quirk this file lives around: an unmapped toplevel can never
/// regain keyboard focus on GNOME/Mutter — re-`set_visible(true)` on a hidden
/// window maps it but leaves it input-dead (proven with gate tests, see the
/// archive in /tmp/opencode). The only reliable way to get focus is a *fresh*
/// window. So every show path here creates a new `SearchWindow` and replaces
/// the stored one; the old window is dropped and its Wayland surface torn
/// down. The `shown` flag inside the window (not GTK's `is_visible`) is the
/// source of truth for the toggle, because GTK's visible state lies while a
/// fade-out is in flight.
pub fn show_search(app: &adw::Application) {
    with_state(|st| {
        let mut w = st.search_win.borrow_mut();
        if let Some(win) = w.as_ref() {
            if win.is_visible() {
                win.present_and_focus();
                return;
            }
            // ponytail: reuse existing window (built once at startup)
            win.present_and_focus();
            return;
        }
        let win = SearchWindow::new(
            app,
            st.config.clone(),
            st.indexer.clone(),
            st.clipboard.clone(),
        );
        win.present_and_focus();
        *w = Some(win);
    });
}

/// Show the clipboard history window (same recreate-on-show pattern as
/// `show_search`). Refreshes the history snapshot before showing so the list
/// is current even if the clipboard changed while Spotty was hidden.
pub fn show_clipboard_search(app: &adw::Application) {
    with_state(|st| {
        st.clipboard.borrow_mut().refresh_now();
        let mut w = st.search_win.borrow_mut();
        if let Some(win) = w.as_ref() {
            if win.is_visible() {
                win.present_clipboard_mode();
                return;
            }
            // ponytail: reuse existing window
            win.present_clipboard_mode();
            return;
        }
        let win = SearchWindow::new(
            app,
            st.config.clone(),
            st.indexer.clone(),
            st.clipboard.clone(),
        );
        win.present_clipboard_mode();
        *w = Some(win);
    });
}

pub fn refresh_search_window() {
    STATE.with(|s| {
        if let Some(st) = s.borrow().as_ref() {
            if let Some(win) = st.search_win.borrow().as_ref() {
                win.refresh_results();
                win.refresh_ops_indicator();
                win.refresh_bt_toast();
            }
        }
    });
}

/// True if the search window doesn't exist or isn't currently visible.
pub fn is_search_window_hidden() -> bool {
    STATE.with(|s| {
        if let Some(st) = s.borrow().as_ref() {
            if let Some(win) = st.search_win.borrow().as_ref() {
                return !win.is_visible();
            }
        }
        true
    })
}

/// Show a desktop notification (via the host's notify-send).
pub fn send_desktop_notification(summary: &str, body: &str) {
    let summary = summary.replace('\'', "'\\''");
    let body = body.replace('\'', "'\\''");
    let cmd = format!("notify-send -a Spotty '{}' '{}'", summary, body);
    let _ = spawn_host_shell_command(&cmd);
}
/// Open the search window directly in the mode of a configured trigger
/// keyword (e.g. a global shortcut bound to "gtk" opens the app launcher).
/// Unknown keyword IDs are silently ignored.
pub fn show_keyword_search(app: &adw::Application, keyword_id: &str) {
    STATE.with(|s| {
        let borrow = s.borrow();
        let Some(st) = borrow.as_ref() else { return; };
        let Some(keyword) = st.config.borrow().keyword_for_id(keyword_id) else {
            return;
        };
        let mut w = st.search_win.borrow_mut();
        if let Some(win) = w.as_ref() {
            if win.is_visible() {
                win.present_keyword_mode(keyword);
                return;
            }
            // ponytail: reuse existing window
            win.present_keyword_mode(keyword);
            return;
        }
        let win = SearchWindow::new(
            app,
            st.config.clone(),
            st.indexer.clone(),
            st.clipboard.clone(),
        );
        win.present_keyword_mode(keyword);
        *w = Some(win);
    });
}
/// Hide the window if visible, show it (fresh instance) if hidden.
///
/// This is the daemon's hot path: the Super+Space keybinding sends SIGUSR1 to
/// the running daemon, the handler in `on_startup` calls this. Keep it free of
/// I/O — index refresh, clipboard snapshot, etc. all happen lazily elsewhere.
/// Toggling while a fade-out is in flight is a no-op (the window's `shown`
/// flag is still true until the fade completes), which prevents an accidental
/// show/hide race from keyboard spam.
pub fn toggle_search(app: &adw::Application) {
    const REUSE_WINDOW: bool = true; // ponytail: flip to false if Wayland refocus breaks
    let t0 = Instant::now();
    STATE.with(|s| {
        let borrow = s.borrow();
        let Some(st) = borrow.as_ref() else { return; };
        let mut w = st.search_win.borrow_mut();
        if let Some(win) = w.as_ref() {
            if win.is_visible() {
                win.hide();
                return;
            }
            if REUSE_WINDOW {
                log::info!("perf: toggle reuse in {}ms", t0.elapsed().as_millis());
                w.as_ref().unwrap().present_and_focus();
                return;
            }
        }
        // ponytail: fresh window — only when no reusable window or REUSE_WINDOW=false
        let win = SearchWindow::new(
            app,
            st.config.clone(),
            st.indexer.clone(),
            st.clipboard.clone(),
        );
        log::info!("perf: toggle fresh in {}ms", t0.elapsed().as_millis());
        win.present_and_focus();
        *w = Some(win);
    });
}
pub fn open_settings(app: &adw::Application) {
    with_state(|st| {
        let mut w = st.settings_win.borrow_mut();
        if let Some(win) = w.as_ref() {
            win.present();
        } else {
            let win = SettingsWindow::new(app, st.config.clone());
            win.present();
            *w = Some(win);
        }
    });
}

/// Open the triggers window (installed list + marketplace browser). Reuses the
/// cached window like settings — installed list is refreshed on every show.
pub fn open_triggers_window(app: &adw::Application) {
    with_state(|st| {
        let mut w = st.triggers_win.borrow_mut();
        if let Some(win) = w.as_ref() {
            win.present();
        } else {
            let win = TriggersWindow::new(app, st.config.clone());
            win.present();
            *w = Some(win);
        }
    });
}

/// Open the triggers window directly on the Marketplace page (the settings
/// "Browse Marketplace…" entry).
pub fn open_triggers_marketplace(app: &adw::Application) {
    with_state(|st| {
        let mut w = st.triggers_win.borrow_mut();
        if let Some(win) = w.as_ref() {
            win.show_marketplace();
        } else {
            let win = TriggersWindow::new(app, st.config.clone());
            win.show_marketplace();
            *w = Some(win);
        }
    });
}
/// Take the signal mark (timestamp + majflt at SIGUSR1 arrival).
/// Returns `None` if no signal is pending; `Some((t, majflt))` otherwise.
pub(crate) fn take_signal_mark() -> Option<(Instant, i64)> {
    SIGNAL_MARK.with(|m| m.borrow_mut().take()).map(|(t, f)| (t, f as i64))
}
pub fn with_state<F, R>(f: F) -> R
where
    F: FnOnce(&AppState) -> R,
{
    STATE.with(|s| f(s.borrow().as_ref().expect("not init")))
}
