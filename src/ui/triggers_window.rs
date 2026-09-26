//! Triggers window: installed trigger list + local manifest import.
//!
//! libadwaita layout (GNOME extension-manager style): a single `adw::Window`
//! with a `NavigationView` — the root page lists installed triggers.
//!
//! Installing = pick a downloaded manifest with "Import Trigger File…" (or
//! the settings Trigger-page `+`) → validate → copy into the triggers dir →
//! reload registry. Uninstall = delete the manifest. Shell triggers require
//! an explicit confirmation dialog that shows the exact command template.
//! Manifests come from the spotty-triggers GitHub repository — there is no
//! in-app marketplace; download a `.json` file and import it.
use crate::triggers::{self, TriggerAction, TriggerManifest};
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub struct TriggersWindow {
    window: adw::Window,
    installed_stack: gtk::Stack,
    installed_list: gtk::ListBox,
    /// Error banner on the root page (install/uninstall/import feedback).
    root_banner: adw::Banner,
}

thread_local! {
    /// The single live window (AppState caches one). `refresh_live` re-finds
    /// it so mutations started from other windows update this view too.
    static LIVE: RefCell<Option<Rc<TriggersWindow>>> = const { RefCell::new(None) };
}

impl TriggersWindow {
    /// Build the window and return it as an `Rc`. The Rc is also stored on
    /// the window widget (data slot) so signal closures can reach the struct;
    /// the returned Rc is what `AppState` keeps.
    pub fn new(app: &adw::Application) -> Rc<Self> {
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
        // Inline feedback for failed installs/uninstalls/imports.
        let root_banner = adw::Banner::builder().revealed(false).build();
        root_box.append(&root_banner);
        root_box.append(&clamp);
        root_scroll.set_child(Some(&root_box));

        // The only install path now: pick a downloaded manifest file.
        let button_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        let import_btn = gtk::Button::builder()
            .label("Import Trigger File…")
            .icon_name("document-open-symbolic")
            .css_classes(["suggested-action"])
            .tooltip_text("Install a trigger manifest (.json) you downloaded")
            .build();
        button_row.append(&import_btn);
        root_box.append(&button_row);

        let empty_page = adw::StatusPage::builder()
            .title("No triggers installed")
            .description("Triggers add new keywords — import a manifest file you downloaded.")
            .icon_name("package-symbolic")
            .build();
        installed_stack.add_named(&empty_page, Some("empty"));
        installed_stack.add_named(&installed_list, Some("list"));
        installed_stack.set_visible_child_name("empty");

        let root_page = adw::NavigationPage::builder()
            .title("Triggers")
            .child(&root_scroll)
            .build();

        nav.push(&root_page);

        let win = Rc::new(Self {
            window,
            installed_stack,
            installed_list,
            root_banner,
        });
        // Keep the Rc reachable for signal closures and the file-dialog
        // response handler (see LIVE above).
        unsafe {
            win.window
                .set_data::<Rc<Self>>("triggers-window-self", win.clone());
        }
        LIVE.with(|l| *l.borrow_mut() = Some(win.clone()));

        // ── Wiring ─────────────────────────────────────────────────────────
        {
            let w = win.clone();
            import_btn.connect_clicked(move |_| w.import_from_file());
        }

        win.rebuild_installed();
        win
    }

    /// Refresh the installed list each time the window shows, so changes
    /// made elsewhere (e.g. the Trigger settings page) show up.
    pub fn present(&self) {
        self.rebuild_installed();
        self.window.present();
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

    /// Shared install path for a manifest file already on disk (the file the
    /// user picked with "Import Trigger File…"): confirm shell triggers,
    /// then install.
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
    /// shared install path (validation + shell confirmation). Public so the
    /// settings Trigger-page `+` can start it after opening the window.
    pub fn import_from_file(&self) {
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

    /// Registry changed → refresh the installed list and re-sync GNOME
    /// keybindings on a background thread.
    pub fn after_mutation(&self) {
        self.hide_error();
        self.rebuild_installed();
        crate::ui::settings_window::refresh_triggers_live();
        std::thread::spawn(crate::keybindings::register_all);
    }

    // ── Feedback ────────────────────────────────────────────────────────

    /// Inline error banner (hidden on the next action).
    fn show_error(&self, msg: &str) {
        self.root_banner.set_title(msg);
        self.root_banner.set_revealed(true);
    }

    fn hide_error(&self) {
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

/// Refresh the triggers window's installed list.
/// Called from other windows after trigger mutations so the two views stay
/// in sync without requiring the triggers window to be closed and reopened.
pub fn refresh_live() {
    if let Some(w) = live_window() {
        w.after_mutation();
    }
}
