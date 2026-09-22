// MPRIS (org.mpris.MediaPlayer2) integration.
//
// Exposes the currently-playing track on the session bus so GNOME's media
// controls — the player widget in the notification/quick-settings shade and on
// the lock screen — show the title, artist, cover art, and Play/Pause/Next/
// Previous buttons, all wired back to Spotty's queue.
//
// Everything here runs on the GTK main thread (the GLib main loop dispatches
// D-Bus callbacks there), so reading the shared player state is contention-free.
//
// History: this module used to pause external players (video, music) when
// Spotty opened and resume them on hide — video freezing was the #1 complaint
// about the show mechanism. That was removed entirely: Spotty is a launcher
// and must never touch what the user is watching. `init()` only *registers*
// the MPRIS service; the controls are a passive read/control surface.

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use std::cell::RefCell;

const BUS_NAME: &str = "org.mpris.MediaPlayer2.spotty";
const OBJ_PATH: &str = "/org/mpris/MediaPlayer2";
const IFACE_ROOT: &str = "org.mpris.MediaPlayer2";
const IFACE_PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

thread_local! {
    static CONN: RefCell<Option<gio::DBusConnection>> = const { RefCell::new(None) };
    static OWNER: RefCell<Option<gio::OwnerId>> = const { RefCell::new(None) };
}

const INTROSPECTION: &str = r#"
<node>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="CanRaise" type="b" access="read"/>
    <property name="HasTrackList" type="b" access="read"/>
    <property name="Identity" type="s" access="read"/>
    <property name="DesktopEntry" type="s" access="read"/>
    <property name="SupportedUriSchemes" type="as" access="read"/>
    <property name="SupportedMimeTypes" type="as" access="read"/>
  </interface>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Play"/>
    <method name="Seek"><arg name="Offset" type="x" direction="in"/></method>
    <method name="SetPosition"><arg name="TrackId" type="o" direction="in"/><arg name="Position" type="x" direction="in"/></method>
    <method name="OpenUri"><arg name="Uri" type="s" direction="in"/></method>
    <signal name="Seeked"><arg name="Position" type="x"/></signal>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="LoopStatus" type="s" access="readwrite"/>
    <property name="Rate" type="d" access="readwrite"/>
    <property name="Shuffle" type="b" access="readwrite"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Volume" type="d" access="readwrite"/>
    <property name="Position" type="x" access="read"/>
    <property name="MinimumRate" type="d" access="read"/>
    <property name="MaximumRate" type="d" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanSeek" type="b" access="read"/>
    <property name="CanControl" type="b" access="read"/>
  </interface>
</node>
"#;

/// Acquire the MPRIS bus name and register the objects. Call once at startup.
pub fn init(app: &adw::Application) {
    let app = app.clone();
    let owner = gio::bus_own_name(
        gio::BusType::Session,
        BUS_NAME,
        gio::BusNameOwnerFlags::NONE,
        move |conn, _name| register(&conn, &app),
        |_conn, _name| {},
        |_conn, _name| log::warn!("mpris: could not acquire {BUS_NAME}"),
    );
    OWNER.with(|o| *o.borrow_mut() = Some(owner));
}

fn register(conn: &gio::DBusConnection, app: &adw::Application) {
    let node = match gio::DBusNodeInfo::for_xml(INTROSPECTION) {
        Ok(n) => n,
        Err(e) => {
            log::error!("mpris: bad introspection xml: {e}");
            return;
        }
    };
    let (Some(root_if), Some(player_if)) = (
        node.lookup_interface(IFACE_ROOT),
        node.lookup_interface(IFACE_PLAYER),
    ) else {
        log::error!("mpris: missing interface in introspection");
        return;
    };

    // Root interface: Raise / Quit + identity properties.
    let app_root = app.clone();
    let _ = conn
        .register_object(OBJ_PATH, &root_if)
        .method_call(move |_c, _s, _p, _i, method, _params, inv| {
            match method {
                "Raise" => crate::app::show_search(&app_root),
                "Quit" => app_root.quit(),
                _ => {}
            }
            inv.return_value(None);
        })
        .property(|_c, _s, _p, _i, prop| root_property(prop))
        .build();

    // Player interface: transport controls + now-playing properties.
    let _ = conn
        .register_object(OBJ_PATH, &player_if)
        .method_call(move |_c, _s, _p, _i, method, _params, inv| {
            match method {
                "PlayPause" => crate::music_operations::toggle(),
                "Play" => crate::music_operations::resume(),
                "Pause" => crate::music_operations::pause(),
                "Stop" => crate::music_operations::stop_music(),
                "Next" => crate::music_operations::next(),
                "Previous" => crate::music_operations::previous(),
                _ => {}
            }
            inv.return_value(None);
        })
        .property(|_c, _s, _p, _i, prop| player_property(prop))
        .set_property(|_c, _s, _p, _i, _prop, _val| true)
        .build();

    CONN.with(|c| *c.borrow_mut() = Some(conn.clone()));
    notify();
}

