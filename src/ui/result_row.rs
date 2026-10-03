use crate::search::{ResultKind, SearchResult};
use crate::i18n::gettext;
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
                } else if let Some(data) = n.strip_prefix("engine-icon-data:") {
                    // The icon the *browser* keeps for its search engine — a
                    // data: URI it read out of its own settings, so no
                    // request is made and no favicon service is involved.
                    icon.set_icon_name(Some("web-browser-symbolic"));
                    set_engine_icon_data(&icon, data);
                } else if let Some(url) = n.strip_prefix("engine-icon:") {
                    // A remote icon the browser recorded for its engine: fetch
                    // that exact icon first, then fall back to the site's own.
                    icon.set_icon_name(Some("web-browser-symbolic"));
                    set_engine_icon_url(&icon, url);
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
                    // Names the active theme doesn't know (legacy Adwaita
                    // icons such as system-software-update or application-pdf)
                    // render as an empty square — fall back to the kind's
                    // generic icon instead.
                    set_icon_name_or(&icon, n, generic_icon_for(r.kind));
                }
            } else {
                icon.set_icon_name(Some(generic_icon_for(r.kind)));
            }
            // Request an overview thumbnail for document files.
            if let Some(path) = match &r.action {
                crate::search::Action::OpenPath(p) => Some(p),
                crate::search::Action::BrowseInto(p) => Some(p),
                _ => None,
            } {
                crate::thumbnails::request(&icon, path);
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
                .tooltip_text(gettext("Remove from history"))
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
            Some(n) if is_app_id(n) => set_app_icon_or(&icon, n, "system-software-install-symbolic"),
            Some(n) if n.starts_with("pkg:") => {
                let rest = n.strip_prefix("pkg:").unwrap();
                let (name, fallback) = rest
                    .rsplit_once(':')
                    .unwrap_or((rest, "system-software-install-symbolic"));
                set_pkg_icon(&icon, name, fallback);
            }
            _ => icon.set_icon_name(Some("system-software-install-symbolic")),
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
                    .tooltip_text(gettext("Cancel"))
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
                    .tooltip_text(gettext("Retry"))
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

/// Generic icon per result kind — used when a row carries no icon, and as
/// the fallback when the active icon theme doesn't know a row's icon name.
fn generic_icon_for(kind: ResultKind) -> &'static str {
    match kind {
        ResultKind::App => "application-x-executable-symbolic",
        ResultKind::File | ResultKind::Calculator => "text-x-generic-symbolic",
        ResultKind::Folder => "folder-symbolic",
        ResultKind::Web => "web-browser-symbolic",
        ResultKind::Clipboard => "edit-paste-symbolic",
        ResultKind::System => "system-shutdown-symbolic",
        ResultKind::Emoji => "face-smile-symbolic",
        ResultKind::Translate => "tools-check-spelling-symbolic",
    }
}

/// Set a plain themed icon, falling back to `fallback` when the active theme
/// has no such icon — legacy names would otherwise render as an empty square.
fn set_icon_name_or(icon: &gtk::Image, name: &str, fallback: &str) {
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        if theme.has_icon(name) {
            icon.set_icon_name(Some(name));
            return;
        }
    }
    icon.set_icon_name(Some(fallback));
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
        set_icon_name_or(icon, name, FALLBACK);
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

/// Apply an icon the browser keeps inline as a `data:` URI — no request at all.
fn set_engine_icon_data(icon: &gtk::Image, data_uri: &str) {
    let data_uri = data_uri.to_string();
    let icon = icon.clone();
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(cache_data_uri(&data_uri));
    });
    glib::MainContext::default().spawn_local(async move {
        if let Ok(Some(path)) = rx.await {
            icon.set_from_file(Some(&path));
        }
    });
}

/// Apply the icon URL the browser recorded for its engine, falling back to the
/// search site's own icons when that URL is gone.
fn set_engine_icon_url(icon: &gtk::Image, url: &str) {
    let url = url.to_string();
    let icon = icon.clone();
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(resolve_engine_icon_url(&url));
    });
    glib::MainContext::default().spawn_local(async move {
        if let Ok(Some(path)) = rx.await {
            icon.set_from_file(Some(&path));
        }
    });
}

