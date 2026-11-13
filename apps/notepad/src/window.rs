//! The main window, just the page: the header holds the app, its tabs and +, then
//! the File menu, find and settings. The sheet holds the text in a centred column
//! and a foot with the cursor's place and buttons for the line endings, the indent
//! and the text size. Find and replace is a small card over the text. Settings opens
//! as a card over the window (see `settings_dialog`).

use crate::doc::{self, Change};
use crate::{editor, paths, prefs, session, settings_dialog, theme, widgets};
use gtk::prelude::*;
use gtk::{gdk, glib};
use nyx_ui::chrome;
use nyx_ui::menu::{self, Entry};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Sizes offered by the text size button.
const SIZES: &[f64] = &[10.0, 11.0, 12.0, 13.0, 14.0, 16.0, 18.0, 20.0, 24.0];

struct Ui {
    window: gtk::ApplicationWindow,
    overlay: gtk::Overlay,
    /// The app's name in the header, left out on narrow windows to leave room for tabs.
    title: Option<gtk::Widget>,
    crumb: gtk::Label,
    header: gtk::Widget,
    header_end: Option<gtk::Widget>,
    /// The tabs, "+N" for the ones that don't fit, and +; hidden with one tab when the
    /// setting says so.
    tab_strip: gtk::Box,
    tabs: gtk::Box,
    more: gtk::Button,
    add: gtk::Button,
    /// Each tab's natural width, and the tabs left out for want of room.
    tab_widths: Vec<i32>,
    more_width: i32,
    hidden: Vec<usize>,
    readout: gtk::Label,
    ending: gtk::Label,
    indent: gtk::Label,
    size: gtk::Label,
    /// Find and replace, a card over the text while finding.
    find_card: gtk::Box,
    find_button: gtk::Button,
    find: gtk::Text,
    find_count: gtk::Label,
    replace_entry: gtk::Text,
}

thread_local! {
    static UI: RefCell<Option<Rc<RefCell<Ui>>>> = const { RefCell::new(None) };
    static NARROW: Cell<bool> = const { Cell::new(false) };
    static FIND_PENDING: Cell<Option<glib::SourceId>> = const { Cell::new(None) };
    /// Opened by `NOTEPAD_SHOW`: a window without focus can't grab input for a menu.
    static SHOWING_FOR_TEST: Cell<bool> = const { Cell::new(false) };
    static TEST_POPOVER: RefCell<glib::WeakRef<gtk::Popover>> = RefCell::new(glib::WeakRef::new());
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
    // Developer aid: NOTEPAD_SHOW=menu, find=TEXT, pill=N (0 to 2) or close=N (a tab),
    // comma-separated, opens those once the window is up, for screenshots of a test copy.
    if let Ok(show) = std::env::var("NOTEPAD_SHOW") {
        glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
            for part in show.split(',') {
                show_for_test(part);
            }
        });
    }
    if prefs::take_broken() {
        toast("Your settings file couldn't be read, so defaults are in use. The old file is kept as settings.toml.bak.");
    }
}

