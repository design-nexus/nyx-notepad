//! The editor: one `TextView` that shows the current tab's buffer. It applies the
//! formatting preferences (font, size, spacing, wrapping, tabs, text width), draws the
//! line-number gutter, highlights the current line, and does find and replace.

use crate::doc::{self, Change};
use crate::{prefs, theme, window};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, pango};
use std::cell::{Cell, RefCell};

/// Matches counted and highlighted at most; past this the count reads "10,000+".
const MAX_MATCHES: usize = 10_000;

struct Editor {
    view: gtk::TextView,
    scroll: gtk::ScrolledWindow,
    gutter: LineGutter,
    provider: gtk::CssProvider,
    /// Signal handlers on the buffer showing now, removed when another shows.
    handlers: Vec<glib::SignalHandlerId>,
    shown: Option<gtk::TextBuffer>,
    /// The width of one character in the editor font, in pixels.
    char_w: Cell<i32>,
    query: String,
    matches: usize,
}

/// How far the current line's band reaches past the text column on each side.
const BAND_PAD: i32 = 20;

thread_local! {
    static ED: RefCell<Option<Editor>> = const { RefCell::new(None) };
    static SIZE_SCALE: RefCell<glib::WeakRef<gtk::Scale>> = RefCell::new(glib::WeakRef::new());
    static WORDS: Cell<usize> = const { Cell::new(0) };
    static WORDS_PENDING: Cell<Option<glib::SourceId>> = const { Cell::new(None) };
    static MATCH_PENDING: Cell<Option<glib::SourceId>> = const { Cell::new(None) };
    static FIT_PENDING: Cell<bool> = const { Cell::new(false) };
}

fn view() -> Option<gtk::TextView> {
    ED.with(|e| e.borrow().as_ref().map(|e| e.view.clone()))
}

pub fn build() -> gtk::Widget {
    let view: PageView = glib::Object::new();
    let view: gtk::TextView = view.upcast();
    view.add_css_class("editor");
    view.add_css_class("editor-font");
    view.set_monospace(false);
    view.set_hexpand(true);
    view.set_vexpand(true);
    view.set_top_margin(28);
    view.set_bottom_margin(36);
    view.set_accepts_tab(true);

    let scroll = gtk::ScrolledWindow::builder().child(&view).hexpand(true).vexpand(true).build();
    scroll.add_css_class("editor-area");

    let gutter: LineGutter = glib::Object::new();
    gutter.add_css_class("line-numbers");
    gutter.add_css_class("editor-font");
    gutter.imp().view.set(Some(&view));

    let provider = gtk::CssProvider::new();
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
    }

    // Tab types spaces when asked to.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(|_, key, _, mods| {
        if key == gdk::Key::Tab
            && !mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SHIFT_MASK)
        {
            let p = prefs::get();
            if p.insert_spaces
                && let Some(d) = doc::current()
            {
                let b = &d.buffer;
                b.begin_user_action();
                b.delete_selection(true, true);
                let col = b.iter_at_mark(&b.get_insert()).line_offset().max(0) as u32;
                let n = p.tab_width - col % p.tab_width.max(1);
                b.insert_at_cursor(&" ".repeat(n as usize));
                b.end_user_action();
                return glib::Propagation::Stop;
            }
        }
        glib::Propagation::Proceed
    });
    view.add_controller(keys);

    // Keep the gutter in step with scrolling and layout.
    let vadj = scroll.vadjustment();
    let g = gutter.clone();
    vadj.connect_value_changed(move |_| g.queue_draw());
    let g = gutter.clone();
    vadj.connect_upper_notify(move |_| g.queue_draw());
    // The text column is re-centred whenever the width changes. Not right away: the
    // view sets the page size while validating its lines, and new margins then would
    // invalidate them under it (GTK aborts on `onscreen_validated`).
    scroll.hadjustment().connect_page_size_notify(|_| fit_width_soon());

    ED.with(|e| {
        *e.borrow_mut() = Some(Editor {
            view: view.clone(),
            scroll: scroll.clone(),
            gutter,
            provider,
            handlers: Vec::new(),
            shown: None,
            char_w: Cell::new(8),
            query: String::new(),
            matches: 0,
        })
    });

    doc::subscribe(&view, |c| {
        if c == Change::Current {
            show_current();
        }
    });
    theme::subscribe(&view, recolor);
    apply_prefs();
    scroll.upcast()
}

