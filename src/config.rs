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
    /// Every engine the settings can offer. `BrowserDefault` is deliberately
    /// not among them: detection is no longer a choice in the combo, so an
    /// old config still saved as `browserdefault` is migrated to DuckDuckGo
    /// on load and the list only ever shows concrete engines.
    pub fn all() -> &'static [Self] {
        &[
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
    /// The legacy combo, translated into the four independent source
    /// switches (Settings → Search for new Apps). Only used to migrate
    /// configs written before the switches existed.
    pub fn to_sources(self) -> AppSources {
        match self {
            Self::FlatpakOnly => AppSources {
                flatpak: true,
                distro: false,
                snap: false,
                appimage: true,
            },
            Self::Both => AppSources {
                flatpak: true,
                distro: true,
                snap: false,
                appimage: true,
            },
            Self::DistroOnly => AppSources {
                flatpak: false,
                distro: true,
                snap: false,
                appimage: true,
            },
            Self::SnapOnly => AppSources {
                flatpak: false,
                distro: false,
                snap: true,
                appimage: true,
            },
            Self::DistroSnap => AppSources {
                flatpak: false,
                distro: true,
                snap: true,
                appimage: true,
            },
            Self::All => AppSources {
                flatpak: true,
                distro: true,
                snap: true,
                appimage: true,
            },
        }
    }
}

/// Which app sources the search/install features consult — one independent
/// switch per source (replaces the old single `package_manager` combo, so
/// any combination is expressible). Availability gating (snapd present,
/// AppImages found...) lives in the Settings UI, not here: the switches only
/// record what the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSources {
    pub flatpak: bool,
    pub distro: bool,
    pub snap: bool,
    pub appimage: bool,
}

impl AppSources {
    pub const fn new(flatpak: bool, distro: bool, snap: bool, appimage: bool) -> Self {
        Self {
            flatpak,
            distro,
            snap,
            appimage,
        }
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
        crate::trigger_defaults::display_name(&self.id)
    }

    /// True for the result types (Applications, Web Search, …) that sit in
    /// the trigger list next to the real triggers.
    pub fn is_result(&self) -> bool {
        RESULT_IDS.contains(&self.id.as_str())
    }

    /// What identifies this keyword's mode to the search: its word, or — for a
    /// keyword with no word, reachable only by its shortcut — its id.
    pub fn mode_key(&self) -> &str {
        if self.word.is_empty() {
            &self.id
        } else {
            &self.word
        }
    }

    /// The mode chip's text.
    pub fn chip_label(&self) -> String {
        if self.word.is_empty() {
            self.display_name().to_string()
        } else {
            crate::search::capitalize(&self.word)
        }
    }
}

