use crate::config::{CommandKeyword, Config, SearchEngine};
use crate::i18n::gettext;
use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

#[path = "../../trigger-backends/src/ui/feature_settings.rs"]
mod feature_settings;
use feature_settings::open_result_dialog;

pub struct SettingsWindow {
    window: adw::PreferencesWindow,
    refresh_triggers: Rc<dyn Fn()>,
}

impl SettingsWindow {
    pub fn new(app: &adw::Application, config: Rc<RefCell<Config>>) -> Self {
        let window = adw::PreferencesWindow::builder()
            .application(app)
            .title(gettext("Spotty Settings"))
            .default_width(640)
            .default_height(720)
            .modal(false)
            .hide_on_close(true)
            .search_enabled(true)
            .build();

        build_general_page(&window, &config);
        let refresh_triggers = build_keywords_page(&window, &config);

        Self {
            window,
            refresh_triggers,
        }
    }
    pub fn present(&self) {
        // Rebuild the trigger list so triggers installed since the window was
        // first opened (trigger imports) appear without re-opening.
        (self.refresh_triggers)();
        refresh_service_row();
        self.window.present();
        // On Wayland, present() on an already-visible window doesn't reliably
        // grab keyboard focus — grab it on the next main-loop iteration.
        let w = self.window.clone();
        glib::idle_add_local_once(move || {
            w.grab_focus();
        });
    }
}

/// Persist a settings change, then run `after`. The ordering is the fix:
/// the search-window refresh re-borrows this same `RefCell`, so running it
/// while a `borrow_mut` is still alive panics with "already mutably
/// borrowed" — that is what crashed the window when a switch was toggled.
fn save_then<R>(
    cfg: &Rc<RefCell<Config>>,
    f: impl FnOnce(&mut Config),
    after: impl FnOnce() -> R,
) -> R {
    {
        let mut c = cfg.borrow_mut();
        f(&mut c);
        c.save();
    }
    after()
}

/// Persist a settings change and refresh the search window (badge, rows).
fn save_and_refresh(cfg: &Rc<RefCell<Config>>, f: impl FnOnce(&mut Config)) {
    save_then(cfg, f, crate::app::refresh_search_window);
}

// ──────────────────────────────────────────────────────────────────────
// General page
// ──────────────────────────────────────────────────────────────────────
fn build_general_page(window: &adw::PreferencesWindow, config: &Rc<RefCell<Config>>) {
    let general = adw::PreferencesPage::builder()
        .title(gettext("General"))
        .icon_name("preferences-system-symbolic")
        .build();
    window.add(&general);

    // Launcher: the one shortcut that opens Spotty. (The result types and
    // triggers live on the Search page.)
    {
        let lg = adw::PreferencesGroup::builder()
            .title(gettext("Launcher"))
            .build();
        let pending = Rc::new(RefCell::new(config.borrow().shortcut.clone()));
        let (row, _) = {
            let cfg = config.clone();
            let pending_c = pending.clone();
            capture_shortcut_row(
                window,
                &gettext("Open Spotty"),
                "",
                pending.clone(),
                Rc::new(move || {
                    let value = pending_c.borrow().clone();
                    save_and_refresh(&cfg, |c| c.shortcut = value.clone());
                    std::thread::spawn(crate::keybindings::register_all);
                }),
                &Config::default().shortcut,
            )
        };
        row.set_subtitle(&gettext("Shows or hides the search window from anywhere"));
        lg.add(&row);
        general.add(&lg);
    }

    {
        let group = adw::PreferencesGroup::builder().title(gettext("Privacy")).build();
        let history = adw::SwitchRow::builder()
            .title(gettext("Keep search history"))
            .subtitle(gettext("Save queries and selected results locally to improve ranking. Existing history is kept when turned off."))
            .active(config.borrow().save_search_history).build();
        let cfg = config.clone();
        history.connect_active_notify(move |row| {
            save_and_refresh(&cfg, |c| c.save_search_history = row.is_active());
        });
        group.add(&history);
        let icons = adw::SwitchRow::builder()
            .title(gettext("Download missing icons"))
            .subtitle(gettext("Contact search websites and Flathub for icons. Cached and local icons work offline."))
            .active(config.borrow().allow_network_icons).build();
        let cfg = config.clone();
        icons.connect_active_notify(move |row| {
            save_and_refresh(&cfg, |c| c.allow_network_icons = row.is_active());
        });
        group.add(&icons);
        general.add(&group);
    }

    // Footer bar: the two buttons at the bottom of the search window. The
    // switches only hide their key indicators — the buttons stay clickable and
    // the shortcuts keep working, so this is purely about the space they take.
    {
        let fg = adw::PreferencesGroup::builder()
            .title(gettext("Search Window"))
            .description(gettext("The Operations and Hints buttons at the bottom"))
            .build();
        // One switch for both icons; the two below are per-button, because a
        // button is only worth keeping if it has something left to show.
        {
            let sw = adw::SwitchRow::builder()
                .title(gettext("Button Icons"))
                .active(config.borrow().show_footer_icons)
                .use_markup(false)
                .build();
            {
                let cfg = config.clone();
                sw.connect_active_notify(move |r| {
                    let active = r.is_active();
                    save_and_refresh(&cfg, |c| c.show_footer_icons = active);
                });
            }
            fg.add(&sw);
        }
        for (label, subtitle, field) in [
            (
                gettext("Operations Shortcut"),
                gettext("Show the key that opens Operations"),
                0usize,
            ),
            (
                gettext("Hints Shortcut"),
                gettext("Show the key that opens Hints"),
                1,
            ),
        ] {
            let on = match field {
                0 => config.borrow().show_operations_shortcut_label,
                _ => config.borrow().show_hints_shortcut_label,
            };
            let sw = adw::SwitchRow::builder()
                .title(label)
                .subtitle(subtitle)
                .active(on)
                .use_markup(false)
                .build();
            {
                let cfg = config.clone();
                sw.connect_active_notify(move |r| {
                    let active = r.is_active();
                    save_and_refresh(&cfg, |c| match field {
                        0 => c.show_operations_shortcut_label = active,
                        _ => c.show_hints_shortcut_label = active,
                    });
                });
            }
            fg.add(&sw);
        }
        general.add(&fg);
    }

    // About — single row opening the native AboutDialog
    let ag = adw::PreferencesGroup::new();
    general.add(&ag);
    let about_row = adw::ActionRow::builder()
        .title(gettext("About Spotty"))
        .subtitle(gettext("Version {v}").replace("{v}", env!("CARGO_PKG_VERSION")))
        .activatable(true)
        .use_markup(false)
        .build();
    about_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let win = window.clone();
        about_row.connect_activated(move |_| present_about_dialog(&win));
    }
    ag.add(&about_row);
}

thread_local! {
    /// The Search page's group for rows not in the regular search (unordered).
    static OTHER_GROUP: RefCell<Option<adw::PreferencesGroup>> = RefCell::new(None);
    /// Rebuilds the trigger list. Set by the Triggers page; the result-type
    /// popups call it after a word or shortcut changes.
    static TRIGGER_LIST_REFRESH: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
    static SERVICE_ROW_REFRESH: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
    static STORE_SERVICE_REFRESH: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
    static BRIDGE_SETTINGS_OPEN: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
}

fn show_bridge_settings() {
    let open = BRIDGE_SETTINGS_OPEN.with(|callback| callback.borrow().clone());
    if let Some(open) = open {
        open();
    }
}

fn refresh_service_row() {
    let settings = SERVICE_ROW_REFRESH.with(|refresh| refresh.borrow().clone());
    if let Some(refresh) = settings { refresh(); }
    let store = STORE_SERVICE_REFRESH.with(|refresh| refresh.borrow().clone());
    if let Some(refresh) = store { refresh(); }
}

fn refresh_trigger_list() {
    let refresh = TRIGGER_LIST_REFRESH.with(|r| r.borrow().clone());
    if let Some(refresh) = refresh {
        refresh();
    }
}

use crate::trigger_defaults::{result_blurb, result_title};

/// The blurb, plus the word and shortcut once the user gave it any.
fn result_subtitle(blurb: &str, word: &str, shortcut: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !blurb.is_empty() {
        parts.push(blurb.to_string());
    }
    if !word.is_empty() {
        parts.push(gettext("Word: {word}").replace("{word}", word));
    }
    if !shortcut.is_empty() {
        parts.push(display_shortcut(shortcut));
    }
    parts.join(" · ")
}

/// A built-in trigger's or result type's name, wherever it is listed.
fn builtin_title(id: &str) -> String {
    if crate::config::RESULT_IDS.contains(&id) {
        result_title(id)
    } else {
        capitalized(id)
    }
}

/// The "Show in Regular Search" switch for a result type or a trigger, and a
/// closure that re-syncs it after the word changed.
///
/// A result type with no trigger word has no other way to be found, so its
/// switch is on and locked until it gets one. A trigger always has a word, so
/// its switch is always the user's call (off unless they opted in).
fn regular_search_row(config: &Rc<RefCell<Config>>, id: &str) -> (adw::SwitchRow, Rc<dyn Fn()>) {
    let is_result = crate::config::RESULT_IDS.contains(&id);
    let row = adw::SwitchRow::builder()
        .title(gettext("Show in Regular Search"))
        .use_markup(false)
        .build();
    let syncing = Rc::new(std::cell::Cell::new(false));
    let sync: Rc<dyn Fn()> = {
        let row = row.clone();
        let cfg = config.clone();
        let id = id.to_string();
        let syncing = syncing.clone();
        Rc::new(move || {
            let (has_word, on) = {
                let c = cfg.borrow();
                let wordless = is_result
                    && c.command_keywords
                        .iter()
                        .find(|k| k.id == id)
                        .map_or(true, |k| k.word.is_empty());
                (!wordless, c.in_regular_search(&id))
            };
            syncing.set(true);
            row.set_active(on);
            syncing.set(false);
            row.set_sensitive(has_word);
            row.set_subtitle(&if !has_word {
                gettext("Always on without a trigger word — it is the only way to find them")
            } else if is_result {
                gettext("Also list these results when you search without the trigger word")
            } else {
                gettext("Also list its results when you search without typing the trigger word")
            });
        })
    };
    sync();
    {
        let cfg = config.clone();
        let id = id.to_string();
        row.connect_active_notify(move |r| {
            if syncing.get() {
                return;
            }
            let on = r.is_active();
            save_and_refresh(&cfg, |c| c.set_in_regular_search(&id, on));
            refresh_trigger_list();
        });
    }
    (row, sync)
}

/// What every result-type popup ends with: a "Trigger" group (a word and a
/// shortcut that open a search of just these results — both optional, with a
/// reset once changed) and, last, the destructive Uninstall row.
fn add_trigger_sections(
    page: &adw::PreferencesPage,
    dlg: &adw::PreferencesDialog,
    config: &Rc<RefCell<Config>>,
    id: &str,
) {
    let (word, shortcut) = config
        .borrow()
        .command_keywords
        .iter()
        .find(|k| k.id == id)
        .map(|k| (k.word.clone(), k.shortcut.clone()))
        .unwrap_or_default();

    let group = adw::PreferencesGroup::builder()
        .title(gettext("Trigger"))
        .description(gettext(
            "Type the word or press the shortcut to search only these results.",
        ))
        .build();

    let (regular_row, sync_regular) = regular_search_row(config, id);
    let word_row = adw::EntryRow::builder()
        .title(gettext("Word"))
        .text(&word)
        .show_apply_button(true)
        .use_markup(false)
        .build();
    // The result types' default is no word at all, so Reset clears it.
    let reset_word = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Remove Word"))
        .build();
    reset_word.set_visible(!word.is_empty());
    word_row.add_suffix(&reset_word);
    {
        let word_row = word_row.clone();
        let reset = reset_word.clone();
        reset_word.connect_clicked(move |_| {
            // Clearing the text and applying it goes through the same save.
            word_row.set_text("");
            word_row.emit_by_name::<()>("apply", &[]);
            reset.set_visible(false);
        });
    }
    {
        let cfg = config.clone();
        let dlg = dlg.clone();
        let id = id.to_string();
        let reset_word = reset_word.clone();
        let sync_regular = sync_regular.clone();
        word_row.connect_apply(move |r| {
            let w = r.text().trim().to_lowercase();
            if w.contains(char::is_whitespace) {
                dlg.add_toast(adw::Toast::new(&gettext("A trigger word is a single word")));
                return;
            }
            if !w.is_empty() && word_taken(&id, &w, &cfg) {
                dlg.add_toast(adw::Toast::new(
                    &gettext("\"{word}\" is already a trigger word").replace("{word}", &w),
                ));
                return;
            }
            r.set_text(&w);
            reset_word.set_visible(!w.is_empty());
            save_and_refresh(&cfg, |c| c.set_result_word(&id, &w));
            sync_regular();
            refresh_trigger_list();
        });
    }
    group.add(&word_row);

    let pending = Rc::new(RefCell::new(shortcut));
    let (sc_row, _reset) = {
        let cfg = config.clone();
        let pending_c = pending.clone();
        let id = id.to_string();
        capture_shortcut_row(
            dlg,
            &gettext("Shortcut"),
            &result_title(&id),
            pending.clone(),
            Rc::new(move || {
                let value = pending_c.borrow().clone();
                save_and_refresh(&cfg, |c| {
                    if let Some(k) = c.command_keywords.iter_mut().find(|k| k.id == id) {
                        k.shortcut = value.clone();
                    }
                });
                std::thread::spawn(crate::keybindings::register_all);
                refresh_trigger_list();
            }),
            "",
        )
    };
    group.add(&sc_row);
    group.add(&regular_row);
    page.add(&group);

    let id = id.to_string();
    let cfg = config.clone();
    page.add(&uninstall_group(dlg, &result_title(&id), move |win| {
        if let Some(win) = win {
            uninstall_builtin_from_list(&win, &cfg, &id);
        }
    }));
}

/// The last group of a detail popup: one destructive "Uninstall" row, the
/// GNOME pattern for removing the thing the dialog is about. `uninstall`
/// gets the settings window (for its toast) once the popup has closed.
fn uninstall_group(
    dlg: &adw::PreferencesDialog,
    name: &str,
    uninstall: impl Fn(Option<adw::PreferencesWindow>) + 'static,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let row = adw::ButtonRow::builder().title(gettext("Uninstall")).build();
    row.add_css_class("destructive-action");
    row.set_tooltip_text(Some(
        &gettext("Remove \"{name}\" from Spotty — reinstall it from the Store").replace("{name}", name),
    ));
    {
        let dlg = dlg.clone();
        row.connect_activated(move |_| {
            let win = dlg
                .root()
                .and_then(|r| r.downcast::<adw::PreferencesWindow>().ok());
            dlg.close();
            uninstall(win);
        });
    }
    group.add(&row);
    group
}