fn show_for_test(what: &str) {
    let Some(ui) = ui() else { return };
    SHOWING_FOR_TEST.with(|t| t.set(true));
    let (what, arg) = what.split_once('=').unwrap_or((what, ""));
    let buttons = |class: &str, under: &gtk::Widget| {
        let mut found = Vec::new();
        let mut stack = vec![under.clone()];
        while let Some(w) = stack.pop() {
            if w.has_css_class(class)
                && let Ok(b) = w.clone().downcast::<gtk::Button>()
            {
                found.push(b);
            }
            let mut child = w.last_child();
            while let Some(c) = child {
                child = c.prev_sibling();
                stack.push(c);
            }
        }
        found
    };
    let window: gtk::Widget = ui.borrow().window.clone().upcast();
    match what {
        "menu" => buttons("file-button", &window).iter().for_each(|b| b.emit_clicked()),
        "pill" => {
            let n = arg.parse().unwrap_or(0);
            if let Some(b) = buttons("foot-pill", &window).get(n) {
                b.emit_clicked();
            }
        }
        "find" => {
            toggle_find(false);
            let find = ui.borrow().find.clone();
            find.set_text(arg);
            editor::set_query(arg);
        }
        "close" => {
            if let Some(d) = doc::all().get(arg.parse().unwrap_or(usize::MAX)) {
                doc::close(d);
            }
        }
        // The compositor doesn't draw a menu of a window without focus, so the window
        // and its menu are drawn into a PNG instead.
        "png" => {
            let (window, out) = (ui.borrow().window.clone(), std::path::PathBuf::from(arg));
            glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
                let Some(child) = window.child() else { return };
                let (w, h) = (child.width(), child.height());
                let snapshot = gtk::Snapshot::new();
                gtk::WidgetPaintable::new(Some(&child)).snapshot(&snapshot, w as f64, h as f64);
                if let Some(pop) = TEST_POPOVER.with(|t| t.borrow().upgrade())
                    && let Some(popup) = pop.surface().and_downcast::<gdk::Popup>()
                {
                    snapshot.save();
                    let (px, py) = pop.surface_transform();
                    let (x, y) = (popup.position_x() as f64 - px, popup.position_y() as f64 - py);
                    snapshot.translate(&gtk::graphene::Point::new(x as f32, y as f32));
                    gtk::WidgetPaintable::new(Some(&pop)).snapshot(&snapshot, pop.width() as f64, pop.height() as f64);
                    snapshot.restore();
                }
                if let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer())
                    && let Err(e) = renderer.render_texture(node, None).save_to_png(&out)
                {
                    eprintln!("snapshot failed: {e}");
                }
            });
        }
        _ => {}
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

    let card = find_card();
    let strip = tab_bar();
    let editor = editor::build();
    let foot = foot();

    // The header: the app and its tabs, then the File menu, find and Settings.
    let header = chrome::app_header(&window, "text-x-generic-symbolic", "Notepad");
    header.start.append(&strip.root);
    let find_button = widgets::bar_button("system-search-symbolic", "Find and replace (Ctrl+F)");
    find_button.connect_clicked(|_| {
        if find_open() {
            close_find();
        } else {
            toggle_find(false);
        }
    });
    header.end.append(&file_button());
    header.end.append(&find_button);
    header.end.append(&chrome::settings_button(settings_dialog::open));
    let title = header.start.parent().and_then(|left| {
        let mut child = left.first_child();
        while let Some(c) = child {
            if c.has_css_class("app-title") {
                return Some(c);
            }
            child = c.next_sibling();
        }
        None
    });

    // The sheet: the text with find's card over it, and the foot.
    let text = gtk::Overlay::new();
    text.set_child(Some(&editor));
    text.add_overlay(&card.root);
    text.set_vexpand(true);
    let sheet = widgets::vbox(0);
    sheet.append(&text);
    sheet.append(&foot.root);

    let frame = nyx_ui::frame::AppFrame::new(&header.root);
    frame.set_page(&sheet);
    frame.set_flush(true);
    let overlay = frame.install(&window);

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

    // Narrow windows (a tiled half-screen) get slimmer padding, and the header leaves
    // out the app's name; tabs that don't fit go behind "+N".
    let apply_width = |w: &gtk::ApplicationWindow| {
        let narrow = w.width() > 0 && w.width() < 980;
        if narrow != NARROW.with(|n| n.replace(narrow)) {
            if let Some(t) = ui().and_then(|u| u.borrow().title.clone()) {
                t.set_visible(!narrow);
            }
            editor::apply_prefs();
            glib::idle_add_local_once(fit_tabs);
        }
        fit_tabs();
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
        title,
        crumb: header.subtitle.clone(),
        header: header.root.clone().upcast(),
        header_end: header.end.parent(),
        tab_strip: strip.root,
        tabs: strip.tabs,
        more: strip.more,
        add: strip.add,
        tab_widths: Vec::new(),
        more_width: 0,
        hidden: Vec::new(),
        readout: foot.readout,
        ending: foot.ending,
        indent: foot.indent,
        size: foot.size,
        find_card: card.root,
        find_button,
        find: card.find,
        find_count: card.count,
        replace_entry: card.replace,
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

/// Find and replace's card and its fields.
struct FindCard {
    root: gtk::Box,
    find: gtk::Text,
    count: gtk::Label,
    replace: gtk::Text,
}

/// A field in the card: an icon, the text, and what follows it inside the field.
fn card_field(icon: &str, placeholder: &str) -> (gtk::Box, gtk::Text) {
    let field = widgets::hbox(8);
    field.add_css_class("find-field");
    field.set_hexpand(true);
    field.append(&gtk::Image::from_icon_name(icon));
    let text = gtk::Text::new();
    text.set_placeholder_text(Some(placeholder));
    text.set_hexpand(true);
    text.update_property(&[gtk::accessible::Property::Label(placeholder)]);
    field.append(&text);
    // A click anywhere on the field types in it.
    let click = gtk::GestureClick::new();
    let t = text.clone();
    click.connect_pressed(move |_, _, _, _| {
        t.grab_focus();
    });
    field.add_controller(click);
    (field, text)
}

/// Find and replace, a card at the top right of the text: the find field with its
/// match count, previous, next and close; under it the replace field, Replace and All.
fn find_card() -> FindCard {
    let root = widgets::vbox(6);
    root.add_css_class("find-card");
    root.set_halign(gtk::Align::End);
    root.set_valign(gtk::Align::Start);
    root.set_margin_top(10);
    root.set_margin_end(12);
    root.set_width_request(340);
    root.set_visible(false);

    let (find_field, find) = card_field("system-search-symbolic", "Find");
    let count = widgets::label("", "find-count");
    find_field.append(&count);
    let prev = widgets::bar_button("go-up-symbolic", "Previous match (Shift+Enter)");
    let next = widgets::bar_button("go-down-symbolic", "Next match (Enter)");
    let close = widgets::bar_button("window-close-symbolic", "Close (Esc)");
    prev.connect_clicked(|_| editor::find_next(false));
    next.connect_clicked(|_| editor::find_next(true));
    close.connect_clicked(|_| close_find());
    let row = widgets::hbox(4);
    row.append(&find_field);
    row.append(&prev);
    row.append(&next);
    row.append(&close);
    root.append(&row);

    let (replace_field, replace) = card_field("document-edit-symbolic", "Replace with");
    let one = gtk::Button::with_label("Replace");
    one.set_tooltip_text(Some("Replace this match and go to the next (Enter)"));
    let all = gtk::Button::with_label("All");
    all.set_tooltip_text(Some("Replace every match (Ctrl+Enter)"));
    all.update_property(&[gtk::accessible::Property::Label("Replace all")]);
    for b in [&one, &all] {
        b.add_css_class("find-action");
        b.set_valign(gtk::Align::Center);
    }
    let row = widgets::hbox(4);
    row.append(&replace_field);
    row.append(&one);
    row.append(&all);
    root.append(&row);

    // Typing settles for a moment before the matches are found again.
    find.connect_changed(|t| {
        if let Some(id) = FIND_PENDING.with(|p| p.take()) {
            id.remove();
        }
        let t = t.clone();
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            FIND_PENDING.with(|p| p.set(None));
            editor::set_query(&t.text());
        });
        FIND_PENDING.with(|p| p.set(Some(id)));
    });
    find.connect_activate(|_| editor::find_next(true));
    // Shift+Enter goes back a match.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(|_, key, _, mods| {
        if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) && mods.contains(gdk::ModifierType::SHIFT_MASK) {
            editor::find_next(false);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    find.add_controller(keys);

    let r = replace.clone();
    one.connect_clicked(move |_| editor::replace_one(&r.text()));
    let r = replace.clone();
    all.connect_clicked(move |_| editor::replace_all(&r.text()));
    replace.connect_activate(|r| editor::replace_one(&r.text()));
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let r = replace.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) && mods.contains(gdk::ModifierType::CONTROL_MASK) {
            editor::replace_all(&r.text());
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    replace.add_controller(keys);

    FindCard { root, find, count, replace }
}

