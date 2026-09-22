use crate::search::{ResultKind, SearchResult};
use adw::prelude::*;
use gtk::glib;
use gtk::pango;

pub struct ResultRow {
    pub container: gtk::Box,
    /// Present only when this row supports removal (clipboard entries).
    pub trash_button: Option<gtk::Button>,
    /// Present only for clipboard text entries: toggles the pinned state.
    pub pin_button: Option<gtk::Button>,
}

impl ResultRow {
    /// Build a row, optionally with a trash/remove button on the right.
    pub fn with_trash(r: &SearchResult, show_trash: bool) -> Self {
        Self::build(r, show_trash, None)
    }

    /// Build a row with both a pin-toggle button (reflecting `pinned`) and an
    /// optional trash/remove button on the right. `pin_label` is the
    /// human-readable shortcut (e.g. "Ctrl+P") shown in the tooltip.
    pub fn with_trash_and_pin(
        r: &SearchResult,
        show_trash: bool,
        pinned: bool,
        pin_label: &str,
    ) -> Self {
        Self::build(r, show_trash, Some((pinned, pin_label.to_string())))
    }

    fn build(r: &SearchResult, show_trash: bool, pin_state: Option<(bool, String)>) -> Self {
        // Running background operation: title + live status, with an animated
        // indeterminate loading bar underneath. This is the install indicator.
        if r.icon.as_deref() == Some("op-progress") {
            return Self::build_progress(r);
        }
        // Now-playing music row: cover art + transport controls + progress.
        if r.icon.as_deref() == Some("music-row") {
            return Self::build_music(r);
        }
        // Dictionary definition: expanded multi-line card below the results.
        if r.icon.as_deref() == Some("dict-def") {
            return Self::build_definition(r);
        }

        let c = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .build();

        if r.icon.as_deref() == Some("emblem-synchronizing-symbolic") {
            // Running operations / scanning indicators: orb + soft pulse.
            c.add_css_class("op-pulse");
            let ring = crate::ui::circular_progress::progress_ring(
                22,
                None,
                crate::ui::circular_progress::RingState::Running,
            );
            c.append(&ring);
        } else if r.kind == ResultKind::Emoji {
            // Render the literal emoji glyph as large text instead of an icon.
            c.append(
                &gtk::Label::builder()
                    .label(r.icon.as_deref().unwrap_or(""))
                    .width_request(32)
                    .css_classes(["title-2"])
                    .build(),
            );
        } else {
            let icon = gtk::Image::builder().pixel_size(24).build();
            if let Some(n) = &r.icon {
                if std::path::Path::new(n).is_absolute() {
                    icon.set_from_file(Some(n));
                } else if matches!(r.kind, ResultKind::App) {
                    set_app_icon(&icon, n);
                } else if let Some(domain) = n.strip_prefix("favicon:") {
                    // Web-search result: show the search engine's own favicon
                    // (works for built-in engines and custom search URLs alike).
                    icon.set_icon_name(Some("web-browser-symbolic"));
                    set_favicon(&icon, domain);
                } else if is_app_id(n) {
                    // App-id style icon name (e.g. install results): resolve the
                    // real app icon, falling back to a generic package icon.
                    set_app_icon_or(&icon, n, "package-x-generic-symbolic");
                } else if let Some(rest) = n.strip_prefix("pkg:") {
                    // Distro package result (e.g. "pkg:firefox-langpacks-en-us:package-x-generic-symbolic"):
                    // try to resolve a real app icon matching the package name,
                    // also trying progressively shorter prefixes (subpackages
                    // like "-devel"/"-langpacks-en-us" rarely ship their own
                    // icon, but the base package usually does), falling back
                    // to the given generic icon.
                    let (name, fallback) = rest
                        .rsplit_once(':')
                        .unwrap_or((rest, "package-x-generic-symbolic"));
                    set_pkg_icon(&icon, name, fallback);
                } else {
                    icon.set_icon_name(Some(n));
                }
            } else {
                icon.set_icon_name(Some(match r.kind {
                    ResultKind::App => "application-x-executable-symbolic",
                    ResultKind::File | ResultKind::Calculator => "text-x-generic-symbolic",
                    ResultKind::Folder => "folder-symbolic",
                    ResultKind::Web => "web-browser-symbolic",
                    ResultKind::Clipboard => "edit-paste-symbolic",
                    ResultKind::System => "system-shutdown-symbolic",
                    ResultKind::Emoji => "face-smile-symbolic",
                }));
            }
            c.append(&icon);
            if r.title.starts_with("Install: ") {
                icon.set_opacity(0.45);
            }
        }

        let tb = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(1)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        tb.append(
            &gtk::Label::builder()
                .label(&r.title)
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(60)
                .build(),
        );
        if let Some(s) = &r.subtitle {
            let subtitle = gtk::Label::builder()
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::Middle)
                .max_width_chars(80)
                .css_classes(["caption", "dim-label"])
                .build();
            if is_text_match_markup(s) {
                subtitle.set_use_markup(true);
                subtitle.set_markup(s);
            } else {
                subtitle.set_label(s);
            }
            tb.append(&subtitle);
        }
        c.append(&tb);

