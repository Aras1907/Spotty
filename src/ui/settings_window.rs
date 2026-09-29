use crate::config::{CommandKeyword, Config, PackageManager, SearchEngine};
use crate::i18n::gettext;
use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

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
        self.window.present();
        // On Wayland, present() on an already-visible window doesn't reliably
        // grab keyboard focus — grab it on the next main-loop iteration.
        let w = self.window.clone();
        glib::idle_add_local_once(move || {
            w.grab_focus();
        });
    }
}

/// The update section's icon: the update glyph while something is
/// pending, the checkmark when there is nothing to update.
fn status_icon_name(pending: bool) -> &'static str {
    if pending {
        "software-update-available-symbolic"
    } else {
        "object-select-symbolic"
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

    // Result source toggles (merged from the former "Sources" page).
    let sg = adw::PreferencesGroup::builder()
        .title(gettext("Results"))
        .description(gettext("Choose what types of results appear"))
        .build();
    general.add(&sg);
    // Applications: its own switch, and activating the row opens the
    // popup with the shortcuts that act on a selected app in the results.
    {
        let (row, sw) = popup_setting_row(
            &gettext("Applications"),
            &gettext("System, Flatpak and Snap apps"),
            config.borrow().enable_apps,
        );
        {
            let cfg = config.clone();
            sw.connect_active_notify(move |r| {
                let mut c = cfg.borrow_mut();
                c.enable_apps = r.is_active();
                c.save();
            });
        }
        let win = window.clone();
        let cfg = config.clone();
        row.connect_activated(move |_| open_apps_dialog(&win, &cfg));
        sg.add(&row);
    }
    // Search for new apps: its own switch, and activating the row opens
    // the popup that carries the Package Manager choice feeding it.
    {
        let (row, sw) = popup_setting_row(
            &gettext("Search for new Apps"),
            &gettext("Also suggest installable apps as you type"),
            config.borrow().enable_new_apps,
        );
        {
            let cfg = config.clone();
            sw.connect_active_notify(move |r| {
                let mut c = cfg.borrow_mut();
                c.enable_new_apps = r.is_active();
                c.save();
            });
        }
        let win = window.clone();
        let cfg = config.clone();
        row.connect_activated(move |_| open_new_apps_dialog(&win, &cfg));
        sg.add(&row);
    }
    // Calculator & Conversions: its own switch, and activating the row
    // opens the popup with result formatting and the conversions group.
    {
        let (row, sw) = popup_setting_row(
            &gettext("Calculator & Conversions"),
            &gettext("Arithmetic, units, currency and number bases"),
            config.borrow().enable_calculator,
        );
        {
            let cfg = config.clone();
            sw.connect_active_notify(move |r| {
                let mut c = cfg.borrow_mut();
                c.enable_calculator = r.is_active();
                c.save();
            });
        }
        let win = window.clone();
        let cfg = config.clone();
        row.connect_activated(move |_| open_calc_dialog(&win, &cfg));
        sg.add(&row);
    }
    // Web search engine + custom URL (URL only relevant for the Custom engine)
    let custom_web = adw::EntryRow::builder()
        .title(gettext("Custom Search URL"))
        .show_apply_button(true)
        .build();
    custom_web.set_text(&config.borrow().custom_web_search_url);
    custom_web.set_tooltip_text(Some(
        "Use {query} as the placeholder, for example https://example.com/search?q={query}",
    ));

    let eg = adw::PreferencesGroup::builder().title(gettext("Web Search")).build();
    general.add(&eg);
    // The web-search switch lives in its own section together with the
    // engine + custom URL rows, not in the generic results toggles.
    {
        let row = adw::SwitchRow::builder()
            .title(gettext("Web Search"))
            .subtitle(gettext("Always show web search row"))
            .active(config.borrow().enable_web)
            .build();
        let cfg = config.clone();
        row.connect_active_notify(move |r| {
            let mut c = cfg.borrow_mut();
            c.enable_web = r.is_active();
            c.save();
        });
        eg.add(&row);
    }
    let er = adw::ComboRow::builder()
        .title(gettext("Search Engine"))
        .subtitle(gettext("Used for web search results"))
        .build();
    er.set_model(Some(&gtk::StringList::new(
        &SearchEngine::all()
            .iter()
            .map(|e| e.display_name())
            .collect::<Vec<_>>(),
    )));
    if let Some(i) = SearchEngine::all()
        .iter()
        .position(|&e| e == config.borrow().search_engine)
    {
        er.set_selected(i as u32);
    }
    // Show detected engine name in subtitle when Browser Default is selected.
    if config.borrow().search_engine == SearchEngine::BrowserDefault {
        if let Some(name) = crate::search::browser_engine::engine_name() {
            er.set_subtitle(&gettext("Uses {name} (detected from your default browser)").replace("{name}", &name));
        } else {
            er.set_subtitle("Detecting your default browser\u{2026}");
        }
    }
    let cfg = config.clone();
    // The Custom Search URL only makes sense for the Custom engine — the
    // row is hidden entirely for everything else.
    let custom_web_vis = custom_web.clone();
    let er_sub = er.clone();
    let is_custom = |e: SearchEngine| e == SearchEngine::Custom;
    custom_web_vis.set_visible(SearchEngine::all()
        .get(er.selected() as usize)
        .copied()
        .map(is_custom)
        .unwrap_or(false));
    er.connect_selected_notify(move |r| {
        if let Some(&e) = SearchEngine::all().get(r.selected() as usize) {
            let mut c = cfg.borrow_mut();
            c.search_engine = e;
            c.save();
            custom_web_vis.set_visible(is_custom(e));
            // Update subtitle for Browser Default
            if e == SearchEngine::BrowserDefault {
                if let Some(name) = crate::search::browser_engine::engine_name() {
                    er_sub.set_subtitle(&gettext("Uses {name} (detected from your default browser)").replace("{name}", &name));
                } else {
                    er_sub.set_subtitle("Detecting your default browser\u{2026}");
                }
            } else {
                er_sub.set_subtitle("Used for web search results");
            }
        }
    });
    eg.add(&er);
    {
        let cfg = config.clone();
        custom_web.connect_apply(move |r| {
            let mut c = cfg.borrow_mut();
            c.custom_web_search_url = r.text().trim().to_string();
            c.save();
        });
    }
eg.add(&custom_web);


    // Updates: one row in Results — the master switch sits on it, the
    // status stays live on its subtitle, and activating it opens the
    // settings popup (no more inline expansion).
    crate::search::cmd::ensure_updates_checked();
    {
        let (row, sw) = popup_setting_row(
            &gettext("Updates"),
            &crate::search::cmd::update_status_text(config.borrow().enable_updates),
            config.borrow().enable_updates,
        );
        // The row's icon follows the state: update available → update
        // glyph, nothing to update (or the feature off) → checkmark.
        let status_icon = gtk::Image::from_icon_name(status_icon_name(
            crate::search::cmd::updates_pending() && config.borrow().enable_updates,
        ));
        row.add_prefix(&status_icon);
        {
            let cfg = config.clone();
            sw.connect_active_notify(move |r| {
                let active = r.is_active();
                save_and_refresh(&cfg, |c| c.enable_updates = active);
            });
        }
        {
            let win = window.clone();
            let cfg = config.clone();
            row.connect_activated(move |_| open_updates_dialog(&win, &cfg));
        }
        sg.add(&row);

        // Keep the status live: a background check landing updates the
        // subtitle and the icon without the window being rebuilt.
        {
            let row = row.clone();
            let icon = status_icon.clone();
            let win = window.clone();
            let cfg = config.clone();
            glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
                if win.is_visible() {
                    let enabled = cfg.borrow().enable_updates;
                    row.set_subtitle(&crate::search::cmd::update_status_text(enabled));
                    icon.set_icon_name(Some(status_icon_name(
                        enabled && crate::search::cmd::updates_pending(),
                    )));
                }
                glib::ControlFlow::Continue
            });
        }
    }

    // Keyboard: one row in General; activating it opens the popup with
    // every in-window shortcut plus a read-only overview of the trigger
    // words and their global shortcuts.
    let kg = adw::PreferencesGroup::builder().title(gettext("Keyboard")).build();
    general.add(&kg);
    {
        let row = adw::ActionRow::builder()
            .title(gettext("Keyboard Shortcuts"))
            .subtitle(gettext("Keyboard shortcuts used inside the search window"))
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let win = window.clone();
        let cfg = config.clone();
        row.connect_activated(move |_| open_keyboard_dialog(&win, &cfg));
        kg.add(&row);
    }

    // About — single row opening the native AboutDialog
    let ag = adw::PreferencesGroup::builder().title(gettext("About")).build();
    general.add(&ag);
    let about_row = adw::ActionRow::builder()
        .title(gettext("About Spotty"))
        .subtitle(format!("Version {}", env!("CARGO_PKG_VERSION")))
        .activatable(true)
        .build();
    about_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let win = window.clone();
        about_row.connect_activated(move |_| present_about_dialog(&win));
    }
    ag.add(&about_row);
}