/// A menu that opens from `button`, built fresh each time, its right edge under the
/// button's (or above it, from the foot).
fn menu_button(button: &gtk::Button, above: bool, entries: impl Fn() -> Vec<Entry> + 'static) {
    button.connect_clicked(move |b| {
        let pop = menu::popover(b, entries());
        pop.set_position(if above { gtk::PositionType::Top } else { gtk::PositionType::Bottom });
        pop.set_halign(gtk::Align::End);
        if SHOWING_FOR_TEST.with(|t| t.get()) {
            pop.set_autohide(false);
            TEST_POPOVER.with(|t| t.borrow().set(Some(&pop)));
        }
        b.add_css_class("open");
        let b2 = b.clone();
        pop.connect_closed(move |_| b2.remove_css_class("open"));
        pop.popup();
    });
}

/// A labelled button with a chevron: its label, and the button.
fn chevron_button(text: &str, class: &str) -> (gtk::Button, gtk::Label) {
    let button = gtk::Button::new();
    button.add_css_class(class);
    button.set_valign(gtk::Align::Center);
    let face = widgets::hbox(5);
    let label = widgets::label(text, "");
    face.append(&label);
    let chevron = gtk::Image::from_icon_name("pan-down-symbolic");
    chevron.add_css_class("chevron");
    face.append(&chevron);
    button.set_child(Some(&face));
    (button, label)
}

