// Background package operations (install / uninstall).
//
// Operations run on detached threads and record their state in a global
// registry, so they keep running even when the search window is hidden. While
// an operation is running it shows up as a live progress row (a loading bar
// with a percentage when the tool reports one). When it finishes it briefly
// shows a "Completed"/"Failed" state and is then removed automatically.

use crate::search::{Action, ResultKind, SearchResult};
use gtk::glib;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone)]
struct Operation {
    id: u64,
    /// Human label, e.g. "Installing Firefox".
    title: String,
    /// Where the package comes from, e.g. "Flatpak".
    source: String,
    /// Icon name / app-id to show on the row.
    icon: String,
    /// Most recent output line, shown as live status.
    status: String,
    /// Reported completion fraction (0.0–1.0), when the tool emits one.
    progress: Option<f64>,
    state: State,
    /// PID of the spawned process (the immediate child, e.g. `flatpak` or
    /// `pkexec`), so the operation can be cancelled from the UI.
    pid: Option<u32>,
    /// Full argv (program + args), kept so a cancelled operation can be
    /// restarted from scratch.
    args: Vec<String>,
    /// When set, the row is hidden behind a reversible swipe-dismiss state.
    dismissed_dir: Option<f64>,
    /// Deadline for auto-closing a dismissed item if it is not restored.
    pending_commit_at: Option<Instant>,
}

/// Grace period a finished operation lingers so the user sees it completed.
const DONE_GRACE: Duration = Duration::from_millis(2200);
/// How long a cancelled operation stays visible (with a redo button) so the
/// user has time to press Enter again to restart it.
const CANCEL_GRACE: Duration = Duration::from_secs(30);
const DISMISS_GRACE: Duration = Duration::from_secs(4);
const PENDING_SWEEP: Duration = Duration::from_millis(250);

fn registry() -> &'static Mutex<Vec<Operation>> {
    static C: OnceLock<Mutex<Vec<Operation>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}

/// A finished operation kept for the Operations history popover.
#[derive(Clone)]
struct HistoryEntry {
    id: u64,
    title: String,
    source: String,
    icon: String,
    state: State,
    at: Instant,
    dismissed_dir: Option<f64>,
    pending_commit_at: Option<Instant>,
}

/// Past operations (installs/uninstalls/commands), newest first.
fn history() -> &'static Mutex<Vec<HistoryEntry>> {
    static C: OnceLock<Mutex<Vec<HistoryEntry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}

/// How many past operations to keep around.
const HISTORY_CAP: usize = 50;

fn history_next_id() -> u64 {
    static H: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    H.fetch_add(1, Ordering::Relaxed)
}

fn push_history(
    title: String,
    source: String,
    icon: String,
    state: State,
    dismissed_dir: Option<f64>,
    pending_commit_at: Option<Instant>,
) {
    let mut h = history().lock().unwrap();
    h.insert(
        0,
        HistoryEntry {
            id: history_next_id(),
            title,
            source,
            icon,
            state,
            at: Instant::now(),
            dismissed_dir,
            pending_commit_at,
        },
    );
    h.truncate(HISTORY_CAP);
}

/// Remove a history entry by id (swipe-to-delete in the Operations popover).
pub fn remove_history(id: u64) {
    history().lock().unwrap().retain(|e| e.id != id);
    nudge_ui();
}

/// Record a free-form command (run via the in-window runner, not the registry)
/// in the Operations history so it shows up alongside installs/uninstalls.
pub fn record_command(command: &str, ok: bool) {
    push_history(
        format!("Run: {}", command),
        "Command".into(),
        "utilities-terminal-symbolic".into(),
        if ok { State::Done } else { State::Failed },
        None,
        None,
    );
    nudge_ui();
}

/// One item shown in the Operations popover (ongoing + past).
#[derive(Clone)]
pub struct OpItem {
    pub title: String,
    pub detail: String,
    /// "running" | "done" | "failed" | "cancelled"
    pub state: &'static str,
    pub icon: String,
    /// Deterministic progress fraction (0..1) for running operations.
    pub progress: Option<f64>,
    /// Set for running operations: swiping calls `cancel(op_id)`.
    pub op_id: Option<u64>,
    /// Set for history entries: swiping calls `remove_history(hist_id)`.
    pub hist_id: Option<u64>,
    /// Swipe direction used to hide this item pending restore/commit.
    pub dismissed_dir: Option<f64>,
}

