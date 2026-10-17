//! One reusable application provider, refreshed on preference or desktop
//! colour-scheme changes. Background alpha is independent of text opacity.
use crate::config::{Appearance, BackgroundStyle, BorderColorMode, BorderStyle, ColorMode, ResultDensity, ThemeColors, ThemePalette};
use adw::prelude::*;
use std::cell::RefCell;

thread_local! {
    static CURRENT: RefCell<Appearance> = RefCell::new(Appearance::default());
    static PROVIDER: gtk::CssProvider = {
        let provider = gtk::CssProvider::new();
        provider.connect_parsing_error(|_, _, error| log::warn!("Appearance CSS: {error}"));
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
        let manager = adw::StyleManager::default();
        manager.connect_dark_notify(|_| render());
        manager.connect_high_contrast_notify(|_| render());
        provider
    };
    static DESKTOP_ANIMATIONS: bool = gtk::Settings::default()
        .is_none_or(|settings| settings.property::<bool>("gtk-enable-animations"));
}

pub fn apply(appearance: &Appearance) {
    let mut appearance = appearance.clone();
    appearance.normalize();
    CURRENT.with(|current| *current.borrow_mut() = appearance.clone());
    // Register the provider and desktop notifications once, before changing
    // the style manager. A notification may synchronously re-enter render.
    PROVIDER.with(|_| {});
    adw::StyleManager::default().set_color_scheme(match appearance.color_mode {
        ColorMode::System => adw::ColorScheme::Default,
        ColorMode::Light => adw::ColorScheme::ForceLight,
        ColorMode::Dark => adw::ColorScheme::ForceDark,
    });
    if let Some(settings) = gtk::Settings::default() {
        DESKTOP_ANIMATIONS.with(|desktop| {
            settings.set_property("gtk-enable-animations", *desktop && !appearance.reduced_motion);
        });
    }
    render();
}

pub fn animations_enabled() -> bool {
    gtk::Settings::default()
        .is_none_or(|settings| settings.property::<bool>("gtk-enable-animations"))
}

fn colors(appearance: &Appearance, dark: bool) -> Option<ThemeColors> {
    let palette = if dark { appearance.dark_theme } else { appearance.light_theme };
    let (background, foreground, accent) = match (palette, dark) {
        (ThemePalette::Default, _) => return None,
        (ThemePalette::Custom, false) => return Some(appearance.light_colors.clone()),
        (ThemePalette::Custom, true) => return Some(appearance.dark_colors.clone()),
        (ThemePalette::Nord, false) => ("#eceff4", "#2e3440", "#406b86"),
        (ThemePalette::Nord, true) => ("#2e3440", "#eceff4", "#88c0d0"),
        (ThemePalette::Solarized, false) => ("#fdf6e3", "#586e75", "#007d9c"),
        (ThemePalette::Solarized, true) => ("#002b36", "#93a1a1", "#2aa198"),
        (ThemePalette::RosePine, false) => ("#faf4ed", "#575279", "#907aa9"),
        (ThemePalette::RosePine, true) => ("#191724", "#e0def4", "#c4a7e7"),
    };
    Some(ThemeColors {
        background: background.into(), foreground: foreground.into(), accent: accent.into(),
    })
}

fn accent_foreground(color: &str) -> &'static str {
    let rgba = gtk::gdk::RGBA::parse(color).expect("normalized theme colour");
    let linear = |channel: f32| {
        let channel = channel as f64;
        if channel <= 0.04045 { channel / 12.92 }
        else { ((channel + 0.055) / 1.055).powf(2.4) }
    };
    let luminance = 0.2126 * linear(rgba.red())
        + 0.7152 * linear(rgba.green()) + 0.0722 * linear(rgba.blue());
    if luminance > 0.179 { "#000000" } else { "#ffffff" }
}

