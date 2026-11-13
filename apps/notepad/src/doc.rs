//! Open files. Each tab is a `Doc`: a text buffer, the file it came from (if any) and
//! how that file ends its lines. `OPEN` holds them in tab order and `CURRENT` says
//! which one is showing. Nothing holds a borrow of `OPEN` while emitting a change.

use crate::{cmd, paths, prefs, window};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

/// Files bigger than this are refused: a text view gets very slow well before.
const MAX_BYTES: u64 = 24 * 1024 * 1024;

pub struct Doc {
    /// Names this tab's scratch file (its unsaved text) in the session.
    pub id: u64,
    path: RefCell<Option<PathBuf>>,
    pub buffer: gtk::TextBuffer,
    crlf: Cell<bool>,
    /// The file's modification time when it was last read or written.
    mtime: Cell<Option<SystemTime>>,
    /// Told the user the file changed on disk under unsaved edits (once).
    warned: Cell<bool>,
    /// The cursor and scroll position, kept while another tab shows.
    pub cursor: Cell<i32>,
    pub scroll: Cell<f64>,
    /// The scratch copy of the text is out of date.
    pub scratch_stale: Cell<bool>,
    /// The untitled name last shown, to rename the tab only when it changes.
    shown_name: RefCell<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    /// Tabs were added, removed or renamed.
    Tabs,
    /// Another tab is showing.
    Current,
    /// The showing tab's saved state changed.
    Dirty,
}

/// What's kept of a closed tab, so Ctrl+Shift+T (or the toast's Undo) brings it back.
struct Closed {
    path: Option<PathBuf>,
    text: String,
    dirty: bool,
    crlf: bool,
    cursor: i32,
}

type Listener = Box<dyn Fn(Change)>;

thread_local! {
    static OPEN: RefCell<Vec<Rc<Doc>>> = const { RefCell::new(Vec::new()) };
    static CURRENT: Cell<usize> = const { Cell::new(0) };
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static CLOSED: RefCell<Vec<Closed>> = const { RefCell::new(Vec::new()) };
    static LISTENERS: RefCell<Vec<(glib::WeakRef<gtk::Widget>, Listener)>> = const { RefCell::new(Vec::new()) };
    /// One tag table for every buffer, so the search colour changes in one place.
    static TAGS: gtk::TextTagTable = make_tags();
}

fn make_tags() -> gtk::TextTagTable {
    let table = gtk::TextTagTable::new();
    table.add(&gtk::TextTag::new(Some("match")));
    table
}

pub fn tags() -> gtk::TextTagTable {
    TAGS.with(|t| t.clone())
}

impl Doc {
    fn new(id: Option<u64>) -> Rc<Doc> {
        let id = id.unwrap_or_else(|| NEXT_ID.with(|n| n.get()));
        NEXT_ID.with(|n| n.set(n.get().max(id + 1)));
        let buffer = gtk::TextBuffer::new(Some(&tags()));
        buffer.set_enable_undo(true);
        let doc = Rc::new(Doc {
            id,
            path: RefCell::new(None),
            buffer,
            crlf: Cell::new(prefs::get().line_ending == "crlf"),
            mtime: Cell::new(None),
            warned: Cell::new(false),
            cursor: Cell::new(0),
            scroll: Cell::new(0.0),
            scratch_stale: Cell::new(false),
            shown_name: RefCell::new("Untitled".into()),
        });
        let weak = Rc::downgrade(&doc);
        doc.buffer.connect_modified_changed(move |_| {
            if let Some(d) = weak.upgrade() {
                emit(Change::Tabs);
                if is_current(&d) {
                    emit(Change::Dirty);
                }
            }
        });
        let weak = Rc::downgrade(&doc);
        doc.buffer.connect_changed(move |_| {
            if let Some(d) = weak.upgrade() {
                d.scratch_stale.set(true);
                // An untitled tab is named after its first line.
                if d.path().is_none() {
                    let name = d.title();
                    if *d.shown_name.borrow() != name {
                        *d.shown_name.borrow_mut() = name;
                        emit(Change::Tabs);
                        if is_current(&d) {
                            emit(Change::Dirty);
                        }
                    }
                }
                crate::session::save_soon();
            }
        });
        doc
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.path.borrow().clone()
    }

    pub fn dirty(&self) -> bool {
        self.buffer.is_modified()
    }