fn relative(at: Instant) -> String {
    let secs = at.elapsed().as_secs();
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Ongoing operations (newest first) followed by past ones, for the popover.
pub fn popover_items() -> Vec<OpItem> {
    let mut out = Vec::new();
    {
        let reg = registry().lock().unwrap();
        for op in reg.iter().rev().filter(|o| o.state == State::Running) {
            out.push(OpItem {
                title: op.title.clone(),
                detail: format!("{} · {}", op.source, op.status),
                state: "running",
                icon: op.icon.clone(),
                progress: op.progress,
                op_id: Some(op.id),
                hist_id: None,
                dismissed_dir: op.dismissed_dir,
            });
        }
    }
    let h = history().lock().unwrap();
    for e in h.iter() {
        let state = match e.state {
            State::Done => "done",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
            State::Running => "running",
        };
        out.push(OpItem {
            title: e.title.clone(),
            detail: format!("{} · {}", e.source, relative(e.at)),
            state,
            icon: e.icon.clone(),
            progress: None,
            op_id: None,
            hist_id: Some(e.id),
            dismissed_dir: e.dismissed_dir,
        });
    }
    out
}

fn next_id() -> u64 {
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

fn nudge_ui() {
    // Always defer to an idle tick rather than `MainContext::invoke`, which runs
    // synchronously when called from the main thread. A cancel/start triggered by
    // a UI callback would otherwise re-enter `refresh_search_window` and rebuild
    // the very popover/rows we're inside — freezing the UI. Deferring lets the
    // current callback unwind first.
    glib::idle_add_once(crate::app::refresh_search_window);
}

/// After an operation finishes or is cancelled, clear the installed-package
/// caches and re-enumerate desktop apps so the results list updates immediately.
fn post_op_refresh() {
    crate::search::cmd::invalidate_installed_caches();
    // Re-enumerate apps on a background thread to avoid stalling the UI,
    // then nudge the window to redraw with the fresh data.
    std::thread::spawn(|| {
        let apps = crate::index::enum_apps();
        crate::app::with_state(|st| {
            st.indexer.snapshot().write().unwrap().apps = apps;
        });
        nudge_ui();
    });
}

fn ensure_pending_sweeper() {
    static START: OnceLock<()> = OnceLock::new();
    START.get_or_init(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(PENDING_SWEEP);
            let now = Instant::now();
            let op_ids: Vec<u64> = {
                let reg = registry().lock().unwrap();
                reg.iter()
                    .filter(|o| {
                        o.dismissed_dir.is_some() && o.pending_commit_at.is_some_and(|at| at <= now)
                    })
                    .map(|o| o.id)
                    .collect()
            };
            let hist_ids: Vec<u64> = {
                let h = history().lock().unwrap();
                h.iter()
                    .filter(|e| {
                        e.dismissed_dir.is_some() && e.pending_commit_at.is_some_and(|at| at <= now)
                    })
                    .map(|e| e.id)
                    .collect()
            };
            for id in op_ids {
                cancel_silently(id);
            }
            for id in hist_ids {
                remove_history(id);
            }
        });
    });
}

/// Start a background operation running `args` (argv; `args[0]` is the program).
pub fn start(title: String, source: String, icon: String, args: Vec<String>) {
    let id = next_id();
    registry().lock().unwrap().push(Operation {
        id,
        title,
        source,
        icon,
        status: "Starting…".into(),
        progress: None,
        state: State::Running,
        pid: None,
        args: args.clone(),
        dismissed_dir: None,
        pending_commit_at: None,
    });
    nudge_ui();
    run_process(id, args);
}

/// Restart a cancelled operation from scratch, reusing its original argv.
pub fn restart(id: u64) {
    let args = {
        let mut reg = registry().lock().unwrap();
        match reg.iter_mut().find(|o| o.id == id) {
            Some(op) if op.state == State::Cancelled => {
                op.state = State::Running;
                op.status = "Starting…".into();
                op.progress = None;
                op.pid = None;
                op.dismissed_dir = None;
                op.pending_commit_at = None;
                op.args.clone()
            }
            _ => return,
        }
    };
    nudge_ui();
    run_process(id, args);
}

