//! The optional "Spotty Focus" GNOME Shell extension. GNOME lets only Shell
//! code raise another app's window, so picking an app that is already open
//! asks this extension to focus it. The user installs and removes it in
//! Settings → Apps; without it Spotty just launches the app.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::PathBuf;

const UUID: &str = "spotty-focus@aras1907.github.io";
const METADATA: &str = include_str!("../data/gnome-shell-extension/metadata.json");
const EXTENSION_JS: &str = include_str!("../data/gnome-shell-extension/extension.js");

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    NotInstalled,
    /// Loaded and running.
    Active,
    /// On disk, but GNOME Shell only loads new extensions at login.
    NeedsRelogin,
    /// Switched off in the Extensions app (or user extensions are off).
    Off,
    /// Shell couldn't load it (error, or a GNOME version it doesn't list).
    Failed,
}

// shortcut: ignores a custom host XDG_DATA_HOME; read it from the host env if anyone sets one.
fn dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".local/share/gnome-shell/extensions").join(UUID))
}

fn shell_extensions_call(method: &str, args: glib::Variant) -> Option<glib::Variant> {
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    conn.call_sync(
        Some("org.gnome.Shell"),
        "/org/gnome/Shell",
        "org.gnome.Shell.Extensions",
        method,
        Some(&args),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        2000,
        gio::Cancellable::NONE,
    )
    .ok()
}

/// Current state. Blocking (disk + D-Bus): call it off the main thread.
pub fn status() -> Status {
    let Some(d) = dir() else { return Status::NotInstalled };
    let js = d.join("extension.js");
    let Ok(installed) = std::fs::read_to_string(&js) else { return Status::NotInstalled };
    if installed != EXTENSION_JS {
        // A newer Spotty ships a newer extension; the Shell loads it at next login.
        let _ = write_files(&d);
    }
    let state = shell_extensions_call("GetExtensionInfo", (UUID,).to_variant())
        .and_then(|v| glib::VariantDict::new(Some(&v.child_value(0))).lookup_value("state", None))
        .and_then(|s| s.get::<f64>());
    match state.map(|s| s as i32) {
        Some(1) | Some(8) => Status::Active,
        Some(2) | Some(7) => Status::Off,
        Some(3) | Some(4) => Status::Failed,
        _ => Status::NeedsRelogin,
    }
}

fn write_files(d: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(d)?;
    std::fs::write(d.join("metadata.json"), METADATA)?;
    std::fs::write(d.join("extension.js"), EXTENSION_JS)
}

/// Add or drop the uuid in org.gnome.shell enabled-extensions. The Shell
/// turns a loaded extension on or off as soon as the key changes.
fn set_enabled(on: bool) -> Result<(), String> {
    let out = crate::app::host_process("gsettings")
        .args(["get", "org.gnome.shell", "enabled-extensions"])
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut list: Vec<String> = glib::Variant::parse(Some(glib::VariantTy::STRING_ARRAY), text.trim())
        .ok()
        .and_then(|v| v.get())
        .ok_or_else(|| format!("unreadable enabled-extensions: {}", text.trim()))?;
    list.retain(|u| u != UUID);
    if on {
        list.push(UUID.to_string());
    }
    // GVariant text, never a bare string (see AGENTS.md).
    let value = list.to_variant().print(true);
    let ok = crate::app::host_process("gsettings")
        .args(["set", "org.gnome.shell", "enabled-extensions", value.as_str()])
        .status()
        .map_err(|e| e.to_string())?
        .success();
    ok.then_some(()).ok_or_else(|| "gsettings set failed".to_string())
}

/// Blocking: call it off the main thread.
pub fn install() -> Result<(), String> {
    let d = dir().ok_or("no home directory")?;
    write_files(&d).map_err(|e| e.to_string())?;
    set_enabled(true)
}

/// Blocking: call it off the main thread.
pub fn remove() -> Result<(), String> {
    set_enabled(false)?;
    // Unloads it now if the Shell has it loaded; false (ignored) otherwise.
    let _ = shell_extensions_call("UninstallExtension", (UUID,).to_variant());
    match dir().map(std::fs::remove_dir_all) {
        Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

/// Ask the extension to raise the running app `desktop_id` (e.g.
/// `org.gnome.Nautilus.desktop`). `done(true)` if a window was focused;
/// `done(false)` if the app has no window or the extension isn't there.
pub fn focus_running(desktop_id: &str, done: impl FnOnce(bool) + 'static) {
    let id = desktop_id.to_string();
    gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |conn| {
        let Ok(conn) = conn else { return done(false) };
        conn.call(
            Some("org.gnome.Shell"),
            "/com/spotty/Focus",
            "com.spotty.Focus",
            "FocusApp",
            Some(&(id,).to_variant()),
            Some(glib::VariantTy::new("(b)").unwrap()),
            gio::DBusCallFlags::NO_AUTO_START,
            500,
            gio::Cancellable::NONE,
            move |r| done(r.ok().and_then(|v| v.get::<(bool,)>()).is_some_and(|(b,)| b)),
        );
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_metadata_matches_the_uuid() {
        assert!(super::METADATA.contains(&format!("\"uuid\": \"{}\"", super::UUID)));
        assert!(super::EXTENSION_JS.contains("FocusAppAsync"));
    }
}
