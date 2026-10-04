//! Trigger registry: installable trigger keywords loaded from
//! `~/.config/spotty/triggers/*.json`.
//!
//! A trigger is a JSON manifest describing a trigger keyword plus an action
//! (web URL template, file-type filter, or a shell command). Installed
//! triggers are plain files in the triggers dir — uninstall is a file delete,
//! and `migrate_keywords` in config.rs can never prune them because they
//! never enter `config.json`.
//!
//! The registry is a process-global `RwLock<Vec<TriggerManifest>>` — not a
//! thread-local, because keybinding sync runs on a spawned thread and reads
//! installed triggers to register their shortcut slots. All other access is
//! on the GTK main thread.
//!
//! Action security: `{query}` in a shell command template is substituted
//! single-quote-escaped, so user input can never break out of the template
//! into additional shell commands — the template itself is author-controlled.
//! Shell triggers additionally require an explicit confirmation before the
//! install runs.
use crate::config::CommandKeyword;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};
use crate::i18n::gettext;

/// Public home of the installable trigger manifests — the Settings Trigger
/// page and the About dialog link here.
pub const REPO_URL: &str = "https://github.com/Aras1907/spotty-triggers";

/// One entry of the repository's `index.json` listing: every manifest field
/// except `action`, which arrives with the full manifest fetch at install
/// time. This is what the Trigger Store (Settings → Trigger → Store) shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoTrigger {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub word: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub shortcut: String,
    /// This entry enables a backend shipped in Spotty rather than installing code.
    #[serde(default)]
    pub builtin: bool,
    /// This entry requires a Rust backend compiled into Spotty.
    #[serde(default)]
    pub native: bool,
}

/// What a trigger does with the user's typed query.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TriggerAction {
    /// Enable an existing native backend through Config, never the registry.
    #[serde(alias = "builtin")]
    Native,
    /// Open a URL with `{query}` substituted (URL-encoded).
    Web { url: String },
    /// Search files filtered by the given extensions. An empty list means
    /// all files (behaves like the built-in "find" trigger).
    Files {
        #[serde(default)]
        extensions: Vec<String>,
    },
    /// Run a shell command with `{query}` substituted (auto single-quote
    /// escaped — see module docs). Output streams in the search window's
    /// progress row.
    Shell { command: String },
}

/// An installable trigger trigger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerManifest {
    pub id: String,
    pub name: String,
    pub word: String,
    pub description: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub shortcut: String,
    /// The word the trigger was installed with; kept so the user can reset
    /// an edited word back to its original.
    #[serde(default)]
    pub default_word: String,
    /// If false, the trigger is disabled: no dispatch, no shortcut slot.
    /// Toggled from the Trigger settings; uninstall is a file delete.
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    /// Free-form usage instructions shown in the preview panel. If empty,
    /// instructions are generated from the action type.
    #[serde(default)]
    pub help: String,
    /// Optional URL of a screenshot/video showing how to use the trigger.
    /// Fetched to a cache file and rendered by the preview panel.
    #[serde(default)]
    pub help_image: String,
    pub action: TriggerAction,
}

/// Usage instructions for the preview panel: the manifest's `help` text, or
/// instructions generated from the action type when none is provided.
pub fn help_text(m: &TriggerManifest) -> String {
    if !m.help.trim().is_empty() {
        return m.help.clone();
    }
    match &m.action {
        TriggerAction::Native => m.description.clone(),
        TriggerAction::Web { url } => format!(
            "Trigger: {}\n\nType \"{} <query>\" in the search bar to search {}.\n\nOpens: {}",
            m.word, m.word, m.name, url
        ),
        TriggerAction::Shell { command } => format!(
            "Trigger: {}\n\nRuns: {}\n\nWhat you type after the trigger replaces {{query}}.",
            m.word, command
        ),
        TriggerAction::Files { extensions } => {
            if extensions.is_empty() {
                format!(
                    "Trigger: {}\n\nSearches all files by name or content.",
                    m.word
                )
            } else {
                format!(
                    "Trigger: {}\n\nSearches files with extensions: {}.",
                    m.word,
                    extensions.join(", ")
                )
            }
        }
    }
}

static REGISTRY: LazyLock<RwLock<Vec<TriggerManifest>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

fn read_registry() -> Vec<TriggerManifest> {
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn write_registry(v: Vec<TriggerManifest>) {
    *REGISTRY.write().unwrap_or_else(|e| e.into_inner()) = v;
}

/// Directory where installed trigger manifests live.
pub fn triggers_dir() -> PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    p.push("spotty/triggers");
    p
}

/// (Re)scan the triggers dir and rebuild the registry. Invalid manifests are
/// skipped with a warning — one broken file must not take down the market.
pub fn load_all() {
    let mut loaded = Vec::new();
    let dir = triggers_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match parse_manifest(&path) {
            Ok(m) => loaded.push(m),
            Err(e) => log::warn!("triggers: skipping {}: {}", path.display(), e),
        }
    }
    write_registry(loaded);
    log::info!("triggers: loaded {} installed triggers", count());
}
pub fn all() -> Vec<TriggerManifest> {
    read_registry()
}