        let pin_button = pin_state.map(|(pinned, pin_label)| {
            let action = if pinned { "Unpin" } else { "Pin" };
            let tooltip = if pin_label.is_empty() {
                action.to_string()
            } else {
                format!("{} ({})", action, pin_label)
            };
            let btn = gtk::Button::builder()
                .icon_name("view-pin-symbolic")
                .css_classes(if pinned {
                    ["flat", "circular", "accent", "row-action-btn"].as_slice()
                } else {
                    ["flat", "circular", "row-action-btn"].as_slice()
                })
                .valign(gtk::Align::Center)
                .tooltip_text(&tooltip)
                .build();
            c.append(&btn);
            btn
        });

        let trash_button = if show_trash {
            let btn = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .css_classes(["flat", "circular", "row-action-btn"])
                .valign(gtk::Align::Center)
                .tooltip_text("Remove from history")
                .build();
            c.append(&btn);
            Some(btn)
        } else {
            None
        };

        // Smoothly fade the pin/trash buttons in/out while the pointer hovers
        // this row, via a CSS class (opacity transition) rather than abruptly
        // toggling widget visibility.
        if pin_button.is_some() || trash_button.is_some() {
            let motion = gtk::EventControllerMotion::new();
            let pin_w = pin_button.clone();
            let trash_w = trash_button.clone();
            motion.connect_enter(move |_, _, _| {
                if let Some(b) = &pin_w {
                    b.add_css_class("row-action-visible");
                }
                if let Some(b) = &trash_w {
                    b.add_css_class("row-action-visible");
                }
            });
            let pin_w = pin_button.clone();
            let trash_w = trash_button.clone();
            motion.connect_leave(move |_| {
                if let Some(b) = &pin_w {
                    b.remove_css_class("row-action-visible");
                }
                if let Some(b) = &trash_w {
                    b.remove_css_class("row-action-visible");
                }
            });
            c.add_controller(motion);
        }

