//! Settings as a card over the window: the groups listed on the left, the
//! chosen group's options on the right, and a search across all of them.

use crate::sections::settings;
use crate::{widgets, window};
use gtk::prelude::*;
use nyx_ui::settings_card::{self, About, Spec, Standard};

pub use nyx_ui::settings_card::{escape, fit, focus_search, is_open};

/// Each settings group's icon and one-line summary in the dialog's list.
const GROUPS: &[(&str, &str, &str)] = &[
    ("Editor", "format-text-bold-symbolic", "Font, size, spacing, wrapping"),
    ("Files", "folder-documents-symbolic", "Tabs, line endings, saving"),
];

const SPEC: Spec = Spec {
    groups: GROUPS,
    search_text: None,
    standard: Some(Standard {
        app: "notepad",
        about: About {
            name: "Notepad",
            icon: "io.github.design_nexus.nyx.Notepad",
            version: env!("CARGO_PKG_VERSION"),
            description: "Plain text with tabs, adjustable fonts and sessions that keep unsaved text.",
            licence: "MIT",
            website: "https://github.com/design-nexus/nyx-notepad",
            issues: "https://github.com/design-nexus/nyx-notepad/issues",
            credits: &[],
        },
        shortcuts: window::SHORTCUTS,
        reduce_motion: || crate::prefs::get().reduce_motion,
        set_reduce_motion: |on| crate::prefs::update(|p| p.reduce_motion = on),
    }),
};

pub fn open() {
    let (Some(overlay), Some(window)) = (window::overlay(), window::window()) else { return };
    settings_card::open(&overlay, &window, SPEC, || {
        let page = widgets::page("settings");
        settings::build(&page);
        (page.root.upcast(), page.body)
    });
}