/// Uninstall a built-in trigger or result type from its list row: it leaves
/// the list, stops working, and waits in the Store for a reinstall.
fn uninstall_builtin_from_list(
    window: &adw::PreferencesWindow,
    config: &Rc<RefCell<Config>>,
    id: &str,
) {
    save_and_refresh(config, |c| c.uninstall_builtin(id));
    std::thread::spawn(crate::keybindings::register_all);
    refresh_trigger_list();
    window.add_toast(adw::Toast::new(
        &gettext("Removed \"{name}\" — reinstall it from the Store").replace("{name}", &builtin_title(id)),
    ));
}

// ──────────────────────────────────────────────────────────────────────
// Trigger page - remap trigger words, add your own, delete installed ones
// ──────────────────────────────────────────────────────────────────────
/// One row in the trigger list: a built-in keyword or an installed trigger.
/// Clicking the row opens an edit dialog (word, shortcut, uninstall).
struct TriggerRow {
    id: String,
    is_installed: bool,
    row: adw::ActionRow,
}

fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn trigger_subtitle(enabled: bool, word: &str, shortcut: &str) -> String {
    let base = if shortcut.is_empty() {
        gettext("Word: {word} · No shortcut").replace("{word}", word)
    } else {
        gettext("Word: {word} · {shortcut}").replace("{word}", word).replace("{shortcut}", shortcut)
    };
    if enabled {
        base
    } else {
        gettext("Disabled · {base}").replace("{base}", &base)
    }
}

/// Build the Trigger settings page. Returns a refresh closure that rebuilds
/// the trigger rows (used when the cached settings window is re-presented, so
/// triggers installed meanwhile show up).
fn build_keywords_page(
    window: &adw::PreferencesWindow,
    config: &Rc<RefCell<Config>>,
) -> Rc<dyn Fn()> {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Search"))
        .icon_name("system-search-symbolic")
        .build();
    window.add(&page);

    // Proton Bridge is a lazily-created page in this PreferencesWindow, so
    // opening its settings never starts another Spotty process or window.
    let bridge_page: Rc<RefCell<Option<adw::PreferencesPage>>> =
        Rc::new(RefCell::new(None));
    let bridge_handle: Rc<RefCell<Option<spotty_proton_bridge_gui::gui::EmbeddedBridge>>> =
        Rc::new(RefCell::new(None));
    let weak_window = window.downgrade();
    let return_page = page.clone();
    BRIDGE_SETTINGS_OPEN.with(|callback| {
        let bridge_page = bridge_page.clone();
        let bridge_handle = bridge_handle.clone();
        let config = config.clone();
        *callback.borrow_mut() = Some(Rc::new(move || {
            if !config.borrow().proton_bridge_enabled {
                return;
            }
            let Some(window) = weak_window.upgrade() else {
                return;
            };
            // Drop the Ref before creating a page: the creation branch stores
            // the new page through borrow_mut below.
            let existing_page = bridge_page.borrow().clone();
            let page = if let Some(page) = existing_page {
                page.clone()
            } else {
                let page = adw::PreferencesPage::builder()
                    .title(gettext("Proton Mail Bridge"))
                    .icon_name("mail-send-receive-symbolic")
                    .build();
                let group = adw::PreferencesGroup::new();
                let weak_window = window.downgrade();
                let return_page = return_page.clone();
                let handle = spotty_proton_bridge_gui::gui::EmbeddedBridge::new(&window, move || {
                    if let Some(window) = weak_window.upgrade() {
                        window.set_visible_page(&return_page);
                    }
                });
                group.add(&handle.widget());
                page.add(&group);
                window.add(&page);
                *bridge_handle.borrow_mut() = Some(handle);
                *bridge_page.borrow_mut() = Some(page.clone());
                page
            };
            window.set_visible_page(&page);
            window.present();
        }));
    });

    // Result types and triggers in one list, in the user's order: the higher a
    // row, the higher its results rank in the regular search.
    // Group titles are markup: the "&" has to be escaped or the title vanishes.
    let g = adw::PreferencesGroup::builder()
        .title(glib::markup_escape_text(&gettext("Results & Triggers")).as_str())
        .description(gettext(
            "Higher rows rank first in the regular search. Drag to reorder.",
        ))
        .build();
    page.add(&g);
    let other_group = adw::PreferencesGroup::builder()
        .title(gettext("Only by Trigger Word"))
        .description(gettext(
            "Not shown in the regular search, so not ranked. Turn on \u{201c}Show in Regular Search\u{201d} to add one to the order above.",
        ))
        .build();
    page.add(&other_group);
    OTHER_GROUP.with(|o| *o.borrow_mut() = Some(other_group));

    let services = adw::PreferencesGroup::builder()
        .title(gettext("Server-side installations"))
        .description(gettext("Local background services configured outside search."))
        .build();
    let bridge = adw::ActionRow::builder()
        .title(gettext("Proton Mail Bridge"))
        .subtitle(gettext("Local mail server for Proton Mail clients"))
        .use_markup(false)
        .build();
    let uninstall = gtk::Button::with_label(&gettext("Uninstall"));
    uninstall.add_css_class("flat");
    uninstall.set_valign(gtk::Align::Center);
    let bridge_settings = gtk::Button::with_label(&gettext("Settings"));
    bridge_settings.add_css_class("flat");
    bridge_settings.set_valign(gtk::Align::Center);
    let installed = config.borrow().proton_bridge_enabled;
    uninstall.set_visible(installed);
    bridge_settings.set_visible(installed);
    bridge_settings.set_sensitive(installed && crate::proton_bridge::supported());
    {
        let cfg = config.clone();
        uninstall.connect_clicked(move |_| {
            save_and_refresh(&cfg, |c| c.proton_bridge_enabled = false);
            refresh_service_row();
        });
    }
    bridge_settings.connect_clicked(move |_| {
        show_bridge_settings();
    });
    bridge.add_suffix(&bridge_settings);
    bridge.add_suffix(&uninstall);
    {
        let uninstall = uninstall.clone();
        let settings = bridge_settings.clone();
        let cfg = config.clone();
        let services = services.clone();
        let page = page.clone();
        let search_page = page.clone();
        let bridge_page = bridge_page.clone();
        let bridge_handle = bridge_handle.clone();
        let weak_window = window.downgrade();
        let attached = Rc::new(std::cell::Cell::new(installed));
        if installed {
            page.add(&services);
        }
        SERVICE_ROW_REFRESH.with(|refresh| {
            *refresh.borrow_mut() = Some(Rc::new(move || {
                let enabled = cfg.borrow().proton_bridge_enabled;
                uninstall.set_visible(enabled);
                settings.set_visible(enabled);
                settings.set_sensitive(enabled && crate::proton_bridge::supported());

                // Remove the complete service group when Bridge is absent.
                if attached.get() != enabled {
                    if enabled {
                        page.add(&services);
                    } else {
                        page.remove(&services);
                    }
                    attached.set(enabled);
                }

                // Uninstalling Bridge must also discard its embedded page so
                // it cannot remain in Settings or be reopened by a stale row.
                if !enabled {
                    if let Some(handle) = bridge_handle.borrow_mut().take() {
                        handle.close();
                    }
                    let old_page = bridge_page.borrow_mut().take();
                    if let (Some(window), Some(old_page)) =
                        (weak_window.upgrade(), old_page)
                    {
                        window.set_visible_page(&search_page);
                        window.remove(&old_page);
                    }
                }
            }));
        });
    }
    services.add(&bridge);

    let rows: Rc<RefCell<Vec<TriggerRow>>> = Rc::new(RefCell::new(Vec::new()));

    // The Store sits in the group header: browse, install and reinstall.
    let store_btn = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name(store_icon())
                .label(gettext("Store"))
                .build(),
        )
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Browse, install and reinstall triggers"))
        .build();
    {
        let win = window.clone();
        let g2 = g.clone();
        let cfg = config.clone();
        let rows2 = rows.clone();
        store_btn.connect_clicked(move |_| {
            store_dialog(&win, &g2, &cfg, &rows2);
        });
    }
    g.set_header_suffix(Some(&store_btn));

    // Reset: last on the page, behind a confirmation — it rewrites every
    // built-in word and shortcut.
    let reset_group = adw::PreferencesGroup::new();
    let reset_row = adw::ButtonRow::builder()
        .title(gettext("Reset Words and Shortcuts"))
        .build();
    {
        let cfg = config.clone();
        let win = window.clone();
        let g2 = g.clone();
        let rows2 = rows.clone();
        reset_row.connect_activated(move |_| {
            let alert = adw::AlertDialog::builder()
                .heading(gettext("Reset Words and Shortcuts?"))
                .body(gettext(
                    "Every built-in trigger and result type gets its original word and shortcut back, and paused ones are switched on.",
                ))
                .close_response("cancel")
                .default_response("cancel")
                .build();
            alert.add_response("cancel", &gettext("Cancel"));
            alert.add_response("reset", &gettext("Reset"));
            alert.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
            let cfg = cfg.clone();
            let win2 = win.clone();
            let g3 = g2.clone();
            let rows3 = rows2.clone();
            alert.connect_response(None, move |_, response| {
                if response != "reset" {
                    return;
                }
                let d = Config::default();
                {
                    let mut c = cfg.borrow_mut();
                    for def in &d.command_keywords {
                        if def.id == "cmd" {
                            continue;
                        }
                        if let Some(existing) = c.command_keywords.iter_mut().find(|k| k.id == def.id) {
                            existing.shortcut = def.shortcut.clone();
                            existing.word = def.word.clone();
                            existing.enabled = true;
                        }
                    }
                    c.save();
                }
                std::thread::spawn(crate::keybindings::register_all);
                rebuild_trigger_rows(&g3, &win2, &cfg, &rows3);
                win2.add_toast(adw::Toast::new(&gettext("Words and shortcuts reset")));
            });
            alert.present(Some(&win));
        });
    }
    reset_group.add(&reset_row);
    page.add(&reset_group);

    {
        let g = g.clone();
        let window = window.clone();
        let config = config.clone();
        let rows = rows.clone();
        rebuild_trigger_rows(&g, &window, &config, &rows);
    }

    let g = g.clone();
    let window = window.clone();
    let config = config.clone();
    let rows = rows.clone();
    let refresh: Rc<dyn Fn()> = Rc::new(move || rebuild_trigger_rows(&g, &window, &config, &rows));
    TRIGGER_LIST_REFRESH.with(|r| *r.borrow_mut() = Some(refresh.clone()));
    refresh
}

/// The symbolic icon at the start of every result and trigger row, so the
/// list scans by shape as well as by name. Unknown names get a generic one.
fn row_icon(name: &str) -> gtk::Image {
    let name = if name == crate::config::STORE_ICON {
        store_icon()
    } else if !name.is_empty() && has_icon(name) {
        name
    } else {
        "application-x-addon-symbolic"
    };
    gtk::Image::from_icon_name(name)
}

/// Spotty's store glyph — or Adwaita's legacy bag on an install that lacks
/// Spotty's icon files.
pub(crate) fn store_icon() -> &'static str {
    if has_icon(crate::config::STORE_ICON) {
        crate::config::STORE_ICON
    } else {
        "system-software-install-symbolic"
    }
}

/// A result type's row: what it shows (or, for Updates, the live status),
/// plus its word and shortcut once set; trash, switch and chevron like a
/// trigger row. The switch is the result type's own on/off; the chevron opens
/// its settings popup, which ends with the word and shortcut sections.
fn result_type_row(
    id: &'static str,
    window: &adw::PreferencesWindow,
    config: &Rc<RefCell<Config>>,
) -> adw::ActionRow {
    let (on, word, shortcut) = {
        let c = config.borrow();
        let kw = c.command_keywords.iter().find(|k| k.id == id);
        (
            c.result_enabled(id),
            kw.map(|k| k.word.clone()).unwrap_or_default(),
            kw.map(|k| k.shortcut.clone()).unwrap_or_default(),
        )
    };
    let subtitle_for = move |on: bool| {
        if id == "updates" {
            result_subtitle(&crate::search::cmd::update_status_text(on), &word, &shortcut)
        } else {
            result_subtitle(&result_blurb(id), &word, &shortcut)
        }
    };
    let row = adw::ActionRow::builder()
        .title(result_title(id))
        .subtitle(subtitle_for(on))
        .activatable(true)
        .use_markup(false)
        .build();
    let icon = config
        .borrow()
        .command_keywords
        .iter()
        .find(|k| k.id == id)
        .map(|k| k.icon.clone())
        .unwrap_or_default();
    add_row_prefixes(&row, &icon, config.borrow().in_regular_search(id));
    let switch = gtk::Switch::builder()
        .active(on)
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&switch);
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let cfg = config.clone();
        switch.connect_active_notify(move |sw| {
            let active = sw.is_active();
            save_and_refresh(&cfg, |c| c.set_result_enabled(id, active));
            // A switched-off result type's shortcut goes with it.
            std::thread::spawn(crate::keybindings::register_all);
        });
    }
    {
        let win = window.clone();
        let cfg = config.clone();
        row.connect_activated(move |_| open_result_dialog(id, &win, &cfg));
    }

    if id == "updates" {
        crate::search::cmd::ensure_updates_checked();
        // Keep the status live: a background check landing updates the
        // subtitle and the icon without the list being rebuilt. The tick ends
        // with the row (a rebuild replaces it).
        let row_c = row.clone();
        let win = window.clone();
        let cfg = config.clone();
        glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            if row_c.parent().is_none() {
                return glib::ControlFlow::Break;
            }
            if win.is_visible() {
                let enabled = cfg.borrow().enable_updates;
                row_c.set_subtitle(&subtitle_for(enabled));
            }
            glib::ControlFlow::Continue
        });
    }
    row
}

/// The start of every row: the icon, after a drag handle when the row is in
/// the ordered list (shown in the regular search).
fn add_row_prefixes(row: &adw::ActionRow, icon: &str, ordered: bool) {
    let handle = gtk::Image::builder()
        .icon_name("list-drag-handle-symbolic")
        .css_classes(["dim-label"])
        .tooltip_text(gettext("Drag to reorder (Alt+Up / Alt+Down)"))
        .build();
    // Prefixes stack from the title outwards: the last one added sits at the
    // row's edge, where the handle belongs.
    row.add_prefix(&row_icon(icon));
    if ordered {
        row.add_prefix(&handle);
    }
}