/// Show a search engine's icon on a plain widget (the Settings row), using the
/// browser's own icon when there is one and the search site's favicon
/// otherwise. `image` starts on `fallback_icon_name` until an icon arrives.
pub(crate) fn set_search_engine_icon(
    image: &gtk::Image,
    icon: Option<crate::search::browser_engine::EngineIcon>,
    fallback_domain: Option<&str>,
    fallback_icon_name: &str,
) {
    use crate::search::browser_engine::EngineIcon;
    match icon {
        Some(EngineIcon::Data(uri)) => set_engine_icon_data(image, &uri),
        Some(EngineIcon::Url(url)) => set_engine_icon_url(image, &url),
        None => match fallback_domain {
            Some(domain) => {
                image.set_icon_name(Some(fallback_icon_name));
                set_favicon(image, domain);
            }
            None => image.set_icon_name(Some(fallback_icon_name)),
        },
    }
}

fn resolve_favicon(domain: &str) -> Option<String> {
    let candidates: Vec<String> = vec![
        format!("https://{}/apple-touch-icon.png", domain),
        format!("https://{}/apple-touch-icon-precomposed.png", domain),
        format!("https://icons.duckduckgo.com/ip3/{}.ico", domain),
        format!("https://{}/favicon.ico", domain),
        format!(
            "https://www.google.com/s2/favicons?sz=256&domain={}",
            domain
        ),
    ];
    fetch_cached_icon(domain, &candidates)
}

/// The engine's own icon URL first, then the search site's icons — the URL
/// lives in the browser's settings and may well be dead by now.
fn resolve_engine_icon_url(url: &str) -> Option<String> {
    let mut candidates = vec![url.to_string()];
    if let Some(domain) = host_of(url) {
        candidates.push(format!("https://{}/apple-touch-icon.png", domain));
        candidates.push(format!("https://icons.duckduckgo.com/ip3/{}.ico", domain));
    }
    fetch_cached_icon(url, &candidates)
}

/// Write a `data:` URI into the icon cache and return the file path.
///
/// Firefox keeps every engine's icon inline (`iconMapObj`), so this is the
/// normal case there — and it means the icon works offline and is exactly the
/// one the browser shows.
fn cache_data_uri(data_uri: &str) -> Option<String> {
    let (meta, payload) = data_uri.split_once(',')?;
    if !meta.starts_with("data:image/") {
        return None;
    }
    let is_base64 = meta.ends_with(";base64");
    let bytes = if is_base64 {
        base64_decode(payload)?
    } else {
        payload.as_bytes().to_vec()
    };
    let cache_dir = dirs::cache_dir()?.join("spotty/favicons");
    let _ = std::fs::create_dir_all(&cache_dir);
    let hash = crate::md5::hex(data_uri.as_bytes());
    let ext = image_extension(&bytes)?;
    let out = cache_dir.join(format!("{hash}.{ext}"));
    std::fs::write(&out, &bytes).ok()?;
    Some(out.to_string_lossy().to_string())
}

/// Look in the cache, then try each candidate URL in turn, storing the first
/// image that turns out to be one gdk-pixbuf can actually load.
fn fetch_cached_icon(key: &str, candidates: &[String]) -> Option<String> {
    let cache_dir = dirs::cache_dir()?.join("spotty/favicons");
    let _ = std::fs::create_dir_all(&cache_dir);
    let hash = crate::md5::hex(key.as_bytes());
    for ext in ["png", "jpg", "svg"] {
        let out = cache_dir.join(format!("{hash}.{ext}"));
        if out.exists() {
            return Some(out.to_string_lossy().to_string());
        }
    }

    for url in candidates {
        let Ok(bytes) = host_curl(url) else { continue };
        if !bytes.status.success() || bytes.stdout.len() <= 100 {
            continue;
        }
        // Identify the actual image format by magic bytes rather than
        // trusting the URL: DuckDuckGo's "icon.ico" endpoint sometimes
        // returns a real multi-res Windows .ico, which gdk-pixbuf can't
        // load — skip those and fall through to a format GTK can render.
        let Some(ext) = image_extension(&bytes.stdout) else {
            continue;
        };
        let out = cache_dir.join(format!("{hash}.{ext}"));
        if std::fs::write(&out, &bytes.stdout).is_ok() {
            return Some(out.to_string_lossy().to_string());
        }
    }
    None
}