pub use crate::trigger_defaults::{DEFAULT_ORDER, RESULT_IDS, STORE_ICON};
use crate::trigger_defaults::command_keywords as default_command_keywords;


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
    /// The converter's own switch. `None` in a config from before it was split
    /// from the calculator: it then follows the calculator (see
    /// [`Config::converter_enabled`]) until the first load pins it.
    #[serde(default)]
    pub enable_converter: Option<bool>,
    /// Fraction digits shown in calculator results (2/4/6/8/10).
    #[serde(default = "default_calc_precision")]
    pub calc_precision: u32,
    /// Group the integer part of a result for display (1,234,567). The
    /// copied value never gets separators.
    #[serde(default)]
    pub calc_separators: bool,
    /// Also show hex/octal/binary of integer results in the subtitle.
    #[serde(default)]
    pub calc_bases: bool,
    /// Enter pastes the result into the previously focused app (Ctrl+V
    /// after the usual delay); off = copy only, like it always was.
    #[serde(default)]
    pub calc_paste: bool,
    /// Show "expression = result" as the row title; off = result only.
    #[serde(default = "dt")]
    pub calc_show_expr: bool,
    /// Unit conversion rows (`10 km to mi`) — master switch for the
    /// "Conversions" group.
    #[serde(default = "dt")]
    pub calc_converter: bool,
    /// Also show a list of equivalents when a query names a unit (or
    /// currency) but no target: `10 km`.
    #[serde(default = "dt")]
    pub calc_equivalents: bool,
    /// Number-base conversion rows (`255 to hex`, `0xff to dec`).
    #[serde(default = "dt")]
    pub calc_base_convert: bool,
    /// Live currency (plus crypto/metals) conversion rows. Rates come
    /// from a free keyless API, fetched on demand and disk-cached.
    #[serde(default = "dt")]
    pub calc_currency: bool,
    /// Scientific notation for results fixed notation would drown
    /// (|v| >= 1e15) or lose to rounding (rounds to zero).
    #[serde(default = "dt")]
    pub calc_sci_notation: bool,
    /// Connector words the converter accepts between source and target
    /// (`20 USD to EUR`, `20 USD em EUR`). The symbols `->`, `→` and `=`
    /// always work; Settings offers presets plus a custom entry.
    #[serde(default = "default_calc_convert_words")]
    pub calc_convert_words: Vec<String>,
    /// Currency a no-target conversion aims at (`20 EUR` -> …). Falls
    /// back to the automatic pick when the source *is* this currency.
    #[serde(default = "default_calc_default_currency")]
    pub calc_default_currency: String,
    /// Per-dimension default target unit for no-target rows
    /// (`{"length": "mi"}`); a missing key means the automatic pick.
    #[serde(default)]
    pub calc_default_targets: std::collections::BTreeMap<String, String>,
    #[serde(default = "dt")]
    pub show_recent_file_searches: bool,
    /// Maximum number of search results shown before the list scrolls.
    #[serde(default = "default_visible_result_limit")]
    pub visible_result_limit: usize,
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
    /// Footer bar: show the key indicator on the Operations button. The button
    /// itself is unaffected — it stays clickable and its shortcut keeps working.
    #[serde(default = "dt")]
    pub show_operations_shortcut_label: bool,
    /// Footer bar: show the key indicator on the Hints button.
    #[serde(default = "dt")]
    pub show_hints_shortcut_label: bool,
    /// Footer bar: show the icons on the Operations and Hints buttons. With
    /// the icons *and* both key indicators off there is nothing left to show,
    /// so the bar disappears — the popovers stay reachable by shortcut.
    #[serde(default = "dt")]
    pub show_footer_icons: bool,
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
    /// A web result: open the search in a private/incognito window of the
    /// default browser. Ctrl+Enter is scoped to the selected result type, so
    /// this does not conflict with Find's terminal shortcut.
    #[serde(default = "default_private_search_shortcut")]
    pub private_search_shortcut: String,
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
    /// Universally pinned search results (apps, files, web searches, etc.) —
    /// always shown first when the query matches, in any search mode.
    #[serde(default)]
    pub pinned_results: Vec<crate::search::SearchResult>,
    /// Built-in triggers and result types the user uninstalled: hidden from
    /// the list and switched off until reinstalled from the Store.
    #[serde(default)]
    pub uninstalled_builtins: Vec<String>,
    /// Result types the user took out of the regular search while giving them
    /// a trigger word. A type without a word is never listed here: the
    /// regular search is then the only way to reach it.
    #[serde(default)]
    pub regular_search_off: Vec<String>,
    /// Triggers (built-in or installed) whose results the user also wants in
    /// the regular search. The opposite default to a result type: a trigger is
    /// reached by its word unless it opts in here.
    #[serde(default)]
    pub regular_search_on: Vec<String>,
    /// The user's order of result types and triggers (ids, highest first).
    /// Anything missing — a new built-in, a freshly installed trigger — is
    /// placed per [`DEFAULT_ORDER`], then at the end (see
    /// [`Config::ordered_ids`]).
    #[serde(default)]
    pub result_order: Vec<String>,
    /// Whether to show a small keyboard-shortcut hint bar in the search window.
    #[serde(default = "dt")]
    pub show_shortcut_hints: bool,
    #[serde(default = "dm")]
    pub max_index_entries: usize,
    /// Remappable command keywords for built-in search modes.
    #[serde(default = "default_command_keywords")]
    pub command_keywords: Vec<CommandKeyword>,
    /// Proton Bridge is a local background mail server, configured outside search.
    #[serde(default)]
    pub proton_bridge_enabled: bool,
    /// Whether Spotty's optional controls for an already-installed Proton VPN CLI are enabled.
    #[serde(default)]
    pub proton_vpn_enabled: bool,
    /// Preferred Proton VPN country code for `vpn on` and quick connect;
    /// empty connects to the fastest server.
    #[serde(default)]
    pub proton_vpn_country: String,
    /// Proton Calendar web integration (`cal` trigger) installed from the Store.
    #[serde(default)]
    pub proton_calendar_enabled: bool,
    /// Proton Calendar view opened by the trigger: day, week or month.
    #[serde(default = "default_proton_calendar_view")]
    pub proton_calendar_view: String,
    /// Proton account slot (`/u/N`) the calendar opens.
    #[serde(default)]
    pub proton_calendar_account: u32,
    /// Proton Drive web integration (`drive` trigger) installed from the Store.
    #[serde(default)]
    pub proton_drive_enabled: bool,
    /// Proton account slot (`/u/N`) Drive opens.
    #[serde(default)]
    pub proton_drive_account: u32,
    /// Optional local folder kept in sync with Proton Drive; searched by `drive`.
    #[serde(default)]
    pub proton_drive_folder: String,
    /// Legacy clipboard shortcut migrated into the clipboard keyword on load.
    #[serde(default, skip_serializing)]
    pub clipboard_shortcut: String,
    /// App sources for search/install, one switch each (Settings → Search
    /// for new Apps). `Option<bool>` so a config written before the
    /// switches existed is detectable and gets migrated on load.
    #[serde(default)]
    pub pm_flatpak: Option<bool>,
    #[serde(default)]
    pub pm_distro: Option<bool>,
    #[serde(default)]
    pub pm_snap: Option<bool>,
    #[serde(default)]
    pub pm_appimage: Option<bool>,
    /// Legacy single-choice package manager: migrated into the four
    /// switches on load and never written back.
    #[serde(default, skip_serializing)]
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
    /// Explicit consent for requests to download missing web/app icons.
    #[serde(default)]
    pub allow_network_icons: bool,
    /// Search queries may contain secrets; persistence is opt-in.
    #[serde(default)]
    pub save_search_history: bool,
    /// Target-language override for the translate trigger (e.g. "de");
    /// empty = follow the system language.
    #[serde(default)]
    pub translate_target: String,
    /// LibreTranslate endpoint: local by default. A remote endpoint receives
    /// the typed text and any API key over HTTPS.
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