/// Put `id` right before or after `anchor` in the saved order, then rebuild
/// the list and keep the keyboard focus on the moved row.
fn move_next_to(
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
    id: &str,
    anchor: &str,
    after: bool,
) {
    if id == anchor {
        return;
    }
    save_and_refresh(config, |c| {
        let mut ids = c.ordered_ids();
        if let Some(from) = ids.iter().position(|o| o == id) {
            ids.remove(from);
        }
        if let Some(a) = ids.iter().position(|o| o == anchor) {
            c.move_in_order(id, if after { a + 1 } else { a });
        }
    });
    // Rebuild after the current event (a drop or key press is still being
    // handled by a row the rebuild removes).
    let rows = rows.clone();
    let id = id.to_string();
    glib::idle_add_local_once(move || {
        refresh_trigger_list();
        if let Some(tr) = rows.borrow().iter().find(|tr| tr.id == id) {
            tr.row.grab_focus();
        }
    });
}

/// Make a list row reorderable: drag it onto another row (the upper half drops
/// it before that row, the lower half after), or press Alt+Up / Alt+Down.
fn make_reorderable(
    row: &adw::ActionRow,
    id: &str,
    prev: Option<String>,
    next: Option<String>,
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
) {
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    {
        let id = id.to_string();
        source.connect_prepare(move |_, _, _| {
            Some(gtk::gdk::ContentProvider::for_value(&id.to_value()))
        });
    }
    {
        let row = row.clone();
        source.connect_drag_begin(move |src, _| {
            let icon = gtk::WidgetPaintable::new(Some(&row));
            src.set_icon(Some(&icon), 0, 0);
        });
    }
    row.add_controller(source);

    let target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
    {
        let row_c = row.clone();
        let id = id.to_string();
        let cfg = config.clone();
        let rows = rows.clone();
        target.connect_drop(move |_, value, _x, y| {
            let Ok(dragged) = value.get::<String>() else {
                return false;
            };
            let after = y > f64::from(row_c.height()) / 2.0;
            move_next_to(&cfg, &rows, &dragged, &id, after);
            true
        });
    }
    row.add_controller(target);

    let keys = gtk::EventControllerKey::new();
    {
        let id = id.to_string();
        let cfg = config.clone();
        let rows = rows.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            if !state.contains(gtk::gdk::ModifierType::ALT_MASK) {
                return glib::Propagation::Proceed;
            }
            let (anchor, after) = match key {
                gtk::gdk::Key::Up => (prev.clone(), false),
                gtk::gdk::Key::Down => (next.clone(), true),
                _ => return glib::Propagation::Proceed,
            };
            if let Some(anchor) = anchor {
                move_next_to(&cfg, &rows, &id, &anchor, after);
            }
            glib::Propagation::Stop
        });
    }
    row.add_controller(keys);
}

/// Rebuild the trigger list (built-in keywords + installed triggers).
/// Called at page build time and after any mutation (save, reset, uninstall).
fn rebuild_trigger_rows(
    g: &adw::PreferencesGroup,
    window: &adw::PreferencesWindow,
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
) {
    for tr in rows.borrow().iter() {
        // Rows live in two groups (Results, Triggers): remove each from its own.
        if let Some(group) = tr
            .row
            .ancestor(adw::PreferencesGroup::static_type())
            .and_then(|w| w.downcast::<adw::PreferencesGroup>().ok())
        {
            group.remove(&tr.row);
        }
    }
    rows.borrow_mut().clear();
    // Built first, then added in the user's order (see the end).
    let mut built: Vec<(String, adw::ActionRow)> = Vec::new();

    let keywords: Vec<CommandKeyword> = config.borrow().command_keywords.clone();

    // Result types first: what the universal search shows.
    for id in crate::config::RESULT_IDS {
        if config.borrow().is_uninstalled(id)
            || !config.borrow().command_keywords.iter().any(|k| k.id == id)
        {
            continue;
        }
        let row = result_type_row(id, window, config);
        built.push((id.to_string(), row.clone()));
        rows.borrow_mut().push(TriggerRow {
            id: id.to_string(),
            is_installed: false,
            row,
        });
    }

    for kw in &keywords {
        if kw.id == "cmd" || kw.is_result() || config.borrow().is_uninstalled(&kw.id) {
            continue;
        }
        let row = adw::ActionRow::builder()
            .title(capitalized(&kw.id))
            .subtitle(&trigger_subtitle(kw.enabled, &kw.word, &kw.shortcut))
            .activatable(true)
            .use_markup(false)
            .build();
        add_row_prefixes(&row, &kw.icon, config.borrow().in_regular_search(&kw.id));
        let switch = gtk::Switch::builder()
            .active(kw.enabled)
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&switch);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        {
            let id = kw.id.clone();
            let cfg = config.clone();
            let g2 = g.clone();
            let win = window.clone();
            let rows2 = rows.clone();
            switch.clone().connect_state_set(move |_, on| {
                {
                    let mut c = cfg.borrow_mut();
                    if let Some(k) = c.command_keywords.iter_mut().find(|k| k.id == id) {
                        k.enabled = on;
                    }
                    c.save();
                }
                // Pausing releases the global shortcut right away — the old
                // path only re-registered when the triggers window was open.
                std::thread::spawn(crate::keybindings::register_all);
                rebuild_trigger_rows(&g2, &win, &cfg, &rows2);
                glib::Propagation::Proceed
            });
        }
        {
            let id = kw.id.clone();
            let win = window.clone();
            let cfg = config.clone();
            let g2 = g.clone();
            let rows2 = rows.clone();
            row.connect_activated(move |_| {
                edit_trigger_dialog(&win, &g2, &cfg, &rows2, &id, false);
            });
        }
        built.push((kw.id.clone(), row.clone()));
        rows.borrow_mut().push(TriggerRow {
            id: kw.id.clone(),
            is_installed: false,
            row: row.clone(),
        });
    }

    for a in crate::triggers::all() {
        let row = adw::ActionRow::builder()
            .title(&a.name)
            .subtitle(&trigger_subtitle(a.enabled, &a.word, &a.shortcut))
            .activatable(true)
            .use_markup(false)
            .build();
        add_row_prefixes(&row, &a.icon, config.borrow().in_regular_search(&a.id));
        let switch = gtk::Switch::builder()
            .active(a.enabled)
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&switch);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        {
            let id = a.id.clone();
            let cfg = config.clone();
            let g2 = g.clone();
            let win = window.clone();
            let rows2 = rows.clone();
            switch.clone().connect_state_set(move |_, on| {
                let m = crate::triggers::by_id(&id);
                let Some(m) = m else {
                    return glib::Propagation::Proceed;
                };
                if let Err(e) =
                    crate::triggers::update_manifest(&id, &m.word, &m.shortcut, on)
                {
                    log::warn!("triggers: cannot update enabled state: {e}");
                    return glib::Propagation::Proceed;
                }
                // Paused → the manifest's global shortcut must go too.
                std::thread::spawn(crate::keybindings::register_all);
                rebuild_trigger_rows(&g2, &win, &cfg, &rows2);
                glib::Propagation::Proceed
            });
        }
        {
            let id = a.id.clone();
            let win = window.clone();
            let cfg = config.clone();
            let g2 = g.clone();
            let rows2 = rows.clone();
            row.connect_activated(move |_| {
                edit_trigger_dialog(&win, &g2, &cfg, &rows2, &id, true);
            });
        }
        built.push((a.id.clone(), row.clone()));
        rows.borrow_mut().push(TriggerRow {
            id: a.id.clone(),
            is_installed: true,
            row: row.clone(),
        });
    }

    // Rows shown in the regular search: in the user's order, each draggable
    // (and movable with Alt+Up/Down). The rest have no rank there, so they sit
    // in their own group, by name.
    let (mut ordered, mut others): (Vec<_>, Vec<_>) = built
        .into_iter()
        .partition(|(id, _)| config.borrow().in_regular_search(id));
    let order = config.borrow().ordered_ids();
    ordered.sort_by_key(|(id, _)| order.iter().position(|o| o == id).unwrap_or(usize::MAX));
    for (i, (id, row)) in ordered.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| ordered[p].0.clone());
        let next = ordered.get(i + 1).map(|(n, _)| n.clone());
        make_reorderable(row, id, prev, next, config, rows);
        g.add(row);
    }
    others.sort_by_key(|(_, row)| row.title().to_lowercase());
    let other_group = OTHER_GROUP.with(|o| o.borrow().clone());
    for (_, row) in &others {
        other_group.as_ref().unwrap_or(g).add(row);
    }
    if let Some(og) = other_group {
        og.set_visible(!others.is_empty());
    }
}

// ──────────────────────────────────────────────────────────────────────
// Import — the Trigger page's + button
// ──────────────────────────────────────────────────────────────────────

/// The `+` on the Trigger page: open a file picker for a downloaded
/// manifest (.json) and install it. No marketplace — the user picks the
/// file they downloaded from the triggers repository.
fn import_trigger_file(
    window: &adw::PreferencesWindow,
    g: &adw::PreferencesGroup,
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Trigger manifests (*.json)"));
    filter.add_suffix("json");
    filter.add_mime_type("application/json");
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title(gettext("Add Trigger Word"))
        .modal(true)
        .filters(&filters)
        .default_filter(&filter)
        .build();
    let win = window.clone();
    let g2 = g.clone();
    let cfg = config.clone();
    let rows2 = rows.clone();
    dialog.open(
        Some(window),
        None::<&gtk::gio::Cancellable>,
        move |result| match result {
            Ok(file) => {
                if let Some(path) = file.path() {
                    install_manifest_file(&win, &g2, &cfg, &rows2, &path, false, None);
                }
            }
            Err(_) => {
                // Dismissed the picker — not an error worth surfacing.
                log::info!("triggers: import cancelled");
            }
        },
    );
}

/// Shared install step: shell triggers show a confirmation dialog with the
/// exact command first, then everything goes through `finish_trigger_install`.
/// `cleanup` marks files we generated (server download, create dialog): the
/// temp file is deleted once the install attempt finishes — a user's own
/// picked file is never touched.
/// What installing a manifest needs first: nothing, or a confirmation that
/// shows exactly what the trigger will be able to do after install.
enum InstallPreview {
    /// Harmless action (file-search filter, web link) — install directly.
    Direct,
    /// Show this before installing: shell triggers can run anything, so
    /// their confirmation carries the exact command (Cancel is the default).
    Confirm {
        name: String,
        body: String,
        destructive: bool,
    },
}

/// Classify a manifest for the install gate: shell actions confirm with the
/// exact command; everything else (web links, file filters) installs
/// directly — a web trigger can only open a browser tab, never run code.
fn install_preview(m: &crate::triggers::TriggerManifest) -> InstallPreview {
    match &m.action {
        crate::triggers::TriggerAction::Shell { command } => InstallPreview::Confirm {
            name: m.name.clone(),
            body: format!(
                "This trigger runs shell commands on your system.\n\n{command}\n\nInstall only if you trust its author."
            ),
            destructive: true,
        },
        _ => InstallPreview::Direct,
    }
}

/// Install a manifest file. `cleanup` marks files we generated (temp files
/// we wrote); `on_done` fires when the install attempt has actually
/// finished — after a deferred shell confirmation too — so callers (the
/// Store) can refresh their row state.
fn install_manifest_file(
    window: &adw::PreferencesWindow,
    g: &adw::PreferencesGroup,
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
    path: &std::path::Path,
    cleanup: bool,
    on_done: Option<Rc<dyn Fn()>>,
) {
    let manifest = crate::triggers::parse_manifest(path);
    let preview = manifest.as_ref().map(install_preview).unwrap_or(InstallPreview::Direct);
    match preview {
        InstallPreview::Direct => {
            finish_trigger_install(window, g, config, rows, path, manifest, cleanup, on_done);
        }
        InstallPreview::Confirm {
            name,
            body,
            destructive,
        } => {
            // Shell triggers run arbitrary commands as the user — always
            // confirm and show the exact command template before installing.
            let dialog = adw::MessageDialog::builder()
                .transient_for(window)
                .heading(gettext("Install {name}?").replace("{name}", &name))
                .body(body)
                .build();
            dialog.add_response("cancel", &gettext("Cancel"));
            dialog.add_response("install", &gettext("Install"));
            dialog.set_default_response(Some("cancel"));
            if destructive {
                dialog
                    .set_response_appearance("install", adw::ResponseAppearance::Destructive);
            }
            {
                let win = window.clone();
                let g2 = g.clone();
                let cfg = config.clone();
                let rows2 = rows.clone();
                let path = path.to_path_buf();
                // Deferred install: the generated temp file must survive until
                // the user answers — removal happens inside finish/cancel below.
                dialog.connect_response(None, move |_, resp| {
                    if resp == "install" {
                        finish_trigger_install(
                            &win,
                            &g2,
                            &cfg,
                            &rows2,
                            &path,
                            manifest.clone(),
                            cleanup,
                            on_done.clone(),
                        );
                    } else if cleanup {
                        let _ = std::fs::remove_file(&path);
                    }
                });
            }
            dialog.present();
        }
    }
}

/// Write a generated manifest to a fresh temp file. `create_new` refuses an
/// existing path — a pre-planted symlink is an error, never a write-through
/// — and the name carries pid + nanoseconds so it isn't predictable.
fn write_generated_manifest(tag: &str, raw: &str) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "spotty_{tag}_{}_{nanos}.json",
        std::process::id()
    ));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| e.to_string())?;
    if let Err(e) = f.write_all(raw.as_bytes()) {
        let _ = std::fs::remove_file(&path);
        return Err(e.to_string());
    }
    Ok(path)
}