/// The File menu: new, open, save, reopen, and the files opened lately.
fn file_button() -> gtk::Button {
    let (button, _) = chevron_button("File", "file-button");
    button.set_tooltip_text(Some("New, open, save and recent files"));
    menu_button(&button, false, file_entries);
    button
}

fn file_entries() -> Vec<Entry> {
    let mut entries = vec![
        menu::item("New tab", doc::new_tab).icon("document-new-symbolic").key("Ctrl+T"),
        menu::item("Open…", doc::open_dialog).icon("document-open-symbolic").key("Ctrl+O"),
        menu::item("Save", doc::save).icon("document-save-symbolic").key("Ctrl+S"),
        menu::item("Save as…", doc::save_as).key("Ctrl+Shift+S"),
        menu::item("Reopen closed tab", doc::reopen_closed).icon("document-open-recent-symbolic").key("Ctrl+Shift+T"),
    ];
    let recent = session::recent();
    if !recent.is_empty() {
        entries.push(menu::separator());
        entries.push(menu::heading("Recent"));
        for p in recent.into_iter().take(8) {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let folder = p.parent().map(|d| short_folder(&paths::pretty(d))).unwrap_or_default();
            entries.push(menu::item(&name, move || doc::open(&p)).icon("text-x-generic-symbolic").key(&folder));
        }
        entries.push(menu::item("Clear recent files", session::clear_recent));
    }
    entries
}

/// A folder short enough for a menu row's end: its last part, past a leading "…".
fn short_folder(folder: &str) -> String {
    const MAX: usize = 26;
    let n = folder.chars().count();
    if n <= MAX { folder.to_string() } else { format!("…{}", folder.chars().skip(n - MAX + 1).collect::<String>()) }
}

/// The tabs in the header: the tabs, "+N" for those that don't fit, and +.
struct TabStrip {
    root: gtk::Box,
    tabs: gtk::Box,
    more: gtk::Button,
    add: gtk::Button,
}

