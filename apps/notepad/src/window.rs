//! The main window: one bar on top (the app, its tabs and +; find and replace while
//! finding, the file menu and settings at the end), the editor and a quiet readout
//! at its foot. Settings opens as a card over the window (see `settings_dialog`).

use crate::doc::{self, Change};
use crate::{editor, paths, prefs, session, settings_dialog, theme, widgets};
use gtk::prelude::*;
use nyx_ui::chrome;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct Ui {
    window: gtk::ApplicationWindow,
    overlay: gtk::Overlay,
    crumb: gtk::Label,
    /// The tabs and the new-tab button, hidden with one tab when the setting says so.
    tab_strip: gtk::Box,
    tabs: gtk::Box,
    readout: gtk::Label,
    /// Find and replace, shown while finding in place of the find button.
    find_bar: gtk::Box,
    find_button: gtk::Button,
    find: gtk::SearchEntry,
    find_count: gtk::Label,
    replace: gtk::Box,
    replace_entry: gtk::Entry,
    /// The toast showing now, replaced by the next.
    toast: Option<gtk::Box>,
}

thread_local! {
    static UI: RefCell<Option<Rc<RefCell<Ui>>>> = const { RefCell::new(None) };
    static NARROW: Cell<bool> = const { Cell::new(false) };
}

fn ui() -> Option<Rc<RefCell<Ui>>> {
    UI.with(|u| u.borrow().clone())
}

pub fn present(app: &gtk::Application, section: Option<&str>) {
    if let Some(ui) = ui() {
        let window = ui.borrow().window.clone();
        window.present();
        if section == Some("settings") {
            settings_dialog::open();
        }
        return;
    }
    theme::install();
    build(app);
    if session::restore() == 0 {
        doc::new_tab();
    }
    doc::watch();
    if section == Some("settings") {
        settings_dialog::open();
    }
    // Developer aid: NOTEPAD_SNAPSHOT=/path.png renders the window to a PNG
    // (invisibly) and quits, so layouts can be checked without a visible window.
    if let Some(out) = std::env::var_os("NOTEPAD_SNAPSHOT") {
        snapshot_and_quit(app, std::path::PathBuf::from(out));
        return;
    }
    if let Some(ui) = ui() {
        ui.borrow().window.present();
    }
    editor::focus();
    if prefs::take_broken() {
        toast("Your settings file couldn't be read, so defaults are in use. The old file is kept as settings.toml.bak.");
    }
}