/// curl on the host — inside the sandbox the network is the host's, so the
/// request has to be made there.
fn host_curl(url: &str) -> std::io::Result<std::process::Output> {
    let mut cmd = std::process::Command::new("curl");
    if crate::search::run::is_sandbox() {
        cmd = std::process::Command::new("flatpak-spawn");
        cmd.args(["--host", "curl"]);
    }
    cmd.args([
        "-fsSL",
        "--max-time",
        "5",
        "-A",
        "Mozilla/5.0 (compatible; Spotty)",
        url,
    ])
    .output()
}

/// The image format from its magic bytes, or `None` for something gdk-pixbuf
/// won't load (notably Windows .ico).
fn image_extension(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if data.starts_with(b"<svg") || data.starts_with(b"<?xml") {
        Some("svg")
    } else {
        None
    }
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    (!host.is_empty()).then(|| host.to_string())
}

/// Standard base64 decode (what the browser's `data:` URIs use).
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in input.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let value = TABLE.iter().position(|t| *t == c)? as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
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

/// The host-side lookup script: find a file named after `name` (any common
/// image extension) under the user's icon dirs, both Flatpak appstream
/// caches, and the system icon dirs. The `/var/lib/flatpak` locations matter
/// for *not-installed* catalog apps: system-scope Flatpak apps keep their
/// appstream icons there (regular files) and their exports as symlinks —
/// hence `-type l` alongside `-type f`.
fn host_icon_lookup_script(name: &str) -> String {
    let escaped = name.replace('\'', "'\\''");
    format!(
        "name='{name}'; \
         for d in \"$HOME/.local/share/icons\" \"$HOME/.local/share/flatpak/appstream\" \"$HOME/.local/share/flatpak/app\" \"$HOME/.local/share/flatpak/exports/share/icons\" /var/lib/flatpak/appstream /var/lib/flatpak/exports/share/icons /var/lib/flatpak/app /usr/share/icons /usr/local/share/icons /usr/share/pixmaps; do \
           [ -d \"$d\" ] || continue; \
           find \"$d\" \\( -type f -o -type l \\) \\( -iname \"$name.png\" -o -iname \"$name.svg\" -o -iname \"$name.xpm\" -o -iname \"$name-symbolic.png\" -o -iname \"$name-symbolic.svg\" \\) -print -quit; \
         done",
        name = escaped
    )
}

/// Flathub's appstream CDN, biggest size first: the repo publishes a
/// 128x128 (and 64x64) PNG per published app id.
fn flathub_icon_urls(app_id: &str) -> [String; 2] {
    [
        format!(
            "https://dl.flathub.org/repo/appstream/x86_64/icons/128x128/{app_id}.png"
        ),
        format!(
            "https://dl.flathub.org/repo/appstream/x86_64/icons/64x64/{app_id}.png"
        ),
    ]
}

