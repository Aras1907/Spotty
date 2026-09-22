//! File & app indexer.
//!
//! Runs on a background thread and owns the only cross-thread state in the
//! app: [`Snapshot`] (`Arc<RwLock<…>>`) of apps + files, shared with the
//! search UI. The main thread never walks the filesystem — it only clones the
//! snapshot lock. Startup (`start_background_indexing`) loads the persisted
//! file cache on a thread so the GTK main thread stays free; first "find"
//! search triggers `ensure_files_indexed` synchronously if the background
//! load hasn't finished yet.
use crate::config::Config;
use ignore::WalkBuilder;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

const APP_WATCH_INTERVAL: Duration = Duration::from_secs(5);
#[derive(Debug, Clone)]
pub struct AppEntry {
    pub name: String,
    /// Pre-computed lowercase name for fast case-insensitive matching.
    pub name_lower: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    pub keywords: Vec<String>,
    pub icon: Option<String>,
    pub desktop_file: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub name_lower: String,
    pub is_dir: bool,
}
#[derive(Debug, Default, Clone)]
pub struct Snapshot {
    pub apps: Vec<AppEntry>,
    pub files: Vec<FileEntry>,
}
pub struct Indexer {
    config: Rc<RefCell<Config>>,
    snapshot: Arc<RwLock<Snapshot>>,
    files_indexing: Arc<AtomicBool>,
}
impl Indexer {
    pub fn new(config: Rc<RefCell<Config>>) -> Self {
        Self {
            config,
            snapshot: Arc::new(RwLock::new(Snapshot::default())),
            files_indexing: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn snapshot(&self) -> Arc<RwLock<Snapshot>> {
        self.snapshot.clone()
    }
    pub fn files_indexing(&self) -> Arc<AtomicBool> {
        self.files_indexing.clone()
    }
    pub fn start_background_indexing(&self) {
        // ponytail: the cached-index load (~50-100ms of JSON parsing) must
        // NOT run on the GTK main thread at startup, or the compositor frame
        // clock stalls and the user's video freezes on first launch. Spawn it
        // on a thread; a first "find" search calls ensure_files_indexed
        // anyway if it isn't done yet.
        let snap = self.snapshot.clone();
        let in_progress = self.files_indexing.clone();
        let max = self.config.borrow().max_index_entries;
        thread::spawn(move || ensure_files_indexed(&snap, &in_progress, max));
        let snap = self.snapshot.clone();
        thread::spawn(move || {
            let t = Instant::now();
            let apps = enum_apps();
            log::info!("indexed {} apps in {:?}", apps.len(), t.elapsed());
            snap.write().unwrap().apps = apps;
            // Apps stay tiny, so refresh them periodically.
            loop {
                thread::sleep(Duration::from_secs(300));
                snap.write().unwrap().apps = enum_apps();
            }
        });

        // ponytail: background OCR scan. Phase 1 — quick priority dirs (mostly
        // cache hits). Phase 2 — full home scan every 5 min to catch everything.
        // Delayed 10s so file indexing finishes first.
        let home = dirs::home_dir();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(10));
            let home = match home {
                Some(h) => h,
                None => return,
            };
            // Phase 1: priority dirs only — fast after first run.
            for sub in &["Downloads", "Pictures", "Documents", "Desktop"] {
                let dir = home.join(sub);
                if dir.exists() {
                    let (n, u, s) = crate::ocr::scan(&dir);
                    log::info!("ocr-scan: {} {} new, {} updated, {} skipped", dir.file_name().unwrap_or_default().to_string_lossy(), n, u, s);
                }
            }
            // Phase 2: everything in home, repeat every 5 min.
            loop {
                let (n, u, s) = crate::ocr::scan(&home);
                log::info!("ocr-scan: home scan — {} new, {} updated, {} skipped", n, u, s);
                thread::sleep(Duration::from_secs(300));
            }
        });