/// Validate + copy the manifest into the triggers dir, then refresh the
/// rows and the registered keybindings. Failures surface as a dialog.
/// `cleanup` deletes a generated temp file once the attempt is over;
/// `on_done` runs after the attempt (success or failure) — the Store uses
/// it to flip the row between Install and Uninstall.
fn finish_trigger_install(
    window: &adw::PreferencesWindow,
    g: &adw::PreferencesGroup,
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
    path: &std::path::Path,
    manifest: Result<crate::triggers::TriggerManifest, String>,
    cleanup: bool,
    on_done: Option<Rc<dyn Fn()>>,
) {
    // Native manifests enable the shipped backend, preserving customized words,
    // shortcuts and ordering. They are never copied into the custom registry.
    let installed = manifest.and_then(|m| {
        if matches!(m.action, crate::triggers::TriggerAction::Native) {
            if m.id == "proton-bridge" {
                if !crate::proton_bridge::supported() {
                    return Err(gettext("Proton Mail Bridge is not supported on this system."));
                }
                save_and_refresh(config, |c| c.proton_bridge_enabled = true);
                refresh_service_row();
                return Ok(m);
            }
            if !crate::trigger_defaults::supports_native(&m.id) {
                return Err(format!("This version of Spotty does not support '{}'", m.id));
            }
            save_and_refresh(config, |c| c.install_builtin(&m.id));
            Ok(m)
        } else {
            crate::triggers::install_manifest(m)
        }
    });
    match installed {
        Ok(m) => {
            log::info!("triggers: installed {} (word: {})", m.id, m.word);
            rebuild_trigger_rows(g, window, config, rows);
            std::thread::spawn(crate::keybindings::register_all);
            window.add_toast(adw::Toast::new(&gettext("Installed \"{name}\"").replace("{name}", &m.name)));
        }
        Err(e) => {
            log::warn!("triggers: install failed: {e}");
            let dialog = adw::MessageDialog::builder()
                .transient_for(window)
                .heading(gettext("Install failed"))
                .body(&e)
                .build();
            dialog.add_response("ok", &gettext("OK"));
            dialog.set_default_response(Some("ok"));
            dialog.present();
        }
    }
    // Generated temp file: the attempt is over (success or failure), remove it.
    if cleanup {
        let _ = std::fs::remove_file(path);
    }
    if let Some(cb) = on_done {
        cb();
    }
}

// ──────────────────────────────────────────────────────────────────────
// Create — the Trigger page's + button (author your own or install a file)
// ──────────────────────────────────────────────────────────────────────

/// Manifest id derived from the trigger name: lowercase ASCII alnum with
/// spaces/dashes folded into single '-'; empty input → "trigger".
fn trigger_id_from_name(name: &str) -> String {
    let mut id = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c.to_ascii_lowercase());
        } else if (c == ' ' || c == '-' || c == '_') && !id.ends_with('-') {
            id.push('-');
        }
    }
    let id = id.trim_matches('-');
    if id.is_empty() {
        "trigger".into()
    } else {
        id.to_string()
    }
}

/// Unique manifest id for a new trigger: the name's slug, suffixed when a
/// built-in keyword or an installed trigger already owns it.
fn unique_trigger_id(name: &str, config: &Rc<RefCell<Config>>) -> String {
    let base = trigger_id_from_name(name);
    let taken = |id: &str| {
        config
            .borrow()
            .command_keywords
            .iter()
            .any(|k| k.id == id)
            || crate::triggers::by_id(id).is_some()
    };
    if !taken(&base) {
        return base;
    }
    for i in 2..500u32 {
        let cand = format!("{base}-{i}");
        if !taken(&cand) {
            return cand;
        }
    }
    format!("{base}-{}", std::process::id())
}

/// The `+` on the Trigger page: a form for authoring your own trigger —
/// name, keyword, command line, global shortcut — with a secondary header
/// action that switches to installing a pre-made manifest from disk.
/// On success the manifest goes through `finish_trigger_install` like every
/// other install (validation, rows rebuild, keybinding re-registration).
fn create_trigger_dialog(
    window: &adw::PreferencesWindow,
    g: &adw::PreferencesGroup,
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
) {
    let dialog = adw::Window::builder()
        .transient_for(window)
        .modal(false)
        .title(gettext("Create a Trigger"))
        .default_width(640)
        .default_height(640)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("Create a Trigger", ""))
        .build();
    let cancel_btn = gtk::Button::with_label("Cancel");
    header.pack_start(&cancel_btn);
    let install_btn = gtk::Button::with_label("Install from file…");
    install_btn.set_tooltip_text(Some("Install a trigger manifest (.json) you downloaded"));
    header.pack_start(&install_btn);
    let add_btn = gtk::Button::builder()
        .label(gettext("Add Trigger"))
        .css_classes(["suggested-action"])
        .build();
    header.pack_end(&add_btn);
    toolbar.add_top_bar(&header);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(6)
        .margin_end(6)
        .build();
    let clamp = adw::Clamp::builder().maximum_size(600).build();
    clamp.set_child(Some(&content));
    scroll.set_child(Some(&clamp));
    toolbar.set_content(Some(&scroll));
    dialog.set_content(Some(&toolbar));

    // Inline validation feedback.
    let banner = adw::Banner::builder().revealed(false).build();
    content.append(&banner);

    let trigger_group = adw::PreferencesGroup::builder()
        .title(gettext("Trigger"))
        .description(gettext("What you type to open it."))
        .build();
    let name_row = adw::EntryRow::builder().title(gettext("Name")).use_markup(false).build();
    let word_row = adw::EntryRow::builder().title(gettext("Keyword")).use_markup(false).build();
    let desc_row = adw::EntryRow::builder()
        .title(gettext("Description (optional)"))
        .use_markup(false)
        .build();
    let icon_row = adw::EntryRow::builder()
        .title(gettext("Icon name (optional)"))
        .use_markup(false)
        .build();
    trigger_group.add(&name_row);
    trigger_group.add(&word_row);
    trigger_group.add(&desc_row);
    trigger_group.add(&icon_row);
    content.append(&trigger_group);

    let action_group = adw::PreferencesGroup::builder()
        .title(gettext("Command"))
        .description(
            "Runs in a shell when the trigger is used. {query} is replaced by \
             what you type after the keyword and single-quote-escaped.",
        )
        .build();
    let cmd_row = adw::EntryRow::builder().title(gettext("Command line")).use_markup(false).build();
    action_group.add(&cmd_row);
    content.append(&action_group);

    let pending_shortcut: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let shortcut_group = adw::PreferencesGroup::builder()
        .title(gettext("Keyboard Shortcut"))
        .build();
    let (short_row, _) = capture_shortcut_row(
        &dialog,
        "Global shortcut",
        "",
        pending_shortcut.clone(),
        Rc::new(|| {}),
        "",
    );
    shortcut_group.add(&short_row);
    content.append(&shortcut_group);

    // ── Wiring ─────────────────────────────────────────────────────────
    {
        let d = dialog.clone();
        cancel_btn.connect_clicked(move |_| d.close());
    }
    {
        let win = window.clone();
        let g2 = g.clone();
        let cfg = config.clone();
        let rows2 = rows.clone();
        let d = dialog.clone();
        install_btn.connect_clicked(move |_| {
            d.close();
            import_trigger_file(&win, &g2, &cfg, &rows2);
        });
    }
    {
        let d = dialog.clone();
        let win = window.clone();
        let g2 = g.clone();
        let cfg = config.clone();
        let rows2 = rows.clone();
        let banner = banner.clone();
        let name_row = name_row.clone();
        let word_row = word_row.clone();
        let desc_row = desc_row.clone();
        let icon_row = icon_row.clone();
        let cmd_row = cmd_row.clone();
        let pending = pending_shortcut.clone();
        add_btn.connect_clicked(move |_| {
            banner.set_revealed(false);
            let name = name_row.text().trim().to_string();
            let word = word_row.text().trim().to_lowercase();
            let command = cmd_row.text().trim().to_string();
            if name.is_empty() || word.is_empty() || command.is_empty() {
                banner.set_title(&gettext("Name, keyword and command are required."));
                banner.set_revealed(true);
                return;
            }
            if !word
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
            {
                banner.set_title(&gettext("Keyword: letters, digits, -, _ and . only."));
                banner.set_revealed(true);
                return;
            }
            if word_taken("", &word, &cfg) {
                banner.set_title(&gettext("The keyword \"{word}\" is already taken.").replace("{word}", &word));
                banner.set_revealed(true);
                return;
            }
            let manifest = crate::triggers::TriggerManifest {
                id: unique_trigger_id(&name, &cfg),
                name,
                word: word.clone(),
                description: desc_row.text().trim().to_string(),
                icon: icon_row.text().trim().to_string(),
                shortcut: pending.borrow().clone(),
                default_word: word,
                enabled: true,
                version: "1.0.0".into(),
                author: "you".into(),
                help: String::new(),
                help_image: String::new(),
                action: crate::triggers::TriggerAction::Shell { command },
            };
            match serde_json::to_string_pretty(&manifest)
                .map_err(|e| e.to_string())
                .and_then(|raw| write_generated_manifest("new_trigger", &raw))
            {
                Ok(tmp) => {
                    d.close();
                    finish_trigger_install(&win, &g2, &cfg, &rows2, &tmp, Ok(manifest.clone()), true, None);
                }
                Err(e) => {
                    banner.set_title(&gettext("Cannot save the trigger: {e}").replace("{e}", &e));
                    banner.set_revealed(true);
                }
            }
        });
    }

    dialog.present();
}

// ──────────────────────────────────────────────────────────────────────
// Store — install triggers fetched from the trigger repository
// ──────────────────────────────────────────────────────────────────────

/// True when the active icon theme knows this name (unknown names render
/// as an empty square on the row).
fn has_icon(name: &str) -> bool {
    gtk::gdk::Display::default()
        .map(|d| gtk::IconTheme::for_display(&d).has_icon(name))
        .unwrap_or(false)
}

/// Small error dialog for browse-dialog failures (fetch / write).
fn show_browse_error(window: &adw::PreferencesWindow, msg: &str) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(window)
        .heading(gettext("Couldn't install"))
        .body(msg)
        .build();
    dialog.add_response("ok", &gettext("OK"));
    dialog.set_default_response(Some("ok"));
    dialog.present();
}

/// (Re)build the store list for the current search text. Rows get a
/// `refresh` closure that calls this again after install/uninstall, so
/// their state flips without reopening the dialog.
fn render_store_list(
    list_box: &gtk::Box,
    items: &Rc<RefCell<Vec<crate::triggers::RepoTrigger>>>,
    search: &gtk::SearchEntry,
    empty_page: &adw::StatusPage,
    stack: &gtk::Stack,
    base: &str,
    window: &adw::PreferencesWindow,
    g: &adw::PreferencesGroup,
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
) {
    while let Some(c) = list_box.first_child() {
        list_box.remove(&c);
    }
    let q = search.text().to_string();
    let ql = q.trim().to_lowercase();
    let mut shown: Vec<_> = items
        .borrow()
        .iter()
        .filter(|t| {
            ql.is_empty()
                || t.name.to_lowercase().contains(&ql)
                || t.word.to_lowercase().contains(&ql)
                || t.description.to_lowercase().contains(&ql)
                || t.id.to_lowercase().contains(&ql)
        })
        .cloned()
        .collect();
    shown.sort_by_key(|t| t.is_service());
    if shown.is_empty() {
        let desc = if ql.is_empty() {
            gettext("The repository has no triggers yet.")
        } else {
            gettext("No triggers match \"{q}\".").replace("{q}", &q)
        };
        empty_page.set_description(Some(&desc));
        stack.set_visible_child_name("empty");
        return;
    }
    let refresh: Rc<dyn Fn()> = {
        let list_box = list_box.clone();
        let items = items.clone();
        let search = search.clone();
        let empty_page = empty_page.clone();
        let stack = stack.clone();
        let base = base.to_string();
        let win = window.clone();
        let g2 = g.clone();
        let cfg = config.clone();
        let rows2 = rows.clone();
        Rc::new(move || {
            render_store_list(
                &list_box, &items, &search, &empty_page, &stack, &base, &win, &g2, &cfg, &rows2,
            );
        })
    };
    let triggers = adw::PreferencesGroup::builder().title(gettext("Triggers")).build();
    let services = adw::PreferencesGroup::builder()
        .title(gettext("Server-side installations"))
        .description(gettext("Local background servers configured through Settings."))
        .build();
    let mut has_triggers = false;
    let mut has_services = false;
    for t in &shown {
        let row = server_trigger_row(t, base, window, g, config, rows, &refresh);
        if t.is_service() {
            services.add(&row);
            has_services = true;
        } else {
            triggers.add(&row);
            has_triggers = true;
        }
    }
    if has_triggers { list_box.append(&triggers); }
    if has_services { list_box.append(&services); }
    stack.set_visible_child_name("list");
}