fn build(app: &gtk::Application) {
    let window =
        gtk::ApplicationWindow::builder().application(app).title("Notepad").default_width(900).default_height(700).build();
    window.add_css_class("notepad-window");
    // No client-side titlebar: Hyprland manages the window.
    window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    nyx_ui::keys::install(&window, nyx_ui::keys::Common { shortcuts: Some(show_shortcuts), ..Default::default() });
    window.set_icon_name(Some(crate::APP_ID));

    let top = find_strip();
    let (tab_bar, tabs) = tab_bar();
    let editor = editor::build();
    let (foot, readout) = foot();

    // The one bar: the app and its tabs; find while finding, the file menu and Settings.
    let header = chrome::app_header(&window, "text-x-generic-symbolic", "Notepad");
    header.start.append(&tab_bar);
    let find_button = widgets::bar_button("system-search-symbolic", "Find (Ctrl+F) · Replace (Ctrl+H)");
    find_button.connect_clicked(|_| toggle_find(false));
    header.end.append(&top.bar);
    header.end.append(&find_button);
    header.end.append(&file_menu());
    header.end.append(&chrome::settings_button(settings_dialog::open));

    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("window-frame");
    frame.append(&header.root);
    frame.append(&editor);
    frame.append(&foot);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&frame));
    window.set_child(Some(&overlay));

    install_keys(&window);
    window.connect_close_request(|_| {
        session::save_now();
        glib::Propagation::Proceed
    });

    // Drop files anywhere on the window to open them.
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    drop.connect_drop(|_, value, _, _| {
        let Ok(list) = value.get::<gdk::FileList>() else { return false };
        for f in list.files() {
            if let Some(p) = f.path() {
                doc::open(&p);
            }
        }
        true
    });
    window.add_controller(drop);

    // Narrow windows (a tiled half-screen) get slimmer padding.
    let apply_width = |w: &gtk::ApplicationWindow| {
        let narrow = w.width() > 0 && w.width() < 980;
        if narrow != NARROW.with(|n| n.replace(narrow)) {
            editor::apply_prefs();
        }
        settings_dialog::fit(w);
    };
    // Tiled windows are resized by the compositor without touching the default
    // size: an invisible layer over the whole window reports each real size
    // change, and the layout follows on the next frame.
    let probe = gtk::DrawingArea::new();
    probe.set_can_target(false);
    probe.set_can_focus(false);
    overlay.add_overlay(&probe);
    overlay.set_measure_overlay(&probe, false);
    {
        let (aw, w2) = (apply_width, window.clone());
        probe.connect_resize(move |_, _, _| {
            let (aw, w2) = (aw, w2.clone());
            // After this layout pass, when the window's width is the new one.
            glib::idle_add_local_once(move || aw(&w2));
        });
    }
    // A slow fallback, in case a resize slips by.
    let w2 = window.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
        apply_width(&w2);
        glib::ControlFlow::Continue
    });

    let ui = Ui {
        window: window.clone(),
        overlay,
        crumb: header.subtitle.clone(),
        tab_strip: tab_bar,
        tabs,
        readout,
        find_bar: top.bar,
        find_button,
        find: top.find,
        find_count: top.find_count,
        replace: top.replace,
        replace_entry: top.replace_entry,
        toast: None,
    };
    UI.with(|u| *u.borrow_mut() = Some(Rc::new(RefCell::new(ui))));

    doc::subscribe(&window, |c| match c {
        Change::Tabs => refresh_tabs(),
        Change::Current => {
            refresh_tabs();
            refresh_title();
        }
        Change::Dirty => refresh_title(),
    });
}

/// Find and replace's parts.
struct TopBar {
    bar: gtk::Box,
    find: gtk::SearchEntry,
    find_count: gtk::Label,
    replace: gtk::Box,
    replace_entry: gtk::Entry,
}