/// Hours paired with the cadence labels (same index).
const INTERVAL_HOURS: [u32; 5] = [1, 6, 12, 24, 168];

/// Fraction-digit choices offered in the calculator popup (same index).
const CALC_PRECISIONS: [u32; 5] = [2, 4, 6, 8, 10];

/// A settings row with its own switch and a chevron: the switch toggles
/// the value, activating the row opens the settings popup.
fn popup_setting_row(
    title: &str,
    subtitle: &str,
    active: bool,
) -> (adw::ActionRow, gtk::Switch) {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(true)
        .build();
    let switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(active)
        .build();
    row.add_suffix(&switch);
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    (row, switch)
}

/// The package-manager choice behind "Search for new apps", as a popup
/// (it used to be a standalone section).
fn open_new_apps_dialog(parent: &adw::PreferencesWindow, config: &Rc<RefCell<Config>>) {
    let dlg = adw::PreferencesDialog::builder()
        .title(gettext("Search for new Apps"))
        .content_width(440)
        .build();
    let page = adw::PreferencesPage::builder().build();
    let g = adw::PreferencesGroup::builder()
        .title(gettext("Package Manager"))
        .build();

    let pm_row = adw::ComboRow::builder()
        .title(gettext("Package Manager"))
        .subtitle(gettext("Used for app search, install, and uninstall via the cmd trigger"))
        .build();
    // Snap options are only shown when snapd is installed on the system.
    let snap_ok = crate::search::cmd::snap_is_available() == Some(true);
    let visible: Vec<PackageManager> = PackageManager::all()
        .iter()
        .copied()
        .filter(|p| snap_ok || !p.use_snap())
        .collect();
    pm_row.set_model(Some(&gtk::StringList::new(
        &visible.iter().map(|p| p.display_name()).collect::<Vec<_>>(),
    )));
    if let Some(i) = visible
        .iter()
        .position(|&p| p == config.borrow().package_manager)
    {
        pm_row.set_selected(i as u32);
    }
    {
        let cfg = config.clone();
        pm_row.connect_selected_notify(move |r| {
            if let Some(&p) = visible.get(r.selected() as usize) {
                let mut c = cfg.borrow_mut();
                c.package_manager = p;
                c.save();
            }
        });
    }
    g.add(&pm_row);
    page.add(&g);
    dlg.add(&page);
    dlg.present(Some(parent));
}

