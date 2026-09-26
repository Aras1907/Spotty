use crate::config::{CommandKeyword, Config, PackageManager, SearchEngine};
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
            .title("Spotty Settings")
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
        // first opened (marketplace installs) appear without re-opening.
        (self.refresh_triggers)();
        self.window.present();
        // On Wayland, present() on an already-visible window doesn't reliably
        // grab keyboard focus — grab it on the next main-loop iteration.
        let w = self.window.clone();
        glib::idle_add_local_once(move || {
            w.grab_focus();
        });
    }
    pub fn refresh_triggers(&self) {
        (self.refresh_triggers)();
    }
}

/// Refresh the settings window's trigger list (built-in + installed triggers).
/// Called from the triggers window after install/uninstall so the two views
/// stay in sync without requiring settings to be closed and reopened.
pub fn refresh_triggers_live() {
    crate::app::with_state(|st| {
        if let Some(win) = st.settings_win.borrow().as_ref() {
            win.refresh_triggers();
        }
    });
}

// ──────────────────────────────────────────────────────────────────────
// General page
// ──────────────────────────────────────────────────────────────────────
fn build_general_page(window: &adw::PreferencesWindow, config: &Rc<RefCell<Config>>) {
    let general = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();
    window.add(&general);

    // Result source toggles (merged from the former "Sources" page)
    let sg = adw::PreferencesGroup::builder()
        .description("Choose what types of results appear")
        .build();
    general.add(&sg);
    let toggles: Vec<(&str, &str, fn(&Config) -> bool, fn(&mut Config, bool))> = vec![
        (
            "Applications",
            "System, Flatpak, Snap apps",
            (|c| c.enable_apps),
            (|c, v| c.enable_apps = v),
        ),
        (
            "Search for new Apps",
            "Also suggest installable Flatpak / distro apps as you type",
            (|c| c.enable_new_apps),
            (|c, v| c.enable_new_apps = v),
        ),
        (
            "Calculator",
            "Inline arithmetic",
            (|c| c.enable_calculator),
            (|c, v| c.enable_calculator = v),
        ),
        (
            "Web Search",
            "Always show web search row",
            (|c| c.enable_web),
            (|c, v| c.enable_web = v),
        ),
    ];
    for (title, subtitle, get, set) in toggles {
        let row = adw::SwitchRow::builder()
            .title(title)
            .subtitle(subtitle)
            .active(get(&config.borrow()))
            .build();
        let cfg = config.clone();
        row.connect_active_notify(move |r| {
            let mut c = cfg.borrow_mut();
            set(&mut c, r.is_active());
            c.save();
        });
        sg.add(&row);
    }

    // Web search engine + custom URL (URL only relevant for the Custom engine)
    let custom_web = adw::EntryRow::builder()
        .title("Custom Search URL")
        .show_apply_button(true)
        .build();
    custom_web.set_text(&config.borrow().custom_web_search_url);
    custom_web.set_tooltip_text(Some(
        "Use {query} as the placeholder, for example https://example.com/search?q={query}",
    ));

    let eg = adw::PreferencesGroup::builder().title("Web Search").build();
    general.add(&eg);
    let er = adw::ComboRow::builder()
        .title("Search Engine")
        .subtitle("Used for web search results")
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
            er.set_subtitle(&format!("Uses {name} (detected from your default browser)"));
        } else {
            er.set_subtitle("Detecting your default browser\u{2026}");
        }
    }
    let cfg = config.clone();
    let custom_web_sens = custom_web.clone();
    let er_sub = er.clone();
    custom_web_sens.set_sensitive(SearchEngine::all()
        .get(er.selected() as usize)
        .map_or(false, |&e| e == SearchEngine::Custom));
    er.connect_selected_notify(move |r| {
        if let Some(&e) = SearchEngine::all().get(r.selected() as usize) {
            let mut c = cfg.borrow_mut();
            c.search_engine = e;
            c.save();
            custom_web_sens.set_sensitive(e == SearchEngine::Custom);
            // Update subtitle for Browser Default
            if e == SearchEngine::BrowserDefault {
                if let Some(name) = crate::search::browser_engine::engine_name() {
                    er_sub.set_subtitle(&format!("Uses {name} (detected from your default browser)"));
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

    // Package manager preference for cmd trigger
    let pmg = adw::PreferencesGroup::builder()
        .title("Package Manager")
        .build();
    general.add(&pmg);
    let pm_row = adw::ComboRow::builder()
        .title("Package Manager")
        .subtitle("Used for app search, install, and uninstall via the cmd trigger")
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
    pmg.add(&pm_row);

    // About — single row opening the native AboutDialog
    let ag = adw::PreferencesGroup::builder().title("About").build();
    general.add(&ag);
    let about_row = adw::ActionRow::builder()
        .title("About Spotty")
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
        format!("Word: {word} · No shortcut")
    } else {
        format!("Word: {word} · {shortcut}")
    };
    if enabled {
        base
    } else {
        format!("Disabled · {base}")
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
        .title("Edit \"Open Spotty\" shortcut")
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
        .title("Keyboard Shortcut")
        .description("Shortcut to focus the Spotty launcher")
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
        .title("Search Window")
        .description("Keyboard shortcuts used inside the search window")
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
        .title("Trigger")
        .icon_name("preferences-desktop-keyboard-symbolic")
        .build();
    window.add(&page);

    let g = adw::PreferencesGroup::builder()
        .description("Edit trigger words, add your own, or delete installed triggers.")
        .build();
    page.add(&g);

    // Open Spotty shortcut — first row of the trigger list; clicking opens
    // the edit dialog, matching the other trigger rows' UX.
    let open_row = adw::ActionRow::builder()
        .title("Open Spotty")
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

    // Add Trigger Word + Reset as compact icon buttons in the group header.
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .build();
    let add_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .tooltip_text("Add Trigger Word (marketplace)")
        .build();
    {
        let win = window.clone();
        add_btn.connect_clicked(move |_| {
            if let Some(app) = win
                .application()
                .and_then(|a| a.downcast::<adw::Application>().ok())
            {
                crate::app::open_triggers_marketplace(&app);
            }
        });
    }
    let reset_btn = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .tooltip_text("Reset trigger words and shortcuts to defaults")
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
            crate::ui::triggers_window::refresh_live();
            open_row2.set_subtitle(&trigger_subtitle(true, "spotty", &d.shortcut));
            win.add_toast(adw::Toast::new(
                "Trigger words and shortcuts reset to defaults",
            ));
        });
    }
    header.append(&add_btn);
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
                    // shortcuts are now only managed inside Spotty
                }
                rebuild_trigger_rows(&g2, &win, &cfg, &rows2);
                crate::ui::triggers_window::refresh_live();
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
            .tooltip_text("Uninstall trigger")
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
                rebuild_trigger_rows(&g2, &win, &cfg, &rows2);
                crate::ui::triggers_window::refresh_live();
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
        .heading(format!("Uninstall \"{id}\"?"))
        .body("Its trigger word and shortcut will be removed.")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("uninstall", "Uninstall");
    dialog.set_response_appearance("uninstall", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.connect_response(Some("uninstall"), move |_, resp| {
        if resp == "uninstall" {
            if let Err(e) = crate::triggers::uninstall(&id) {
                log::warn!("triggers: uninstall failed: {e}");
            }
            rebuild_trigger_rows(&g2, &win, &cfg, &rows2);
            crate::ui::triggers_window::refresh_live();
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
        .title(format!("Edit \"{name}\" trigger"))
        .default_width(640)
        .default_height(600)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(&format!("Edit \"{name}\" trigger"), ""))
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
    let entry = adw::EntryRow::builder()
        .title("Trigger word")
        .text(&word)
        .build();
    trigger_group.add(&entry);
    content.append(&trigger_group);

    // ── Pending state (auto-saved on every change) ─────────────────────
    let pending_shortcut: Rc<RefCell<String>> = Rc::new(RefCell::new(shortcut.clone()));
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
        let pending_pin_c = pending_pin_shortcut.clone();
        let pending_limit_c = pending_limit.clone();
        let pending_show_hints_c = pending_show_hints.clone();
        let pending_terminal_c = pending_terminal.clone();
        Rc::new(move || {
            let new_word = entry_c.text().trim().to_lowercase();
            let new_shortcut = pending_shortcut_c.borrow().clone();
            if is_installed {
                if let Err(e) = crate::triggers::update_manifest(
                    &id_c,
                    &new_word,
                    &new_shortcut,
                    enabled,
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
            crate::ui::triggers_window::refresh_live();
        })
    };

    // ── Shortcut group ──────────────────────────────────────────────
    let shortcut_group = adw::PreferencesGroup::builder()
        .title("Keyboard Shortcut")
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
        let clip_group = adw::PreferencesGroup::builder().title("Clipboard").build();
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
            .title("History Limit")
            .subtitle("Maximum number of clipboard entries to keep")
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
            .title("Keep history")
            .subtitle("Delete clipboard entries older than this (pinned items are exempt)")
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
            .title("Show Keyboard Shortcuts")
            .subtitle("Display a hint bar in the clipboard manager")
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
        let find_group = adw::PreferencesGroup::builder().title("Find").build();
        content.append(&find_group);

        let root_row = adw::SwitchRow::builder()
            .title("Root Path Browsing")
            .subtitle("Allow typing / or ~/ to browse the filesystem directly")
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
            .title("Recent File/Folder Searches")
            .subtitle("Show recently opened files and folders")
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
            .label("Reset")
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
                        win_d.add_toast(adw::Toast::new("Trigger word cannot be empty"));
                        entry_d.set_text(&last_d.borrow());
                        return;
                    }
                    if new_word != *last_d.borrow() && word_taken(&id_d, &new_word, &cfg_d) {
                        let msg = format!("Trigger word \"{new_word}\" is already in use");
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
        .website("https://github.com/spotty/spotty")
        .issue_url("https://github.com/spotty/spotty/issues")
        .build();
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

/// Write one of the in-window shortcut config fields by name.
fn set_shortcut(cfg: &mut Config, field: &str, value: &str) {
    let v = value.to_string();
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
        .label("Reset")
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
        .label("Set shortcut…")
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