        self.start_change_watcher();
    }

    /// Kick off the lazy file index on a background thread if it hasn't been
    /// built yet (and isn't already being built). Returns immediately —
    /// searches against `snap.files` simply return nothing until the index
    /// is populated, rather than blocking the UI thread on a full filesystem walk.
    pub fn ensure_files_indexed(&self) {
        ensure_files_indexed(&self.snapshot, &self.files_indexing, self.config.borrow().max_index_entries);
    }

    /// A lightweight watcher that checks the priority folders and app dirs
    /// periodically. It does NOT walk the whole tree; it only reads the TOP LEVEL
    /// of those folders and compares a cheap signature (entry names + mtimes) so
    /// newly installed apps and downloaded files are picked up quickly.
    fn start_change_watcher(&self) {
        let snap = self.snapshot.clone();
        let max = self.config.borrow().max_index_entries;
        thread::spawn(move || {
            let app_dirs = watched_app_dirs();
            let file_dirs = priority_dirs();
            let mut last_apps_sig = quick_signature(&app_dirs);
            let mut last_files_sig = quick_signature(&file_dirs);
            loop {
                thread::sleep(APP_WATCH_INTERVAL);

                // Watch newly-installed apps (.desktop dirs).
                let apps_sig = quick_signature(&app_dirs);
                if apps_sig != last_apps_sig {
                    last_apps_sig = apps_sig;
                    let apps = enum_apps();
                    log::debug!("change watcher: apps changed -> re-indexed {}", apps.len());
                    snap.write().unwrap().apps = apps;
                    gtk::glib::MainContext::default().invoke(crate::app::refresh_search_window);
                }

                // Watch priority file folders (Downloads, Documents, etc.).
                let files_sig = quick_signature(&file_dirs);
                if files_sig != last_files_sig {
                    last_files_sig = files_sig;
                    let t = Instant::now();
                    let files = enum_files(max);
                    log::info!("change watcher: indexed {} files in {:?}", files.len(), t.elapsed());
                    save_file_index(&files);
                    snap.write().unwrap().files = files;
                    crate::search::files::clear_content_caches();
                    gtk::glib::MainContext::default().invoke(crate::app::refresh_search_window);
                }
            }
        });
    }
}

/// Kick off the lazy file index if it hasn't been built yet (and isn't
/// already being built). Loads the persisted cache synchronously (fast, but
/// ~50-100ms of JSON parsing — keep it off the main thread at startup; a
/// first "find" search will call this from the search path if needed), then
/// spawns a background refresh to catch changes. Returns immediately.
///
/// Free function, not a method: it runs on spawned threads, so it must only
/// touch `Send` state (`Arc<RwLock<Snapshot>>`, `Arc<AtomicBool>`), never
/// the `Rc`-based `Indexer`.
fn ensure_files_indexed(
    snap: &Arc<RwLock<Snapshot>>,
    files_indexing: &Arc<AtomicBool>,
    max: usize,
) {
    if !snap.read().unwrap().files.is_empty() {
        return;
    }
    // Try loading the persisted index first — much faster than a full walk.
    if let Some(files) = load_cached_file_index() {
        log::info!("loaded {} files from cached index", files.len());
        snap.write().unwrap().files = files;
        crate::search::files::clear_content_caches();
    }
    if !snap.read().unwrap().files.is_empty() {
        // We have a cached index; spawn a background refresh but don't block.
        let snap = snap.clone();
        thread::spawn(move || {
            let t = Instant::now();
            let files = enum_files(max);
            log::info!("refreshed {} files in {:?}", files.len(), t.elapsed());
            save_file_index(&files);
            snap.write().unwrap().files = files;
            crate::search::files::clear_content_caches();
            gtk::glib::MainContext::default().invoke(crate::app::refresh_search_window);
        });
        return;
    }
    if files_indexing.swap(true, Ordering::SeqCst) {
        return;
    }
    let snap = snap.clone();
    let in_progress = files_indexing.clone();
    thread::spawn(move || {
        let t = Instant::now();
        let files = enum_files(max);
        log::info!("indexed {} files in {:?}", files.len(), t.elapsed());
        save_file_index(&files);
        snap.write().unwrap().files = files;
        crate::search::files::clear_content_caches();
        in_progress.store(false, Ordering::SeqCst);
        gtk::glib::MainContext::default().invoke(crate::app::refresh_search_window);
    });
}