        Self {
            container: c,
            trash_button,
            pin_button,
        }
    }

    // A running-operation row: the operation title, live status text,
    // and a cancel/retry button.  The subtitle updates in place via
    // a self-cancelling timer so the results list doesn't rebuild
    // every tick (which would restart the row's fade-in animation).
    fn build_progress(r: &SearchResult) -> Self {
        // Render hints are carried in the action sentinel:
        //   "__op__\x1f<fraction>\x1f<state>\x1f<icon>\x1f<id>"
        let (fraction, state, icon_name, op_id) = parse_op_hints(&r.action);

        let c = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        c.add_css_class("op-row");

        // Progress orb (same design as the install orb in the bar).
        let ring_state = match state {
            OpRender::Running => crate::ui::circular_progress::RingState::Running,
            OpRender::Done => crate::ui::circular_progress::RingState::Done,
            OpRender::Failed => crate::ui::circular_progress::RingState::Failed,
            OpRender::Cancelled => crate::ui::circular_progress::RingState::Failed,
        };
        let ring = crate::ui::circular_progress::progress_ring(22, fraction, ring_state);
        c.append(&ring);

        // Keep the app's own icon through every state (running / done / failed);
        // the ring colour and status text convey completion, so the icon never
        // disappears or turns into a generic glyph.
        let icon = gtk::Image::builder().pixel_size(24).build();
        match &icon_name {
            Some(n) if is_app_id(n) => set_app_icon_or(&icon, n, "software-install-symbolic"),
            Some(n) if n.starts_with("pkg:") => {
                let rest = n.strip_prefix("pkg:").unwrap();
                let (name, fallback) = rest
                    .rsplit_once(':')
                    .unwrap_or((rest, "software-install-symbolic"));
                set_pkg_icon(&icon, name, fallback);
            }
            _ => icon.set_icon_name(Some("software-install-symbolic")),
        }

        c.append(&icon);

        let tb = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        tb.append(
            &gtk::Label::builder()
                .label(&r.title)
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(60)
                .build(),
        );

        let sub_label = gtk::Label::builder()
            .label(r.subtitle.as_deref().unwrap_or(""))
            .halign(gtk::Align::Start)
            .ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(80)
            .css_classes(["caption", "dim-label"])
            .build();
        tb.append(&sub_label);
        c.append(&tb);

        // While the operation is still running, offer a button to cancel it.
        if state == OpRender::Running {
            if let Some(id) = op_id {
                let cancel_btn = gtk::Button::builder()
                    .icon_name("process-stop-symbolic")
                    .css_classes(["flat", "circular"])
                    .valign(gtk::Align::Center)
                    .tooltip_text("Cancel")
                    .build();
                cancel_btn.connect_clicked(move |_| {
                    crate::operations::cancel(id);
                });
                c.append(&cancel_btn);
            }
        }

        // Once cancelled, offer a button to restart the operation.
        if state == OpRender::Cancelled {
            if let Some(id) = op_id {
                let redo_btn = gtk::Button::builder()
                    .icon_name("view-refresh-symbolic")
                    .css_classes(["flat", "circular"])
                    .valign(gtk::Align::Center)
                    .tooltip_text("Retry")
                    .build();
                redo_btn.connect_clicked(move |_| {
                    crate::operations::restart(id);
                });
                c.append(&redo_btn);
            }
        }

        // When indeterminate, pulse the whole row so the user knows something
        // is happening even though the ring is empty.  The timeout self-cancels
        // once the row is dropped (list rebuild on completion / cancellation).
        if state == OpRender::Running && fraction.is_none() {
            c.add_css_class("op-pulse");
        }

        // Self-updating timer: keep the subtitle and pulse class current
        // while the op runs, so the results list doesn't have to rebuild.
        if state == OpRender::Running {
            if let Some(id) = op_id {
                let lbl = sub_label.downgrade();
                let cw = c.downgrade();
                glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                    let (Some(lbl), Some(cw)) = (lbl.upgrade(), cw.upgrade()) else {
                        return glib::ControlFlow::Break;
                    };
                    match crate::operations::op_row_update(id) {
                        Some((text, indeterminate)) => {
                            if lbl.text().as_str() != text {
                                lbl.set_text(&text);
                            }
                            if indeterminate {
                                cw.add_css_class("op-pulse");
                            } else {
                                cw.remove_css_class("op-pulse");
                            }
                            glib::ControlFlow::Continue
                        }
                        None => glib::ControlFlow::Break,
                    }
                });
            }
        }

        Self {
            container: c,
            trash_button: None,
            pin_button: None,
        }
    }

    // The now-playing music row: cover art, title/artist + a source badge,
    // a progress bar, and transport controls (previous / play-pause / next /
    // stop). Hints are carried in the action sentinel:
    //   "__music__␟<frac>␟<state>␟<yt>␟<has_prev>␟<has_next>␟<cover_path>"
    fn build_music(r: &SearchResult) -> Self {
        let (frac, state, is_yt, has_prev, has_next, cover) = parse_music_hints(&r.action);

        let c = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        c.add_css_class("op-row");

        // Cover art (or a generic music glyph until it loads / for local files).
        let cover_img = gtk::Image::builder().pixel_size(44).build();
        match &cover {
            Some(path) if std::path::Path::new(path).exists() => {
                cover_img.set_from_file(Some(path));
            }
            _ => cover_img.set_icon_name(Some("audio-x-generic-symbolic")),
        }
        cover_img.add_css_class("music-cover");
        c.append(&cover_img);

        let tb = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();

        // Title + a small source badge (YouTube Music / Local file).
        let title_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        title_row.append(
            &gtk::Label::builder()
                .label(&r.title)
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(50)
                .build(),
        );
        let badge = gtk::Label::builder()
            .label(if is_yt { "YouTube Music" } else { "Local" })
            .css_classes(["caption"])
            .build();
        badge.add_css_class(if is_yt {
            "music-badge-yt"
        } else {
            "music-badge-local"
        });
        title_row.append(&badge);
        tb.append(&title_row);

        let bar = gtk::ProgressBar::builder().hexpand(true).build();
        bar.add_css_class("op-progressbar");
        bar.set_fraction(frac.clamp(0.0, 1.0));
        tb.append(&bar);

        let sub_label = gtk::Label::builder()
            .label(r.subtitle.as_deref().unwrap_or(""))
            .halign(gtk::Align::Start)
            .ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(80)
            .css_classes(["caption", "dim-label"])
            .build();
        tb.append(&sub_label);
        c.append(&tb);

        // Advance the progress bar + elapsed text smoothly without rebuilding
        // the whole results list. Self-cancels when the row is dropped.
        if state == "playing" {
            let bar_weak = bar.downgrade();
            let lbl_weak = sub_label.downgrade();
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                let (Some(bar), Some(lbl)) = (bar_weak.upgrade(), lbl_weak.upgrade()) else {
                    return gtk::glib::ControlFlow::Break;
                };
                match crate::music_operations::current() {
                    Some(op) => {
                        let f = if op.duration_ms > 0 {
                            (op.elapsed_ms as f64 / op.duration_ms as f64).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        bar.set_fraction(f);
                        if let Some(t) = op.current_track() {
                            let artist = if t.artist.is_empty() {
                                t.source_label().to_string()
                            } else {
                                t.artist.clone()
                            };
                            lbl.set_label(&format!(
                                "{} · {} / {} · {}",
                                artist,
                                fmt_dur(op.elapsed_ms),
                                fmt_dur(op.duration_ms),
                                t.source_label(),
                            ));
                        }
                        gtk::glib::ControlFlow::Continue
                    }
                    None => gtk::glib::ControlFlow::Break,
                }
            });
        }

        // Transport controls.
        let controls = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .valign(gtk::Align::Center)
            .build();

        let prev_btn = transport_button("media-skip-backward-symbolic", "Previous");
        prev_btn.set_sensitive(has_prev);
        prev_btn.connect_clicked(|_| crate::music_operations::previous());
        controls.append(&prev_btn);

        let (play_icon, play_tip) = if state == "playing" {
            ("media-playback-pause-symbolic", "Pause")
        } else {
            ("media-playback-start-symbolic", "Play")
        };
        let play_btn = transport_button(play_icon, play_tip);
        play_btn.connect_clicked(|_| crate::music_operations::toggle());
        controls.append(&play_btn);

        let next_btn = transport_button("media-skip-forward-symbolic", "Next");
        next_btn.set_sensitive(has_next);
        next_btn.connect_clicked(|_| crate::music_operations::next());
        controls.append(&next_btn);

        let stop_btn = transport_button("media-playback-stop-symbolic", "Stop");
        // Dismiss (pause + hide) rather than fully stop, so it can be resumed
        // from where it left off via Ctrl+Z or the Operations undo bar.
        stop_btn.connect_clicked(|_| crate::music_operations::dismiss(1.0));
        controls.append(&stop_btn);

        c.append(&controls);

        Self {
            container: c,
            trash_button: None,
            pin_button: None,
        }
    }

    /// Expanded dictionary card: word title + wrapped multi-line definition
    /// (the dict trigger's live lookup), on a soft accent background.
    fn build_definition(r: &SearchResult) -> Self {
        let c = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        c.add_css_class("spotty-dict-row");

        let icon = gtk::Image::from_icon_name("accessories-dictionary-symbolic");
        icon.set_pixel_size(24);
        c.append(&icon);

        let tb = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        tb.append(
            &gtk::Label::builder()
                .label(&r.title)
                .halign(gtk::Align::Start)
                .ellipsize(pango::EllipsizeMode::End)
                .build(),
        );
        let sub = gtk::Label::builder()
            .label(r.subtitle.as_deref().unwrap_or(""))
            .halign(gtk::Align::Start)
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build();
        tb.append(&sub);
        c.append(&tb);

        Self {
            container: c,
            trash_button: None,
            pin_button: None,
        }
    }
}

