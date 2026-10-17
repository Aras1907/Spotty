//! `spotty-toggle`: what the global shortcuts run. It does only the
//! `spotty --toggle` / `--keyword=…` fast path — validate the running daemon,
//! write the keyword file, send SIGUSR1 — and links nothing but libc, so it
//! starts in about a millisecond instead of loading all of GTK first (the
//! full binary takes ~50 ms just to be loaded before it can signal).
//! With no daemon running it hands over to `spotty` with the same arguments.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) if Path::new(&d).is_absolute() => Some(PathBuf::from(d)),
        _ => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")),
    }
    .map(|d| d.join("spotty"))
}

/// The daemon's PID, only if it is ours and runs the `spotty` binary next to
/// this one (the same identity check `spotty --toggle` makes).
fn daemon_pid(dir: &Path, spotty: &Path) -> Option<i32> {
    let pid: i32 = fs::read_to_string(dir.join("spotty.pid")).ok()?.trim().parse().ok()?;
    if pid <= 1 {
        return None;
    }
    let proc_dir = PathBuf::from(format!("/proc/{pid}"));
    if fs::metadata(&proc_dir).ok()?.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    let exe = fs::read_link(proc_dir.join("exe")).ok()?;
    let exe = exe.to_str()?;
    let live = exe.strip_suffix(" (deleted)").unwrap_or(exe);
    (Path::new(live) == spotty && unsafe { libc::kill(pid, 0) } == 0).then_some(pid)
}

/// Mode-0600 write through a fresh temp file and a rename, into a directory
/// that is ours and not a symlink (as `security::write_private` does).
fn write_private(dir: &Path, name: &str, data: &str) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::other("untrusted config directory"));
    }
    let tmp = dir.join(format!(".spotty-write-{}-toggle", std::process::id()));
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    let res = f.write_all(data.as_bytes()).and_then(|_| fs::rename(&tmp, dir.join(name)));
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let keyword = match args.first().map(String::as_str) {
        Some("--toggle") | None => "",
        Some("--clipboard") => "clipboard",
        Some(a) if a.starts_with("--keyword=") => &a["--keyword=".len()..],
        Some("--keyword") => args.get(1).map(String::as_str).unwrap_or(""),
        Some(_) => "",
    };
    let spotty = std::env::current_exe()
        .ok()
        .and_then(|p| fs::canonicalize(p).ok())
        .map(|p| p.with_file_name("spotty"))
        .unwrap_or_else(|| PathBuf::from("spotty"));
    if let Some(dir) = config_dir() {
        if let Some(pid) = daemon_pid(&dir, &spotty) {
            if keyword.is_empty() || write_private(&dir, "spotty_keyword.txt", keyword).is_ok() {
                unsafe {
                    libc::kill(pid, libc::SIGUSR1);
                }
                return;
            }
        }
    }
    // No daemon (or it couldn't be trusted): the full binary starts one.
    let err = std::process::Command::new(&spotty).args(&args).exec();
    eprintln!("spotty-toggle: cannot start {}: {err}", spotty.display());
    std::process::exit(1);
}