/// Find and replace, in the bar while finding: the find field and its match
/// count, and the replace field after Ctrl+H.
fn find_strip() -> TopBar {
    let find = gtk::SearchEntry::new();
    find.set_placeholder_text(Some("Find"));
    find.add_css_class("bar-search");
    find.set_width_chars(20);
    find.set_search_delay(120);
    let find_count = widgets::label("", "find-count");
    find_count.set_visible(false);

    let replace = widgets::hbox(4);
    replace.set_visible(false);
    let replace_entry = gtk::Entry::new();
    replace_entry.set_placeholder_text(Some("Replace with"));
    replace_entry.add_css_class("bar-search");
    replace_entry.set_width_chars(14);
    let one = widgets::bar_button("edit-find-replace-symbolic", "Replace this match (Enter)");
    let all = widgets::bar_button("edit-select-all-symbolic", "Replace every match (Ctrl+Enter)");
    replace.append(&replace_entry);
    replace.append(&one);
    replace.append(&all);

    let bar = widgets::hbox(4);
    bar.set_visible(false);
    bar.append(&find);
    bar.append(&find_count);
    bar.append(&replace);

    find.connect_search_changed(|e| editor::set_query(&e.text()));
    find.connect_activate(|_| editor::find_next(true));
    find.connect_stop_search(|_| close_find());
    // Shift+Enter goes back a match.
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(|_, key, _, mods| {
        if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) && mods.contains(gdk::ModifierType::SHIFT_MASK) {
            editor::find_next(false);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    find.add_controller(keys);
    let e = replace_entry.clone();
    one.connect_clicked(move |_| editor::replace_one(&e.text()));
    let e = replace_entry.clone();
    all.connect_clicked(move |_| editor::replace_all(&e.text()));
    replace_entry.connect_activate(|e| editor::replace_one(&e.text()));
    let keys = gtk::EventControllerKey::new();
    let e = replace_entry.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) && mods.contains(gdk::ModifierType::CONTROL_MASK) {
            editor::replace_all(&e.text());
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    replace_entry.add_controller(keys);

    TopBar { bar, find, find_count, replace, replace_entry }
}

/// The file menu: new, open, save, and the files opened lately.
fn file_menu() -> gtk::MenuButton {
    let menu = gtk::MenuButton::new();
    menu.set_icon_name("document-open-symbolic");
    menu.add_css_class("bar-button");
    menu.set_valign(gtk::Align::Center);
    menu.set_tooltip_text(Some("File"));
    let popover = gtk::Popover::new();
    popover.add_css_class("menu-popover");
    popover.set_has_arrow(false);
    let list = widgets::vbox(0);
    popover.set_child(Some(&list));
    menu.set_popover(Some(&popover));

    // Built each time it opens, so the recent files are current.
    let l = list.clone();
    popover.connect_show(move |pop| {
        while let Some(c) = l.first_child() {
            l.remove(&c);
        }
        let item = |text: &str, keys: &str, act: Box<dyn Fn()>| {
            let b = gtk::Button::new();
            b.add_css_class("menu-item");
            let row = widgets::hbox(24);
            let t = widgets::label(text, "");
            t.set_hexpand(true);
            row.append(&t);
            if !keys.is_empty() {
                row.append(&widgets::label(keys, "dim"));
            }
            b.set_child(Some(&row));
            let pop = pop.clone();
            b.connect_clicked(move |_| {
                pop.popdown();
                act();
            });
            b
        };
        l.append(&item("New tab", "Ctrl+T", Box::new(doc::new_tab)));
        l.append(&item("Open…", "Ctrl+O", Box::new(doc::open_dialog)));
        l.append(&item("Save", "Ctrl+S", Box::new(doc::save)));
        l.append(&item("Save as…", "Ctrl+Shift+S", Box::new(doc::save_as)));
        l.append(&item("Reopen closed tab", "Ctrl+Shift+T", Box::new(doc::reopen_closed)));
        let recent = session::recent();
        if !recent.is_empty() {
            l.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let h = widgets::label("RECENT", "menu-heading");
            l.append(&h);
            for p in recent.into_iter().take(8) {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let tip = paths::pretty(&p);
                let b = item(&name, "", Box::new(move || doc::open(&p)));
                b.set_tooltip_text(Some(&tip));
                l.append(&b);
            }
            l.append(&item("Clear recent files", "", Box::new(session::clear_recent)));
        }
    });
    menu
}

/// The tabs in the bar, with a + at their end (hidden with one tab unless the
/// setting says otherwise; the bar then names the file).
fn tab_bar() -> (gtk::Box, gtk::Box) {
    let strip = widgets::hbox(2);
    strip.add_css_class("tab-bar");
    let tabs = widgets::hbox(2);
    tabs.add_css_class("tabs");
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .child(&tabs)
        .build();
    // A vertical wheel moves along the tabs.
    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    let s = scroll.clone();
    wheel.connect_scroll(move |_, _, dy| {
        let adj = s.hadjustment();
        adj.set_value(adj.value() + dy * 40.0);
        glib::Propagation::Stop
    });
    scroll.add_controller(wheel);
    strip.append(&scroll);
    let add = widgets::bar_button("list-add-symbolic", "New tab (Ctrl+T)");
    add.connect_clicked(|_| doc::new_tab());
    strip.append(&add);
    (strip, tabs)
}

/// One tab per open file: its name, a dot when it has unsaved changes, and ✕.
fn refresh_tabs() {
    let Some(ui) = ui() else { return };
    let (bar, tabs) = {
        let u = ui.borrow();
        (u.tab_strip.clone(), u.tabs.clone())
    };
    while let Some(c) = tabs.first_child() {
        tabs.remove(&c);
    }
    let docs = doc::all();
    let current = doc::current_index();
    for (i, d) in docs.iter().enumerate() {
        let tab = widgets::hbox(0);
        tab.add_css_class("doc-tab");
        if i == current {
            tab.add_css_class("active");
        }
        let main = gtk::Button::new();
        main.add_css_class("tab-main");
        let content = widgets::hbox(6);
        let name = widgets::label(&d.title(), "tab-name");
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.set_max_width_chars(24);
        content.append(&name);
        if d.dirty() {
            content.append(&widgets::label("•", "tab-dirty"));
        }
        main.set_child(Some(&content));
        main.set_tooltip_text(Some(&match d.path() {
            Some(p) => paths::pretty(&p),
            None => "Not saved yet".to_string(),
        }));
        main.connect_clicked(move |_| doc::switch_to(i));
        // A middle click closes the tab.
        let middle = gtk::GestureClick::builder().button(2).build();
        let dd = d.clone();
        middle.connect_released(move |_, _, _, _| doc::close(&dd));
        tab.add_controller(middle);
        tab.append(&main);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("tab-close");
        close.set_valign(gtk::Align::Center);
        close.set_tooltip_text(Some("Close the tab (Ctrl+W)"));
        let dd = d.clone();
        close.connect_clicked(move |_| doc::close(&dd));
        tab.append(&close);
        tabs.append(&tab);
    }
    let show_tabs = prefs::get().always_show_tabs || docs.len() > 1;
    bar.set_visible(show_tabs);
    ui.borrow().crumb.set_visible(!show_tabs);
}

/// The header's subtitle, window title and readout follow the current tab.
fn refresh_title() {
    let Some(ui) = ui() else { return };
    let Some(d) = doc::current() else { return };
    let u = ui.borrow();
    let name = d.title();
    let dot = if d.dirty() { " •" } else { "" };
    u.crumb.set_text(&format!("{name}{dot}"));
    u.window.set_title(Some(&format!("{}{name} — Notepad", if d.dirty() { "• " } else { "" })));
    drop(u);
    refresh_status();
}

/// Re-read the cursor position and counts into the status bar.
pub fn refresh_status() {
    let Some(ui) = ui() else { return };
    let readout = ui.borrow().readout.clone();
    readout.set_text(&editor::status());
    refresh_find_count();
}

pub fn refresh_find_count() {
    let Some(ui) = ui() else { return };
    let label = ui.borrow().find_count.clone();
    if find_open() {
        let text = editor::find_count();
        label.set_visible(!text.is_empty());
        label.set_text(&text);
    }
}

/// Ctrl+F opens find; Ctrl+H opens find and replace. Either focuses the find field.
fn toggle_find(replace: bool) {
    let Some(ui) = ui() else { return };
    let (bar, button, find, rep, rep_entry) = {
        let u = ui.borrow();
        (u.find_bar.clone(), u.find_button.clone(), u.find.clone(), u.replace.clone(), u.replace_entry.clone())
    };
    let opening = !bar.is_visible();
    bar.set_visible(true);
    button.set_visible(false);
    if replace {
        rep.set_visible(true);
    }
    // Start from the selected text, if any.
    if let Some(d) = doc::current()
        && let Some((s, e)) = d.buffer.selection_bounds()
        && s.line() == e.line()
    {
        find.set_text(&d.buffer.text(&s, &e, false));
    }
    if replace && !opening && find.has_focus() {
        rep_entry.grab_focus();
    } else {
        find.grab_focus();
        find.select_region(0, -1);
    }
    if !find.text().is_empty() {
        editor::set_query(&find.text());
    }
}

fn close_find() {
    let Some(ui) = ui() else { return };
    let u = ui.borrow();
    u.find.set_text("");
    u.find_bar.set_visible(false);
    u.find_button.set_visible(true);
    u.find_count.set_visible(false);
    u.replace.set_visible(false);
    drop(u);
    editor::set_query("");
    editor::focus();
}

fn find_open() -> bool {
    ui().is_some_and(|u| u.borrow().find_bar.is_visible())
}

/// The editor's foot: the cursor position and counts as quiet text, no bar.
fn foot() -> (gtk::Box, gtk::Label) {
    let foot = widgets::hbox(0);
    foot.add_css_class("doc-foot");
    let readout = widgets::label("", "status-readout");
    readout.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    readout.set_hexpand(true);
    readout.set_xalign(1.0);
    foot.append(&readout);
    (foot, readout)
}

/// Every keyboard shortcut, for the shortcuts dialog and Settings.
pub const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["Ctrl", "T"], "New tab (also Ctrl+N)"),
    (&["Ctrl", "O"], "Open files"),
    (&["Ctrl", "S"], "Save"),
    (&["Ctrl", "Shift", "S"], "Save as…"),
    (&["Ctrl", "W"], "Close the tab"),
    (&["Ctrl", "Shift", "T"], "Reopen the tab closed last"),
    (&["Ctrl", "Tab"], "Next tab (with Shift, the one before)"),
    (&["Ctrl", "PgDn"], "Next tab (PgUp: the one before)"),
    (&["Alt", "1…9"], "Go to tab 1 to 9"),
    (&["Ctrl", "F"], "Find"),
    (&["Ctrl", "H"], "Find and replace"),
    (&["Enter"], "Next match (with Shift, the one before)"),
    (&["Ctrl", "Z"], "Undo"),
    (&["Ctrl", "Shift", "Z"], "Redo"),
    (&["Ctrl", "+"], "Bigger text"),
    (&["Ctrl", "−"], "Smaller text"),
    (&["Ctrl", "0"], "Default text size"),
    (&["Ctrl", ","], "Settings"),
    (&["Esc"], "Close find, or the settings"),
    (&["F1"], "Show these shortcuts (or ?)"),
    (&["Ctrl", "Q"], "Quit (unsaved text is kept for next time)"),
];

