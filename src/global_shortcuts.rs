//! Global shortcuts through the XDG GlobalShortcuts portal, for desktops
//! that don't run GNOME Shell (Plasma, Phosh, Hyprland, …). GNOME's custom
//! keybindings (`keybindings.rs`) only work under GNOME Shell; here Spotty
//! receives the key press itself, so no command has to be run.
//!
//! Everything but `available()` runs on the GTK main thread: the portal
//! answers through D-Bus signals dispatched on the main context.
use std::cell::RefCell;
use std::rc::Rc;

use gio::prelude::*;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

thread_local! {
    /// The live portal session and its Activated subscription.
    static SESSION: RefCell<Option<(gio::DBusConnection, String, gio::SignalSubscriptionId)>> =
        const { RefCell::new(None) };
}

/// Whether the desktop's portal offers GlobalShortcuts (xdg-desktop-portal
/// only exports interfaces a backend implements). Blocking; any thread.
pub fn available() -> bool {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return false;
    };
    bus.call_sync(
        Some(PORTAL),
        PATH,
        "org.freedesktop.DBus.Properties",
        "Get",
        Some(&(IFACE, "version").to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        2000,
        gio::Cancellable::NONE,
    )
    .is_ok()
}

/// A stored shortcut ("Ctrl+Space") as a portal trigger ("CTRL+space", the
/// XDG shortcuts format: CTRL/ALT/SHIFT/LOGO plus an xkb key name).
fn portal_trigger(human: &str) -> String {
    let mut parts = Vec::new();
    let mut key = String::new();
    for part in human.split('+') {
        let p = part.trim();
        match p.to_lowercase().as_str() {
            "ctrl" | "control" | "primary" => parts.push("CTRL".to_string()),
            "alt" | "meta" => parts.push("ALT".to_string()),
            "shift" => parts.push("SHIFT".to_string()),
            "super" | "logo" => parts.push("LOGO".to_string()),
            lower => {
                key = match lower {
                    "return" | "enter" => "Return".into(),
                    "tab" => "Tab".into(),
                    "escape" | "esc" => "Escape".into(),
                    "backspace" => "BackSpace".into(),
                    "delete" => "Delete".into(),
                    "insert" => "Insert".into(),
                    "home" => "Home".into(),
                    "end" => "End".into(),
                    f if f.len() > 1 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
                        f.to_uppercase()
                    }
                    other => other.to_string(),
                }
            }
        }
    }
    parts.push(key);
    parts.join("+")
}

/// The shortcut id for a keyword ("" is the main toggle).
fn shortcut_id(keyword_id: &str) -> String {
    if keyword_id.is_empty() { "toggle".into() } else { format!("kw:{keyword_id}") }
}

/// Close the current portal session, if any (its shortcuts go with it).
fn close() {
    if let Some((bus, session, sub)) = SESSION.with(|s| s.borrow_mut().take()) {
        bus.signal_unsubscribe(sub);
        bus.call(
            Some(PORTAL),
            &session,
            "org.freedesktop.portal.Session",
            "Close",
            None,
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            |_| {},
        );
    }
}

/// Call a portal method that answers through a Request object's Response
/// signal; `on_ok` gets the results when the user/portal accepted.
fn request(
    bus: &gio::DBusConnection,
    method: &'static str,
    params: impl FnOnce(&str) -> glib::Variant,
    on_ok: impl FnOnce(&gio::DBusConnection, glib::VariantDict) + 'static,
) {
    let Some(sender) = bus.unique_name() else { return };
    let sender = sender.trim_start_matches(':').replace('.', "_");
    let token = format!("spotty{}", glib::random_int());
    let path = format!("{PATH}/request/{sender}/{token}");
    let sub: Rc<RefCell<Option<gio::SignalSubscriptionId>>> = Rc::default();
    let sub_cb = sub.clone();
    let on_ok = RefCell::new(Some(on_ok));
    // Subscribe before calling, so a fast Response can't be missed.
    let id = bus.signal_subscribe(
        Some(PORTAL),
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        Some(&path),
        None,
        gio::DBusSignalFlags::NONE,
        move |bus, _, _, _, _, params| {
            if let Some(id) = sub_cb.borrow_mut().take() {
                bus.signal_unsubscribe(id);
            }
            let code = params.child_value(0).get::<u32>().unwrap_or(2);
            if code != 0 {
                log::warn!("global shortcuts: {method} declined (response {code})");
                return;
            }
            if let Some(f) = on_ok.borrow_mut().take() {
                f(bus, glib::VariantDict::new(Some(&params.child_value(1))));
            }
        },
    );
    *sub.borrow_mut() = Some(id);
    bus.call(
        Some(PORTAL),
        PATH,
        IFACE,
        method,
        Some(&params(&token)),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        move |res| {
            if let Err(e) = res {
                log::warn!("global shortcuts: {method} failed: {e}");
            }
        },
    );
}