/// The application-search settings as a popup: the shortcuts that act on
/// a selected app in the results (uninstall / kill), adjustable right
/// where the app results are toggled — the same fields the Open Spotty
/// dialog exposes, so both stay in sync through the config.
fn open_apps_dialog(parent: &adw::PreferencesWindow, config: &Rc<RefCell<Config>>) {
    let dlg = adw::PreferencesDialog::builder()
        .title(gettext("Applications"))
        .content_width(440)
        .build();
    let page = adw::PreferencesPage::builder().build();
    let g = adw::PreferencesGroup::builder()
        .title(gettext("Keyboard Shortcuts"))
        .build();

    for (title, field, default) in [
        (gettext("Uninstall app"), "uninstall", "<Control>u"),
        (gettext("Kill app"), "kill", "<Control>k"),
    ] {
        let current = shortcut_value(config, field, default);
        let cell: Rc<RefCell<String>> = Rc::new(RefCell::new(current.clone()));
        let cfg_k = config.clone();
        let field_k = field.to_string();
        let cell_k = cell.clone();
        let (row, _) = capture_shortcut_row(
            &dlg,
            &title,
            &display_shortcut(&current),
            cell,
            Rc::new(move || {
                let mut c = cfg_k.borrow_mut();
                set_shortcut(&mut c, &field_k, &cell_k.borrow());
                c.save();
            }),
            default,
        );
        g.add(&row);
    }
    page.add(&g);
    dlg.add(&page);
    dlg.present(Some(parent));
}

/// The calculator settings as a popup: how results are shown and what
/// Enter does with them.
fn open_calc_dialog(parent: &adw::PreferencesWindow, config: &Rc<RefCell<Config>>) {
    let dlg = adw::PreferencesDialog::builder()
        .title(gettext("Calculator & Conversions"))
        .content_width(440)
        .build();
    let page = adw::PreferencesPage::builder().build();

    // Results: how answers are shaped.
    {
        let g = adw::PreferencesGroup::builder().title(gettext("Results")).build();

        // Decimal places: 2/4/6/8/10 — the labels are digits, no translation.
        {
            let row = adw::ComboRow::builder()
                .title(gettext("Decimal places"))
                .build();
            row.set_model(Some(&gtk::StringList::new(&["2", "4", "6", "8", "10"])));
            row.set_selected(
                CALC_PRECISIONS
                    .iter()
                    .position(|p| *p == config.borrow().calc_precision)
                    .unwrap_or(2) as u32,
            );
            let cfg = config.clone();
            row.connect_selected_notify(move |r| {
                let p = CALC_PRECISIONS
                    .get(r.selected() as usize)
                    .copied()
                    .unwrap_or(6);
                let mut c = cfg.borrow_mut();
                c.calc_precision = p;
                c.save();
            });
            g.add(&row);
        }

        for (title, get, set) in [
            (
                gettext("Thousands separators"),
                (|c: &Config| c.calc_separators) as fn(&Config) -> bool,
                (|c, v| c.calc_separators = v) as fn(&mut Config, bool),
            ),
            (
                gettext("Show hex, octal and binary"),
                |c| c.calc_bases,
                |c, v| c.calc_bases = v,
            ),
            (
                gettext("Show the expression"),
                |c| c.calc_show_expr,
                |c, v| c.calc_show_expr = v,
            ),
            (
                gettext("Paste the result automatically"),
                |c| c.calc_paste,
                |c, v| c.calc_paste = v,
            ),
            (
                gettext("Scientific notation"),
                |c| c.calc_sci_notation,
                |c, v| c.calc_sci_notation = v,
            ),
        ] {
            add_calc_toggle(&g, config, &title, get, set);
        }
        page.add(&g);
    }

    // Conversions: what else the search box can answer.
    {
        let g = adw::PreferencesGroup::builder()
            .title(gettext("Conversions"))
            .description(gettext("Rates are fetched from a free public API when you convert"))
            .build();
        for (title, get, set) in [
            (
                gettext("Unit conversions"),
                (|c: &Config| c.calc_converter) as fn(&Config) -> bool,
                (|c, v| c.calc_converter = v) as fn(&mut Config, bool),
            ),
            (
                gettext("Show equivalents when no target unit is given"),
                |c| c.calc_equivalents,
                |c, v| c.calc_equivalents = v,
            ),
            (
                gettext("Number base conversions"),
                |c| c.calc_base_convert,
                |c, v| c.calc_base_convert = v,
            ),
            (
                gettext("Currency conversion"),
                |c| c.calc_currency,
                |c, v| c.calc_currency = v,
            ),
        ] {
            add_calc_toggle(&g, config, &title, get, set);
        }
        page.add(&g);
    }

    dlg.add(&page);
    dlg.present(Some(parent));
}

/// One on/off row wired straight to a `Config` bool, saved on change —
/// the shape every toggle in the Calculator & Conversions dialog uses.
fn add_calc_toggle(
    g: &adw::PreferencesGroup,
    config: &Rc<RefCell<Config>>,
    title: &str,
    get: fn(&Config) -> bool,
    set: fn(&mut Config, bool),
) {
    let row = adw::SwitchRow::builder()
        .title(title)
        .active(get(&config.borrow()))
        .build();
    let cfg = config.clone();
    row.connect_active_notify(move |r| {
        let active = r.is_active();
        let mut c = cfg.borrow_mut();
        set(&mut c, active);
        c.save();
    });
    g.add(&row);
}