pub fn count() -> usize {
    read_registry().len()
}

pub fn by_id(id: &str) -> Option<TriggerManifest> {
    read_registry().into_iter().find(|a| a.id == id)
}

/// All installed triggers as trigger keywords (for dispatch, suggestions,
/// actions and keybinding registration). Disabled triggers are excluded —
/// everything that routes by keyword must agree on the enabled set.
pub fn keywords() -> Vec<CommandKeyword> {
    all()
        .iter()
        .filter(|a| a.enabled)
        .map(to_keyword)
        .collect()
}

/// Convert a trigger to the keyword the rest of the app understands.
pub fn to_keyword(a: &TriggerManifest) -> CommandKeyword {
    let (extensions, all_files) = match &a.action {
        TriggerAction::Files { extensions } => (extensions.clone(), extensions.is_empty()),
        _ => (Vec::new(), false),
    };
    CommandKeyword {
        id: a.id.clone(),
        word: a.word.clone(),
        description: a.description.clone(),
        extensions,
        icon: a.icon.clone(),
        all_files,
        shortcut: a.shortcut.clone(),
        enabled: a.enabled,
    }
}

pub fn keyword_for_word(word: &str) -> Option<CommandKeyword> {
    all()
        .into_iter()
        .filter(|a| a.enabled)
        .find(|a| a.word.eq_ignore_ascii_case(word))
        .map(|a| to_keyword(&a))
}

pub fn keyword_for_id(id: &str) -> Option<CommandKeyword> {
    by_id(id)
        .filter(|a| a.enabled)
        .map(|a| to_keyword(&a))
}

/// Validate + copy a manifest file into the triggers dir, then reload the
/// registry. Returns the installed manifest. `path` is the source file
/// (anywhere — a file the user downloaded from the triggers repository).
pub fn install_from_file(path: &Path) -> Result<TriggerManifest, String> {
    let m = parse_manifest(path)?;
    validate(&m)?;
    let dir = triggers_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create triggers dir: {e}"))?;
    let dest = dir.join(format!("{}.json", m.id));
    // Remember the original word so the user can reset an edited one.
    let mut m = m;
    if m.default_word.is_empty() {
        m.default_word = m.word.clone();
    }
    let raw = serde_json::to_string_pretty(&m).map_err(|e| format!("serialize: {e}"))?;
    fs::write(&dest, raw).map_err(|e| format!("cannot write trigger: {e}"))?;
    load_all();
    Ok(m)
}

/// Remove an installed trigger (deletes its manifest file) and reload.
pub fn uninstall(id: &str) -> Result<(), String> {
    let path = triggers_dir().join(format!("{id}.json"));
    if !path.exists() {
        return Err(gettext("trigger '{id}' is not installed").replace("{id}", id));
    }
    fs::remove_file(&path).map_err(|e| format!("cannot remove trigger: {e}"))?;
    // Delete everything else the trigger owns — its cached help image —
    // so an uninstall leaves no trace behind.
    let _ = fs::remove_file(triggers_dir().join("cache").join(format!("{id}.img")));
    load_all();
    Ok(())
}

/// Persist user-edited fields (word, shortcut, enabled) back into an
/// installed trigger's manifest.
pub fn update_manifest(id: &str, word: &str, shortcut: &str, enabled: bool) -> Result<(), String> {
    let path = triggers_dir().join(format!("{id}.json"));
    let mut m = parse_manifest(&path)?;
    m.word = word.trim().to_string();
    m.shortcut = shortcut.to_string();
    m.enabled = enabled;
    let raw = serde_json::to_string_pretty(&m).map_err(|e| format!("serialize: {e}"))?;
    fs::write(&path, raw).map_err(|e| format!("write: {e}"))?;
    load_all();
    Ok(())
}

fn default_true() -> bool {
    true
}

pub fn parse_manifest(path: &Path) -> Result<TriggerManifest, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("invalid JSON: {e}"))
}

/// Structural checks that keep a bad manifest from wedging the app:
/// id charset (must be a legal GTK action name), non-empty word, known
/// action, no word collision with built-ins or other triggers.
pub fn validate(m: &TriggerManifest) -> Result<(), String> {
    if matches!(m.action, TriggerAction::Native) {
        return Err("Native manifests must be installed through Spotty Settings.".into());
    }
    if m.id.is_empty()
        || !m
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(gettext("invalid id '{id}': use only letters, digits, '_', '-' or '.'").replace(
            "{id}",
            &m.id,
        ));
    }
    if m.word.trim().is_empty() {
        return Err(gettext("missing 'word' (the trigger text)"));
    }
    if crate::trigger_defaults::supports_native(&m.id) {
        return Err(gettext("id '{id}' is a built-in trigger").replace("{id}", &m.id));
    }
    if by_id(&m.id).is_some() {
        return Err(gettext("trigger '{id}' is already installed").replace("{id}", &m.id));
    }
    if keyword_for_word(&m.word).is_some() {
        return Err(gettext("trigger word '{word}' is already in use").replace("{word}", &m.word));
    }
    match &m.action {
        TriggerAction::Web { url } if url.trim().is_empty() => {
            return Err(gettext("web action needs a 'url' template"));
        }
        TriggerAction::Shell { command } if command.trim().is_empty() => {
            return Err(gettext("shell action needs a 'command' template"));
        }
        _ => {}
    }
    Ok(())
}

