// Detect the default browser's search engine from Chromium-family Preferences.
// Falls back to DuckDuckGo when detection fails or the browser is unsupported.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Detected {
    name: String,
    url_template: String,
}

fn cache() -> &'static Mutex<Option<(Instant, Option<Detected>)>> {
    static C: OnceLock<Mutex<Option<(Instant, Option<Detected>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

/// Browser's search engine URL for a query. Triggers background detection on
/// first call; returns `None` if detection hasn't completed yet or failed.
pub fn url_for(q: &str) -> Option<String> {
    ensure_detected();
    let c = cache().lock().unwrap();
    let d = c.as_ref()?.1.as_ref()?;
    let template = &d.url_template;
    let encoded = urlencoding::encode(q);
    let url = if template.contains("{searchTerms}") {
        template.replace("{searchTerms}", &encoded)
    } else if template.contains("%s") {
        template.replace("%s", &encoded)
    } else {
        return None;
    };
    Some(url)
}

/// The detected search engine name (e.g. "Kagi"), if known.
pub fn engine_name() -> Option<String> {
    ensure_detected();
    let c = cache().lock().unwrap();
    c.as_ref()?.1.as_ref().map(|d| d.name.clone())
}

fn ensure_detected() {
    {
        let c = cache().lock().unwrap();
        if let Some((t, _)) = c.as_ref() {
            if t.elapsed() < Duration::from_secs(300) {
                return;
            }
        }
    }
    std::thread::spawn(|| {
        let result = detect_via_host();
        *cache().lock().unwrap() = Some((Instant::now(), result));
        // Schedule UI refresh on the main thread via glib idle.
        glib::idle_add_once(|| {
            crate::app::refresh_search_window();
        });
    });
}

// ── Host-side detection via xdg-settings / gio mime ───────────────────────────

fn detect_via_host() -> Option<Detected> {
    // Try `gio mime x-scheme-handler/https` first (most reliable).
    let out = crate::app::run_host_shell_command("gio mime x-scheme-handler/https")
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Output: "Default application for 'x-scheme-handler/https': com.brave.Browser.desktop\n"
    let id = stdout
        .lines()
        .find(|l| l.contains("Default application for"))?
        .split_once(':')?
        .1
        .trim()
        .trim_end_matches('\n')
        .to_string();
    if id.is_empty() {
        return None;
    }
    detect_chromium_for_desktop(&id)
}

// ── Chromium-family detection ─────────────────────────────────────────────────

/// Map desktop ID → Flatpak/Chromium config directory name.
fn chromium_config_dir(desktop_id: &str) -> Option<&'static str> {
    let id = desktop_id.to_lowercase();
    if id.contains("brave") {
        Some("BraveSoftware/Brave-Browser")
    } else if id.contains("chrome") && !id.contains("chromium") {
        Some("google-chrome")
    } else if id.contains("chromium") {
        Some("chromium")
    } else if id.contains("edge") {
        Some("microsoft-edge")
    } else if id.contains("vivaldi") {
        Some("vivaldi")
    } else if id.contains("opera") {
        Some("opera")
    } else if id.contains("yandex") {
        Some("yandex-browser")
    } else {
        None
    }
}

/// Try to detect the search engine from a Chromium-family browser's Preferences.
fn detect_chromium_for_desktop(desktop_id: &str) -> Option<Detected> {
    let dir_name = chromium_config_dir(desktop_id)?;
    let flatpak_id = desktop_id_to_flatpak_id(desktop_id)?;

    // Candidate roots: Flatpak first, then native.
    let home = dirs::home_dir()?;
    let candidates = vec![
        home.join(format!(".var/app/{flatpak_id}/config/{dir_name}")),
        home.join(format!(".config/{dir_name}")),
    ];

    for root in candidates {
        if !root.exists() {
            continue;
        }
        let profile_dir = find_chromium_profile(&root)?;
        let prefs_path = profile_dir.join("Preferences");
        if !prefs_path.exists() {
            continue;
        }
        if let Some(d) = parse_chromium_prefs(&prefs_path) {
            return Some(d);
        }
    }
    None
}

/// Convert a desktop ID to its Flatpak app-id (if it's a Flatpak app).
/// E.g. "com.brave.Browser.desktop" → "com.brave.Browser"
fn desktop_id_to_flatpak_id(desktop_id: &str) -> Option<String> {
    let id = desktop_id.strip_suffix(".desktop").unwrap_or(desktop_id);
    // Flatpak IDs always contain at least one dot
    if id.contains('.') {
        Some(id.to_string())
    } else {
        None
    }
}