/// The tabs in the bar, with a + at their end (hidden with one tab unless the
/// setting says otherwise; the bar then names the file).
fn tab_bar() -> TabStrip {
    let root = widgets::hbox(2);
    root.add_css_class("tab-row");
    let tabs = widgets::hbox(2);
    tabs.add_css_class("tabs");
    // Tabs that don't fit are left out rather than scrolled, but the strip may still
    // shrink below its tabs' width while the window narrows.
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .child(&tabs)
        .build();
    root.append(&scroll);
    let more = gtk::Button::with_label("+0");
    more.add_css_class("tab-more");
    more.set_valign(gtk::Align::Center);
    more.set_tooltip_text(Some("The tabs that don't fit"));
    more.set_visible(false);
    more.connect_clicked(|b| {
        let Some(ui) = ui() else { return };
        let hidden = ui.borrow().hidden.clone();
        let docs = doc::all();
        let entries: Vec<Entry> = hidden
            .into_iter()
            .filter_map(|i| docs.get(i).map(|d| (i, d)))
            .map(|(i, d)| {
                let e = menu::item(&d.title(), move || doc::switch_to(i)).icon("text-x-generic-symbolic");
                if d.dirty() { e.key("Not saved") } else { e }
            })
            .collect();
        menu::popup_below(b, entries);
    });
    root.append(&more);
    let add = widgets::bar_button("list-add-symbolic", "New tab (Ctrl+T)");
    add.add_css_class("tab-new");
    add.connect_clicked(|_| doc::new_tab());
    root.append(&add);
    TabStrip { root, tabs, more, add }
}

/// One tab per open file: its name, a dot when it has unsaved changes, and ✕.
fn refresh_tabs() {
    let Some(ui) = ui() else { return };
    let (bar, tabs, more) = {
        let u = ui.borrow();
        (u.tab_strip.clone(), u.tabs.clone(), u.more.clone())
    };
    while let Some(c) = tabs.first_child() {
        tabs.remove(&c);
    }
    let docs = doc::all();
    let current = doc::current_index();
    let mut widths = Vec::with_capacity(docs.len());
    for (i, d) in docs.iter().enumerate() {
        let tab = widgets::hbox(0);
        tab.add_css_class("tab");
        if i == current {
            tab.add_css_class("active");
        }
        let main = gtk::Button::new();
        main.add_css_class("tab-main");
        let content = widgets::hbox(7);
        let name = widgets::label(&d.title(), "tab-name");
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.set_max_width_chars(24);
        content.append(&name);
        if d.dirty() {
            let dot = widgets::hbox(0);
            dot.add_css_class("tab-dot");
            dot.set_valign(gtk::Align::Center);
            content.append(&dot);
        }
        main.set_child(Some(&content));
        main.set_tooltip_text(Some(&match d.path() {
            Some(p) => paths::pretty(&p),
            None => "Not saved yet".to_string(),
        }));
        main.update_property(&[gtk::accessible::Property::Label(&if d.dirty() {
            format!("{}, not saved", d.title())
        } else {
            d.title()
        })]);
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
        widths.push(tab.measure(gtk::Orientation::Horizontal, -1).1);
    }
    more.set_label(&format!("+{}", docs.len()));
    more.set_visible(true);
    let more_width = more.measure(gtk::Orientation::Horizontal, -1).1;
    {
        let mut u = ui.borrow_mut();
        u.tab_widths = widths;
        u.more_width = more_width;
        u.hidden.clear();
    }
    let show_tabs = prefs::get().always_show_tabs || docs.len() > 1;
    bar.set_visible(show_tabs);
    ui.borrow().crumb.set_visible(!show_tabs);
    fit_tabs();
}