/// One row in the store: icon, name, word + description, and an Install or
/// Uninstall button — whichever matches the current installed state.
fn server_trigger_row(
    t: &crate::triggers::RepoTrigger,
    base: &str,
    window: &adw::PreferencesWindow,
    g: &adw::PreferencesGroup,
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
    refresh: &Rc<dyn Fn()>,
) -> gtk::ListBoxRow {
    let subtitle = if t.is_service() {
        gettext("Local mail server · Login and mail-client credentials in Settings")
    } else if t.description.is_empty() {
        t.word.clone()
    } else if t.word.is_empty() {
        t.description.clone()
    } else {
        gettext("{word} — {description}").replace("{word}", &t.word).replace("{description}", &t.description)
    };
    let action = adw::ActionRow::builder()
        .title(&t.name)
        .subtitle(&subtitle)
        .use_markup(false)
        .build();
    if !t.icon.is_empty() && has_icon(&t.icon) {
        let img = gtk::Image::from_icon_name(&t.icon);
        img.set_pixel_size(24);
        action.add_prefix(&img);
    }
    if t.native {
        let service = t.is_service() && t.id == "proton-bridge";
        let supported = if service { crate::proton_bridge::supported() } else { crate::trigger_defaults::supports_native(&t.id) };
        let installed = if service { config.borrow().proton_bridge_enabled } else {
            config.borrow().command_keywords.iter().any(|k| k.id == t.id)
                && !config.borrow().is_uninstalled(&t.id)
        };
        let label = if service && installed {
            gettext("Settings")
        } else if t.id == "cmd" {
            gettext("Installed")
        } else if !supported {
            gettext("Unavailable")
        } else if !installed {
            gettext("Install")
        } else {
            gettext("Uninstall")
        };
        let btn = gtk::Button::builder()
            .label(label)
            .css_classes([if !installed { "suggested-action" } else { "flat" }])
            .valign(gtk::Align::Center)
            .sensitive(supported && t.id != "cmd")
            .build();
        if service {
            let win = window.clone();
            let cfg = config.clone();
            let refresh = refresh.clone();
            let name = t.name.clone();
            btn.set_label(&gettext(if installed { "Settings" } else { "Install" }));
            btn.set_visible(!installed);
            let settings = gtk::Button::with_label(&gettext("Settings"));
            settings.add_css_class("flat");
            settings.set_valign(gtk::Align::Center);
            settings.set_visible(installed);
            settings.set_sensitive(supported && installed);
            let uninstall = gtk::Button::with_label(&gettext("Uninstall"));
            uninstall.add_css_class("flat");
            uninstall.set_valign(gtk::Align::Center);
            uninstall.set_visible(installed);
            {
                settings.connect_clicked(move |button| {
                    if let Some(store) = button.root().and_then(|root| root.downcast::<adw::Window>().ok()) {
                        store.close();
                    }
                    show_bridge_settings();
                });
            }
            {
                let cfg = cfg.clone();
                let refresh = refresh.clone();
                uninstall.connect_clicked(move |_| {
                    save_and_refresh(&cfg, |c| c.proton_bridge_enabled = false);
                    refresh_service_row();
                    refresh();
                });
            }
            action.add_suffix(&btn);
            action.add_suffix(&settings);
            action.add_suffix(&uninstall);
            {
                let win = win.clone();
                let cfg = cfg.clone();
                let refresh = refresh.clone();
                btn.connect_clicked(move |_| {
                    save_and_refresh(&cfg, |c| c.proton_bridge_enabled = true);
                    refresh_service_row();
                    refresh();
                    win.add_toast(adw::Toast::new(&gettext("Installed \"{name}\"").replace("{name}", &name)));
                });
            }
            return action.upcast();
        }
        let id = t.id.clone();
        let name = t.name.clone();
        let win = window.clone();
        let g2 = g.clone();
        let cfg = config.clone();
        let rows2 = rows.clone();
        let refresh = refresh.clone();
        btn.connect_clicked(move |_| {
            save_and_refresh(&cfg, |c| {
                if !installed { c.install_builtin(&id); }
                else { c.uninstall_builtin(&id); }
            });
            rebuild_trigger_rows(&g2, &win, &cfg, &rows2);
            std::thread::spawn(crate::keybindings::register_all);
            let text = if !installed { gettext("Installed \"{name}\"") }
                else { gettext("Removed \"{name}\"") };
            win.add_toast(adw::Toast::new(&text.replace("{name}", &name)));
            refresh();
        });
        action.add_suffix(&btn);
    } else if crate::triggers::by_id(&t.id).is_some() {
        let uninstall = gtk::Button::builder()
            .label(gettext("Uninstall"))
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        {
            let id = t.id.clone();
            let name = t.name.clone();
            let win = window.clone();
            let g2 = g.clone();
            let cfg = config.clone();
            let rows2 = rows.clone();
            let refresh = refresh.clone();
            uninstall.connect_clicked(move |_| match crate::triggers::uninstall(&id) {
                Ok(()) => {
                    rebuild_trigger_rows(&g2, &win, &cfg, &rows2);
                    // Drop the removed trigger's keybinding slot immediately.
                    std::thread::spawn(crate::keybindings::register_all);
                    win.add_toast(adw::Toast::new(&gettext("Removed \"{name}\"").replace("{name}", &name)));
                    // Flip this row back to Install without reopening.
                    refresh();
                }
                Err(e) => show_browse_error(&win, &gettext("Couldn't uninstall: {e}").replace("{e}", &e)),
            });
        }
        action.add_suffix(&uninstall);
    } else {
        let install = gtk::Button::builder()
            .label(gettext("Install"))
            .css_classes(["suggested-action"])
            .valign(gtk::Align::Center)
            .build();
        {
            let url = format!("{base}/triggers/{}.json", t.id);
            let win = window.clone();
            let g2 = g.clone();
            let cfg = config.clone();
            let rows2 = rows.clone();
            let btn = install.clone();
            let refresh = refresh.clone();
            install.connect_clicked(move |_| {
                btn.set_sensitive(false);
                // Fetch on a worker; the continuation hops back via oneshot +
                // spawn_local (it captures GTK widgets, which aren't Send —
                // same pattern as resolve_host_app_icon_async).
                let (tx, rx) = futures::channel::oneshot::channel();
                let url = url.clone();
                std::thread::spawn(move || {
                    let body = crate::triggers::fetch_json_text(&url);
                    let _ = tx.send(body);
                });
                let btn2 = btn.clone();
                let win2 = win.clone();
                let g3 = g2.clone();
                let cfg3 = cfg.clone();
                let rows3 = rows2.clone();
                let refresh_c = refresh.clone();
                glib::MainContext::default().spawn_local(async move {
                    let body = rx
                        .await
                        .unwrap_or_else(|_| Err("download aborted".to_string()));
                    btn2.set_sensitive(true);
                    match body {
                        Ok(raw) => {
                            // Generated file → cleanup=true; a shell/web
                            // confirmation (if any) defers removal until
                            // the install attempt finishes.
                            match write_generated_manifest("repo_trigger", &raw) {
                                Ok(tmp) => {
                                    // Refresh AFTER the install attempt
                                    // finishes — for shell triggers that is
                                    // after the confirmation dialog, so the
                                    // row flips to Uninstall reliably.
                                    install_manifest_file(
                                        &win2, &g3, &cfg3, &rows3, &tmp, true,
                                        Some(refresh_c),
                                    );
                                }
                                Err(e) => show_browse_error(
                                    &win2,
                                    &gettext("Cannot save the manifest: {e}").replace("{e}", &e),
                                ),
                            }
                        }
                        Err(e) => show_browse_error(&win2, &gettext("Download failed: {e}").replace("{e}", &e)),
                    }
                });
            });
        }
        action.add_suffix(&install);
    }
    action.upcast()
}

/// The Trigger Store: fetch the repository index, search it, and install
/// what you pick through the shared install path (validation, shell
/// confirmation, rows + keybinding refresh). The repository URL is config-
/// only — never shown. The header + opens the create/import dialog for
/// local triggers.
fn store_dialog(
    window: &adw::PreferencesWindow,
    g: &adw::PreferencesGroup,
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
) {
    let dialog = adw::Window::builder()
        .transient_for(window)
        .modal(false)
        .title(gettext("Store"))
        .default_width(640)
        .default_height(640)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("Store", ""))
        .build();
    // Local installs: create your own trigger or import a downloaded
    // manifest — both live in the create dialog behind this +.
    let add_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(gettext("Add a local trigger"))
        .build();
    header.pack_end(&add_btn);
    toolbar.add_top_bar(&header);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    toolbar.set_content(Some(&content));
    dialog.set_content(Some(&toolbar));

    let base = config
        .borrow()
        .trigger_repo_url
        .trim()
        .trim_end_matches('/')
        .to_string();

    let search = gtk::SearchEntry::builder()
        .placeholder_text(gettext("Search triggers and services"))
        .build();
    content.append(
        &adw::Clamp::builder()
            .maximum_size(600)
            .child(&search)
            .build(),
    );

    let list_box = gtk::Box::new(gtk::Orientation::Vertical, 24);
    let loading_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .margin_top(48)
        .build();
    loading_box.append(&crate::ui::circular_progress::progress_ring(
        24,
        None,
        crate::ui::circular_progress::RingState::Running,
    ));
    loading_box.append(
        &gtk::Label::builder()
            .label(gettext("Fetching triggers…"))
            .css_classes(["dim-label", "caption"])
            .build(),
    );

    let empty_page = adw::StatusPage::builder()
        .title(gettext("No triggers found"))
        .icon_name("edit-find-symbolic")
        .build();
    let error_page = adw::StatusPage::builder()
        .title(gettext("Couldn't load the trigger repository"))
        .icon_name("dialog-error-symbolic")
        .build();

    let stack = gtk::Stack::builder().vhomogeneous(false).build();
    stack.add_named(&loading_box, Some("loading"));
    stack.add_named(&list_box, Some("list"));
    stack.add_named(&empty_page, Some("empty"));
    stack.add_named(&error_page, Some("error"));
    stack.set_visible_child_name("loading");
    let clamp = adw::Clamp::builder()
        .maximum_size(600)
        .child(&stack)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();
    content.append(&scroll);

    let items: Rc<RefCell<Vec<crate::triggers::RepoTrigger>>> =
        Rc::new(RefCell::new(Vec::new()));

    // (Re)render the list for the current search text; rows re-render after
    // install/uninstall through the same free function.
    let render: Rc<dyn Fn()> = {
        let list_box = list_box.clone();
        let items = items.clone();
        let stack = stack.clone();
        let search = search.clone();
        let empty_page = empty_page.clone();
        let base = base.clone();
        let win = window.clone();
        let g2 = g.clone();
        let cfg = config.clone();
        let rows2 = rows.clone();
        Rc::new(move || {
            render_store_list(
                &list_box, &items, &search, &empty_page, &stack, &base, &win, &g2, &cfg, &rows2,
            );
        })
    };

    STORE_SERVICE_REFRESH.with(|refresh| *refresh.borrow_mut() = Some(render.clone()));
    {
        dialog.connect_close_request(move |_| {
            STORE_SERVICE_REFRESH.with(|callback| *callback.borrow_mut() = None);
            glib::Propagation::Proceed
        });
    }

    {
        let render = render.clone();
        search.connect_changed(move |_| {
            render();
        });
    }
    {
        let win = window.clone();
        let g2 = g.clone();
        let cfg = config.clone();
        let rows2 = rows.clone();
        let d = dialog.clone();
        add_btn.connect_clicked(move |_| {
            d.close();
            create_trigger_dialog(&win, &g2, &cfg, &rows2);
        });
    }

    if base.is_empty() {
        error_page
            .set_description(Some(&gettext("Set the trigger repository URL in the Trigger Repository row.")));
        stack.set_visible_child_name("error");
    } else {
        let items = items.clone();
        let render = render.clone();
        let stack = stack.clone();
        let error_page = error_page.clone();
        let base_c = base.clone();
        let (tx, rx) = futures::channel::oneshot::channel();
        let url = format!("{base_c}/index.json");
        std::thread::spawn(move || {
            let body = crate::triggers::fetch_json_text(&url);
            let _ = tx.send(body);
        });
        glib::MainContext::default().spawn_local(async move {
            let body = rx
                .await
                .unwrap_or_else(|_| Err("fetch aborted".to_string()));
            match body {
                Ok(raw) => match serde_json::from_str::<Vec<crate::triggers::RepoTrigger>>(&raw)
                {
                    Ok(list) => {
                        *items.borrow_mut() = list.into_iter().filter(|entry| crate::security::valid_id(&entry.id)).collect();
                        render();
                    }
                    Err(e) => {
                        error_page.set_description(Some(&format!(
                            "The repository sent an invalid response: {e}"
                        )));
                        stack.set_visible_child_name("error");
                    }
                },
                Err(e) => {
                    error_page.set_description(Some(&gettext("Couldn't load {url}: {e}").replace("{url}", &base_c).replace("{e}", &e)));
                    stack.set_visible_child_name("error");
                }
            }
        });
    }

    dialog.present();
}

