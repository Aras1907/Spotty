use crate::clipboard::ClipboardHistory;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32String};
use crate::config::Config;
use crate::index::Indexer;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use crate::i18n::gettext;

pub mod apps;
pub mod appimage;
pub mod bluetooth;
pub mod browse;
pub mod browser_engine;
pub mod calculator;
pub mod clipboard;
pub mod cmd;
pub mod convert;
pub mod currency;
pub mod dictionary;
pub mod emoji;
pub mod files;
pub mod jobs;
pub mod run;
pub mod settings_panels;
pub mod system;
pub mod translate;
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
    /// A translation row: Enter copies the text (never auto-pastes).
    Translate,
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
    /// Run an installed AppImage (a portable file, not a desktop entry —
    /// it may have no integration entry at all).
    LaunchAppImage(std::path::PathBuf),
    /// Trash an installed AppImage (confirmed first; see the search window).
    RemoveAppImage(std::path::PathBuf),
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
    /// Pick a target language for the translate trigger; `strip` is the
    /// trailing token of the query that named it ("hello po" → "po") and is
    /// removed from the entry when set.
    SetTranslateTarget {
        code: String,
        strip: String,
    },
    /// Expand a language list next to the typed text: the target picker
    /// (`source == false`) or the source picker (manual override of
    /// auto-detection).
    TranslateExpand {
        source: bool,
    },
    /// Pick the source language for this session; empty = auto-detect.
    SetTranslateSource {
        code: String,
    },
    /// Translate right now, skipping the 3 s auto-translate delay (also the
    /// retry for a failed attempt).
    TranslateNow {
        text: String,
        target: String,
    },
    /// Switch the background update feature on/off (from search or the
    /// Settings row).
    ToggleUpdates,
    /// "Remind tomorrow": snooze the update notice for 24 hours.
    SnoozeUpdates,
    /// "Dismiss update notice": hide this update set until a new one
    /// appears (carries its signature).
    DismissUpdates(String),
    /// Re-run the update check right now. Enter on "No updates available"
    /// (or on the "Checking for updates" row) checks again instead of
    /// doing nothing.
    CheckUpdates,
    /// A row that only exists to be displayed (e.g. "No updates
    /// available") — Enter does nothing.
    Noop,
}

/// Fuzzy match for typed keywords: nucleo scores `text` against `query`
/// above a length-scaled threshold — that catches dropped letters ("sytem"
/// → system, "fltak" → flatpak). nucleo only accepts subsequences, so a
/// second check covers what subsequence scoring rejects: swapped,
/// substituted or doubled letters ("udpat", "sistem", "aall") within two
/// edits of a short candidate. Unrelated words never hit: "upload" is four
/// edits away from "update". Queries shorter than two characters stay with
/// the explicit prefix logic.
pub fn fuzzy_match(query: &str, text: &str) -> bool {
    let len = query.chars().count();
    if len < 2 {
        return false;
    }
    let mut matcher = Matcher::default();
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    if pattern
        .score(Utf32String::from(text).slice(..), &mut matcher)
        .map(|s| s >= (len as u32) * 20)
        .unwrap_or(false)
    {
        return true;
    }
    len >= 3
        && edit_distance(&query.to_lowercase(), &text.to_lowercase()) <= 2
}

/// Classic Levenshtein distance over chars — the words compared here are
/// short keywords, candidates and verbs.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
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
            let title = p.title.to_lowercase();
            let sub = p.subtitle.as_deref().map(str::to_lowercase);
            // Substring first; fuzzy below it so a typo'd pinned query
            // ("setings") still surfaces its pinned row.
            title.contains(query_lower)
                || fuzzy_match(query_lower, &title)
                || sub
                    .as_deref()
                    .map(|s| s.contains(query_lower) || fuzzy_match(query_lower, s))
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
    let kw = inline_keyword(word, config)?;
    // All-files keywords inline-route ("find foo"); the dictionary and the
    // translate trigger route the same way so `dict word` / `translate text`
    // work in one go, without a separate mode-entry step first.
    (kw.all_files || matches!(kw.id.as_str(), "dictionary" | "translate"))
        .then(|| (kw.word.clone(), rest.to_string()))
}

/// The inline trigger behind a typed word: exact first, then a bounded
/// fuzzy pass (3–12 chars) so a typo'd inline trigger still routes —
/// "fnd report" searches files — while short or long words, which is where
/// unrelated queries live, are never guessed.
fn inline_keyword(word: &str, config: &Config) -> Option<crate::config::CommandKeyword> {
    if let Some(kw) = config.keyword_for_word(word) {
        return Some(kw);
    }
    let wl = word.to_lowercase();
    if !(3..=12).contains(&wl.chars().count()) {
        return None;
    }
    let inline_capable = |kw: &crate::config::CommandKeyword| {
        kw.all_files || matches!(kw.id.as_str(), "dictionary" | "translate")
    };
    let owned = crate::triggers::keywords();
    config
        .command_keywords
        .iter()
        .chain(owned.iter())
        .filter(|kw| kw.enabled && inline_capable(kw))
        .find(|kw| fuzzy_match(&wl, &kw.word))
        .cloned()
}