/// Application directories we poll for newly-installed apps.
fn watched_app_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = dirs::home_dir() {
        v.push(h.join(".local/share/applications"));
        v.push(h.join(".local/share/flatpak/exports/share/applications"));
    }
    if std::env::var("FLATPAK_ID").is_ok() {
        v.push(PathBuf::from("/app/share/applications"));
        return v;
    }
    v.push(PathBuf::from("/usr/share/applications"));
    v.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    v
}

/// Compute a cheap change signature over the TOP LEVEL of the given dirs. This
/// is intentionally shallow (one read_dir per folder, no recursion) so it costs
/// almost nothing to run every second. The signature changes when an entry is
/// added, removed, or its mtime changes.
fn quick_signature(dirs: &[PathBuf]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325; // FNV-1a
    fn mix(h: &mut u64, bytes: &[u8]) {
        for b in bytes {
            *h ^= *b as u64;
            *h = h.wrapping_mul(0x100000001b3);
        }
    }
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in rd.flatten() {
            mix(&mut h, entry.file_name().to_string_lossy().as_bytes());
            if let Ok(meta) = entry.metadata() {
                if let Ok(mt) = meta.modified() {
                    if let Ok(d) = mt.duration_since(std::time::UNIX_EPOCH) {
                        mix(&mut h, &d.as_secs().to_le_bytes());
                    }
                }
            }
        }
    }
    h
}

pub(crate) fn enum_apps() -> Vec<AppEntry> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(h) = dirs::home_dir() {
        dirs.push(h.join(".local/share/applications"));
    }
    if std::env::var("FLATPAK_ID").is_ok() {
        // Inside Flatpak, the sandbox's /usr/share/applications is the runtime,
        // not the host desktop. Import the current app from /app, then merge the
        // real host desktop entries through flatpak-spawn.
        dirs.push("/app/share/applications".into());
        for dir in &dirs {
            append_apps_from_dir(dir, &mut seen, &mut out);
        }
        let mut host_dirs = vec![
            "/usr/share/applications".to_string(),
            "/usr/local/share/applications".to_string(),
            "/var/lib/flatpak/exports/share/applications".to_string(),
            "/var/lib/snapd/desktop/applications".to_string(),
        ];
        if let Some(h) = dirs::home_dir() {
            host_dirs.push(
                h.join(".local/share/applications")
                    .to_string_lossy()
                    .to_string(),
            );
            host_dirs.push(
                h.join(".local/share/flatpak/exports/share/applications")
                    .to_string_lossy()
                    .to_string(),
            );
        }
        append_host_apps(&host_dirs, &mut seen, &mut out);
    } else {
        dirs.extend(
            ["/usr/share/applications", "/usr/local/share/applications"].map(PathBuf::from),
        );
        dirs.push("/var/lib/flatpak/exports/share/applications".into());
        if let Some(h) = dirs::home_dir() {
            dirs.push(h.join(".local/share/flatpak/exports/share/applications"));
        }
        dirs.push("/var/lib/snapd/desktop/applications".into());
        for dir in &dirs {
            append_apps_from_dir(dir, &mut seen, &mut out);
        }
    }
    out
}

fn append_apps_from_dir(
    dir: &PathBuf,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<AppEntry>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("desktop") {
            continue;
        }
        let Some(id) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        let raw = std::fs::read_to_string(&p).ok();
        if let Some(app) = parse_desktop_entry_text(&p, raw.as_deref()) {
            out.push(app);
        }
    }
}

fn append_host_apps(
    dirs: &[String],
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<AppEntry>,
) {
    for (path, text) in host_desktop_entries(dirs) {
        let Some(id) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        if let Some(app) = parse_desktop_entry_text(&path, Some(&text)) {
            out.push(app);
        }
    }
}