    pub fn crlf(&self) -> bool {
        self.crlf.get()
    }

    /// Save with other line endings. A file then differs from what's on disk.
    pub fn set_crlf(&self, crlf: bool) {
        if self.crlf.replace(crlf) == crlf {
            return;
        }
        if self.path().is_some() {
            self.scratch_stale.set(true);
            self.buffer.set_modified(true);
        }
        crate::session::save_soon();
    }

    pub fn mtime(&self) -> Option<SystemTime> {
        self.mtime.get()
    }

    pub fn text(&self) -> String {
        let (s, e) = self.buffer.bounds();
        self.buffer.text(&s, &e, true).to_string()
    }

    /// The tab's name: the file name, or the first line of an untitled tab.
    pub fn title(&self) -> String {
        match self.path() {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
            None => untitled_name(&self.first_line()),
        }
    }

    fn first_line(&self) -> String {
        let start = self.buffer.start_iter();
        let mut end = start;
        // Only the first line is needed; don't copy a whole big buffer for it.
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        self.buffer.text(&start, &end, false).to_string()
    }

    /// Replace the text without leaving an undo step behind.
    fn load_text(&self, text: &str, modified: bool) {
        self.buffer.begin_irreversible_action();
        self.buffer.set_text(text);
        self.buffer.end_irreversible_action();
        self.buffer.place_cursor(&self.buffer.start_iter());
        self.buffer.set_modified(modified);
    }
}

/// "Untitled", or the first words typed in an untitled tab.
pub fn untitled_name(first_line: &str) -> String {
    let line = first_line.trim();
    if line.is_empty() {
        return "Untitled".into();
    }
    let mut name: String = line.chars().take(22).collect();
    if line.chars().count() > 22 {
        name = name.trim_end().to_string();
        name.push('…');
    }
    name
}

/// Text as read from disk: whether it uses CRLF, and the text with plain `\n` endings.
pub fn split_line_endings(text: &str) -> (bool, String) {
    if text.contains("\r\n") { (true, text.replace("\r\n", "\n")) } else { (false, text.to_string()) }
}

/// The bytes to write for `text`.
pub fn encode(text: &str, crlf: bool, trim_trailing: bool) -> String {
    let mut out = if trim_trailing {
        text.split('\n').map(|l| l.trim_end_matches([' ', '\t'])).collect::<Vec<_>>().join("\n")
    } else {
        text.to_string()
    };
    if crlf {
        out = out.replace('\n', "\r\n");
    }
    out
}

fn modified_time(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

pub enum Read {
    Text { text: String, crlf: bool, lossy: bool },
    Failed(String),
}

/// Read a text file, refusing binaries and huge files with a reason to show.
pub fn read_file(path: &Path) -> Read {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => return Read::Failed(format!("{name} is a folder.")),
        Ok(m) if m.len() > MAX_BYTES => return Read::Failed(format!("{name} is too big to open here.")),
        Err(e) => return Read::Failed(format!("Couldn't open {name}: {e}")),
        _ => {}
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return Read::Failed(format!("Couldn't open {name}: {e}")),
    };
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Read::Failed(format!("{name} looks like a binary file, not text."));
    }
    let (text, lossy) = match String::from_utf8(bytes) {
        Ok(t) => (t, false),
        Err(e) => (String::from_utf8_lossy(e.as_bytes()).into_owned(), true),
    };
    let (crlf, text) = split_line_endings(&text);
    Read::Text { text, crlf, lossy }
}

// ---------- The list of tabs ----------

pub fn all() -> Vec<Rc<Doc>> {
    OPEN.with(|o| o.borrow().clone())
}

pub fn current() -> Option<Rc<Doc>> {
    let i = CURRENT.with(|c| c.get());
    OPEN.with(|o| o.borrow().get(i).cloned())
}

pub fn current_index() -> usize {
    CURRENT.with(|c| c.get())
}

fn is_current(d: &Rc<Doc>) -> bool {
    current().is_some_and(|c| Rc::ptr_eq(&c, d))
}

fn index_of(d: &Rc<Doc>) -> Option<usize> {
    OPEN.with(|o| o.borrow().iter().position(|x| Rc::ptr_eq(x, d)))
}

