//! User configuration: a single JSON file (`~/.config/spotty/config.json`).
//!
//! Loaded once at startup (`Config::load`) and held in `Rc<RefCell<…>>` on
//! the main thread. Every field has a `#[serde(default = …)]` so a missing or
//! partially-edited file degrades gracefully — `load` never fails, it just
//! merges what it can parse. `save` rewrites the whole file (only used by the
//! settings window and auto-shortcut registration, both rare).
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use crate::i18n::gettext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchEngine {
    DuckDuckGo,
    Google,
    Startpage,
    Ecosia,
    Brave,
    BrowserDefault,
    Custom,
}

impl SearchEngine {
    pub fn url_for(self, q: &str) -> String {
        let q = urlencoding::encode(q);
        match self {
            Self::DuckDuckGo => format!("https://duckduckgo.com/?q={q}"),
            Self::Google => format!("https://www.google.com/search?q={q}"),
            Self::Startpage => format!("https://www.startpage.com/do/search?q={q}"),
            Self::Ecosia => format!("https://www.ecosia.org/search?q={q}"),
            Self::Brave => format!("https://search.brave.com/search?q={q}"),
            Self::BrowserDefault => format!("https://duckduckgo.com/?q={q}"),
            Self::Custom => format!("https://duckduckgo.com/?q={q}"),
        }
    }
    pub fn display_name(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Google => "Google",
            Self::Startpage => "Startpage",
            Self::Ecosia => "Ecosia",
            Self::Brave => "Brave Search",
            Self::BrowserDefault => "Browser Default",
            Self::Custom => "Custom",
        }
    }
    pub fn all() -> &'static [Self] {
        &[
            Self::BrowserDefault,
            Self::DuckDuckGo,
            Self::Google,
            Self::Startpage,
            Self::Ecosia,
            Self::Brave,
            Self::Custom,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    #[default]
    FlatpakOnly,
    Both,
    DistroOnly,
    SnapOnly,
    DistroSnap,
    All,
}

impl PackageManager {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::FlatpakOnly => "Flatpak only",
            Self::Both => "Flatpak + Distro",
            Self::DistroOnly => "Distro only",
            Self::SnapOnly => "Snap only",
            Self::DistroSnap => "Snap + Distro",
            Self::All => "Flatpak + Distro + Snap",
        }
    }
    pub fn all() -> &'static [Self] {
        &[
            Self::FlatpakOnly,
            Self::Both,
            Self::DistroOnly,
            Self::SnapOnly,
            Self::DistroSnap,
            Self::All,
        ]
    }
    pub fn use_flatpak(self) -> bool {
        matches!(self, Self::FlatpakOnly | Self::Both | Self::All)
    }
    pub fn use_distro(self) -> bool {
        matches!(self, Self::DistroOnly | Self::Both | Self::DistroSnap | Self::All)
    }
    pub fn use_snap(self) -> bool {
        matches!(self, Self::SnapOnly | Self::DistroSnap | Self::All)
    }
}

fn default_custom_web_search_url() -> String {
    String::new()
}

/// A remappable command keyword. `id` is the internal action name (fixed),
/// `word` is the user-customizable trigger text, `description` explains it,
/// and `extensions` restricts file types for the keyword.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandKeyword {
    pub id: String,
    pub word: String,
    pub description: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Symbolic icon name shown on the mode chip (Raycast-style).
    #[serde(default)]
    pub icon: String,
    /// If true, this trigger searches ALL files (no extension filter).
    #[serde(default)]
    pub all_files: bool,
    /// Optional global shortcut that opens Spotty directly in this mode.
    #[serde(default)]
    pub shortcut: String,
    /// If false, the trigger is disabled: no dispatch, no shortcut slot.
    #[serde(default = "dt")]
    pub enabled: bool,
}

