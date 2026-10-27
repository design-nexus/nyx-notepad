//! Notepad — a plain-text notepad for Omarchy.

mod cmd;
mod control;
mod doc;
mod editor;
mod paths;
mod prefs;
mod sections;
mod session;
mod settings_dialog;
mod theme;
mod widgets;
mod window;

use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::PathBuf;

pub const APP_ID: &str = "io.github.design_nexus.nyx.Notepad";

const USAGE: &str = "Usage: nyx-notepad [OPTIONS] [FILE…]\n\
\n\
  FILE            open this file in a tab (a new name is created on the first save)\n\
  --section ID    open the settings card with: --section settings\n\
  --toggle        close the window if it's open, otherwise open it (for a keybinding)\n";

fn main() -> glib::ExitCode {
    nyx_core::app::init(nyx_core::app::App { slug: "nyx-notepad", name: "Notepad" });
    nyx_core::migrate::from(&["nexus-notepad"]);
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return glib::ExitCode::SUCCESS;
    }

    // GTK's Vulkan renderer enumerates every GPU at startup, which wakes a
    // sleeping discrete GPU on hybrid laptops. GL renders only on the one in use.
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: still single-threaded; nothing else reads the environment yet.
        unsafe { std::env::set_var("GSK_RENDERER", "ngl") };
    }

    // Developer aid: a different id gives a separate instance, so a test run doesn't hand
    // its arguments to the copy that's open.
    let id = std::env::var("NOTEPAD_APP_ID").unwrap_or_else(|_| APP_ID.to_string());
    let app = gtk::Application::builder().application_id(id).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
    // Nyx Assistant works the open tabs through this (only the primary instance serves it).
    app.connect_startup(|app| nyx_core::control::serve(app, control::tools(), control::handle));
    app.connect_command_line(|app, cl| {
        let argv: Vec<String> = cl.arguments().iter().map(|a| a.to_string_lossy().to_string()).collect();
        let has = |flag: &str| argv.iter().any(|a| a == flag);
        let value = |flag: &str| argv.iter().position(|a| a == flag).and_then(|i| argv.get(i + 1)).cloned();
        let section = value("--section");
        let mut files: Vec<PathBuf> = Vec::new();
        let mut skip = true; // argv[0]
        for a in &argv {
            if std::mem::take(&mut skip) {
                continue;
            }
            if a == "--section" {
                skip = true;
                continue;
            }
            if a.starts_with("--") {
                continue;
            }
            // Relative paths resolve against the caller's directory, and URIs work too.
            if let Some(p) = cl.create_file_for_arg(a).path() {
                files.push(p);
            }
        }

        if has("--toggle")
            && let Some(w) = window::window()
            && w.is_visible()
        {
            w.close();
            return glib::ExitCode::SUCCESS;
        }
        window::present(app, section.as_deref());
        for path in files {
            doc::open(&path);
        }
        glib::ExitCode::SUCCESS
    });
    app.connect_shutdown(|_| {
        prefs::flush();
        session::save_now();
    });
    app.run()
}
