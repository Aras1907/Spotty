//! Triggers window: installed trigger list + marketplace browser.
//!
//! libadwaita layout (GNOME extension-manager style): a single `adw::Window`
//! with a `NavigationView` — the root page lists installed triggers, the
//! pushed "Marketplace" page browses a GitHub (or local `file://`) repository
//! with a `gtk::SearchEntry`. Every fetch goes through curl on a spawned
//! thread (flatpak-spawn aware, no new HTTP dependency), results hop back to
//! the main thread via `glib::MainContext::invoke`.
//!
//! Loading feedback is a spinner next to the search entry; errors surface in
//! an inline `adw::Banner` above the list (no toasts).
//!
//! Install = download manifest (or pick a downloaded file with
//! "Import Trigger File…") → validate → copy into the triggers dir →
//! reload registry. Uninstall = delete the manifest. Shell triggers require
//! an explicit confirmation dialog that shows the exact command template.
use crate::triggers::{self, TriggerAction, TriggerManifest, MarketEntry};
use crate::config::Config;
use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

pub struct TriggersWindow {
    window: adw::Window,
    nav: adw::NavigationView,
    installed_stack: gtk::Stack,
    installed_list: gtk::ListBox,
    market_list: gtk::ListBox,
    market_stack: gtk::Stack,
    market_empty_page: adw::StatusPage,
    market_error_page: adw::StatusPage,
    market_search: gtk::SearchEntry,
    market_spinner: gtk::DrawingArea,
    error_banner: adw::Banner,
    /// Error banner on the root page (install/uninstall/import feedback —
    /// the marketplace banner is only visible on the marketplace page).
    root_banner: adw::Banner,
    market_page: adw::NavigationPage,
    config: Rc<RefCell<Config>>,
    market: Rc<RefCell<Vec<MarketEntry>>>,
    
}

thread_local! {
    /// The single live window (AppState caches one). Spawned fetch threads
    /// can't capture the `Rc` (not Send), so response handlers re-find it
    /// here via a Send-only payload on the main thread.
    static LIVE: RefCell<Option<Rc<TriggersWindow>>> = const { RefCell::new(None) };
}