fn fmt_dur(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn transport_button(icon: &str, tip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .tooltip_text(tip)
        .build()
}

// Parse "__music__␟<frac>␟<state>␟<yt>␟<has_prev>␟<has_next>␟<cover_path>".
fn parse_music_hints(
    action: &crate::search::Action,
) -> (f64, String, bool, bool, bool, Option<String>) {
    if let crate::search::Action::EnterMode(s) = action {
        let mut p = s.split('\u{1f}');
        let _tag = p.next();
        let frac = p.next().and_then(|f| f.parse::<f64>().ok()).unwrap_or(0.0);
        let state = p.next().unwrap_or("stopped").to_string();
        let is_yt = p.next() == Some("1");
        let has_prev = p.next() == Some("1");
        let has_next = p.next() == Some("1");
        let cover = p.next().filter(|s| !s.is_empty()).map(|s| s.to_string());
        return (frac, state, is_yt, has_prev, has_next, cover);
    }
    (0.0, "stopped".to_string(), false, false, false, None)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpRender {
    Running,
    Done,
    Failed,
    Cancelled,
}

// Parse the operation render hints out of the action sentinel.
fn parse_op_hints(
    action: &crate::search::Action,
) -> (Option<f64>, OpRender, Option<String>, Option<u64>) {
    if let crate::search::Action::EnterMode(s) = action {
        let mut parts = s.split('\u{1f}');
        let _tag = parts.next();
        let fraction = parts.next().and_then(|f| f.parse::<f64>().ok());
        let state = match parts.next() {
            Some("done") => OpRender::Done,
            Some("failed") => OpRender::Failed,
            Some("cancelled") => OpRender::Cancelled,
            _ => OpRender::Running,
        };
        let icon = parts
            .next()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let id = parts.next().and_then(|s| s.parse::<u64>().ok());
        return (fraction, state, icon, id);
    }
    (None, OpRender::Running, None, None)
}

// Looks like a reverse-DNS flatpak app-id (e.g. "org.mozilla.firefox"), not a
// freedesktop symbolic icon name.
fn is_text_match_markup(text: &str) -> bool {
    text.starts_with("Text match: ") && text.contains("<b>")
}

fn is_app_id(name: &str) -> bool {
    name.contains('.') && !name.ends_with("-symbolic") && !name.contains('/')
}

/// Resolve an Operations-popover row icon from its stored `name`, robustly:
/// an absolute path is loaded directly; an app-id or `pkg:`-prefixed package is
/// resolved against the icon theme and host appstream (with an async fallback so
/// a freshly-installed app's icon appears once available); anything else is
/// treated as a plain themed icon name. Falls back to a generic package glyph
/// rather than showing nothing.
pub fn set_op_row_icon(icon: &gtk::Image, name: &str) {
    const FALLBACK: &str = "package-x-generic-symbolic";
    if name.is_empty() {
        icon.set_icon_name(Some(FALLBACK));
    } else if std::path::Path::new(name).is_absolute() {
        icon.set_from_file(Some(name));
    } else if let Some(rest) = name.strip_prefix("pkg:") {
        let (n, fb) = rest.rsplit_once(':').unwrap_or((rest, FALLBACK));
        set_pkg_icon(icon, n, fb);
    } else if is_app_id(name) {
        set_app_icon_or(icon, name, FALLBACK);
    } else {
        icon.set_icon_name(Some(name));
    }
}

// Resolve `name` as an app icon (theme, then host appstream), using `fallback`
// if nothing is found rather than a broken/missing icon.
fn set_app_icon_or(icon: &gtk::Image, name: &str, fallback: &str) {
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        if theme.has_icon(name) {
            icon.set_icon_name(Some(name));
            return;
        }
    }
    icon.set_icon_name(Some(fallback));
    resolve_host_app_icon_async(icon, name);
}

// Build a list of icon-name candidates for a distro package: the full name,
// then progressively shorter dash-separated prefixes (e.g.
// "firefox-langpacks-en-us" -> "firefox-langpacks-en" -> ... -> "firefox"),
// since subpackages rarely ship their own icon but the base package usually does.
fn pkg_icon_candidates(name: &str) -> Vec<String> {
    let mut out = vec![name.to_string()];
    let parts: Vec<&str> = name.split('-').collect();
    for i in (1..parts.len()).rev() {
        out.push(parts[..i].join("-"));
    }
    out.dedup();
    out
}

// Like `set_app_icon_or`, but for distro packages: tries each candidate name
// (see `pkg_icon_candidates`) against the icon theme, then against the host
// icon search, before falling back to `fallback`.
fn set_pkg_icon(icon: &gtk::Image, name: &str, fallback: &str) {
    let candidates = pkg_icon_candidates(name);
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        for c in &candidates {
            if theme.has_icon(c) {
                icon.set_icon_name(Some(c));
                return;
            }
        }
    }
    icon.set_icon_name(Some(fallback));
    resolve_host_app_icon_async_multi(icon, candidates);
}