impl CommandKeyword {
    pub fn display_name(&self) -> &'static str {
        match self.id.as_str() {
            "files" => "Find",
            "clipboard" => "Clip",
            "cmd" => "App",
            "run" => "Cmd",
            "emoji" => "Emoji",
            "music" => "Music",
            "translate" => "Translate",
            _ => "Trigger",
        }
    }
}

fn default_command_keywords() -> Vec<CommandKeyword> {
    vec![
        CommandKeyword {
            id: "files".into(),
            word: "find".into(),
            description: gettext("Search all files and folders").into(),
            extensions: vec![],
            icon: "system-search-symbolic".into(),
            all_files: true,
            shortcut: "Super+Ctrl+F".into(),
            enabled: true,
        },
        CommandKeyword {
            id: "clipboard".into(),
            word: "clip".into(),
            description: gettext("Search clipboard history").into(),
            extensions: vec![],
            icon: "edit-paste-symbolic".into(),
            all_files: false,
            shortcut: "Super+Ctrl+V".into(),
            enabled: true,
        },
        CommandKeyword {
            id: "cmd".into(),
            word: "app".into(),
            description: gettext("Install, uninstall, and manage apps").into(),
            extensions: vec![],
            icon: "application-x-executable-symbolic".into(),
            all_files: false,
            shortcut: "Super+Ctrl+A".into(),
            enabled: true,
        },
        CommandKeyword {
            id: "run".into(),
            word: "cmd".into(),
            description: gettext("Run a command").into(),
            extensions: vec![],
            icon: "utilities-terminal-symbolic".into(),
            all_files: false,
            shortcut: "Super+Ctrl+T".into(),
            enabled: true,
        },
        CommandKeyword {
            id: "emoji".into(),
            word: "emoji".into(),
            description: gettext("Search emoji").into(),
            extensions: vec![],
            icon: "face-smile-symbolic".into(),
            all_files: false,
            shortcut: "Super+Ctrl+E".into(),
            enabled: true,
        },
        CommandKeyword {
            id: "bluetooth".into(),
            word: "bt".into(),
            description: gettext("Bluetooth devices").into(),
            extensions: vec![],
            icon: "bluetooth-active-symbolic".into(),
            all_files: false,
            shortcut: "Super+Ctrl+B".into(),
            enabled: true,
        },
    ]
}