pub fn focus() {
    if let Some(v) = view() {
        v.grab_focus();
    }
}

/// Note where the showing tab's cursor and scroll are, before another tab shows.
pub fn remember_position() {
    let state = ED.with(|e| e.borrow().as_ref().map(|e| (e.shown.clone(), e.scroll.vadjustment().value())));
    let Some((Some(buffer), value)) = state else { return };
    if let Some(d) = doc::all().into_iter().find(|d| d.buffer == buffer) {
        d.cursor.set(buffer.cursor_position());
        d.scroll.set(value);
    }
}

fn show_current() {
    let Some(d) = doc::current() else { return };
    let Some(view) = view() else { return };
    let same = ED.with(|e| e.borrow().as_ref().and_then(|e| e.shown.clone()).is_some_and(|b| b == d.buffer));
    if !same {
        // Stop listening to the buffer that was showing.
        ED.with(|e| {
            let mut e = e.borrow_mut();
            let Some(e) = e.as_mut() else { return };
            if let Some(old) = e.shown.take() {
                for h in e.handlers.drain(..) {
                    old.disconnect(h);
                }
            }
        });
        view.set_buffer(Some(&d.buffer));
        let b = d.buffer.clone();
        let handlers = vec![
            b.connect_changed(|_| {
                count_words_soon();
                rematch_soon();
                if let Some(g) = gutter() {
                    g.fit();
                    g.queue_draw();
                }
            }),
            b.connect_cursor_position_notify(|_| {
                mark_current_line();
                window::refresh_status();
            }),
            b.connect_mark_set(|_, _, mark| {
                if mark.name().as_deref() == Some("selection_bound") {
                    window::refresh_status();
                }
            }),
        ];
        ED.with(|e| {
            if let Some(e) = e.borrow_mut().as_mut() {
                e.handlers = handlers;
                e.shown = Some(b);
            }
        });
        d.buffer.place_cursor(&d.buffer.iter_at_offset(d.cursor.get()));
        let (scroll, vadj) = (d.scroll.get(), ED.with(|e| e.borrow().as_ref().map(|e| e.scroll.vadjustment())));
        let v = view.clone();
        // Restore the scroll on the first frame after the view has its real size;
        // scrolling earlier works from sizes that are about to change.
        v.add_tick_callback(move |v, _| {
            if v.width() <= 1 {
                return glib::ControlFlow::Continue;
            }
            if scroll > 0.0
                && let Some(vadj) = &vadj
            {
                vadj.set_value(scroll);
            } else {
                // Only as far as needed to show the cursor: none at all when it's in view.
                v.scroll_mark_onscreen(&v.buffer().get_insert());
            }
            glib::ControlFlow::Break
        });
        if let Some(g) = gutter() {
            g.fit();
            g.queue_draw();
        }
        mark_current_line();
        count_words_now();
        rematch();
    }
    view.grab_focus();
    window::refresh_status();
}

fn gutter() -> Option<LineGutter> {
    ED.with(|e| e.borrow().as_ref().map(|e| e.gutter.clone()))
}

// ---------- Preferences ----------

/// The app font, as in the rest of the window.
const APP_FONTS: &str = "\"JetBrains Mono\", \"JetBrainsMono Nerd Font\", monospace";

/// CSS strings can't hold a bare quote or backslash.
fn css_string(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '"' | '\\' | '\n' | ';' | '{' | '}')).collect()
}