/// Trigger-word suggestions for the universal search (Raycast-style):
/// prefix matches at any length — so the fully-typed word `bluetooth` or
/// `translate` still matches — plus a fuzzy tier for typos ("fils" → Find)
/// that only runs for short queries, so long searches never fuzzy-match a
/// trigger. Pressing Enter on a suggestion enters that mode. Shared with the
/// worker (`jobs::compute`), which is the path that runs in production.
pub fn trigger_suggestions(query: &str, config: &Config) -> Vec<SearchResult> {
    let ql = query.trim().to_lowercase();
    if ql.is_empty() || !ql.chars().all(|c| c.is_alphabetic()) {
        return Vec::new();
    }
    let trigger_kws = crate::triggers::keywords();
    let kw_iter = config
        .command_keywords
        .iter()
        .chain(trigger_kws.iter());
    let mut r = Vec::new();
    for kw in kw_iter {
        let dn = kw.display_name().to_lowercase();
        let word_match = kw.word.starts_with(&ql);
        let name_match = dn.starts_with(&ql);
        // Fuzzy last: a typo'd trigger word ("fils" → find) still suggests
        // its mode, ranked below every real prefix hit. Bounded to short
        // queries so a long search never fuzzy-hits a trigger by accident.
        let fuzzy = !word_match
            && !name_match
            && ql.chars().count() <= 12
            && (fuzzy_match(&ql, &kw.word) || fuzzy_match(&ql, &dn));
        if !(word_match || name_match || fuzzy) {
            continue;
        }
        // Exact match scores highest; display-name prefix a bit lower than
        // word prefix; fuzzy below both.
        let score = if kw.word == ql || dn == ql {
            100_000
        } else if word_match {
            50_000
        } else if name_match {
            45_000
        } else {
            40_000
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
    r
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

    let ql = query.to_lowercase();
    // Trigger suggestions (Raycast-style): shared with the worker — that is
    // the path that runs in production.
    r.extend(trigger_suggestions(query, config));

    // Updates are a General-section feature now (no trigger): the
    // update/updates/upd/upgrade/upg verbs show the inline update list.
    if let Some(update_rows) = cmd::update_verb_rows(query, config) {
        r.extend(update_rows);
    }

    r.extend(system::search(query, config));
    r.extend(settings_panels::search(query));
    if config.enable_calculator {
        if let Some(calc) = calculator::evaluate(query, config) {
            r.push(calc);
        }
        if let Some(conv) = convert::convert(query, config) {
            r.push(conv);
        }
    }
    if config.enable_apps {
        r.extend(apps::search(query, &snap.apps));
        // Portable AppImages: launch rows for files that have no desktop
        // entry of their own (integrated ones are already indexed apps).
        if config.app_sources().appimage {
            r.extend(appimage::search(query, 3));
        }
    }
    // Installable apps (Flatpak / distro) surfaced without the "install" verb, so
    // the user can discover apps to install while searching for anything. Gated
    // behind a setting and a min length to avoid noise / catalog churn on very
    // short queries. Catalog-backed and cached, so this is cheap per keystroke.
    if config.enable_new_apps && query.chars().count() >= 3 {
        r.extend(cmd::universal_install(query, config.app_sources(), 4));
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
            cmd::search(rest, config, &snap_guard.apps),
            &rl,
            pinned,
        );
    }
    if kw.id == "translate" {
        // The translate trigger: live, local translation — target follows
        // the system language unless a language was picked.
        return translate::results(rest, config);
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
                        title: gettext("Type something to search with {name}").replace("{name}", &trigger.name),
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
                    title: gettext("{name}: {query}").replace("{name}", &trigger.name).replace("{query}", rest),
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
                        title: gettext("Type a command to run with {name}").replace("{name}", &trigger.name),
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
                    title: gettext("Run {name}").replace("{name}", &trigger.name),
                    subtitle: Some(rendered.clone()),
                    icon,
                    action: Action::RunWithProgress {
                        title: gettext("{name}: {query}").replace("{name}", &trigger.name).replace("{query}", rest),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A config carrying the dictionary keyword (store-installed trigger).
    fn config_with_dictionary() -> Config {
        let mut cfg = Config::default();
        cfg.command_keywords.push(crate::config::CommandKeyword {
            id: "dictionary".into(),
            word: "dict".into(),
            description: String::new(),
            extensions: vec![],
            icon: String::new(),
            all_files: false,
            shortcut: String::new(),
            enabled: true,
        });
        cfg
    }

    #[test]
    fn fuzzy_match_catches_typos_but_not_unrelated_words() {
        // Mid-word typos hit their target…
        assert!(fuzzy_match("sytem", "system"));
        assert!(fuzzy_match("fltak", "flatpak"));
        assert!(fuzzy_match("updte", "update"));
        assert!(fuzzy_match("shutdn", "Shutdown"));
        // …including classes nucleo's subsequence scoring rejects:
        assert!(fuzzy_match("sistem", "system"), "substituted letter");
        assert!(fuzzy_match("updaet", "update"), "swapped letters");
        assert!(fuzzy_match("aall", "all"), "doubled letter");
        // …short queries stay with the explicit prefix logic…
        assert!(!fuzzy_match("a", "anything"));
        // …and words that merely share letters never match.
        assert!(!fuzzy_match("upload", "update"));
        assert!(!fuzzy_match("zyx", "system"));
    }

    #[test]
    fn typoed_trigger_word_still_suggests_its_mode() {
        let snap = crate::index::Snapshot {
            apps: Vec::new(),
            files: Vec::new(),
        };
        let lock = std::sync::Arc::new(std::sync::RwLock::new(snap));
        let cfg = Config::default();

        // "find" with a dropped letter still suggests entering Find mode…
        let rows = super::search("fnd", &cfg, &lock);
        assert!(
            rows.iter()
                .any(|r| matches!(&r.action, Action::EnterMode(w) if w == "find")),
            "fnd: {:?}",
            rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>()
        );
        // …an unrelated word never suggests a mode. (Operation rows encode
        // their state in EnterMode too, so only real keyword suggestions
        // count here.)
        let rows = super::search("xyzzy", &cfg, &lock);
        assert!(
            !rows.iter().any(|r| match &r.action {
                Action::EnterMode(w) => cfg.keyword_for_word(w).is_some(),
                _ => false,
            }),
            "xyzzy: {:?}",
            rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn typoed_inline_trigger_still_routes() {
        let cfg = Config::default();
        // A dropped letter routes "fnd report" into the file search…
        assert_eq!(
            super::inline_file_mode("fnd report", &cfg),
            Some(("find".to_string(), "report".to_string()))
        );
        // …while short words, where unrelated queries live, never hijack.
        assert_eq!(super::inline_file_mode("san francisco", &cfg), None);
    }

    #[test]
    fn trigger_suggestions_match_typos_and_fully_typed_long_words() {
        let mut cfg = Config::default();
        cfg.command_keywords.push(crate::config::CommandKeyword {
            id: "superlong".into(),
            word: "superlongword".into(),
            description: "Test trigger".into(),
            extensions: vec![],
            icon: String::new(),
            all_files: false,
            shortcut: String::new(),
            enabled: true,
        });
        // The fully-typed 13-char word matches — the old <=8 gate hid it.
        let rows = super::trigger_suggestions("superlongword", &cfg);
        assert!(
            rows.iter()
                .any(|r| matches!(&r.action, Action::EnterMode(w) if w == "superlongword")),
            "long word: {:?}",
            rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>()
        );
        // A typo'd default keyword still suggests its mode…
        let rows = super::trigger_suggestions("fnd", &cfg);
        assert!(
            rows.iter()
                .any(|r| matches!(&r.action, Action::EnterMode(w) if w == "find")),
            "fnd: {:?}",
            rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>()
        );
        // …and non-words never match anything.
        assert!(super::trigger_suggestions("xyzzy123", &cfg).is_empty());
    }

    #[test]
    fn dict_word_inline_routes_to_the_dictionary() {
        let cfg = config_with_dictionary();
        // The whole point: `dict <word>` shows the meaning directly, without
        // a separate mode-entry step first.
        assert_eq!(
            inline_file_mode("dict serendipity", &cfg),
            Some(("dict".to_string(), "serendipity".to_string()))
        );
        assert_eq!(
            inline_file_mode("dict  spaced rest", &cfg),
            Some(("dict".to_string(), "spaced rest".to_string()))
        );
        // A lone keyword (no rest) is not an inline route — it stays the
        // mode suggestion the user enters with.
        assert_eq!(inline_file_mode("dict", &cfg), None);
        // Unknown words never route.
        assert_eq!(inline_file_mode("nope x", &cfg), None);

        // Regression guard: the all-files keyword still routes inline.
        let files = Config::default();
        assert_eq!(
            inline_file_mode("find notes.txt", &files),
            Some(("find".to_string(), "notes.txt".to_string()))
        );
    }
}



