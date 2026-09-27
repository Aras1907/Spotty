// CMD trigger mode: kill running processes, install/uninstall/search Flatpak+distro apps.
// Suggestions are built from async-fetched caches so the UI never blocks.

use crate::config::{Config, PackageManager};
use crate::index::AppEntry;
use crate::search::{Action, ResultKind, SearchResult};
use crate::i18n::gettext;
use gtk::glib;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32String};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

// ── Data types ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct UpdateInfo {
    source: String,
    app_id: Option<String>,
    name: String,
    /// The concrete packages behind this row (used by the preview pane):
    /// "name  version" lines per source, app names for flatpak.
    details: Vec<String>,
}

#[derive(Clone)]
struct FlatpakApp {
    app_id: String,
    name: String,
    description: String,
    app_id_lc: String,
    name_lc: String,
    description_lc: String,
}

impl FlatpakApp {
    fn new(app_id: String, name: String, description: String) -> Self {
        let app_id_lc = app_id.to_lowercase();
        let name_lc = name.to_lowercase();
        let description_lc = description.to_lowercase();
        Self {
            app_id,
            name,
            description,
            app_id_lc,
            name_lc,
            description_lc,
        }
    }
}

#[derive(Clone)]
struct DistroPackage {
    name: String,
    description: String,
    name_lc: String,
    description_lc: String,
}

impl DistroPackage {
    fn new(name: String, description: String) -> Self {
        let name_lc = name.to_lowercase();
        let description_lc = description.to_lowercase();
        Self {
            name,
            description,
            name_lc,
            description_lc,
        }
    }
}

// ── Thread-safe caches ────────────────────────────────────────────────────────

fn process_cache() -> &'static Mutex<Option<(Instant, Vec<String>)>> {
    static C: OnceLock<Mutex<Option<(Instant, Vec<String>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn process_fetching() -> &'static Mutex<bool> {
    static C: OnceLock<Mutex<bool>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(false))
}

// Running Flatpak app-ids (from `flatpak ps`), refreshed alongside `ps`.
fn flatpak_running_cache() -> &'static Mutex<Option<(Instant, Vec<String>)>> {
    static C: OnceLock<Mutex<Option<(Instant, Vec<String>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn flatpak_running_fetching() -> &'static Mutex<bool> {
    static C: OnceLock<Mutex<bool>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(false))
}

fn installed_cache() -> &'static Mutex<Option<(Instant, Vec<FlatpakApp>)>> {
    static C: OnceLock<Mutex<Option<(Instant, Vec<FlatpakApp>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn installed_fetching() -> &'static Mutex<bool> {
    static C: OnceLock<Mutex<bool>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(false))
}

fn flatpak_catalog_cache() -> &'static Mutex<Option<(Instant, Vec<FlatpakApp>)>> {
    static C: OnceLock<Mutex<Option<(Instant, Vec<FlatpakApp>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn flatpak_catalog_fetching() -> &'static Mutex<bool> {
    static C: OnceLock<Mutex<bool>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(false))
}

// Distro PM: full available-package catalog, fetched once and fuzzy-matched
// in-memory (same approach as the Flatpak catalog) so typing feels instant
// instead of shelling out to the package manager on every keystroke.
fn distro_catalog_cache() -> &'static Mutex<Option<(Instant, String, Arc<Vec<DistroPackage>>)>> {
    static C: OnceLock<Mutex<Option<(Instant, String, Arc<Vec<DistroPackage>>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn distro_catalog_fetching() -> &'static Mutex<bool> {
    static C: OnceLock<Mutex<bool>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(false))
}

// Background fuzzy search over the distro catalog: keeps the UI thread from
// scanning the (potentially tens-of-thousands-of-entries) catalog on every
// keystroke. `ensure_distro_search` kicks off a background match for the
// current query; `distro_search_cache` holds the latest completed matches as
// raw `DistroPackage`s — the `SearchResult` rows (and the already-installed
// filter) are built at read time, so an installed list that warms up after
// the match still takes effect. The generation counter ensures only the most
// recent query's results win, even if an older search thread finishes after a
// newer one.
fn distro_search_generation() -> &'static AtomicU64 {
    static C: OnceLock<AtomicU64> = OnceLock::new();
    C.get_or_init(|| AtomicU64::new(0))
}
fn distro_search_cache() -> &'static Mutex<Option<(String, Vec<DistroPackage>)>> {
    static C: OnceLock<Mutex<Option<(String, Vec<DistroPackage>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

// Distro PM: installed packages cache
fn distro_installed_cache() -> &'static Mutex<Option<(Instant, Vec<DistroPackage>)>> {
    static C: OnceLock<Mutex<Option<(Instant, Vec<DistroPackage>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn distro_installed_fetching() -> &'static Mutex<bool> {
    static C: OnceLock<Mutex<bool>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(false))
}

// Distro PM auto-detection cache — None = not yet detected, Some(None) = none found
fn distro_pm_cache() -> &'static Mutex<Option<Option<String>>> {
    static C: OnceLock<Mutex<Option<Option<String>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn distro_pm_detecting() -> &'static Mutex<bool> {
    static C: OnceLock<Mutex<bool>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(false))
}

// Snap: like distro, an async presence check — None = not yet checked,
// Some(true/false) = snapd available on the host.
fn snap_available_cache() -> &'static Mutex<Option<bool>> {
    static C: OnceLock<Mutex<Option<bool>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn snap_available_fetching() -> &'static Mutex<bool> {
    static C: OnceLock<Mutex<bool>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(false))
}

// Snap installed-package cache (`snap list`), same shape as the distro one.
fn snap_installed_cache() -> &'static Mutex<Option<(Instant, Vec<DistroPackage>)>> {
    static C: OnceLock<Mutex<Option<(Instant, Vec<DistroPackage>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn snap_installed_fetching() -> &'static Mutex<bool> {
    static C: OnceLock<Mutex<bool>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(false))
}

// Snap find is a live, server-side search (there's no local catalog dump),
// so results are cached per-query instead of against a preloaded catalog.
// Generation counter keeps only the newest query's results winning.
fn snap_search_generation() -> &'static AtomicU64 {
    static C: OnceLock<AtomicU64> = OnceLock::new();
    C.get_or_init(|| AtomicU64::new(0))
}
fn snap_search_cache() -> &'static Mutex<Option<(String, Vec<SearchResult>)>> {
    static C: OnceLock<Mutex<Option<(String, Vec<SearchResult>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

// Software update cache (5-minute TTL — updates change faster than catalogs).
fn update_cache() -> &'static Mutex<Option<(Instant, Vec<UpdateInfo>)>> {
    static C: OnceLock<Mutex<Option<(Instant, Vec<UpdateInfo>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}
fn update_fetching() -> &'static Mutex<bool> {
    static C: OnceLock<Mutex<bool>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(false))
}

/// Clear all installed-package caches so the next search re-queries flatpak/dnf/snap.
/// Called after an operation finishes to ensure removed apps vanish from results.
pub fn invalidate_installed_caches() {
    *installed_cache().lock().unwrap() = None;
    *distro_installed_cache().lock().unwrap() = None;
    *snap_installed_cache().lock().unwrap() = None;
}

// ── Host-aware command helpers ────────────────────────────────────────────────

fn is_sandbox() -> bool {
    std::env::var("FLATPAK_ID").is_ok()
}

// Build a Command that runs `prog` on the host (via flatpak-spawn when sandboxed).
fn host_command(prog: &str) -> std::process::Command {
    if is_sandbox() {
        let mut c = std::process::Command::new("flatpak-spawn");
        c.args(["--host", prog]);
        c
    } else {
        std::process::Command::new(prog)
    }
}

// Build the argv for a flatpak sub-command, prefixing with flatpak-spawn when needed.
fn flatpak_cmd_args(subargs: &[&str]) -> Vec<String> {
    if is_sandbox() {
        let mut v = vec![
            "flatpak-spawn".to_string(),
            "--host".to_string(),
            "flatpak".to_string(),
        ];
        v.extend(subargs.iter().map(|s| s.to_string()));
        v
    } else {
        let mut v = vec!["flatpak".to_string()];
        v.extend(subargs.iter().map(|s| s.to_string()));
        v
    }
}

// ── Distro PM detection ───────────────────────────────────────────────────────