/// Every in-window shortcut in one place — some had no UI at all, the
/// others were only reachable through the per-trigger dialogs (which keep
/// editing the same config fields). Below that, a read-only overview of
/// the trigger words and their global shortcuts: editing stays in the
/// Triggers page.
fn open_keyboard_dialog(parent: &adw::PreferencesWindow, config: &Rc<RefCell<Config>>) {
    let dlg = adw::PreferencesDialog::builder()
        .title(gettext("Keyboard Shortcuts"))
        .content_width(440)
        .build();
    let page = adw::PreferencesPage::builder().build();

    let g = adw::PreferencesGroup::builder()
        .title(gettext("Search Window"))
        .description(gettext("Keyboard shortcuts used inside the search window"))
        .build();
    for (title, field, default) in [
        (gettext("Copy"), "copy", "<Control>c"),
        (gettext("Cut"), "cut", "<Control>x"),
        (gettext("Paste"), "paste", "<Control>v"),
        (gettext("Select all"), "select_all", "<Control>a"),
        (gettext("Undo"), "undo", "<Control>z"),
        (gettext("Redo"), "redo", "<Control><Shift>z"),
        (gettext("Delete word"), "delete_word", "<Control>space"),
        (gettext("Operations"), "operations", "<Control>o"),
        (gettext("Keyboard shortcuts"), "hints", "<Control>h"),
        (gettext("Uninstall app"), "uninstall", "<Control>u"),
        (gettext("Kill app"), "kill", "<Control>k"),
        (gettext("Delete file / folder"), "delete_file", "<Control>d"),
        (gettext("Open location in file manager"), "open_location", "<Control>Return"),
        (gettext("Open folder in terminal"), "terminal", "<Control><Shift>Return"),
    ] {
        let current = shortcut_value(config, field, default);
        let cell: Rc<RefCell<String>> = Rc::new(RefCell::new(current.clone()));
        let cfg_k = config.clone();
        let field_k = field.to_string();
        let cell_k = cell.clone();
        let (row, _) = capture_shortcut_row(
            &dlg,
            &title,
            &display_shortcut(&current),
            cell,
            Rc::new(move || {
                let mut c = cfg_k.borrow_mut();
                set_shortcut(&mut c, &field_k, &cell_k.borrow());
                c.save();
            }),
            default,
        );
        g.add(&row);
    }
    page.add(&g);

    // Trigger words: read-only here, exactly as the Triggers page shows
    // them.
    let tg = adw::PreferencesGroup::builder().title(gettext("Triggers")).build();
    {
        let row = adw::ActionRow::builder()
            .title(gettext("Open Spotty"))
            .subtitle(&trigger_subtitle(true, "spotty", &config.borrow().shortcut))
            .build();
        tg.add(&row);
    }
    let keywords: Vec<CommandKeyword> = config.borrow().command_keywords.clone();
    for kw in &keywords {
        if kw.id == "cmd" {
            continue;
        }
        let row = adw::ActionRow::builder()
            .title(capitalized(&kw.id))
            .subtitle(&trigger_subtitle(kw.enabled, &kw.word, &kw.shortcut))
            .build();
        tg.add(&row);
    }
    for a in crate::triggers::all() {
        let row = adw::ActionRow::builder()
            .title(&a.name)
            .subtitle(&trigger_subtitle(a.enabled, &a.word, &a.shortcut))
            .build();
        tg.add(&row);
    }
    page.add(&tg);

    dlg.add(&page);
    dlg.present(Some(parent));
}

/// The updates settings as a popup: check-now with its live status,
/// notification, cadence, notice controls and the restart row. Built fresh
/// on every open so it always shows the current state.
fn open_updates_dialog(parent: &adw::PreferencesWindow, config: &Rc<RefCell<Config>>) {
    let enabled = config.borrow().enable_updates;
    let dlg = adw::PreferencesDialog::builder()
        .title(gettext("Updates"))
        .content_width(440)
        .build();
    let page = adw::PreferencesPage::builder().build();

    // Check now: the status as its subtitle; the ↻ (or the row) re-checks.
    let g = adw::PreferencesGroup::builder().build();
    let check_row = adw::ActionRow::builder()
        .title(gettext("Check now"))
        .subtitle(&crate::search::cmd::update_status_text(enabled))
        .activatable(true)
        .build();
    let check_btn = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Check now"))
        .build();
    check_btn.set_sensitive(enabled && !crate::search::cmd::updates_checking());
    check_row.add_suffix(&check_btn);
    let recheck: Rc<dyn Fn()> = {
        let row = check_row.clone();
        let btn = check_btn.clone();
        let cfg = config.clone();
        Rc::new(move || {
            crate::search::cmd::check_updates_now();
            let on = cfg.borrow().enable_updates;
            row.set_subtitle(&crate::search::cmd::update_status_text(on));
            btn.set_sensitive(false);
        })
    };
    {
        let f = recheck.clone();
        check_row.connect_activated(move |_| f());
    }
    {
        let f = recheck.clone();
        check_btn.connect_clicked(move |_| f());
    }
    g.add(&check_row);

    {
        let row = adw::SwitchRow::builder()
            .title(gettext("Update notification"))
            .subtitle(gettext("Show a badge and a desktop notification when updates exist"))
            .active(config.borrow().update_notification)
            .build();
        let cfg = config.clone();
        row.connect_active_notify(move |r| {
            let active = r.is_active();
            save_and_refresh(&cfg, |c| c.update_notification = active);
        });
        g.add(&row);
    }
    {
        let row = adw::ComboRow::builder()
            .title(gettext("Check every"))
            .build();
        let intervals_model = gtk::StringList::new(&[]);
        // Literal gettext calls so the labels land in the translation
        // catalogue (a variable-driven gettext() can't be extracted).
        for label in [
            gettext("Hourly"),
            gettext("Every 6 hours"),
            gettext("Every 12 hours"),
            gettext("Daily"),
            gettext("Weekly"),
        ] {
            intervals_model.append(&label);
        }
        row.set_model(Some(&intervals_model));
        let cur = config.borrow().update_check_interval_hours;
        row.set_selected(
            INTERVAL_HOURS
                .iter()
                .position(|h| *h == cur)
                .unwrap_or(3) as u32,
        );
        let cfg = config.clone();
        row.connect_selected_notify(move |r| {
            if let Some(hours) = INTERVAL_HOURS.get(r.selected() as usize) {
                let hours = *hours;
                save_and_refresh(&cfg, |c| c.update_check_interval_hours = hours);
            }
        });
        g.add(&row);
    }
    // The settings only mean something while the master switch is on.
    g.set_sensitive(enabled);
    page.add(&g);

    if crate::search::cmd::updates_pending() {
        let g2 = adw::PreferencesGroup::builder().build();
        let row = adw::ActionRow::builder()
            .title(gettext("Remind tomorrow"))
            .subtitle(gettext("Hide the update notice for 24 hours"))
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("document-open-recent-symbolic"));
        row.connect_activated(|_| crate::search::cmd::snooze_update_notice());
        g2.add(&row);

        let row = adw::ActionRow::builder()
            .title(gettext("Dismiss update notice"))
            .subtitle(gettext("Hidden until a new update appears"))
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("window-close-symbolic"));
        row.connect_activated(|_| crate::search::cmd::dismiss_current_update_notice());
        g2.add(&row);
        g2.set_sensitive(enabled);
        page.add(&g2);
    }

    if crate::search::cmd::reboot_pending() {
        // Updates still pending → "Update & Restart" (upgrade, then reboot);
        // already applied → a plain restart.
        let with_updates = crate::search::cmd::updates_pending();
        let heading = if with_updates {
            gettext("Update & Restart")
        } else {
            gettext("Restart required to finish the update")
        };
        let btn_label = if with_updates {
            gettext("Update & Restart")
        } else {
            gettext("Restart")
        };
        let g3 = adw::PreferencesGroup::builder().build();
        let row = adw::ActionRow::builder().title(heading).activatable(true).build();
        row.add_prefix(&gtk::Image::from_icon_name("system-reboot-symbolic"));
        let btn = gtk::Button::builder()
            .label(btn_label)
            .css_classes(["suggested-action"])
            .valign(gtk::Align::Center)
            .build();
        {
            let win = parent.clone();
            btn.connect_clicked(move |_| {
                if crate::search::cmd::updates_pending() {
                    crate::operations::start(
                        gettext("Update & Restart"),
                        "System Update".into(),
                        "system-reboot-symbolic".into(),
                        crate::search::cmd::update_then_restart_args(),
                    );
                    win.close();
                    return;
                }
                let dialog = adw::MessageDialog::builder()
                    .transient_for(&win)
                    .heading(gettext("Are you sure?"))
                    .build();
                dialog.add_response("cancel", &gettext("Cancel"));
                dialog.add_response("confirm", &gettext("Confirm"));
                dialog.set_default_response(Some("confirm"));
                dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
                let cmd = crate::search::system::reboot_command();
                dialog.connect_response(None, move |_, resp| {
                    if resp == "confirm" {
                        let _ = crate::app::spawn_host_shell_command(&cmd);
                    }
                });
                dialog.present();
            });
        }
        row.add_suffix(&btn);
        g3.add(&row);
        page.add(&g3);
    }

    dlg.add(&page);

    // The status + ↻ keep refreshing while the popup is open, so a check
    // finishing behind it updates the row it was started from.
    {
        let alive = Rc::new(std::cell::Cell::new(true));
        let a2 = alive.clone();
        let row = check_row.clone();
        let btn = check_btn.clone();
        let cfg = config.clone();
        glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            if !a2.get() {
                return glib::ControlFlow::Break;
            }
            let on = cfg.borrow().enable_updates;
            row.set_subtitle(&crate::search::cmd::update_status_text(on));
            btn.set_sensitive(on && !crate::search::cmd::updates_checking());
            glib::ControlFlow::Continue
        });
        let a3 = alive.clone();
        dlg.connect_closed(move |_| a3.set(false));
    }
    dlg.present(Some(parent));
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