fn render() {
    let manager = adw::StyleManager::default();
    let dark = manager.is_dark();
    let high_contrast = manager.is_high_contrast();
    let appearance = CURRENT.with(|current| current.borrow().clone());
    let mut css = String::new();
    if !high_contrast {
        if let Some(colors) = colors(&appearance, dark) {
            let bg = &colors.background;
            let fg = &colors.foreground;
            let accent = &colors.accent;
            let accent_fg = accent_foreground(accent);
            css.push_str(&format!(
                "@define-color window_bg_color {bg};\n\
                 @define-color window_fg_color {fg};\n\
                 @define-color view_bg_color {bg};\n\
                 @define-color view_fg_color {fg};\n\
                 @define-color headerbar_bg_color {bg};\n\
                 @define-color headerbar_fg_color {fg};\n\
                 @define-color dialog_bg_color {bg};\n\
                 @define-color dialog_fg_color {fg};\n\
                 @define-color popover_bg_color {bg};\n\
                 @define-color popover_fg_color {fg};\n\
                 @define-color card_bg_color mix({bg}, {fg}, 0.04);\n\
                 @define-color card_fg_color {fg};\n\
                 @define-color sidebar_bg_color {bg};\n\
                 @define-color sidebar_fg_color {fg};\n\
                 @define-color accent_color {accent};\n\
                 @define-color accent_bg_color {accent};\n\
                 @define-color accent_fg_color {accent_fg};\n\
                 @define-color borders alpha({fg}, 0.18);\n"
            ));
        }
    }
    let radius = appearance.corner_radius;
    let scale = appearance.text_scale as f64 / 100.0;
    let (padding, margin) = match appearance.density {
        ResultDensity::Compact => (0, 1),
        ResultDensity::Comfortable => (2, 2),
        ResultDensity::Spacious => (6, 4),
    };
    let row_radius = radius.saturating_sub(6).min(12);
    let shadow = if appearance.show_shadow { "0 3px 12px alpha(black, 0.20)" } else { "none" };
    css.push_str(&format!(
        "window.spotty-window, window.spotty-window > *, .spotty-outer {{ border-radius: {radius}px; }}\n\
         window.spotty-window {{ font-size: {scale}em; }}\n\
         .spotty-outer {{ box-shadow: {shadow}; }}\n\
         .spotty-list row {{ padding-top: {padding}px; padding-bottom: {padding}px;\
             margin-top: {margin}px; margin-bottom: {margin}px; border-radius: {row_radius}px; }}\n"
    ));
    if appearance.show_shadow {
        // Leave room inside the toplevel surface for the shadow to paint.
        css.push_str("window.spotty-window { background: transparent; }\n\
            .spotty-outer { margin: 12px; }\n");
    }
    if appearance.background_style != BackgroundStyle::Solid && !high_contrast {
        let alpha = appearance.background_opacity as f64 / 100.0;
        css.push_str(&format!(
            "window.spotty-window {{ background: transparent; }}\n\
             .spotty-outer {{ background-color: alpha(@window_bg_color, {alpha}); }}\n\
             window.spotty-window .preview-pane {{ background-color: alpha(@view_bg_color, 0.08); }}\n"
        ));
        if appearance.background_style == BackgroundStyle::Frosted {
            // GTK has no backdrop-blur operation. Keep the surface translucent
            // for the desktop's application-blur effect; tint only the plate,
            // never the text or icons. Settings explains the desktop support.
            css.push_str(
                ".spotty-outer { background-image: linear-gradient(135deg,\
                     alpha(@window_fg_color, 0.04), alpha(@window_bg_color, 0.02)); }\n"
            );
        }
    }
    let width = if high_contrast { appearance.border_width.max(2) }
        else if appearance.show_border { appearance.border_width } else { 0 };
    let style = if high_contrast { "solid" } else { match appearance.border_style {
        BorderStyle::Solid => "solid", BorderStyle::Dashed => "dashed", BorderStyle::Dotted => "dotted",
    } };
    let color = if high_contrast { "@window_fg_color" } else { match appearance.border_color_mode {
        BorderColorMode::Theme if appearance.background_style == BackgroundStyle::Frosted => "alpha(@window_fg_color, 0.22)",
        BorderColorMode::Theme => "@borders",
        BorderColorMode::Accent => "@accent_color",
        BorderColorMode::Custom => &appearance.border_color,
    } };
    let opacity = if high_contrast { 1.0 } else { appearance.border_opacity as f64 / 100.0 };
    css.push_str(&format!(".spotty-outer {{ border: {width}px {style} alpha({color}, {opacity}); }}\n"));
    if appearance.reduced_motion {
        css.push_str("* { animation: none; transition: none; }\n");
    }
    PROVIDER.with(|provider| provider.load_from_string(&css));
}
