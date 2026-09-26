// CMD trigger mode: kill running processes, install/uninstall/search Flatpak+distro apps.
// Suggestions are built from async-fetched caches so the UI never blocks.

use crate::config::PackageManager;
use crate::index::AppEntry;
use crate::search::{Action, ResultKind, SearchResult};
use gtk::glib;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32String};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ── Data types ────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct UpdateInfo {
    source: String,
    app_id: Option<String>,
    name: String,
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

/// Result for the universal search bar when the user types "update"/"upgrade".
/// Kicks off the background check if stale, returns a placeholder while fetching,
/// and once the cache is ready, returns the "Update: all packages" action.
pub fn update_all_result() -> Vec<SearchResult> {
    ensure_updates_checked();
    let updates: Vec<UpdateInfo> = match update_cache()
        .try_lock()
        .ok()
        .and_then(|g| g.clone())
        .map(|(_, v)| v)
    {
        Some(v) => v,
        None => {
            if *update_fetching().lock().unwrap() {
                return searching_placeholder("Checking for updates…");
            }
            return vec![SearchResult {
                kind: ResultKind::System,
                title: "No updates available".into(),
                subtitle: Some("All packages are up to date".into()),
                icon: Some("system-software-update".into()),
                action: Action::EnterMode("cmd".into()),
                score: 1000,
            }];
        }
    };
    if updates.is_empty() {
        return vec![SearchResult {
            kind: ResultKind::System,
            title: "No updates available".into(),
            subtitle: Some("All packages are up to date".into()),
            icon: Some("system-software-update".into()),
            action: Action::EnterMode("cmd".into()),
            score: 1000,
        }];
    }
    let args = update_cmd_args("all", None);
    vec![SearchResult {
        kind: ResultKind::System,
        title: "Update: all packages".into(),
        subtitle: Some("Install available system updates".into()),
        icon: Some("system-software-update".into()),
        action: Action::StartOperation {
            title: "Update: all packages".into(),
            source: "System Update".into(),
            icon: "system-software-update".into(),
            args,
        },
        score: 1000,
    }]
}

fn ensure_updates_checked() {
    {
        let c = update_cache().lock().unwrap();
        if let Some((t, _)) = c.as_ref() {
            if t.elapsed() < Duration::from_secs(5 * 60) {
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
        *update_cache().lock().unwrap() = Some((Instant::now(), updates));
        *update_fetching().lock().unwrap() = false;
        glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
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
    let mut out = Vec::new();

    // Flatpak: per-app + aggregate
    let fp_updates = fetch_flatpak_updates();
    for (app_id, name) in &fp_updates {
        out.push(UpdateInfo {
            source: "flatpak".into(),
            app_id: Some(app_id.clone()),
            name: name.clone(),
        });
    }
    if fp_updates.len() > 1 {
        out.push(UpdateInfo {
            source: "flatpak".into(),
            app_id: None,
            name: format!("{} flatpak packages", fp_updates.len()),
        });
    }

    // System packages — check each known PM independently.
    // Each check function returns false when the command doesn't exist,
    // so there's no race with async PM detection.
    for pm in &["dnf", "apt", "pacman", "zypper"] {
        let has = match *pm {
            "dnf" => dnf_has_updates(),
            "apt" => apt_has_updates(),
            "pacman" => pacman_has_updates(),
            "zypper" => zypper_has_updates(),
            _ => false,
        };
        if has {
            out.push(UpdateInfo {
                source: (*pm).into(),
                app_id: None,
                name: format!("system packages ({pm})"),
            });
        }
    }

    // Snap
    if snap_has_updates() {
        out.push(UpdateInfo {
            source: "snap".into(),
            app_id: None,
            name: "snap packages".into(),
        });
    }

    // "Update all" at the top
    if !out.is_empty() {
        let mut parts: Vec<&str> = Vec::new();
        if !fp_updates.is_empty() {
            parts.push("flatpak");
        }
        for pm in &["dnf", "apt", "pacman", "zypper"] {
            if out.iter().any(|u| u.source == *pm) {
                parts.push(pm);
            }
        }
        if out.iter().any(|u| u.source == "snap") {
            parts.push("snap");
        }
        out.insert(
            0,
            UpdateInfo {
                source: "all".into(),
                app_id: None,
                name: format!("all packages ({})", parts.join(", ")),
            },
        );
    }

    out
}

fn fetch_flatpak_updates() -> Vec<(String, String)> {
    let mut all = Vec::new();
    for scope in ["--user", "--system"] {
        let raw = host_command("flatpak")
            .args([
                scope,
                "remote-ls",
                "--updates",
                "--app",
                "--columns=application,name",
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
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

fn dnf_has_updates() -> bool {
    host_command("dnf")
        .args(["check-update", "-q"])
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(false)
}

fn apt_has_updates() -> bool {
    let raw = host_command("apt")
        .args(["list", "--upgradable"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    raw.lines().filter(|l| !l.starts_with("Listing")).count() > 0
}

fn pacman_has_updates() -> bool {
    let raw = host_command("pacman")
        .args(["-Qu"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    !raw.trim().is_empty()
}

fn snap_has_updates() -> bool {
    let raw = host_command("snap")
        .args(["refresh", "--list"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let count = raw.lines().filter(|l| !l.starts_with("Name")).count();
    count > 0
}

fn zypper_has_updates() -> bool {
    let raw = host_command("zypper")
        .args(["lu", "--no-refresh"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    raw.lines().any(|l| l.contains('|') && !l.starts_with('S') && !l.contains("---"))
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
            title: "Kill".into(),
            subtitle: Some("kill <app-name>  —  stop a running process".into()),
            icon: Some("process-stop-symbolic".into()),
            action: Action::EnterMode("cmd".into()),
            score: 1000,
        },
        SearchResult {
            kind: ResultKind::System,
            title: "Install".into(),
            subtitle: Some("install <app>  —  install from Flatpak, distro, or Snap".into()),
            icon: Some("package-x-generic-symbolic".into()),
            action: Action::EnterMode("cmd".into()),
            score: 999,
        },
        SearchResult {
            kind: ResultKind::System,
            title: "Uninstall".into(),
            subtitle: Some("uninstall <app>  —  remove an installed app".into()),
            icon: Some("edit-delete-symbolic".into()),
            action: Action::EnterMode("cmd".into()),
            score: 998,
        },
        SearchResult {
            kind: ResultKind::System,
            title: "Update".into(),
            subtitle: Some("update  —  update installed packages".into()),
            icon: Some("system-software-update".into()),
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
        subtitle: Some("Please wait…".into()),
        icon: Some("emblem-synchronizing-symbolic".into()),
        action: Action::EnterMode("cmd".into()),
        score: 1000,
    }]
}

// ── Main search ───────────────────────────────────────────────────────────────

pub fn search(query: &str, pm: PackageManager, apps: &[AppEntry]) -> Vec<SearchResult> {
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
                "system-software-update",
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
            return searching_placeholder("Scanning running processes…");
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
                title: format!("No running app matching \"{}\"", rest),
                subtitle: Some("Only running apps can be killed".into()),
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
        let mut results = search_updates(&rest);
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
        return searching_placeholder(&format!("Searching for \"{}\"…", query));
    }

    if flatpak_results.is_empty() && distro_results.is_empty() && snap_results.is_empty() {
        return vec![SearchResult {
            kind: ResultKind::System,
            title: format!("No apps found for \"{}\"", query),
            subtitle: Some("Check spelling or try a different name".into()),
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
        return searching_placeholder("Scanning installed apps…");
    }

    if results.is_empty() {
        return vec![SearchResult {
            kind: ResultKind::System,
            title: format!("No installed app matching \"{}\"", query),
            subtitle: Some("Check spelling or try fewer characters".into()),
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

fn search_updates(query: &str) -> Vec<SearchResult> {
    ensure_updates_checked();
    // Clone updates out of the cache so the lock is released before
    // update_result/update_cmd_args try to access it (std::sync::Mutex
    // is not reentrant — holding the lock across those calls deadlocks).
    let updates: Option<Vec<UpdateInfo>> = update_cache()
        .try_lock()
        .ok()
        .and_then(|g| g.clone())
        .map(|(_, v)| v);
    match updates {
        Some(updates) if !updates.is_empty() => {
            if query.is_empty() {
                return updates.iter().take(10).map(update_result).collect();
            }
            let ql = query.trim().to_lowercase();
            let mut out: Vec<SearchResult> = updates
                .iter()
                .filter(|u| u.name.to_lowercase().contains(&ql))
                .map(update_result)
                .collect();
            out.truncate(10);
            out
        }
        _ => {
            if *update_fetching().lock().unwrap() {
                searching_placeholder("Checking for updates…")
            } else {
                vec![SearchResult {
                    kind: ResultKind::System,
                    title: "No updates available".into(),
                    subtitle: Some("All packages are up to date".into()),
                    icon: Some("emblem-ok-symbolic".into()),
                    action: Action::EnterMode("cmd".into()),
                    score: 0,
                }]
            }
        }
    }
}

fn update_result(u: &UpdateInfo) -> SearchResult {
    let (sub, icon) = match u.source.as_str() {
        "flatpak" if u.app_id.is_some() => (
            format!("flatpak: {} — update available", u.app_id.as_ref().unwrap()),
            u.app_id.clone().unwrap(),
        ),
        "flatpak" => (
            "flatpak packages — update available".into(),
            "system-software-update".into(),
        ),
        "dnf" => (
            "system packages (dnf) — update available".into(),
            "system-software-update".into(),
        ),
        "apt" => (
            "system packages (apt) — update available".into(),
            "system-software-update".into(),
        ),
        "pacman" => (
            "system packages (pacman) — update available".into(),
            "system-software-update".into(),
        ),
        "zypper" => (
            "system packages (zypper) — update available".into(),
            "system-software-update".into(),
        ),
        "snap" => (
            "snap packages — update available".into(),
            "system-software-update".into(),
        ),
        "all" => {
            // Source list is baked into the name at fetch-time,
            // so don't touch update_cache() here — caller holds the lock.
            let srcs = u.name
                .strip_prefix("all packages (")
                .and_then(|s| s.strip_suffix(')'))
                .unwrap_or("");
            (srcs.to_string(), "system-software-update".into())
        }
        _ => ("update available".into(), "system-software-update".into()),
    };
    let title = format!("Update: {}", u.name);
    let args = update_cmd_args(&u.source, u.app_id.as_deref());
    SearchResult {
        kind: ResultKind::System,
        title: title.clone(),
        subtitle: Some(sub),
        icon: Some(icon.clone()),
        action: Action::StartOperation {
            title,
            source: format!("{} Update", u.source),
            icon,
            args,
        },
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
        title: format!("Kill: {}", name),
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
        format!("via Flatpak ({})", app.app_id)
    } else {
        format!("{} — via Flatpak ({})", app.description, app.app_id)
    };
    let args = {
        let mut a = flatpak_cmd_args(&["install", "--user", "--assumeyes"]);
        a.push(app.app_id.clone());
        a
    };
    SearchResult {
        kind: ResultKind::System,
        title: format!("Install: {}", app.name),
        subtitle: Some(sub),
        // The app-id doubles as an icon name so the row shows the real app icon
        // when available (result_row falls back to a package icon otherwise).
        icon: Some(app.app_id.clone()),
        action: Action::StartOperation {
            title: format!("Installing {}", app.name),
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
        title: format!("Uninstall: {}", app.name),
        subtitle: Some(format!("via Flatpak ({})", app.app_id)),
        icon: Some(app.app_id.clone()),
        action: Action::StartOperation {
            title: format!("Uninstalling {}", app.name),
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
                title: format!("Install: {}", pkg.name),
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
        title: format!("Install: {}", pkg.name),
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
        title: format!("Install: {}", pkg.name),
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
        subtitle: Some("via snap".into()),
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
