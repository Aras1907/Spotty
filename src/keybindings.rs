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
        Ok(o) => {
            if !o.status.success() {
                log::warn!(
                    "keybindings: gsettings {} failed: {}",
                    args.first().unwrap_or(&""),
                    String::from_utf8_lossy(&o.stderr).trim()
                );
            }
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        Err(e) => {
            log::warn!("keybindings: gsettings failed: {e}");
            String::new()
        }
    }
}

/// `s` as GVariant text (a quoted, escaped string literal) for `gsettings set`.
fn gvariant_string(s: &str) -> String {
    use glib::prelude::ToVariant;
    s.to_variant().print(false).to_string()
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

/// Whether this is a GNOME Shell session (not Phosh, which reports GNOME too).
fn gnome_shell_session() -> bool {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    crate::de::detect() == crate::de::Desktop::Gnome
        && !desktop.split(':').any(|d| d.eq_ignore_ascii_case("phosh"))
}

/// Whether `gtk` (e.g. "<Control>space") is one of the bindings in a
/// gsettings dump (`list-recursively`, or a single `get` value).
fn binding_listed(dump: &str, gtk: &str) -> bool {
    let norm = |s: &str| s.to_lowercase().replace("<primary>", "<control>").replace("<ctrl>", "<control>");
    norm(dump).contains(&format!("'{}'", norm(gtk)))
}

/// Schemas whose bindings GNOME Shell, Mutter or gsd grab ahead of custom
/// keybindings.
const GRAB_SCHEMAS: &[&str] = &[
    "org.gnome.desktop.wm.keybindings",
    "org.gnome.shell.keybindings",
    "org.gnome.mutter.keybindings",
    "org.gnome.mutter.wayland.keybindings",
    "org.gnome.settings-daemon.plugins.media-keys",
];

/// Whether one of GNOME's own bindings (`GRAB_SCHEMAS`) uses `gtk`.
fn grabbed_by_gnome(gtk: &str) -> bool {
    GRAB_SCHEMAS.iter().any(|&schema| binding_listed(&gsettings(&["list-recursively", schema]), gtk))
}

/// Whether a GNOME binding outside Spotty already uses `gtk`.
fn accel_taken(gtk: &str) -> bool {
    // A bare `list-recursively` can list only some schemas (GLib 2.88 lists
    // just ~/.local/share/glib-2.0/schemas once that exists), so the schemas
    // that grab keys are read one by one too. It skips relocatable schemas,
    // so custom keybindings are read one by one as well.
    binding_listed(&gsettings(&["list-recursively"]), gtk)
        || grabbed_by_gnome(gtk)
        || binding_listed(&gsettings(&["get", "org.freedesktop.ibus.general.hotkey", "triggers"]), gtk)
        || current_keybindings()
            .iter()
            .filter(|p| !p.contains("/spotty"))
            .any(|p| binding_listed(&gsettings(&["get", &format!("{ENTRY_SCHEMA}:{p}"), "binding"]), gtk))
}

/// Number of keyboard layouts / input methods in a `sources` value.
fn input_source_count(sources: &str) -> usize {
    sources.matches("('").count()
}

/// Some distros bind Ctrl+Space to switching input sources (postmarketOS
/// ships it for Chromebooks), and Mutter takes that key before the custom
/// shortcut. With at most one input source there is nothing to switch, so
/// input switching moves to GNOME's own default, Super+Space. Returns whether
/// it moved.
fn free_ctrl_space() -> bool {
    const WM: &str = "org.gnome.desktop.wm.keybindings";
    if !binding_listed(&gsettings(&["get", WM, "switch-input-source"]), "<Control>space") {
        return false;
    }
    let sources = gsettings(&["get", "org.gnome.desktop.input-sources", "sources"]);
    if input_source_count(&sources) > 1 || grabbed_by_gnome("<Super>space") {
        return false;
    }
    gsettings(&["set", WM, "switch-input-source", "['<Super>space']"]);
    if binding_listed(&gsettings(&["get", WM, "switch-input-source-backward"]), "<Shift><Control>space") {
        gsettings(&["set", WM, "switch-input-source-backward", "['<Shift><Super>space']"]);
    }
    log::info!("keybindings: input source switching moved to Super+Space to free Ctrl+Space");
    true
}

/// Mutter keeps an accelerator that a window-manager binding holds, so Alt+Space
/// is taken off GNOME's window menu; its other bindings stay.
fn free_alt_space() {
    const WM: &str = "org.gnome.desktop.wm.keybindings";
    let menu = gsettings(&["get", WM, "activate-window-menu"]);
    if !binding_listed(&menu, "<Alt>space") {
        return;
    }
    let rest: Vec<&str> = menu
        .trim_start_matches("@as ")
        .trim_matches(|c| c == '[' || c == ']')
        .split(", ")
        .filter(|b| !b.is_empty() && !binding_listed(b, "<Alt>space"))
        .collect();
    let value = if rest.is_empty() { "@as []".to_string() } else { format!("[{}]", rest.join(", ")) };
    gsettings(&["set", WM, "activate-window-menu", &value]);
}

// ── public API ─────────────────────────────────────────────────────────

/// Register all Spotty shortcuts as GNOME custom keybindings.
///
/// Reads shortcuts from config.json (main toggle + command keywords) and
/// installed trigger manifests. Each shortcut gets its own gsettings slot.
/// Old spotty slots are cleaned up first.
pub fn register_all() {
    let mut config = crate::config::Config::load();

    // Ctrl+Space is the default; when something else owns it, use Alt+Space.
    // shortcut: a running Settings window keeps its old copy of the config and
    // may save Ctrl+Space back; the next registration falls back again.
    if to_gtk_accel(&config.shortcut) == "<Control>space" {
        if gnome_shell_session() {
            free_ctrl_space();
        }
        if accel_taken("<Control>space") {
            log::info!("keybindings: Ctrl+Space is taken, using Alt+Space");
            config.shortcut = "Alt+Space".into();
            config.save();
        }
    }
    if to_gtk_accel(&config.shortcut) == "<Alt>space" {
        free_alt_space();
    }

    // Collect (keyword_id, display_name, shortcut)
    let mut shortcuts: Vec<(String, String, String)> = Vec::new();

    // Main toggle — empty keyword_id means toggle
    if !config.shortcut.is_empty() {
        shortcuts.push(("".into(), "Spotty".into(), config.shortcut.clone()));
    }

    // Built-in command keywords
    for kw in &config.command_keywords {
        // Uninstalled, paused, or a result type that is switched off: no slot.
        if kw.shortcut.is_empty() || !config.keyword_usable(kw) {
            continue;
        }
        let name = match kw.id.as_str() {
            "files" => "Spotty: Files",
            "clipboard" => "Spotty: Clipboard",
            "emoji" => "Spotty: Emoji",
            "music" => "Spotty: Music",
            "updates" => "Spotty: Updates",
            "apps" => "Spotty: Applications",
            "newapps" => "Spotty: New Apps",
            "web" => "Spotty: Web Search",
            "calc" => "Spotty: Calculator",
            "convert" => "Spotty: Converter",
            "translate" => "Spotty: Translate",
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

    // Only GNOME Shell runs GNOME's custom keybindings. Elsewhere (Plasma,
    // Phosh, Hyprland, …) bind through the GlobalShortcuts portal when the
    // desktop offers it, and drop old slots so nothing fires twice.
    if !gnome_shell_session() && crate::global_shortcuts::available() {
        unregister_all();
        glib::MainContext::default().invoke(move || crate::global_shortcuts::bind(shortcuts));
        return;
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
        // String values go in as GVariant text: a command that starts with a
        // quoted path ('/…/spotty-toggle' '--toggle') is otherwise parsed as
        // GVariant and rejected, and the old command silently stays.
        gsettings(&["set", &schema, "name", &gvariant_string(display_name)]);
        gsettings(&["set", &schema, "command", &gvariant_string(&cmd)]);
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_bindings_in_gsettings_output() {
        let dump = "org.freedesktop.ibus.general.hotkey triggers ['<Primary>space']\n\
                    org.gnome.desktop.wm.keybindings activate-window-menu ['<Alt>space']";
        assert!(binding_listed(dump, "<Control>space"));
        assert!(binding_listed(dump, "<Alt>space"));
        assert!(!binding_listed(dump, "<Super>space"));
        assert!(!binding_listed("['<Shift><Control>space']", "<Control>space"));
        assert_eq!(to_gtk_accel("Ctrl+Space"), "<Control>space");
        assert_eq!(to_gtk_accel("Alt+Space"), "<Alt>space");
    }

    #[test]
    fn counts_input_sources() {
        assert_eq!(input_source_count("@a(ss) []"), 0);
        assert_eq!(input_source_count("[('xkb', 'pt')]"), 1);
        assert_eq!(input_source_count("[('xkb', 'us'), ('ibus', 'anthy')]"), 2);
    }
}