fn run_process(id: u64, mut args: Vec<String>) {
    if args.is_empty() {
        finish(id, State::Failed);
        return;
    }
    let program = args.remove(0);

    // flatpak consults $BROWSER for webflow auth.  Override it so that
    // authenticated remotes never open a browser on the host — they fail
    // cleanly instead (same as --noninteractive did).
    let is_flatpak = program == "flatpak"
        || (program == "flatpak-spawn" && args.first().map(|s| s.as_str()) == Some("--host"));

    std::thread::spawn(move || {
        let mut cmd = std::process::Command::new(&program);
        cmd.args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if is_flatpak {
            cmd.env("BROWSER", "true");
        }
        let child = cmd.spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(_) => {
                finish(id, State::Failed);
                return;
            }
        };
        {
            let mut reg = registry().lock().unwrap();
            if let Some(op) = reg.iter_mut().find(|o| o.id == id) {
                if op.state == State::Cancelled {
                    drop(reg);
                    let _ = child.kill();
                    return;
                }
                op.pid = Some(child.id());
            }
        }

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        if let Some(out) = child.stdout.take() {
            spawn_reader(out, tx.clone());
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, tx.clone());
        }
        drop(tx);

        // Tools like flatpak emit progress via carriage-return updates many
        // times per second. Writing each one into the shared registry would
        // hammer its mutex in a tight loop and can starve the GTK main thread of
        // the same lock — making the window unresponsive (e.g. when cancelling).
        // So we keep only the latest line and flush it (registry write + UI
        // nudge) at most every ~120ms.
        let mut last_flush = Instant::now() - Duration::from_secs(1);
        let mut pending: Option<(String, Option<f64>)> = None;
        for line in rx {
            let line = line.trim().to_string();
            if !line.is_empty() {
                // Abort immediately on webflow — prevents browser popup for
                // authenticated remotes (BROWSER=true already prevents the
                // launch, but this avoids a hang if BROWSER is overridden
                // elsewhere).
                if is_flatpak && line.contains("Waiting for browser") {
                    update(id, "Authentication required (remote login unsupported)", None);
                    let _ = child.kill();
                    break;
                }
                let pct = parse_percent(&line).filter(|p| *p > 0.005);
                pending = Some((clean_status(&line), pct));
            }
            // Touch the shared registry only on the throttle tick (not per line):
            // here we both check for cancellation (bail out + kill so a cancelled
            // install stops promptly) and flush the latest progress + UI nudge.
            if last_flush.elapsed() >= Duration::from_millis(120) {
                if is_cancelled(id) {
                    let _ = child.kill();
                    break;
                }
                if let Some((l, p)) = pending.take() {
                    update(id, &l, p);
                }
                nudge_ui();
                last_flush = Instant::now();
            }
        }
        // Flush the final status line.
        if let Some((l, p)) = pending.take() {
            update(id, &l, p);
        }

        let ok = child.wait().map(|s| s.success()).unwrap_or(false);
        finish(id, if ok { State::Done } else { State::Failed });
    });
}

// Read `r` line by line, splitting on BOTH '\n' and '\r' so carriage-return
// progress updates (as flatpak emits) are captured incrementally.
fn spawn_reader<R: Read + Send + 'static>(r: R, tx: std::sync::mpsc::Sender<String>) {
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(r);
        let mut buf: Vec<u8> = Vec::with_capacity(128);
        let mut byte = [0u8; 1];
        loop {
            match reader.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    if byte[0] == b'\n' || byte[0] == b'\r' {
                        if !buf.is_empty() {
                            let line = String::from_utf8_lossy(&buf).into_owned();
                            if tx.send(line).is_err() {
                                return;
                            }
                            buf.clear();
                        }
                    } else {
                        buf.push(byte[0]);
                    }
                }
                Err(_) => break,
            }
        }
        if !buf.is_empty() {
            let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
        }
    });
}

// Pull a percentage (e.g. the "57" in "57%") out of a status line, if present.
// Also understands dnf's `(N/M)` download-counter lines (e.g. "(2/5): pkg.rpm")
// as a fraction, since piped dnf suppresses its "%" progress bar entirely.
pub(crate) fn parse_percent(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'%' {
            let mut j = i;
            while j > 0 && bytes[j - 1].is_ascii_digit() {
                j -= 1;
            }
            if j < i {
                if let Ok(n) = s[j..i].parse::<f64>() {
                    return Some((n / 100.0).clamp(0.0, 1.0));
                }
            }
        }
    }
    // dnf: "(2/5): package.rpm  12 MB/s | 5.2 MB  00:00"
    if s.starts_with('(') {
        if let Some(close) = s.find(')') {
            let inner = &s[1..close];
            if let Some((a, b)) = inner.split_once('/') {
                if let (Ok(n), Ok(m)) = (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
                    if m > 0.0 {
                        return Some((n / m).clamp(0.0, 1.0));
                    }
                }
            }
        }
    }
    None
}

