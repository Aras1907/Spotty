// Desktop-environment detection for DE-aware system actions.
// Reads XDG_CURRENT_DESKTOP / XDG_SESSION_DESKTOP / DESKTOP_SESSION and
// classifies into a known DE enum, falling back to Unknown.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desktop {
    Gnome,
    Kde,
    Cinnamon,
    Mate,
    Xfce,
    Pantheon,
    Budgie,
    Cosmic,
    Sway,
    Hyprland,
    Unknown,
}

/// Classify a colon-separated env value (e.g. "ubuntu:GNOME") into a DE.
/// Specific tokens win over generic ones (e.g. "Budgie:GNOME" → Budgie).
pub fn classify(value: &str) -> Desktop {
    let mut result = Desktop::Unknown;
    for token in value.split(':') {
        let t = token.trim();
        let specific = match t {
            "gnome" | "GNOME" | "gnome-classic" | "gnome-flashback" | "ubuntu:GNOME" => Desktop::Gnome,
            "kde" | "KDE" | "plasma" | "Plasma" => Desktop::Kde,
            "X-Cinnamon" | "Cinnamon" | "cinnamon" => Desktop::Cinnamon,
            "MATE" | "mate" | "MATE::GNOME" => Desktop::Mate,
            "XFCE" | "xfce" => Desktop::Xfce,
            "Pantheon" | "pantheon" => Desktop::Pantheon,
            "Budgie" | "budgie" => Desktop::Budgie,
            "COSMIC" | "cosmic" => Desktop::Cosmic,
            "sway" | "Sway" => Desktop::Sway,
            "Hyprland" | "hyprland" => Desktop::Hyprland,
            _ => continue,
        };
        // Prefer specific tokens (Budgie/Cinnamon) over generic GNOME.
        result = specific;
        if result != Desktop::Gnome {
            return result;
        }
    }
    result
}

/// Cached DE detection. Runs once, reads XDG env vars.
pub fn detect() -> Desktop {
    static DETECTED: OnceLock<Desktop> = OnceLock::new();
    *DETECTED.get_or_init(|| {
        if let Ok(v) = std::env::var("XDG_CURRENT_DESKTOP") {
            let d = classify(&v);
            if d != Desktop::Unknown {
                return d;
            }
        }
        if let Ok(v) = std::env::var("XDG_SESSION_DESKTOP") {
            let d = classify(&v);
            if d != Desktop::Unknown {
                return d;
            }
        }
        if let Ok(v) = std::env::var("DESKTOP_SESSION") {
            let d = classify(&v);
            if d != Desktop::Unknown {
                return d;
            }
        }
        Desktop::Unknown
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_single_tokens() {
        assert_eq!(classify("GNOME"), Desktop::Gnome);
        assert_eq!(classify("KDE"), Desktop::Kde);
        assert_eq!(classify("X-Cinnamon"), Desktop::Cinnamon);
        assert_eq!(classify("MATE"), Desktop::Mate);
        assert_eq!(classify("XFCE"), Desktop::Xfce);
        assert_eq!(classify("Pantheon"), Desktop::Pantheon);
        assert_eq!(classify("COSMIC"), Desktop::Cosmic);
        assert_eq!(classify("sway"), Desktop::Sway);
        assert_eq!(classify("Hyprland"), Desktop::Hyprland);
    }

    #[test]
    fn classifies_colon_separated() {
        assert_eq!(classify("ubuntu:GNOME"), Desktop::Gnome);
        assert_eq!(classify("Budgie:GNOME"), Desktop::Budgie);
        assert_eq!(classify("MATE::GNOME"), Desktop::Mate);
    }

    #[test]
    fn unknown_falls_back() {
        assert_eq!(classify(""), Desktop::Unknown);
        assert_eq!(classify("i3"), Desktop::Unknown);
    }
}