pub fn subscribe(owner: &impl IsA<gtk::Widget>, f: impl Fn(Change) + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push((owner.upcast_ref::<gtk::Widget>().downgrade(), Box::new(f))));
}

pub fn emit(change: Change) {
    // Take the listeners out while calling them, so one can subscribe or emit in turn.
    let listeners = LISTENERS.with(|l| {
        let mut l = l.borrow_mut();
        l.retain(|(w, _)| w.upgrade().is_some());
        std::mem::take(&mut *l)
    });
    for (_, f) in &listeners {
        f(change);
    }
    LISTENERS.with(|l| {
        let mut l = l.borrow_mut();
        let added = std::mem::take(&mut *l);
        *l = listeners;
        l.extend(added);
    });
}

pub fn switch_to(i: usize) {
    let n = OPEN.with(|o| o.borrow().len());
    if n == 0 {
        return;
    }
    let i = i.min(n - 1);
    if i == current_index() && n > 0 {
        emit(Change::Current);
        return;
    }
    crate::editor::remember_position();
    CURRENT.with(|c| c.set(i));
    emit(Change::Current);
}

/// Move through the tabs, wrapping around.
pub fn cycle(step: i32) {
    let n = OPEN.with(|o| o.borrow().len()) as i32;
    if n > 1 {
        switch_to((current_index() as i32 + step).rem_euclid(n) as usize);
    }
}

fn push(doc: Rc<Doc>, show: bool) {
    // A fresh, empty Untitled tab is replaced by the next file opened.
    let replace = current().filter(|c| c.path().is_none() && !c.dirty() && c.buffer.char_count() == 0 && show);
    let at = OPEN.with(|o| {
        let mut o = o.borrow_mut();
        match replace.as_ref().and_then(|r| o.iter().position(|x| Rc::ptr_eq(x, r))) {
            Some(i) => {
                o[i] = doc;
                i
            }
            None => {
                let at = if o.is_empty() { 0 } else { (current_index() + 1).min(o.len()) };
                o.insert(at, doc);
                at
            }
        }
    });
    if let Some(r) = replace {
        crate::session::forget(&r);
    }
    emit(Change::Tabs);
    if show {
        crate::editor::remember_position();
        CURRENT.with(|c| c.set(at));
        emit(Change::Current);
    } else if at <= current_index() && OPEN.with(|o| o.borrow().len()) > 1 {
        CURRENT.with(|c| c.set(current_index() + 1));
    }
    crate::session::save_soon();
}

/// A new, empty tab.
pub fn new_tab() {
    push(Doc::new(None), true);
}