pub fn show_shortcuts() {
    let (dialog, card) = widgets::dialog("Keyboard shortcuts", 520);
    let list = widgets::vbox(0);
    list.add_css_class("group-list");
    for (keys, what) in SHORTCUTS {
        list.append(&widgets::row(what, "", Some(widgets::key_caps(keys).upcast_ref())));
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Scrolls without a scrollbar, which would cover the key caps.
        .vscrollbar_policy(gtk::PolicyType::External)
        .propagate_natural_height(true)
        .max_content_height(600)
        .child(&list)
        .build();
    card.append(&scroll);
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    let d = dialog.clone();
    close.connect_clicked(move |_| d.close());
    card.append(&close);
    dialog.present();
}

fn install_keys(window: &gtk::ApplicationWindow) {
    // Capture phase: these work wherever focus is, ahead of the text view's own keys.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w2 = window.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
        let alt = mods.contains(gdk::ModifierType::ALT_MASK);
        let stop = glib::Propagation::Stop;
        if settings_dialog::is_open() {
            return match key {
                gdk::Key::Escape => {
                    settings_dialog::escape();
                    stop
                }
                gdk::Key::f if ctrl => {
                    settings_dialog::focus_search();
                    stop
                }
                gdk::Key::q if ctrl => {
                    w2.close();
                    stop
                }
                _ => glib::Propagation::Proceed,
            };
        }
        let k = key.to_lower();
        if ctrl {
            return match k {
                gdk::Key::f => {
                    toggle_find(false);
                    stop
                }
                gdk::Key::h => {
                    toggle_find(true);
                    stop
                }
                gdk::Key::o => {
                    doc::open_dialog();
                    stop
                }
                gdk::Key::s if shift => {
                    doc::save_as();
                    stop
                }
                gdk::Key::s => {
                    doc::save();
                    stop
                }
                gdk::Key::t if shift => {
                    doc::reopen_closed();
                    stop
                }
                gdk::Key::t | gdk::Key::n => {
                    doc::new_tab();
                    stop
                }
                gdk::Key::w => {
                    doc::close_current();
                    stop
                }
                gdk::Key::q => {
                    w2.close();
                    stop
                }
                gdk::Key::comma => {
                    settings_dialog::open();
                    stop
                }
                gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
                    doc::cycle(if shift || key == gdk::Key::ISO_Left_Tab { -1 } else { 1 });
                    stop
                }
                gdk::Key::Page_Down => {
                    doc::cycle(1);
                    stop
                }
                gdk::Key::Page_Up => {
                    doc::cycle(-1);
                    stop
                }
                gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => {
                    editor::zoom(1);
                    stop
                }
                gdk::Key::minus | gdk::Key::KP_Subtract => {
                    editor::zoom(-1);
                    stop
                }
                gdk::Key::_0 | gdk::Key::KP_0 => {
                    editor::zoom(0);
                    stop
                }
                _ => glib::Propagation::Proceed,
            };
        }
        if alt && let Some(n) = key.to_unicode().and_then(|c| c.to_digit(10)).filter(|n| (1..=9).contains(n)) {
            doc::switch_to(n as usize - 1);
            return stop;
        }
        match key {
            gdk::Key::F1 => show_shortcuts(),
            gdk::Key::Escape if find_open() => close_find(),
            _ => return glib::Propagation::Proceed,
        }
        stop
    });
    window.add_controller(keys);
}

