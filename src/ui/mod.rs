pub mod result_row;
pub mod search_window;
pub mod settings_window;
pub mod circular_progress;

/// GTK's (and so the desktop's) animation switch.
pub fn animations_enabled() -> bool {
    use gtk::prelude::*;
    gtk::Settings::default()
        .is_none_or(|settings| settings.property::<bool>("gtk-enable-animations"))
}
