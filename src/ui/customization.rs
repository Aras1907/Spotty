use crate::config::{Appearance, BackgroundStyle, BorderColorMode, BorderStyle, ColorMode, Config, ResultDensity, ThemePalette};
use crate::i18n::gettext;
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn update(config: &Rc<RefCell<Config>>, change: impl FnOnce(&mut Appearance)) {
    {
        let mut config = config.borrow_mut();
        change(&mut config.appearance);
        config.appearance.normalize();
        config.save();
    }
    super::appearance::apply(&config.borrow().appearance);
    crate::app::refresh_search_window();
}

fn combo(
    group: &adw::PreferencesGroup, config: &Rc<RefCell<Config>>, syncing: &Rc<Cell<bool>>,
    title: &str, subtitle: &str, choices: &[&str], selected: u32,
    change: fn(&mut Appearance, u32),
) -> adw::ComboRow {
    let choices: Vec<String> = choices.iter().map(|label| gettext(label)).collect();
    let strings: Vec<&str> = choices.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .title(gettext(title)).subtitle(gettext(subtitle)).use_markup(false)
        .model(&gtk::StringList::new(&strings)).selected(selected).build();
    let cfg = config.clone();
    let guard = syncing.clone();
    row.connect_selected_notify(move |row| {
        if !guard.get() { update(&cfg, |a| change(a, row.selected())); }
    });
    group.add(&row);
    row
}

fn spin(
    group: &adw::PreferencesGroup, config: &Rc<RefCell<Config>>, syncing: &Rc<Cell<bool>>,
    title: &str, subtitle: &str, value: u32, min: f64, max: f64, step: f64,
    change: fn(&mut Appearance, u32),
) -> adw::SpinRow {
    let row = adw::SpinRow::builder()
        .title(gettext(title)).subtitle(gettext(subtitle)).use_markup(false)
        .adjustment(&gtk::Adjustment::new(value as f64, min, max, step, step * 5.0, 0.0))
        .numeric(true).digits(0).build();
    let cfg = config.clone();
    let guard = syncing.clone();
    row.connect_value_notify(move |row| {
        if !guard.get() { update(&cfg, |a| change(a, row.value().round() as u32)); }
    });
    group.add(&row);
    row
}

fn switch(
    group: &adw::PreferencesGroup, config: &Rc<RefCell<Config>>, syncing: &Rc<Cell<bool>>,
    title: &str, subtitle: &str, active: bool, change: fn(&mut Appearance, bool),
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(gettext(title)).subtitle(gettext(subtitle)).use_markup(false).active(active).build();
    let cfg = config.clone();
    let guard = syncing.clone();
    row.connect_active_notify(move |row| {
        if !guard.get() { update(&cfg, |a| change(a, row.is_active())); }
    });
    group.add(&row);
    row
}

const PALETTES: [ThemePalette; 5] = [
    ThemePalette::Default, ThemePalette::Nord, ThemePalette::Solarized,
    ThemePalette::RosePine, ThemePalette::Custom,
];

