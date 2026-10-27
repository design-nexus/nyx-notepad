//! Theming: the shared Nyx theme engine, given this app's stylesheet.

pub use nyx_theme::*;

use crate::prefs;

/// Install the stylesheet and the current palette, and follow Omarchy live.
pub fn install() {
    nyx_theme::install(Style {
        window_class: "notepad-window",
        base_skip: &[
            ".toast",
            ".status-key",
            "window.@APP@ button.toast-action",
        ],
        css: include_str!("style.css"),
        no_glow_css: "",
        extra_colors: None,
        prefs: || {
            let p = prefs::get();
            ThemePrefs { mode: p.mode, theme: p.theme, glow: p.glow, reduce_motion: p.reduce_motion }
        },
        set_theme: Some(|mode, theme| {
            prefs::update(|p| {
                p.mode = mode;
                p.theme = theme;
            })
        }),
    });
}
