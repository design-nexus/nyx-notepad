use crate::widgets::{self, Page};
use crate::{editor, prefs, theme};
use gtk::glib;
use gtk::prelude::*;

/// Apply an editor preference live.
fn set(change: impl FnOnce(&mut prefs::Prefs)) {
    prefs::update(change);
    editor::apply_prefs();
}

pub fn build(page: &Page) {
    let p = prefs::get();

    // ----- Editor -----
    let g = page.group("Editor");
    // The app font first, then every installed family, searchable by typing.
    let mut families: Vec<String> = page
        .body
        .pango_context()
        .font_map()
        .map(|m| m.list_families().iter().map(|f| f.name().to_string()).collect())
        .unwrap_or_default();
    families.sort_by_key(|f| f.to_lowercase());
    families.dedup();
    let mut options = vec![(String::new(), "App font (JetBrains Mono)".to_string())];
    options.extend(families.into_iter().map(|f| (f.clone(), f)));
    if !options.iter().any(|(id, _)| *id == p.font_family) {
        options.push((p.font_family.clone(), p.font_family.clone()));
    }
    let font = widgets::dropdown(&options, &p.font_family);
    font.set_enable_search(true);
    font.set_expression(Some(gtk::PropertyExpression::new(gtk::StringObject::static_type(), gtk::Expression::NONE, "string")));
    font.set_search_match_mode(gtk::StringFilterMatchMode::Substring);
    font.set_width_request(220);
    font.connect_selected_notify(move |d| {
        if let Some((id, _)) = options.get(d.selected() as usize) {
            let id = id.clone();
            set(|p| p.font_family = id);
        }
    });
    let font_box = font;
    g.add(&widgets::row("Font", "Any installed font. Monospaced fonts keep columns lined up.", Some(font_box.upcast_ref())));

    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 6.0, 36.0, 1.0);
    scale.set_value(p.font_size);
    scale.set_draw_value(false);
    scale.set_width_request(180);
    let readout = widgets::label(&format!("{} pt", p.font_size), "value-readout");
    readout.set_width_chars(6);
    readout.set_xalign(1.0);
    let r = readout.clone();
    scale.connect_value_changed(move |s| {
        let v = s.value().round();
        r.set_text(&format!("{v} pt"));
        if (prefs::get().font_size - v).abs() > f64::EPSILON {
            set(|p| p.font_size = v);
        }
    });
    editor::register_size_scale(&scale);
    let size_box = widgets::hbox(8);
    size_box.append(&scale);
    size_box.append(&readout);
    g.add(&widgets::row("Size", "Also Ctrl + and Ctrl − while typing; Ctrl 0 goes back to 12 pt.", Some(size_box.upcast_ref())));

    g.add(&widgets::segmented_row(
        "Line spacing",
        "Room between lines.",
        widgets::opts(&[("1.0", "1.0"), ("1.2", "1.2"), ("1.5", "1.5"), ("2.0", "2.0")]),
        &format!("{:.1}", p.line_spacing),
        |v| set(|p| p.line_spacing = v.parse().unwrap_or(1.2)),
    ));
    let (r, _) =
        widgets::switch_row("Word wrap", "Wrap long lines at the edge instead of scrolling sideways.", p.word_wrap, |on| {
            set(|p| p.word_wrap = on)
        });
    g.add(&r);
    g.add(&widgets::segmented_row(
        "Text width",
        "Keep wrapped text in a centered column that's easier to read on a wide window.",
        widgets::opts(&[("full", "Full"), ("80", "80"), ("100", "100")]),
        &p.text_width,
        |v| set(|p| p.text_width = v),
    ));
    g.add(&widgets::segmented_row(
        "Tab width",
        "How many columns a tab spans.",
        widgets::opts(&[("2", "2"), ("4", "4"), ("8", "8")]),
        &p.tab_width.to_string(),
        |v| set(|p| p.tab_width = v.parse().unwrap_or(4)),
    ));
    let (r, _) = widgets::switch_row("Insert spaces", "Tab types spaces up to the next tab stop.", p.insert_spaces, |on| {
        set(|p| p.insert_spaces = on)
    });
    g.add(&r);
    let (r, _) = widgets::switch_row("Line numbers", "Number each line down the left edge.", p.line_numbers, |on| {
        set(|p| p.line_numbers = on)
    });
    g.add(&r);
    let (r, _) =
        widgets::switch_row("Highlight current line", "A faint band behind the line you're on.", p.highlight_line, |on| {
            set(|p| p.highlight_line = on)
        });
    g.add(&r);

    // ----- Files -----
    let g = page.group("Files");
    let (r, _) = widgets::switch_row(
        "Reopen files",
        "Open last time's files again on start. Unsaved text always comes back.",
        p.restore_tabs,
        |on| prefs::update(|p| p.restore_tabs = on),
    );
    g.add(&r);
    let (r, _) =
        widgets::switch_row("Always show tabs", "Show the tab strip even with one file open.", p.always_show_tabs, |on| {
            prefs::update(|p| p.always_show_tabs = on);
            crate::doc::emit(crate::doc::Change::Tabs);
        });
    g.add(&r);
    g.add(&widgets::segmented_row(
        "Line endings",
        "For new files. Opened files keep the endings they have.",
        widgets::opts(&[("lf", "LF"), ("crlf", "CRLF")]),
        &p.line_ending,
        |v| prefs::update(|p| p.line_ending = v),
    ));
    let (r, _) = widgets::switch_row(
        "Trim trailing spaces",
        "Remove spaces and tabs at the ends of lines when saving.",
        p.trim_trailing,
        |on| prefs::update(|p| p.trim_trailing = on),
    );
    g.add(&r);

    // ----- Notepad window -----
    let g = page.group("Notepad window");
    let app_themes = theme::all();
    let options: Vec<(String, String)> = app_themes.iter().map(|t| (t.id.clone(), t.name.clone())).collect();
    let (theme_row, theme_dd) = widgets::choice_row(
        "Theme",
        "Dracula, Catppuccin, Tokyo Night, One Dark Pro and more. Add your own in <tt>~/.config/nyx-notepad/themes</tt>.",
        options,
        &p.theme,
        |id| {
            prefs::update(|p| {
                p.theme = id;
                p.mode = prefs::ThemeMode::Theme;
            });
            theme::apply();
        },
    );
    theme_dd.set_sensitive(p.mode == prefs::ThemeMode::Theme || !theme::omarchy_available());

    if theme::omarchy_available() {
        let dd = theme_dd.clone();
        let (r, _) = widgets::switch_row(
            "Follow Omarchy theme",
            "Match the desktop's colors and update live whenever the Omarchy theme changes.",
            p.mode == prefs::ThemeMode::Omarchy,
            move |on| {
                prefs::update(|p| p.mode = if on { prefs::ThemeMode::Omarchy } else { prefs::ThemeMode::Theme });
                dd.set_sensitive(!on);
                theme::apply();
            },
        );
        g.add(&r);
    }
    g.add(&theme_row);

    let swatches = widgets::hbox(4);
    let refresh_swatches = {
        let swatches = swatches.clone();
        move || {
            while let Some(c) = swatches.first_child() {
                swatches.remove(&c);
            }
            let pal = theme::palette();
            for c in [&pal.bg, &pal.surface, &pal.muted, &pal.text, &pal.accent, &pal.danger] {
                let s = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                s.add_css_class("swatch");
                let provider = gtk::CssProvider::new();
                provider.load_from_string(&format!("box {{ background: {c}; }}"));
                #[allow(deprecated)]
                s.style_context().add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_USER);
                swatches.append(&s);
            }
        }
    };
    refresh_swatches();
    theme::subscribe(&swatches, refresh_swatches);
    g.add(&widgets::row("Current colors", "", Some(swatches.upcast_ref())));

    g.add(&widgets::accent_row("notepad"));
    let (r, _) =
        widgets::switch_row("Reduce motion", "Turn off transitions and animations in this window.", p.reduce_motion, |on| {
            prefs::update(|p| p.reduce_motion = on);
            theme::apply();
        });
    g.add(&r);

    // ----- Keyboard -----
    let g = page.group("Keyboard");
    for (keys, what) in crate::window::SHORTCUTS {
        g.add(&widgets::row(what, "", Some(widgets::key_caps(keys).upcast_ref())));
    }
    g.note(&format!("Open files from a terminal or a binding with <tt>{}</tt>.", glib::markup_escape_text("notepad FILE…")));
}