impl TriggersWindow {
    /// Build the window and return it as an `Rc`. The Rc is also stored on
    /// the window widget (data slot) so signal closures can reach the struct;
    /// the returned Rc is what `AppState` keeps.
    pub fn new(app: &adw::Application, config: Rc<RefCell<Config>>) -> Rc<Self> {
        let window = adw::Window::builder()
            .application(app)
            .title("Triggers")
            .default_width(560)
            .default_height(640)
            .hide_on_close(true)
            .build();

        let nav = adw::NavigationView::new();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&nav));
        window.set_content(Some(&toolbar));

        // ── Root page: installed triggers ─────────────────────────────────
        let installed_stack = gtk::Stack::new();
        let installed_list = gtk::ListBox::builder()
            .css_classes(["spotty-flat-list"])
            .selection_mode(gtk::SelectionMode::None)
            .build();

        let root_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let root_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(24)
            .margin_end(24)
            .build();
        let clamp = adw::Clamp::builder().maximum_size(520).build();
        clamp.set_child(Some(&installed_stack));
        // Inline feedback for failed installs/uninstalls started from this
        // page (the marketplace's own banner lives on the marketplace page).
        let root_banner = adw::Banner::builder().revealed(false).build();
        root_box.append(&root_banner);
        root_box.append(&clamp);
        root_scroll.set_child(Some(&root_box));

        let button_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        let browse_btn = gtk::Button::builder()
            .label("Browse New Trigger")
            .icon_name("folder-download-symbolic")
            .css_classes(["suggested-action"])
            .build();
        // Install a manifest the user downloaded from GitHub (or anywhere)
        // without going through the marketplace index.
        let import_btn = gtk::Button::builder()
            .label("Import Trigger File…")
            .icon_name("document-open-symbolic")
            .tooltip_text("Install a downloaded trigger manifest (.json)")
            .build();
        button_row.append(&browse_btn);
        button_row.append(&import_btn);
        root_box.append(&button_row);

        let empty_page = adw::StatusPage::builder()
            .title("No triggers installed")
            .description("Triggers add new keywords — browse the marketplace.")
            .icon_name("package-symbolic")
            .build();
        installed_stack.add_named(&empty_page, Some("empty"));
        installed_stack.add_named(&installed_list, Some("list"));
        installed_stack.set_visible_child_name("empty");

        let root_page = adw::NavigationPage::builder()
            .title("Triggers")
            .child(&root_scroll)
            .build();

        // ── Marketplace page ──────────────────────────────────────────────
        let market_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let market_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(24)
            .margin_end(24)
            .build();
        market_scroll.set_child(Some(&market_box));

        // Error banner: inline feedback for failed fetches/installs.
        let error_banner = adw::Banner::builder().revealed(false).build();
        market_box.append(&error_banner);

        let search_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let market_search = gtk::SearchEntry::builder()
            .placeholder_text("Search triggers")
            .hexpand(true)
            .build();
        let market_spinner = crate::ui::circular_progress::progress_ring(
            20,
            None,
            crate::ui::circular_progress::RingState::Running,
        );
        market_spinner.set_visible(false);
        search_bar.append(&market_search);
        search_bar.append(&market_spinner);
        market_box.append(&search_bar);

        let market_list = gtk::ListBox::builder()
            .css_classes(["spotty-flat-list"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let market_empty_page = adw::StatusPage::builder()
            .title("No triggers found")
            .description("The marketplace has no triggers yet.")
            .icon_name("edit-find-symbolic")
            .build();
        let market_error_page = adw::StatusPage::builder()
            .title("Couldn't load the marketplace")
            .description("Check your connection and try again.")
            .icon_name("dialog-error-symbolic")
            .build();

        let market_stack = gtk::Stack::new();
        market_stack.add_named(&market_list, Some("list"));
        market_stack.add_named(&market_empty_page, Some("empty"));
        market_stack.add_named(&market_error_page, Some("error"));
        market_stack.set_visible_child_name("list");
        let market_clamp = adw::Clamp::builder().maximum_size(520).build();
        market_clamp.set_child(Some(&market_stack));
        market_box.append(&market_clamp);

        let market_page = adw::NavigationPage::builder()
            .title("Marketplace")
            .child(&market_scroll)
            .build();

        nav.push(&root_page);

        let win = Rc::new(Self {
            window,
            nav,
            installed_stack,
            installed_list,
            market_list,
            market_stack,
            market_empty_page,
            market_error_page,
            market_search,
            market_spinner,
            error_banner,
            root_banner,
            market_page,
            config,
            market: Rc::new(RefCell::new(Vec::new())),
        });
        // Keep the Rc reachable for signal closures and spawned-thread
        // response handlers (see LIVE above).
        unsafe {
            win.window
                .set_data::<Rc<Self>>("triggers-window-self", win.clone());
        }
        LIVE.with(|l| *l.borrow_mut() = Some(win.clone()));

        // ── Wiring ─────────────────────────────────────────────────────────
        {
            let w = win.clone();
            browse_btn.connect_clicked(move |_| w.show_marketplace());
        }

        {
            let w = win.clone();
            import_btn.connect_clicked(move |_| w.import_from_file());
        }

        {
            let w = win.clone();
            let search = win.market_search.clone();
            search.connect_search_changed(move |_| w.rebuild_market());
        }
        // Fetch a fresh index on every visit so a stale GitHub CDN response
        // never sticks in the cached list once it propagates.
        {
            let w = win.clone();
            let mp = w.market_page.clone();
            mp.connect_map(move |_| w.fetch_index());
        }

        win.rebuild_installed();
        win
    }

    /// Refresh installed list + marketplace rows each time the window shows,
    /// so uninstalls done elsewhere (e.g. the Trigger settings page) show up.
    pub fn present(&self) {
        self.rebuild_installed();
        self.rebuild_market();
        self.window.present();
    }

    /// Show the window directly on the Marketplace page (used by the
    /// "Browse Marketplace…" entry from settings).
    pub fn show_marketplace(&self) {
        self.present();
        self.nav.push(&self.market_page);
    }

    // ── Installed list ──────────────────────────────────────────────────

    fn rebuild_installed(&self) {
        while let Some(c) = self.installed_list.first_child() {
            self.installed_list.remove(&c);
        }
        let triggers = triggers::all();
        if triggers.is_empty() {
            log::info!("triggers: window installed list is empty (registry count 0)");
            self.installed_stack.set_visible_child_name("empty");
            return;
        }
        log::info!(
            "triggers: window showing {} installed: {}",
            triggers.len(),
            triggers.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(", ")
        );
        self.installed_stack.set_visible_child_name("list");
        for a in &triggers {
            let row = self.installed_row(a);
            self.installed_list.append(&row);
        }
    }

    fn installed_row(&self, a: &TriggerManifest) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        let action = adw::ActionRow::builder()
            .title(&a.name)
            .subtitle(format!(
                "{} — {}",
                a.word,
                if a.description.is_empty() {
                    "installed trigger"
                } else {
                    &a.description
                }
            ))
            .build();
        action.add_prefix(&Self::icon_image(&a.icon));
        let uninstall = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .css_classes(["flat", "circular"])
            .tooltip_text("Uninstall")
            .build();
        {
            let w = self.self_rc();
            let id = a.id.clone();
            uninstall.connect_clicked(move |_| w.uninstall(&id));
        }
        action.add_suffix(&uninstall);
        row.set_child(Some(&action));
        row
    }

    // ── Marketplace ─────────────────────────────────────────────────────

    fn fetch_index(&self) {
        self.hide_error();
        self.set_loading(true);
        let url = format!(
            "{}/index.json",
            self.config.borrow().trigger_repo_url.trim().trim_end_matches('/')
        );
        std::thread::spawn(move || {
            let body = triggers::fetch_text(&url);
            glib::MainContext::default().invoke(move || {
                if let Some(w) = live_window() {
                    w.handle_index_response(body);
                }
            });
        });
    }

    /// Main thread: apply a fetched index (rebuild the marketplace list).
    fn handle_index_response(&self, body: Result<String, String>) {
        self.set_loading(false);
        match body {
            Ok(text) => match serde_json::from_str::<Vec<MarketEntry>>(&text) {
                Ok(entries) => {
                    log::info!("triggers: market index loaded: {} entries", entries.len());
                    *self.market.borrow_mut() = entries;
                    self.rebuild_market();
                }
                Err(e) => {
                    log::warn!("triggers: invalid repository index: {e}");
                    self.market_error_page
                        .set_description(Some(&format!("The repository index is not valid: {e}")));
                    self.market_stack.set_visible_child_name("error");
                    self.show_error("Couldn't load the marketplace");
                }
            },
            Err(e) => {
                log::warn!("triggers: market fetch failed: {e}");
                self.market_error_page.set_description(Some(&e));
                self.market_stack.set_visible_child_name("error");
                self.show_error("Couldn't load the marketplace");
            }
        }
    }

    fn rebuild_market(&self) {
        while let Some(c) = self.market_list.first_child() {
            self.market_list.remove(&c);
        }
        let q = self.market_search.text().to_lowercase();
        let mut count = 0;
        for e in self.market.borrow().iter() {
            if !q.is_empty()
                && !e.name.to_lowercase().contains(&q)
                && !e.id.to_lowercase().contains(&q)
                && !e.summary.to_lowercase().contains(&q)
            {
                continue;
            }
            count += 1;
            let row = self.market_row(e);
            self.market_list.append(&row);
        }
        if count == 0 {
            let qtext = self.market_search.text();
            let desc = if q.is_empty() {
                "The marketplace has no triggers yet.".to_string()
            } else {
                format!("No triggers match \"{qtext}\".")
            };
            self.market_empty_page.set_description(Some(&desc));
            self.market_stack.set_visible_child_name("empty");
        } else {
            self.market_stack.set_visible_child_name("list");
        }
    }

    fn market_row(&self, e: &MarketEntry) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        let action = adw::ActionRow::builder()
            .title(&e.name)
            .subtitle(&e.summary)
            .subtitle_lines(2)
            .build();
        action.add_prefix(&Self::icon_image(&e.icon));
        if triggers::by_id(&e.id).is_some() {
            let uninstall = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .css_classes(["flat", "circular"])
                .tooltip_text("Uninstall")
                .build();
            {
                let w = self.self_rc();
                let id = e.id.clone();
                uninstall.connect_clicked(move |_| w.uninstall(&id));
            }
            action.add_suffix(&uninstall);
        } else {
            let install = gtk::Button::builder()
                .label("Install")
                .css_classes(["flat"])
                .build();
            {
                let w = self.self_rc();
                let entry = e.clone();
                install.connect_clicked(move |_| w.install_market(&entry));
            }
            action.add_suffix(&install);
        }
        row.set_child(Some(&action));
        row
    }

    /// The trigger's icon as a plain prefix image (no tile background), with
    /// a generic fallback when the manifest has no icon.
    fn icon_image(name: &str) -> gtk::Image {
        let icon = gtk::Image::from_icon_name(if name.is_empty() {
            "package-x-generic-symbolic"
        } else {
            name
        });
        icon.set_pixel_size(24);
        icon
    }

    /// Install a trigger from the marketplace: fetch its manifest, confirm
    /// shell triggers (showing the command), then install.
    fn install_market(&self, entry: &MarketEntry) {
        self.hide_error();
        self.set_loading(true);
        let url = format!(
            "{}/triggers/{}.json",
            self.config.borrow().trigger_repo_url.trim().trim_end_matches('/'),
            entry.id
        );
        let id = entry.id.clone();
        std::thread::spawn(move || {
            let body = triggers::fetch_text(&url);
            glib::MainContext::default().invoke(move || {
                if let Some(w) = live_window() {
                    w.handle_manifest_response(body, id.clone());
                }
            });
        });
    }

    /// Main thread: manifest downloaded — parse, write to a temp file,
    /// confirm shell triggers, then install.
    fn handle_manifest_response(&self, body: Result<String, String>, id: String) {
        self.set_loading(false);
        let text = match body {
            Ok(t) => t,
            Err(e) => {
                log::warn!("triggers: manifest fetch failed: {e}");
                self.show_error(&format!("Couldn't fetch {}: {e}", id));
                return;
            }
        };
        let tmp = std::env::temp_dir().join(format!("spotty_trigger_{id}.json"));
        if let Err(e) = std::fs::write(&tmp, &text) {
            self.show_error(&format!("Cannot save manifest: {e}"));
            return;
        }
        self.install_path(&tmp);
    }

    /// Shared install path for a manifest file already on disk (a freshly
    /// downloaded marketplace manifest or a file the user picked with
    /// "Import Trigger File…"): confirm shell triggers, then install.
    fn install_path(&self, path: &std::path::Path) {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(manifest) = serde_json::from_str::<TriggerManifest>(&text) {
                if matches!(manifest.action, TriggerAction::Shell { .. }) {
                    self.confirm_shell_install(manifest, path.to_path_buf());
                    return;
                }
            }
        }
        self.finish_install(path);
    }

    /// Let the user install a downloaded manifest directly: file picker →
    /// shared install path (validation + shell confirmation).
    fn import_from_file(&self) {
        self.hide_error();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Trigger manifests (*.json)"));
        filter.add_suffix("json");
        filter.add_mime_type("application/json");
        let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Import Trigger File")
            .modal(true)
            .filters(&filters)
            .default_filter(&filter)
            .build();
        let w = self.self_rc();
        dialog.open(
            Some(&self.window),
            None::<&gtk::gio::Cancellable>,
            move |result| match result {
                Ok(file) => {
                    if let Some(path) = file.path() {
                        w.install_path(&path);
                    }
                }
                Err(_) => {
                    // Dismissed the picker — not an error worth surfacing.
                    log::info!("triggers: import cancelled");
                }
            },
        );
    }

    /// Shell triggers run arbitrary commands as the user — always confirm and
    /// show the exact command template before installing.
    fn confirm_shell_install(&self, manifest: TriggerManifest, tmp: std::path::PathBuf) {
        let command = match &manifest.action {
            TriggerAction::Shell { command } => command.clone(),
            _ => String::new(),
        };
        let dialog = adw::MessageDialog::builder()
            .transient_for(&self.window)
            .heading(format!("Install {}?", manifest.name))
            .body(format!(
                "This trigger runs shell commands on your system.\n\n{}\n\nInstall only if you trust its author.",
                command
            ))
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("install", "Install");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("install", adw::ResponseAppearance::Destructive);
        {
            let w = self.self_rc();
            dialog.connect_response(Some("install"), move |_, resp| {
                if resp == "install" {
                    w.finish_install(&tmp);
                }
            });
        }
        dialog.present();
    }

    fn finish_install(&self, tmp: &std::path::Path) {
        match triggers::install_from_file(tmp) {
            Ok(_) => self.after_mutation(),
            Err(e) => {
                log::warn!("triggers: install failed: {e}");
                self.show_error(&format!("Install failed: {e}"));
            }
        }
    }

    fn uninstall(&self, id: &str) {
        match triggers::uninstall(id) {
            Ok(()) => self.after_mutation(),
            Err(e) => {
                log::warn!("triggers: uninstall failed: {e}");
                self.show_error(&format!("Uninstall failed: {e}"));
            }
        }
    }

    /// Registry changed → refresh both lists and re-sync GNOME keybindings
    /// on a background thread.
    pub fn after_mutation(&self) {
        self.hide_error();
        self.rebuild_installed();
        self.rebuild_market();
        crate::ui::settings_window::refresh_triggers_live();
        std::thread::spawn(crate::keybindings::register_all);
    }

    // ── Feedback ────────────────────────────────────────────────────────

    /// Spinner next to the search entry while any fetch is in flight.
    fn set_loading(&self, loading: bool) {
        self.market_spinner.set_visible(loading);
    }

    /// Inline error banner (hidden on the next action). Shown on both pages
    /// so a failure started on one is visible wherever the user is.
    fn show_error(&self, msg: &str) {
        self.error_banner.set_title(msg);
        self.error_banner.set_revealed(true);
        self.root_banner.set_title(msg);
        self.root_banner.set_revealed(true);
    }

    fn hide_error(&self) {
        self.error_banner.set_revealed(false);
        self.root_banner.set_revealed(false);
    }

    fn self_rc(&self) -> Rc<Self> {
        match unsafe { self.window.data::<Rc<Self>>("triggers-window-self") } {
            Some(ptr) => unsafe { ptr.as_ref() }
                .clone(),
            None => panic!("triggers window self not set"),
        }
    }
}

/// The live triggers window, if one exists (see `LIVE`).
pub fn live_window() -> Option<Rc<TriggersWindow>> {
    LIVE.with(|l| l.borrow().clone())
}

/// Refresh the triggers window's installed list + marketplace rows.
/// Called from other windows after trigger mutations so the two views stay
/// in sync without requiring the triggers window to be closed and reopened.
pub fn refresh_live() {
    if let Some(w) = live_window() {
        w.after_mutation();
    }
}