/// Show as many tabs as fit in the header, always the current one; the rest are
/// counted in "+N", whose menu lists them.
fn fit_tabs() {
    let Some(ui) = ui() else { return };
    let u = ui.borrow();
    let (widths, more_width) = (&u.tab_widths, u.more_width);
    let children: Vec<gtk::Widget> = {
        let mut v = Vec::new();
        let mut child = u.tabs.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            v.push(c);
        }
        v
    };
    if children.len() != widths.len() {
        return;
    }
    const GAP: i32 = 2;
    let n = widths.len();
    let header_w = u.header.width();
    let start = u.tab_strip.compute_point(&u.header, &gtk::graphene::Point::new(0.0, 0.0)).map(|p| p.x() as i32);
    let end_w = u.header_end.as_ref().map(|e| e.width()).unwrap_or(0);
    let add_w = u.add.measure(gtk::Orientation::Horizontal, -1).1 + GAP;
    let mut shown = vec![true; n];
    if let Some(start) = start.filter(|_| header_w > 0 && n > 1) {
        let room = header_w - start - end_w - 16;
        let all: i32 = widths.iter().map(|w| w + GAP).sum::<i32>() + add_w;
        if all > room {
            let current = doc::current_index().min(n - 1);
            let mut budget = room - add_w - more_width - GAP - widths[current] - GAP;
            shown = vec![false; n];
            shown[current] = true;
            for i in (0..n).filter(|&i| i != current) {
                if widths[i] + GAP > budget {
                    break;
                }
                budget -= widths[i] + GAP;
                shown[i] = true;
            }
        }
    }
    let hidden: Vec<usize> = (0..n).filter(|&i| !shown[i]).collect();
    for (c, on) in children.iter().zip(&shown) {
        c.set_visible(*on);
    }
    u.more.set_label(&format!("+{}", hidden.len()));
    u.more.set_visible(!hidden.is_empty());
    drop(u);
    ui.borrow_mut().hidden = hidden;
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

/// Re-read the cursor position, counts and the foot's buttons.
pub fn refresh_status() {
    let Some(ui) = ui() else { return };
    let (readout, ending, indent, size) = {
        let u = ui.borrow();
        (u.readout.clone(), u.ending.clone(), u.indent.clone(), u.size.clone())
    };
    readout.set_text(&editor::status());
    let p = prefs::get();
    ending.set_text(if doc::current().is_some_and(|d| d.crlf()) { "CRLF" } else { "LF" });
    indent.set_text(&format!("{}: {}", if p.insert_spaces { "Spaces" } else { "Tabs" }, p.tab_width));
    size.set_text(&format!("{} pt", p.font_size));
    refresh_find_count();
}

pub fn refresh_find_count() {
    let Some(ui) = ui() else { return };
    let label = ui.borrow().find_count.clone();
    if find_open() {
        label.set_text(&editor::find_count());
    }
}