fn host_desktop_entries(dirs: &[String]) -> Vec<(PathBuf, String)> {
    let mut args = vec!["--host".to_string(), "sh".to_string(), "-lc".to_string()];
    let find_script = dirs
        .iter()
        .map(|d| {
            format!(
                "[ -d '{0}' ] && find -L '{0}' -maxdepth 1 \\( -type f -o -type l \\) -name '*.desktop' -print",
                d.replace('\'', "'\\''")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let mut script = format!("{{ {find_script}; ");
    script.push_str(
        "[ -d \"$HOME/.local/share/flatpak/exports/share/applications\" ] && find -L \"$HOME/.local/share/flatpak/exports/share/applications\" -maxdepth 1 \\( -type f -o -type l \\) -name '*.desktop' -print; ",
    );
    script.push_str(
        "[ -d \"$HOME/.local/share/flatpak/app\" ] && find \"$HOME/.local/share/flatpak/app\" -path '*/export/share/applications/*.desktop' -type f -print; ",
    );
    script.push_str(
        "} | sort -u | while IFS= read -r f; do \
           [ -f \"$f\" ] || continue; \
           printf '\\036%s\\037\\n' \"$f\"; \
           cat \"$f\" 2>/dev/null; \
           printf '\\n'; \
         done",
    );
    args.push(script);
    let Ok(out) = std::process::Command::new("flatpak-spawn")
        .args(&args)
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .split('\x1e')
        .filter_map(|chunk| {
            let (path, text) = chunk.split_once('\x1f')?;
            let path = path.trim();
            if path.is_empty() {
                return None;
            }
            Some((
                PathBuf::from(path),
                text.trim_start_matches('\n').to_string(),
            ))
        })
        .collect()
}

fn parse_desktop_entry_text(path: &std::path::Path, text: Option<&str>) -> Option<AppEntry> {
    let text = text?;
    let locale_tags = desktop_locale_tags();
    let mut in_desktop_entry = false;
    let mut name = None::<(String, u8)>;
    let mut generic_name = None::<(String, u8)>;
    let mut comment = None::<(String, u8)>;
    let mut icon = None::<String>;
    let mut keywords = None::<(Vec<String>, u8)>;
    let mut exec = None::<String>;
    let mut flatpak_id = None::<String>;
    let mut no_display = false;
    let mut hidden = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            // Only the [Desktop Entry] section is read; [Desktop Action …] and
            // any other groups are skipped.
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = desktop_unescape(value.trim());
        if let Some(priority) = localized_key_priority(key, "Name", &locale_tags) {
            set_localized(&mut name, value, priority);
        } else if let Some(priority) = localized_key_priority(key, "GenericName", &locale_tags) {
            set_localized(&mut generic_name, value, priority);
        } else if let Some(priority) = localized_key_priority(key, "Comment", &locale_tags) {
            set_localized(&mut comment, value, priority);
        } else if let Some(priority) = localized_key_priority(key, "Keywords", &locale_tags) {
            let values = value
                .split(';')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect();
            set_localized(&mut keywords, values, priority);
        } else {
            match key {
                "Icon" => icon = Some(value),
                "Exec" => exec = Some(value),
                "X-Flatpak" => flatpak_id = Some(value),
                "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
                "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
                _ => {}
            }
        }
    }
    if no_display || hidden {
        return None;
    }
    let mut keywords = keywords.map(|(values, _)| values).unwrap_or_default();
    if let Some(id) = flatpak_id {
        if !keywords.iter().any(|k| k.eq_ignore_ascii_case(&id)) {
            keywords.push(id);
        }
    }
    if let Some(exec) = exec {
        for token in exec
            .split(|c: char| c.is_whitespace() || matches!(c, '%' | '=' | '/'))
            .filter(|s| !s.is_empty())
        {
            let token = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-');
            if token.len() >= 3 && !keywords.iter().any(|k| k.eq_ignore_ascii_case(token)) {
                keywords.push(token.to_string());
            }
        }
    }
    let fallback_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.replace(['.', '-', '_'], " "))
        .filter(|s| !s.trim().is_empty());
    let name = name.map(|(value, _)| value).or(fallback_name)?;
    Some(AppEntry {
        name_lower: name.to_lowercase(),
        name,
        generic_name: generic_name.map(|(value, _)| value),
        comment: comment.map(|(value, _)| value),
        keywords,
        icon,
        desktop_file: path.to_path_buf(),
    })
}

fn set_localized<T>(slot: &mut Option<(T, u8)>, value: T, priority: u8) {
    if slot.as_ref().map(|(_, p)| priority >= *p).unwrap_or(true) {
        *slot = Some((value, priority));
    }
}

fn localized_key_priority(key: &str, base: &str, locale_tags: &[String]) -> Option<u8> {
    if key == base {
        return Some(1);
    }
    let tag = key
        .strip_prefix(base)?
        .strip_prefix('[')?
        .strip_suffix(']')?
        .replace('-', "_");
    locale_tags
        .iter()
        .position(|candidate| candidate == &tag)
        .map(|idx| 3u8.saturating_sub(idx as u8))
}

fn desktop_locale_tags() -> Vec<String> {
    let raw = std::env::var("LC_MESSAGES")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_else(|| "C".to_string());
    let locale = raw
        .split('.')
        .next()
        .unwrap_or(&raw)
        .split('@')
        .next()
        .unwrap_or(&raw)
        .replace('-', "_");
    let mut tags = Vec::new();
    if locale != "C" && !locale.is_empty() {
        tags.push(locale.clone());
        if let Some((lang, _)) = locale.split_once('_') {
            tags.push(lang.to_string());
        }
    }
    tags
}

fn desktop_unescape(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('s') => out.push(' '),
                Some('\\') => out.push('\\'),
                Some(';') => out.push(';'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(ch);
        }
    }
    out
}
/// Build a `WalkBuilder` pre-configured with Spotty's standard settings
/// (ignore `.gitignore`, hidden files allowed, no follow links, etc.).
fn walk_builder(root: &std::path::Path, max_depth: Option<usize>) -> WalkBuilder {
    let mut wb = WalkBuilder::new(root);
    wb.hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .require_git(false)
        .follow_links(false)
        .max_depth(max_depth.map(|d| d as usize));
    wb
}

