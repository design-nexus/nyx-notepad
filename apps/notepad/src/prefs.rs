//! This app's own preferences (`~/.config/nyx-notepad/settings.toml`).

pub use nyx_theme::ThemeMode;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub mode: ThemeMode,
    pub theme: String,
    pub reduce_motion: bool,
    pub glow: bool,
    /// The editor's font family; empty for the app font (JetBrains Mono).
    pub font_family: String,
    /// The editor's font size, in points.
    pub font_size: f64,
    /// Line height as a multiple of the font size: 1.0, 1.2, 1.5 or 2.0.
    pub line_spacing: f64,
    /// Long lines wrap at the window edge (otherwise they scroll sideways).
    pub word_wrap: bool,
    /// Columns per tab stop.
    pub tab_width: u32,
    /// Tab types spaces up to the next tab stop.
    pub insert_spaces: bool,
    /// The text column's width: full, 80 or 100 characters, centred.
    pub text_width: String,
    pub line_numbers: bool,
    pub highlight_line: bool,
    /// Reopen saved files from last time (unsaved text always comes back).
    pub restore_tabs: bool,
    /// Line endings for new files: lf or crlf.
    pub line_ending: String,
    pub trim_trailing: bool,
    /// Show the tab strip even with one tab open.
    pub always_show_tabs: bool,
}

pub const DEFAULT_SIZE: f64 = 12.0;

impl Default for Prefs {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Omarchy,
            theme: "tokyo-night".into(),
            reduce_motion: false,
            glow: true,
            font_family: String::new(),
            font_size: DEFAULT_SIZE,
            line_spacing: 1.2,
            word_wrap: true,
            tab_width: 4,
            insert_spaces: false,
            text_width: "full".into(),
            line_numbers: false,
            highlight_line: true,
            restore_tabs: true,
            line_ending: "lf".into(),
            trim_trailing: false,
            always_show_tabs: true,
        }
    }
}

nyx_core::prefs_store!(Prefs);