/// Root config object. Shared app-wide via `Rc<RefCell<Config>>`; read
/// everywhere, written only by the settings window and keybinding sync.
/// Field order and defaults are managed by serde defaults — see module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "de")]
    pub search_engine: SearchEngine,
    #[serde(default = "default_custom_web_search_url")]
    pub custom_web_search_url: String,
    #[serde(default = "dt")]
    pub enable_apps: bool,
    #[serde(default = "dt")]
    pub enable_files: bool,
    #[serde(default = "dt")]
    pub enable_web: bool,
    #[serde(default = "dt")]
    pub enable_clipboard: bool,
    #[serde(default = "dt")]
    pub enable_calculator: bool,
    #[serde(default = "dt")]
    pub show_recent_file_searches: bool,
    /// Allow browsing absolute paths (/ and ~/) in the main search box.
    #[serde(default = "dt")]
    pub enable_root_browsing: bool,
    #[serde(default = "ds")]
    pub shortcut: String,
    #[serde(default = "dc")]
    pub clipboard_history_limit: usize,
    /// Retention period for clipboard entries in days. None or absent = keep forever.
    /// Entries older than this are pruned (pinned items are exempt).
    #[serde(default)]
    pub clipboard_retention_days: Option<u64>,
    /// Clipboard entries pinned by the user — always shown first in clip mode.
    #[serde(default)]
    pub pinned_clipboard: Vec<String>,
    /// Pinned copied images, by cache path — always shown first in clip mode.
    #[serde(default)]
    pub pinned_clipboard_images: Vec<String>,
    /// Pinned copied files/folders, by path — always shown first in clip mode.
    #[serde(default)]
    pub pinned_clipboard_files: Vec<String>,
    /// clipboard entry from the clipboard manager.
    #[serde(default = "default_pin_shortcut")]
    pub clipboard_pin_shortcut: String,
    /// Keyboard shortcut (GTK accelerator string) to delete the selected
    /// clipboard entry from the clipboard manager.
    #[serde(default = "default_delete_shortcut")]
    pub clipboard_delete_shortcut: String,
    /// In-window shortcuts shown/used in the search window (not global
    /// GNOME keybindings). Empty string disables the action.
    #[serde(default = "default_operations_shortcut")]
    pub operations_shortcut: String,
    #[serde(default = "default_hints_shortcut")]
    pub hints_shortcut: String,
    #[serde(default = "default_copy_shortcut")]
    pub copy_shortcut: String,
    #[serde(default = "default_cut_shortcut")]
    pub cut_shortcut: String,
    #[serde(default = "default_paste_shortcut")]
    pub paste_shortcut: String,
    #[serde(default = "default_terminal_shortcut")]
    pub terminal_shortcut: String,
    /// Find mode: open the selected file's/folder's location in the default
    /// file manager (works with any file manager).
    #[serde(default = "default_open_location_shortcut")]
    pub open_location_shortcut: String,
    /// Find mode: move the selected file/folder to the Trash (confirmation
    /// dialog first — default answer is No).
    #[serde(default = "default_delete_file_shortcut")]
    pub delete_file_shortcut: String,
    #[serde(default = "default_uninstall_shortcut")]
    pub uninstall_shortcut: String,
    #[serde(default = "default_kill_shortcut")]
    pub kill_shortcut: String,
    #[serde(default = "default_select_all_shortcut")]
    pub select_all_shortcut: String,
    #[serde(default = "default_undo_shortcut")]
    pub undo_shortcut: String,
    #[serde(default = "default_redo_shortcut")]
    pub redo_shortcut: String,
    #[serde(default = "default_delete_word_shortcut")]
    pub delete_word_shortcut: String,
    /// Update shortcuts (search window, update context): check + upgrade
    /// everything, system packages only, flatpak only.
    #[serde(default = "default_update_all_shortcut")]
    pub update_all_shortcut: String,
    #[serde(default = "default_update_system_shortcut")]
    pub update_system_shortcut: String,
    #[serde(default = "default_update_flatpak_shortcut")]
    pub update_flatpak_shortcut: String,
    /// Universally pinned search results (apps, files, web searches, etc.) —
    /// always shown first when the query matches, in any search mode.
    #[serde(default)]
    pub pinned_results: Vec<crate::search::SearchResult>,
    /// Whether to show a small keyboard-shortcut hint bar in the search window.
    #[serde(default = "dt")]
    pub show_shortcut_hints: bool,
    #[serde(default = "dm")]
    pub max_index_entries: usize,
    /// Remappable command keywords for built-in search modes.
    #[serde(default = "default_command_keywords")]
    pub command_keywords: Vec<CommandKeyword>,
    /// Legacy clipboard shortcut migrated into the clipboard keyword on load.
    #[serde(default, skip_serializing)]
    pub clipboard_shortcut: String,
    /// Which package manager(s) to use in the cmd trigger for install/uninstall/search.
    #[serde(default)]
    pub package_manager: PackageManager,
    /// Also surface installable apps (Flatpak / distro) in the default search,
    /// so the user finds apps to install without typing the "install" verb.
    #[serde(default = "dt")]
    pub enable_new_apps: bool,
    /// Base URL of the trigger repository Spotty browses for installable
    /// triggers (`index.json` + `triggers/<id>.json`). Only contacted when
    /// the user opens Settings → Trigger → Store.
    #[serde(default = "default_trigger_repo_url")]
    pub trigger_repo_url: String,
    /// Target-language override for the translate trigger (e.g. "de");
    /// empty = follow the system language.
    #[serde(default)]
    pub translate_target: String,
    /// Local LibreTranslate endpoint for the translate trigger. The query
    /// text is only ever POSTed here — by default that is your own machine,
    /// so translations are 100% private.
    #[serde(default = "default_translate_endpoint")]
    pub translate_endpoint: String,
    /// Optional API key for that endpoint (self-hosted instances normally
    /// don't need one).
    #[serde(default)]
    pub translate_api_key: String,
    /// Background update checking + the search "update" verb (updates are a
    /// General-section feature, not a trigger).
    #[serde(default = "dt")]
    pub enable_updates: bool,
    /// Badge + banner near the orb when updates are available.
    #[serde(default = "dt")]
    pub update_notification: bool,
    /// How often the background update check runs, in hours.
    #[serde(default = "default_update_check_hours")]
    pub update_check_interval_hours: u32,
    /// Epoch seconds until which the update notice is snoozed
    /// ("Remind tomorrow"); 0 = not snoozed.
    #[serde(default)]
    pub update_snooze_until: i64,
    /// Signature of an update set the user already dismissed — the notice
    /// stays hidden until a *different* (new) update shows up.
    #[serde(default)]
    pub update_dismissed_sig: String,
}

