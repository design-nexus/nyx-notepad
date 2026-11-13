//! This app's own preferences (`~/.config/nyx-notepad/settings.toml`).

pub use nyx_theme::ThemeMode;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Which [`migrate`] steps this file has had. Files from before versions read as 0.
    #[serde(default)]
    pub version: u32,
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

pub const VERSION: u32 = 1;

impl Default for Prefs {
    fn default() -> Self {
        Self {
            version: VERSION,
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
            text_width: "80".into(),
            line_numbers: false,
            highlight_line: true,
            restore_tabs: true,
            line_ending: "lf".into(),
            trim_trailing: false,
            always_show_tabs: true,
        }
    }
}

/// Bring a file written by an older version up to date; true if anything changed.
fn migrate(p: &mut Prefs) -> bool {
    if p.version >= VERSION {
        return false;
    }
    // Full width was the default before the centred column, so nobody chose it.
    if p.version < 1 && p.text_width == "full" {
        p.text_width = "80".into();
    }
    p.version = VERSION;
    true
}

nyx_core::prefs_store!(Prefs, nyx_core::prefs::Store::<Prefs>::standard_with(migrate));

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Prefs {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn old_full_width_becomes_the_centred_column_once() {
        let mut p = read("theme = \"nyx-dark\"\ntext_width = \"full\"\nline_numbers = true\n");
        assert_eq!(p.version, 0);
        assert!(migrate(&mut p));
        assert_eq!((p.version, p.text_width.as_str(), p.line_numbers), (VERSION, "80", true));

        // Picking Full afterwards sticks.
        p.text_width = "full".into();
        let mut p = read(&toml::to_string_pretty(&p).unwrap());
        assert!(!migrate(&mut p));
        assert_eq!(p.text_width, "full");
    }

    #[test]
    fn old_files_keep_a_width_that_was_chosen() {
        let mut p = read("text_width = \"100\"\n");
        assert!(migrate(&mut p));
        assert_eq!((p.version, p.text_width.as_str()), (VERSION, "100"));
    }

    #[test]
    fn new_settings_need_no_migration() {
        assert!(!migrate(&mut Prefs::default()));
    }
}