fn ensure_distro_pm() {
    {
        let c = distro_pm_cache().lock().unwrap();
        if c.is_some() {
            return;
        }
    }
    {
        let mut f = distro_pm_detecting().lock().unwrap();
        if *f {
            return;
        }
        *f = true;
    }
    std::thread::spawn(|| {
        let pm = detect_distro_pm_blocking();
        *distro_pm_cache().lock().unwrap() = Some(pm);
        *distro_pm_detecting().lock().unwrap() = false;
        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

fn detect_distro_pm_blocking() -> Option<String> {
    if is_sandbox() {
        for (check_cmd, name) in [
            ("apt-get", "apt"),
            ("dnf", "dnf"),
            ("pacman", "pacman"),
            ("zypper", "zypper"),
        ] {
            let found = std::process::Command::new("flatpak-spawn")
                .args(["--host", "which", check_cmd])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if found {
                return Some(name.to_string());
            }
        }
        None
    } else {
        for (path, name) in [
            ("/usr/bin/apt", "apt"),
            ("/usr/bin/dnf", "dnf"),
            ("/usr/bin/pacman", "pacman"),
            ("/usr/bin/zypper", "zypper"),
        ] {
            if std::path::Path::new(path).exists() {
                return Some(name.to_string());
            }
        }
        None
    }
}

fn get_detected_pm() -> Option<String> {
    distro_pm_cache()
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|o| o.clone()))
}

// ── Snap presence ─────────────────────────────────────────────────────────────

fn ensure_snap_available() {
    {
        let c = snap_available_cache().lock().unwrap();
        if c.is_some() {
            return;
        }
    }
    {
        let mut f = snap_available_fetching().lock().unwrap();
        if *f {
            return;
        }
        *f = true;
    }
    std::thread::spawn(|| {
        let available = host_command("snap")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        *snap_available_cache().lock().unwrap() = Some(available);
        *snap_available_fetching().lock().unwrap() = false;
        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

pub(crate) fn snap_is_available() -> Option<bool> {
    snap_available_cache().lock().ok().and_then(|g| *g)
}

// ── Background fetchers ───────────────────────────────────────────────────────

pub fn prewarm_update_cache() {
    ensure_updates_checked();
}

pub fn preload_install_cache_async(pm: PackageManager) {
    ensure_distro_pm();
    // Snap presence is always probed (not just when the saved PM uses snap) so
    // the settings dropdown can show/hide the snap options based on it.
    ensure_snap_available();
    if pm.use_flatpak() {
        ensure_flatpak_catalog();
    }
    if pm.use_distro() {
        // Distro PM detection happens on a background thread; poll briefly
        // for it to finish, then kick off the (also-cached) full package
        // catalog fetch so it's warm before the user finishes typing "install ".
        std::thread::spawn(|| {
            for _ in 0..50 {
                if let Some(pm_name) = get_detected_pm() {
                    ensure_distro_catalog(pm_name);
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
    }
}

fn ensure_processes() {
    {
        let c = process_cache().lock().unwrap();
        if let Some((t, _)) = c.as_ref() {
            if t.elapsed() < Duration::from_secs(3) {
                return;
            }
        }
    }
    {
        let mut f = process_fetching().lock().unwrap();
        if *f {
            return;
        }
        *f = true;
    }
    {
        let mut f = flatpak_running_fetching().lock().unwrap();
        *f = true;
    }
    std::thread::spawn(|| {
        let raw = host_command("ps")
            .args(["-eo", "comm", "--no-headers"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let mut names: Vec<String> = raw
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|s| !s.is_empty() && s != "ps" && s != "flatpak-spawn")
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        names.sort();
        *process_cache().lock().unwrap() = Some((Instant::now(), names));
        *process_fetching().lock().unwrap() = false;

        let raw = host_command("flatpak")
            .args(["ps", "--columns=application"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let ids: Vec<String> = raw
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        *flatpak_running_cache().lock().unwrap() = Some((Instant::now(), ids));
        *flatpak_running_fetching().lock().unwrap() = false;

        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

fn ensure_installed() {
    {
        let c = installed_cache().lock().unwrap();
        if let Some((t, _)) = c.as_ref() {
            if t.elapsed() < Duration::from_secs(30) {
                return;
            }
        }
    }
    {
        let mut f = installed_fetching().lock().unwrap();
        if *f {
            return;
        }
        *f = true;
    }
    std::thread::spawn(|| {
        let raw = host_command("flatpak")
            .args(["list", "--app", "--columns=application,name"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let apps: Vec<FlatpakApp> = raw
            .lines()
            .filter_map(|line| {
                let mut p = line.splitn(2, '\t');
                let app_id = p.next()?.trim().to_string();
                if app_id.is_empty() {
                    return None;
                }
                let name = p.next().unwrap_or(&app_id).trim().to_string();
                Some(FlatpakApp::new(app_id, name, String::new()))
            })
            .collect();
        *installed_cache().lock().unwrap() = Some((Instant::now(), apps));
        *installed_fetching().lock().unwrap() = false;
        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

// ── Update checking ───────────────────────────────────────────────────────────

/// The universal update verb: `update`, `updates`, `upd`, `upgrade`,
/// `upg`, optionally followed by a target ("update flatpak", "update all",
/// "update firefox"). Updates are not a trigger, so this runs in plain
/// universal search. A still-incomplete verb ("up", "updat") shows the very
/// same rows as the complete verb — one update row in every state, with
/// ghost text completing the verb itself (see [`verb_completion`]).
pub fn update_verb_rows(query: &str, config: &Config) -> Option<Vec<SearchResult>> {
    let q = query.trim();
    let (first, rest) = match q.find(char::is_whitespace) {
        Some(i) => (&q[..i], q[i..].trim()),
        None => (q, ""),
    };
    let verb = first.to_ascii_lowercase();
    let exact = verb == "update"
        || verb == "updates"
        || verb == "upd"
        || verb.starts_with("upg");
    let prefix = rest.is_empty() && verb_completion(q).is_some();
    if !exact && !prefix {
        return None;
    }
    let score = if rest.is_empty() { 100_000 } else { 50_000 };
    let mut rows = update_results(rest, config);
    // Boost the actual update rows above incidental matches; leave the
    // "disable update checks" tail row where it belongs.
    for r in &mut rows {
        if r.score >= 1000 {
            r.score = r.score.max(score);
        }
    }
    Some(rows)
}

/// The ghost completion for a still-incomplete update verb — "upd" →
/// "update ", "upg" → "upgrade " — `None` once the verb is complete or the
/// text is shorter than two characters (so "u" stays available for other
/// words like "uninstall").
pub fn verb_completion(user_text: &str) -> Option<String> {
    if user_text.chars().count() < 2 || user_text.contains(char::is_whitespace) {
        return None;
    }
    let t = user_text.to_ascii_lowercase();
    // A complete verb stays as it is ("update" must not grow into
    // "updates"), even though the longer one starts with it.
    if ["update", "updates", "upgrade"].contains(&t.as_str()) {
        return None;
    }
    ["update", "updates", "upgrade"]
        .iter()
        .find(|v| v.starts_with(&t))
        .map(|v| format!("{v} "))
}

pub fn ensure_updates_checked() {
    ensure_updates_checked_age(Duration::from_secs(5 * 60));
}

/// Refresh the cached update list once it is older than `max_age`. The
/// background scheduler passes the user-configured interval, searches pass
/// a short one so the list in the results stays fresh.
pub fn ensure_updates_checked_age(max_age: Duration) {
    {
        let c = update_cache().lock().unwrap();
        if let Some((t, _)) = c.as_ref() {
            if t.elapsed() < max_age {
                return;
            }
        }
    }
    {
        let mut f = update_fetching().lock().unwrap();
        if *f {
            return;
        }
        *f = true;
    }
    std::thread::spawn(|| {
        let updates = fetch_updates();
        *update_cache().lock().unwrap() = Some((Instant::now(), updates.clone()));
        *update_fetching().lock().unwrap() = false;
        glib::MainContext::default().invoke(move || {
            crate::app::refresh_search_window();
            notify_if_new(&updates);
        });
    });
}

/// Startup / periodic tick: respects the feature switch and the
/// user-configured interval (Settings → Updates).
pub fn periodic_update_check(config: &Config) {
    if !config.enable_updates {
        return;
    }
    let interval =
        Duration::from_secs(config.update_check_interval_hours.max(1) as u64 * 3600);
    ensure_updates_checked_age(interval);
}

/// "Check now" in Settings → Updates: ignore the freshness window.
pub fn check_updates_now() {
    ensure_updates_checked_age(Duration::ZERO);
}

/// True when `query` opens the update list ("update", "upd", "upgrade"...)
/// — the context the update shortcuts (Ctrl+Enter / Ctrl+D / Ctrl+F) act in.
pub fn in_update_context(query: &str) -> bool {
    let first = query
        .trim()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        first.as_str(),
        "update" | "updates" | "upd" | "upgrade" | "upg"
    )
}

/// Args for a shortcut-driven scope run right now — `None` when that scope
/// has nothing pending, so the shortcut never fires an empty pkexec run.
pub fn update_scope_args(scope: &str) -> Option<Vec<String>> {
    let list = update_cache().lock().ok()?.as_ref()?.1.clone();
    if list.is_empty() {
        return None;
    }
    let has = |s: &str| list.iter().any(|u| u.source == s);
    let src = match scope {
        "all" => "all",
        "flatpak" if has("flatpak") => "flatpak",
        "snap" if has("snap") => "snap",
        "distro" => list
            .iter()
            .map(|u| u.source.as_str())
            .find(|s| matches!(*s, "dnf" | "apt" | "pacman" | "zypper"))?,
        _ => return None,
    };
    let args = update_cmd_args(src, None);
    if args.len() == 1 && args[0] == "true" {
        None
    } else {
        Some(args)
    }
}

fn update_cmd_args(source: &str, app_id: Option<&str>) -> Vec<String> {
    match source {
        "flatpak" if app_id.is_some() => {
            flatpak_cmd_args(&["update", "--assumeyes", app_id.unwrap()])
        }
        "flatpak" => flatpak_cmd_args(&["update", "--assumeyes"]),
        "dnf" => pkexec_cmd_args(vec!["dnf".into(), "upgrade".into(), "-y".into()]),
        "apt" => pkexec_cmd_args(vec!["apt-get".into(), "upgrade".into(), "-y".into()]),
        "pacman" => pkexec_cmd_args(vec![
            "pacman".into(),
            "-Syu".into(),
            "--noconfirm".into(),
        ]),
        "zypper" => pkexec_cmd_args(vec!["zypper".into(), "update".into(), "-y".into()]),
        "snap" => pkexec_cmd_args(vec!["snap".into(), "refresh".into()]),
        "all" => {
            let mut parts: Vec<(&str, String)> = Vec::new();
            let cache = update_cache().lock().unwrap();
            if let Some((_, entries)) = cache.as_ref() {
                let has_fp = entries
                    .iter()
                    .any(|u| u.source == "flatpak");
                if has_fp {
                    parts.push(("flatpak", "flatpak update --assumeyes".into()));
                }
                for pm in &["dnf", "apt", "pacman", "zypper"] {
                    if entries.iter().any(|u| u.source == *pm) {
                        let cmd = match *pm {
                            "dnf" => "pkexec dnf upgrade -y",
                            "apt" => "pkexec apt-get upgrade -y",
                            "pacman" => "pkexec pacman -Syu --noconfirm",
                            "zypper" => "pkexec zypper update -y",
                            _ => continue,
                        };
                        parts.push((pm, cmd.into()));
                    }
                }
                if entries.iter().any(|u| u.source == "snap") {
                    parts.push(("snap", "pkexec snap refresh".into()));
                }
            }
            drop(cache);
            if parts.is_empty() {
                vec!["true".to_string()]
            } else {
                chained_script(&parts)
            }
        }
        _ => vec!["true".to_string()],
    }
}

/// Build the `sh -c` argv for a chained "update all" run. Each part is
/// prefixed with an `__spotty_part_k_m_tool__` echo marker so the progress
/// tracker knows which tool is running and what share of the whole update it
/// owns (see `opprogress`). `&&` keeps the original stop-on-failure semantics.
fn chained_script(parts: &[(&str, String)]) -> Vec<String> {
    let total = parts.len();
    let mut script = String::new();
    for (i, (tool, cmd)) in parts.iter().enumerate() {
        if i > 0 {
            script.push_str(" && ");
        }
        script.push_str(&format!(
            "echo __spotty_part_{}_{}_{}__ && {}",
            i + 1,
            total,
            tool,
            cmd
        ));
    }
    vec!["sh".to_string(), "-c".to_string(), script]
}
fn fetch_updates() -> Vec<UpdateInfo> {
    // Every source check runs in parallel: the wait is the slowest single
    // check (dnf/flatpak metadata) instead of the sum of all seven. Nothing
    // is cached here — each command still queries live state. Each check
    // returns the *package list*, so "has updates" is just "non-empty" and
    // the preview can show what exactly wants updating.
    let (fp_updates, dnf, apt, pacman, zypper, snap) = std::thread::scope(|s| {
        let fp = s.spawn(|| fetch_flatpak_updates());
        let dnf = s.spawn(|| dnf_updates());
        let apt = s.spawn(|| apt_updates());
        let pacman = s.spawn(|| pacman_updates());
        let zypper = s.spawn(|| zypper_updates());
        let snap = s.spawn(|| snap_updates());
        (
            fp.join().unwrap_or_default(),
            dnf.join().unwrap_or_default(),
            apt.join().unwrap_or_default(),
            pacman.join().unwrap_or_default(),
            zypper.join().unwrap_or_default(),
            snap.join().unwrap_or_default(),
        )
    });
    assemble_updates(
        fp_updates,
        [("dnf", dnf), ("apt", apt), ("pacman", pacman), ("zypper", zypper)],
        snap,
    )
}

/// Turn the per-source package lists into the update cache: one entry per
/// real package (a flatpak app, a distro package, a snap). Counts, previews
/// and the "update <package>" rows all derive from these entries, so
/// aggregates can never inflate the "N updates available" badge.
fn assemble_updates(
    fp_updates: Vec<(String, String)>,
    system: [(&str, Vec<String>); 4],
    snap: Vec<String>,
) -> Vec<UpdateInfo> {
    let mut out = Vec::new();
    // Flatpak: one entry per app (name = display name, app_id = command).
    for (app_id, name) in &fp_updates {
        out.push(UpdateInfo {
            source: "flatpak".into(),
            app_id: Some(app_id.clone()),
            name: name.clone(),
            details: vec![name.clone()],
        });
    }
    // System packages: one entry per package; the parsers return
    // "name  version" lines, which is exactly what the preview shows.
    for (pm, pkgs) in &system {
        for pkg in pkgs {
            out.push(UpdateInfo {
                source: (*pm).into(),
                app_id: None,
                name: pkg.clone(),
                details: vec![pkg.clone()],
            });
        }
    }
    // Snap: one entry per package.
    for pkg in snap {
        out.push(UpdateInfo {
            source: "snap".into(),
            app_id: None,
            name: pkg.clone(),
            details: vec![pkg.clone()],
        });
    }
    out
}

fn fetch_flatpak_updates() -> Vec<(String, String)> {
    // --user and --system in parallel too.
    let raws: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = ["--user", "--system"]
            .iter()
            .copied()
            .map(|scope| {
                s.spawn(move || {
                    host_command("flatpak")
                        .args([
                            scope,
                            "remote-ls",
                            "--updates",
                            "--app",
                            "--columns=application,name",
                        ])
                        .output()
                        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                        .unwrap_or_default()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let mut all = Vec::new();
    for raw in raws {
        for line in raw.lines() {
            let mut p = line.splitn(2, '\t');
            let app_id = p.next().unwrap_or("").trim().to_string();
            if app_id.is_empty() || !app_id.contains('.') {
                continue;
            }
            let name = p.next().unwrap_or(&app_id).trim().to_string();
            all.push((app_id, name));
        }
    }
    all
}

/// Pure parsers: command output → "name  version" lines for the preview.

fn parse_dnf_updates(out: &str) -> Vec<String> {
    // `dnf -q check-update`: "name.arch  version  repo" per line.
    out.lines()
        .filter_map(|l| {
            let toks: Vec<&str> = l.split_whitespace().collect();
            if toks.len() < 3 {
                return None;
            }
            Some(format!("{}  {}", toks[0], toks[1]))
        })
        .collect()
}

fn parse_apt_updates(out: &str) -> Vec<String> {
    // `apt list --upgradable`: "name/repo  version  arch [upgradable from: …]"
    out.lines()
        .filter(|l| !l.starts_with("Listing") && !l.starts_with("WARNING"))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pkg = it.next()?;
            let ver = it.next()?;
            let name = pkg.split('/').next().unwrap_or(pkg);
            Some(format!("{name}  {ver}"))
        })
        .collect()
}

fn parse_pacman_updates(out: &str) -> Vec<String> {
    // `pacman -Qu`: "name  old -> new"
    out.lines()
        .filter(|l| l.contains(" -> "))
        .map(|l| l.replace(" -> ", " → "))
        .collect()
}

fn parse_snap_updates(out: &str) -> Vec<String> {
    // `snap refresh --list`: a "Name Version …" table after a header line.
    let mut seen_header = false;
    out.lines()
        .filter_map(|l| {
            let t = l.trim_start();
            if !seen_header {
                if t.starts_with("Name") {
                    seen_header = true;
                }
                return None;
            }
            if t.starts_with('=') || t.is_empty() {
                return None;
            }
            let mut it = t.split_whitespace();
            let name = it.next()?;
            let ver = it.next()?;
            Some(format!("{name}  {ver}"))
        })
        .collect()
}

fn parse_zypper_updates(out: &str) -> Vec<String> {
    // `zypper lu --no-refresh`: "| S | Repo | Name | Current | Available | …"
    out.lines()
        .filter(|l| l.contains('|'))
        .filter_map(|l| {
            let cols: Vec<&str> = l.split('|').map(|c| c.trim()).collect();
            if cols.len() < 5 {
                return None;
            }
            let (name, cur, avail) = (cols[2], cols[3], cols[4]);
            if name == "Name" || cur == "Current" {
                return None;
            }
            Some(format!("{name}  {cur} → {avail}"))
        })
        .collect()
}

fn dnf_updates() -> Vec<String> {
    host_command("dnf")
        .args(["check-update", "-q"])
        .output()
        .map(|o| parse_dnf_updates(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

fn apt_updates() -> Vec<String> {
    host_command("apt")
        .args(["list", "--upgradable"])
        .output()
        .map(|o| parse_apt_updates(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

fn pacman_updates() -> Vec<String> {
    host_command("pacman")
        .args(["-Qu"])
        .output()
        .map(|o| parse_pacman_updates(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

fn snap_updates() -> Vec<String> {
    host_command("snap")
        .args(["refresh", "--list"])
        .output()
        .map(|o| parse_snap_updates(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

fn zypper_updates() -> Vec<String> {
    host_command("zypper")
        .args(["lu", "--no-refresh"])
        .output()
        .map(|o| parse_zypper_updates(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

fn ensure_flatpak_catalog() {
    {
        let cache = flatpak_catalog_cache().lock().unwrap();
        if let Some((updated, apps)) = cache.as_ref() {
            if updated.elapsed() < Duration::from_secs(6 * 60 * 60) && !apps.is_empty() {
                return;
            }
        }
    }
    {
        let mut fetching = flatpak_catalog_fetching().lock().unwrap();
        if *fetching {
            return;
        }
        *fetching = true;
    }
    std::thread::spawn(|| {
        let apps = fetch_flatpak_catalog();
        *flatpak_catalog_cache().lock().unwrap() = Some((Instant::now(), apps));
        *flatpak_catalog_fetching().lock().unwrap() = false;
        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

fn fetch_flatpak_catalog() -> Vec<FlatpakApp> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for scope in ["--user", "--system"] {
        let raw = host_command("flatpak")
            .args([
                scope,
                "remote-ls",
                "--cached",
                "--app",
                "--columns=application,name,description",
                "flathub",
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        for app in parse_flatpak_apps(&raw) {
            if seen.insert(app.app_id.clone()) {
                out.push(app);
            }
        }
    }
    out
}

fn parse_flatpak_apps(raw: &str) -> Vec<FlatpakApp> {
    raw.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(3, '\t').collect();
            let app_id = parts.first()?.trim().to_string();
            if app_id.is_empty() {
                return None;
            }
            let name = parts
                .get(1)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| app_id.clone());
            let description = parts
                .get(2)
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            Some(FlatpakApp::new(app_id, name, description))
        })
        .collect()
}

fn ensure_distro_catalog(pm: String) {
    {
        let c = distro_catalog_cache().lock().unwrap();
        if let Some((updated, cpm, _)) = c.as_ref() {
            if *cpm == pm && updated.elapsed() < Duration::from_secs(3600) {
                return;
            }
        }
    }
    {
        let mut f = distro_catalog_fetching().lock().unwrap();
        if *f {
            return;
        }
        *f = true;
    }
    std::thread::spawn(move || {
        let pkgs = fetch_distro_catalog(&pm);
        *distro_catalog_cache().lock().unwrap() = Some((Instant::now(), pm, Arc::new(pkgs)));
        *distro_catalog_fetching().lock().unwrap() = false;
        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

// Kick off a background fuzzy match of `query` against the distro catalog (if
// not already cached/in-flight), so the UI thread never scans the full
// (tens-of-thousands-entry) package list directly. Matches land in
// `distro_search_cache` and trigger a refresh when ready.
fn ensure_distro_search(query: String, catalog: Arc<Vec<DistroPackage>>) {
    {
        let c = distro_search_cache().lock().unwrap();
        if let Some((cq, _)) = c.as_ref() {
            if *cq == query {
                return;
            }
        }
    }
    let generation = distro_search_generation().fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let matches: Vec<DistroPackage> = fuzzy_distro(&query, &catalog)
            .into_iter()
            .map(|(pkg, _)| pkg.clone())
            .collect();
        if distro_search_generation().load(Ordering::SeqCst) != generation {
            return;
        }
        *distro_search_cache().lock().unwrap() = Some((query, matches));
        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

fn ensure_distro_installed(pm: String) {
    {
        let c = distro_installed_cache().lock().unwrap();
        if let Some((t, _)) = c.as_ref() {
            if t.elapsed() < Duration::from_secs(30) {
                return;
            }
        }
    }
    {
        let mut f = distro_installed_fetching().lock().unwrap();
        if *f {
            return;
        }
        *f = true;
    }
    std::thread::spawn(move || {
        let pkgs = fetch_distro_installed(&pm);
        *distro_installed_cache().lock().unwrap() = Some((Instant::now(), pkgs));
        *distro_installed_fetching().lock().unwrap() = false;
        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

/// Lowercased names of the distro packages already installed on the host.
/// Empty while the background fetch is still warming — early searches then
/// show install rows until it lands, and the next search drops them.
fn distro_installed_names() -> std::collections::HashSet<String> {
    distro_installed_cache()
        .lock()
        .unwrap()
        .as_ref()
        .map(|(_, pkgs)| pkgs.iter().map(|p| p.name_lc.clone()).collect())
        .unwrap_or_default()
}

// ── Distro PM fetch + parse ───────────────────────────────────────────────────

// Fetch the FULL list of available packages once (like the Flatpak catalog),
// so subsequent searches are instant fuzzy lookups in memory instead of a
// fresh package-manager invocation per keystroke.
fn fetch_distro_catalog(pm: &str) -> Vec<DistroPackage> {
    let output = match pm {
        "apt" => host_command("apt-cache").args(["search", "."]).output(),
        "dnf" => host_command("dnf")
            .args([
                "repoquery",
                "--available",
                "--cacheonly",
                "--queryformat",
                "%{name}\t%{summary}\n",
            ])
            .output(),
        "pacman" => host_command("pacman").args(["-Ss", "."]).output(),
        "zypper" => host_command("zypper")
            .args(["--no-refresh", "search"])
            .output(),
        _ => return vec![],
    };
    let raw = output
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let mut pkgs = parse_distro_search(pm, &raw);
    let mut seen = std::collections::HashSet::new();
    pkgs.retain(|p| seen.insert(p.name_lc.clone()));
    pkgs
}

fn parse_distro_search(pm: &str, raw: &str) -> Vec<DistroPackage> {
    match pm {
        "apt" => raw
            .lines()
            .filter_map(|line| {
                let (name, desc) = line.split_once(" - ")?;
                let name = name.trim().to_string();
                if name.is_empty() {
                    return None;
                }
                Some(DistroPackage::new(name, desc.trim().to_string()))
            })
            .collect(),
        "dnf" => {
            let mut pkgs = Vec::new();
            for line in raw.lines() {
                let t = line.trim();
                if t.is_empty() || t.starts_with('=') || t.starts_with("Matched fields") {
                    continue;
                }
                // dnf5 search: " name.arch\tdescription"; dnf4 search:
                // "name.arch : description"; repoquery (catalog): "name\tdescription"
                // (no arch suffix). Only strip a trailing ".<arch>" component, since
                // package names themselves can legitimately contain dots.
                let parsed = t.split_once('\t').or_else(|| t.split_once(" : "));
                if let Some((name_arch, desc)) = parsed {
                    let name_arch = name_arch.trim();
                    let name = match name_arch.rsplit_once('.') {
                        Some((base, arch))
                            if matches!(
                                arch,
                                "x86_64"
                                    | "i686"
                                    | "noarch"
                                    | "aarch64"
                                    | "armv7hl"
                                    | "s390x"
                                    | "ppc64le"
                            ) =>
                        {
                            base.to_string()
                        }
                        _ => name_arch.to_string(),
                    };
                    if !name.is_empty() && !name.starts_with('=') {
                        pkgs.push(DistroPackage::new(name, desc.trim().to_string()));
                    }
                }
            }
            pkgs
        }
        "pacman" => {
            let lines: Vec<&str> = raw.lines().collect();
            let mut pkgs = Vec::new();
            let mut i = 0;
            while i < lines.len() {
                let line = lines[i].trim();
                if let Some(slash) = line.find('/') {
                    let after = &line[slash + 1..];
                    let name = after.split_whitespace().next().unwrap_or("").to_string();
                    let desc = lines
                        .get(i + 1)
                        .map(|l| l.trim().to_string())
                        .unwrap_or_default();
                    if !name.is_empty() {
                        pkgs.push(DistroPackage::new(name, desc));
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            pkgs
        }
        "zypper" => {
            let mut pkgs = Vec::new();
            for line in raw.lines() {
                if !line.contains('|') {
                    continue;
                }
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() >= 3 {
                    let name = parts[1].trim().to_string();
                    let desc = parts[2].trim().to_string();
                    if !name.is_empty() && name != "Name" && !name.starts_with('-') {
                        pkgs.push(DistroPackage::new(name, desc));
                    }
                }
            }
            pkgs
        }
        _ => vec![],
    }
}

fn fetch_distro_installed(pm: &str) -> Vec<DistroPackage> {
    let output = match pm {
        "apt" => host_command("dpkg-query")
            .args(["-W", "-f=${Package}\t${binary:Summary}\n"])
            .output(),
        "dnf" => host_command("rpm")
            .args(["-qa", "--queryformat", "%{NAME}\t%{SUMMARY}\n"])
            .output(),
        "pacman" => host_command("pacman").args(["-Q"]).output(),
        "zypper" => host_command("zypper")
            .args(["packages", "--installed-only"])
            .output(),
        _ => return vec![],
    };
    let raw = output
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    parse_distro_installed(pm, &raw)
}

fn parse_distro_installed(pm: &str, raw: &str) -> Vec<DistroPackage> {
    match pm {
        "apt" | "dnf" => raw
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(2, '\t');
                let name = parts.next()?.trim().to_string();
                let desc = parts.next().unwrap_or("").trim().to_string();
                if name.is_empty() {
                    return None;
                }
                Some(DistroPackage::new(name, desc))
            })
            .collect(),
        "pacman" => raw
            .lines()
            .filter_map(|line| {
                let name = line.split_whitespace().next()?.to_string();
                if name.is_empty() {
                    return None;
                }
                Some(DistroPackage::new(name, String::new()))
            })
            .collect(),
        "zypper" => {
            let mut pkgs = Vec::new();
            for line in raw.lines() {
                if !line.contains('|') {
                    continue;
                }
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() >= 5 {
                    let name = parts[2].trim().to_string();
                    if !name.is_empty() && name != "Name" && !name.starts_with('-') {
                        pkgs.push(DistroPackage::new(name, String::new()));
                    }
                }
            }
            pkgs
        }
        _ => vec![],
    }
}

// ── Snap fetch + parse ────────────────────────────────────────────────────────

// Snap has no local package catalog — `snap find` is a live store search, so
// we fetch per query (instead of preloading a full catalog like distro) and
// cache the results keyed by query string.
fn ensure_snap_search(query: String) {
    {
        let c = snap_search_cache().lock().unwrap();
        if let Some((q, _)) = c.as_ref() {
            if *q == query {
                return;
            }
        }
    }
    // Ensure we know whether installed snaps shadow the candidates.
    ensure_snap_installed();
    let generation = snap_search_generation().fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let results = fetch_snap_search(&query);
        if snap_search_generation().load(Ordering::SeqCst) != generation {
            return;
        }
        *snap_search_cache().lock().unwrap() = Some((query, results));
        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

fn fetch_snap_search(query: &str) -> Vec<SearchResult> {
    let raw = host_command("snap")
        .args(["find", "--limit=20", query])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let installed: std::collections::HashSet<String> = snap_installed_cache()
        .lock()
        .unwrap()
        .as_ref()
        .map(|(_, pkgs)| pkgs.iter().map(|p| p.name.clone()).collect())
        .unwrap_or_default();
    parse_snap_find(&raw)
        .into_iter()
        .filter(|pkg| !installed.contains(&pkg.name))
        .map(|pkg| snap_install_result(pkg))
        .collect()
}

// `snap find` output: Name  Version  Publisher  Notes  Summary
fn parse_snap_find(raw: &str) -> Vec<DistroPackage> {
    raw.lines()
        .filter_map(|line| {
            let t = line.trim();
            if t.is_empty() || t.starts_with("Name") {
                return None;
            }
            let name = t.split_whitespace().next()?.to_string();
            if name.is_empty() {
                return None;
            }
            Some(DistroPackage::new(name, String::new()))
        })
        .collect()
}

fn ensure_snap_installed() {
    {
        let c = snap_installed_cache().lock().unwrap();
        if let Some((t, _)) = c.as_ref() {
            if t.elapsed() < Duration::from_secs(30) {
                return;
            }
        }
    }
    {
        let mut f = snap_installed_fetching().lock().unwrap();
        if *f {
            return;
        }
        *f = true;
    }
    std::thread::spawn(|| {
        let pkgs = fetch_snap_installed();
        *snap_installed_cache().lock().unwrap() = Some((Instant::now(), pkgs));
        *snap_installed_fetching().lock().unwrap() = false;
        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

// `snap list` columns: Name Version Rev Tracking Publisher Notes
fn fetch_snap_installed() -> Vec<DistroPackage> {
    let raw = host_command("snap")
        .args(["list"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    raw.lines()
        .skip(1)
        .filter_map(|line| {
            let name = line.split_whitespace().next()?.to_string();
            if name.is_empty() {
                return None;
            }
            Some(DistroPackage::new(name, String::new()))
        })
        .collect()
}

// ── Fuzzy helpers ─────────────────────────────────────────────────────────────

/// Cap the number of items scanned by the fuzzy/typo pass in catalog searches.
/// Keeps worst-case latency under ~20ms on the UI thread for large Flatpak
/// catalogs while still catching most plausible typos.
const FUZZY_SCAN_CAP: usize = 4000;

fn fuzzy_strings<'a>(query: &str, items: &'a [String]) -> Vec<(&'a String, u32)> {
    if query.is_empty() {
        return items.iter().take(10).map(|s| (s, 1000)).collect();
    }
    let ql = query.to_lowercase();
    let mut matcher = Matcher::default();
    let pattern = Pattern::parse(&ql, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(&String, u32)> = items
        .iter()
        .filter_map(|s| {
            let sl = s.to_lowercase();
            let score = if sl == ql {
                100_000
            } else if sl.starts_with(&ql) {
                50_000
            } else if sl.contains(&ql) {
                30_000
            } else {
                let fs = pattern.score(Utf32String::from(s.as_str()).slice(..), &mut matcher)?;
                let threshold = (ql.len() as u32).saturating_mul(20);
                if fs >= threshold {
                    fs
                } else {
                    return None;
                }
            };
            Some((s, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.truncate(20);
    scored
}

fn fuzzy_apps<'a>(query: &str, items: &'a [FlatpakApp]) -> Vec<(&'a FlatpakApp, u32)> {
    let ql = query.trim().to_lowercase();
    if ql.is_empty() {
        return items.iter().take(8).map(|app| (app, 1_000)).collect();
    }

    let ql_len = ql.len();
    let mut best: Vec<(&FlatpakApp, u32)> = Vec::with_capacity(8);
    let mut matcher = Matcher::default();
    let pattern = Pattern::parse(&ql, CaseMatching::Ignore, Normalization::Smart);
    for (i, app) in items.iter().enumerate() {
        let score = if app.name_lc == ql || app.app_id_lc == ql {
            100_000
        } else if app.name_lc.starts_with(&ql) {
            80_000u32.saturating_sub(app.name_lc.len() as u32)
        } else if app.app_id_lc.starts_with(&ql) {
            70_000u32.saturating_sub(app.app_id_lc.len() as u32)
        } else if word_starts_with(&app.name_lc, &ql) {
            60_000u32.saturating_sub(app.name_lc.len() as u32)
        } else if app.name_lc.contains(&ql) {
            40_000u32.saturating_sub(app.name_lc.len() as u32)
        } else if app.app_id_lc.contains(&ql) {
            30_000u32.saturating_sub(app.app_id_lc.len() as u32)
        } else if ql_len >= 4 && app.description_lc.contains(&ql) {
            10_000
        } else if i < FUZZY_SCAN_CAP && ql_len >= 3 {
            // Nucleo fuzzy fallback for typos (e.g. "blendr" → "Blender")
            if let Some(fs) = pattern.score(
                Utf32String::from(app.name.as_str()).slice(..),
                &mut matcher,
            ) {
                if fs >= (ql_len as u32).saturating_mul(25) {
                    fs / 4 // scale to ~25000 range, below name-contains (40k)
                } else {
                    // Keyboard-layout typo pass (e.g. "frefox" → "Firefox")
                    if let Some(ts) = crate::search::typo::keyboard_similarity(&ql, &app.name_lc) {
                        ts / 2 // scale to ~450 range
                    } else {
                        continue;
                    }
                }
            } else if let Some(ts) = crate::search::typo::keyboard_similarity(&ql, &app.name_lc) {
                ts / 2
            } else {
                continue;
            }
        } else {
            continue;
        };
        insert_top_match(&mut best, app, score);
    }
    best
}

fn word_starts_with(text: &str, query: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|word| word.starts_with(query))
}

fn insert_top_match<'a, T>(best: &mut Vec<(&'a T, u32)>, item: &'a T, score: u32) {
    let pos = best
        .iter()
        .position(|(_, existing)| score > *existing)
        .unwrap_or(best.len());
    if pos < 8 {
        best.insert(pos, (item, score));
        if best.len() > 8 {
            best.pop();
        }
    }
}

fn fuzzy_distro<'a>(query: &str, items: &'a [DistroPackage]) -> Vec<(&'a DistroPackage, u32)> {
    let ql = query.trim().to_lowercase();
    if ql.is_empty() {
        return items.iter().take(8).map(|pkg| (pkg, 1_000)).collect();
    }

    let ql_len = ql.len();
    let mut best: Vec<(&DistroPackage, u32)> = Vec::with_capacity(8);
    let mut matcher = Matcher::default();
    let pattern = Pattern::parse(&ql, CaseMatching::Ignore, Normalization::Smart);
    for (i, pkg) in items.iter().enumerate() {
        let score = if pkg.name_lc == ql {
            100_000
        } else if pkg.name_lc.starts_with(&ql) {
            60_000u32.saturating_sub(pkg.name_lc.len() as u32)
        } else if pkg.name_lc.contains(&ql) {
            40_000u32.saturating_sub(pkg.name_lc.len() as u32)
        } else if ql_len >= 4 && pkg.description_lc.contains(&ql) {
            10_000
        } else if i < FUZZY_SCAN_CAP && ql_len >= 3 {
            // Nucleo fuzzy fallback for typos (e.g. "chrom" → "chromium")
            if let Some(fs) = pattern.score(
                Utf32String::from(pkg.name.as_str()).slice(..),
                &mut matcher,
            ) {
                if fs >= (ql_len as u32).saturating_mul(25) {
                    fs / 4
                } else if let Some(ts) = crate::search::typo::keyboard_similarity(&ql, &pkg.name_lc) {
                    ts / 2
                } else {
                    continue;
                }
            } else if let Some(ts) = crate::search::typo::keyboard_similarity(&ql, &pkg.name_lc) {
                ts / 2
            } else {
                continue;
            }
        } else {
            continue;
        };
        insert_top_match(&mut best, pkg, score);
    }
    best
}

// ── Template list ─────────────────────────────────────────────────────────────

fn templates() -> Vec<SearchResult> {
    // Running operations (live loading bars) sit at the very top.
    let mut v = crate::operations::running_result_rows();
    v.extend([
        SearchResult {
            kind: ResultKind::System,
            title: gettext("Kill").into(),
            subtitle: Some(gettext("kill <app-name>  —  stop a running process").into()),
            icon: Some("process-stop-symbolic".into()),
            action: Action::EnterMode("cmd".into()),
            score: 1000,
        },
        SearchResult {
            kind: ResultKind::System,
            title: gettext("Install").into(),
            subtitle: Some(gettext("install <app>  —  install from Flatpak, distro, or Snap").into()),
            icon: Some("package-x-generic-symbolic".into()),
            action: Action::EnterMode("cmd".into()),
            score: 999,
        },
        SearchResult {
            kind: ResultKind::System,
            title: gettext("Uninstall").into(),
            subtitle: Some(gettext("uninstall <app>  —  remove an installed app").into()),
            icon: Some("edit-delete-symbolic".into()),
            action: Action::EnterMode("cmd".into()),
            score: 998,
        },
        SearchResult {
            kind: ResultKind::System,
            title: gettext("Update").into(),
            subtitle: Some(gettext("update  —  update installed packages").into()),
            icon: Some("software-update-available-symbolic".into()),
            action: Action::EnterMode("cmd".into()),
            score: 997,
        },
    ]);
    v
}

fn searching_placeholder(label: &str) -> Vec<SearchResult> {
    vec![SearchResult {
        kind: ResultKind::System,
        title: label.into(),
        subtitle: Some(gettext("Please wait…").into()),
        icon: Some("emblem-synchronizing-symbolic".into()),
        action: Action::EnterMode("cmd".into()),
        score: 1000,
    }]
}

// ── Main search ───────────────────────────────────────────────────────────────

pub fn search(query: &str, config: &Config, apps: &[AppEntry]) -> Vec<SearchResult> {
    let pm = config.package_manager;
    // Kick off distro PM detection early so it's ready when needed
    ensure_distro_pm();

    let q = query.trim();
    if q.is_empty() {
        return templates();
    }
    let ql = q.to_lowercase();

    let (verb, rest) = match ql.find(' ') {
        Some(i) => (ql[..i].to_string(), ql[i + 1..].trim().to_string()),
        None => (ql.clone(), String::new()),
    };

    // ── Synonym suggestions ───────────────────────────────────────────────────
    // Words like "add"/"remove"/"delete"/"stop" mean the same thing as
    // install/uninstall/kill but aren't recognized verbs themselves. Suggest
    // the matching action template so the user can press Enter to autofill
    // the canonical action word and continue typing the app name.
    if rest.is_empty() {
        let suggestion = match verb.as_str() {
            "add" | "get" | "download" => Some((
                "Install",
                "install <app-name>  —  searches Flatpak and/or distro",
                "package-x-generic-symbolic",
            )),
            "remove" | "delete" | "del" | "erase" | "rem" => Some((
                "Uninstall",
                "uninstall <app-name>  —  remove an installed app",
                "edit-delete-symbolic",
            )),
            "stop" | "end" | "terminate" | "close" => Some((
                "Kill",
                "kill <app-name>  —  stop a running process",
                "process-stop-symbolic",
            )),
            "upgrade" | "up" => Some((
                "Update",
                "update  —  check for & install package updates",
                "software-update-available-symbolic",
            )),
            _ => None,
        };
        if let Some((title, sub, icon)) = suggestion {
            let mut results = vec![SearchResult {
                kind: ResultKind::System,
                title: title.into(),
                subtitle: Some(sub.into()),
                icon: Some(icon.into()),
                action: Action::EnterMode("cmd".into()),
                score: 1000,
            }];
            results.extend(crate::operations::running_result_rows());
            return results;
        }
    }

    // ── Kill ──────────────────────────────────────────────────────────────────
    // Restricted to running processes that correspond to indexed (visible) apps —
    // never arbitrary system processes.
    // Only act on these verbs once the action word has been fully typed
    // ("kill"/"install"/"uninstall") or accepted via autocomplete (which always
    // expands to the canonical word + a space). A bare alias with nothing after
    // it (e.g. just "k") falls through to the template list instead of jumping
    // straight to recommendations.
    if verb == "kill" || (matches_verb(&verb, &["kill", "k", "stop"]) && !rest.is_empty()) {
        ensure_processes();
        let cache = process_cache().lock().unwrap();
        let Some((_, names)) = cache.as_ref() else {
            drop(cache);
            return searching_placeholder(&gettext("Scanning running processes…"));
        };
        let names = names.clone();
        drop(cache);
        let flatpak_ids = flatpak_running_cache()
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, ids)| ids.clone())
            .unwrap_or_default();
        let running = running_apps(&names, &flatpak_ids, apps);
        if rest.is_empty() {
            return running
                .iter()
                .take(10)
                .map(|(name, target)| kill_result(name, target))
                .collect();
        }
        let display_names: Vec<String> = running.iter().map(|(name, _)| name.clone()).collect();
        let matches = fuzzy_strings(&rest, &display_names);
        if matches.is_empty() {
            return vec![SearchResult {
                kind: ResultKind::System,
                title: gettext("No running app matching \"{query}\"").replace("{query}", &rest),
                subtitle: Some(gettext("Only running apps can be killed").into()),
                icon: Some("process-stop-symbolic".into()),
                action: Action::EnterMode("cmd".into()),
                score: 0,
            }];
        }
        return matches
            .iter()
            .filter_map(|(name, _)| {
                running
                    .iter()
                    .find(|(n, _)| n == *name)
                    .map(|(n, p)| kill_result(n, p))
            })
            .collect();
    }

    // ── Install ───────────────────────────────────────────────────────────────
    if verb == "install"
        || (matches_verb(&verb, &["install", "ins", "add", "i"]) && !rest.is_empty())
    {
        // While an install/uninstall is in flight, surface its live loading bar
        // above the search results so the user sees it's already running.
        let mut results = search_install(&rest, pm);
        results.extend(crate::operations::running_result_rows());
        return results;
    }

    // ── Uninstall ─────────────────────────────────────────────────────────────
    if verb == "uninstall"
        || (matches_verb(&verb, &["uninstall", "remove", "rem", "uninst", "del"])
            && !rest.is_empty())
    {
        // Surface any running operation's live loading bar above the results.
        let mut results = search_uninstall(&rest, pm);
        results.extend(crate::operations::running_result_rows());
        return results;
    }

    // ── Update ─────────────────────────────────────────────────────────────────
    if verb == "update"
        || (matches_verb(&verb, &["update", "upgrade", "up"]) && !rest.is_empty())
    {
        let mut results = search_updates(&rest, config);
        results.extend(crate::operations::running_result_rows());
        return results;
    }

    // ── Fuzzy fallback across template titles ─────────────────────────────────
    let mut matcher = Matcher::default();
    let pattern = Pattern::parse(&ql, CaseMatching::Ignore, Normalization::Smart);
    let mut out: Vec<SearchResult> = templates()
        .into_iter()
        .filter(|r| {
            let tl = r.title.to_lowercase();
            tl.contains(&ql)
                || pattern
                    .score(Utf32String::from(r.title.as_str()).slice(..), &mut matcher)
                    .map(|s| s >= (ql.len() as u32).saturating_mul(25))
                    .unwrap_or(false)
        })
        .collect();
    out.truncate(4);
    out
}

// Search for apps to install across Flatpak and/or distro PM.
fn search_install(query: &str, pm: PackageManager) -> Vec<SearchResult> {
    let detected = get_detected_pm();
    let use_flatpak = pm.use_flatpak();
    let use_distro = pm.use_distro() && detected.is_some();
    let use_snap = pm.use_snap() && snap_is_available() == Some(true);

    let mut flatpak_results: Vec<SearchResult> = Vec::new();
    let mut distro_results: Vec<SearchResult> = Vec::new();
    let mut snap_results: Vec<SearchResult> = Vec::new();
    let mut loading = false;

    if use_flatpak {
        ensure_flatpak_catalog();
        ensure_installed();
        let installed: std::collections::HashSet<String> = installed_cache()
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, apps)| apps.iter().map(|a| a.app_id.clone()).collect())
            .unwrap_or_default();
        match flatpak_catalog_cache().try_lock() {
            Ok(catalog) => match catalog.as_ref() {
                Some((_, apps)) if !apps.is_empty() => {
                    let matches = fuzzy_apps(query, apps);
                    let mut seen = std::collections::HashSet::new();
                    flatpak_results.extend(
                        matches
                            .iter()
                            .filter(|(app, _)| seen.insert(app.name.to_lowercase()))
                            .filter(|(app, _)| !installed.contains(&app.app_id))
                            .map(|(app, _)| install_result(app)),
                    );
                }
                _ => loading = true,
            },
            Err(_) => loading = true,
        }
    }

    if use_distro {
        let pm_name = detected.as_deref().unwrap_or("");
        ensure_distro_catalog(pm_name.to_string());
        // Warm the installed-package list so we don't offer "Install: x" for
        // something you already have (Flatpak and Snap already filter this).
        ensure_distro_installed(pm_name.to_string());
        let installed = distro_installed_names();
        match distro_catalog_cache().try_lock() {
            Ok(cache) => match cache.as_ref() {
                Some((_, cpm, pkgs)) if cpm == pm_name && !pkgs.is_empty() => {
                    ensure_distro_search(query.to_string(), pkgs.clone());
                    let sc = distro_search_cache().lock().unwrap();
                    match sc.as_ref() {
                        Some((q, matches)) if q == query => {
                            distro_results.extend(
                                matches
                                    .iter()
                                    .filter(|p| !installed.contains(&p.name_lc))
                                    .map(|p| distro_install_result(p, pm_name)),
                            );
                        }
                        _ => {
                            if flatpak_results.is_empty() {
                                loading = true;
                            }
                        }
                    }
                }
                _ => {
                    loading = true;
                }
            },
            Err(_) => {
                loading = true;
            }
        }
    }

    if use_snap {
        ensure_snap_search(query.to_string());
        let sc = snap_search_cache().lock().unwrap();
        match sc.as_ref() {
            Some((q, res)) if q == query => {
                snap_results.extend(res.iter().cloned());
            }
            _ => {
                if flatpak_results.is_empty() && distro_results.is_empty() {
                    loading = true;
                }
            }
        }
    }

    if flatpak_results.is_empty() && distro_results.is_empty() && snap_results.is_empty() && loading {
        return searching_placeholder(&gettext("Searching for \"{query}\"…").replace("{query}", query));
    }

    if flatpak_results.is_empty() && distro_results.is_empty() && snap_results.is_empty() {
        return vec![SearchResult {
            kind: ResultKind::System,
            title: gettext("No apps found for \"{query}\"").replace("{query}", query),
            subtitle: Some(gettext("Check spelling or try a different name").into()),
            icon: Some("package-x-generic-symbolic".into()),
            action: Action::EnterMode("cmd".into()),
            score: 0,
        }];
    }

    // In combined modes, interleave the active sources (each already ordered
    // by relevance) so the best matches from EACH package manager appear at
    // the top — e.g. searching "firefox" surfaces the Flatpak, distro, and
    // Snap builds of Firefox as the first results — instead of listing every
    // match from one source before any from another.
    let sources = [use_flatpak, use_distro, use_snap];
    let mut results: Vec<SearchResult> = Vec::new();
    if sources.iter().filter(|&&s| s).count() > 1 {
        let mut fi = flatpak_results.into_iter();
        let mut di = distro_results.into_iter();
        let mut si = snap_results.into_iter();
        loop {
            let f = fi.next();
            let d = di.next();
            let s = si.next();
            if f.is_none() && d.is_none() && s.is_none() {
                break;
            }
            if let Some(f) = f {
                results.push(f);
            }
            if let Some(d) = d {
                results.push(d);
            }
            if let Some(s) = s {
                results.push(s);
            }
        }
    } else {
        results.extend(flatpak_results);
        results.extend(distro_results);
        results.extend(snap_results);
    }

    results.truncate(20);
    results
}

/// Install suggestions for the default (universal) search.
///
/// Reuses the catalog-backed `search_install`, but keeps only real install
/// actions — dropping the "Searching…"/"No apps found" placeholder rows that
/// would be noise outside the dedicated App mode — caps the count, and re-scores
/// them to sit just below locally-installed apps (which score ≥1500) yet above
/// web (100). Returns empty while catalogs are still warming or nothing matches.
pub fn universal_install(query: &str, pm: PackageManager, limit: usize) -> Vec<SearchResult> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<SearchResult> = search_install(q, pm)
        .into_iter()
        .filter(|r| matches!(r.action, Action::StartOperation { .. }))
        .take(limit)
        .collect();
    for (i, r) in out.iter_mut().enumerate() {
        r.score = 800 - i as i32 * 10;
    }
    out
}

// Search for installed apps to uninstall across Flatpak and distro PM.
// Unlike installing, uninstalling always considers both sources regardless of
// the configured package manager preference, so the user can remove anything
// that's actually installed on the system.
fn search_uninstall(query: &str, _pm: PackageManager) -> Vec<SearchResult> {
    let detected = get_detected_pm();
    let use_flatpak = true;
    let use_distro = detected.is_some();
    let use_snap = snap_is_available() == Some(true);

    let mut results: Vec<SearchResult> = Vec::new();
    let mut loading = false;

    if use_flatpak {
        ensure_installed();
        let cache = installed_cache().lock().unwrap();
        match cache.as_ref() {
            None => {
                loading = true;
            }
            Some((_, apps)) => {
                if query.is_empty() {
                    results.extend(apps.iter().take(8).map(|app| uninstall_result(app)));
                } else {
                    let matches = fuzzy_apps(query, apps);
                    results.extend(matches.iter().map(|(app, _)| uninstall_result(app)));
                }
            }
        }
    }

    if use_distro {
        let pm_name = detected.as_deref().unwrap_or("");
        ensure_distro_installed(pm_name.to_string());
        let cache = distro_installed_cache().lock().unwrap();
        match cache.as_ref() {
            None => {
                loading = true;
            }
            Some((_, pkgs)) => {
                if query.is_empty() {
                    results.extend(
                        pkgs.iter()
                            .take(4)
                            .map(|pkg| distro_uninstall_result(pkg, pm_name)),
                    );
                } else {
                    let matches = fuzzy_distro(query, pkgs);
                    results.extend(
                        matches
                            .iter()
                            .map(|(pkg, _)| distro_uninstall_result(pkg, pm_name)),
                    );
                }
            }
        }
    }

    if use_snap {
        ensure_snap_installed();
        let cache = snap_installed_cache().lock().unwrap();
        match cache.as_ref() {
            None => {
                loading = true;
            }
            Some((_, pkgs)) => {
                if query.is_empty() {
                    results.extend(pkgs.iter().take(4).map(snap_uninstall_result));
                } else {
                    let matches = fuzzy_distro(query, pkgs);
                    results.extend(
                        matches.iter().map(|(pkg, _)| snap_uninstall_result(pkg)),
                    );
                }
            }
        }
    }

    if results.is_empty() && loading {
        return searching_placeholder(&gettext("Scanning installed apps…"));
    }

    if results.is_empty() {
        return vec![SearchResult {
            kind: ResultKind::System,
            title: gettext("No installed app matching \"{query}\"").replace("{query}", query),
            subtitle: Some(gettext("Check spelling or try fewer characters").into()),
            icon: Some("edit-delete-symbolic".into()),
            action: Action::EnterMode("cmd".into()),
            score: 0,
        }];
    }

    results.sort_by(|a, b| b.score.cmp(&a.score));
    results.truncate(20);
    results
}

// ── Software update search ────────────────────────────────────────────────────

pub fn update_results(query: &str, config: &Config) -> Vec<SearchResult> {
    search_updates(query, config)
}

fn search_updates(query: &str, config: &Config) -> Vec<SearchResult> {
    ensure_updates_checked();
    // Clone updates out of the cache so the lock is released before the
    // row builders touch it (std::sync::Mutex is not reentrant — holding
    // the lock across those calls deadlocks).
    let updates: Option<Vec<UpdateInfo>> = update_cache()
        .try_lock()
        .ok()
        .and_then(|g| g.clone())
        .map(|(_, v)| v);
    let mut out: Vec<SearchResult> = Vec::new();
    // A finished system update asked for a reboot → always on top.
    if reboot_pending() {
        out.push(restart_required_row());
    }
    let rest = query.trim();
    let rl = rest.to_lowercase();
    if *update_fetching().lock().unwrap() {
        out.push(check_now_row(
            gettext("Checking for updates…"),
            gettext("Press Enter to check again"),
            "emblem-synchronizing-symbolic",
        ));
    } else {
        match &updates {
            Some(list) if !list.is_empty() => {
                // 1. Scopes — the choices behind "update ", "update
                //    flatpak", "update all", "update distro"...
                let scopes = scopes_from(list);
                let mut matched = false;
                let mut flatpak_scope = false;
                let mut distro_scope = false;
                let mut snap_scope = false;
                for sc in &scopes {
                    if rest.is_empty() || sc.matches(&rl) {
                        out.push(sc.row());
                        matched = true;
                        match sc.key {
                            "flatpak" => flatpak_scope = true,
                            "distro" => distro_scope = true,
                            "snap" => snap_scope = true,
                            _ => {} // "all" already covers every package
                        }
                    }
                }
                // 2. One row per pending package ("update firefox"). Picking
                //    a source scope also lists its packages, so a single one
                //    can still be chosen ("update flatpak" -> Firefox…).
                let in_scope = |u: &UpdateInfo| match u.source.as_str() {
                    "flatpak" => flatpak_scope,
                    "snap" => snap_scope,
                    _ => distro_scope,
                };
                let mut n = 0;
                for u in list {
                    if n >= 10 {
                        break;
                    }
                    if rest.is_empty()
                        || in_scope(u)
                        || u.name.to_lowercase().contains(&rl)
                        || package_display(u).to_lowercase().contains(&rl)
                    {
                        out.push(package_row(u));
                        n += 1;
                        matched = true;
                    }
                }
                if !matched {
                    out.push(SearchResult {
                        kind: ResultKind::System,
                        title: gettext("No matching updates").into(),
                        subtitle: Some(
                            gettext("Type all, flatpak, distro or a package name").into(),
                        ),
                        icon: Some("edit-find-symbolic".into()),
                        action: Action::Noop,
                        score: 1000,
                    });
                }
            }
            _ => {
                out.push(check_now_row(
                    gettext("No updates available"),
                    format!(
                        "{} · {}",
                        gettext("All packages are up to date"),
                        gettext("Press Enter to check again")
                    ),
                    "object-select-symbolic",
                ));
            }
        }
    }
    // Always offer the switch, from search as well as from Settings.
    out.push(update_toggle_row(config));
    // Notice controls live in the list as well: options only appear once
    // the user types "update".
    if let Some(list) = &updates {
        if !list.is_empty() {
            let sig = update_signature(list);
            out.push(SearchResult {
                kind: ResultKind::System,
                title: gettext("Remind tomorrow").into(),
                subtitle: Some(gettext("Hide the update notice for 24 hours").into()),
                icon: Some("document-open-recent-symbolic".into()),
                action: Action::SnoozeUpdates,
                score: 400,
            });
            out.push(SearchResult {
                kind: ResultKind::System,
                title: gettext("Dismiss update notice").into(),
                subtitle: Some(gettext("Hidden until a new update appears").into()),
                icon: Some("window-close-symbolic".into()),
                action: Action::DismissUpdates(sig),
                score: 300,
            });
        }
    }
    out
}

/// Row shown above the updates while a reboot is pending.
/// Top row while a reboot is pending. With updates still to install it
/// becomes "Update & Restart" (upgrade first, then reboot); once
/// everything is applied it is a plain restart.
fn restart_required_row() -> SearchResult {
    if updates_pending() {
        let title = gettext("Update & Restart");
        remember_details(&title, all_update_details());
        SearchResult {
            kind: ResultKind::System,
            title: title.clone(),
            subtitle: Some(gettext("Install the updates, then restart").into()),
            icon: Some("system-reboot-symbolic".into()),
            action: Action::StartOperation {
                title,
                source: "System Update".into(),
                icon: "system-reboot-symbolic".into(),
                args: update_then_restart_args(),
            },
            score: 100_000,
        }
    } else {
        SearchResult {
            kind: ResultKind::System,
            title: gettext("Restart required to finish the update").into(),
            subtitle: None,
            icon: Some("system-reboot-symbolic".into()),
            action: Action::ConfirmRunCommand(crate::search::system::reboot_command()),
            score: 100_000,
        }
    }
}

/// On/off switch for the whole update feature, reachable from search.
fn update_toggle_row(config: &Config) -> SearchResult {
    let on = config.enable_updates;
    SearchResult {
        kind: ResultKind::System,
        title: if on {
            gettext("Disable update checks")
        } else {
            gettext("Enable update checks")
        }
        .into(),
        subtitle: None,
        icon: Some(if on {
            "changes-prevent-symbolic"
        } else {
            "emblem-ok-symbolic"
        }
        .into()),
        action: Action::ToggleUpdates,
        score: 500,
    }
}

/// Split a parser's "name  version" line into (name, version).
fn split_pkg(raw: &str) -> (&str, &str) {
    match raw.find(char::is_whitespace) {
        Some(i) => (raw[..i].trim(), raw[i..].trim()),
        None => (raw, ""),
    }
}

/// The name a package row shows: flatpak keeps its display name, the
/// distro parsers carry "name  version" and the version goes to the
/// subtitle instead.
fn package_display(u: &UpdateInfo) -> String {
    if u.source == "flatpak" {
        u.name.clone()
    } else {
        split_pkg(&u.name).0.to_string()
    }
}

/// Command arguments to update exactly one package of `source`.
fn package_args(source: &str, target: &str) -> Vec<String> {
    let t = target.to_string();
    match source {
        "flatpak" => flatpak_cmd_args(&["update", "--assumeyes", &t]),
        "dnf" => pkexec_cmd_args(vec!["dnf".into(), "upgrade".into(), "-y".into(), t]),
        "apt" => pkexec_cmd_args(vec![
            "apt-get".into(),
            "install".into(),
            "--only-upgrade".into(),
            "-y".into(),
            t,
        ]),
        "pacman" => pkexec_cmd_args(vec!["pacman".into(), "-S".into(), "--noconfirm".into(), t]),
        "zypper" => pkexec_cmd_args(vec!["zypper".into(), "update".into(), "-y".into(), t]),
        "snap" => pkexec_cmd_args(vec!["snap".into(), "refresh".into(), t]),
        _ => vec!["true".to_string()],
    }
}

/// One row per pending package: "Update: firefox" — Enter asks once and
/// then upgrades exactly that package.
fn package_row(u: &UpdateInfo) -> SearchResult {
    let display = package_display(u);
    let (sub, icon, target) = if u.source == "flatpak" {
        let id = u.app_id.clone().unwrap_or_else(|| u.name.clone());
        (format!("flatpak — {id}"), id.clone(), id)
    } else {
        let (_, ver) = split_pkg(&u.name);
        let sub = if ver.is_empty() {
            u.source.clone()
        } else {
            format!("{} — {}", u.source, ver)
        };
        (
            sub,
            "software-update-available-symbolic".to_string(),
            display.clone(),
        )
    };
    let title = gettext("Update: {name}").replace("{name}", &display);
    remember_details(&title, u.details.clone());
    remember_query(&title, format!("update {display}"));
    let args = package_args(&u.source, &target);
    SearchResult {
        kind: ResultKind::System,
        title: title.clone(),
        subtitle: Some(sub),
        icon: Some(icon.clone()),
        action: Action::StartOperation {
            title,
            source: gettext("{source} Update").replace("{source}", &u.source),
            icon,
            args,
        },
        score: 1000,
    }
}

/// A target the user can type after the verb: "update all", "update
/// flatpak", "update distro" (the detected PM), "update snap".
#[derive(Debug)]
struct Scope {
    /// What the user types (and what the ghost suggestion completes to).
    key: &'static str,
    /// Source name `update_cmd_args` understands ("distro" -> detected PM).
    cmd: String,
    /// Typing keywords that select this scope.
    kws: Vec<String>,
    /// Title name: "all packages (flatpak, dnf)", "2 flatpak packages"...
    name: String,
    /// Sources, shown in the row subtitle.
    sources: String,
    /// Package lines this scope would upgrade (the preview list).
    details: Vec<String>,
}

impl Scope {
    /// True when the typed target ("update f") picks this scope.
    fn matches(&self, rl: &str) -> bool {
        self.kws.iter().any(|k| k.starts_with(rl))
    }

    /// The result row for this scope.
    fn row(&self) -> SearchResult {
        let title = gettext("Update: {name}").replace("{name}", &self.name);
        remember_details(&title, self.details.clone());
        remember_query(&title, format!("update {}", self.key));
        let args = update_cmd_args(&self.cmd, None);
        let icon = "software-update-available-symbolic";
        SearchResult {
            kind: ResultKind::System,
            title: title.clone(),
            subtitle: Some(format!("{} — update available", self.sources)),
            icon: Some(icon.to_string()),
            action: Action::StartOperation {
                title,
                source: gettext("{source} Update").replace("{source}", &self.cmd),
                icon: icon.to_string(),
                args,
            },
            score: match self.key {
                "all" => 2000,
                "flatpak" => 1900,
                "distro" => 1850,
                _ => 1800,
            },
        }
    }
}

/// (package count, package lines, sources) for every entry whose source
/// passes `pred`.
fn scope_group(
    list: &[UpdateInfo],
    pred: impl Fn(&str) -> bool,
) -> (usize, Vec<String>, Vec<String>) {
    let mut details = Vec::new();
    let mut sources: Vec<String> = Vec::new();
    for u in list.iter().filter(|u| pred(&u.source)) {
        details.extend(u.details.iter().cloned());
        if !sources.contains(&u.source) {
            sources.push(u.source.clone());
        }
    }
    (details.len(), details, sources)
}

/// The scopes offered for `list`, in typing order: all, flatpak, distro,
/// snap — only the ones with something pending.
fn scopes_from(list: &[UpdateInfo]) -> Vec<Scope> {
    let mut out = Vec::new();
    let pm = list
        .iter()
        .map(|u| u.source.as_str())
        .find(|s| matches!(*s, "dnf" | "apt" | "pacman" | "zypper"))
        .map(str::to_string);

    let (count, details, sources) = scope_group(list, |_| true);
    if count > 0 {
        out.push(Scope {
            key: "all",
            cmd: "all".into(),
            kws: vec!["all".into(), "everything".into()],
            name: gettext("all packages ({sources})")
                .replace("{sources}", &sources.join(", ")),
            sources: sources.join(", "),
            details,
        });
    }
    let (count, details, sources) = scope_group(list, |s| s == "flatpak");
    if count > 0 {
        out.push(Scope {
            key: "flatpak",
            cmd: "flatpak".into(),
            kws: vec!["flatpak".into(), "flathub".into()],
            name: gettext("{n} flatpak packages").replace("{n}", &count.to_string()),
            sources: sources.join(", "),
            details,
        });
    }
    if let Some(pm) = &pm {
        let pm_for_group = pm.clone();
        let (count, details, sources) = scope_group(list, move |s| s == pm_for_group);
        if count > 0 {
            out.push(Scope {
                key: "distro",
                cmd: pm.clone(),
                kws: vec![
                    "distro".into(),
                    "distribution".into(),
                    "system".into(),
                    "sys".into(),
                    pm.clone(),
                ],
                name: gettext("{n} system packages ({pm})")
                    .replace("{n}", &count.to_string())
                    .replace("{pm}", pm),
                sources: sources.join(", "),
                details,
            });
        }
    }
    let (count, details, sources) = scope_group(list, |s| s == "snap");
    if count > 0 {
        out.push(Scope {
            key: "snap",
            cmd: "snap".into(),
            kws: vec!["snap".into(), "snapd".into()],
            name: gettext("{n} snap packages").replace("{n}", &count.to_string()),
            sources: sources.join(", "),
            details,
        });
    }
    out
}

/// The "No updates available" / "Checking for updates..." rows: Enter
/// re-runs the check instead of doing nothing.
fn check_now_row(title: String, subtitle: String, icon: &str) -> SearchResult {
    SearchResult {
        kind: ResultKind::System,
        title,
        subtitle: Some(subtitle),
        icon: Some(icon.to_string()),
        action: Action::CheckUpdates,
        score: 1000,
    }
}

// ── Result builders ───────────────────────────────────────────────────────────

// What to kill: a regular host process (matched by name via pkill), or a
// running Flatpak app instance (killed by app-id via `flatpak kill`).
#[derive(Clone)]
enum KillTarget {
    Process(String),
    Flatpak(String),
}

// Cross-reference running processes and Flatpak instances against indexed
// apps, returning (app display name, kill target) pairs for apps currently
// running. Flatpak matches take priority since `flatpak kill` is more
// reliable than `pkill` for sandboxed apps (whose host process name may not
// match the app's display name).
fn running_apps(
    proc_names: &[String],
    flatpak_ids: &[String],
    apps: &[AppEntry],
) -> Vec<(String, KillTarget)> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for app in apps {
        for id in flatpak_ids {
            let il = id.to_lowercase();
            if app.keywords.iter().any(|k| k.to_lowercase() == il) && seen.insert(app.name.clone())
            {
                out.push((app.name.clone(), KillTarget::Flatpak(id.clone())));
                break;
            }
        }
    }
    for proc in proc_names {
        let pl = proc.to_lowercase();
        for app in apps {
            let nl = app.name.to_lowercase();
            let nl_compact = nl.replace(' ', "");
            let matches =
                nl == pl || nl_compact == pl || app.keywords.iter().any(|k| k.to_lowercase() == pl);
            if matches && seen.insert(app.name.clone()) {
                out.push((app.name.clone(), KillTarget::Process(proc.clone())));
                break;
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn kill_result(name: &str, target: &KillTarget) -> SearchResult {
    let subtitle = match target {
        KillTarget::Process(proc) => format!("pkill -i {}", proc),
        KillTarget::Flatpak(id) => format!("flatpak kill {}", id),
    };
    SearchResult {
        kind: ResultKind::System,
        title: gettext("Kill: {name}").replace("{name}", &name.to_string()),
        subtitle: Some(subtitle),
        icon: Some("process-stop-symbolic".into()),
        action: kill_action(target),
        score: 1000,
    }
}

fn kill_action(target: &KillTarget) -> Action {
    let cmd = match target {
        KillTarget::Process(proc) => {
            let safe = shell_safe(proc);
            if is_sandbox() {
                format!("flatpak-spawn --host pkill -i {}", safe)
            } else {
                format!("pkill -i {}", safe)
            }
        }
        KillTarget::Flatpak(id) => {
            let safe = shell_safe(id);
            if is_sandbox() {
                format!("flatpak-spawn --host flatpak kill {}", safe)
            } else {
                format!("flatpak kill {}", safe)
            }
        }
    };
    Action::RunCommand(cmd)
}

fn install_result(app: &FlatpakApp) -> SearchResult {
    let sub = if app.description.is_empty() {
        gettext("via Flatpak ({app})").replace("{app}", &app.app_id)
    } else {
        gettext("{description} — via Flatpak ({app})").replace("{description}", &app.description).replace("{app}", &app.app_id)
    };
    let args = {
        let mut a = flatpak_cmd_args(&["install", "--user", "--assumeyes"]);
        a.push(app.app_id.clone());
        a
    };
    SearchResult {
        kind: ResultKind::System,
        title: gettext("Install: {name}").replace("{name}", &app.name),
        subtitle: Some(sub),
        // The app-id doubles as an icon name so the row shows the real app icon
        // when available (result_row falls back to a package icon otherwise).
        icon: Some(app.app_id.clone()),
        action: Action::StartOperation {
            title: gettext("Installing {name}").replace("{name}", &app.name),
            source: "Flatpak".into(),
            icon: app.app_id.clone(),
            args,
        },
        score: 1000,
    }
}

fn uninstall_result(app: &FlatpakApp) -> SearchResult {
    let args = {
        let mut a = flatpak_cmd_args(&["uninstall", "--assumeyes"]);
        a.push(app.app_id.clone());
        a
    };
    SearchResult {
        kind: ResultKind::System,
        title: gettext("Uninstall: {name}").replace("{name}", &app.name),
        subtitle: Some(gettext("via Flatpak ({app})").replace("{app}", &app.app_id)),
        icon: Some(app.app_id.clone()),
        action: Action::StartOperation {
            title: gettext("Uninstalling {name}").replace("{name}", &app.name),
            source: "Flatpak".into(),
            icon: app.app_id.clone(),
            args,
        },
        score: 1000,
    }
}

// Build argv for `subargs` run as root via `pkexec`, prefixed with
// `flatpak-spawn --host` when sandboxed. `pkexec` shows a PolicyKit GUI prompt
// asking the user for their password before running the command.
fn pkexec_cmd_args(subargs: Vec<String>) -> Vec<String> {
    let mut v = if is_sandbox() {
        vec![
            "flatpak-spawn".to_string(),
            "--host".to_string(),
            "pkexec".to_string(),
        ]
    } else {
        vec!["pkexec".to_string()]
    };
    v.extend(subargs);
    v
}

fn distro_install_result(pkg: &DistroPackage, pm: &str) -> SearchResult {
    let (inner, label): (Vec<String>, &str) = match pm {
        "apt" => (
            vec![
                "apt-get".into(),
                "install".into(),
                "-y".into(),
                pkg.name.clone(),
            ],
            "apt",
        ),
        "dnf" => (
            vec![
                "dnf".into(),
                "install".into(),
                "-y".into(),
                pkg.name.clone(),
            ],
            "dnf",
        ),
        "pacman" => (
            vec![
                "pacman".into(),
                "-S".into(),
                "--noconfirm".into(),
                pkg.name.clone(),
            ],
            "pacman",
        ),
        "zypper" => (
            vec![
                "zypper".into(),
                "--non-interactive".into(),
                "install".into(),
                pkg.name.clone(),
            ],
            "zypper",
        ),
        _ => {
            return SearchResult {
                kind: ResultKind::System,
                title: gettext("Install: {name}").replace("{name}", &pkg.name),
                subtitle: None,
                icon: Some("package-x-generic-symbolic".into()),
                action: Action::EnterMode("cmd".into()),
                score: 900,
            }
        }
    };
    let sub = if pkg.description.is_empty() {
        format!("via {}", label)
    } else {
        format!("{} — via {}", pkg.description, label)
    };
    // "pkg:<name>:<fallback>" — result_row tries to resolve a real app icon
    // matching the package name (theme + host icon dirs), falling back to
    // a generic package icon if none is found.
    let icon = format!("pkg:{}:package-x-generic-symbolic", pkg.name);
    SearchResult {
        kind: ResultKind::System,
        title: gettext("Install: {name}").replace("{name}", &pkg.name),
        subtitle: Some(sub),
        icon: Some(icon.clone()),
        action: Action::StartOperation {
            title: format!("Installing {}", pkg.name),
            source: label.into(),
            icon,
            args: pkexec_cmd_args(inner),
        },
        score: 900,
    }
}

fn distro_uninstall_result(pkg: &DistroPackage, pm: &str) -> SearchResult {
    let (inner, label): (Vec<String>, &str) = match pm {
        "apt" => (
            vec![
                "apt-get".into(),
                "remove".into(),
                "-y".into(),
                pkg.name.clone(),
            ],
            "apt",
        ),
        "dnf" => (
            vec!["dnf".into(), "remove".into(), "-y".into(), pkg.name.clone()],
            "dnf",
        ),
        "pacman" => (
            vec![
                "pacman".into(),
                "-R".into(),
                "--noconfirm".into(),
                pkg.name.clone(),
            ],
            "pacman",
        ),
        "zypper" => (
            vec![
                "zypper".into(),
                "--non-interactive".into(),
                "remove".into(),
                pkg.name.clone(),
            ],
            "zypper",
        ),
        _ => {
            return SearchResult {
                kind: ResultKind::System,
                title: format!("Uninstall: {}", pkg.name),
                subtitle: None,
                icon: Some("edit-delete-symbolic".into()),
                action: Action::EnterMode("cmd".into()),
                score: 900,
            }
        }
    };
    let sub = if pkg.description.is_empty() {
        format!("via {}", label)
    } else {
        format!("{} — via {}", pkg.description, label)
    };
    let icon = format!("pkg:{}:edit-delete-symbolic", pkg.name);
    SearchResult {
        kind: ResultKind::System,
        title: format!("Uninstall: {}", pkg.name),
        subtitle: Some(sub),
        icon: Some(icon.clone()),
        action: Action::StartOperation {
            title: format!("Uninstalling {}", pkg.name),
            source: label.into(),
            icon,
            args: pkexec_cmd_args(inner),
        },
        score: 900,
    }
}

fn snap_install_result(pkg: DistroPackage) -> SearchResult {
    let sub = if pkg.description.is_empty() {
        "via Snap".into()
    } else {
        format!("{} — via snap", pkg.description)
    };
    let icon = format!("pkg:{}:package-x-generic-symbolic", pkg.name);
    SearchResult {
        kind: ResultKind::System,
        title: gettext("Install: {name}").replace("{name}", &pkg.name),
        subtitle: Some(sub),
        icon: Some(icon.clone()),
        action: Action::StartOperation {
            title: format!("Installing {}", pkg.name),
            source: "snap".into(),
            icon,
            args: pkexec_cmd_args(vec![
                "snap".into(),
                "install".into(),
                pkg.name.clone(),
            ]),
        },
        score: 900,
    }
}

fn snap_uninstall_result(pkg: &DistroPackage) -> SearchResult {
    let icon = format!("pkg:{}:edit-delete-symbolic", pkg.name);
    SearchResult {
        kind: ResultKind::System,
        title: format!("Uninstall: {}", pkg.name),
        subtitle: Some(gettext("via snap").into()),
        icon: Some(icon.clone()),
        action: Action::StartOperation {
            title: format!("Uninstalling {}", pkg.name),
            source: "snap".into(),
            icon,
            args: pkexec_cmd_args(vec!["snap".into(), "remove".into(), pkg.name.clone()]),
        },
        score: 900,
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn matches_verb(verb: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|c| *c == verb || c.starts_with(verb) || verb.starts_with(c))
}

fn shell_safe(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::chained_script;

    /// Fill the cache so `update_verb_rows` doesn't kick off a live
    /// dnf/flatpak check inside the test.
    fn prime_update_cache() {
        let mut c = super::update_cache().lock().unwrap();
        if c.is_none() {
            *c = Some((std::time::Instant::now(), Vec::new()));
        }
    }

    #[test]
    fn update_verb_is_recognised_in_plain_search() {
        let _g = cache_guard();
        prime_update_cache();
        let cfg = crate::config::Config::default();
        for q in ["update", "updates", "upd", "upgrade", "upg", "update flatpak"] {
            assert!(super::update_verb_rows(q, &cfg).is_some(), "should match {q:?}");
        }
        for q in ["", "install firefox", "daily", "sup", "updateing", "u"] {
            assert!(super::update_verb_rows(q, &cfg).is_none(), "should NOT match {q:?}");
        }
        // A still-incomplete verb shows the very same rows as the complete
        // verb — one update row in every state, no separate suggestion row.
        let titles_of = |q: &str| -> Vec<String> {
            super::update_verb_rows(q, &cfg)
                .expect("rows")
                .into_iter()
                .map(|r| r.title)
                .collect()
        };
        let full = titles_of("update");
        for q in ["up", "upda", "updat", "upgrad"] {
            assert_eq!(
                titles_of(q),
                full,
                "{q:?} must show the same rows as \"update\""
            );
        }
    }

    #[test]
    fn verb_completion_only_completes_incomplete_verbs() {
        assert_eq!(super::verb_completion("up").as_deref(), Some("update "));
        assert_eq!(super::verb_completion("upd").as_deref(), Some("update "));
        assert_eq!(super::verb_completion("updat").as_deref(), Some("update "));
        assert_eq!(super::verb_completion("upg").as_deref(), Some("upgrade "));
        assert_eq!(super::verb_completion("upgrad").as_deref(), Some("upgrade "));
        assert_eq!(super::verb_completion("update"), None);
        assert_eq!(super::verb_completion("updates"), None);
        assert_eq!(super::verb_completion("upgrade"), None);
        assert_eq!(super::verb_completion("update f"), None);
        // Below two characters other words are at play ("u" → uninstall).
        assert_eq!(super::verb_completion("u"), None);
        assert_eq!(super::verb_completion(""), None);
    }

    #[test]
    fn update_context_follows_the_verb() {
        for q in ["update", "update flatpak", "upd", "upgrade now", "  UPDATE  "] {
            assert!(super::in_update_context(q), "{q:?}");
        }
        for q in ["", "install", "up", "find foo"] {
            assert!(!super::in_update_context(q), "{q:?}");
        }
    }

    #[test]
    fn one_cache_entry_per_package_and_scopes_on_top() {
        // The cache holds one entry per *real* package: aggregates are
        // built for display only, so they can never inflate the
        // "N updates available" badge.
        let solo = super::assemble_updates(
            vec![],
            [
                ("dnf", vec!["vim.x86_64  2:9.2.1129-1.fc44".to_string()]),
                ("apt", vec![]),
                ("pacman", vec![]),
                ("zypper", vec![]),
            ],
            vec![],
        );
        assert_eq!(solo.len(), 1, "solo list: {solo:?}");
        assert_eq!(solo[0].source, "dnf");
        // The entry carries its package line for the preview.
        assert_eq!(solo[0].details, vec!["vim.x86_64  2:9.2.1129-1.fc44"]);

        // Two sources: still two entries, and the derived scopes cover
        // every package exactly once.
        let duo = super::assemble_updates(
            vec![],
            [
                ("dnf", vec!["vim.x86_64  9.2".to_string()]),
                ("apt", vec![]),
                ("pacman", vec![]),
                ("zypper", vec![]),
            ],
            vec!["core22  2024".to_string()],
        );
        assert_eq!(duo.len(), 2, "dnf + snap: {duo:?}");
        let scopes = super::scopes_from(&duo);
        let keys: Vec<&str> = scopes.iter().map(|s| s.key).collect();
        assert_eq!(keys, vec!["all", "distro", "snap"], "{scopes:?}");
        assert_eq!(scopes[0].details.len(), 2, "all scope merges both");
        assert_eq!(scopes[1].cmd, "dnf", "distro scope runs the detected PM");
        assert!(scopes[0].matches("a"));
        assert!(scopes[1].matches("d") && scopes[1].matches("dnf"));
        assert!(scopes[2].matches("sn"));
        assert!(!scopes[2].matches("f"));
    }

    #[test]
    fn update_rows_offer_scopes_packages_and_ghost_queries() {
        let _g = cache_guard();
        {
            let mut c = super::update_cache().lock().unwrap();
            *c = Some((
                std::time::Instant::now(),
                vec![
                    super::UpdateInfo {
                        source: "flatpak".into(),
                        app_id: Some("org.mozilla.firefox".into()),
                        name: "Firefox".into(),
                        details: vec!["Firefox".into()],
                    },
                    super::UpdateInfo {
                        source: "dnf".into(),
                        app_id: None,
                        name: "vim.x86_64  2:9.2.1129-1.fc44".into(),
                        details: vec!["vim.x86_64  2:9.2.1129-1.fc44".into()],
                    },
                ],
            ));
        }
        let cfg = crate::config::Config::default();

        // "update" → scopes first, then one row per pending package.
        let rows = super::update_results("", &cfg);
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert!(titles.iter().any(|t| t.contains("all packages (")), "{titles:?}");
        assert!(titles.iter().any(|t| t.contains("1 flatpak packages")), "{titles:?}");
        assert!(titles.iter().any(|t| t.contains("1 system packages (dnf)")), "{titles:?}");
        assert!(titles.contains(&"Update: Firefox"), "{titles:?}");
        assert!(titles.contains(&"Update: vim.x86_64"), "{titles:?}");
        let all_i = titles.iter().position(|t| t.contains("all packages")).unwrap();
        let pkg_i = titles.iter().position(|t| *t == "Update: Firefox").unwrap();
        assert!(all_i < pkg_i, "scope rows rank above packages: {titles:?}");

        // The ghost suggestions each row completes to.
        let all_title = titles.iter().find(|t| t.contains("all packages")).unwrap();
        assert_eq!(super::query_for(all_title).as_deref(), Some("update all"));
        assert_eq!(super::query_for("Update: Firefox").as_deref(), Some("update Firefox"));
        assert_eq!(super::query_for("no such row"), None);

        // "update flatpak" → the flatpak scope + its package, nothing distro.
        let rows = super::update_results("flatpak", &cfg);
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert!(titles.iter().any(|t| t.contains("1 flatpak packages")), "{titles:?}");
        assert!(titles.contains(&"Update: Firefox"), "{titles:?}");
        assert!(!titles.iter().any(|t| t.contains("system packages")), "{titles:?}");

        // "update d|di|distro|dnf|system" all pick the distro scope.
        for q in ["d", "di", "distro", "dnf", "system"] {
            let rows = super::update_results(q, &cfg);
            assert!(
                rows.iter().any(|r| r.title.contains("system packages (dnf)")),
                "{q:?} should select the distro scope: {:?}",
                rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>()
            );
        }

        // "update fire" → the package row itself.
        let rows = super::update_results("fire", &cfg);
        assert_eq!(rows[0].title, "Update: Firefox");
        // …and an unknown target says so instead of showing nothing.
        let rows = super::update_results("zzz", &cfg);
        assert!(rows.iter().any(|r| r.title.contains("No matching updates")), "{rows:?}");
    }

    #[test]
    fn no_updates_offers_a_recheck_on_enter() {
        let _g = cache_guard();
        {
            let mut c = super::update_cache().lock().unwrap();
            *c = Some((std::time::Instant::now(), Vec::new()));
        }
        let cfg = crate::config::Config::default();
        let rows = super::update_results("", &cfg);
        let row = rows
            .iter()
            .find(|r| r.title.contains("No updates available"))
            .expect("no-updates row");
        assert!(
            matches!(row.action, crate::search::Action::CheckUpdates),
            "Enter re-checks: {:?}",
            row.action
        );
    }

    #[test]
    fn scope_shortcuts_only_fire_with_something_pending() {
        let _g = cache_guard();
        {
            let mut c = super::update_cache().lock().unwrap();
            *c = Some((std::time::Instant::now(), Vec::new()));
        }
        assert!(super::update_scope_args("all").is_none(), "empty cache");
        assert!(super::update_scope_args("flatpak").is_none());
        {
            let mut c = super::update_cache().lock().unwrap();
            *c = Some((
                std::time::Instant::now(),
                vec![super::UpdateInfo {
                    source: "dnf".into(),
                    app_id: None,
                    name: "vim.x86_64  2:9.2".into(),
                    details: vec!["vim.x86_64  2:9.2".into()],
                }],
            ));
        }
        let all = super::update_scope_args("all").expect("all runs");
        assert!(all.join(" ").contains("dnf upgrade"), "{all:?}");
        let sys = super::update_scope_args("distro").expect("distro runs");
        assert!(sys.join(" ").contains("dnf"), "{sys:?}");
        assert!(super::update_scope_args("flatpak").is_none(), "nothing flatpak pending");
        assert!(super::update_scope_args("snap").is_none(), "nothing snap pending");
    }

    #[test]
    fn package_rows_build_per_source_commands() {
        let dnf = super::package_args("dnf", "vim.x86_64");
        assert!(dnf.contains(&"upgrade".to_string()), "{dnf:?}");
        assert!(dnf.contains(&"vim.x86_64".to_string()), "{dnf:?}");
        let apt = super::package_args("apt", "firefox");
        assert!(apt.contains(&"--only-upgrade".to_string()), "{apt:?}");
        let pacman = super::package_args("pacman", "vim");
        assert!(pacman.contains(&"-S".to_string()), "{pacman:?}");
        let fp = super::package_args("flatpak", "org.mozilla.firefox");
        assert!(fp.contains(&"org.mozilla.firefox".to_string()), "{fp:?}");
        assert!(fp.contains(&"update".to_string()), "{fp:?}");
        let snap = super::package_args("snap", "core22");
        assert!(snap.contains(&"refresh".to_string()), "{snap:?}");

        // A single-package pacman run has the same argv as an install —
        // the row title is what marks it as an update.
        assert!(!super::is_update_op(&pacman));
        assert!(super::is_update_run("Update: vim", &pacman));
        assert!(!super::is_update_run("Install: vim", &pacman));
        assert!(super::is_update_run("Update & Restart", &super::update_cmd_args("dnf", None)));
    }

    #[test]
    fn update_operations_are_recognised() {
        assert!(super::is_update_op(&[
            "pkexec".into(),
            "dnf".into(),
            "upgrade".into(),
            "-y".into()
        ]));
        assert!(super::is_update_op(&["flatpak".into(), "update".into()]));
        assert!(super::is_update_op(&["pacman".into(), "-Syu".into()]));
        assert!(super::is_update_op(&["pkexec".into(), "snap".into(), "refresh".into()]));
        assert!(!super::is_update_op(&["flatpak".into(), "install".into(), "org.x.Y".into()]));
    }

    /// Serialize the tests that seed the shared update cache (tests run on
    /// parallel threads and the cache is one static slot).
    fn cache_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
    }

    fn seed_updates() {
        let mut c = super::update_cache().lock().unwrap();
        *c = Some((
            std::time::Instant::now(),
            vec![super::UpdateInfo {
                source: "dnf".into(),
                app_id: None,
                name: "vim.x86_64  2:9.2.1129-1.fc44".to_string(),
                details: vec!["vim.x86_64  2:9.2.1129-1.fc44".to_string()],
            }],
        ));
    }

    #[test]
    fn package_list_parsers_extract_name_and_version() {
        assert_eq!(
            super::parse_dnf_updates(
                "vim.x86_64      2:9.2.1129-1.fc44   updates\nmanifold.x86_64 3.5.3-1.fc44 updates\n\n"
            ),
            vec![
                "vim.x86_64  2:9.2.1129-1.fc44",
                "manifold.x86_64  3.5.3-1.fc44"
            ]
        );
        assert_eq!(
            super::parse_apt_updates(
                "Listing...\nfirefox/x86_64 130.0-1.fc44 fedora [upgradable from: 129.0-1]\n"
            ),
            vec!["firefox  130.0-1.fc44"]
        );
        assert_eq!(
            super::parse_pacman_updates("vim 9.2-1 -> 9.3-1\n"),
            vec!["vim 9.2-1 → 9.3-1"]
        );
        assert_eq!(
            super::parse_snap_updates(
                "Name    Version  Rev  Tracking  Notes\ncore22  2024     1234 latest    -\n========\n"
            ),
            vec!["core22  2024"]
        );
        assert_eq!(
            super::parse_zypper_updates(
                "S | Repository | Name   | Current | Available | Repository\n                 ---+------------+--------+---------+-----------+----------\n                   | repo       | glibc  | 2.39    | 2.40      | repo\n"
            ),
            vec!["glibc  2.39 → 2.40"]
        );
        // Command missing / no output → no rows (not an error).
        assert!(super::parse_dnf_updates("").is_empty());
        assert!(super::parse_apt_updates("Listing...\n").is_empty());
    }

    #[test]
    fn update_list_carries_notice_controls_and_status() {
        let _g = cache_guard();
        seed_updates();
        let cfg = crate::config::Config::default();
        let rows = super::update_results("", &cfg);
        assert!(
            rows.iter().any(|r| matches!(r.action, crate::search::Action::SnoozeUpdates)),
            "Remind tomorrow row"
        );
        assert!(
            rows.iter()
                .any(|r| matches!(r.action, crate::search::Action::DismissUpdates(_))),
            "Dismiss update notice row"
        );
        assert!(
            rows.iter()
                .any(|r| matches!(r.action, crate::search::Action::ToggleUpdates)),
            "enable/disable row"
        );
        assert!(super::updates_pending());
        assert_eq!(super::update_status_text(), crate::i18n::gettext("One update available"));
    }

    #[test]
    fn status_reports_no_updates_for_an_empty_list() {
        let _g = cache_guard();
        {
            let mut c = super::update_cache().lock().unwrap();
            *c = Some((std::time::Instant::now(), Vec::new()));
        }
        // The Settings status row says "No updates available" — never
        // "0 updates available".
        assert_eq!(
            super::update_status_text(),
            crate::i18n::gettext("No updates available")
        );
        assert!(!super::updates_checking());
    }

    #[test]
    fn update_and_restart_chains_after_the_update() {
        // Single source → a plain argv chain becomes a shell chain.
        let base = super::update_cmd_args("dnf", None);
        let args = super::update_then_restart_args_for(&base);
        assert_eq!(args[0], "sh");
        assert_eq!(args[1], "-c");
        let script = &args[2];
        assert!(script.ends_with("&& systemctl reboot"), "{script}");
        assert!(script.contains("dnf"), "{script}");
        // …and the reboot check recognises the chain as an update run.
        assert!(super::is_update_op(&args));

        // Multi source (already a shell script) → append to *that* script.
        let chained = vec!["sh".to_string(), "-c".to_string(), "a && b".to_string()];
        let args = super::update_then_restart_args_for(&chained);
        assert_eq!(args[2], "a && b && systemctl reboot");
    }

    #[test]
    fn preview_details_round_trip() {
        super::remember_details(
            "Update: system packages (dnf)",
            vec!["vim.x86_64  2:9.2.1129-1.fc44".to_string()],
        );
        assert_eq!(
            super::details_for("Update: system packages (dnf)"),
            vec!["vim.x86_64  2:9.2.1129-1.fc44".to_string()]
        );
        assert!(super::details_for("nonsense row").is_empty());
    }

    #[test]
    fn update_signature_ignores_order() {
        let a = super::UpdateInfo {
            source: "dnf".into(),
            app_id: None,
            name: "system packages (dnf)".into(),
            details: Vec::new(),
        };
        let b = super::UpdateInfo {
            source: "flatpak".into(),
            app_id: Some("org.x.Y".into()),
            name: "Y".into(),
            details: Vec::new(),
        };
        assert_eq!(
            super::update_signature(&[a.clone(), b.clone()]),
            super::update_signature(&[b, a])
        );
    }

    #[test]
    fn chained_update_script_carries_progress_markers() {
        let script = chained_script(&[
            ("flatpak", "flatpak update --assumeyes".to_string()),
            ("dnf", "pkexec dnf upgrade -y".to_string()),
            ("snap", "pkexec snap refresh".to_string()),
        ]);
        assert_eq!(&script[..2], &["sh".to_string(), "-c".to_string()]);
        assert_eq!(
            script[2],
            "echo __spotty_part_1_3_flatpak__ && flatpak update --assumeyes \
             && echo __spotty_part_2_3_dnf__ && pkexec dnf upgrade -y \
             && echo __spotty_part_3_3_snap__ && pkexec snap refresh"
        );
        // Markers must be standalone echo arguments (no quoting needed).
        assert!(script[2].starts_with("echo __spotty_part_1_3_flatpak__ && "));
    }
}

// ── Update notice + reboot state (badge/banner near the orb) ─────────────────

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Stable signature of an update set: dismissing hides exactly this set —
/// a *new* update re-raises the notice.
fn update_signature(updates: &[UpdateInfo]) -> String {
    let mut keys: Vec<String> = updates
        .iter()
        .map(|u| format!("{}:{}:{}", u.source, u.app_id.as_deref().unwrap_or(""), u.name))
        .collect();
    keys.sort();
    keys.join("|")
}

/// The update notice as (count, signature): None when there is nothing to
/// show — no updates, the feature or its notification is off, the user
/// snoozed it, or this exact set was dismissed.
pub fn update_notice(cfg: &Config) -> Option<(usize, String)> {
    if !cfg.enable_updates || !cfg.update_notification {
        return None;
    }
    if cfg.update_snooze_until > now_epoch() {
        return None;
    }
    let updates = {
        let g = update_cache().lock().ok()?;
        match g.as_ref() {
            Some((_, v)) => v.clone(),
            None => return None,
        }
    };
    if updates.is_empty() {
        return None;
    }
    let sig = update_signature(&updates);
    if cfg.update_dismissed_sig == sig {
        return None;
    }
    Some((updates.len(), sig))
}

/// Whether the machine wants a reboot after a finished update: the classic
/// Debian/Ubuntu flag, or dnf's `needs-restarting -r` (exit 1 = reboot).
/// Whether the machine wants a reboot after a finished update. Checks, in
/// order: the Debian/Ubuntu flag, dnf's `needs-restarting -r` (exit 1 =
/// reboot) and — since Fedora ships neither by default — whether a newer
/// kernel than the running one is installed (1 = reboot applies it).
/// Live reboot detection — evaluated at startup and after every update
/// operation, kept in memory only. Deliberately NOT persisted: comparing
/// package install times with the boot time means the notice clears
/// itself after the user actually reboots.
fn detect_reboot() -> bool {
    if std::path::Path::new("/run/reboot-required").exists() {
        return true;
    }
    // Debian/Ubuntu flag above; Fedora has neither /run/reboot-required
    // nor (by default) needs-restarting — so fall back to: did any
    // reboot-relevant package get installed *after* the current boot?
    let script = r#"
if command -v needs-restarting >/dev/null 2>&1; then
    needs-restarting -r >/dev/null 2>&1; echo $?; exit 0
fi
if command -v rpm >/dev/null 2>&1; then
    boot=$(($(date +%s) - $(cut -d. -f1 /proc/uptime)))
    latest=$(rpm -q --qf '%{INSTALLTIME}\n' kernel-core glibc systemd microcode_ctl 2>/dev/null \
             | grep -E '^[0-9]+$' | sort -n | tail -1)
    if [ -n "$latest" ] && [ "$latest" -gt "$boot" ]; then echo 1; else echo 0; fi
    exit 0
fi
echo 127
"#;
    crate::app::run_host_shell_command(script)
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
        .unwrap_or(false)
}

/// Re-evaluate the reboot notice (off-thread: rpm queries take a moment).
pub fn refresh_reboot_state() {
    std::thread::spawn(|| {
        let required = detect_reboot();
        REBOOT_REQUIRED.store(required, Ordering::SeqCst);
        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

/// True when a finished update still needs a reboot to take full effect.
pub fn reboot_pending() -> bool {
    REBOOT_REQUIRED.load(Ordering::SeqCst)
}

/// True when these operation args are an update run (dnf upgrade, flatpak
/// update, pacman -Syu, zypper update, snap refresh) — used to decide
/// whether to run the reboot check afterwards.
pub fn is_update_op(args: &[String]) -> bool {
    args.iter().any(|a| {
        a == "upgrade"
            || a == "update"
            || a == "-Syu"
            || a == "refresh"
            // `sh -c` chains (e.g. "pkexec dnf upgrade -y && systemctl reboot")
            || a.contains("dnf upgrade")
            || a.contains("apt-get upgrade")
            || a.contains("flatpak update")
            || a.contains("pacman -Syu")
            || a.contains("zypper update")
            || a.contains("snap refresh")
    })
}

/// True when a whole operation is a package update: recognised by its argv
/// or by its title ("Update: ..."). The argv of a single-package pacman run
/// (`pacman -S pkg`) is identical to an install, so the title is what
/// disambiguates it.
pub fn is_update_run(title: &str, args: &[String]) -> bool {
    title.starts_with("Update") || is_update_op(args)
}

/// Args for "Update & Restart": run the update chain, reboot once it
/// succeeded. Only offered while updates are still pending, so the chain
/// is never empty.
pub fn update_then_restart_args() -> Vec<String> {
    update_then_restart_args_for(&update_cmd_args("all", None))
}

/// Pure helper (testable): turn update argv/sh-script args into
/// "update && systemctl reboot".
fn update_then_restart_args_for(base: &[String]) -> Vec<String> {
    let script = match base.first().map(String::as_str) {
        Some("sh") if base.len() >= 3 => base[2].clone(),
        // Plain argv (pkexec dnf upgrade -y …) — fixed strings, no user input.
        _ => base.join(" "),
    };
    vec!["sh".into(), "-c".into(), format!("{script} && systemctl reboot")]
}

/// Merged package list of every pending update (the aggregate row's own
/// details are excluded — they duplicate their sources).
fn all_update_details() -> Vec<String> {
    update_cache()
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref().map(|(_, v)| {
                v.iter()
                    .filter(|u| u.source != "all")
                    .flat_map(|u| u.details.iter().cloned())
                    .collect()
            })
        })
        .unwrap_or_default()
}

static NOTIFIED_SIG: OnceLock<Mutex<String>> = OnceLock::new();
/// Whether a reboot is pending (live — see [`refresh_reboot_state`]).
static REBOOT_REQUIRED: AtomicBool = AtomicBool::new(false);

fn notified_sig() -> &'static Mutex<String> {
    NOTIFIED_SIG.get_or_init(|| Mutex::new(String::new()))
}

/// Desktop notification when Spotty is hidden and this update set is new —
/// same eligibility rules as the in-app notice, plus "not notified yet".
fn notify_if_new(updates: &[UpdateInfo]) {
    if updates.is_empty() || !crate::app::is_search_window_hidden() {
        return;
    }
    let sig = update_signature(updates);
    crate::app::with_state(|st| {
        let cfg = st.config.borrow();
        if update_notice(&cfg).map(|(_, s)| s) != Some(sig.clone()) {
            return;
        }
        let mut seen = notified_sig().lock().unwrap();
        if *seen == sig {
            return;
        }
        *seen = sig.clone();
        crate::app::send_desktop_notification(
            "Spotty",
            &gettext("{n} updates available").replace("{n}", &updates.len().to_string()),
        );
    });
}


/// "Remind tomorrow": hide the update notice for 24 hours.
pub fn snooze_update_notice() {
    crate::app::with_state(|st| {
        let mut c = st.config.borrow_mut();
        c.update_snooze_until = now_epoch() + 24 * 3600;
        c.save();
    });
    crate::app::refresh_search_window();
}

/// "Dismiss": hide exactly this update set — a *new* update re-raises it.
pub fn dismiss_update_notice(sig: &str) {
    crate::app::with_state(|st| {
        let mut c = st.config.borrow_mut();
        c.update_dismissed_sig = sig.to_string();
        c.save();
    });
    crate::app::refresh_search_window();
}

// ── Preview details + Settings helpers ───────────────────────────────────────

static DETAILS: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();

fn details_store() -> &'static Mutex<HashMap<String, Vec<String>>> {
    DETAILS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Row title → package list, recorded as update rows are built (the
/// preview pane looks the list up by the title it sees on the row).
fn remember_details(title: &str, details: Vec<String>) {
    if let Ok(mut g) = details_store().lock() {
        if g.len() > 200 {
            g.clear();
        }
        g.insert(title.to_string(), details);
    }
}

/// Package list for a row title (empty when unknown / no details).
pub fn details_for(title: &str) -> Vec<String> {
    details_store()
        .lock()
        .ok()
        .and_then(|g| g.get(title).cloned())
        .unwrap_or_default()
}

/// Row title -> the exact query the row runs, so ghost text can complete
/// "update f" to "update flatpak" while typing (see `candidate_for`).
static QUERIES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn query_store() -> &'static Mutex<HashMap<String, String>> {
    QUERIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the canonical query for an update row title (pair with
/// `remember_details`).
pub fn remember_query(title: &str, query: String) {
    if let Ok(mut g) = query_store().lock() {
        if g.len() > 200 {
            g.clear();
        }
        g.insert(title.to_string(), query);
    }
}

/// The canonical query for a row title, when it is an update row.
pub fn query_for(title: &str) -> Option<String> {
    query_store()
        .lock()
        .ok()
        .and_then(|g| g.get(title).cloned())
}

/// One-line status for Settings → Updates (and the search rows).
pub fn update_status_text() -> String {
    if updates_checking() {
        return gettext("Checking for updates…");
    }
    match update_cache()
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|(_, v)| v.len()))
    {
        Some(0) => gettext("No updates available"),
        Some(1) => gettext("One update available"),
        Some(n) => gettext("{n} updates available").replace("{n}", &n.to_string()),
        None => gettext("Checking for updates…"),
    }
}

/// True while a background update check is running — Settings' refresh
/// button shows itself as busy for that time.
pub fn updates_checking() -> bool {
    *update_fetching().lock().unwrap()
}

/// Signature of the currently cached update set, if any — what Settings'
/// "Dismiss update notice" hides.
pub fn dismiss_current_update_notice() {
    let sig = update_cache()
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|(_, v)| update_signature(v)));
    if let Some(sig) = sig {
        dismiss_update_notice(&sig);
    }
}

/// True when the cached update list has at least one entry — gates the
/// notice rows in Settings → Updates.
pub fn updates_pending() -> bool {
    update_cache()
        .lock()
        .ok()
        .map(|g| g.as_ref().is_some_and(|(_, v)| !v.is_empty()))
        .unwrap_or(false)
}