// Like `resolve_host_app_icon_async`, but tries each candidate name in order
// on the background thread and applies the first one that resolves. Memoized by
// the candidate list (same rationale as `resolve_host_app_icon_async`).
fn resolve_host_app_icon_async_multi(icon: &gtk::Image, names: Vec<String>) {
    let key = format!("multi:{}", names.join(","));
    {
        let mut cache = host_icon_cache().lock().unwrap();
        match cache.get(&key) {
            Some(IconSlot::Done(Some(path))) => {
                icon.set_from_file(Some(path));
                return;
            }
            Some(IconSlot::Done(None)) | Some(IconSlot::Pending) => return,
            None => {
                cache.insert(key.clone(), IconSlot::Pending);
            }
        }
    }
    let icon = icon.clone();
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let mut found = None;
        for name in &names {
            if let Some(path) = resolve_host_app_icon(name) {
                found = Some(path);
                break;
            }
        }
        let _ = tx.send(found);
    });
    glib::MainContext::default().spawn_local(async move {
        let result = rx.await.ok().flatten();
        host_icon_cache()
            .lock()
            .unwrap()
            .insert(key, IconSlot::Done(result.clone()));
        if let Some(path) = result {
            icon.set_from_file(Some(&path));
        }
        glib::idle_add_local_once(crate::app::refresh_search_window);
    });
}