/// What a press of shortcut `id` does.
fn activated(id: &str, token: Option<String>) {
    let Some(app) = gio::Application::default().and_downcast::<adw::Application>() else {
        return;
    };
    crate::app::set_activation_token(token);
    match id.strip_prefix("kw:") {
        Some(kw) => crate::app::show_keyword_search(&app, kw),
        None => crate::app::toggle_search(&app),
    }
}

/// Bind `shortcuts` — (keyword id, name, stored shortcut) — through the
/// portal, replacing the previous session. Main thread only.
pub fn bind(shortcuts: Vec<(String, String, String)>) {
    close();
    if shortcuts.is_empty() {
        return;
    }
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return;
    };
    request(
        &bus,
        "CreateSession",
        |token| {
            let o = glib::VariantDict::new(None);
            o.insert("handle_token", token);
            o.insert("session_handle_token", token);
            glib::Variant::tuple_from_iter([o.end()])
        },
        move |bus, results| {
            let Some(session) = results
                .lookup_value("session_handle", None)
                .and_then(|v| v.str().map(str::to_owned))
            else {
                log::warn!("global shortcuts: no session handle");
                return;
            };
            let session_cb = session.clone();
            let sub = bus.signal_subscribe(
                Some(PORTAL),
                Some(IFACE),
                Some("Activated"),
                Some(PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |_, _, _, _, _, params| {
                    if params.child_value(0).str() != Some(session_cb.as_str()) {
                        return;
                    }
                    let Some(id) = params.child_value(1).str().map(str::to_owned) else { return };
                    let token = glib::VariantDict::new(Some(&params.child_value(3)))
                        .lookup::<String>("activation_token")
                        .ok()
                        .flatten();
                    activated(&id, token);
                },
            );
            SESSION.with(|s| *s.borrow_mut() = Some((bus.clone(), session.clone(), sub)));

            let item_ty = glib::VariantTy::new("(sa{sv})").unwrap();
            let items = shortcuts.iter().map(|(kw, name, shortcut)| {
                let d = glib::VariantDict::new(None);
                d.insert("description", name.as_str());
                d.insert("preferred_trigger", portal_trigger(shortcut));
                glib::Variant::tuple_from_iter([shortcut_id(kw).to_variant(), d.end()])
            });
            let list = glib::Variant::array_from_iter_with_type(item_ty, items);
            let Ok(path) = glib::variant::ObjectPath::try_from(session) else { return };
            let count = shortcuts.len();
            request(
                bus,
                "BindShortcuts",
                move |token| {
                    let o = glib::VariantDict::new(None);
                    o.insert("handle_token", token);
                    glib::Variant::tuple_from_iter([path.to_variant(), list, "".to_variant(), o.end()])
                },
                move |_, _| log::info!("global shortcuts: bound {count} through the portal"),
            );
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_shortcuts_become_portal_triggers() {
        assert_eq!(portal_trigger("Ctrl+Space"), "CTRL+space");
        assert_eq!(portal_trigger("Alt+Space"), "ALT+space");
        assert_eq!(portal_trigger("Super+Shift+F12"), "LOGO+SHIFT+F12");
        assert_eq!(portal_trigger("Ctrl+Return"), "CTRL+Return");
        assert_eq!(portal_trigger("Ctrl+C"), "CTRL+c");
        assert_eq!(shortcut_id(""), "toggle");
        assert_eq!(shortcut_id("files"), "kw:files");
    }
}
