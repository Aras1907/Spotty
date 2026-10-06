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
    /// Background services have settings instead of a search keyword.
    #[serde(default)]
    pub category: String,
    /// Enable this shipped backend on a fresh Spotty install. This is not a
    /// Store category; services are never search defaults.
    #[serde(default, alias = "builtin")]
    pub preinstalled: bool,
    /// This entry requires a Rust backend compiled into Spotty.
    #[serde(default)]
    pub native: bool,
}

impl RepoTrigger {
    pub fn is_service(&self) -> bool {
        self.category == "service" || matches!(self.id.as_str(), "proton-bridge" | "proton-vpn")
    }
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
        write_registry(loaded);
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match parse_manifest(&path) {
            // Preserve legacy files, but never register either Proton service
            // as a keyword or shortcut.
            Ok(m) if matches!(m.id.as_str(), "proton-bridge" | "proton-vpn") => {},
            Ok(m) if validate_structure(&m).is_ok()
                && path.file_stem().and_then(|s| s.to_str()) == Some(m.id.as_str())
                && !loaded.iter().any(|other: &TriggerManifest|
                    other.id == m.id || other.word.eq_ignore_ascii_case(&m.word)) => loaded.push(m),
            Ok(_) => log::warn!("triggers: skipping invalid or duplicate manifest {}", path.display()),
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
    install_manifest(parse_manifest(path)?)
}

/// Install the exact manifest that was shown in the confirmation dialog.
pub fn install_manifest(m: TriggerManifest) -> Result<TriggerManifest, String> {
    validate(&m)?;
    let dir = triggers_dir();
    crate::security::private_dir(&dir).map_err(|e| format!("cannot create triggers dir: {e}"))?;
    let dest = dir.join(format!("{}.json", m.id));
    // Remember the original word so the user can reset an edited one.
    let mut m = m;
    if m.default_word.is_empty() {
        m.default_word = m.word.clone();
    }
    let raw = serde_json::to_string_pretty(&m).map_err(|e| format!("serialize: {e}"))?;
    crate::security::write_private(&dest, raw).map_err(|e| format!("cannot write trigger: {e}"))?;
    load_all();
    Ok(m)
}

/// Remove an installed trigger (deletes its manifest file) and reload.
pub fn uninstall(id: &str) -> Result<(), String> {
    validate_id(id)?;
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
    validate_id(id)?;
    let path = triggers_dir().join(format!("{id}.json"));
    let mut m = parse_manifest(&path)?;
    m.word = word.trim().to_string();
    m.shortcut = shortcut.to_string();
    m.enabled = enabled;
    validate_structure(&m)?;
    if m.id != id || all().iter().any(|other|
        other.id != id && other.word.eq_ignore_ascii_case(&m.word)) {
        return Err("Mismatched id or trigger word already in use".into());
    }
    let raw = serde_json::to_string_pretty(&m).map_err(|e| format!("serialize: {e}"))?;
    crate::security::write_private(&path, raw).map_err(|e| format!("write: {e}"))?;
    load_all();
    Ok(())
}

fn default_true() -> bool {
    true
}

pub fn parse_manifest(path: &Path) -> Result<TriggerManifest, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path).map_err(|e| format!("read: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Manifest must be a regular file".into());
    }
    let mut raw = String::new();
    file.take(1_048_577).read_to_string(&mut raw).map_err(|e| format!("read: {e}"))?;
    if raw.len() > 1_048_576 { return Err("Manifest exceeds 1 MiB".into()); }
    serde_json::from_str(&raw).map_err(|e| format!("invalid JSON: {e}"))
}

/// Structural checks that keep a bad manifest from wedging the app:
/// id charset (must be a legal GTK action name), non-empty word, known
/// action, no word collision with built-ins or other triggers.
fn validate_id(id: &str) -> Result<(), String> {
    if crate::security::valid_id(id) { Ok(()) }
    else { Err("Invalid trigger id".into()) }
}

fn validate_structure(m: &TriggerManifest) -> Result<(), String> {
    validate_id(&m.id)?;
    if matches!(m.action, TriggerAction::Native) {
        return Err("Native manifests must be installed through Spotty Settings.".into());
    }
    if m.word.is_empty() || m.word.len() > 128
        || m.word.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Trigger word must be one nonempty word (at most 128 bytes)".into());
    }
    if crate::trigger_defaults::supports_native(&m.id) {
        return Err(gettext("id '{id}' is a built-in trigger").replace("{id}", &m.id));
    }
    if let TriggerAction::Web { url } = &m.action {
        crate::security::http_uri(&url.replace("{query}", "spotty-query"))?;
    }
    if !m.help_image.is_empty() { crate::security::http_uri(&m.help_image)?; }
    if let TriggerAction::Shell { command } = &m.action {
        if command.trim().is_empty() || command.contains('\0') {
            return Err("Shell action needs a command without NUL bytes".into());
        }
        // Quoting belongs to the launcher. Wrapping {query} in quotes can
        // undo shell_escape and expose metacharacters to the shell.
        validate_shell_template(command)?;
    }
    Ok(())
}

/// {query} may only appear as an unquoted shell word. Templates remain
/// arbitrary code, but a query must never become code inside that template.
fn validate_shell_template(command: &str) -> Result<(), String> {
    let mut quote = None;
    let mut escaped = false;
    let mut chars = command.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if command[index..].starts_with("{query}") {
            // Shell grammar is complex (backticks, substitutions, heredocs).
            // Reject those contexts rather than guessing how they quote data.
            if quote.is_some() || escaped || command.contains('`')
                || command.contains("$(") || command.contains("<<") {
                return Err("Use {query} unquoted, outside shell substitutions and heredocs".into());
            }
        }
        if escaped { escaped = false; continue; }
        match ch {
            '\\' if quote != Some('\'') => escaped = true,
            '\'' | '"' if quote == Some(ch) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(ch),
            _ => {},
        }
    }
    Ok(())
}

pub fn validate(m: &TriggerManifest) -> Result<(), String> {
    validate_structure(m)?;
    if by_id(&m.id).is_some() {
        return Err(gettext("trigger '{id}' is already installed").replace("{id}", &m.id));
    }
    if keyword_for_word(&m.word).is_some()
        || crate::config::Config::load().command_keywords.iter()
            .any(|kw| kw.word.eq_ignore_ascii_case(&m.word)) {
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

/// Fetch a bounded HTTP(S) response, with TLS required for remote services.
pub fn fetch_text(url: &str) -> Result<String, String> {
    fetch(url, 2_097_152, 4, false)
}

/// Store downloads fail on HTTP errors and are limited to 1 MiB.
pub fn fetch_json_text(url: &str) -> Result<String, String> {
    fetch(url, 1_048_576, 8, true)
}

fn fetch(url: &str, limit: usize, seconds: u64, fail_http: bool) -> Result<String, String> {
    let out = crate::security::curl_request(url, None, limit, seconds, fail_http)
        .map_err(|e| e.to_string())?;
    if !out.status.success() { return Err(format!("curl exited with {}", out.status)); }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() { return Err(gettext("empty response")); }
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