// Fetch and cache a search engine's favicon (via Google's favicon service,
// which works for any domain including user-entered custom search engines),
// then apply it to `icon` once downloaded.
fn set_favicon(icon: &gtk::Image, domain: &str) {
    let domain = domain.to_string();
    let icon = icon.clone();
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(resolve_favicon(&domain));
    });
    glib::MainContext::default().spawn_local(async move {
        if let Ok(Some(path)) = rx.await {
            icon.set_from_file(Some(&path));
        }
    });
}

fn resolve_favicon(domain: &str) -> Option<String> {
    let cache_dir = dirs::cache_dir()?.join("spotty/favicons");
    let _ = std::fs::create_dir_all(&cache_dir);
    let hash = crate::md5::hex(domain.as_bytes());
    for ext in ["png", "jpg", "svg"] {
        let out = cache_dir.join(format!("{}.{}", hash, ext));
        if out.exists() {
            return Some(out.to_string_lossy().to_string());
        }
    }

    // Prefer the site's own high-resolution apple-touch-icon (typically
    // 180x180, transparent PNG) when present, then DuckDuckGo's icon
    // service, then the domain's own favicon.ico, then Google's favicon
    // service at a larger size (which almost always returns *something*,
    // even for completely unknown/custom domains, so the chain ends with a
    // real icon rather than a blank one).
    let candidates = [
        format!("https://{}/apple-touch-icon.png", domain),
        format!("https://{}/apple-touch-icon-precomposed.png", domain),
        format!("https://icons.duckduckgo.com/ip3/{}.ico", domain),
        format!("https://{}/favicon.ico", domain),
        format!(
            "https://www.google.com/s2/favicons?sz=256&domain={}",
            domain
        ),
    ];
    for url in candidates {
        let bytes = std::process::Command::new("flatpak-spawn")
            .args([
                "--host",
                "curl",
                "-fsSL",
                "--max-time",
                "5",
                "-A",
                "Mozilla/5.0 (compatible; Spotty)",
                &url,
            ])
            .output();
        let bytes = match bytes {
            Ok(b) => b,
            Err(_) => continue,
        };
        if !bytes.status.success() || bytes.stdout.len() <= 100 {
            continue;
        }
        // Identify the actual image format by magic bytes rather than
        // trusting the URL: DuckDuckGo's "icon.ico" endpoint sometimes
        // returns a real multi-res Windows .ico, which gdk-pixbuf can't
        // load — skip those and fall through to a format GTK can render.
        let data = &bytes.stdout;
        let ext = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            "png"
        } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "jpg"
        } else if data.starts_with(b"<svg") || data.starts_with(b"<?xml") {
            "svg"
        } else {
            continue;
        };
        let out = cache_dir.join(format!("{}.{}", hash, ext));
        if std::fs::write(&out, data).is_ok() {
            return Some(out.to_string_lossy().to_string());
        }
    }
    None
}