/// Ctrl+F opens find; Ctrl+H opens it with the replace field focused. Either starts
/// from the selected text.
fn toggle_find(replace: bool) {
    let Some(ui) = ui() else { return };
    let (card, button, find, rep) = {
        let u = ui.borrow();
        (u.find_card.clone(), u.find_button.clone(), u.find.clone(), u.replace_entry.clone())
    };
    let opening = !card.is_visible();
    card.set_visible(true);
    button.add_css_class("on");
    editor::set_finding(true);
    if let Some(d) = doc::current()
        && let Some((s, e)) = d.buffer.selection_bounds()
        && s.line() == e.line()
    {
        find.set_text(&d.buffer.text(&s, &e, false));
    }
    if replace && (!opening || !find.text().is_empty()) {
        rep.grab_focus();
        rep.select_region(0, -1);
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
    // Clearing the field counts as typing; nothing is left to find.
    if let Some(id) = FIND_PENDING.with(|p| p.take()) {
        id.remove();
    }
    u.find_card.set_visible(false);
    u.find_button.remove_css_class("on");
    u.find_count.set_text("");
    drop(u);
    editor::set_finding(false);
    editor::set_query("");
    editor::focus();
}

fn find_open() -> bool {
    ui().is_some_and(|u| u.borrow().find_card.is_visible())
}

/// The foot's parts.
struct Foot {
    root: gtk::Box,
    readout: gtk::Label,
    ending: gtk::Label,
    indent: gtk::Label,
    size: gtk::Label,
}

/// The sheet's foot: the cursor's place and counts, then buttons for the file's line
/// endings, the indent and the text size, each opening a menu that changes it.
fn foot() -> Foot {
    let root = widgets::hbox(2);
    root.add_css_class("doc-foot");
    let readout = widgets::label("", "status-readout");
    readout.set_ellipsize(gtk::pango::EllipsizeMode::End);
    readout.set_hexpand(true);
    readout.set_xalign(0.0);
    root.append(&readout);

    let (b, ending) = chevron_button("LF", "foot-pill");
    b.set_tooltip_text(Some("This file's line endings"));
    menu_button(&b, true, || {
        let crlf = doc::current().is_some_and(|d| d.crlf());
        vec![
            menu::heading("Line endings"),
            menu::check("LF", !crlf, |_| set_line_ending(false)).key("Linux, macOS"),
            menu::check("CRLF", crlf, |_| set_line_ending(true)).key("Windows"),
        ]
    });
    root.append(&b);

    let (b, indent) = chevron_button("Spaces: 4", "foot-pill");
    b.set_tooltip_text(Some("What Tab types, and how wide a tab is"));
    menu_button(&b, true, || {
        let p = prefs::get();
        let mut entries = vec![
            menu::heading("Tab types"),
            menu::check("Spaces", p.insert_spaces, |_| set_editor(|p| p.insert_spaces = true)),
            menu::check("A tab", !p.insert_spaces, |_| set_editor(|p| p.insert_spaces = false)),
            menu::separator(),
            menu::heading("Width"),
        ];
        for w in [2, 4, 8] {
            entries.push(menu::check(&format!("{w} columns"), p.tab_width == w, move |_| set_editor(|p| p.tab_width = w)));
        }
        entries
    });
    root.append(&b);

    let (b, size) = chevron_button("12 pt", "foot-pill");
    b.set_tooltip_text(Some("Text size"));
    menu_button(&b, true, || {
        let now = prefs::get().font_size;
        let mut entries = vec![
            menu::item("Bigger", || editor::set_size((prefs::get().font_size + 1.0).min(48.0))).key("Ctrl++"),
            menu::item("Smaller", || editor::set_size((prefs::get().font_size - 1.0).max(6.0))).key("Ctrl+−"),
            menu::item(&format!("Default, {} pt", prefs::DEFAULT_SIZE), || editor::set_size(prefs::DEFAULT_SIZE)).key("Ctrl+0"),
            menu::separator(),
        ];
        for &s in SIZES {
            entries.push(menu::check(&format!("{s} pt"), (now - s).abs() < 0.01, move |_| editor::set_size(s)));
        }
        entries
    });
    root.append(&b);

    Foot { root, readout, ending, indent, size }
}

fn set_line_ending(crlf: bool) {
    if let Some(d) = doc::current() {
        d.set_crlf(crlf);
    }
    refresh_status();
}

fn set_editor(change: impl FnOnce(&mut prefs::Prefs)) {
    prefs::update(change);
    editor::apply_prefs();
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
    match overlay() {
        Some(o) => nyx_ui::toast::show(&o, message),
        None => eprintln!("nyx-notepad: {message}"),
    }
}

/// A toast with a button (Undo); it stays a little longer.
pub fn toast_with_action(message: &str, label: &str, act: impl Fn() + 'static) {
    match overlay() {
        Some(o) => nyx_ui::toast::show_action(&o, message, label, act),
        None => eprintln!("nyx-notepad: {message}"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_long_folders_from_the_start() {
        assert_eq!(short_folder("~/Documents"), "~/Documents");
        let long = "~/Projects/nyx-design/mockups/revamp/apps";
        let short = short_folder(long);
        assert_eq!(short.chars().count(), 26);
        assert!(short.starts_with('…') && long.ends_with(short.trim_start_matches('…')));
    }
}