fn default_proton_calendar_view() -> String {
    "week".into()
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

/// Calculator fraction digits: 6 keeps the long-standing behaviour.
fn default_calc_convert_words() -> Vec<String> {
    vec!["to".to_string(), "in".to_string()]
}

fn default_calc_default_currency() -> String {
    "usd".to_string()
}

fn default_calc_precision() -> u32 {
    6
}

fn de() -> SearchEngine {
    // Not `BrowserDefault`: the combo no longer offers it, so the factory
    // value is the first engine it does offer. `Config::load` migrates old
    // files that still say `browserdefault` to the same value.
    SearchEngine::DuckDuckGo
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
fn default_visible_result_limit() -> usize {
    5
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
    "<Control>Return".into()
}
fn default_open_location_shortcut() -> String {
    "<Control><Shift>Return".into()
}
fn default_private_search_shortcut() -> String {
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
            enable_converter: Some(true),
            calc_precision: default_calc_precision(),
            calc_separators: false,
            calc_bases: false,
            calc_paste: false,
            calc_show_expr: true,
            calc_converter: true,
            calc_equivalents: true,
            calc_base_convert: true,
            calc_currency: true,
            calc_sci_notation: true,
            calc_convert_words: default_calc_convert_words(),
            calc_default_currency: default_calc_default_currency(),
            calc_default_targets: std::collections::BTreeMap::new(),
            show_recent_file_searches: true,
            visible_result_limit: default_visible_result_limit(),
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
            show_operations_shortcut_label: true,
            show_hints_shortcut_label: true,
            show_footer_icons: true,
            copy_shortcut: default_copy_shortcut(),
            cut_shortcut: default_cut_shortcut(),
            paste_shortcut: default_paste_shortcut(),
            terminal_shortcut: default_terminal_shortcut(),
            open_location_shortcut: default_open_location_shortcut(),
            private_search_shortcut: default_private_search_shortcut(),
            delete_file_shortcut: default_delete_file_shortcut(),
            uninstall_shortcut: default_uninstall_shortcut(),
            kill_shortcut: default_kill_shortcut(),
            select_all_shortcut: default_select_all_shortcut(),
            undo_shortcut: default_undo_shortcut(),
            redo_shortcut: default_redo_shortcut(),
            delete_word_shortcut: default_delete_word_shortcut(),
            pinned_results: Vec::new(),
            uninstalled_builtins: Vec::new(),
            regular_search_off: Vec::new(),
            regular_search_on: Vec::new(),
            result_order: Vec::new(),
            show_shortcut_hints: true,
            max_index_entries: dm(),
            command_keywords: default_command_keywords(),
            proton_bridge_enabled: false,
            proton_vpn_enabled: false,
            proton_vpn_country: String::new(),
            proton_calendar_enabled: false,
            proton_calendar_view: default_proton_calendar_view(),
            proton_calendar_account: 0,
            proton_drive_enabled: false,
            proton_drive_account: 0,
            proton_drive_folder: String::new(),
            clipboard_shortcut: String::new(),
            pm_flatpak: Some(true),
            pm_distro: Some(false),
            pm_snap: Some(false),
            pm_appimage: Some(true),
            package_manager: PackageManager::default(),
            enable_new_apps: true,
            trigger_repo_url: default_trigger_repo_url(),
            allow_network_icons: false,
            save_search_history: false,
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

    /// The active app sources (post-migration; any missing switch falls back
    /// to the fresh-install default).
    pub fn app_sources(&self) -> AppSources {
        AppSources::new(
            self.pm_flatpak.unwrap_or(true),
            self.pm_distro.unwrap_or(false),
            self.pm_snap.unwrap_or(false),
            self.pm_appimage.unwrap_or(true),
        )
    }

    /// Read + merge the config file. Never fails: a missing file, corrupt
    /// JSON, or unknown fields all fall back to serde defaults. Called once
    /// at startup; hot path for every other access is the `Rc` clone, not
    /// this.
    pub fn load() -> Self {
        Self::load_from_path(&Self::config_path())
    }

    fn load_from_path(path: &std::path::Path) -> Self {
        // Pre-filter: drop pinned_results entries referencing the removed
        // `PlayMusic` action so old configs don't fail to deserialize.
        let raw = fs::read_to_string(path).ok();
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
        cfg.migrate_proton_bridge_service();
        cfg.migrate_keywords();
        cfg.migrate_proton_vpn_trigger();
        cfg.migrate_converter_switch();
        cfg.migrate_resource_defaults();
        cfg.migrate_app_sources();
        cfg.migrate_search_engine();
        let after = serde_json::to_string(&cfg).unwrap_or_default();
        if before != after {
            if let Err(error) = cfg.save_to_path(path) {
                log::warn!("config: migration save failed: {error}");
            }
        }
        cfg.apply_privacy_preferences();
        cfg
    }

    /// "Browser Default" was the factory value while detection lived behind
    /// that choice; the settings no longer offer it, so an old config that
    /// still says it lands on the list's first engine — the combo always has
    /// a selection and the choice stays concrete. Idempotent.
    fn migrate_search_engine(&mut self) {
        if self.search_engine == SearchEngine::BrowserDefault {
            self.search_engine = SearchEngine::DuckDuckGo;
        }
    }

    /// Old configs stored one `package_manager` combo; the four source
    /// switches replace it (any combination is now expressible). Fills only
    /// the switches the file didn't have, so a hand-edited config keeps its
    /// choices and a migrated one is idempotent.
    fn migrate_app_sources(&mut self) {
        if self.pm_flatpak.is_some()
            && self.pm_distro.is_some()
            && self.pm_snap.is_some()
            && self.pm_appimage.is_some()
        {
            return;
        }
        let s = self.package_manager.to_sources();
        self.pm_flatpak.get_or_insert(s.flatpak);
        self.pm_distro.get_or_insert(s.distro);
        self.pm_snap.get_or_insert(s.snap);
        self.pm_appimage.get_or_insert(s.appimage);
    }

    /// Keep only the supported built-in trigger rows and add any missing ones.
    /// Older configs had many file-type triggers; those are intentionally pruned.
    fn migrate_keywords(&mut self) {
        let defaults = default_command_keywords();
        self.command_keywords
            .retain(|kw| crate::trigger_defaults::supports_native(&kw.id));
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
                                // Adwaita's legacy bag, replaced by Spotty's
                                // own store glyph.
                                | "system-software-install-symbolic"
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

    /// Move old Proton Bridge trigger installs into the service switch. The
    /// account and Bridge data live outside Spotty and are left untouched.
    fn migrate_proton_bridge_service(&mut self) {
        if let Some(old) = self.command_keywords.iter().find(|k| k.id == "proton-bridge") {
            self.proton_bridge_enabled |= old.enabled
                && !self.uninstalled_builtins.iter().any(|id| id == "proton-bridge");
        }
        self.command_keywords.retain(|k| k.id != "proton-bridge");
        self.uninstalled_builtins.retain(|id| id != "proton-bridge");
        self.regular_search_on.retain(|id| id != "proton-bridge");
        self.regular_search_off.retain(|id| id != "proton-bridge");
        self.result_order.retain(|id| id != "proton-bridge");
    }

    /// Whether the Store installed the Proton integration `id`.
    pub fn proton_service_enabled(&self, id: &str) -> bool {
        match id {
            "proton-bridge" => self.proton_bridge_enabled,
            "proton-vpn" => self.proton_vpn_enabled,
            "proton-calendar" => self.proton_calendar_enabled,
            "proton-drive" => self.proton_drive_enabled,
            _ => false,
        }
    }

    /// Install or uninstall a Proton integration, together with its search
    /// trigger when it has one.
    pub fn set_proton_service(&mut self, id: &str, enabled: bool) {
        match id {
            "proton-bridge" => self.proton_bridge_enabled = enabled,
            "proton-vpn" => self.proton_vpn_enabled = enabled,
            "proton-calendar" => self.proton_calendar_enabled = enabled,
            "proton-drive" => self.proton_drive_enabled = enabled,
            _ => return,
        }
        if id == "proton-bridge" {
            return;
        }
        if enabled {
            self.install_builtin(id);
        } else {
            self.uninstall_builtin(id);
        }
    }

    /// The Proton VPN, Calendar and Drive integrations are each both a
    /// Settings service and an optional search trigger. Keep old
    /// installations in sync while respecting an explicit trigger uninstall
    /// made from Search settings.
    fn migrate_proton_vpn_trigger(&mut self) {
        for (id, ..) in crate::trigger_defaults::PROTON_TRIGGERS {
            let has_keyword = self.command_keywords.iter().any(|keyword| keyword.id == id);
            if self.proton_service_enabled(id) {
                if crate::trigger_defaults::supports_native(id)
                    && !self.is_uninstalled(id)
                    && !has_keyword
                {
                    self.install_builtin(id);
                }
            } else if has_keyword {
                self.uninstall_builtin(id);
            }
        }
    }

    /// The converter used to live under the calculator's switch: keep whatever
    /// that switch said until the user flips the converter's own.
    fn migrate_converter_switch(&mut self) {
        self.enable_converter.get_or_insert(self.enable_calculator);
    }

    fn migrate_resource_defaults(&mut self) {
        if self.max_index_entries > 75_000 {
            self.max_index_entries = dm();
        }
        // Migrate old clipboard history limit default (100 → 1000).
        if self.clipboard_history_limit == 100 {
            self.clipboard_history_limit = 1000;
        }
        // Restore Ctrl+Enter as the Find terminal shortcut. Previous defaults
        // assigned Ctrl+Enter to the file manager and Ctrl+Shift+Enter to the
        // terminal, so swap those stored defaults as a pair.
        if self.terminal_shortcut == "<Control><Shift>Return"
            && self.open_location_shortcut == "<Control>Return"
        {
            self.terminal_shortcut = default_terminal_shortcut();
            self.open_location_shortcut = default_open_location_shortcut();
        } else if self.terminal_shortcut == "<Control><Shift>Return"
            && self.open_location_shortcut == "<Control><Shift>Return"
        {
            // Older configs can predate the separate file-manager shortcut;
            // serde gives that missing field the new Ctrl+Shift+Enter default.
            self.terminal_shortcut = default_terminal_shortcut();
        } else if self.terminal_shortcut == "<Control>Return"
            && self.open_location_shortcut == "<Control>Return"
        {
            // Also migrate configs from before the separate location shortcut
            // was introduced, where both fields can carry Ctrl+Enter.
            self.open_location_shortcut = default_open_location_shortcut();
        }
    }

    fn apply_privacy_preferences(&self) {
        let clipboard = self.command_keywords.iter().any(|kw| kw.id == "clipboard" && kw.enabled)
            && !self.is_uninstalled("clipboard");
        crate::security::set_preferences(self.save_search_history, self.allow_network_icons, clipboard);
    }

    pub fn save(&self) {
        if let Err(error) = self.save_checked() {
            log::warn!("config: {error}");
        }
    }

    /// Save and report filesystem failures so service installation can avoid
    /// claiming success when its persistent state was not written.
    pub fn save_checked(&self) -> Result<(), String> {
        self.save_to_path(&Self::config_path())
    }

    fn save_to_path(&self, path: &std::path::Path) -> Result<(), String> {
        self.apply_privacy_preferences();
        let data = serde_json::to_vec_pretty(self)
            .map_err(|_| "Cannot encode Spotty settings.".to_owned())?;
        crate::security::write_private(path, data)
            .map_err(|error| format!("Cannot save Spotty settings: {error}"))
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
        if word.is_empty() {
            return None;
        }
        self.command_keywords
            .iter()
            .find(|k| self.keyword_usable(k) && k.word.eq_ignore_ascii_case(word))
            .cloned()
            .or_else(|| crate::triggers::keyword_for_word(word))
    }

    pub fn keyword_for_id(&self, id: &str) -> Option<CommandKeyword> {
        self.command_keywords
            .iter()
            .find(|k| self.keyword_usable(k) && k.id == id)
            .cloned()
            .or_else(|| crate::triggers::keyword_for_id(id))
    }

    /// Enabled, installed, and — for a result type — switched on.
    pub fn keyword_usable(&self, k: &CommandKeyword) -> bool {
        k.enabled
            && !self.is_uninstalled(&k.id)
            && (!k.is_result() || self.result_enabled(&k.id))
    }

    /// A result type's switch.
    pub fn result_enabled(&self, id: &str) -> bool {
        self.command_keywords.iter().any(|kw| kw.id == id)
            && match id {
                "apps" => self.enable_apps,
                "newapps" => self.enable_new_apps,
                "web" => self.enable_web,
                "calc" => self.enable_calculator,
                "convert" => self.converter_enabled(),
                "updates" => self.enable_updates,
                _ => false,
            }
    }

    pub fn set_result_enabled(&mut self, id: &str, on: bool) {
        match id {
            "apps" => self.enable_apps = on,
            "newapps" => self.enable_new_apps = on,
            "web" => self.enable_web = on,
            "calc" => self.enable_calculator = on,
            "convert" => self.enable_converter = Some(on),
            "updates" => self.enable_updates = on,
            _ => {}
        }
    }

    /// The converter's switch.
    pub fn converter_enabled(&self) -> bool {
        self.enable_converter.unwrap_or(self.enable_calculator)
    }

    /// Whether `id` shows in the regular search.
    ///
    /// A result type does by default, and always when it has no trigger word —
    /// nothing else would reach it — unless the user turned it off while
    /// giving it one. A trigger is the other way round: it is reached by its
    /// word, and shows in the regular search only if it opted in.
    pub fn in_regular_search(&self, id: &str) -> bool {
        if !RESULT_IDS.contains(&id) {
            return self.regular_search_on.iter().any(|u| u == id);
        }
        let wordless = self
            .command_keywords
            .iter()
            .find(|k| k.id == id)
            .map_or(true, |k| k.word.is_empty());
        wordless || !self.regular_search_off.iter().any(|u| u == id)
    }

    pub fn set_in_regular_search(&mut self, id: &str, on: bool) {
        if RESULT_IDS.contains(&id) {
            self.regular_search_off.retain(|u| u != id);
            if !on {
                self.regular_search_off.push(id.to_string());
            }
        } else {
            self.regular_search_on.retain(|u| u != id);
            if on {
                self.regular_search_on.push(id.to_string());
            }
        }
    }

    /// Every result type and trigger, highest priority first: the saved order,
    /// then whatever it doesn't mention yet in the default order, then any
    /// installed trigger. "cmd" (the app launcher behind every app action) has
    /// no results of its own and isn't listed.
    pub fn ordered_ids(&self) -> Vec<String> {
        let installed: Vec<String> = crate::triggers::all().into_iter().map(|t| t.id).collect();
        self.ordered_ids_with(&installed)
    }

    /// [`Config::ordered_ids`] with the installed triggers given — testable
    /// without a triggers directory.
    pub fn ordered_ids_with(&self, installed: &[String]) -> Vec<String> {
        let known = |id: &str| {
            id != "cmd"
                && (RESULT_IDS.contains(&id)
                    || self.command_keywords.iter().any(|k| k.id == id)
                    || installed.iter().any(|i| i == id))
        };
        let mut out: Vec<String> = Vec::new();
        let candidates = self
            .result_order
            .iter()
            .map(String::as_str)
            .chain(DEFAULT_ORDER)
            .chain(self.command_keywords.iter().map(|k| k.id.as_str()))
            .chain(installed.iter().map(String::as_str));
        for id in candidates {
            if known(id) && !out.iter().any(|o| o == id) {
                out.push(id.to_string());
            }
        }
        out
    }

    /// Where `id` ranks (0 = first), if it is listed at all.
    pub fn order_position(&self, id: &str) -> Option<usize> {
        self.ordered_ids().iter().position(|o| o == id)
    }

    /// Move `id` to `index` in the order (clamped), saving the whole order.
    pub fn move_in_order(&mut self, id: &str, index: usize) {
        let installed: Vec<String> = crate::triggers::all().into_iter().map(|t| t.id).collect();
        self.move_in_order_with(id, index, &installed);
    }

    pub fn move_in_order_with(&mut self, id: &str, index: usize, installed: &[String]) {
        let mut ids = self.ordered_ids_with(installed);
        let Some(from) = ids.iter().position(|o| o == id) else {
            return;
        };
        let item = ids.remove(from);
        ids.insert(index.min(ids.len()), item);
        self.result_order = ids;
    }

    /// Give a result type a trigger word, or take it away. Taking it away
    /// puts the type back in the regular search: that is then its only door.
    pub fn set_result_word(&mut self, id: &str, word: &str) {
        if let Some(k) = self.command_keywords.iter_mut().find(|k| k.id == id) {
            k.word = word.to_string();
        }
        if word.is_empty() {
            self.set_in_regular_search(id, true);
        }
    }

    pub fn is_uninstalled(&self, id: &str) -> bool {
        self.uninstalled_builtins.iter().any(|u| u == id)
    }

    /// Uninstall a built-in trigger or result type: off, out of the list, its
    /// word and shortcut kept for a later reinstall. "cmd" (the app launcher
    /// behind every app action) can't be removed.
    pub fn uninstall_builtin(&mut self, id: &str) {
        if id == "cmd" || self.is_uninstalled(id) {
            return;
        }
        self.uninstalled_builtins.push(id.to_string());
        if RESULT_IDS.contains(&id) {
            self.set_result_enabled(id, false);
        } else if let Some(k) = self.command_keywords.iter_mut().find(|k| k.id == id) {
            k.enabled = false;
        }
    }

    /// Reinstall it from the Store: back in the list and switched on.
    pub fn install_builtin(&mut self, id: &str) {
        self.uninstalled_builtins.retain(|u| u != id);
        if RESULT_IDS.contains(&id) {
            self.set_result_enabled(id, true);
        }
        if let Some(k) = self.command_keywords.iter_mut().find(|k| k.id == id) {
            k.enabled = true;
        } else if let Some(k) = crate::trigger_defaults::command_keyword(id) {
            self.command_keywords.push(k);
        }
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
    fn proton_bridge_trigger_migrates_to_service_flag_without_keyword() {
        let mut config = Config::default();
        let legacy = CommandKeyword {
            id: "proton-bridge".into(), word: "proton".into(),
            description: "Legacy Bridge trigger".into(), extensions: vec![],
            icon: String::new(), all_files: false, shortcut: "<Super>p".into(), enabled: true,
        };
        config.command_keywords.push(legacy);
        config.regular_search_on.push("proton-bridge".into());
        config.regular_search_off.push("proton-bridge".into());
        config.result_order.push("proton-bridge".into());
        config.migrate_proton_bridge_service();
        config.migrate_keywords();
        assert!(config.proton_bridge_enabled);
        assert!(config.keyword_for_id("proton-bridge").is_none());
        assert!(!config.command_keywords.iter().any(|k| k.word == "proton"));
        assert!(config.regular_search_on.iter().all(|id| id != "proton-bridge"));
        assert!(config.regular_search_off.iter().all(|id| id != "proton-bridge"));
        assert!(config.result_order.iter().all(|id| id != "proton-bridge"));
        config.proton_bridge_enabled = false;
        config.migrate_proton_bridge_service();
        assert!(!config.proton_bridge_enabled, "Migration must not re-enable a disabled service");
    }

    #[test]
    fn proton_bridge_migration_respects_uninstalled_state() {
        let mut config = Config::default();
        config.command_keywords.push(CommandKeyword {
            id: "proton-bridge".into(), word: "proton".into(),
            description: String::new(), extensions: vec![], icon: String::new(),
            all_files: false, shortcut: String::new(), enabled: true,
        });
        config.uninstalled_builtins.push("proton-bridge".into());
        config.migrate_proton_bridge_service();
        assert!(!config.proton_bridge_enabled);
        assert!(config.command_keywords.iter().all(|k| k.id != "proton-bridge"));
    }

    #[test]
    fn proton_bridge_service_install_and_uninstall_preserve_keyword_absence() {
        let mut config = Config::default();
        config.proton_bridge_enabled = true;
        let installed = serde_json::to_string(&config).unwrap();
        let mut restored: Config = serde_json::from_str(&installed).unwrap();
        assert!(restored.proton_bridge_enabled);
        assert!(restored.keyword_for_id("proton-bridge").is_none());

        restored.proton_bridge_enabled = false;
        let uninstalled = serde_json::to_string(&restored).unwrap();
        let restored: Config = serde_json::from_str(&uninstalled).unwrap();
        assert!(!restored.proton_bridge_enabled);
        assert!(restored.keyword_for_id("proton-bridge").is_none());
    }

    #[test]
    fn result_types_are_store_installs_and_begin_unavailable() {
        let mut c = Config::default();
        for id in RESULT_IDS {
            assert!(!c.command_keywords.iter().any(|k| k.id == id), "{id} starts uninstalled");
            assert!(!c.result_enabled(id), "{id} is unavailable until installed");
            c.install_builtin(id);
            let kw = c.command_keywords.iter().find(|k| k.id == id).expect(id);
            assert!(kw.is_result(), "{id}");
            assert!(kw.word.is_empty() && kw.shortcut.is_empty(), "{id} starts without a trigger word");
            assert_eq!(kw.mode_key(), id);
            assert!(!kw.chip_label().is_empty(), "{id} has a chip label");
        }
        assert!(c.keyword_for_word("").is_none(), "an empty word never matches");
        assert!(c.keyword_for_id("calc").is_some());
    }

    #[test]
    fn the_regular_search_switch_only_means_something_with_a_word() {
        let mut c = Config::default();
        c.install_builtin("calc");
        // No word: the regular search is the only door, so it is always on —
        // whatever was stored.
        assert!(c.in_regular_search("calc"));
        c.regular_search_off.push("calc".into());
        assert!(c.in_regular_search("calc"), "wordless means on");
        // With a word it is the user's call.
        c.set_result_word("calc", "calc");
        assert!(!c.in_regular_search("calc"), "off while it has a word");
        c.set_in_regular_search("calc", true);
        assert!(c.in_regular_search("calc"));
        c.set_in_regular_search("calc", false);
        // Taking the word away turns it back on — and keeps it on afterwards.
        c.set_result_word("calc", "");
        assert!(c.in_regular_search("calc"));
        assert!(c.regular_search_off.is_empty(), "{:?}", c.regular_search_off);
        c.set_result_word("calc", "again");
        assert!(c.in_regular_search("calc"), "a new word starts out shown");
        // Other types are untouched.
        assert!(c.in_regular_search("web"));
    }

    #[test]
    fn a_trigger_shows_in_the_regular_search_only_when_it_opts_in() {
        let mut c = Config::default();
        c.install_builtin("web");
        for id in ["files", "clipboard", "run", "emoji", "bluetooth", "dictionary"] {
            assert!(!c.in_regular_search(id), "{id} is reached by its word");
            c.set_in_regular_search(id, true);
            assert!(c.in_regular_search(id), "{id} opted in");
        }
        c.set_in_regular_search("emoji", false);
        assert!(!c.in_regular_search("emoji"));
        // Opting a trigger in never touches a result type, and the reverse.
        assert!(c.in_regular_search("web"));
        assert!(c.regular_search_off.is_empty());
        c.set_in_regular_search("web", false);
        c.set_result_word("web", "web");
        assert!(!c.in_regular_search("web"));
        assert!(c.in_regular_search("files"), "still opted in");
        // Each setting lives in its own list.
        assert_eq!(c.regular_search_off, ["web"]);
        assert!(!c.regular_search_on.iter().any(|u| u == "web" || u == "emoji"));
    }

    #[test]
    fn the_order_lists_everything_once_and_keeps_what_the_user_set() {
        let c = Config::default();
        let installed = vec!["dictionary".to_string()];
        let ids = c.ordered_ids_with(&installed);
        // Every result type, built-in trigger and installed trigger — once.
        for id in RESULT_IDS.iter().copied().chain(["files", "clipboard", "run", "emoji", "bluetooth", "dictionary"]) {
            assert_eq!(ids.iter().filter(|i| *i == id).count(), 1, "{id}: {ids:?}");
        }
        assert!(!ids.iter().any(|i| i == "cmd"), "the app launcher isn't a result source");
        // The default: specific answers first, the web fallback last of the
        // built-ins, installed triggers after.
        assert_eq!(ids[0], "calc");
        let web = ids.iter().position(|i| i == "web").unwrap();
        assert_eq!(ids[web + 1], "dictionary");

        // Moving one keeps the rest in place.
        let mut c = c;
        c.move_in_order_with("web", 0, &installed);
        let moved = c.ordered_ids_with(&installed);
        assert_eq!(moved[0], "web");
        assert_eq!(moved[1], "calc");
        assert_eq!(moved.len(), ids.len());
        c.move_in_order_with("web", 999, &installed);
        assert_eq!(c.ordered_ids_with(&installed).last().unwrap(), "web", "clamped to the end");

        // A saved order that predates a new entry still lists it, and drops
        // ids that no longer exist.
        let mut old = Config::default();
        old.result_order = vec!["web".into(), "gone".into(), "apps".into()];
        let ids = old.ordered_ids_with(&[]);
        assert_eq!(&ids[..2], ["web", "apps"]);
        assert!(!ids.iter().any(|i| i == "gone"));
        assert!(ids.iter().any(|i| i == "convert"), "new entries are added");
    }

    #[test]
    fn the_converter_is_its_own_result_type() {
        let c = Config::default();
        assert!(RESULT_IDS.contains(&"calc") && RESULT_IDS.contains(&"convert"));
        assert!(c.converter_enabled() && c.enable_calculator);
        // Two switches, two keywords.
        let mut c = c;
        c.set_result_enabled("convert", false);
        assert!(c.enable_calculator && !c.converter_enabled());
        assert!(c.keyword_for_id("calc").is_some() && c.keyword_for_id("convert").is_none());
        c.set_result_enabled("calc", false);
        c.set_result_enabled("convert", true);
        assert!(!c.enable_calculator && c.converter_enabled());
    }

    #[test]
    fn a_config_from_before_the_split_keeps_the_converter_where_it_was() {
        // Calculator off used to mean converter off too.
        let mut off: Config = serde_json::from_str(r#"{"enable_calculator":false}"#).unwrap();
        assert!(off.enable_converter.is_none() && !off.converter_enabled());
        off.migrate_converter_switch();
        assert_eq!(off.enable_converter, Some(false));
        // …and once pinned, it no longer follows the calculator.
        off.enable_calculator = true;
        assert!(!off.converter_enabled());
        let mut on: Config = serde_json::from_str("{}").unwrap();
        on.migrate_converter_switch();
        assert_eq!(on.enable_converter, Some(true));
    }

    #[test]
    fn result_types_are_not_added_during_config_migration() {
        let mut c = Config::default();
        c.command_keywords.retain(|k| !k.is_result());
        c.migrate_keywords();
        for id in RESULT_IDS {
            assert!(!c.command_keywords.iter().any(|k| k.id == id), "{id} stays opt-in");
        }
    }

    #[test]
    fn a_result_type_follows_its_switch() {
        let mut c = Config::default();
        c.install_builtin("web");
        if let Some(k) = c.command_keywords.iter_mut().find(|k| k.id == "web") {
            k.word = "web".into();
        }
        assert!(c.keyword_for_word("web").is_some());
        c.set_result_enabled("web", false);
        assert!(!c.enable_web);
        assert!(c.keyword_for_word("web").is_none(), "switched off, no mode");
        assert!(c.keyword_for_id("web").is_none(), "nor its shortcut");
    }

    #[test]
    fn uninstalling_a_builtin_turns_it_off_and_reinstalling_brings_it_back() {
        let mut c = Config::default();
        let word = c.keyword_for_id("emoji").expect("emoji").word;
        c.uninstall_builtin("emoji");
        assert!(c.is_uninstalled("emoji"));
        assert!(c.keyword_for_word(&word).is_none());
        assert!(c.keyword_for_id("emoji").is_none(), "its shortcut goes too");
        // A second uninstall doesn't list it twice.
        c.uninstall_builtin("emoji");
        assert_eq!(c.uninstalled_builtins.iter().filter(|u| *u == "emoji").count(), 1);
        c.install_builtin("emoji");
        assert!(!c.is_uninstalled("emoji"));
        assert_eq!(c.keyword_for_id("emoji").map(|k| k.word), Some(word), "word kept");

        // A result type: uninstalling switches its results off as well.
        c.uninstall_builtin("calc");
        assert!(!c.enable_calculator);
        c.install_builtin("calc");
        assert!(c.enable_calculator);

        // The app launcher behind every app action stays.
        c.uninstall_builtin("cmd");
        assert!(!c.is_uninstalled("cmd"));
    }

    #[test]
    fn new_shortcut_defaults_and_terminal_migration() {
        // Fresh defaults: Ctrl+Enter opens the location in the terminal,
        // Ctrl+Shift+Enter opens it in the file manager, Ctrl+D deletes behind a
        // confirmation dialog.
        let cfg = Config::default();
        assert_eq!(cfg.open_location_shortcut, "<Control><Shift>Return");
        assert_eq!(cfg.delete_file_shortcut, "<Control>d");
        assert_eq!(cfg.terminal_shortcut, "<Control>Return");

        // Configs from the previous defaults swap the shortcut assignments.
        let mut cfg = Config::default();
        cfg.terminal_shortcut = "<Control><Shift>Return".into();
        cfg.open_location_shortcut = "<Control>Return".into();
        cfg.migrate_resource_defaults();
        assert_eq!(cfg.terminal_shortcut, "<Control>Return");
        assert_eq!(cfg.open_location_shortcut, "<Control><Shift>Return");

        // A config without the old location field can deserialize both fields
        // to the previous defaults; move only the terminal shortcut in that case.
        cfg.terminal_shortcut = "<Control><Shift>Return".into();
        cfg.open_location_shortcut = "<Control><Shift>Return".into();
        cfg.migrate_resource_defaults();
        assert_eq!(cfg.terminal_shortcut, "<Control>Return");
        assert_eq!(cfg.open_location_shortcut, "<Control><Shift>Return");

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
        assert_eq!(cfg.open_location_shortcut, "<Control><Shift>Return");
        assert_eq!(cfg.delete_file_shortcut, "<Control>d");
        assert_eq!(cfg.terminal_shortcut, "<Control>Return");
        assert_eq!(
            cfg.trigger_repo_url,
            "https://raw.githubusercontent.com/Aras1907/spotty-triggers/main"
        );
    }

    #[test]
    fn browser_default_leaves_the_engine_list_and_migrates() {
        // The settings combo only ever offers concrete engines…
        assert!(!SearchEngine::all().contains(&SearchEngine::BrowserDefault));
        assert_eq!(SearchEngine::all().len(), 6);
        // …a config still saved on the old factory value lands on the list's
        // first engine (same path `Config::load` takes)…
        let mut cfg: Config =
            serde_json::from_str(r#"{"search_engine": "browserdefault"}"#).unwrap();
        cfg.migrate_search_engine();
        assert_eq!(cfg.search_engine, SearchEngine::DuckDuckGo);
        // …and so does a config that never named one.
        assert_eq!(Config::default().search_engine, SearchEngine::DuckDuckGo);
    }

    #[test]
    fn legacy_package_manager_migrates_into_source_switches() {
        // Every old combo value maps onto the four independent switches;
        // AppImages are always introduced as available-on (Settings hides
        // the switch when the machine has none).
        for (legacy, fp, ds, sn) in [
            ("flatpakonly", true, false, false),
            ("both", true, true, false),
            ("distroonly", false, true, false),
            ("snaponly", false, false, true),
            ("distrosnap", false, true, true),
            ("all", true, true, true),
        ] {
            let json = format!(r#"{{"package_manager": "{legacy}"}}"#);
            let mut cfg: Config = serde_json::from_str(&json).unwrap();
            cfg.migrate_app_sources();
            assert_eq!(
                (cfg.pm_flatpak, cfg.pm_distro, cfg.pm_snap, cfg.pm_appimage),
                (Some(fp), Some(ds), Some(sn), Some(true)),
                "{legacy}"
            );
            assert_eq!(cfg.app_sources(), AppSources::new(fp, ds, sn, true));
            // Second pass is a no-op (migrations run on every load).
            let before = serde_json::to_string(&cfg).unwrap();
            cfg.migrate_app_sources();
            assert_eq!(before, serde_json::to_string(&cfg).unwrap());
        }
    }

    #[test]
    fn migrated_switches_persist_and_the_legacy_combo_does_not() {
        let mut cfg: Config = serde_json::from_str(r#"{"package_manager": "all"}"#).unwrap();
        cfg.migrate_app_sources();
        let s = serde_json::to_string(&cfg).unwrap();
        assert!(!s.contains("package_manager"), "{s}");
        assert!(s.contains(r#""pm_flatpak":true"#), "{s}");

        // A hand-edited config keeps the switches it has and only gains the
        // missing ones from the legacy value.
        let mut cfg: Config =
            serde_json::from_str(r#"{"package_manager": "all", "pm_snap": false}"#).unwrap();
        cfg.migrate_app_sources();
        assert_eq!(cfg.pm_snap, Some(false));
        assert_eq!(cfg.pm_flatpak, Some(true));
        assert_eq!(
            cfg.app_sources(),
            AppSources::new(true, true, false, true)
        );
    }
}

#[cfg(test)]
mod service_persistence_tests {
    use super::Config;
    use std::path::PathBuf;

    fn isolated_config_path() -> (PathBuf, PathBuf) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/spotty-config-tests")
            .join(format!("{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        (root.clone(), root.join("spotty/config.json"))
    }

    #[test]
    fn installed_service_state_survives_a_config_reload() {
        let (root, path) = isolated_config_path();
        let mut config = Config::default();
        config.proton_bridge_enabled = true;
        config.proton_vpn_enabled = true;
        config.install_builtin("proton-vpn");
        config.save_to_path(&path).expect("save installed services");

        let loaded = Config::load_from_path(&path);
        assert!(loaded.proton_bridge_enabled);
        assert!(loaded.proton_vpn_enabled);
        assert_eq!(loaded.keyword_for_word("vpn").unwrap().id, "proton-vpn");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn vpn_service_migration_adds_a_missing_trigger_without_resetting_preferences() {
        let mut config = Config::default();
        config.proton_vpn_enabled = true;
        config.migrate_proton_vpn_trigger();
        let keyword = config.keyword_for_id("proton-vpn").expect("VPN trigger");
        assert_eq!(keyword.word, "vpn");

        // An existing row may have been disabled independently of its
        // service. Migration must preserve that choice and custom settings.
        if let Some(keyword) = config.command_keywords.iter_mut().find(|item| item.id == "proton-vpn") {
            keyword.enabled = false;
            keyword.word = "myvpn".into();
            keyword.shortcut = "<Control><Alt>v".into();
        }
        config.migrate_proton_vpn_trigger();
        let keyword = config.command_keywords.iter().find(|item| item.id == "proton-vpn").unwrap();
        assert!(!keyword.enabled);
        assert_eq!(keyword.word, "myvpn");
        assert_eq!(keyword.shortcut, "<Control><Alt>v");

        config.uninstall_builtin("proton-vpn");
        config.migrate_proton_vpn_trigger();
        assert!(config.is_uninstalled("proton-vpn"));
        assert!(config.keyword_for_id("proton-vpn").is_none());
    }

    #[test]
    fn calendar_and_drive_services_install_their_triggers_and_survive_reload() {
        let (root, path) = isolated_config_path();
        let mut config = Config::default();
        config.set_proton_service("proton-calendar", true);
        config.set_proton_service("proton-drive", true);
        config.proton_calendar_view = "month".into();
        config.proton_drive_folder = "~/ProtonDrive".into();
        config.save_to_path(&path).expect("save installed services");

        let mut loaded = Config::load_from_path(&path);
        assert!(loaded.proton_calendar_enabled && loaded.proton_drive_enabled);
        assert_eq!(loaded.keyword_for_word("cal").unwrap().id, "proton-calendar");
        assert_eq!(loaded.keyword_for_word("drive").unwrap().id, "proton-drive");
        assert_eq!(loaded.proton_calendar_view, "month");
        assert_eq!(loaded.proton_drive_folder, "~/ProtonDrive");

        loaded.set_proton_service("proton-drive", false);
        assert!(!loaded.proton_drive_enabled);
        assert!(loaded.keyword_for_id("proton-drive").is_none());
        assert!(loaded.keyword_for_id("proton-calendar").is_some());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn service_state_save_rejects_a_symlinked_config_directory() {
        use std::os::unix::fs::symlink;

        let (root, path) = isolated_config_path();
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, path.parent().unwrap()).unwrap();

        let error = Config::default()
            .save_to_path(&path)
            .expect_err("must not follow a pre-planted config-directory symlink");
        assert!(error.starts_with("Cannot save Spotty settings:"));

        let _ = std::fs::remove_dir_all(root);
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
}

#[cfg(test)]
mod calculator_setting_tests {
    #[test]
    fn serde_defaults_for_calculator_settings() {
        let c: super::Config = serde_json::from_str("{}").expect("empty config");
        assert_eq!(c.calc_precision, 6);
        assert!(!c.calc_separators, "separators are opt-in");
        assert!(!c.calc_bases, "bases are opt-in");
        assert!(!c.calc_paste, "Enter keeps copying until turned on");
        assert!(c.calc_show_expr, "\"expr = result\" titles stay the default");
        assert!(c.calc_converter, "unit conversions are on by default");
        assert!(c.calc_equivalents, "equivalents are on by default");
        assert!(c.calc_base_convert, "base conversions are on by default");
        assert!(c.calc_currency, "currency conversion is on by default");
        assert!(c.calc_sci_notation, "scientific notation is on by default");
        assert_eq!(
            c.calc_convert_words,
            ["to".to_string(), "in".to_string()],
            "both connector words ship enabled"
        );
        assert_eq!(c.calc_default_currency, "usd", "default currency ships USD");
        assert!(
            c.calc_default_targets.is_empty(),
            "default targets start automatic"
        );
    }

    #[test]
    fn old_configs_gain_the_new_fields() {
        // A config written before the conversions feature must still
        // parse and get every new switch at its default.
        let c: super::Config =
            serde_json::from_str(r#"{"enable_calculator":false,"calc_precision":8}"#)
                .expect("old config");
        assert!(!c.enable_calculator, "existing fields survive");
        assert_eq!(c.calc_precision, 8);
        assert!(c.calc_converter && c.calc_equivalents && c.calc_base_convert);
        assert_eq!(c.calc_convert_words, ["to".to_string(), "in".to_string()]);
        assert_eq!(c.calc_default_currency, "usd");
        assert!(c.calc_currency && c.calc_sci_notation);
    }
}