// Strip ANSI escapes, block-progress bar glyphs, and collapse whitespace from
// flatpak's CLI output so the status text reads cleanly in the UI.
fn clean_status(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_escape = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
            continue;
        }
        if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        // flatpak uses these Unicode block chars in its progress bar
        if matches!(
            ch,
            '\u{2580}' | '\u{2581}' | '\u{2582}' | '\u{2583}' | '\u{2584}'
                | '\u{2585}' | '\u{2586}' | '\u{2587}' | '\u{2588}' | '\u{2589}'
                | '\u{258a}' | '\u{258b}' | '\u{258c}' | '\u{258d}' | '\u{258e}'
                | '\u{258f}' | '\u{2590}' | '\u{2591}' | '\u{2592}' | '\u{2593}'
                | '\u{2594}' | '\u{2595}' | '\u{2596}' | '\u{2597}' | '\u{2598}'
                | '\u{2599}' | '\u{259a}' | '\u{259b}' | '\u{259c}' | '\u{259d}'
                | '\u{259e}' | '\u{259f}' | '\u{2500}' | '\u{2501}'
        ) {
            continue;
        }
        out.push(ch);
    }
    // collapse multiple spaces and strip NN% tokens (leave parse_percent to
    // read the raw line before this step, so the orb still works).
    let mut prev = ' ';
    let mut digits = String::new();
    let mut collapsed = String::with_capacity(out.len());
    for ch in out.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            digits.push(ch);
            continue;
        }
        if ch == '%' && !digits.is_empty() {
            digits.clear();
            continue;
        }
        if !digits.is_empty() {
            collapsed.push_str(&digits);
            prev = digits.chars().last().unwrap();
            digits.clear();
        }
        if ch.is_whitespace() && prev.is_whitespace() {
            continue;
        }
        collapsed.push(ch);
        prev = ch;
    }
    if !digits.is_empty() {
        collapsed.push_str(&digits);
    }
    collapsed.trim().to_string()
}

// Turn a running-operation title like "Installing Firefox" or "Removing Foo"
// into a finished-state notification message.
fn notification_text(title: &str, state: State) -> String {
    let suffix = if state == State::Failed {
        "failed"
    } else {
        "is finished"
    };
    for (prefix, noun) in [
        ("Installing ", "Installation of"),
        ("Uninstalling ", "Uninstallation of"),
        ("Removing ", "Removal of"),
        ("Updating ", "Update of"),
    ] {
        if let Some(target) = title.strip_prefix(prefix) {
            return format!("{} {} {}", noun, target, suffix);
        }
    }
    format!("{} {}", title, suffix)
}

fn is_cancelled(id: u64) -> bool {
    registry()
        .lock()
        .unwrap()
        .iter()
        .any(|o| o.id == id && o.state == State::Cancelled)
}

fn update(id: u64, status: &str, progress: Option<f64>) {
    let mut reg = registry().lock().unwrap();
    if let Some(op) = reg.iter_mut().find(|o| o.id == id) {
        op.status = status.to_string();
        if progress.is_some() {
            op.progress = progress;
        }
    }
}

pub fn dismiss_item(op_id: Option<u64>, hist_id: Option<u64>, dir: f64) {
    let mut changed = false;
    let deadline = Instant::now() + DISMISS_GRACE;
    if let Some(id) = op_id {
        let mut reg = registry().lock().unwrap();
        if let Some(op) = reg
            .iter_mut()
            .find(|o| o.id == id && o.state == State::Running)
        {
            op.dismissed_dir = Some(dir);
            op.pending_commit_at = Some(deadline);
            changed = true;
        }
    } else if let Some(id) = hist_id {
        let mut h = history().lock().unwrap();
        if let Some(entry) = h.iter_mut().find(|e| e.id == id) {
            entry.dismissed_dir = Some(dir);
            entry.pending_commit_at = Some(deadline);
            changed = true;
        }
    }
    if changed {
        ensure_pending_sweeper();
        nudge_ui();
    }
}

