//! Tiny gettext bridge — the app's text domain without any extra crate.
//!
//! Strings are marked with [`gettext`], extracted into `po/spotty.pot` by
//! `scripts/extract_i18n.py`, translated per language in `po/<lang>.po`,
//! and compiled to `<locale>/LC_MESSAGES/spotty.mo` by
//! `scripts/build_locales.sh`. Untranslated languages (or a missing .mo)
//! simply fall back to the English msgid.
//!
//! The three entry points live in glibc itself, so no extra linkage is
//! needed. GTK already calls `setlocale(LC_ALL, "")` during init, so the
//! locale is set by the time these run.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::{Path, PathBuf};

extern "C" {
    fn bindtextdomain(domain: *const c_char, dir: *const c_char) -> *mut c_char;
    fn dgettext(domain: *const c_char, msgid: *const c_char) -> *mut c_char;
}

const DOMAIN: &[u8] = b"spotty\0";

/// Translate `msgid` for the current locale. Returns the English text
/// unchanged when nothing is loaded or no translation exists.
pub fn gettext(msgid: &str) -> String {
    let Ok(c) = CString::new(msgid) else {
        return msgid.to_string();
    };
    unsafe {
        let out = dgettext(DOMAIN.as_ptr() as *const c_char, c.as_ptr());
        if out.is_null() {
            return msgid.to_string();
        }
        CStr::from_ptr(out).to_string_lossy().into_owned()
    }
}

/// Point the `spotty` text domain at `dir`, which must contain
/// `<lang>/LC_MESSAGES/spotty.mo`.
pub fn bind_domain(dir: &Path) {
    let Ok(c) = CString::new(dir.to_string_lossy().as_bytes()) else {
        return;
    };
    unsafe {
        bindtextdomain(DOMAIN.as_ptr() as *const c_char, c.as_ptr());
    }
    let _ = c; // kept alive across the call above
}

/// Locale directory resolution: env override → Flatpak's /app → the
/// user-local install that `scripts/build_locales.sh --install` targets.
pub fn default_locale_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SPOTTY_LOCALEDIR") {
        return dir.into();
    }
    if Path::new("/app/share/locale").is_dir() {
        return "/app/share/locale".into();
    }
    let mut p = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    p.push(".local/share/locale");
    p
}