/// Apply every formatting preference to the editor, live.
pub fn apply_prefs() {
    let p = prefs::get();
    let Some(view) = view() else { return };
    let size = p.font_size.clamp(6.0, 72.0);
    ED.with(|e| {
        if let Some(e) = e.borrow().as_ref() {
            let family = if p.font_family.is_empty() {
                APP_FONTS.to_string()
            } else {
                format!("\"{}\", monospace", css_string(&p.font_family))
            };
            e.provider.load_from_string(&format!(".editor-font {{ font-family: {family}; font-size: {size}pt; }}"));
        }
    });
    view.set_wrap_mode(if p.word_wrap { gtk::WrapMode::WordChar } else { gtk::WrapMode::None });
    let px = size * 96.0 / 72.0;
    let gap = ((p.line_spacing - 1.0).max(0.0) * px).round() as i32;
    view.set_pixels_inside_wrap(gap);
    view.set_pixels_below_lines(gap);
    let g = gutter();
    view.set_gutter(gtk::TextWindowType::Left, if p.line_numbers { g.as_ref() } else { None });
    recolor();
    window::refresh_status();
    // The font change reaches the view's Pango context on the next frame.
    glib::idle_add_local_once(|| {
        let Some(view) = self::view() else { return };
        let (w, _) = view.create_pango_layout(Some("0000000000")).pixel_size();
        let char_w = (w as f64 / 10.0).round().max(1.0) as i32;
        ED.with(|e| {
            if let Some(e) = e.borrow().as_ref() {
                e.char_w.set(char_w);
            }
        });
        let mut tabs = pango::TabArray::new(1, true);
        tabs.set_tab(0, pango::TabAlign::Left, char_w * prefs::get().tab_width.max(1) as i32);
        view.set_tabs(&tabs);
        fit_width();
        if let Some(g) = gutter() {
            g.fit();
            g.queue_draw();
        }
    });
}

fn fit_width_soon() {
    if FIT_PENDING.with(|p| p.replace(true)) {
        return;
    }
    glib::idle_add_local_once(|| {
        FIT_PENDING.with(|p| p.set(false));
        fit_width();
    });
}

/// Between the line numbers' gutter and the text.
const GUTTER_GAP: i32 = 12;

/// Side margins: the page padding, or wider to centre a column of 80 or 100 characters
/// in the whole view. Line numbers stay beside the text: the gutter widens to fill the
/// space on the left.
fn fit_width() {
    let Some((view, char_w, gutter)) =
        ED.with(|e| e.borrow().as_ref().map(|e| (e.view.clone(), e.char_w.get(), e.gutter.clone())))
    else {
        return;
    };
    let p = prefs::get();
    let width = view.width();
    let pad = if window::narrow() { 16 } else { 24 };
    let lead = if p.line_numbers { gutter.imp().min_width.get() + GUTTER_GAP } else { pad };
    let (left, right) = match p.text_width.parse::<i32>() {
        Ok(cols) if p.word_wrap && width > 0 => {
            let column = cols * char_w;
            let left = ((width - column) / 2).max(lead);
            (left, (width - left - column).max(pad))
        }
        _ => (lead, pad),
    };
    if p.line_numbers {
        if gutter.width_request() != left - GUTTER_GAP {
            gutter.set_width_request(left - GUTTER_GAP);
        }
        view.set_left_margin(GUTTER_GAP);
    } else {
        view.set_left_margin(left);
    }
    view.set_right_margin(right);
}

/// The match and current-line colours follow the theme.
fn recolor() {
    let pal = theme::palette();
    let parse = |s: &str| gdk::RGBA::parse(s).unwrap_or(gdk::RGBA::BLACK);
    let (bg, accent, warning) = (parse(&pal.bg), parse(&pal.accent), parse(&theme::look().warning));
    let mix = |to: &gdk::RGBA, t: f32| {
        gdk::RGBA::new(
            bg.red() + (to.red() - bg.red()) * t,
            bg.green() + (to.green() - bg.green()) * t,
            bg.blue() + (to.blue() - bg.blue()) * t,
            1.0,
        )
    };
    if let Some(t) = doc::tags().lookup("match") {
        t.set_background_rgba(Some(&mix(&warning, 0.3)));
    }
    if let Some(v) = view().and_downcast::<PageView>() {
        let band = prefs::get().highlight_line.then(|| mix(&accent, if pal.light { 0.08 } else { 0.06 }));
        *v.imp().band.borrow_mut() = band;
    }
    mark_current_line();
}

/// The band behind the cursor's line is drawn with the text; redraw it where it moved.
fn mark_current_line() {
    if let Some(v) = view() {
        v.queue_draw();
    }
    if let Some(g) = gutter() {
        g.queue_draw();
    }
}

/// While finding, the selected match is drawn like a current match.
pub fn set_finding(on: bool) {
    if let Some(v) = view() {
        if on {
            v.add_css_class("finding");
        } else {
            v.remove_css_class("finding");
        }
    }
}