/// Edit the global "Open Spotty" shortcut in its own dialog, mirroring the
/// trigger edit windows. Reset only restores the built-in default — the
/// launcher toggle always stays bound, only rebindable.
fn open_spotty_edit_dialog(
    parent: &adw::PreferencesWindow,
    config: &Rc<RefCell<Config>>,
    row: &adw::ActionRow,
) {
    let dialog = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(gettext("Edit \"Open Spotty\" shortcut"))
        .default_width(640)
        .default_height(600)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("Edit \"Open Spotty\" shortcut", ""))
        .build();

    let cancel_btn = gtk::Button::with_label("Cancel");
    header.pack_start(&cancel_btn);
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

    let group = adw::PreferencesGroup::builder()
        .title(gettext("Keyboard Shortcut"))
        .description(gettext("Shortcut to focus the Spotty launcher"))
        .build();
    let pending: Rc<RefCell<String>> = Rc::new(RefCell::new(config.borrow().shortcut.clone()));
    let open_default = Config::default().shortcut;
    let (sc_row, _sc_reset_btn) = {
        let cfg_s = config.clone();
        let row_s = row.clone();
        let pending_s = pending.clone();
        capture_shortcut_row(
            &dialog,
            "Open Spotty",
            &display_shortcut(&pending.borrow()),
            pending.clone(),
            Rc::new(move || {
                let val = pending_s.borrow().clone();
                {
                    let mut c = cfg_s.borrow_mut();
                    c.shortcut = val.clone();
                    c.save();
                }
                std::thread::spawn(crate::keybindings::register_all);
                row_s.set_subtitle(&trigger_subtitle(true, "spotty", &val));
            }),
            &open_default,
        )
    };
    group.add(&sc_row);
    content.append(&group);

    // In-window shortcuts (not global GNOME keybindings): each row captures
    // a combo into its own pending cell; Reset restores the row's default.
    let win_group = adw::PreferencesGroup::builder()
        .title(gettext("Search Window"))
        .description(gettext("Keyboard shortcuts used inside the search window"))
        .build();
    let win_fields: [(&str, &str, &str); 4] = [
        ("Uninstall app", "uninstall", "<Control>u"),
        ("Kill app", "kill", "<Control>k"),
        ("Delete file / folder", "delete_file", "<Control>d"),
        ("Open location in file manager", "open_location", "<Control>Return"),
    ];
    for &(title, field, default) in &win_fields {
        let current = shortcut_value(config, field, default);
        let cell: Rc<RefCell<String>> = Rc::new(RefCell::new(current.clone()));
        let (win_row, _) = {
            let cfg_w = config.clone();
            let field_w = field.to_string();
            let cell_w = cell.clone();
            capture_shortcut_row(
                &dialog,
                title,
                &display_shortcut(&current),
                cell.clone(),
                Rc::new(move || {
                    let mut c = cfg_w.borrow_mut();
                    set_shortcut(&mut c, &field_w, &cell_w.borrow());
                    c.save();
                    drop(c);
                    std::thread::spawn(crate::keybindings::register_all);
                }),
                default,
            )
        };
        win_group.add(&win_row);
    }
    content.append(&win_group);

    let clamp = adw::Clamp::builder().maximum_size(600).build();
    clamp.set_child(Some(&content));
    scroll.set_child(Some(&clamp));
    toolbar.set_content(Some(&scroll));
    dialog.set_content(Some(&toolbar));

    {
        let dialog_c = dialog.clone();
        cancel_btn.connect_clicked(move |_| dialog_c.close());
    }

    dialog.present();
}