/// Fetch a URL via curl (flatpak-spawn aware on the host). Works with
/// http(s) and file:// URLs — used for dictionary lookups and trigger help
/// images.
pub fn fetch_text(url: &str) -> Result<String, String> {
    // Redirects capped and responses size-capped — the payloads here
    // (dictionary APIs) are small; the lenient `-s` (no `-f`) stays, some
    // callers read 404 bodies deliberately.
    let out = crate::app::run_host_shell_command(&format!(
        "curl -sL --max-time 4 --max-redirs 5 --max-filesize 2097152 '{}'",
        url.replace('\'', "'\\''")
    ))
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(gettext("curl exited with {status}").replace("{status}", &out.status.to_string()));
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.trim().is_empty() {
        return Err(gettext("empty response"));
    }
    Ok(text)
}

/// Fetch a URL as JSON: like [`fetch_text`], but `-f` makes HTTP errors
/// (404, 500, …) fail loudly instead of handing the error page back as if it
/// were the payload. Used by the Trigger Store, where a missing index or
/// manifest must read as "couldn't load", not as an unparseable response.
/// 8s budget — a cold raw-GitHub CDN can take a couple of seconds.
pub fn fetch_json_text(url: &str) -> Result<String, String> {
    let out = crate::app::run_host_shell_command(&format!(
        "curl -fsSL --max-time 8 --max-redirs 5 --max-filesize 1048576 '{}'",
        url.replace('\'', "'\\''")
    ))
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return Err(if err.is_empty() {
            format!("curl exited with {}", out.status)
        } else {
            err.to_string()
        });
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return Err(gettext("empty response"));
    }
    Ok(text)
}

/// Single-quote-escape a string so it can be safely interpolated into a
/// shell command: `'` → `'\''` and the whole thing wrapped in quotes. This
/// is the trust boundary for `{query}` in shell triggers — user input can
/// never inject additional commands through the template.
pub fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, word: &str, action: TriggerAction) -> TriggerManifest {
        TriggerManifest {
            id: id.into(),
            name: id.into(),
            word: word.into(),
            description: String::new(),
            icon: String::new(),
            shortcut: String::new(),
            default_word: word.into(),
            enabled: true,
            version: String::new(),
            author: String::new(),
            help: String::new(),
            help_image: String::new(),
            action,
        }
    }

    #[test]
    fn repo_index_entry_parses_everything_spotty_shows() {
        // The exact shape of the repository's index.json — the browse list
        // renders word, description and icon straight from these fields.
        let raw = r#"[{
            "id": "dictionary",
            "name": "Dictionary",
            "word": "dict",
            "description": "Look up a word definition",
            "icon": "accessories-dictionary-symbolic",
            "version": "1.0.0",
            "author": "spotty",
            "shortcut": ""
        }]"#;
        let list: Vec<RepoTrigger> = serde_json::from_str(raw).expect("index.json shape");
        assert_eq!(list.len(), 1);
        let t = &list[0];
        assert_eq!(t.id, "dictionary");
        assert_eq!(t.word, "dict");
        assert_eq!(t.description, "Look up a word definition");
        assert_eq!(t.icon, "accessories-dictionary-symbolic");

        // Fields the repository omits still deserialize with defaults.
        let minimal: Vec<RepoTrigger> =
            serde_json::from_str(r#"[{"id":"x","name":"X"}]"#).expect("minimal shape");
        assert!(minimal[0].word.is_empty());
        assert!(minimal[0].icon.is_empty());
    }

    #[test]
    fn validate_rejects_bad_ids_and_collisions() {
        let ok = manifest("yt", "y", TriggerAction::Web { url: "https://x/{query}".into() });
        assert!(validate(&ok).is_ok());
        assert!(validate(&manifest("bad id!", "y", TriggerAction::Web { url: "https://x".into() })).is_err());
        assert!(validate(&manifest("files", "y", TriggerAction::Web { url: "https://x".into() })).is_err());
        assert!(validate(&manifest("yt", "", TriggerAction::Web { url: "https://x".into() })).is_err());
        assert!(validate(&manifest("web1", "y", TriggerAction::Web { url: "".into() })).is_err());
        assert!(validate(&manifest("sh1", "y", TriggerAction::Shell { command: "".into() })).is_err());
    }

    #[test]
    fn shell_escape_quotes_everything() {
        assert_eq!(shell_escape("hello"), "'hello'");
        assert_eq!(shell_escape("a'b"), "'a'\\''b'");
        assert_eq!(shell_escape("; rm -rf /"), "'; rm -rf /'");
    }
}
