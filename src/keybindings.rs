//! GNOME custom-keybinding registration via gsettings.
//!
//! Replaces the GNOME Shell extension approach. Custom keybindings are
//! registered immediately via `gsettings` and visible in GNOME Settings >
//! Keyboard > Custom Shortcuts. Works on Wayland without a shell extension
//! or shell restart.
use std::process::Command;

const MEDIA_KEYS_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const ENTRY_SCHEMA: &str =
    "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const PATH_BASE: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings";

// ── helpers ────────────────────────────────────────────────────────────

/// Convert human-readable accelerator "Super+Ctrl+F" to GTK format
/// "<Super><Control>f".
fn to_gtk_accel(human: &str) -> String {
    let mut mods = String::new();
    let mut key = String::new();
    for part in human.split('+') {
        let lower = part.trim().to_lowercase();
        match lower.as_str() {
            "super" => mods.push_str("<Super>"),
            "ctrl" | "control" => mods.push_str("<Control>"),
            "alt" | "meta" => mods.push_str("<Alt>"),
            "shift" => mods.push_str("<Shift>"),
            _ => key = lower,
        }
    }
    format!("{}{}", mods, key)
}

fn is_flatpak() -> bool {
    std::env::var("FLATPAK_ID")
        .ok()
        .as_deref()
        == Some("com.spotty.Spotty")
}

fn gsettings(args: &[&str]) -> String {
    let output = if is_flatpak() {
        Command::new("flatpak-spawn")
            .args(["--host", "gsettings"])
            .args(args)
            .output()
    } else {
        Command::new("gsettings").args(args).output()
    };
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(e) => {
            log::warn!("keybindings: gsettings failed: {e}");
            String::new()
        }
    }
}

fn slot_path(slot: usize) -> String {
    format!("{PATH_BASE}/spotty{slot}/")
}

fn slot_schema(slot: usize) -> String {
    format!("{ENTRY_SCHEMA}:{PATH_BASE}/spotty{slot}/")
}

/// Read the current custom-keybindings array.
fn current_keybindings() -> Vec<String> {
    let raw = gsettings(&["get", MEDIA_KEYS_SCHEMA, "custom-keybindings"]);
    // Output: ['/path1/', '/path2/'] or empty
    raw.trim_start_matches("@as ")
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(", ")
        .map(|s| s.trim().trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Build the GVariant array string for gsettings.
fn array_value(paths: &[String]) -> String {
    format!(
        "[{}]",
        paths
            .iter()
            .map(|p| format!("'{}'", p))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

// ── public API ─────────────────────────────────────────────────────────

/// Register all Spotty shortcuts as GNOME custom keybindings.
///
/// Reads shortcuts from config.json (main toggle + command keywords) and
/// installed trigger manifests. Each shortcut gets its own gsettings slot.
/// Old spotty slots are cleaned up first.
pub fn register_all() {
    let config = crate::config::Config::load();

    // Collect (keyword_id, display_name, shortcut)
    let mut shortcuts: Vec<(String, String, String)> = Vec::new();

    // Main toggle — empty keyword_id means toggle
    if !config.shortcut.is_empty() {
        shortcuts.push(("".into(), "Spotty".into(), config.shortcut.clone()));
    }

    // Built-in command keywords
    for kw in &config.command_keywords {
        if kw.shortcut.is_empty() || !kw.enabled {
            continue;
        }
        let name = match kw.id.as_str() {
            "files" => "Spotty: Files",
            "clipboard" => "Spotty: Clipboard",
            "cmd" => "Spotty: Apps",
            "run" => "Spotty: Run",
            "emoji" => "Spotty: Emoji",
            "music" => "Spotty: Music",
            _ => "Spotty",
        };
        shortcuts.push((kw.id.clone(), name.into(), kw.shortcut.clone()));
    }

    // Installed trigger manifests
    for trigger in crate::triggers::keywords() {
        if trigger.shortcut.is_empty() || !trigger.enabled {
            continue;
        }
        if shortcuts.iter().any(|(id, _, _)| *id == trigger.id) {
            continue;
        }
        shortcuts.push((
            trigger.id.clone(),
            format!("Spotty: {}", trigger.id),
            trigger.shortcut.clone(),
        ));
    }

    // Read existing keybindings, remove old spotty slots
    let non_spotty: Vec<String> = current_keybindings()
        .into_iter()
        .filter(|p| !p.contains("/spotty"))
        .collect();

    // Build new array: existing non-spotty + new spotty slots
    let mut all_paths = non_spotty;
    for i in 0..shortcuts.len() {
        all_paths.push(slot_path(i));
    }

    // Set the array
    gsettings(&[
        "set",
        MEDIA_KEYS_SCHEMA,
        "custom-keybindings",
        &array_value(&all_paths),
    ]);

    // Register each shortcut
    for (i, (keyword_id, display_name, shortcut)) in shortcuts.iter().enumerate() {
        let schema = slot_schema(i);
        let gtk = to_gtk_accel(shortcut);
        let cmd = crate::app::host_signal_command(keyword_id);
        gsettings(&["set", &schema, "name", display_name]);
        gsettings(&["set", &schema, "command", &cmd]);
        gsettings(&["set", &schema, "binding", &gtk]);
        gsettings(&["set", &schema, "enable-in-lockscreen", "false"]);
    }

    log::info!("keybindings: registered {} shortcuts", shortcuts.len());
}

/// Unregister all Spotty keybindings (cleanup).
pub fn unregister_all() {
    let non_spotty: Vec<String> = current_keybindings()
        .into_iter()
        .filter(|p| !p.contains("/spotty"))
        .collect();
    gsettings(&[
        "set",
        MEDIA_KEYS_SCHEMA,
        "custom-keybindings",
        &array_value(&non_spotty),
    ]);
    log::info!("keybindings: unregistered all spotty shortcuts");
}

/// Remove the GNOME Shell extension that was previously used for shortcuts.
/// Safe to call even if the extension is not installed.
pub fn uninstall_old_extension() {
    let ext_dir = dirs::data_dir()
        .unwrap_or_default()
        .join("gnome-shell/extensions/spotty@spotty");
    if !ext_dir.exists() {
        return;
    }
    // Disable first, then remove
    let _ = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.gnome.Shell.Extensions",
            "--object-path",
            "/org/gnome/Shell/Extensions",
            "--method",
            "org.gnome.Shell.Extensions.DisableExtension",
            "spotty@spotty",
        ])
        .output();
    if let Err(e) = std::fs::remove_dir_all(&ext_dir) {
        log::warn!(
            "keybindings: failed to remove old extension dir: {}",
            e
        );
    } else {
        log::info!("keybindings: removed old GNOME Shell extension");
    }
}