fn custom_colors(
    page: &adw::PreferencesPage, config: &Rc<RefCell<Config>>, syncing: &Rc<Cell<bool>>, dark: bool,
) -> (adw::PreferencesGroup, Vec<gtk::ColorDialogButton>) {
    let group = adw::PreferencesGroup::builder()
        .title(gettext(if dark { "Custom Dark Colors" } else { "Custom Light Colors" }))
        .description(gettext("Choose the background, text and accent for this mode."))
        .build();
    let appearance = config.borrow().appearance.clone();
    let colors = if dark { &appearance.dark_colors } else { &appearance.light_colors };
    let mut buttons = Vec::new();
    for (index, (title, value)) in [
        ("Background", &colors.background), ("Text", &colors.foreground), ("Accent", &colors.accent),
    ].into_iter().enumerate() {
        let row = adw::ActionRow::builder().title(gettext(title)).use_markup(false).build();
        let dialog = gtk::ColorDialog::builder().title(gettext(title)).with_alpha(false).build();
        let button = gtk::ColorDialogButton::builder().dialog(&dialog)
            .rgba(&gtk::gdk::RGBA::parse(value).expect("normalized theme colour"))
            .valign(gtk::Align::Center).tooltip_text(gettext("Choose a color")).build();
        let cfg = config.clone();
        let guard = syncing.clone();
        button.connect_rgba_notify(move |button| {
            if guard.get() { return; }
            let color = button.rgba();
            let hex = format!("#{:02x}{:02x}{:02x}",
                (color.red() * 255.0).round() as u8,
                (color.green() * 255.0).round() as u8,
                (color.blue() * 255.0).round() as u8);
            update(&cfg, |a| {
                let colors = if dark { &mut a.dark_colors } else { &mut a.light_colors };
                match index {
                    0 => colors.background = hex,
                    1 => colors.foreground = hex,
                    _ => colors.accent = hex,
                }
            });
        });
        row.add_suffix(&button);
        group.add(&row);
        buttons.push(button);
    }
    let palette = if dark { appearance.dark_theme } else { appearance.light_theme };
    group.set_visible(palette == ThemePalette::Custom);
    page.add(&group);
    (group, buttons)
}

