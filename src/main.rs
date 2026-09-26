//! Entry point: CLI parsing, daemon bootstrap, signal fast-path, GTK init.
//!
//! # Process model (one daemon, signals for hot keys)
//!
//! Spotty runs as a single background daemon; every UI action is *signaled*,
//! not spawned. A PID file (`~/.config/spotty/spotty.pid`) lets any process
//! deliver commands: `--toggle` / `--clipboard` / `--keyword=ID` write an
//! optional keyword file and send SIGUSR1 to the live PID (fast path, no GTK
//! init). The daemon's handler (in `app::on_startup`) reads the keyword file
//! and shows the right window.
//!
//! If no daemon is running (cold boot, first Super+Space ever) the gsettings
//! binding's `||` fallback runs `spotty --toggle` as a full process: startup
//! runs `on_startup` then shows the window. That one-time path pays GTK init
//! + window construction on the same press — everything heavy is deferred to
//! background threads (`Indexer` cache load, OCR probe, keybinding sync) so
//! the compositor frame clock isn't blocked (a stalled frame clock reads as
//! a video freeze — see `on_startup` docs in app.rs).
//!
//! Plain `spotty` (no args, not daemonized) spawns a detached child with
//! `SPOTTY_DAEMON=1` and returns to the shell; the child itself runs the
//! daemon below.
use adw::prelude::*;
use gtk::{gio, glib};
mod triggers;
mod app;
mod clipboard;
mod config;
mod de;
mod fileops;
mod history;
mod imageinfo;
mod index;
mod keysynth;
mod md5;
mod operations;
mod preview;
mod recent_paths;
mod search;
mod keybindings;
mod ocr;
mod opprogress;
mod thumbnails;
mod ui;
fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();

    // ── CLI-only commands (no GTK init) ──
    if args.len() >= 2 && args[1] == "--ocr-scan" {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("spotty=info"))
            .init();
        let path = if let Some(pos) = args.iter().position(|a| a == "--path") {
            let p = args.get(pos + 1).map(String::as_str).unwrap_or("");
            std::path::PathBuf::from(p)
        } else {
            dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."))
        };
        let (new, updated, skipped) = ocr::scan(&path);
        let (pnew, pupd, pskip) = ocr::scan_pdfs(&path);
        println!(
            "ocr-scan: {} images ({} new, {} updated, {} skipped), {} pdfs ({} new, {} updated, {} skipped)",
            new + updated + skipped,
            new,
            updated,
            skipped,
            pnew + pupd + pskip,
            pnew,
            pupd,
            pskip
        );
        return glib::ExitCode::SUCCESS;
    }
    if args.len() >= 3 && args[1] == "--ocr-query" {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("spotty=info"))
            .init();
        let query = &args[2];
        let results = ocr::search(query);
        if results.is_empty() {
            println!("ocr-query: no files found matching \"{query}\"");
        } else {
            for (path, text) in &results {
                println!("{}  —  {:.100}", path.display(), text);
            }
        }
        return glib::ExitCode::SUCCESS;
    }
    // ── Fast-path: signal running daemon instead of starting GTK ──
    let signal_mode: Option<&str> = if args.len() >= 2 {
        let a1 = &args[1];
        if a1 == "--toggle" {
            Some("")
        } else if a1 == "--clipboard" {
            Some("clipboard")
        } else if let Some(val) = a1.strip_prefix("--keyword=") {
            Some(val)
        } else if a1 == "--keyword" && args.len() >= 3 {
            Some(&args[2])
        } else {
            None
        }
    } else {
        None
    };
    if let Some(mode) = signal_mode {
        if let Some(pid) = read_instance_pid() {
            if unsafe { libc::kill(pid, 0) } == 0 {
                if !mode.is_empty() {
                    let kw_path = config_keyword_file();
                    let _ = std::fs::create_dir_all(kw_path.parent().unwrap());
                    let _ = std::fs::write(&kw_path, mode);
                }
                unsafe { libc::kill(pid, libc::SIGUSR1); }
                return glib::ExitCode::SUCCESS;
            }
            let _ = std::fs::remove_file(config_pid_file());
        }
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("spotty=info"))
        .init();

    // No args → daemon mode: spawn detached child, return terminal prompt
    if signal_mode.is_none() && args.len() == 1 && std::env::var("SPOTTY_DAEMON").is_err() {
        let self_exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("spotty"));
        match std::process::Command::new(&self_exe)
            .env("SPOTTY_DAEMON", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => log::info!("daemon: spawned background pid={}", child.id()),
            Err(e) => eprintln!("failed to start daemon: {e}"),
        }
        return glib::ExitCode::SUCCESS;
    }

    std::thread::spawn(|| ocr::probe_availability());
    // Force the cairo (CPU) renderer so the first frame after idle doesn't
    // depend on NVIDIA GL/EGL wake-up or GBM buffer allocation.  The launcher
    // is small enough that software rendering is negligible; revert if preview
    // performance regresses.
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "cairo");
    }
    adw::init().expect("adw init");
    let app = adw::Application::builder()
        .application_id("com.spotty.Spotty")
        .flags(
            gio::ApplicationFlags::HANDLES_COMMAND_LINE | gio::ApplicationFlags::NON_UNIQUE,
        )
        .build();
    for (l, s, d) in [
        ("toggle", "t", "Toggle"),
        ("quit", "q", "Quit"),
        ("daemon", "d", "Daemon"),
        ("settings", "s", "Settings"),
        ("clipboard", "c", "Clipboard mode"),
    ] {
        app.add_main_option(
            l,
            s.bytes().next().unwrap().into(),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            d,
            None,
        );
    }
    app.add_main_option(
        "keyword",
        b'k'.into(),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Open directly in a specific keyword mode",
        Some("KEYWORD_ID"),
    );
    app.connect_command_line(|app, cmd| {
        let o = cmd.options_dict();
        let get = |k: &str| o.lookup::<bool>(k).ok().flatten().unwrap_or(false);
        let keyword = o.lookup::<String>("keyword").ok().flatten();
        if get("quit") {
            app.quit();
            return 0;
        }
        if get("settings") {
            app::open_settings(app);
        } else if get("clipboard") {
            app::show_clipboard_search(app);
        } else if let Some(keyword) = keyword.as_deref() {
            app::show_keyword_search(app, keyword);
        } else if get("toggle") {
            app::toggle_search(app);
        } else if get("daemon") {
        } else {
            // default: daemon mode — start without window, summon via shortcut
        }
        0
    });
    app.connect_startup(app::on_startup);
    app.connect_activate(|app| {
        app::show_search(app);
    });

    // Persist our PID so future shortcuts can signal us for fast toggle.
    // Only claim the pid file when no live daemon owns it — transient
    // instances (e.g. `--settings`) must not clobber the background daemon.
    let daemon_live = read_instance_pid()
        .map(|pid| unsafe { libc::kill(pid, 0) } == 0)
        .unwrap_or(false);
    if !daemon_live {
        if let Err(e) = write_instance_pid(std::process::id() as i32) {
            log::warn!("failed to write PID file: {e}");
        }
    }

    app.run()
}

fn config_pid_file() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("spotty/spotty.pid")
}

fn config_keyword_file() -> std::path::PathBuf {
    config_pid_file().with_file_name("spotty_keyword.txt")
}

fn read_instance_pid() -> Option<i32> {
    std::fs::read_to_string(config_pid_file())
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn write_instance_pid(pid: i32) -> std::io::Result<()> {
    let path = config_pid_file();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, pid.to_string())
}