/// Walk a directory tree and add entries to `out` (deduplicating via `seen`),
/// up to `max` total entries. Returns the number of entries added.
fn walk_into(
    root: &std::path::Path,
    max_depth: Option<usize>,
    filter: fn(&str) -> bool,
    out: &mut Vec<FileEntry>,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
    max: usize,
) {
    if !root.exists() {
        return;
    }
    for e in walk_builder(root, max_depth)
        .filter_entry(move |e| !filter(&e.file_name().to_string_lossy()))
        .build()
        .flatten()
    {
        let p = e.path().to_path_buf();
        let Some(name) = e.file_name().to_str().map(String::from) else {
            continue;
        };
        if seen.insert(p.clone()) {
            out.push(FileEntry {
                path: p,
                name: name.clone(),
                name_lower: name.to_ascii_lowercase(),
                is_dir: e.file_type().map(|t| t.is_dir()).unwrap_or(false),
            });
        }
        if out.len() >= max {
            return;
        }
    }
}

fn enum_files(max: usize) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let Some(home) = dirs::home_dir() else {
        return out;
    };

    // Pass 1: list EVERY top-level entry in home (depth 1), INCLUDING all
    // hidden dotfiles/dotdirs. std::fs::read_dir returns every entry; we do
    // NOT filter by name here so .local, .config, .bashrc all get indexed.
    let mut top_level_count = 0usize;
    if let Ok(rd) = std::fs::read_dir(&home) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name: String = match p.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if seen.insert(p.clone()) {
                out.push(FileEntry {
                    path: p,
                    name: name.clone(),
                    name_lower: name.to_ascii_lowercase(),
                    is_dir,
                });
                top_level_count += 1;
            }
            if out.len() >= max {
                return out;
            }
        }
    }
    log::debug!(
        "indexed {} top-level home entries (hidden included)",
        top_level_count
    );

    // Pass 1.5: shallowly index well-known app profile/config locations that the
    // general walk prunes (.mozilla, .var, …). This is what lets "find firefox
    // profile" locate ~/.mozilla/firefox/<profile>. Depth 2 captures the profile
    // folders and their immediate files (prefs.js, places.sqlite) without
    // descending into the huge cache/storage subtrees.
    let profile_roots = [
        home.join(".mozilla/firefox"),
        home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
        home.join(".thunderbird"),
        home.join(".config"),
    ];
    for root in &profile_roots {
        walk_into(root, Some(2), is_heavy_profile_dir, &mut out, &mut seen, max);
        if out.len() >= max {
            return out;
        }
    }

    // Pass 2: walk PRIORITY directories first (Pictures, Documents, etc.) so the
    // most-likely-relevant files get indexed before the budget runs out.
    let priority_dirs = [
        "Pictures",
        "Documents",
        "Downloads",
        "Desktop",
        "Music",
        "Videos",
    ];
    for sub in &priority_dirs {
        let root = home.join(sub);
        walk_into(&root, Some(10), is_prune_dir, &mut out, &mut seen, max);
        if out.len() >= max {
            return out;
        }
    }

    // Pass 3: walk the rest of home for everything else (deeper, to reach files
    // nested in project trees like ~/dev/<project>/docs/...).
    walk_into(&home, Some(6), is_prune_dir, &mut out, &mut seen, max);

    log::debug!("file index: {} total entries", out.len());
    out
}