pub(super) fn build(window: &adw::PreferencesWindow, config: &Rc<RefCell<Config>>) {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Customization")).icon_name("applications-graphics-symbolic").build();
    window.add(&page);
    let syncing = Rc::new(Cell::new(false));
    let appearance = config.borrow().appearance.clone();
    let themes = adw::PreferencesGroup::builder().title(gettext("Theme"))
        .description(gettext("Changes apply immediately. Default uses the desktop's Adwaita colors. High contrast keeps the desktop's accessible colors."))
        .build();
    page.add(&themes);
    let mode = combo(&themes, config, &syncing, "Appearance", "Follow the desktop or choose a fixed mode",
        &["Follow System", "Light", "Dark"], appearance.color_mode as u32,
        |a, i| a.color_mode = [ColorMode::System, ColorMode::Light, ColorMode::Dark][i.min(2) as usize]);
    let names = ["Default (Adwaita)", "Nord", "Solarized", "Rosé Pine", "Custom Colors"];
    let light = combo(&themes, config, &syncing, "Light Theme", "Palette used in light mode",
        &names, appearance.light_theme as u32,
        |a, i| a.light_theme = PALETTES[i.min(4) as usize]);
    let dark = combo(&themes, config, &syncing, "Dark Theme", "Palette used in dark mode",
        &names, appearance.dark_theme as u32,
        |a, i| a.dark_theme = PALETTES[i.min(4) as usize]);
    let (light_group, light_colors) = custom_colors(&page, config, &syncing, false);
    let (dark_group, dark_colors) = custom_colors(&page, config, &syncing, true);
    light.connect_selected_notify(move |row| light_group.set_visible(row.selected() == 4));
    dark.connect_selected_notify(move |row| dark_group.set_visible(row.selected() == 4));

    let background = adw::PreferencesGroup::builder().title(gettext("Window Background"))
        .description(gettext("Transparency changes the background only, keeping text and icons opaque."))
        .build();
    page.add(&background);
    let style = combo(&background, config, &syncing, "Background Style", "Choose a solid or translucent launcher",
        &["Solid", "Transparent", "Frosted Glass (Desktop Blur)"], appearance.background_style as u32,
        |a, i| a.background_style = [BackgroundStyle::Solid, BackgroundStyle::Transparent, BackgroundStyle::Frosted][i.min(2) as usize]);
    let opacity = spin(&background, config, &syncing, "Background Opacity (%)", "Lower values show more of the desktop",
        appearance.background_opacity, 35.0, 100.0, 5.0, |a, v| a.background_opacity = v);
    opacity.set_sensitive(appearance.background_style != BackgroundStyle::Solid);
    let blur_help = adw::ActionRow::builder().title(gettext("Enable Desktop Blur"))
        .subtitle(gettext("On GNOME, enable application blur in Blur My Shell and add Spotty to its whitelist. Without desktop blur, Frosted Glass remains translucent. High contrast uses a solid background."))
        .use_markup(false).build();
    let blur_link = gtk::LinkButton::with_label(
        "https://github.com/aunetx/blur-my-shell#functionalities", &gettext("Setup Guide"));
    blur_link.set_valign(gtk::Align::Center);
    blur_help.add_suffix(&blur_link);
    blur_help.set_visible(appearance.background_style == BackgroundStyle::Frosted);
    background.add(&blur_help);
    {
        let opacity = opacity.clone();
        style.connect_selected_notify(move |row| {
            opacity.set_sensitive(row.selected() != 0);
            blur_help.set_visible(row.selected() == 2);
        });
    }

    let border = adw::PreferencesGroup::builder().title(gettext("Window Border"))
        .description(gettext("Choose the launcher's outline. High contrast keeps a visible theme border."))
        .build();
    page.add(&border);
    let border_enabled = switch(&border, config, &syncing, "Show Border", "Draw an outline around the launcher",
        appearance.show_border, |a, v| a.show_border = v);
    let border_width = spin(&border, config, &syncing, "Border Width (px)", "Thickness of the window outline",
        appearance.border_width, 1.0, 8.0, 1.0, |a, v| a.border_width = v);
    let border_style = combo(&border, config, &syncing, "Border Style", "Pattern used for the outline",
        &["Solid", "Dashed", "Dotted"], appearance.border_style as u32,
        |a, i| a.border_style = [BorderStyle::Solid, BorderStyle::Dashed, BorderStyle::Dotted][i.min(2) as usize]);
    let border_color_mode = combo(&border, config, &syncing, "Border Color", "Follow the theme, use its accent, or pick a color",
        &["Follow Theme", "Accent Color", "Custom Color"], appearance.border_color_mode as u32,
        |a, i| a.border_color_mode = [BorderColorMode::Theme, BorderColorMode::Accent, BorderColorMode::Custom][i.min(2) as usize]);
    let border_opacity = spin(&border, config, &syncing, "Border Opacity (%)", "Strength of the selected border color",
        appearance.border_opacity, 10.0, 100.0, 5.0, |a, v| a.border_opacity = v);
    let color_row = adw::ActionRow::builder().title(gettext("Custom Border Color")).use_markup(false).build();
    let color_dialog = gtk::ColorDialog::builder().title(gettext("Border Color")).with_alpha(false).build();
    let border_color = gtk::ColorDialogButton::builder().dialog(&color_dialog)
        .rgba(&gtk::gdk::RGBA::parse(&appearance.border_color).expect("normalized border colour"))
        .valign(gtk::Align::Center).tooltip_text(gettext("Choose a color")).build();
    {
        let cfg = config.clone();
        let guard = syncing.clone();
        border_color.connect_rgba_notify(move |button| {
            if guard.get() { return; }
            let color = button.rgba();
            let hex = format!("#{:02x}{:02x}{:02x}",
                (color.red() * 255.0).round() as u8,
                (color.green() * 255.0).round() as u8,
                (color.blue() * 255.0).round() as u8);
            update(&cfg, |a| a.border_color = hex);
        });
    }
    color_row.add_suffix(&border_color);
    color_row.set_visible(appearance.border_color_mode == BorderColorMode::Custom);
    border.add(&color_row);
    border_color_mode.connect_selected_notify({
        let color_row = color_row.clone();
        move |row| color_row.set_visible(row.selected() == 2)
    });
    let controls = vec![border_width.clone().upcast::<gtk::Widget>(), border_style.clone().upcast(),
        border_color_mode.clone().upcast(), border_opacity.clone().upcast(), color_row.upcast()];
    for row in &controls { row.set_sensitive(appearance.show_border); }
    border_enabled.connect_active_notify(move |row| {
        for control in &controls { control.set_sensitive(row.is_active()); }
    });

    let layout = adw::PreferencesGroup::builder().title(gettext("Layout & Text"))
        .description(gettext("Adjust the launcher size and the space used by results."))
        .build();
    page.add(&layout);
    let width = spin(&layout, config, &syncing, "Window Width (px)", "Width of the search window",
        appearance.window_width, 650.0, 1100.0, 25.0, |a, v| a.window_width = v);
    let rows = spin(&layout, config, &syncing, "Visible Results", "More results remain available by scrolling",
        appearance.visible_results, 3.0, 12.0, 1.0, |a, v| a.visible_results = v);
    let text = spin(&layout, config, &syncing, "Text Size (%)", "Scale launcher text relative to your desktop font",
        appearance.text_scale, 80.0, 140.0, 5.0, |a, v| a.text_scale = v);
    let density = combo(&layout, config, &syncing, "Result Spacing", "Space around each search result",
        &["Compact", "Comfortable", "Spacious"], appearance.density as u32,
        |a, i| a.density = [ResultDensity::Compact, ResultDensity::Comfortable, ResultDensity::Spacious][i.min(2) as usize]);
    let corners = spin(&layout, config, &syncing, "Corner Radius (px)", "Use zero for square corners",
        appearance.corner_radius, 0.0, 32.0, 2.0, |a, v| a.corner_radius = v);
    let shadow = switch(&layout, config, &syncing, "Window Shadow", "Add a soft shadow to the launcher",
        appearance.show_shadow, |a, v| a.show_shadow = v);
    let preview = switch(&layout, config, &syncing, "Result Preview", "Show a preview beside files and supported results",
        appearance.show_preview, |a, v| a.show_preview = v);
    let motion = switch(&layout, config, &syncing, "Reduce Motion", "Disable transitions, fading and animated highlights",
        appearance.reduced_motion, |a, v| a.reduced_motion = v);
    super::settings_window::build_footer_customization(&page, config);

    let actions = adw::PreferencesGroup::new();
    page.add(&actions);
    let preview_row = adw::ActionRow::builder().title(gettext("Preview Spotty"))
        .subtitle(gettext("Open the launcher with your current appearance"))
        .activatable(true).use_markup(false).build();
    preview_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let app = window.application().and_downcast::<adw::Application>();
    preview_row.connect_activated(move |_| {
        if let Some(app) = &app { crate::app::show_search(app); }
    });
    actions.add(&preview_row);
    let reset = adw::ActionRow::builder().title(gettext("Reset Appearance"))
        .subtitle(gettext("Restore default themes, background, border, layout and text size"))
        .activatable(true).use_markup(false).build();
    reset.add_css_class("spotty-reset");
    reset.add_suffix(&gtk::Image::from_icon_name("edit-undo-symbolic"));
    let cfg = config.clone();
    reset.connect_activated(move |_| {
        let defaults = Appearance::default();
        syncing.set(true);
        mode.set_selected(defaults.color_mode as u32);
        light.set_selected(defaults.light_theme as u32);
        dark.set_selected(defaults.dark_theme as u32);
        style.set_selected(defaults.background_style as u32);
        opacity.set_value(defaults.background_opacity as f64);
        width.set_value(defaults.window_width as f64);
        rows.set_value(defaults.visible_results as f64);
        text.set_value(defaults.text_scale as f64);
        density.set_selected(defaults.density as u32);
        corners.set_value(defaults.corner_radius as f64);
        border_enabled.set_active(defaults.show_border);
        border_width.set_value(defaults.border_width as f64);
        border_style.set_selected(defaults.border_style as u32);
        border_color_mode.set_selected(defaults.border_color_mode as u32);
        border_opacity.set_value(defaults.border_opacity as f64);
        border_color.set_rgba(&gtk::gdk::RGBA::parse(&defaults.border_color).expect("default border colour"));
        shadow.set_active(defaults.show_shadow);
        preview.set_active(defaults.show_preview);
        motion.set_active(defaults.reduced_motion);
        for (buttons, colors) in [(&light_colors, &defaults.light_colors), (&dark_colors, &defaults.dark_colors)] {
            for (button, color) in buttons.iter().zip([&colors.background, &colors.foreground, &colors.accent]) {
                button.set_rgba(&gtk::gdk::RGBA::parse(color).expect("default theme colour"));
            }
        }
        syncing.set(false);
        update(&cfg, |a| *a = defaults);
    });
    actions.add(&reset);
}