// ---------- Size ----------

pub fn register_size_scale(scale: &gtk::Scale) {
    SIZE_SCALE.with(|s| s.borrow().set(Some(scale)));
}

/// Ctrl + / Ctrl − / Ctrl 0.
pub fn zoom(step: i32) {
    let size = if step == 0 { prefs::DEFAULT_SIZE } else { (prefs::get().font_size + step as f64).clamp(6.0, 48.0) };
    set_size(size);
    window::toast(&format!("Text size {size} pt"));
}

pub fn set_size(size: f64) {
    prefs::update(|p| p.font_size = size);
    apply_prefs();
    if let Some(s) = SIZE_SCALE.with(|s| s.borrow().upgrade()) {
        s.set_value(size);
    }
}

// ---------- Status ----------

fn count_words_now() {
    let n = doc::current().map(|d| d.text().split_whitespace().count()).unwrap_or(0);
    WORDS.with(|w| w.set(n));
    window::refresh_status();
}

fn count_words_soon() {
    if let Some(id) = WORDS_PENDING.with(|p| p.take()) {
        id.remove();
    }
    let id = glib::timeout_add_local_once(std::time::Duration::from_millis(250), || {
        WORDS_PENDING.with(|p| p.set(None));
        count_words_now();
    });
    WORDS_PENDING.with(|p| p.set(Some(id)));
}

/// "Ln 12, Col 4 · 231 words", with the selection's size while there is one and the
/// match count while finding.
pub fn status() -> String {
    let Some(d) = doc::current() else { return String::new() };
    let b = &d.buffer;
    let it = b.iter_at_mark(&b.get_insert());
    let mut parts = vec![format!("Ln {}, Col {}", it.line() + 1, it.line_offset() + 1)];
    let finding = !query().is_empty();
    // While finding, the selection is the match.
    if let Some((s, e)) = b.selection_bounds().filter(|_| !finding) {
        let n = (e.offset() - s.offset()).unsigned_abs() as usize;
        parts.push(format!("{} selected", thousands(n)));
    }
    let words = WORDS.with(|w| w.get());
    parts.push(format!("{} {}", thousands(words), if words == 1 { "word" } else { "words" }));
    if finding {
        let n = ED.with(|e| e.borrow().as_ref().map(|e| e.matches).unwrap_or(0));
        let count = if n >= MAX_MATCHES { format!("{}+", thousands(n)) } else { thousands(n) };
        parts.push(format!("{count} {}", if n == 1 { "match" } else { "matches" }));
    }
    parts.join(" · ")
}

/// 1,204
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ---------- Find and replace ----------

const FLAGS: gtk::TextSearchFlags = gtk::TextSearchFlags::CASE_INSENSITIVE.union(gtk::TextSearchFlags::TEXT_ONLY);

/// Highlight every match of `q` and select the first one at or after the cursor.
pub fn set_query(q: &str) {
    ED.with(|e| {
        if let Some(e) = e.borrow_mut().as_mut() {
            e.query = q.to_string();
        }
    });
    rematch();
    if !q.is_empty() {
        find_from_cursor();
    }
}

fn query() -> String {
    ED.with(|e| e.borrow().as_ref().map(|e| e.query.clone()).unwrap_or_default())
}

fn rematch_soon() {
    if query().is_empty() {
        return;
    }
    if let Some(id) = MATCH_PENDING.with(|p| p.take()) {
        id.remove();
    }
    let id = glib::timeout_add_local_once(std::time::Duration::from_millis(200), || {
        MATCH_PENDING.with(|p| p.set(None));
        rematch();
    });
    MATCH_PENDING.with(|p| p.set(Some(id)));
}

fn rematch() {
    let Some(d) = doc::current() else { return };
    let b = &d.buffer;
    let (s, e) = b.bounds();
    b.remove_tag_by_name("match", &s, &e);
    let q = query();
    let mut n = 0;
    if !q.is_empty() {
        let mut at = b.start_iter();
        while let Some((ms, me)) = at.forward_search(&q, FLAGS, None) {
            b.apply_tag_by_name("match", &ms, &me);
            n += 1;
            if n >= MAX_MATCHES || me == at {
                break;
            }
            at = me;
        }
    }
    ED.with(|e| {
        if let Some(e) = e.borrow_mut().as_mut() {
            e.matches = n;
        }
    });
    window::refresh_status();
}