/// Build the Trigger settings page. Returns a refresh closure that rebuilds
/// the trigger rows (used when the cached settings window is re-presented, so
/// triggers installed meanwhile show up).
fn build_keywords_page(
    window: &adw::PreferencesWindow,
    config: &Rc<RefCell<Config>>,
) -> Rc<dyn Fn()> {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Triggers"))
        .icon_name("preferences-desktop-keyboard-symbolic")
        .build();
    window.add(&page);

    let g = adw::PreferencesGroup::builder()
        .description(gettext("Edit trigger words, import a manifest with +, or delete installed triggers."))
        .build();
    page.add(&g);

    // Open Spotty shortcut — first row of the trigger list; clicking opens
    // the edit dialog, matching the other trigger rows' UX.
    let open_row = adw::ActionRow::builder()
        .title(gettext("Open Spotty"))
        .subtitle(&trigger_subtitle(true, "spotty", &config.borrow().shortcut))
        .activatable(true)
        .build();
    open_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let win = window.clone();
        let cfg = config.clone();
        let row = open_row.clone();
        open_row.connect_activated(move |_| {
            open_spotty_edit_dialog(&win, &cfg, &row);
        });
    }
    g.add(&open_row);

    let rows: Rc<RefCell<Vec<TriggerRow>>> = Rc::new(RefCell::new(Vec::new()));

    // Store: browse and install triggers straight from the repository (its
    // URL is config-only and never shown). Local installs — create your own
    // or import a downloaded manifest — live behind the + inside the store.
    let store_row = adw::ActionRow::builder()
        .title(gettext("Store"))
        .subtitle(gettext("Browse and install triggers"))
        .activatable(true)
        .build();
    store_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let win = window.clone();
        let g2 = g.clone();
        let cfg = config.clone();
        let rows2 = rows.clone();
        store_row.connect_activated(move |_| {
            store_dialog(&win, &g2, &cfg, &rows2);
        });
    }
    g.add(&store_row);

    // Reset as a compact icon button in the group header (adding triggers
    // happens in the store now).
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .build();
    let reset_btn = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Reset trigger words and shortcuts to defaults"))
        .build();
    {
        let cfg = config.clone();
        let win = window.clone();
        let g2 = g.clone();
        let rows2 = rows.clone();
        let open_row2 = open_row.clone();
        reset_btn.connect_clicked(move |_| {
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
                c.shortcut = d.shortcut.clone();
                c.save();
                std::thread::spawn(crate::keybindings::register_all);
            }
            rebuild_trigger_rows(&g2, &win, &cfg, &rows2);
            open_row2.set_subtitle(&trigger_subtitle(true, "spotty", &d.shortcut));
            win.add_toast(adw::Toast::new(
                "Trigger words and shortcuts reset to defaults",
            ));
        });
    }
    header.append(&reset_btn);
    g.set_header_suffix(Some(&header));

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
    Rc::new(move || rebuild_trigger_rows(&g, &window, &config, &rows))
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
        g.remove(&tr.row);
    }
    rows.borrow_mut().clear();

    let keywords: Vec<CommandKeyword> = config.borrow().command_keywords.clone();
    for kw in &keywords {
        if kw.id == "cmd" {
            continue;
        }
        let row = adw::ActionRow::builder()
            .title(capitalized(&kw.id))
            .subtitle(&trigger_subtitle(kw.enabled, &kw.word, &kw.shortcut))
            .activatable(true)
            .build();
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
        g.add(&row);
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
            .build();
        let del_btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Uninstall trigger"))
            .build();
        let switch = gtk::Switch::builder()
            .active(a.enabled)
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&del_btn);
        row.add_suffix(&switch);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        {
            let id = a.id.clone();
            let win = window.clone();
            let g2 = g.clone();
            let cfg = config.clone();
            let rows2 = rows.clone();
            del_btn.connect_clicked(move |_| {
                confirm_uninstall(&win, &g2, &cfg, &rows2, &id);
            });
        }
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
        g.add(&row);
        rows.borrow_mut().push(TriggerRow {
            id: a.id.clone(),
            is_installed: true,
            row: row.clone(),
        });
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
fn install_preview(path: &std::path::Path) -> InstallPreview {
    let Ok(text) = std::fs::read_to_string(path) else {
        return InstallPreview::Direct;
    };
    let Ok(m) = serde_json::from_str::<crate::triggers::TriggerManifest>(&text) else {
        return InstallPreview::Direct;
    };
    match m.action {
        crate::triggers::TriggerAction::Shell { command } => InstallPreview::Confirm {
            name: m.name,
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
    match install_preview(path) {
        InstallPreview::Direct => {
            finish_trigger_install(window, g, config, rows, path, cleanup, on_done);
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
    cleanup: bool,
    on_done: Option<Rc<dyn Fn()>>,
) {
    match crate::triggers::install_from_file(path) {
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
    let name_row = adw::EntryRow::builder().title(gettext("Name")).build();
    let word_row = adw::EntryRow::builder().title(gettext("Keyword")).build();
    let desc_row = adw::EntryRow::builder()
        .title(gettext("Description (optional)"))
        .build();
    let icon_row = adw::EntryRow::builder()
        .title(gettext("Icon name (optional)"))
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
    let cmd_row = adw::EntryRow::builder().title(gettext("Command line")).build();
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
                    finish_trigger_install(&win, &g2, &cfg, &rows2, &tmp, true, None);
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
    list_box: &gtk::ListBox,
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
    let shown: Vec<_> = items
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
    for t in &shown {
        list_box.append(&server_trigger_row(t, base, window, g, config, rows, &refresh));
    }
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
    let row = gtk::ListBoxRow::new();
    let subtitle = if t.description.is_empty() {
        t.word.clone()
    } else if t.word.is_empty() {
        t.description.clone()
    } else {
        gettext("{word} — {description}").replace("{word}", &t.word).replace("{description}", &t.description)
    };
    let action = adw::ActionRow::builder()
        .title(&t.name)
        .subtitle(&subtitle)
        .build();
    if !t.icon.is_empty() && has_icon(&t.icon) {
        let img = gtk::Image::from_icon_name(&t.icon);
        img.set_pixel_size(24);
        action.add_prefix(&img);
    }
    if crate::triggers::by_id(&t.id).is_some() {
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
    row.set_child(Some(&action));
    row
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
        .title(gettext("Trigger Store"))
        .default_width(640)
        .default_height(640)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("Trigger Store", ""))
        .build();
    let close_btn = gtk::Button::with_label("Close");
    header.pack_start(&close_btn);
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
        .placeholder_text(gettext("Search triggers"))
        .build();
    content.append(&search);

    let list_box = gtk::ListBox::builder()
        .css_classes(["spotty-flat-list"])
        .selection_mode(gtk::SelectionMode::None)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .propagate_natural_height(true)
        .child(&list_box)
        .build();

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

    let stack = gtk::Stack::new();
    stack.add_named(&loading_box, Some("loading"));
    stack.add_named(&scroll, Some("list"));
    stack.add_named(&empty_page, Some("empty"));
    stack.add_named(&error_page, Some("error"));
    stack.set_visible_child_name("loading");
    content.append(&stack);

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

    {
        let render = render.clone();
        search.connect_changed(move |_| render());
    }
    {
        let d = dialog.clone();
        close_btn.connect_clicked(move |_| d.close());
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
                        *items.borrow_mut() = list;
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

    let dialog = adw::Window::builder()
        .transient_for(window)
        .modal(false)
        .title(gettext("Edit \"{name}\" trigger").replace("{name}", &name))
        .default_width(640)
        .default_height(600)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(&gettext("Edit \"{name}\" trigger").replace("{name}", &name), ""))
        .build();

    let cancel_btn = gtk::Button::with_label("Cancel");
    header.pack_start(&cancel_btn);

    let uninstall_btn: Option<gtk::Button> = if is_installed {
        let b = gtk::Button::with_label("Uninstall");
        b.add_css_class("destructive-action");
        header.pack_start(&b);
        Some(b)
    } else {
        None
    };
    toolbar.add_top_bar(&header);

    let clamp = adw::Clamp::builder().maximum_size(600).build();
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

    // ── Trigger group ─────────────────────────────────────────────────
    let trigger_group = adw::PreferencesGroup::new();
    if is_installed && !description.is_empty() {
        trigger_group.set_description(Some(&description));
    }
    let enabled_row = adw::SwitchRow::builder()
        .title(gettext("Enabled"))
        .subtitle(gettext("Pause this trigger without uninstalling it"))
        .active(enabled)
        .build();
    trigger_group.add(&enabled_row);
    let entry = adw::EntryRow::builder()
        .title(gettext("Trigger word"))
        .text(&word)
        .build();
    trigger_group.add(&entry);
    content.append(&trigger_group);

    // ── Pending state (auto-saved on every change) ─────────────────────
    let pending_shortcut: Rc<RefCell<String>> = Rc::new(RefCell::new(shortcut.clone()));
    let pending_enabled: Rc<RefCell<bool>> = Rc::new(RefCell::new(enabled));
    let pending_limit: Rc<RefCell<f64>> = Rc::new(RefCell::new(config.borrow().clipboard_history_limit as f64));
    let pending_pin_shortcut: Rc<RefCell<String>> = Rc::new(RefCell::new(config.borrow().clipboard_pin_shortcut.clone()));
    let pending_show_hints: Rc<RefCell<bool>> = Rc::new(RefCell::new(config.borrow().show_shortcut_hints));
    let pending_terminal: Rc<RefCell<String>> = Rc::new(RefCell::new(config.borrow().terminal_shortcut.clone()));

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

    // ── Shortcut group ──────────────────────────────────────────────
    let shortcut_group = adw::PreferencesGroup::builder()
        .title(gettext("Keyboard Shortcut"))
        .build();
    let shortcut_default = Config::default()
        .command_keywords
        .iter()
        .find(|k| k.id == id)
        .map(|k| k.shortcut.clone())
        .unwrap_or_else(|| shortcut.clone());
    let (short_row, _) = capture_shortcut_row(
        &dialog,
        "Keyboard shortcut",
        &display_shortcut(&pending_shortcut.borrow()),
        pending_shortcut.clone(),
        apply.clone(),
        &shortcut_default,
    );
    shortcut_group.add(&short_row);
    content.append(&shortcut_group);

    if is_clipboard {
        let clip_group = adw::PreferencesGroup::builder().title(gettext("Clipboard")).build();
        content.append(&clip_group);

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
            &display_shortcut(&config.borrow().clipboard_pin_shortcut),
            pending_pin_shortcut.clone(),
            apply.clone(),
            &Config::default().clipboard_pin_shortcut,
        );
        clip_group.add(&_pin_row);
    }

    if is_find {
        let find_group = adw::PreferencesGroup::builder().title(gettext("Find")).build();
        content.append(&find_group);

        let root_row = adw::SwitchRow::builder()
            .title(gettext("Root Path Browsing"))
            .subtitle(gettext("Allow typing / or ~/ to browse the filesystem directly"))
            .active(config.borrow().enable_root_browsing)
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

        // Terminal shortcut
        let (terminal_row, _) = capture_shortcut_row(
            &dialog,
            "Open folder in terminal",
            &display_shortcut(&config.borrow().terminal_shortcut),
            pending_terminal.clone(),
            apply.clone(),
            &Config::default().terminal_shortcut,
        );
        find_group.add(&terminal_row);
    }

    clamp.set_child(Some(&content));
    scroll.set_child(Some(&clamp));
    toolbar.set_content(Some(&scroll));
    dialog.set_content(Some(&toolbar));

    // Trigger word: reset button appears only once the word differs from
    // the default; auto-save, debounced so intermediate keystrokes ("f"
    // while typing "find") never persist. Invalid words are reverted.
    {
        let original_word = default_word;
        let reset_btn = gtk::Button::builder()
            .label(gettext("Reset"))
            .css_classes(["flat", "spotty-reset"])
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
        let win_c = window.clone();
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

    cancel_btn.connect_clicked({
        let dialog = dialog.clone();
        move |_| dialog.close()
    });

    let win_c = window.clone();
    let g_c = g.clone();
    let cfg_c = config.clone();
    let rows_c = rows.clone();
    if let Some(uninstall_btn) = uninstall_btn {
        let dialog_c = dialog.clone();
        let id_c = id.clone();
        uninstall_btn.connect_clicked(move |_| {
            confirm_uninstall(&win_c, &g_c, &cfg_c, &rows_c, &id_c);
            dialog_c.close();
        });
    }

    dialog.present();
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
fn normalize_accel(s: &str) -> String {
    if gtk::accelerator_parse(s).is_some() {
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
    current: &str,
    pending: Rc<RefCell<String>>,
    on_change: Rc<dyn Fn()>,
    default: &str,
) -> (adw::ActionRow, gtk::Button) {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(if current.is_empty() {
            "No shortcut"
        } else {
            current
        })
        .build();

    // Reset → back to the built-in default. Only visible while the pending
    // value differs from the default (i.e. the shortcut was customized).
    let reset_btn = gtk::Button::builder()
        .label(gettext("Reset"))
        .css_classes(["flat", "spotty-reset"])
        .valign(gtk::Align::Center)
        .build();
    reset_btn.set_visible(pending.borrow().as_str() != default);
    row.add_suffix(&reset_btn);
    {
        let pending_c = pending.clone();
        let row_c = row.clone();
        let reset_c = reset_btn.clone();
        let on_change_c = on_change.clone();
        let default_c = default.to_string();
        reset_btn.connect_clicked(move |_| {
            *pending_c.borrow_mut() = default_c.clone();
            row_c.set_subtitle(&display_shortcut(&default_c));
            reset_c.set_visible(false);
            on_change_c();
        });
    }

    let set_btn = gtk::Button::builder()
        .label(gettext("Set shortcut…"))
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&set_btn);

    // Set → capture the next key combo
    let parent_w = parent.upcast_ref::<gtk::Widget>().clone();
    {
        let pending_s = pending.clone();
        let row_s = row.clone();
        let parent_s = parent_w.clone();
        let on_change_s = on_change.clone();
        let reset_s = reset_btn.clone();
        let default_s = default.to_string();
        set_btn.connect_clicked(move |btn| {
            btn.set_label("Press keys…");
            let kc = gtk::EventControllerKey::new();
            let pending_k = pending_s.clone();
            let row_k = row_s.clone();
            let btn_k = btn.clone();
            let parent_k = parent_s.clone();
            let on_change_k = on_change_s.clone();
            let reset_k = reset_s.clone();
            let default_k = default_s.clone();
            kc.connect_key_pressed(move |ctrl, key, _, state| {
                use gtk::gdk::Key;
                if matches!(
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
                ) {
                    return glib::Propagation::Stop;
                }
                if key == Key::Escape {
                    btn_k.set_label("Set shortcut…");
                    parent_k.remove_controller(ctrl);
                    return glib::Propagation::Stop;
                }
                let combo = key_combo_string(key, state);
                if !combo.is_empty() {
                    *pending_k.borrow_mut() = combo.clone();
                    row_k.set_subtitle(&combo);
                    reset_k.set_visible(combo != default_k);
                    btn_k.set_label("Set shortcut…");
                    parent_k.remove_controller(ctrl);
                    on_change_k();
                }
                glib::Propagation::Stop
            });
            parent_w.add_controller(kc);
        });
    }
    (row, reset_btn)
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
    fn trigger_id_slug_from_name() {
        assert_eq!(trigger_id_from_name("My Cool Trigger"), "my-cool-trigger");
        assert_eq!(trigger_id_from_name("  --weird__  "), "weird");
        assert_eq!(trigger_id_from_name("Updates 2"), "updates-2");
        assert_eq!(trigger_id_from_name("!!!"), "trigger");
    }

    #[test]
    fn install_gate_confirms_shell_and_web_but_not_files() {
        let dir = std::env::temp_dir();
        let tag = std::process::id();
        let write = |name: &str, body: &str| -> std::path::PathBuf {
            let p = dir.join(format!("spotty_gate_test_{tag}_{name}.json"));
            std::fs::write(&p, body).expect("write fixture");
            p
        };
        let shell = write(
            "shell",
            r#"{"id":"x","name":"X","word":"x","description":"d","action":{"type":"shell","command":"rm -rf ~"}}"#,
        );
        let web = write(
            "web",
            r#"{"id":"y","name":"Y","word":"y","description":"d","action":{"type":"web","url":"https://e/{query}"}}"#,
        );
        let files = write(
            "files",
            r#"{"id":"z","name":"Z","word":"z","description":"d","action":{"type":"files","extensions":["pdf"]}}"#,
        );

        // Shell = destructive confirmation showing the exact command.
        match install_preview(&shell) {
            InstallPreview::Confirm { body, destructive, .. } => {
                assert!(destructive);
                assert!(body.contains("rm -rf ~"));
            }
            _ => panic!("shell triggers must confirm"),
        }
        // Web triggers can only open a browser link — install directly,
        // no dialog (shell is the only gate, checked above).
        assert!(matches!(install_preview(&web), InstallPreview::Direct));
        // File filters are harmless: install directly.
        assert!(matches!(install_preview(&files), InstallPreview::Direct));
        // A non-manifest file must not bypass anything by accident —
        // it goes Direct and fails later in validation instead.
        let junk = write("junk", "not json at all");
        assert!(matches!(install_preview(&junk), InstallPreview::Direct));

        for p in [&shell, &web, &files, &junk] {
            let _ = std::fs::remove_file(p);
        }
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