/// The layer over the window, for toasts and the settings card.
pub fn overlay() -> Option<gtk::Overlay> {
    ui().map(|u| u.borrow().overlay.clone())
}

pub fn narrow() -> bool {
    NARROW.with(|n| n.get())
}

/// Show a short message at the bottom of the window.
pub fn toast(message: &str) {
    show_toast(message, None);
}

/// A toast with a button (Undo); it stays a little longer.
pub fn toast_with_action(message: &str, label: &str, act: impl Fn() + 'static) {
    show_toast(message, Some((label, Box::new(act))));
}

/// A toast's button: its label and what it does.
type ToastAction<'a> = Option<(&'a str, Box<dyn Fn()>)>;

fn show_toast(message: &str, action: ToastAction<'_>) {
    let Some(ui) = ui() else {
        eprintln!("nyx-notepad: {message}");
        return;
    };
    let overlay = ui.borrow().overlay.clone();
    if let Some(old) = ui.borrow_mut().toast.take() {
        overlay.remove_overlay(&old);
    }
    let label = gtk::Label::new(Some(message));
    label.set_wrap(true);
    label.set_max_width_chars(70);
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bx.add_css_class("toast");
    bx.append(&label);
    bx.set_halign(gtk::Align::Center);
    bx.set_valign(gtk::Align::End);
    let secs = if action.is_some() { 6000 } else { 3500 };
    if let Some((text, act)) = action {
        let b = gtk::Button::with_label(text);
        b.add_css_class("toast-action");
        b.set_valign(gtk::Align::Center);
        let (o, t) = (overlay.clone(), bx.clone());
        b.connect_clicked(move |_| {
            o.remove_overlay(&t);
            act();
        });
        bx.append(&b);
    }
    overlay.add_overlay(&bx);
    ui.borrow_mut().toast = Some(bx.clone());
    glib::timeout_add_local_once(std::time::Duration::from_millis(secs), move || {
        if bx.parent().is_some() {
            overlay.remove_overlay(&bx);
        }
    });
}