fn select_match(s: &gtk::TextIter, e: &gtk::TextIter) {
    let Some(view) = view() else { return };
    let b = view.buffer();
    b.select_range(s, e);
    view.scroll_to_mark(&b.get_insert(), 0.15, false, 0.0, 0.0);
    window::refresh_find_count();
}

fn find_from_cursor() {
    let Some(d) = doc::current() else { return };
    let b = &d.buffer;
    let from = b.selection_bounds().map(|(s, _)| s).unwrap_or_else(|| b.iter_at_mark(&b.get_insert()));
    let q = query();
    if let Some((s, e)) = from.forward_search(&q, FLAGS, None).or_else(|| b.start_iter().forward_search(&q, FLAGS, None)) {
        select_match(&s, &e);
    }
}

/// The next (or previous) match, wrapping around the ends.
pub fn find_next(forward: bool) {
    let Some(d) = doc::current() else { return };
    let q = query();
    if q.is_empty() {
        return;
    }
    let b = &d.buffer;
    let (sel_s, sel_e) = b.selection_bounds().unwrap_or_else(|| {
        let i = b.iter_at_mark(&b.get_insert());
        (i, i)
    });
    let hit = if forward {
        sel_e.forward_search(&q, FLAGS, None).or_else(|| b.start_iter().forward_search(&q, FLAGS, None))
    } else {
        sel_s.backward_search(&q, FLAGS, None).or_else(|| b.end_iter().backward_search(&q, FLAGS, None))
    };
    match hit {
        Some((s, e)) => select_match(&s, &e),
        None => window::toast(&format!("“{q}” isn't in this file")),
    }
}

/// "3 of 12", "No matches", or nothing while not searching.
pub fn find_count() -> String {
    let q = query();
    if q.is_empty() {
        return String::new();
    }
    let n = ED.with(|e| e.borrow().as_ref().map(|e| e.matches).unwrap_or(0));
    if n == 0 {
        return "No matches".into();
    }
    let total = if n >= MAX_MATCHES { format!("{}+", thousands(n)) } else { thousands(n) };
    let Some(d) = doc::current() else { return total };
    let b = &d.buffer;
    let Some((s, e)) = b.selection_bounds() else { return total };
    if !b.text(&s, &e, false).to_lowercase().eq(&q.to_lowercase()) {
        return total;
    }
    // Which match is selected: count the ones before it.
    let mut i = 1;
    let mut at = b.start_iter();
    while let Some((ms, me)) = at.forward_search(&q, FLAGS, Some(&s)) {
        if ms >= s || i >= MAX_MATCHES {
            break;
        }
        i += 1;
        at = me;
    }
    format!("{} of {total}", thousands(i))
}

/// Replace the selected match, then go to the next one.
pub fn replace_one(with: &str) {
    let Some(d) = doc::current() else { return };
    let q = query();
    if q.is_empty() {
        return;
    }
    let b = &d.buffer;
    if let Some((mut s, mut e)) = b.selection_bounds()
        && b.text(&s, &e, false).to_lowercase() == q.to_lowercase()
    {
        b.begin_user_action();
        b.delete(&mut s, &mut e);
        b.insert(&mut s, with);
        b.end_user_action();
        b.place_cursor(&s);
    }
    rematch();
    find_next(true);
}

/// Replace every match as one undo step.
pub fn replace_all(with: &str) {
    let Some(d) = doc::current() else { return };
    let q = query();
    if q.is_empty() {
        return;
    }
    let b = &d.buffer;
    let mut n = 0;
    b.begin_user_action();
    let mut at = b.start_iter();
    while let Some((mut s, mut e)) = at.forward_search(&q, FLAGS, None) {
        b.delete(&mut s, &mut e);
        b.insert(&mut s, with);
        at = s;
        n += 1;
    }
    b.end_user_action();
    rematch();
    window::toast(&match n {
        0 => format!("“{q}” isn't in this file"),
        1 => "Replaced 1 match".into(),
        n => format!("Replaced {} matches", thousands(n)),
    });
}

// ---------- The text view ----------

mod view_imp {
    use super::*;