/// Find the active Chromium profile directory by reading `Local State`.
fn find_chromium_profile(chromium_root: &PathBuf) -> Option<PathBuf> {
    let local_state_path = chromium_root.join("Local State");
    if local_state_path.exists() {
        let data = fs::read_to_string(&local_state_path).ok()?;
        let v: Value = serde_json::from_str(&data).ok()?;
        // Try last_active_profiles first (Brave uses this)
        if let Some(profiles) = v.pointer("/profile/last_active_profiles") {
            if let Some(arr) = profiles.as_array() {
                if let Some(name) = arr.first().and_then(|v| v.as_str()) {
                    let p = chromium_root.join(name);
                    if p.is_dir() {
                        return Some(p);
                    }
                }
            }
        }
        // Try last_used
        if let Some(name) = v.pointer("/profile/last_used").and_then(|v| v.as_str()) {
            let p = chromium_root.join(name);
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    // Fallback: Default profile
    let default = chromium_root.join("Default");
    if default.is_dir() {
        return Some(default);
    }
    // Fallback: first Profile_* directory
    let entries = fs::read_dir(chromium_root).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("Profile ") {
            return Some(entry.path());
        }
    }
    None
}

/// Parse a Chromium Preferences JSON for the default search engine template.
fn parse_chromium_prefs(prefs_path: &PathBuf) -> Option<Detected> {
    let data = fs::read_to_string(prefs_path).ok()?;
    let v: Value = serde_json::from_str(&data).ok()?;

    // Modern path: default_search_provider_data.template_url_data
    if let Some(tud) = v.pointer("/default_search_provider_data/template_url_data") {
        let url = tud.get("url").and_then(|v| v.as_str())?;
        if !url.contains("{searchTerms}") && !url.contains("%s") {
            return None;
        }
        let name = tud
            .get("short_name")
            .or_else(|| tud.get("keyword"))
            .and_then(|v| v.as_str())
            .unwrap_or("default browser")
            .to_string();
        return Some(Detected {
            name,
            url_template: url.to_string(),
        });
    }

    // Legacy path: default_search_provider.search_url
    if let Some(search_url) = v
        .pointer("/default_search_provider/search_url")
        .and_then(|v| v.as_str())
    {
        if search_url.contains("{searchTerms}") || search_url.contains("%s") {
            let name = v
                .pointer("/default_search_provider/name")
                .and_then(|v| v.as_str())
                .unwrap_or("default browser")
                .to_string();
            return Some(Detected {
                name,
                url_template: search_url.to_string(),
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_search_terms() {
        let mut d = Detected {
            name: "Kagi".into(),
            url_template: "https://kagi.com/search?q={searchTerms}".into(),
        };
        let encoded = urlencoding::encode("hello world");
        let url = d.url_template.replace("{searchTerms}", &encoded);
        assert!(url.contains("hello%20world"));

        d.url_template = "https://example.com/search?q=%s".into();
        let url = d.url_template.replace("%s", &encoded);
        assert!(url.contains("hello%20world"));
    }

    #[test]
    fn maps_chromium_desktop_ids() {
        assert_eq!(
            chromium_config_dir("com.brave.Browser.desktop"),
            Some("BraveSoftware/Brave-Browser")
        );
        assert_eq!(
            chromium_config_dir("com.google.Chrome.desktop"),
            Some("google-chrome")
        );
        assert_eq!(
            chromium_config_dir("org.chromium.Chromium.desktop"),
            Some("chromium")
        );
        assert_eq!(
            chromium_config_dir("com.microsoft.Edge.desktop"),
            Some("microsoft-edge")
        );
        assert_eq!(
            chromium_config_dir("com.vivaldi.Vivaldi.desktop"),
            Some("vivaldi")
        );
        assert_eq!(
            chromium_config_dir("com.opera.Opera.desktop"),
            Some("opera")
        );
        assert_eq!(
            chromium_config_dir("ru.yandex.Browser.desktop"),
            Some("yandex-browser")
        );
        // Non-Chromium browsers
        assert_eq!(chromium_config_dir("org.mozilla.firefox.desktop"), None);
        assert_eq!(chromium_config_dir("io.gitlab.librewolf-community.desktop"), None);
    }

    #[test]
    fn parses_chromium_prefs_json() {
        let json = r#"{
            "default_search_provider_data": {
                "template_url_data": {
                    "url": "https://kagi.com/search?q={searchTerms}",
                    "short_name": "Kagi"
                }
            }
        }"#;
        let v: Value = serde_json::from_str(json).unwrap();
        let tud = v.pointer("/default_search_provider_data/template_url_data").unwrap();
        let url = tud.get("url").unwrap().as_str().unwrap();
        assert!(url.contains("{searchTerms}"));
        let name = tud.get("short_name").unwrap().as_str().unwrap();
        assert_eq!(name, "Kagi");
    }

    #[test]
    fn parses_legacy_prefs_json() {
        let json = r#"{
            "default_search_provider": {
                "search_url": "https://duckduckgo.com/?q={searchTerms}",
                "name": "DuckDuckGo"
            }
        }"#;
        let v: Value = serde_json::from_str(json).unwrap();
        let url = v
            .pointer("/default_search_provider/search_url")
            .and_then(|v| v.as_str())
            .unwrap();
        assert!(url.contains("{searchTerms}"));
    }

    #[test]
    fn desktop_id_to_flatpak_id_works() {
        // Chromium-family Flatpak IDs (contain dots)
        assert_eq!(
            desktop_id_to_flatpak_id("com.brave.Browser.desktop"),
            Some("com.brave.Browser".into())
        );
        assert_eq!(
            desktop_id_to_flatpak_id("com.google.Chrome.desktop"),
            Some("com.google.Chrome".into())
        );
        // Non-Flatpak system Firefox (no dot after stripping .desktop)
        assert_eq!(desktop_id_to_flatpak_id("firefox.desktop"), None);
        // Bare ID without .desktop suffix and no dot
        assert_eq!(desktop_id_to_flatpak_id("firefox"), None);
    }
}