/// Confirm + run a trigger uninstall, then rebuild the trigger list and
/// re-register keybindings. Shared by the row's trash button and the edit
/// dialog's Uninstall response.
fn confirm_uninstall(
    window: &adw::PreferencesWindow,
    g: &adw::PreferencesGroup,
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
    id: &str,
) {
    let win = window.clone();
    let cfg = config.clone();
    let g2 = g.clone();
    let rows2 = rows.clone();
    let id = id.to_string();
    let dialog = adw::MessageDialog::builder()
        .transient_for(&win)
        .heading(gettext("Uninstall \"{id}\"?").replace("{id}", &id))
        .body(gettext("Its trigger word and shortcut will be removed."))
        .build();
    dialog.add_response("cancel", &gettext("Cancel"));
    dialog.add_response("uninstall", &gettext("Uninstall"));
    dialog.set_response_appearance("uninstall", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.connect_response(Some("uninstall"), move |_, resp| {
        if resp == "uninstall" {
            if let Err(e) = crate::triggers::uninstall(&id) {
                log::warn!("triggers: uninstall failed: {e}");
            }
            rebuild_trigger_rows(&g2, &win, &cfg, &rows2);
            // Drop the removed trigger's keybinding slot immediately.
            std::thread::spawn(crate::keybindings::register_all);
        }
    });
    dialog.present();
}

/// Open the edit dialog for a trigger: word, enable switch, and shortcut
/// capture — for both built-ins and triggers; triggers additionally get an
/// Uninstall response. The clipboard trigger gets extra settings (history
/// limit, pin/delete shortcuts, show hints). Changes apply automatically as
/// the user edits (word changes are debounced and validated).
fn edit_trigger_dialog(
    window: &adw::PreferencesWindow,
    g: &adw::PreferencesGroup,
    config: &Rc<RefCell<Config>>,
    rows: &Rc<RefCell<Vec<TriggerRow>>>,
    id: &str,
    is_installed: bool,
) {
    let id = id.to_string();
    let is_clipboard = id == "clipboard";
    let is_find = id == "files";
    let (name, word, shortcut, enabled, description): (String, String, String, bool, String) =
        if is_installed {
            match crate::triggers::by_id(&id) {
                Some(a) => (a.name, a.word, a.shortcut, a.enabled, a.description),
                None => return,
            }
        } else {
            match config
                .borrow()
                .command_keywords
                .iter()
                .find(|k| k.id == id)
            {
                Some(k) => (
                    capitalized(&k.id),
                    k.word.clone(),
                    k.shortcut.clone(),
                    k.enabled,
                    k.description.clone(),
                ),
                None => return,
            }
        };

    // The word this trigger ships with by default — the reset button restores
    // this, so it must be the built-in default, not the current (edited) word.
    let default_word = if is_installed {
        crate::triggers::by_id(&id)
            .map(|m| {
                if m.default_word.is_empty() {
                    m.word.clone()
                } else {
                    m.default_word.clone()
                }
            })
            .unwrap_or_else(|| word.clone())
    } else {
        Config::default()
            .command_keywords
            .iter()
            .find(|k| k.id == id)
            .map(|k| k.word.clone())
            .unwrap_or_else(|| word.clone())
    };

    // A libadwaita preferences dialog, like the result types' popups: every
    // change saves as it is made, so there is nothing to confirm or cancel.
    let dialog = adw::PreferencesDialog::builder()
        .title(&name)
        .content_width(480)
        .build();
    let content = adw::PreferencesPage::new();

    // ── Trigger group ─────────────────────────────────────────────────
    let trigger_group = adw::PreferencesGroup::new();
    if is_installed && !description.is_empty() {
        trigger_group.set_description(Some(&description));
    }
    let enabled_row = adw::SwitchRow::builder()
        .title(gettext("Enabled"))
        .subtitle(gettext("Pause this trigger without uninstalling it"))
        .active(enabled)
        .use_markup(false)
        .build();
    trigger_group.add(&enabled_row);
    let entry = adw::EntryRow::builder()
        .title(gettext("Word"))
        .text(&word)
        .use_markup(false)
        .build();
    trigger_group.add(&entry);
    content.add(&trigger_group);

    // ── Pending state (auto-saved on every change) ─────────────────────
    let pending_shortcut: Rc<RefCell<String>> = Rc::new(RefCell::new(shortcut.clone()));
    let pending_enabled: Rc<RefCell<bool>> = Rc::new(RefCell::new(enabled));
    let pending_limit: Rc<RefCell<f64>> = Rc::new(RefCell::new(config.borrow().clipboard_history_limit as f64));
    let pending_pin_shortcut: Rc<RefCell<String>> = Rc::new(RefCell::new(config.borrow().clipboard_pin_shortcut.clone()));
    let pending_show_hints: Rc<RefCell<bool>> = Rc::new(RefCell::new(config.borrow().show_shortcut_hints));
    let pending_terminal: Rc<RefCell<String>> = Rc::new(RefCell::new(config.borrow().terminal_shortcut.clone()));
    let pending_delete_file: Rc<RefCell<String>> = Rc::new(RefCell::new(config.borrow().delete_file_shortcut.clone()));
    let pending_open_location: Rc<RefCell<String>> = Rc::new(RefCell::new(config.borrow().open_location_shortcut.clone()));

    // Apply all pending values to config / manifest immediately.
    let apply: Rc<dyn Fn()> = {
        let win = window.clone();
        let cfg = config.clone();
        let g2 = g.clone();
        let rows2 = rows.clone();
        let id_c = id.clone();
        let entry_c = entry.clone();
        let pending_shortcut_c = pending_shortcut.clone();
        let pending_enabled_c = pending_enabled.clone();
        let pending_pin_c = pending_pin_shortcut.clone();
        let pending_limit_c = pending_limit.clone();
        let pending_show_hints_c = pending_show_hints.clone();
        let pending_terminal_c = pending_terminal.clone();
        let pending_delete_file_c = pending_delete_file.clone();
        let pending_open_location_c = pending_open_location.clone();
        Rc::new(move || {
            let new_word = entry_c.text().trim().to_lowercase();
            let new_shortcut = pending_shortcut_c.borrow().clone();
            let new_enabled = *pending_enabled_c.borrow();
            if is_installed {
                if let Err(e) = crate::triggers::update_manifest(
                    &id_c,
                    &new_word,
                    &new_shortcut,
                    new_enabled,
                ) {
                    log::warn!("triggers: cannot update manifest: {e}");
                    return;
                }
                std::thread::spawn(crate::keybindings::register_all);
            } else {
                let mut c = cfg.borrow_mut();
                if let Some(k) = c.command_keywords.iter_mut().find(|k| k.id == id_c) {
                    k.word = new_word;
                    k.shortcut = new_shortcut;
                    k.enabled = new_enabled;
                }
                if is_clipboard {
                    c.clipboard_pin_shortcut = pending_pin_c.borrow().clone();
                    c.clipboard_history_limit = *pending_limit_c.borrow() as usize;
                    c.show_shortcut_hints = *pending_show_hints_c.borrow();
                }
                if is_find {
                    c.terminal_shortcut = pending_terminal_c.borrow().clone();
                    c.delete_file_shortcut = pending_delete_file_c.borrow().clone();
                    c.open_location_shortcut = pending_open_location_c.borrow().clone();
                }
                c.save();
                std::thread::spawn(crate::keybindings::register_all);
            }
            rebuild_trigger_rows(&g2, &win, &cfg, &rows2);
        })
    };

    // Enabled switch → pending state → the shared auto-save path.
    {
        let pending = pending_enabled.clone();
        let apply2 = apply.clone();
        enabled_row.connect_active_notify(move |r| {
            *pending.borrow_mut() = r.is_active();
            apply2();
        });
    }

    // ── Shortcut: in the trigger group, under the word ───────────────
    let shortcut_default = Config::default()
        .command_keywords
        .iter()
        .find(|k| k.id == id)
        .map(|k| k.shortcut.clone())
        .unwrap_or_else(|| shortcut.clone());
    let (short_row, _) = capture_shortcut_row(
        &dialog,
        &gettext("Shortcut"),
        &name,
        pending_shortcut.clone(),
        apply.clone(),
        &shortcut_default,
    );
    trigger_group.add(&short_row);
    // Reachable by its word — and, if the user wants, from the regular search.
    if id != "cmd" {
        let (regular_row, _) = regular_search_row(config, &id);
        trigger_group.add(&regular_row);
    }

    if is_clipboard {
        let clip_group = adw::PreferencesGroup::builder().title(gettext("Clipboard")).build();
        content.add(&clip_group);

        // History Limit
        let limit_label = gtk::Label::new(Some(
            &(config.borrow().clipboard_history_limit).to_string(),
        ));
        let limit_adj = gtk::Adjustment::new(
            config.borrow().clipboard_history_limit as f64,
            10.0,
            2000.0,
            50.0,
            100.0,
            0.0,
        );
        let limit_spin = gtk::SpinButton::builder()
            .adjustment(&limit_adj)
            .numeric(true)
            .valign(gtk::Align::Center)
            .width_chars(5)
            .build();
        {
            let cfg = config.clone();
            let lbl = limit_label.clone();
            let pending = pending_limit.clone();
            limit_spin.connect_value_changed(move |s| {
                let v = s.value() as usize;
                *pending.borrow_mut() = v as f64;
                cfg.borrow_mut().clipboard_history_limit = v;
                cfg.borrow().save();
                lbl.set_text(&v.to_string());
            });
        }
        let limit_row = adw::ActionRow::builder()
            .title(gettext("History Limit"))
            .subtitle(gettext("Maximum number of clipboard entries to keep"))
            .use_markup(false)
            .build();
        limit_row.add_suffix(&limit_label);
        limit_row.add_suffix(&limit_spin);
        clip_group.add(&limit_row);

        // Retention Period
        let retention_options = ["Forever", "7 days", "30 days", "90 days", "1 year"];
        let retention_values: [Option<u64>; 5] = [None, Some(7), Some(30), Some(90), Some(365)];
        let retention_model = gtk::StringList::new(&retention_options);
        let current_retention_idx = config.borrow().clipboard_retention_days
            .and_then(|d| retention_values.iter().position(|v| *v == Some(d)))
            .unwrap_or(0);
        let retention_combo = adw::ComboRow::builder()
            .title(gettext("Keep history"))
            .subtitle(gettext("Delete clipboard entries older than this (pinned items are exempt)"))
            .model(&retention_model)
            .selected(current_retention_idx as u32)
            .use_markup(false)
            .build();
        {
            let cfg = config.clone();
            retention_combo.connect_selected_notify(move |row| {
                let idx = row.selected() as usize;
                if let Some(val) = retention_values.get(idx) {
                    cfg.borrow_mut().clipboard_retention_days = *val;
                    cfg.borrow().save();
                }
            });
        }
        clip_group.add(&retention_combo);

        // Show Keyboard Shortcuts
        let hints_row = adw::SwitchRow::builder()
            .title(gettext("Show Keyboard Shortcuts"))
            .subtitle(gettext("Display a hint bar in the clipboard manager"))
            .active(config.borrow().show_shortcut_hints)
            .use_markup(false)
            .build();
        {
            let pending = pending_show_hints.clone();
            let cfg = config.clone();
            hints_row.connect_active_notify(move |r| {
                *pending.borrow_mut() = r.is_active();
                cfg.borrow_mut().show_shortcut_hints = r.is_active();
                cfg.borrow().save();
            });
        }
        clip_group.add(&hints_row);

        // Pin Shortcut
        let (_pin_row, _) = capture_shortcut_row(
            &dialog,
            "Pin / Unpin Shortcut",
            "",
            pending_pin_shortcut.clone(),
            apply.clone(),
            &Config::default().clipboard_pin_shortcut,
        );
        clip_group.add(&_pin_row);
    }

    if is_find {
        let find_group = adw::PreferencesGroup::builder().title(gettext("Find")).build();
        content.add(&find_group);

        let root_row = adw::SwitchRow::builder()
            .title(gettext("Root Path Browsing"))
            .subtitle(gettext("Allow typing / or ~/ to browse the filesystem directly"))
            .active(config.borrow().enable_root_browsing)
            .use_markup(false)
            .build();
        {
            let cfg = config.clone();
            root_row.connect_active_notify(move |r| {
                let mut c = cfg.borrow_mut();
                c.enable_root_browsing = r.is_active();
                c.save();
            });
        }
        find_group.add(&root_row);

        let recent_row = adw::SwitchRow::builder()
            .title(gettext("Recent File/Folder Searches"))
            .subtitle(gettext("Show recently opened files and folders"))
            .active(config.borrow().show_recent_file_searches)
            .use_markup(false)
            .build();
        {
            let cfg = config.clone();
            recent_row.connect_active_notify(move |r| {
                let mut c = cfg.borrow_mut();
                c.show_recent_file_searches = r.is_active();
                c.save();
            });
        }
        find_group.add(&recent_row);

        // Result shortcuts for Find mode: delete and open-location sit with
        // the terminal row — all three save through the dialog's apply.
        let (delete_row, _) = capture_shortcut_row(
            &dialog,
            &gettext("Delete file / folder"),
            "",
            pending_delete_file.clone(),
            apply.clone(),
            &Config::default().delete_file_shortcut,
        );
        find_group.add(&delete_row);

        let (open_loc_row, _) = capture_shortcut_row(
            &dialog,
            &gettext("Open location in file manager"),
            "",
            pending_open_location.clone(),
            apply.clone(),
            &Config::default().open_location_shortcut,
        );
        find_group.add(&open_loc_row);

        // Terminal shortcut
        let (terminal_row, _) = capture_shortcut_row(
            &dialog,
            &gettext("Open folder in terminal"),
            "",
            pending_terminal.clone(),
            apply.clone(),
            &Config::default().terminal_shortcut,
        );
        find_group.add(&terminal_row);
    }

    dialog.add(&content);

    // Trigger word: reset button appears only once the word differs from
    // the default; auto-save, debounced so intermediate keystrokes ("f"
    // while typing "find") never persist. Invalid words are reverted.
    {
        let original_word = default_word;
        let reset_btn = gtk::Button::builder()
            .icon_name("edit-undo-symbolic")
            .css_classes(["flat", "circular"])
            .tooltip_text(gettext("Reset to Default"))
            .valign(gtk::Align::Center)
            .build();
        reset_btn.set_visible(word.trim().to_lowercase() != original_word.trim().to_lowercase());
        entry.add_suffix(&reset_btn);
        {
            let entry_c = entry.clone();
            let reset_c = reset_btn.clone();
            let original_c = original_word.clone();
            let apply_c = apply.clone();
            reset_btn.connect_clicked(move |_| {
                entry_c.set_text(&original_c);
                reset_c.set_visible(false);
                apply_c();
            });
        }

        let entry_c = entry.clone();
        let entry_cl = entry.clone();
        let entry_changed = entry.clone();
        let apply_c = apply.clone();
        let win_c = dialog.clone();
        let id_c = id.clone();
        let cfg_c = config.clone();
        let reset_c = reset_btn.clone();
        let original_c = original_word.clone();
        let last_valid: Rc<RefCell<String>> = Rc::new(RefCell::new(word.clone()));
        let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        entry_changed.connect_changed(move |_| {
            reset_c.set_visible(entry_c.text().trim().to_lowercase() != original_c.trim().to_lowercase());
            if let Some(sid) = debounce.borrow_mut().take() {
                sid.remove();
            }
            let entry_d = entry_cl.clone();
            let apply_d = apply_c.clone();
            let win_d = win_c.clone();
            let id_d = id_c.clone();
            let cfg_d = cfg_c.clone();
            let last_d = last_valid.clone();
            let debounce_d = debounce.clone();
            let sid = glib::timeout_add_local_once(
                std::time::Duration::from_millis(500),
                move || {
                    // This one-shot timer has fired; drop the (now dead)
                    // SourceId so a later `changed` can't remove it again.
                    *debounce_d.borrow_mut() = None;
                    let new_word = entry_d.text().trim().to_lowercase();
                    if new_word.is_empty() {
                        win_d.add_toast(adw::Toast::new(&gettext("Trigger word cannot be empty")));
                        entry_d.set_text(&last_d.borrow());
                        return;
                    }
                    if new_word != *last_d.borrow() && word_taken(&id_d, &new_word, &cfg_d) {
                        let msg = gettext("Trigger word \"{word}\" is already in use").replace("{word}", &new_word);
                        win_d.add_toast(adw::Toast::new(&msg));
                        entry_d.set_text(&last_d.borrow());
                        return;
                    }
                    *last_d.borrow_mut() = new_word.clone();
                    apply_d();
                },
            );
            *debounce.borrow_mut() = Some(sid);
        });
    }

    // Last: Uninstall. A repository trigger goes through its confirmation;
    // a built-in one leaves the list and waits in the Store.
    if id != "cmd" {
        let g_c = g.clone();
        let cfg_c = config.clone();
        let rows_c = rows.clone();
        let id_c = id.clone();
        content.add(&uninstall_group(&dialog, &name, move |win| {
            let Some(win) = win else { return };
            if is_installed {
                confirm_uninstall(&win, &g_c, &cfg_c, &rows_c, &id_c);
            } else {
                uninstall_builtin_from_list(&win, &cfg_c, &id_c);
            }
        }));
    }

    dialog.present(Some(window));
}

/// True when another trigger (built-in or trigger) already uses `word`.
fn word_taken(id: &str, word: &str, config: &Rc<RefCell<Config>>) -> bool {
    let c = config.borrow();
    c.command_keywords
        .iter()
        .any(|k| k.id != id && k.word.eq_ignore_ascii_case(word))
        || crate::triggers::all()
            .iter()
            .any(|a| a.id != id && a.word.eq_ignore_ascii_case(word))
}


// ──────────────────────────────────────────────────────────────────────
// About
// ──────────────────────────────────────────────────────────────────────
/// Present the native `adw::AboutDialog` for Spotty.
fn present_about_dialog(parent: &impl IsA<gtk::Window>) {
    let about = adw::AboutDialog::builder()
        .application_name("Spotty")
        .application_icon("com.spotty.Spotty")
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("The Spotty Project")
        .comments("A Raycast-style launcher for GNOME Linux")
        .license_type(gtk::License::MitX11)
        .website("https://github.com/Aras1907/Spotty")
        .issue_url("https://github.com/Aras1907/Spotty/issues")
        .build();
    about.add_link("Trigger repository", crate::triggers::REPO_URL);
    about.present(Some(parent.upcast_ref::<gtk::Window>().upcast_ref::<gtk::Widget>()));
}

/// Pop up a dialog to add a custom command: name, description, command,
/// and an optional GNOME-style keyboard shortcut captured by key press.
/// A shortcut row with a conditional "Reset" (to the row's default) + "Set"
/// (key-capture) buttons, shared by the settings edits and the trigger edit
/// window.
///
/// `parent` is the widget the temporary key controller is attached to (the dialog
/// or window). `pending` holds the current combo; every successful capture and
/// every reset is applied to it. `on_change` is called afterwards so the caller
/// can persist the value. `default` is the row's built-in default accelerator;
/// the Reset button is only shown while the pending value differs from it
/// (i.e. the shortcut has been customized), and restores `default` when clicked.
/// Read one of the in-window shortcut config fields by name, falling back to
/// the given default when the field is missing/empty (config is old, or the
/// row was reset and never set).
fn shortcut_value(cfg: &Rc<RefCell<Config>>, field: &str, default: &str) -> String {
    let c = cfg.borrow();
    let s = match field {
        "operations" => c.operations_shortcut.clone(),
        "hints" => c.hints_shortcut.clone(),
        "copy" => c.copy_shortcut.clone(),
        "cut" => c.cut_shortcut.clone(),
        "paste" => c.paste_shortcut.clone(),
        "terminal" => c.terminal_shortcut.clone(),
        "open_location" => c.open_location_shortcut.clone(),
        "private_search" => c.private_search_shortcut.clone(),
        "delete_file" => c.delete_file_shortcut.clone(),
        "uninstall" => c.uninstall_shortcut.clone(),
        "kill" => c.kill_shortcut.clone(),
        "select_all" => c.select_all_shortcut.clone(),
        "undo" => c.undo_shortcut.clone(),
        "redo" => c.redo_shortcut.clone(),
        "delete_word" => c.delete_word_shortcut.clone(),
        _ => String::new(),
    };
    if s.is_empty() {
        default.to_string()
    } else {
        s
    }
}

/// Write one of the in-window shortcut config fields by name. The capture
/// UI hands over "Ctrl+D"-style text while `hit()` parses GTK accelerators
/// ("<Control>d"), so normalize here — without it a re-set shortcut would
/// silently stop matching.
fn set_shortcut(cfg: &mut Config, field: &str, value: &str) {
    let v = normalize_accel(value);
    match field {
        "operations" => cfg.operations_shortcut = v,
        "hints" => cfg.hints_shortcut = v,
        "copy" => cfg.copy_shortcut = v,
        "cut" => cfg.cut_shortcut = v,
        "paste" => cfg.paste_shortcut = v,
        "terminal" => cfg.terminal_shortcut = v,
        "open_location" => cfg.open_location_shortcut = v,
        "private_search" => cfg.private_search_shortcut = v,
        "delete_file" => cfg.delete_file_shortcut = v,
        "uninstall" => cfg.uninstall_shortcut = v,
        "kill" => cfg.kill_shortcut = v,
        "select_all" => cfg.select_all_shortcut = v,
        "undo" => cfg.undo_shortcut = v,
        "redo" => cfg.redo_shortcut = v,
        "delete_word" => cfg.delete_word_shortcut = v,
        _ => {}
    }
}

/// Human ("Ctrl+Shift+Z") -> GTK ("<Control><Shift>Z") accelerator form;
/// values already in GTK form pass through unchanged.
/// A stored shortcut ("Super+Space", "<Control>z") as an accelerator GTK can
/// parse, for display. GTK key names are case-sensitive ("space", "Return"),
/// and the stored human form isn't always spelled that way: try as written,
/// then with the key lowercased. Empty (or unparseable) shows as disabled.
fn gtk_accelerator(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let accel = normalize_accel(value);
    if gtk::accelerator_parse(&accel).is_some() {
        return accel;
    }
    let (mods, key) = match accel.rfind('>') {
        Some(i) => accel.split_at(i + 1),
        None => ("", accel.as_str()),
    };
    let lower = format!("{mods}{}", key.to_lowercase());
    if gtk::accelerator_parse(&lower).is_some() {
        lower
    } else {
        String::new()
    }
}

pub(crate) fn normalize_accel(s: &str) -> String {
    // Already GTK form ("<Control><Shift>z") — decided by looking at the
    // string rather than by asking GTK, so this stays usable (and testable)
    // before/without a display.
    if s.contains('<') && s.contains('>') {
        return s.to_string();
    }
    let mut out = String::new();
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "primary" => out.push_str("<Control>"),
            "super" | "meta" | "win" | "logo" => out.push_str("<Super>"),
            "shift" => out.push_str("<Shift>"),
            "alt" => out.push_str("<Alt>"),
            _ => out.push_str(part),
        }
    }
    out
}