pub fn restore_item(op_id: Option<u64>, hist_id: Option<u64>) -> bool {
    let mut restored = false;
    if let Some(id) = op_id {
        let mut reg = registry().lock().unwrap();
        if let Some(op) = reg
            .iter_mut()
            .find(|o| o.id == id && o.dismissed_dir.is_some())
        {
            op.dismissed_dir = None;
            op.pending_commit_at = None;
            restored = true;
        }
    } else if let Some(id) = hist_id {
        let mut h = history().lock().unwrap();
        if let Some(entry) = h
            .iter_mut()
            .find(|e| e.id == id && e.dismissed_dir.is_some())
        {
            entry.dismissed_dir = None;
            entry.pending_commit_at = None;
            restored = true;
        }
    }
    if restored {
        nudge_ui();
    }
    restored
}

pub fn commit_item(op_id: Option<u64>, hist_id: Option<u64>) {
    if let Some(id) = op_id {
        cancel_silently(id);
    } else if let Some(id) = hist_id {
        remove_history(id);
    }
}

fn cancel_silently(id: u64) {
    let pid = {
        let mut reg = registry().lock().unwrap();
        match reg.iter_mut().find(|o| o.id == id) {
            Some(op) if op.state == State::Running || op.state == State::Cancelled => {
                op.state = State::Cancelled;
                op.status = "Cancelled".into();
                op.dismissed_dir = None;
                op.pending_commit_at = None;
                op.pid
            }
            _ => return,
        }
    };
    if let Some(pid) = pid {
        std::thread::spawn(move || {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output();
        });
    }
    nudge_ui();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        let mut reg = registry().lock().unwrap();
        reg.retain(|o| o.id != id || o.state != State::Cancelled);
        drop(reg);
        nudge_ui();
    });
}

/// Cancel a running operation: kill the spawned process and mark it cancelled.
/// If the process hasn't been spawned yet, mark it so `start`'s thread kills
/// it as soon as it appears.
pub fn cancel(id: u64) {
    let (pid, hist) = {
        let mut reg = registry().lock().unwrap();
        match reg.iter_mut().find(|o| o.id == id) {
            Some(op) if op.state == State::Running => {
                op.state = State::Cancelled;
                op.status = "Cancelled".into();
                (
                    op.pid,
                    Some((
                        op.title.clone(),
                        op.source.clone(),
                        op.icon.clone(),
                        op.dismissed_dir,
                        op.pending_commit_at,
                    )),
                )
            }
            _ => return,
        }
    };
    if let Some((title, source, icon, dismissed_dir, pending_commit_at)) = hist {
        push_history(
            title,
            source,
            icon,
            State::Cancelled,
            dismissed_dir,
            pending_commit_at,
        );
    }
    if let Some(pid) = pid {
        // Kill off the main thread: spawning + reaping a process synchronously in
        // a UI callback can stall the window.
        std::thread::spawn(move || {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output();
        });
    }
    nudge_ui();
    post_op_refresh();
    std::thread::spawn(move || {
        std::thread::sleep(CANCEL_GRACE);
        // If the user restarted it in the meantime, leave it running.
        let mut reg = registry().lock().unwrap();
        reg.retain(|o| o.id != id || o.state != State::Cancelled);
        drop(reg);
        nudge_ui();
    });
}

fn finish(id: u64, state: State) {
    let mut notify_title: Option<String> = None;
    let mut hist: Option<(String, String, String, Option<f64>, Option<Instant>)> = None;
    {
        let mut reg = registry().lock().unwrap();
        if let Some(op) = reg.iter_mut().find(|o| o.id == id) {
            if op.state == State::Cancelled {
                return;
            }
            op.state = state;
            op.progress = Some(1.0);
            op.status = match state {
                State::Done => "Completed".into(),
                State::Failed => "Failed".into(),
                State::Cancelled => "Cancelled".into(),
                State::Running => op.status.clone(),
            };
            if matches!(state, State::Done | State::Failed) {
                notify_title = Some(notification_text(&op.title, state));
                hist = Some((
                    op.title.clone(),
                    op.source.clone(),
                    op.icon.clone(),
                    op.dismissed_dir,
                    op.pending_commit_at,
                ));
            }
        }
    }
    if let Some((title, source, icon, dismissed_dir, pending_commit_at)) = hist {
        push_history(title, source, icon, state, dismissed_dir, pending_commit_at);
    }
    if let Some(text) = notify_title {
        glib::MainContext::default().invoke(move || {
            if crate::app::is_search_window_hidden() {
                crate::app::send_desktop_notification("Spotty", &text);
            }
        });
    }
    nudge_ui();
    post_op_refresh();
    // Linger briefly so the completed/failed state is visible, then drop it.
    std::thread::spawn(move || {
        std::thread::sleep(DONE_GRACE);
        registry().lock().unwrap().retain(|o| o.id != id);
        nudge_ui();
    });
}