fn set_app_icon(icon: &gtk::Image, name: &str) {
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        if theme.has_icon(name) {
            icon.set_icon_name(Some(name));
            return;
        }
    }

    icon.set_icon_name(Some("application-x-executable-symbolic"));
    resolve_host_app_icon_async(icon, name);
}

// Resolve a host-side app icon on a background thread (it shells out via
// `flatpak-spawn` to search and copy the icon, which is too slow to run on
// the UI thread for every result row on every keystroke), then apply it to
// `icon` once found.
// In-memory memo of host-icon lookups, keyed by icon name. Without it, the
// per-name `flatpak-spawn --host find` (and the thread spawn around it) would
// re-run on every list/popover rebuild — and those rebuild several times a
// second during an install, which would be a thread + host-command storm.
enum IconSlot {
    /// A lookup is in flight; don't start another.
    Pending,
    /// Lookup finished: resolved path, or `None` if the icon wasn't found.
    Done(Option<String>),
}

fn host_icon_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, IconSlot>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, IconSlot>>> =
        std::sync::OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn resolve_host_app_icon_async(icon: &gtk::Image, name: &str) {
    let name = name.to_string();
    {
        let mut cache = host_icon_cache().lock().unwrap();
        match cache.get(&name) {
            Some(IconSlot::Done(Some(path))) => {
                icon.set_from_file(Some(path));
                return;
            }
            // Known-absent or already resolving: nothing more to do.
            Some(IconSlot::Done(None)) | Some(IconSlot::Pending) => return,
            None => {
                cache.insert(name.clone(), IconSlot::Pending);
            }
        }
    }
    let icon = icon.clone();
    let (tx, rx) = futures::channel::oneshot::channel();
    let lookup = name.clone();
    std::thread::spawn(move || {
        let _ = tx.send(resolve_host_app_icon(&lookup));
    });
    glib::MainContext::default().spawn_local(async move {
        let result = rx.await.ok().flatten();
        host_icon_cache()
            .lock()
            .unwrap()
            .insert(name, IconSlot::Done(result.clone()));
        if let Some(path) = result {
            icon.set_from_file(Some(&path));
        }
        glib::idle_add_local_once(crate::app::refresh_search_window);
    });
}

fn resolve_host_app_icon(name: &str) -> Option<String> {
    let cache_dir = dirs::cache_dir()?.join("spotty/host-icons");
    let _ = std::fs::create_dir_all(&cache_dir);
    let hash = crate::md5::hex(name.as_bytes());
    if let Ok(rd) = std::fs::read_dir(&cache_dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            let stem = path.file_stem().and_then(|s| s.to_str());
            if stem == Some(hash.as_str()) {
                return Some(path.to_string_lossy().to_string());
            }
        }
    }

    let escaped = name.replace('\'', "'\\''");
    let script = format!(
        "name='{name}'; \
         for d in \"$HOME/.local/share/icons\" \"$HOME/.local/share/flatpak/appstream\" \"$HOME/.local/share/flatpak/app\" /usr/share/icons /usr/local/share/icons /usr/share/pixmaps; do \
           [ -d \"$d\" ] || continue; \
           find \"$d\" -type f \\( -iname \"$name.png\" -o -iname \"$name.svg\" -o -iname \"$name.xpm\" -o -iname \"$name-symbolic.png\" -o -iname \"$name-symbolic.svg\" \\) -print -quit; \
         done",
        name = escaped
    );
    let found = if crate::app::is_flatpak() {
        std::process::Command::new("flatpak-spawn")
            .args(["--host", "sh", "-lc", &script])
            .output()
    } else {
        std::process::Command::new("sh")
            .args(["-lc", &script])
            .output()
    }
    .ok()?;
    if !found.status.success() {
        return None;
    }
    let host_path = String::from_utf8_lossy(&found.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .to_string();
    let ext = std::path::Path::new(&host_path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("png");
    let out = cache_dir.join(format!("{}.{}", hash, ext));
    let file_bytes: Vec<u8> = if crate::app::is_flatpak() {
        let output = std::process::Command::new("flatpak-spawn")
            .args(["--host", "cat", &host_path])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        output.stdout
    } else {
        std::fs::read(&host_path).ok()?
    };
    std::fs::write(&out, &file_bytes).ok()?;
    Some(out.to_string_lossy().to_string())
}