/// The default trigger repository: the official spotty-triggers repository
/// read straight from raw GitHub — no server involved.
pub fn default_trigger_repo_url() -> String {
    "https://raw.githubusercontent.com/Aras1907/spotty-triggers/main".into()
}

/// LibreTranslate's own default: a local-only instance on 127.0.0.1:5000.
pub fn default_translate_endpoint() -> String {
    "http://localhost:5000".into()
}

/// Background update check cadence: once a day.
pub fn default_update_check_hours() -> u32 {
    24
}

fn de() -> SearchEngine {
    SearchEngine::BrowserDefault
}
fn dt() -> bool {
    true
}
fn ds() -> String {
    "Super+Space".into()
}
fn dc() -> usize {
    1000
}
fn dm() -> usize {
    50_000
}
fn default_pin_shortcut() -> String {
    "<Control>p".into()
}
fn default_delete_shortcut() -> String {
    "Delete".into()
}
fn default_operations_shortcut() -> String {
    "<Control>o".into()
}
fn default_hints_shortcut() -> String {
    "<Control>h".into()
}
fn default_copy_shortcut() -> String {
    "<Control>c".into()
}
fn default_cut_shortcut() -> String {
    "<Control>x".into()
}
fn default_paste_shortcut() -> String {
    "<Control>v".into()
}
fn default_terminal_shortcut() -> String {
    // Ctrl+Enter is taken by "open location in file manager"; the terminal
    // action lives on Ctrl+Shift+Enter (migrate_resource_defaults moves old
    // configs off the previous Ctrl+Enter default).
    "<Control><Shift>Return".into()
}
fn default_open_location_shortcut() -> String {
    "<Control>Return".into()
}
fn default_delete_file_shortcut() -> String {
    "<Control>d".into()
}
fn default_uninstall_shortcut() -> String {
    "<Control>u".into()
}
fn default_kill_shortcut() -> String {
    "<Control>k".into()
}
fn default_select_all_shortcut() -> String {
    "<Control>a".into()
}
fn default_undo_shortcut() -> String {
    "<Control>z".into()
}
fn default_redo_shortcut() -> String {
    "<Control><Shift>z".into()
}
fn default_delete_word_shortcut() -> String {
    "<Control>space".into()
}
fn default_update_all_shortcut() -> String {
    // Ctrl+Enter only means "open location" inside Find mode; in the
    // update context it checks for updates and upgrades everything.
    "<Control>Return".into()
}
fn default_update_system_shortcut() -> String {
    // Ctrl+D is "delete file" only inside Find mode.
    "<Control>d".into()
}
fn default_update_flatpak_shortcut() -> String {
    "<Control>f".into()
}
impl Default for Config {
    fn default() -> Self {
        Self {
            search_engine: de(),
            custom_web_search_url: default_custom_web_search_url(),
            enable_apps: true,
            enable_files: true,
            enable_web: true,
            enable_clipboard: true,
            enable_calculator: true,
            show_recent_file_searches: true,
            enable_root_browsing: true,
            shortcut: ds(),
            clipboard_history_limit: dc(),
            clipboard_retention_days: None,
            pinned_clipboard: Vec::new(),
            pinned_clipboard_images: Vec::new(),
            pinned_clipboard_files: Vec::new(),
            clipboard_pin_shortcut: default_pin_shortcut(),
            clipboard_delete_shortcut: default_delete_shortcut(),
            operations_shortcut: default_operations_shortcut(),
            hints_shortcut: default_hints_shortcut(),
            copy_shortcut: default_copy_shortcut(),
            cut_shortcut: default_cut_shortcut(),
            paste_shortcut: default_paste_shortcut(),
            terminal_shortcut: default_terminal_shortcut(),
            open_location_shortcut: default_open_location_shortcut(),
            delete_file_shortcut: default_delete_file_shortcut(),
            uninstall_shortcut: default_uninstall_shortcut(),
            kill_shortcut: default_kill_shortcut(),
            select_all_shortcut: default_select_all_shortcut(),
            undo_shortcut: default_undo_shortcut(),
            redo_shortcut: default_redo_shortcut(),
            delete_word_shortcut: default_delete_word_shortcut(),
            update_all_shortcut: default_update_all_shortcut(),
            update_system_shortcut: default_update_system_shortcut(),
            update_flatpak_shortcut: default_update_flatpak_shortcut(),
            pinned_results: Vec::new(),
            show_shortcut_hints: true,
            max_index_entries: dm(),
            command_keywords: default_command_keywords(),
            clipboard_shortcut: String::new(),
            package_manager: PackageManager::default(),
            enable_new_apps: true,
            trigger_repo_url: default_trigger_repo_url(),
            translate_target: String::new(),
            translate_endpoint: default_translate_endpoint(),
            translate_api_key: String::new(),
            enable_updates: true,
            update_notification: true,
            update_check_interval_hours: default_update_check_hours(),
            update_snooze_until: 0,
            update_dismissed_sig: String::new(),
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        dirs::config_dir().unwrap().join("spotty/config.json")
    }

    /// Read + merge the config file. Never fails: a missing file, corrupt
    /// JSON, or unknown fields all fall back to serde defaults. Called once
    /// at startup; hot path for every other access is the `Rc` clone, not
    /// this.
    pub fn load() -> Self {
        // Pre-filter: drop pinned_results entries referencing the removed
        // `PlayMusic` action so old configs don't fail to deserialize.
        let raw = fs::read_to_string(Self::config_path()).ok();
        let mut cfg: Config = raw.as_deref().and_then(|s| {
            let mut v: serde_json::Value = serde_json::from_str(s).ok()?;
            if let Some(arr) = v.get_mut("pinned_results").and_then(|x| x.as_array_mut()) {
                arr.retain(|r| {
                    r.get("action")
                        .and_then(|a| a.get("PlayMusic"))
                        .is_none()
                });
            }
            serde_json::from_value(v).ok()
        }).unwrap_or_default();
        let before = serde_json::to_string(&cfg).unwrap_or_default();
        cfg.migrate_keywords();
        cfg.migrate_resource_defaults();
        let after = serde_json::to_string(&cfg).unwrap_or_default();
        if before != after {
            let _ = cfg.save();
        }
        cfg
    }

    /// Keep only the supported built-in trigger rows and add any missing ones.
    /// Older configs had many file-type triggers; those are intentionally pruned.
    fn migrate_keywords(&mut self) {
        let defaults = default_command_keywords();
        self.command_keywords
            .retain(|kw| defaults.iter().any(|def| def.id == kw.id));
        for def in &defaults {
            match self.command_keywords.iter_mut().find(|k| k.id == def.id) {
                Some(existing) => {
                    // Backfill metadata older configs didn't have.
                    if existing.icon.is_empty()
                        || matches!(
                            existing.icon.as_str(),
                            "application-pdf-symbolic"
                                | "x-office-presentation-symbolic"
                                | "x-office-document-symbolic"
                                | "x-office-spreadsheet-symbolic"
                                | "utilities-terminal-symbolic"
                                | "folder-symbolic"
                                // Removed from modern Adwaita — refresh to
                                // the keyword's current default icon.
                                | "system-software-update-symbolic"
                        )
                    {
                        existing.icon = def.icon.clone();
                    }
                    // The "cmd" trigger was originally framed as a terminal
                    // command runner; older configs may still have its old
                    // word/wording from before it became app-focused.
                    if existing.id == "cmd" && existing.word == "cmd" {
                        existing.word = def.word.clone();
                    }
                    // The "cmd"/App trigger used to own Super+Ctrl+T, which now
                    // belongs to the new free-form "cmd" command runner ("run").
                    // Move older configs off that shortcut so the two don't
                    // register the same accelerator with GNOME.
                    if existing.id == "cmd" && existing.shortcut == "Super+Ctrl+T" {
                        existing.shortcut = def.shortcut.clone();
                    }
                    // The "files" trigger was originally "files"; rename its
                    // default word to "find" for older configs.
                    if existing.id == "files" && existing.word == "files" {
                        existing.word = def.word.clone();
                    }
                    existing.all_files = def.all_files;
                    if existing.shortcut.is_empty() {
                        existing.shortcut = def.shortcut.clone();
                    }
                    existing.extensions = def.extensions.clone();
                    // Refresh the description to the current wording.
                    existing.description = def.description.clone();
                }
                None => self.command_keywords.push(def.clone()),
            }
        }
        if !self.clipboard_shortcut.trim().is_empty() {
            if let Some(clipboard) = self
                .command_keywords
                .iter_mut()
                .find(|k| k.id == "clipboard")
            {
                if clipboard.shortcut.trim().is_empty() {
                    clipboard.shortcut = self.clipboard_shortcut.clone();
                }
            }
            self.clipboard_shortcut.clear();
        }
    }

    fn migrate_resource_defaults(&mut self) {
        if self.max_index_entries > 75_000 {
            self.max_index_entries = dm();
        }
        // Migrate old clipboard history limit default (100 → 1000).
        if self.clipboard_history_limit == 100 {
            self.clipboard_history_limit = 1000;
        }
        // Ctrl+Enter now opens the location in the file manager; the terminal
        // action moved to Ctrl+Shift+Enter. Move configs still sitting on the
        // old default so the two don't both match the same accelerator
        // (same pattern as the old cmd/run Super+Ctrl+T move).
        if self.terminal_shortcut == "<Control>Return" {
            self.terminal_shortcut = default_terminal_shortcut();
        }
    }

    pub fn save(&self) {
        let p = Self::config_path();
        let _ = fs::create_dir_all(p.parent().unwrap());
        let _ = fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
    }

    /// Get the extensions for a command keyword by the word the user typed.
    pub fn extensions_for_word(&self, word: &str) -> Option<&Vec<String>> {
        self.command_keywords
            .iter()
            .find(|k| k.word.eq_ignore_ascii_case(word) && !k.extensions.is_empty())
            .map(|k| &k.extensions)
    }

    /// Find a command keyword whose trigger word matches `word` exactly.
    /// Falls back to installed triggers (see crate::triggers) so trigger words
    /// from imported manifests work through the same paths as built-ins.
    /// Disabled triggers never match.
    pub fn keyword_for_word(&self, word: &str) -> Option<CommandKeyword> {
        self.command_keywords
            .iter()
            .find(|k| k.enabled && k.word.eq_ignore_ascii_case(word))
            .cloned()
            .or_else(|| crate::triggers::keyword_for_word(word))
    }

    pub fn keyword_for_id(&self, id: &str) -> Option<CommandKeyword> {
        self.command_keywords
            .iter()
            .find(|k| k.enabled && k.id == id)
            .cloned()
            .or_else(|| crate::triggers::keyword_for_id(id))
    }

    pub fn web_search_url_for(&self, q: &str) -> String {
        let custom = self.custom_web_search_url.trim();
        if self.search_engine == SearchEngine::BrowserDefault {
            if let Some(url) = crate::search::browser_engine::url_for(q) {
                return url;
            }
            return self.search_engine.url_for(q);
        }
        if self.search_engine != SearchEngine::Custom || custom.is_empty() {
            return self.search_engine.url_for(q);
        }
        let encoded = urlencoding::encode(q);
        if custom.contains("{query}") {
            custom.replace("{query}", &encoded)
        } else if custom.contains("{}") {
            custom.replace("{}", &encoded)
        } else {
            let sep = if custom.contains('?') { '&' } else { '?' };
            format!("{custom}{sep}q={encoded}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_shortcut_defaults_and_terminal_migration() {
        // Fresh defaults: Ctrl+Enter opens the location in the file manager,
        // terminal moved to Ctrl+Shift+Enter, Ctrl+D deletes behind a
        // confirmation dialog.
        let cfg = Config::default();
        assert_eq!(cfg.open_location_shortcut, "<Control>Return");
        assert_eq!(cfg.delete_file_shortcut, "<Control>d");
        assert_eq!(cfg.terminal_shortcut, "<Control><Shift>Return");

        // Old configs still carrying the previous terminal default move off
        // it so both actions don't match the same accelerator.
        let mut cfg = Config::default();
        cfg.terminal_shortcut = "<Control>Return".into();
        cfg.migrate_resource_defaults();
        assert_eq!(cfg.terminal_shortcut, "<Control><Shift>Return");

        // A deliberately customized terminal shortcut is left alone.
        cfg.terminal_shortcut = "<Super>t".into();
        cfg.migrate_resource_defaults();
        assert_eq!(cfg.terminal_shortcut, "<Super>t");
    }

    #[test]
    fn config_without_new_fields_deserializes_with_defaults() {
        // Configs written before the fields existed must still load and pick
        // up the new defaults via serde.
        let cfg: Config = serde_json::from_str(r#"{"shortcut": "Super+Space"}"#).unwrap();
        assert_eq!(cfg.shortcut, "Super+Space");
        assert_eq!(cfg.open_location_shortcut, "<Control>Return");
        assert_eq!(cfg.delete_file_shortcut, "<Control>d");
        assert_eq!(cfg.terminal_shortcut, "<Control><Shift>Return");
        assert_eq!(
            cfg.trigger_repo_url,
            "https://raw.githubusercontent.com/Aras1907/spotty-triggers/main"
        );
    }
}

#[cfg(test)]
mod update_flag_tests {
    #[test]
    fn serde_defaults_for_update_flags() {
        let c: super::Config = serde_json::from_str("{}").expect("empty config");
        assert!(c.enable_updates, "enable_updates must default to true");
        assert!(c.update_notification, "update_notification must default to true");
        assert_eq!(c.update_check_interval_hours, 24);
        assert_eq!(c.update_snooze_until, 0);
    }

    #[test]
    fn serde_defaults_for_update_shortcuts() {
        let c = crate::config::Config::default();
        assert_eq!(c.update_all_shortcut, "<Control>Return");
        assert_eq!(c.update_system_shortcut, "<Control>d");
        assert_eq!(c.update_flatpak_shortcut, "<Control>f");
        // Config files written before these keys existed still get them.
        let c: super::Config = serde_json::from_str("{}").expect("empty config");
        assert_eq!(c.update_all_shortcut, "<Control>Return");
        assert_eq!(c.update_system_shortcut, "<Control>d");
        assert_eq!(c.update_flatpak_shortcut, "<Control>f");
    }
}