/// Newest running operation's (title, fraction), if any.
pub fn active_op_progress() -> Option<(String, Option<f64>)> {
    let reg = registry().lock().unwrap();
    reg.iter()
        .rev()
        .find(|o| o.state == State::Running)
        .map(|o| (o.title.clone(), o.progress))
}

/// Live (subtitle, indeterminate) for a still-running op, keyed by id.
/// Returns `None` once the op is gone or no longer running.
pub fn op_row_update(id: u64) -> Option<(String, bool)> {
    let reg = registry().lock().unwrap();
    let op = reg.iter().find(|o| o.id == id && o.state == State::Running)?;
    Some((format!("{} · {}", op.source, op.status), op.progress.is_none()))
}

/// One live progress result row per operation, newest first. Empty when nothing
/// is active, so indicators vanish on completion.
pub fn running_result_rows() -> Vec<SearchResult> {
    let reg = registry().lock().unwrap();
    let mut score: i32 = 80_000;
    reg.iter()
        .rev()
        .map(|op| {
            let state_str = match op.state {
                State::Running => "running",
                State::Done => "done",
                State::Failed => "failed",
                State::Cancelled => "cancelled",
            };
            let frac_str = match op.state {
                State::Running => op.progress.map(|f| format!("{:.4}", f)).unwrap_or_default(),
                _ => "1".into(),
            };
            let status_text = match op.state {
                State::Done => format!("{} · Completed ✓", op.source),
                State::Failed => format!("{} · Failed", op.source),
                State::Cancelled => format!("{} · Cancelled", op.source),
                State::Running => format!("{} · {}", op.source, op.status),
            };
            // The action string is a sentinel carrying render hints:
            //   "__op__\x1f<fraction>\x1f<state>\x1f<icon>\x1f<id>"
            let action = Action::EnterMode(format!(
                "__op__\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
                frac_str, state_str, op.icon, op.id
            ));
            let row = SearchResult {
                kind: ResultKind::System,
                title: op.title.clone(),
                subtitle: Some(status_text),
                icon: Some("op-progress".into()),
                action,
                score,
            };
            score -= 1;
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_percent_various() {
        assert_eq!(parse_percent("45% done"), Some(0.45));
        assert_eq!(parse_percent("  0%  "), Some(0.0));
        assert_eq!(parse_percent("Installing ████░░ 50%"), Some(0.50));
        assert_eq!(parse_percent("100%"), Some(1.0));
        assert_eq!(parse_percent("no number here"), None);
        assert_eq!(parse_percent("(3/7): foo.rpm"), Some(3.0 / 7.0));
        assert_eq!(parse_percent("(0/5): bar.rpm"), Some(0.0));
        assert_eq!(parse_percent("done!"), None);
    }

    #[test]
    fn clean_status_strips_ansi() {
        let raw = "\x1b[1mInstalling\x1b[0m firefox";
        assert_eq!(clean_status(raw), "Installing firefox");
    }

    #[test]
    fn clean_status_strips_block_bar() {
        let raw = "Installing… ████████░░░░ 45%  1.2 MB/s";
        assert_eq!(clean_status(raw), "Installing… 1.2 MB/s");
    }

    #[test]
    fn clean_status_collapses_whitespace() {
        let raw = "  hello   world  ";
        assert_eq!(clean_status(raw), "hello world");
    }

    #[test]
    fn clean_status_plain_text_unchanged() {
        assert_eq!(clean_status("hello"), "hello");
        assert_eq!(clean_status(""), "");
    }

    #[test]
    fn clean_status_strips_percent_tokens() {
        assert_eq!(clean_status("45% done"), "done");
        assert_eq!(clean_status("Installing… 100% complete"), "Installing… complete");
        assert_eq!(clean_status("3.2% I/O"), "I/O");
        assert_eq!(clean_status("no percent here"), "no percent here");
    }
}