/// Web/Rust accelerator form ("<Control><Shift>z") into the human form the
/// rest of the UI displays ("Ctrl+Shift+Z"). Falls back to the raw string.
fn display_shortcut(s: &str) -> String {
    if let Some((key, state)) = gtk::accelerator_parse(s) {
        let combo = key_combo_string(key, state);
        if !combo.is_empty() {
            return combo;
        }
    }
    s.to_string()
}

fn capture_shortcut_row(
    parent: &impl IsA<gtk::Widget>,
    title: &str,
    feature: &str,
    pending: Rc<RefCell<String>>,
    on_change: Rc<dyn Fn()>,
    default: &str,
) -> (adw::ActionRow, gtk::Button) {
    // GNOME Settings' keyboard-shortcut row: the whole row opens the capture
    // dialog, the shortcut sits on the right as keycaps (or "Disabled"), and a
    // reset icon appears once it differs from the default.
    let row = adw::ActionRow::builder()
        .title(title)
        .activatable(true)
        .use_markup(false)
        .build();
    let label = gtk::ShortcutLabel::builder()
        .disabled_text(gettext("Disabled"))
        .valign(gtk::Align::Center)
        .build();
    let show = {
        let label = label.clone();
        move |value: &str| label.set_accelerator(&gtk_accelerator(value))
    };
    show(&pending.borrow());
    let reset_btn = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .tooltip_text(if default.is_empty() {
            gettext("Remove Shortcut")
        } else {
            gettext("Reset to Default")
        })
        .build();
    reset_btn.set_visible(pending.borrow().as_str() != default);
    row.add_suffix(&label);
    row.add_suffix(&reset_btn);
    {
        let pending_c = pending.clone();
        let reset_c = reset_btn.clone();
        let on_change_c = on_change.clone();
        let default_c = default.to_string();
        let show = show.clone();
        reset_btn.connect_clicked(move |_| {
            *pending_c.borrow_mut() = default_c.clone();
            show(&default_c);
            reset_c.set_visible(false);
            on_change_c();
        });
    }
    {
        // Activating the row → GNOME's "Set Shortcut" capture dialog.
        let parent_s = parent.clone();
        // The capture dialog names what is being changed: the feature when
        // the row's own title is generic ("Shortcut"), else the title.
        let title_s = if feature.is_empty() { title } else { feature }.to_string();
        let reset_s = reset_btn.clone();
        let default_s = default.to_string();
        row.connect_activated(move |_| {
            let pending_k = pending.clone();
            let reset_k = reset_s.clone();
            let on_change_k = on_change.clone();
            let default_k = default_s.clone();
            let show_k = show.clone();
            let on_accept: Rc<dyn Fn(Option<String>)> = Rc::new(move |captured| {
                // `None` is Backspace in the dialog: the shortcut is disabled.
                let value = captured.unwrap_or_default();
                *pending_k.borrow_mut() = value.clone();
                show_k(&value);
                reset_k.set_visible(value != default_k);
                on_change_k();
            });
            shortcut_capture_dialog(&parent_s, &title_s, on_accept);
        });
    }
    (row, reset_btn)
}

/// GNOME's "Set Shortcut" dialog: a modal window that waits for the next key
/// press, dressed like GNOME's own shortcut editors — a centered "Set Shortcut"
/// title with a round × close, an illustration of three arrows raining keys
/// down onto a pile of keycaps, and no buttons anywhere.
///
/// * modifiers alone are shown as they are pressed, but never accepted
/// * Esc (or the ×) cancels, Backspace disables the shortcut
/// * a valid combination applies itself immediately and closes the dialog
///
/// `on_accept` gets `Some(accelerator)` for a new combination and `None` when
/// the shortcut was disabled. The value is in human form ("Ctrl+Shift+T"), the
/// same form the rows display — [`set_shortcut`] normalises it for the config.
/// `feature` is the raw feature name, shown bold in the caption line.
fn shortcut_capture_dialog(
    parent: &impl IsA<gtk::Widget>,
    feature: &str,
    on_accept: Rc<dyn Fn(Option<String>)>,
) {
    let mut builder = adw::Window::builder()
        .modal(true)
        .resizable(false)
        .default_width(404);
    if let Some(host) = parent_window(parent) {
        builder = builder.transient_for(&host);
    }
    let window = builder.build();

    // Chrome: the dialog's name dead center, a circular × to close it, and
    // nothing else — Esc cancels, Backspace disables, and a valid
    // combination is its own confirmation.
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .css_classes(["flat", "spotty-close-round"])
        .tooltip_text(gettext("Close"))
        .valign(gtk::Align::Center)
        .build();
    {
        let win = window.clone();
        close.connect_clicked(move |_| win.close());
    }
    let title = gtk::Label::builder()
        .label(gettext("Set Shortcut"))
        .css_classes(["spotty-shortcut-title"])
        .halign(gtk::Align::Center)
        .hexpand(true)
        .build();
    // A spacer as wide as the round × keeps the title truly centered.
    let title_spacer = gtk::Box::builder().width_request(22).build();
    let title_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_start(12)
        .margin_end(12)
        .margin_top(8)
        .build();
    title_row.append(&title_spacer);
    title_row.append(&title);
    title_row.append(&close);

    // "Enter new shortcut to change **Feature name**" — the template keeps
    // the sentence whole for translators; the name arrives bold and escaped.
    // Left-aligned, like the reference dialog: only the title is centered.
    let caption = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .justify(gtk::Justification::Left)
        .margin_start(28)
        .margin_end(28)
        .margin_top(20)
        .margin_bottom(4)
        .build();
    caption.set_markup(&shortcut_caption(
        &gettext("Enter new shortcut to change {name}"),
        feature,
    ));

    // Waiting: GNOME's keys-and-arrows illustration. The stack is homogeneous
    // in both directions, so switching to the captured keycaps doesn't resize
    // the dialog.
    let waiting = shortcut_illustration();
    waiting.set_margin_top(40);

    let keycaps = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    let stage = gtk::Stack::builder()
        .hhomogeneous(true)
        .vhomogeneous(true)
        .build();
    stage.add_named(&waiting, Some("waiting"));
    stage.add_named(&keycaps, Some("captured"));
    stage.set_visible_child_name("waiting");

    let hint = gtk::Label::builder()
        .label(gettext("Press Esc to cancel or Backspace to disable the keyboard shortcut"))
        .wrap(true)
        .xalign(0.0)
        .justify(gtk::Justification::Left)
        .css_classes(["spotty-shortcut-hint"])
        .margin_start(28)
        .margin_end(28)
        .margin_top(34)
        .margin_bottom(28)
        .build();
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Fill)
        .build();
    body.append(&title_row);
    body.append(&caption);
    body.append(&stage);
    body.append(&hint);
    window.set_content(Some(&body));

    let kc = gtk::EventControllerKey::new();
    {
        let win = window.clone();
        let stage_k = stage.clone();
        let keycaps_k = keycaps.clone();
        let on_accept_k = on_accept.clone();
        kc.connect_key_pressed(move |_, key, _, state| {
            use gtk::gdk::{Key, ModifierType};
            // Esc cancels; Backspace disables — GNOME's two escape hatches.
            if key == Key::Escape {
                win.close();
                return glib::Propagation::Stop;
            }
            if key == Key::BackSpace {
                on_accept_k(None);
                win.close();
                return glib::Propagation::Stop;
            }
            let default_mods = gtk::accelerator_get_default_mod_mask();
            let mods = state & default_mods;
            // Modifiers on their own are not a shortcut: show them, keep
            // waiting, exactly like GNOME does.
            if is_modifier_key(key) {
                let mut parts: Vec<String> = Vec::new();
                for (mask, label) in [
                    (ModifierType::CONTROL_MASK, "Ctrl"),
                    (ModifierType::ALT_MASK, "Alt"),
                    (ModifierType::SHIFT_MASK, "Shift"),
                    (ModifierType::SUPER_MASK, "Super"),
                    (ModifierType::META_MASK, "Meta"),
                ] {
                    if state.contains(mask) {
                        parts.push(label.to_string());
                    }
                }
                // A lock key on its own (Caps Lock…) is not worth replacing
                // the illustration for.
                if !parts.is_empty() {
                    stage_k.set_visible_child_name("captured");
                    render_keycaps(&keycaps_k, parts, true);
                }
                return glib::Propagation::Stop;
            }
            // A shortcut needs a modifier, or a key that stands on its own
            // (the function row, the media keys) — `accelerator_valid` says
            // yes to a bare letter, so this is the line gnome-control-center
            // draws: a stray keypress must never capture anything.
            if mods.is_empty() && !key_stands_alone(key) {
                return glib::Propagation::Stop;
            }
            // GTK's own exclusions: Tab, the lock and ISO keys — never an
            // accelerator, whatever else is held.
            if !gtk::accelerator_valid(key, mods) {
                return glib::Propagation::Stop;
            }
            let combo = key_combo_string(key, state);
            if combo.is_empty() {
                return glib::Propagation::Stop;
            }
            // No buttons in this dialog: a valid combination confirms itself,
            // exactly like GNOME's — apply it and close.
            on_accept_k(Some(combo));
            win.close();
            glib::Propagation::Stop
        });
    }
    // Letting go of the last modifier returns to the illustration; keeping
    // one held keeps its keycaps on screen.
    {
        let stage_k = stage.clone();
        let keycaps_k = keycaps.clone();
        kc.connect_key_released(move |_, key, _, state| {
            let Some(mask) = modifier_mask(key) else {
                return;
            };
            // Whether the event's state still carries the released bit or
            // not, stripping it here gives the same answer: what is left.
            let held = state & gtk::accelerator_get_default_mod_mask() & !mask;
            if held.is_empty() {
                stage_k.set_visible_child_name("waiting");
                return;
            }
            let mut parts: Vec<String> = Vec::new();
            for (m, label) in [
                (gtk::gdk::ModifierType::CONTROL_MASK, "Ctrl"),
                (gtk::gdk::ModifierType::ALT_MASK, "Alt"),
                (gtk::gdk::ModifierType::SHIFT_MASK, "Shift"),
                (gtk::gdk::ModifierType::SUPER_MASK, "Super"),
                (gtk::gdk::ModifierType::META_MASK, "Meta"),
            ] {
                if held.contains(m) {
                    parts.push(label.to_string());
                }
            }
            render_keycaps(&keycaps_k, parts, true);
        });
    }
    window.add_controller(kc);
    window.present();
}