/// Open a file in a tab, or show its tab if it's already open.
pub fn open(path: &Path) {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(i) = OPEN.with(|o| o.borrow().iter().position(|d| d.path().as_deref() == Some(canon.as_path()))) {
        switch_to(i);
        return;
    }
    let doc = Doc::new(None);
    if canon.exists() {
        match read_file(&canon) {
            Read::Text { text, crlf, lossy } => {
                doc.load_text(&text, false);
                doc.crlf.set(crlf);
                doc.mtime.set(modified_time(&canon));
                if lossy {
                    window::toast(&format!(
                        "{} isn't valid UTF-8. Odd characters show as �, and saving will keep them that way.",
                        canon.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
            }
            Read::Failed(why) => {
                window::toast(&why);
                return;
            }
        }
    }
    // A path that doesn't exist yet opens empty and is created on the first save.
    *doc.path.borrow_mut() = Some(canon.clone());
    crate::session::add_recent(&canon);
    push(doc, true);
}

/// Restore a tab from the session (not shown; the caller picks the current one).
pub fn restore(
    id: u64,
    path: Option<PathBuf>,
    unsaved: Option<String>,
    crlf: bool,
    mtime: Option<SystemTime>,
    cursor: i32,
) -> bool {
    let doc = Doc::new(Some(id));
    match (&path, unsaved) {
        (_, Some(text)) => {
            // An untitled tab with text, or a file with edits that weren't saved.
            doc.load_text(&text, true);
            doc.crlf.set(crlf);
            doc.mtime.set(mtime);
            if let Some(p) = &path
                && p.exists()
                && modified_time(p) != mtime
            {
                doc.warned.set(true);
                window::toast(&format!(
                    "{} changed on disk since you edited it. Your unsaved version is open.",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
        (Some(p), None) => match read_file(p) {
            Read::Text { text, crlf, .. } => {
                doc.load_text(&text, false);
                doc.crlf.set(crlf);
                doc.mtime.set(modified_time(p));
            }
            Read::Failed(_) => return false,
        },
        (None, None) => {}
    }
    *doc.path.borrow_mut() = path;
    doc.scratch_stale.set(true);
    doc.cursor.set(cursor);
    OPEN.with(|o| o.borrow_mut().push(doc));
    true
}

/// After restoring, show tab `i` and tell everyone.
pub fn restored(i: usize) {
    let n = OPEN.with(|o| o.borrow().len());
    CURRENT.with(|c| c.set(i.min(n.saturating_sub(1))));
    emit(Change::Tabs);
    emit(Change::Current);
}

/// Close a tab. Unsaved text is dropped, with a toast whose Undo brings it back.
pub fn close(doc: &Rc<Doc>) {
    let Some(i) = index_of(doc) else { return };
    crate::editor::remember_position();
    let empty_untitled = doc.path().is_none() && doc.buffer.char_count() == 0;
    if !empty_untitled {
        CLOSED.with(|c| {
            let mut c = c.borrow_mut();
            c.push(Closed { path: doc.path(), text: doc.text(), dirty: doc.dirty(), crlf: doc.crlf(), cursor: doc.cursor.get() });
            if c.len() > 20 {
                c.remove(0);
            }
        });
    }
    let was_current = i == current_index();
    let n = OPEN.with(|o| {
        let mut o = o.borrow_mut();
        o.remove(i);
        o.len()
    });
    crate::session::forget(doc);
    if n == 0 {
        // There's always a tab to type in.
        CURRENT.with(|c| c.set(0));
        push(Doc::new(None), true);
    } else {
        let cur = current_index();
        if was_current {
            CURRENT.with(|c| c.set(i.min(n - 1)));
        } else if i < cur {
            CURRENT.with(|c| c.set(cur - 1));
        }
        emit(Change::Tabs);
        emit(Change::Current);
    }
    if doc.dirty() && !empty_untitled {
        window::toast_with_action(&format!("Closed {} without saving", doc.title()), "Undo", reopen_closed);
    }
    crate::session::save_soon();
}

pub fn close_current() {
    if let Some(d) = current() {
        close(&d);
    }
}

/// Bring back the tab closed last.
pub fn reopen_closed() {
    let Some(c) = CLOSED.with(|c| c.borrow_mut().pop()) else {
        window::toast("No closed tabs to reopen.");
        return;
    };
    if let Some(p) = &c.path
        && !c.dirty
    {
        open(p);
        return;
    }
    let doc = Doc::new(None);
    doc.load_text(&c.text, c.dirty);
    doc.crlf.set(c.crlf);
    doc.cursor.set(c.cursor);
    if let Some(p) = &c.path {
        doc.mtime.set(modified_time(p));
    }
    *doc.path.borrow_mut() = c.path;
    push(doc, true);
}

// ---------- Saving ----------

fn write(doc: &Rc<Doc>, path: &Path) -> bool {
    let p = prefs::get();
    let bytes = encode(&doc.text(), doc.crlf(), p.trim_trailing);
    match cmd::atomic_write(path, bytes) {
        Ok(()) => {
            let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            let renamed = doc.path().as_deref() != Some(canon.as_path());
            *doc.path.borrow_mut() = Some(canon.clone());
            doc.mtime.set(modified_time(&canon));
            doc.warned.set(false);
            doc.buffer.set_modified(false);
            crate::session::add_recent(&canon);
            if renamed {
                emit(Change::Tabs);
                emit(Change::Dirty);
            }
            crate::session::save_soon();
            true
        }
        Err(e) => {
            window::toast(&format!("Couldn't save: {e:#}"));
            false
        }
    }
}

/// Save `doc` to `path` (the control interface: no dialog, no toast).
pub fn write_to(doc: &Rc<Doc>, path: &Path) -> bool {
    write(doc, path)
}

pub fn save() {
    let Some(doc) = current() else { return };
    match doc.path() {
        Some(p) => {
            if write(&doc, &p) {
                window::toast(&format!("Saved {}", paths::pretty(&p)));
            }
        }
        None => save_as(),
    }
}

pub fn save_as() {
    let Some(doc) = current() else { return };
    let dialog = gtk::FileDialog::builder().title("Save as").modal(true).build();
    match doc.path() {
        Some(p) => dialog.set_initial_file(Some(&gio::File::for_path(&p))),
        None => {
            let name = doc.title();
            let name = if name == "Untitled" {
                "untitled.txt".to_string()
            } else {
                format!("{}.txt", name.trim_end_matches('…').trim())
            };
            dialog.set_initial_name(Some(&sanitize(&name)));
            if let Some(dir) = last_dir() {
                dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
            }
        }
    }
    let Some(win) = window::window() else { return };
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        if let Ok(file) = res
            && let Some(path) = file.path()
            && write(&doc, &path)
        {
            window::toast(&format!("Saved {}", paths::pretty(&path)));
        }
    });
}

/// File names can't hold slashes; untitled names come from typed text.
fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c == '/' || c == '\0' { '-' } else { c }).collect()
}

/// The folder of the file showing, else the last one opened.
fn last_dir() -> Option<PathBuf> {
    current()
        .and_then(|d| d.path())
        .or_else(|| crate::session::recent().into_iter().next())
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

pub fn open_dialog() {
    let dialog = gtk::FileDialog::builder().title("Open").modal(true).build();
    if let Some(dir) = last_dir() {
        dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
    }
    let Some(win) = window::window() else { return };
    dialog.open_multiple(Some(&win), gio::Cancellable::NONE, |res| {
        let Ok(files) = res else { return };
        for f in files.iter::<gio::File>().flatten() {
            if let Some(p) = f.path() {
                open(&p);
            }
        }
    });
}

// ---------- Watching files ----------

/// Every couple of seconds, pick up changes other programs made to open files: a tab
/// without edits reloads (keeping its place); a tab with edits says so once.
pub fn watch() {
    glib::timeout_add_seconds_local(2, || {
        for d in all() {
            let Some(p) = d.path() else { continue };
            let now = modified_time(&p);
            if now.is_none() || now == d.mtime() {
                continue;
            }
            if d.dirty() {
                if !d.warned.replace(true) {
                    window::toast(&format!("{} changed on disk. Saving will replace that version with yours.", d.title()));
                }
                continue;
            }
            if let Read::Text { text, crlf, .. } = read_file(&p) {
                let showing = is_current(&d);
                let offset = if showing { d.buffer.cursor_position() } else { d.cursor.get() };
                d.load_text(&text, false);
                d.crlf.set(crlf);
                d.buffer.place_cursor(&d.buffer.iter_at_offset(offset));
                d.cursor.set(offset);
            }
            d.mtime.set(now);
        }
        glib::ControlFlow::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_untitled_tabs() {
        assert_eq!(untitled_name(""), "Untitled");
        assert_eq!(untitled_name("   "), "Untitled");
        assert_eq!(untitled_name("  shopping list "), "shopping list");
        assert_eq!(untitled_name("a very long first line that goes on and on"), "a very long first line…");
    }

    #[test]
    fn keeps_line_endings() {
        let (crlf, text) = split_line_endings("one\r\ntwo\r\n");
        assert!(crlf);
        assert_eq!(text, "one\ntwo\n");
        assert_eq!(encode(&text, crlf, false), "one\r\ntwo\r\n");
        let (crlf, text) = split_line_endings("one\ntwo");
        assert!(!crlf);
        assert_eq!(encode(&text, crlf, false), "one\ntwo");
    }

    #[test]
    fn trims_trailing_whitespace() {
        assert_eq!(encode("a  \nb\t\n  c", false, true), "a\nb\n  c");
        assert_eq!(encode("a  \nb", true, true), "a\r\nb");
    }

    #[test]
    fn refuses_binary_files() {
        let dir = std::env::temp_dir().join(format!("notepad-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("x.bin");
        std::fs::write(&bin, [0x7f, b'E', b'L', b'F', 0, 1]).unwrap();
        assert!(matches!(read_file(&bin), Read::Failed(_)));
        let txt = dir.join("x.txt");
        std::fs::write(&txt, b"caf\xe9\r\nok").unwrap();
        match read_file(&txt) {
            Read::Text { text, crlf, lossy } => {
                assert!(crlf && lossy);
                assert_eq!(text, "caf\u{fffd}\nok");
            }
            Read::Failed(why) => panic!("{why}"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