/// Heavy subdirectories inside app profile/config trees that we skip while
/// shallowly indexing those trees (caches, on-disk storage, telemetry, …).
/// Does NOT include the profile roots themselves (e.g. "firefox", ".config").
fn is_heavy_profile_dir(name: &str) -> bool {
    matches!(
        name,
        "cache2"
            | "Cache"
            | "cache"
            | "startupCache"
            | "OfflineCache"
            | "storage"
            | "datareporting"
            | "minidumps"
            | "saved-telemetry-pings"
            | "thumbnails"
            | "crashes"
            | "shader-cache"
            | "GPUCache"
            | "Code Cache"
            | "Service Worker"
    )
}

/// Names of directories we never recurse into when indexing.
fn is_prune_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | "target"
            | ".git"
            | ".cache"
            | "__pycache__"
            | ".venv"
            | "venv"
            | ".npm"
            | ".cargo"
            | ".rustup"
            | ".local"
            | ".var"
            | ".mozilla"
            | ".thumbnails"
            | "snap"
            | ".gradle"
            | ".m2"
            | ".steam"
            | "Trash"
    )
}

fn index_cache_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("spotty")
}

fn cached_index_path() -> PathBuf {
    index_cache_dir().join("files_index.json")
}

fn cached_sig_path() -> PathBuf {
    index_cache_dir().join("files_index.sig")
}

fn priority_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(h) = dirs::home_dir() {
        dirs.push(h.join("Downloads"));
        dirs.push(h.join("Documents"));
        dirs.push(h.join("Desktop"));
        dirs.push(h.join("Pictures"));
        dirs.push(h.join("Music"));
        dirs.push(h.join("Videos"));
    }
    dirs
}

fn save_file_index(files: &[FileEntry]) {
    let dir = index_cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string(files) {
        let _ = std::fs::write(cached_index_path(), &json);
        let sig = quick_signature(&priority_dirs());
        let _ = std::fs::write(cached_sig_path(), &sig.to_le_bytes());
    }
}

fn load_cached_file_index() -> Option<Vec<FileEntry>> {
    let path = cached_index_path();
    let sig_path = cached_sig_path();
    // Check signature first — if the priority dirs changed, the cached index is stale.
    let current_sig = quick_signature(&priority_dirs());
    if let Ok(sig_bytes) = std::fs::read(&sig_path) {
        if sig_bytes.len() == 8 {
            let saved_sig = u64::from_le_bytes(sig_bytes.try_into().unwrap());
            if saved_sig == current_sig {
                // Signature matches, try loading the index.
                if let Ok(json) = std::fs::read_to_string(&path) {
                    if let Ok(files) = serde_json::from_str::<Vec<FileEntry>>(&json) {
                        return Some(files);
                    }
                }
            }
        }
    }
    None
}