/// Whether a pressed key is only a modifier — never a shortcut on its own.
fn is_modifier_key(key: gtk::gdk::Key) -> bool {
    use gtk::gdk::Key;
    matches!(
        key,
        Key::Control_L
            | Key::Control_R
            | Key::Shift_L
            | Key::Shift_R
            | Key::Alt_L
            | Key::Alt_R
            | Key::Super_L
            | Key::Super_R
            | Key::Meta_L
            | Key::Meta_R
            | Key::Caps_Lock
            | Key::Num_Lock
            | Key::Scroll_Lock
            | Key::ISO_Level3_Shift
    )
}

/// The mask a released key contributes to a shortcut's modifier set. The lock
/// keys contribute nothing, so their releases are ignored.
fn modifier_mask(key: gtk::gdk::Key) -> Option<gtk::gdk::ModifierType> {
    use gtk::gdk::{Key, ModifierType};
    match key {
        Key::Control_L | Key::Control_R => Some(ModifierType::CONTROL_MASK),
        Key::Shift_L | Key::Shift_R => Some(ModifierType::SHIFT_MASK),
        Key::Alt_L | Key::Alt_R => Some(ModifierType::ALT_MASK),
        Key::Super_L | Key::Super_R => Some(ModifierType::SUPER_MASK),
        Key::Meta_L | Key::Meta_R => Some(ModifierType::META_MASK),
        _ => None,
    }
}

/// Whether a key may form a shortcut with no modifier held: keys that have no
/// character of their own — the function row, Print, the media keys — while
/// printable keys and arrows always need a modifier. That is the line
/// gnome-control-center draws: a stray keypress must never capture.
fn key_stands_alone(key: gtk::gdk::Key) -> bool {
    use gtk::gdk::Key;
    key.to_unicode().is_none()
        && !matches!(key, Key::Left | Key::Right | Key::Up | Key::Down)
}

/// The dialog's illustration, drawn to GNOME's measurements: three staggered
/// rows of seven rounded keys (28×13, a 34px pitch, each row 10px lower and
/// 6px further right) with three straight ↓ arrows dropping onto them in turn.
/// Keys are painted back to front, each first clearing a 2px outline into the
/// rows behind it, so the gaps show the window through in either theme.
fn shortcut_illustration() -> gtk::DrawingArea {
    const HEAP_W: f64 = 244.0;
    const HEAP_TOP: f64 = 44.0;
    // Arrow stems relative to the heap's left edge, as in the reference.
    const ARROWS_X: [f64; 3] = [47.5, 117.5, 219.0];
    let area = gtk::DrawingArea::builder()
        .content_height(80)
        .hexpand(true)
        .build();
    area.set_draw_func(|w, cr, width, _| {
        let fg = w.color();
        let ox = ((width as f64 - HEAP_W) / 2.0).round();
        // Seconds since the dialog's frame clock started; 0 before mapping.
        let now = w
            .frame_clock()
            .map_or(0.0, |c| c.frame_time() as f64 / 1_000_000.0);

        cr.push_group();
        for row in 0..3 {
            for k in 0..7 {
                let x = ox + 6.0 * row as f64 + 34.0 * k as f64;
                let y = HEAP_TOP + 10.0 * row as f64;
                keycap_path(cr, x, y);
                cr.set_operator(gtk::cairo::Operator::Clear);
                cr.set_line_width(4.0);
                let _ = cr.stroke_preserve();
                cr.set_operator(gtk::cairo::Operator::Over);
                cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, 1.0);
                let _ = cr.fill();
            }
        }
        let _ = cr.pop_group_to_source();
        let _ = cr.paint_with_alpha(fg.alpha() as f64);

        cr.set_line_width(2.5);
        for (i, ax) in ARROWS_X.iter().enumerate() {
            let (dy, alpha) = arrow_phase(now, i);
            let x = ox + ax;
            let tip = HEAP_TOP - 7.0 + dy;
            cr.set_source_rgba(
                fg.red() as f64,
                fg.green() as f64,
                fg.blue() as f64,
                fg.alpha() as f64 * alpha,
            );
            cr.move_to(x, tip - 17.0);
            cr.line_to(x, tip - 1.0);
            let _ = cr.stroke();
            cr.move_to(x - 8.0, tip - 8.5);
            cr.line_to(x, tip - 0.5);
            cr.line_to(x + 8.0, tip - 8.5);
            let _ = cr.stroke();
        }
    });
    // Redraw every frame while shown; the callback dies with the widget.
    area.add_tick_callback(|w, _| {
        w.queue_draw();
        glib::ControlFlow::Continue
    });
    area
}

/// Arrow `i` at `now` seconds as (y offset, opacity): each loops over 1.8s,
/// a third of a cycle behind its left neighbour — fade in 10px up, ease down
/// to rest 7px above the keys, hold, fade out.
fn arrow_phase(now: f64, i: usize) -> (f64, f64) {
    let t = (now / 1.8 + 1.0 - i as f64 / 3.0).fract();
    let fall = (t / 0.45).min(1.0);
    let dy = -10.0 * (1.0 - fall).powi(2);
    let alpha = if t < 0.15 {
        t / 0.15
    } else if t > 0.8 {
        (1.0 - t) / 0.2
    } else {
        1.0
    };
    (dy, alpha)
}

/// One 28×13 keycap seen from the front: a top edge dipping 1.3px in the
/// middle, sides flaring 1px outward, soft bottom corners.
fn keycap_path(cr: &gtk::cairo::Context, x: f64, y: f64) {
    cr.new_path();
    cr.move_to(x + 1.0, y);
    cr.curve_to(x + 9.0, y + 1.8, x + 19.0, y + 1.8, x + 27.0, y);
    cr.line_to(x + 28.5, y + 11.5);
    cr.curve_to(x + 28.8, y + 13.0, x + 28.8, y + 13.0, x + 27.3, y + 13.0);
    cr.line_to(x + 0.7, y + 13.0);
    cr.curve_to(x - 0.8, y + 13.0, x - 0.8, y + 13.0, x - 0.5, y + 11.5);
    cr.close_path();
}

/// The instruction line above the illustration: the template keeps the
/// sentence whole for translators; the feature name arrives bold and escaped.
fn shortcut_caption(template: &str, feature: &str) -> String {
    template.replace("{name}", &format!("<b>{}</b>", escape_markup(feature)))
}

/// Pango markup only needs these three escaped — ampersand first, or the
/// others' replacements would get escaped twice.
fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The window a dialog should be transient for, when there is one.
///
/// Most parents are windows, but the settings popups are
/// `adw::PreferencesDialog`s — which are `GtkNativeDialog`s, and carry their
/// parent window separately.
fn parent_window(parent: &impl IsA<gtk::Widget>) -> Option<gtk::Window> {
    let widget = parent.clone().upcast::<gtk::Widget>();
    if let Some(win) = widget.clone().downcast::<gtk::Window>().ok() {
        return Some(win);
    }
    // A GtkNativeDialog is not a widget, so it cannot be downcast to — but it
    // exposes its host window through its `parent` property.
    widget.property_value("parent").get::<gtk::Window>().ok()
}

/// Replace the keycaps in `box_` with `parts`, one labelled box per key.
fn render_keycaps(box_: &gtk::Box, parts: Vec<String>, muted: bool) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
    if parts.is_empty() {
        let dash = gtk::Label::builder()
            .label("—")
            .css_classes(["spotty-keycap", "spotty-keycap-muted"])
            .build();
        box_.append(&dash);
        return;
    }
    for part in parts {
        let mut label = gtk::Label::builder()
            .label(part)
            .css_classes(["spotty-keycap", "spotty-keycap-label"]);
        if muted {
            label = label.css_classes(["spotty-keycap-muted"]);
        }
        box_.append(&label.build());
    }
}

/// Build a "Super+Ctrl+T"-style string from a key + modifier state.
fn key_combo_string(key: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> String {
    use gtk::gdk::ModifierType;
    let mut parts: Vec<&str> = Vec::new();
    if state.contains(ModifierType::SUPER_MASK) {
        parts.push("Super");
    }
    if state.contains(ModifierType::CONTROL_MASK) {
        parts.push("Ctrl");
    }
    if state.contains(ModifierType::ALT_MASK) {
        parts.push("Alt");
    }
    if state.contains(ModifierType::SHIFT_MASK) {
        parts.push("Shift");
    }
    let key_name = key.name().map(|s| s.to_string()).unwrap_or_default();
    if key_name.is_empty() {
        return String::new();
    }
    // Normalize single letters to uppercase for display
    let key_disp = if key_name.chars().count() == 1 {
        key_name.to_uppercase()
    } else {
        key_name
    };
    let mut combo = parts.join("+");
    if !combo.is_empty() {
        combo.push('+');
    }
    combo.push_str(&key_disp);
    combo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caption_bolds_and_escapes_the_feature_name() {
        // The template stays whole for translators; the feature name is
        // inserted bold, with Pango's three specials escaped — ampersand
        // first, or the others would be double-escaped.
        assert_eq!(
            shortcut_caption("Enter new shortcut to change {name}", "Search privately"),
            "Enter new shortcut to change <b>Search privately</b>"
        );
        assert_eq!(
            shortcut_caption("Change {name} now", "A & B <c>"),
            "Change <b>A &amp; B &lt;c&gt;</b> now"
        );
    }

    #[test]
    fn the_arrows_fall_in_a_left_to_right_cascade() {
        let near = |a: f64, b: f64| (a - b).abs() < 0.05;
        // At 0s the left arrow starts its cycle: invisible, 10px up.
        let (dy, alpha) = arrow_phase(0.0, 0);
        assert!(near(dy, -10.0) && near(alpha, 0.0));
        // The middle and right arrows start a third and two thirds later
        // (just after the start, so they are barely faded in).
        assert!(arrow_phase(0.61, 1).1 < 0.1);
        assert!(arrow_phase(1.21, 2).1 < 0.1);
        // Mid-cycle the left arrow has landed and is fully shown…
        let (dy, alpha) = arrow_phase(0.9, 0);
        assert!(near(dy, 0.0) && near(alpha, 1.0));
        // …and the cycle repeats every 1.8s.
        assert!(near(arrow_phase(1.801, 0).0, -10.0));
    }

    #[test]
    fn only_function_and_media_keys_stand_alone() {
        // The dialog's capture gate: a stray letter, space, or arrow can
        // never become a shortcut — but the function row and the media keys
        // can, with no modifier held.
        use gtk::gdk::Key;
        assert!(key_stands_alone(Key::F5));
        assert!(key_stands_alone(Key::Print));
        let kx = Key::from_name("XF86AudioPlay").unwrap();
        assert!(key_stands_alone(kx));
        assert!(!key_stands_alone(Key::from_name("a").unwrap()));
        assert!(!key_stands_alone(Key::from_name("space").unwrap()));
        assert!(!key_stands_alone(Key::Left));
    }

    #[test]
    fn a_private_search_shortcut_round_trips_through_the_config() {
        // The row writes what the dialog captured, and the config keeps it in
        // the form every matcher parses.
        let mut cfg = Config::default();
        assert_eq!(cfg.private_search_shortcut, "<Control>Return");
        set_shortcut(&mut cfg, "private_search", "Ctrl+Shift+P");
        assert_eq!(cfg.private_search_shortcut, "<Control><Shift>P");
        // …and reading it back through the row's own accessor agrees.
        let cfg_rc = Rc::new(RefCell::new(cfg.clone()));
        assert_eq!(
            shortcut_value(&cfg_rc, "private_search", "<Control>Return"),
            "<Control><Shift>P"
        );
    }

    #[test]
    fn trigger_id_slug_from_name() {
        assert_eq!(trigger_id_from_name("My Cool Trigger"), "my-cool-trigger");
        assert_eq!(trigger_id_from_name("  --weird__  "), "weird");
        assert_eq!(trigger_id_from_name("Updates 2"), "updates-2");
        assert_eq!(trigger_id_from_name("!!!"), "trigger");
    }

    #[test]
    fn install_gate_confirms_shell_and_allows_web_files_and_native() {
        let parse = |body: &str| -> crate::triggers::TriggerManifest {
            serde_json::from_str(body).expect("valid fixture")
        };
        let shell = parse(r#"{"id":"x","name":"X","word":"x","description":"d","action":{"type":"shell","command":"rm -rf ~"}}"#);
        let web = parse(r#"{"id":"y","name":"Y","word":"y","description":"d","action":{"type":"web","url":"https://e/{query}"}}"#);
        let files = parse(r#"{"id":"z","name":"Z","word":"z","description":"d","action":{"type":"files","extensions":["pdf"]}}"#);
        let native = parse(include_str!("../../trigger-backends/triggers/proton-bridge.json"));
        match install_preview(&shell) {
            InstallPreview::Confirm { body, destructive, .. } => {
                assert!(destructive);
                assert!(body.contains("rm -rf ~"));
            }
            _ => panic!("shell triggers must confirm"),
        }
        assert!(matches!(install_preview(&web), InstallPreview::Direct));
        assert!(matches!(install_preview(&files), InstallPreview::Direct));
        assert!(matches!(install_preview(&native), InstallPreview::Direct));
        assert!(serde_json::from_str::<crate::triggers::TriggerManifest>("not json at all").is_err());
    }
}

#[cfg(test)]
mod save_borrow_tests {
    use super::save_then;
    use crate::config::Config;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The crash this fixes: the refresh (the `after` step) borrows the
    /// config while a `borrow_mut` from the save is still alive → panic.
    #[test]
    fn the_after_step_can_borrow_the_config() {
        let path = Config::config_path();
        let before = std::fs::read_to_string(&path).unwrap_or_default();
        let cfg = Rc::new(RefCell::new(Config::load()));
        let original = cfg.borrow().update_notification;

        let seen = save_then(&cfg, |c| c.update_notification = !original, || {
            // Exactly what refresh_search_window does on every refresh.
            cfg.borrow().update_notification
        });
        assert_eq!(seen, !original, "the refresh sees the new value");
        assert!(cfg.try_borrow().is_ok(), "no borrow may leak out of save_then");

        // Leave the real config file exactly as we found it.
        if !before.is_empty() {
            let _ = std::fs::write(&path, before);
        }
    }
}
