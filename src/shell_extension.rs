//! GNOME Shell extension installer and enabler.
//!
//! Installs `extension/extension.js` + `metadata.json` into
//! `~/.local/share/gnome-shell/extensions/spotty@spotty/`, writes the
//! binary-launch command to `~/.config/spotty/spotty_bin` (the extension
//! reads it back), and enables the extension via
//! `org.gnome.Shell.Extensions` D-Bus. `sync()` is called after any
//! shortcut/keyword mutation so the extension re-reads config and re-grabs.
use std::fs;
use std::path::PathBuf;

const EXTENSION_UUID: &str = "spotty@spotty";

fn extensions_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("gnome-shell/extensions")
}

fn extension_dir() -> PathBuf {
    extensions_dir().join(EXTENSION_UUID)
}

/// Embed the extension source at compile time.
const EXTENSION_JS: &str = include_str!("../extension/extension.js");
const METADATA_JSON: &str = include_str!("../extension/metadata.json");

/// Write extension files to disk. Returns true if anything changed.
fn write_extension_files() -> bool {
    let dir = extension_dir();
    let _ = fs::create_dir_all(&dir);

    let js_path = dir.join("extension.js");
    let meta_path = dir.join("metadata.json");

    let js_changed = fs::read_to_string(&js_path)
        .map(|c| c != EXTENSION_JS)
        .unwrap_or(true);
    let meta_changed = fs::read_to_string(&meta_path)
        .map(|c| c != METADATA_JSON)
        .unwrap_or(true);

    if js_changed {
        let _ = fs::write(&js_path, EXTENSION_JS);
    }
    if meta_changed {
        let _ = fs::write(&meta_path, METADATA_JSON);
    }

    js_changed || meta_changed
}

/// Write the command the extension should run to summon Spotty
/// (absolute binary path, or `flatpak run --user …` when sandboxed).
fn write_bin_file() {
    let base = crate::app::spotty_base_command();
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    p.push("spotty");
    if let Some(parent) = p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(p.join("spotty_bin"), base);
}

fn gdbus_call(method: &str) -> bool {
    let output = std::process::Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.gnome.Shell.Extensions",
            "--object-path",
            "/org/gnome/Shell/Extensions",
            "--method",
            &format!("org.gnome.Shell.Extensions.{method}"),
            EXTENSION_UUID,
        ])
        .output();
    match output {
        Ok(o) => {
            if !o.status.success() {
                log::warn!(
                    "shell_extension: {method} failed: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
                false
            } else {
                log::info!("shell_extension: {method} ok");
                true
            }
        }
        Err(e) => {
            log::warn!("shell_extension: gdbus call failed: {e}");
            false
        }
    }
}

/// Full install: write files + bin file + enable. Call from on_startup.
pub fn install() {
    write_extension_files();
    write_bin_file();
    if !gdbus_call("EnableExtension") {
        log::warn!("shell_extension: enable failed (Shell not running?)");
    }
}

/// Re-sync after shortcut/keyword mutations: the extension re-reads config
/// and re-grabs accelerators on reload/enable.
pub fn sync() {
    write_extension_files();
    write_bin_file();
    if !gdbus_call("ReloadExtension") {
        gdbus_call("EnableExtension");
    }
}