pub fn window() -> Option<gtk::ApplicationWindow> {
    ui().map(|u| u.borrow().window.clone())
}

fn snapshot_and_quit(app: &gtk::Application, out: std::path::PathBuf) {
    let Some(ui) = ui() else { return };
    let window = ui.borrow().window.clone();
    window.set_opacity(0.01);
    // A distinct title lets a window rule float it at a set size for screenshots.
    window.set_title(Some("Notepad snapshot"));
    window.set_default_size(
        std::env::var("NOTEPAD_SNAPSHOT_W").ok().and_then(|v| v.parse().ok()).unwrap_or(900),
        std::env::var("NOTEPAD_SNAPSHOT_H").ok().and_then(|v| v.parse().ok()).unwrap_or(700),
    );
    window.present();
    let app = app.clone();
    let delay = std::env::var("NOTEPAD_SNAPSHOT_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
    glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
        if let Some(child) = window.child() {
            let paintable = gtk::WidgetPaintable::new(Some(&child));
            let (w, h) = (child.width(), child.height());
            let snapshot = gtk::Snapshot::new();
            snapshot.append_color(&gdk::RGBA::BLACK, &gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32));
            paintable.snapshot(&snapshot, w as f64, h as f64);
            if let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) {
                let texture = renderer.render_texture(node, None);
                match texture.save_to_png(&out) {
                    Ok(()) => println!("snapshot {w}x{h} -> {}", out.display()),
                    Err(e) => eprintln!("snapshot failed: {e}"),
                }
            }
        }
        app.quit();
    });
}