/// Last resort for an app-id shaped icon: when nothing local matches, the
/// icon may simply not be exported on this machine — Flathub serves it from
/// its appstream CDN (downloaded once, then cached in `host-icons` like any
/// locally found icon). Non-app-id names and 404s (CLI-only packages,
/// BaseApps) return `None` and keep the row's generic icon.
fn flathub_icon_fallback(
    name: &str,
    cache_dir: &std::path::Path,
    hash: &str,
) -> Option<String> {
    if !is_app_id(name) {
        log::debug!("icon: nothing local for {name}");
        return None;
    }
    for url in flathub_icon_urls(name) {
        let mut cmd = if crate::app::is_flatpak() {
            let mut c = std::process::Command::new("flatpak-spawn");
            c.args(["--host", "curl"]);
            c
        } else {
            std::process::Command::new("curl")
        };
        let output = cmd
            .args([
                "-fsSL",
                "--max-time",
                "5",
                "-A",
                "Mozilla/5.0 (compatible; Spotty)",
                &url,
            ])
            .output();
        let Ok(out) = output else {
            continue;
        };
        if !out.status.success() || out.stdout.len() <= 100 {
            continue;
        }
        // Trust magic bytes, not the URL — same rule as the favicon fetch.
        let ext = if out.stdout.starts_with(b"\x89PNG\r\n\x1a\n") {
            "png"
        } else if out.stdout.starts_with(b"<svg") || out.stdout.starts_with(b"<?xml") {
            "svg"
        } else {
            continue;
        };
        let path = cache_dir.join(format!("{}.{}", hash, ext));
        if std::fs::write(&path, &out.stdout).is_ok() {
            return Some(path.to_string_lossy().to_string());
        }
    }
    // Local search and the Flathub CDN both missed — the row keeps the
    // generic icon; log it so "icon missing" reports name the app id.
    log::debug!("icon: no app icon for {name} (local + Flathub CDN miss)");
    None
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

    let script = host_icon_lookup_script(name);
    let found = if crate::app::is_flatpak() {
        std::process::Command::new("flatpak-spawn")
            .args(["--host", "sh", "-lc", &script])
            .output()
            .ok()
    } else {
        std::process::Command::new("sh")
            .args(["-lc", &script])
            .output()
            .ok()
    };
    let host_path = found.and_then(|out| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)
    });
    let Some(host_path) = host_path else {
        // Nothing local — a Flatpak app id's icon may still be published on
        // Flathub's appstream CDN (one download, then cached like the rest).
        return flathub_icon_fallback(name, &cache_dir, &hash);
    };
    let ext = std::path::Path::new(&host_path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("png");
    let out = cache_dir.join(format!("{}.{}", hash, ext));
    let file_bytes: Vec<u8> = if crate::app::is_flatpak() {
        match std::process::Command::new("flatpak-spawn")
            .args(["--host", "cat", &host_path])
            .output()
        {
            Ok(o) if o.status.success() => o.stdout,
            _ => return flathub_icon_fallback(name, &cache_dir, &hash),
        }
    } else {
        match std::fs::read(&host_path) {
            Ok(b) => b,
            Err(_) => return flathub_icon_fallback(name, &cache_dir, &hash),
        }
    };
    if std::fs::write(&out, &file_bytes).is_err() {
        return flathub_icon_fallback(name, &cache_dir, &hash);
    }
    Some(out.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_data_uris_browsers_store_icons_as() {
        // What Firefox keeps in `iconMapObj` and Chromium in `favicon_url`.
        assert_eq!(
            base64_decode("aGVsbG8=").as_deref(),
            Some(&b"hello"[..]),
            "padded base64"
        );
        assert_eq!(
            base64_decode("aGVsbG8").as_deref(),
            Some(&b"hello"[..]),
            "unpadded, as some browsers write it"
        );
        // A 1x1 PNG is what a browser's inline icon usually decodes to.
        let png = base64_decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
        )
        .expect("a PNG");
        assert_eq!(image_extension(&png), Some("png"));
        // A data: URI is only accepted when it really carries an image…
        assert!(cache_data_uri("text/plain;base64,aGVsbG8=").is_none());
        // …and its bytes have to be something gdk-pixbuf can load.
        assert_eq!(
            image_extension(&[0x00, 0x00, 0x01, 0x00]),
            None,
            "a Windows .ico is not renderable here"
        );
    }

    #[test]
    fn engine_icon_urls_and_hosts_resolve_sensibly() {
        assert_eq!(host_of("https://kagi.com/favicon.ico").as_deref(), Some("kagi.com"));
        assert_eq!(host_of("kagi.com/x").as_deref(), Some("kagi.com"));
        assert_eq!(host_of(""), None);
    }

    #[test]
    fn host_icon_lookup_covers_the_system_flatpak_dirs() {
        let script = host_icon_lookup_script("org.kde.kdiff3");
        // System-scope Flatpak apps keep their appstream icons and their
        // exports under /var/lib/flatpak — the search used to miss both,
        // which is why catalog rows showed the generic package icon.
        assert!(script.contains("/var/lib/flatpak/appstream"), "{script}");
        assert!(script.contains("/var/lib/flatpak/exports/share/icons"), "{script}");
        // The exports dir is full of symlinks into /var/lib/flatpak/app,
        // which the old `-type f` predicate skipped.
        assert!(script.contains("-type l"), "{script}");
        // The app id lands in the script's shell variable (after escaping);
        // `$name` expands to it inside `sh` at runtime.
        assert!(script.contains("name='org.kde.kdiff3'"), "{script}");
        assert!(script.contains("$name.png"), "{script}");
    }

    #[test]
    fn flathub_cdn_urls_cover_both_appstream_sizes() {
        let [big, small] = flathub_icon_urls("org.kde.krita");
        assert_eq!(
            big,
            "https://dl.flathub.org/repo/appstream/x86_64/icons/128x128/org.kde.krita.png"
        );
        assert_eq!(
            small,
            "https://dl.flathub.org/repo/appstream/x86_64/icons/64x64/org.kde.krita.png"
        );
        // Not an app id → no CDN attempt (falls straight to the generic icon).
        assert!(!is_app_id("firefox-langpacks-en-us"));
    }
}