    /// The text view, drawing the current line's band across the text column only
    /// (a paragraph background would fill the margins too).
    #[derive(Default)]
    pub struct PageView {
        pub band: RefCell<Option<gdk::RGBA>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PageView {
        const NAME: &'static str = "NotepadPageView";
        type Type = super::PageView;
        type ParentType = gtk::TextView;
    }

    impl ObjectImpl for PageView {}
    impl WidgetImpl for PageView {}

    impl TextViewImpl for PageView {
        fn snapshot_layer(&self, layer: gtk::TextViewLayer, snapshot: gtk::Snapshot) {
            if layer == gtk::TextViewLayer::BelowText
                && let Some(color) = *self.band.borrow()
            {
                self.draw_band(&snapshot, &color);
            }
            self.parent_snapshot_layer(layer, snapshot);
        }
    }

    impl PageView {
        /// Only while nothing is selected: the selection says enough. In buffer coordinates.
        fn draw_band(&self, snapshot: &gtk::Snapshot, color: &gdk::RGBA) {
            let view = self.obj();
            let b = view.buffer();
            if b.has_selection() {
                return;
            }
            let (y, h) = view.line_yrange(&b.iter_at_mark(&b.get_insert()));
            let rect = view.visible_rect();
            let (x0, x1) = if view.wrap_mode() == gtk::WrapMode::None {
                (rect.x(), rect.x() + rect.width())
            } else {
                let x0 = (view.left_margin() - BAND_PAD).max(0);
                (x0, (rect.width() - view.right_margin() + BAND_PAD).clamp(x0, rect.width()))
            };
            snapshot.append_color(color, &gtk::graphene::Rect::new(x0 as f32, y as f32, (x1 - x0) as f32, h as f32));
        }
    }
}

glib::wrapper! {
    pub struct PageView(ObjectSubclass<view_imp::PageView>)
        @extends gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable, gtk::AccessibleText;
}

// ---------- Line numbers ----------

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct LineGutter {
        pub view: glib::WeakRef<gtk::TextView>,
        /// Room for the biggest line number; `fit_width` may make the gutter wider.
        pub min_width: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LineGutter {
        const NAME: &'static str = "NotepadLineGutter";
        type Type = super::LineGutter;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for LineGutter {}

    impl WidgetImpl for LineGutter {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(view) = self.view.upgrade() else { return };
            let widget = self.obj();
            let color = widget.color();
            let width = widget.width() as f32;
            let rect = view.visible_rect();
            let bottom = rect.y() + rect.height();
            // The top margin makes the visible area start above the first line.
            let mut iter = view.iter_at_location(0, rect.y().max(0)).unwrap_or_else(|| view.buffer().start_iter());
            iter.set_line_offset(0);
            let current = view.buffer().iter_at_mark(&view.buffer().get_insert()).line();
            loop {
                let (y, _) = view.line_yrange(&iter);
                if y > bottom {
                    break;
                }
                let (_, wy) = view.buffer_to_window_coords(gtk::TextWindowType::Left, 0, y);
                let layout = widget.create_pango_layout(Some(&(iter.line() + 1).to_string()));
                let (lw, _) = layout.pixel_size();
                snapshot.save();
                snapshot.translate(&gtk::graphene::Point::new(width - 12.0 - lw as f32, wy as f32));
                // The cursor's line number is drawn at full strength.
                let c =
                    if iter.line() == current { gdk::RGBA::new(color.red(), color.green(), color.blue(), 1.0) } else { color };
                snapshot.append_layout(&layout, &c);
                snapshot.restore();
                if !iter.forward_line() {
                    break;
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct LineGutter(ObjectSubclass<imp::LineGutter>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LineGutter {
    /// Wide enough for the biggest line number, and at least two digits.
    fn fit(&self) {
        let Some(view) = self.imp().view.upgrade() else { return };
        let digits = view.buffer().line_count().max(1).to_string().len().max(2);
        let (w, _) = self.create_pango_layout(Some(&"0".repeat(digits))).pixel_size();
        let want = w + 24;
        if self.imp().min_width.replace(want) != want {
            fit_width_soon();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1204), "1,204");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn css_strings_stay_inside_quotes() {
        assert_eq!(css_string("Fira \"Code\"; }"), "Fira Code ");
    }
}
