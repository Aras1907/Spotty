use crate::clipboard::ClipboardHistory;
use crate::config::Config;
use crate::index::Indexer;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, RwLock};

pub mod apps;
pub mod bluetooth;
pub mod browse;
pub mod browser_engine;
pub mod calculator;
pub mod clipboard;
pub mod cmd;
pub mod dictionary;
pub mod emoji;
pub mod files;
pub mod jobs;
pub mod run;
pub mod settings_panels;
pub mod system;
pub mod typo;
pub mod uninstall;
pub mod web;

pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ResultKind {
    App,
    File,
    Folder,
    Web,
    Clipboard,
    Calculator,
    System,
    /// An emoji result: `icon` carries the literal emoji glyph, rendered as
    /// large text instead of an icon.
    Emoji,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchResult {
    pub kind: ResultKind,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: Option<String>,
    pub action: Action,
    pub score: i32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Action {
    LaunchDesktopFile(std::path::PathBuf),
    OpenPath(std::path::PathBuf),
    OpenInFileManager(std::path::PathBuf),
    BrowseInto(std::path::PathBuf),
    OpenUrl(String),
    CopyToClipboard(String),
    CopyImageToClipboard(std::path::PathBuf),
    /// Re-copy a file/folder onto the clipboard (from the clipboard manager).
    CopyFileToClipboard(std::path::PathBuf),
    InsertCalculatorResult(String),
    RunCommand(String),
    /// Run a command with a confirmation dialog first (for destructive actions
    /// like shutdown/reboot/logout/suspend).
    ConfirmRunCommand(String),
    /// Run a command inside a terminal emulator window (for interactive use).
    RunInTerminal(String),
    /// Run a command and show streaming progress inline in the search window.
    /// `title` is the header label; `args[0]` is the program, rest are argv.
    RunWithProgress {
        title: String,
        args: Vec<String>,
    },
    /// Enter a trigger mode (carries the trigger word, e.g. "files", "pdf").
    EnterMode(String),
    /// Open the triggers window (installed triggers + file import).
    ShowTriggersWindow,
    /// Remove an installed trigger by id.
    UninstallTrigger(String),
    /// Start a long-running package operation in the background. It keeps
    /// running even if the search window is hidden. `args[0]` is the program.
    StartOperation {
        title: String,
        source: String,
        /// Icon name / app-id shown on the operation's progress row.
        icon: String,
        args: Vec<String>,
    },
    /// Bluetooth control (bt trigger). `op` ∈ connect/disconnect/pair/scan/
    /// power_on/power_off; `mac` is empty for the scan/power/check actions.
    Bluetooth {
        op: String,
        mac: String,
    },
}

/// Universally pinned results (any kind) whose title/subtitle matches `query_lower`.
/// Matches always score above normal results so they sort to the top.
pub fn pinned_matches(query_lower: &str, pinned: &[SearchResult]) -> Vec<SearchResult> {
    if pinned.is_empty() || query_lower.is_empty() {
        return vec![];
    }
    pinned
        .iter()
        .filter(|p| {
            p.title.to_lowercase().contains(query_lower)
                || p
                    .subtitle
                    .as_deref()
                    .map(|s| s.to_lowercase().contains(query_lower))
                    .unwrap_or(false)
        })
        .cloned()
        .map(|mut p| {
            p.score = 1_000_000;
            p
        })
        .collect()
}

/// Prepend matching pinned results to `results`, removing any duplicates
/// (same action) that already appear naturally.
pub fn merge_pinned(
    mut results: Vec<SearchResult>,
    query_lower: &str,
    pinned: &[SearchResult],
) -> Vec<SearchResult> {
    let pins = pinned_matches(query_lower, pinned);
    if pins.is_empty() {
        return results;
    }
    results.retain(|r| !pins.iter().any(|p| p.action == r.action));
    let mut out = pins;
    out.extend(results);
    out
}

fn inline_file_mode(query: &str, config: &Config) -> Option<(String, String)> {
    let mut parts = query.splitn(2, char::is_whitespace);
    let word = parts.next()?.trim();
    let rest = parts.next()?.trim();
    if word.is_empty() {
        return None;
    }
    let kw = config.keyword_for_word(word)?;
    kw.all_files.then(|| (kw.word.clone(), rest.to_string()))
}

pub fn search(
    query: &str,
    config: &Config,
    snap_lock: &Arc<RwLock<crate::index::Snapshot>>,
) -> Vec<SearchResult> {
    let query = query.trim();
    if query.is_empty() {
        return vec![];
    }
    if let Some((mode_word, rest)) = inline_file_mode(query, config) {
        return search_mode(&mode_word, &rest, config, snap_lock);
    }

    let snap_guard = snap_lock.read().unwrap();
    let snap = &*snap_guard;

    // Path browsing mode
    if config.enable_root_browsing
        && (query.starts_with('/') || query.starts_with("~/") || query == "~")
    {
        return browse::browse(query);
    }

    let mut r = Vec::with_capacity(32);

    // Trigger suggestions (Raycast-style): if the query is a PREFIX of one or
    // more trigger words, surface those triggers as top results. Pressing Enter
    // on one enters that mode. Only when the query is short and alphabetic.
    let trigger_kws = crate::triggers::keywords();
    let kw_iter = config
        .command_keywords
        .iter()
        .chain(trigger_kws.iter());
    let ql = query.to_lowercase();
    if ql.len() >= 1 && ql.len() <= 8 && ql.chars().all(|c| c.is_alphabetic()) {
        for kw in kw_iter {
            let dn = kw.display_name().to_lowercase();
            let word_match = kw.word.starts_with(&ql);
            let name_match = dn.starts_with(&ql);
            if word_match || name_match {
                // Exact match scores highest; display-name prefix a bit lower than word prefix.
                let score = if kw.word == ql || dn == ql {
                    100_000
                } else if word_match {
                    50_000
                } else {
                    45_000
                };
                r.push(SearchResult {
                    kind: ResultKind::System,
                    title: capitalize(&kw.word),
                    subtitle: Some(kw.description.clone()),
                    icon: Some(if kw.icon.is_empty() {
                        "folder-symbolic".into()
                    } else {
                        kw.icon.clone()
                    }),
                    action: Action::EnterMode(kw.word.clone()),
                    score,
                });
            }
        }
    }

    // Update action in universal search — type "update" or "upgrade" to see
    // and install available package updates. Not a keyword mode, just a direct
    // action result so it works from the main bar.
    if ql == "update" || ql == "upgrade" || ql.starts_with("upd") || ql.starts_with("upg") {
        let mut update_results = cmd::update_all_result();
        let score = if ql == "update" || ql == "upgrade" {
            100_000
        } else if ql.starts_with("upd") || ql.starts_with("upg") {
            50_000
        } else {
            30_000
        };
        for r in &mut update_results {
            r.score = r.score.max(score);
        }
        r.extend(update_results);
    }

    r.extend(system::search(query, config));
    r.extend(settings_panels::search(query));
    if config.enable_calculator {
        if let Some(calc) = calculator::evaluate(query) {
            r.push(calc);
        }
    }
    if config.enable_apps {
        r.extend(apps::search(query, &snap.apps));
    }
    // Installable apps (Flatpak / distro) surfaced without the "install" verb, so
    // the user can discover apps to install while searching for anything. Gated
    // behind a setting and a min length to avoid noise / catalog churn on very
    // short queries. Catalog-backed and cached, so this is cheap per keystroke.
    if config.enable_new_apps && query.chars().count() >= 3 {
        r.extend(cmd::universal_install(query, config.package_manager, 4));
    }
    if config.enable_web {
        r.push(web::result(query, config));
    }

    let mut r = merge_pinned(r, &ql, &config.pinned_results);
    r.extend(crate::operations::running_result_rows());
    r.sort_by(|a, b| b.score.cmp(&a.score));
    r.truncate(20);
    r
}

pub fn search_mode(
    mode_word: &str,
    query: &str,
    config: &Config,
    snap_lock: &Arc<RwLock<crate::index::Snapshot>>,
) -> Vec<SearchResult> {
    let kw = match config.keyword_for_word(mode_word) {
        Some(kw) => kw,
        None => return vec![],
    };

    if kw.all_files || !kw.extensions.is_empty() {
        // ensure_files_indexed() must be called by the caller (main thread)
        // before invoking this function.
    }

    let rest = query.trim();

    if kw.id == "clipboard" {
        // Clipboard mode not used in worker (it uses Rc<ClipboardHistory> on main).
        return vec![];
    }
    let rl = rest.to_lowercase();
    let pinned = &config.pinned_results;
    if kw.id == "emoji" {
        return emoji::search(rest);
    }
    if kw.id == "run" {
        return run::search(rest);
    }
    if kw.id == "cmd" {
        let snap_guard = snap_lock.read().unwrap();
        return merge_pinned(
            cmd::search(rest, config.package_manager, &snap_guard.apps),
            &rl,
            pinned,
        );
    }
    if kw.all_files {
        // The dedicated "Find" trigger always supports path browsing,
        // independent of the general "Root Path Browsing" toggle (which only
        // governs typing `/` directly into the universal search box).
        if rest.starts_with('/') || rest.starts_with("~/") || rest == "~" {
            return browse::browse(rest);
        }
        if rest.is_empty() {
            return if config.show_recent_file_searches {
                crate::recent_paths::results()
            } else {
                vec![]
            };
        }
        let files = files::search_find(rest, &snap_lock);
        return merge_pinned(files, &rl, pinned);
    }
    if !kw.extensions.is_empty() {
        let query = if rest.is_empty() {
            kw.word.clone()
        } else {
            format!("{} {}", kw.word, rest)
        };
        let mut files = {
            let snap_guard = snap_lock.read().unwrap();
            files::search(&query, &snap_guard.files, config)
        };
        files.truncate(20);
        return merge_pinned(files, &rl, pinned);
    }
    // Installed trigger actions. Files-type triggers were already handled by the
    // all_files/extensions branches above (they convert to ordinary keywords);
    // only web and shell actions reach this branch.
    if let Some(trigger) = crate::triggers::by_id(&kw.id) {
        // The dictionary trigger does live lookups (word autocomplete +
        // definition) instead of a plain web search.
        if kw.id == "dictionary" {
            return dictionary::results(rest);
        }
        if kw.id == "bluetooth" {
            return bluetooth::search(rest);
        }
        let icon = Some(if trigger.icon.is_empty() {
            "folder-symbolic".into()
        } else {
            trigger.icon.clone()
        });
        return match &trigger.action {
            crate::triggers::TriggerAction::Web { url } => {
                if rest.is_empty() {
                    return vec![SearchResult {
                        kind: ResultKind::System,
                        title: format!("Type something to search with {}", trigger.name),
                        subtitle: Some(trigger.description.clone()),
                        icon,
                        action: Action::EnterMode(kw.word.clone()),
                        score: 1000,
                    }];
                }
                let encoded = urlencoding::encode(rest);
                let url = url.replace("{query}", &encoded);
                vec![SearchResult {
                    kind: ResultKind::Web,
                    title: format!("{}: {}", trigger.name, rest),
                    subtitle: Some(url.clone()),
                    icon,
                    action: Action::OpenUrl(url),
                    score: 100_000,
                }]
            }
            crate::triggers::TriggerAction::Shell { command } => {
                if rest.is_empty() {
                    return vec![SearchResult {
                        kind: ResultKind::System,
                        title: format!("Type a command to run with {}", trigger.name),
                        subtitle: Some(trigger.description.clone()),
                        icon,
                        action: Action::EnterMode(kw.word.clone()),
                        score: 1000,
                    }];
                }
                // {query} is single-quote-escaped: user input can't inject
                // extra shell commands, only the template author's command runs.
                let rendered = command.replace("{query}", &crate::triggers::shell_escape(rest));
                vec![SearchResult {
                    kind: ResultKind::System,
                    title: format!("Run {}", trigger.name),
                    subtitle: Some(rendered.clone()),
                    icon,
                    action: Action::RunWithProgress {
                        title: format!("{}: {}", trigger.name, rest),
                        args: run::command_argv(&rendered),
                    },
                    score: 100_000,
                }]
            }
            crate::triggers::TriggerAction::Files { .. } => vec![],
        };
    }
    vec![]
}