fn root_property(prop: &str) -> glib::Variant {
    match prop {
        "CanQuit" => true.to_variant(),
        "CanRaise" => true.to_variant(),
        "HasTrackList" => false.to_variant(),
        "Identity" => "Spotty".to_variant(),
        "DesktopEntry" => "com.spotty.Spotty".to_variant(),
        "SupportedUriSchemes" => Vec::<String>::new().to_variant(),
        "SupportedMimeTypes" => Vec::<String>::new().to_variant(),
        _ => "".to_variant(),
    }
}

fn playback_status() -> &'static str {
    use crate::music_operations::PlaybackState;
    match crate::music_operations::current().map(|op| op.state) {
        Some(PlaybackState::Playing) => "Playing",
        Some(PlaybackState::Paused) => "Paused",
        _ => "Stopped",
    }
}

fn player_property(prop: &str) -> glib::Variant {
    let op = crate::music_operations::current();
    let has = op.is_some();
    let has_next = op
        .as_ref()
        .map(|o| o.index + 1 < o.queue.len())
        .unwrap_or(false);
    let has_prev = op.as_ref().map(|o| o.index > 0).unwrap_or(false);
    match prop {
        "PlaybackStatus" => playback_status().to_variant(),
        "LoopStatus" => "None".to_variant(),
        "Rate" | "MinimumRate" | "MaximumRate" => 1.0_f64.to_variant(),
        "Shuffle" => false.to_variant(),
        "Volume" => 1.0_f64.to_variant(),
        "Metadata" => metadata(op.as_ref()),
        "Position" => (op.as_ref().map(|o| o.elapsed_ms).unwrap_or(0) as i64 * 1000).to_variant(),
        "CanGoNext" => has_next.to_variant(),
        "CanGoPrevious" => has_prev.to_variant(),
        "CanPlay" => has.to_variant(),
        "CanPause" => has.to_variant(),
        "CanSeek" => false.to_variant(),
        "CanControl" => true.to_variant(),
        _ => "".to_variant(),
    }
}

/// Build the `Metadata` a{sv} dict for the current track (empty if none).
fn metadata(op: Option<&crate::music_operations::MusicOperation>) -> glib::Variant {
    let dict = glib::VariantDict::new(None);
    if let Some(op) = op {
        if let Some(t) = op.current_track() {
            if let Ok(trackid) = glib::Variant::parse(
                Some(glib::VariantTy::OBJECT_PATH),
                &format!("/com/spotty/track/{}", op.index),
            ) {
                dict.insert_value("mpris:trackid", &trackid);
            }
            if op.duration_ms > 0 {
                dict.insert("mpris:length", op.duration_ms as i64 * 1000);
            }
            dict.insert("xesam:title", t.title.clone());
            let artist = if t.artist.is_empty() {
                t.source_label().to_string()
            } else {
                t.artist.clone()
            };
            dict.insert_value("xesam:artist", &vec![artist].to_variant());
            // Prefer the local cached cover file; fall back to the remote URL.
            if let Some(cp) = &op.cover_path {
                dict.insert("mpris:artUrl", format!("file://{}", cp.display()));
            } else if !t.cover_url.is_empty() {
                dict.insert("mpris:artUrl", t.cover_url.clone());
            }
        }
    }
    dict.end()
}

/// Emit PropertiesChanged so GNOME refreshes the media widget. Call on the main
/// thread whenever the track or playback state changes.
pub fn notify() {
    CONN.with(|c| {
        let Some(conn) = c.borrow().clone() else {
            return;
        };
        let op = crate::music_operations::current();
        let has = op.is_some();
        let has_next = op
            .as_ref()
            .map(|o| o.index + 1 < o.queue.len())
            .unwrap_or(false);
        let has_prev = op.as_ref().map(|o| o.index > 0).unwrap_or(false);

        let changed = glib::VariantDict::new(None);
        changed.insert_value("PlaybackStatus", &playback_status().to_variant());
        changed.insert_value("Metadata", &metadata(op.as_ref()));
        changed.insert_value("CanGoNext", &has_next.to_variant());
        changed.insert_value("CanGoPrevious", &has_prev.to_variant());
        changed.insert_value("CanPlay", &has.to_variant());
        changed.insert_value("CanPause", &has.to_variant());

        // PropertiesChanged signature is (s a{sv} as) — build the tuple from the
        // exact element variants so the dict stays a{sv} (not wrapped as 'v').
        let body = glib::Variant::tuple_from_iter([
            IFACE_PLAYER.to_variant(),
            changed.end(),
            Vec::<String>::new().to_variant(),
        ]);
        let _ = conn.emit_signal(
            None,
            OBJ_PATH,
            PROPS_IFACE,
            "PropertiesChanged",
            Some(&body),
        );
    });
}
