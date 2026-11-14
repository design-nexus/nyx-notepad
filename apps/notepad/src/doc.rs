//! Open files. Each tab is a `Doc`: a text buffer, the file it came from (if any) and
//! how that file ends its lines. `OPEN` holds them in tab order and `CURRENT` says
//! which one is showing. Nothing holds a borrow of `OPEN` while emitting a change.

use crate::{cmd, paths, prefs, window};
use encoding_rs::{Encoding, UTF_8};
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
    /// The encoding the file was read in, and the one it's saved in.
    opened: Cell<&'static Encoding>,
    encoding: Cell<&'static Encoding>,
    /// Some bytes weren't text in any encoding Notepad recognised and read as �;
    /// saving asks before writing � in their place.
    lossy: Cell<bool>,
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

/// What's kept of a closed tab, so Ctrl+Shift+T (or the toast's Undo) brings it back,
/// also after a restart (the session keeps them a week).
#[derive(Clone)]
pub struct Closed {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub text: String,
    pub dirty: bool,
    pub crlf: bool,
    pub opened: &'static Encoding,
    pub encoding: &'static Encoding,
    pub lossy: bool,
    pub cursor: i32,
    /// When it was closed, in seconds since 1970.
    pub at: i64,
}

/// Closed tabs kept for Ctrl+Shift+T.
const MAX_CLOSED: usize = 20;

impl Closed {
    /// Its text is needed to bring it back (else it's the file at `path`, reopened).
    pub fn keeps_text(&self) -> bool {
        self.dirty || self.path.is_none()
    }
}

/// Look at the closed tabs, oldest first.
pub fn with_closed<R>(f: impl FnOnce(&[Closed]) -> R) -> R {
    CLOSED.with(|c| f(&c.borrow()))
}

/// The closed tabs a restart brought back from the session.
pub fn set_closed(closed: Vec<Closed>) {
    if let Some(max) = closed.iter().map(|c| c.id).max() {
        // Ids also name the closed tabs' kept text, so new tabs don't reuse them.
        NEXT_ID.with(|n| n.set(n.get().max(max + 1)));
    }
    CLOSED.with(|c| *c.borrow_mut() = closed);
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
            opened: Cell::new(UTF_8),
            encoding: Cell::new(UTF_8),
            lossy: Cell::new(false),
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

    pub fn opened_encoding(&self) -> &'static Encoding {
        self.opened.get()
    }

    pub fn encoding(&self) -> &'static Encoding {
        self.encoding.get()
    }

    pub fn lossy(&self) -> bool {
        self.lossy.get()
    }

    /// Save in another encoding. A file then differs from what's on disk.
    pub fn set_encoding(&self, encoding: &'static Encoding) {
        if self.encoding.replace(encoding) == encoding {
            return;
        }
        if self.path().is_some() {
            self.scratch_stale.set(true);
            self.buffer.set_modified(true);
        }
        crate::session::save_soon();
    }

    /// The file was read (or restored) as `encoding`.
    fn set_read_as(&self, opened: &'static Encoding, encoding: &'static Encoding, lossy: bool) {
        self.opened.set(opened);
        self.encoding.set(encoding);
        self.lossy.set(lossy);
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
    Text { text: String, crlf: bool, encoding: &'static Encoding, lossy: bool },
    Failed(String),
}

/// Text from a file's bytes: UTF-8 when they are, else the legacy encoding they most
/// likely use (Windows-1252, Shift_JIS, …). Bytes that fit neither read as � in UTF-8
/// (`lossy`).
pub fn decode(bytes: Vec<u8>) -> (String, &'static Encoding, bool) {
    let bytes = match String::from_utf8(bytes) {
        Ok(text) => return (text, UTF_8, false),
        Err(e) => e.into_bytes(),
    };
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(&bytes, true);
    let guess = detector.guess(None, chardetng::Utf8Detection::Deny);
    match guess.decode_without_bom_handling_and_without_replacement(&bytes) {
        Some(text) => (text.into_owned(), guess, false),
        None => (String::from_utf8_lossy(&bytes).into_owned(), UTF_8, true),
    }
}

/// Why a tab's text can't be saved as it is without asking.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Unsaveable {
    /// Unreadable bytes show as �; saving writes � in their place.
    Lossy,
    /// The file's encoding has no form for some of the characters.
    Unmappable(&'static Encoding),
}

/// `text` as bytes in `encoding`.
pub fn to_encoding(text: &str, encoding: &'static Encoding) -> Result<Vec<u8>, Unsaveable> {
    if encoding == UTF_8 {
        return Ok(text.as_bytes().to_vec());
    }
    let (bytes, _, unmappable) = encoding.encode(text);
    if unmappable { Err(Unsaveable::Unmappable(encoding)) } else { Ok(bytes.into_owned()) }
}

/// The bytes to save `doc` as.
fn bytes(doc: &Doc) -> Result<Vec<u8>, Unsaveable> {
    if doc.lossy() {
        return Err(Unsaveable::Lossy);
    }
    to_encoding(&encode(&doc.text(), doc.crlf(), prefs::get().trim_trailing), doc.encoding())
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
    let (text, encoding, lossy) = decode(bytes);
    let (crlf, text) = split_line_endings(&text);
    Read::Text { text, crlf, encoding, lossy }
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
            Read::Text { text, crlf, encoding, lossy } => {
                doc.load_text(&text, false);
                doc.crlf.set(crlf);
                doc.set_read_as(encoding, encoding, lossy);
                doc.mtime.set(modified_time(&canon));
                if lossy {
                    window::toast(&format!(
                        "Some of {} isn't readable text. It shows as �, and saving asks first.",
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

/// How a file's text was read: see `Doc`'s fields of the same names.
#[derive(Clone, Copy)]
pub struct ReadAs {
    pub opened: &'static Encoding,
    pub encoding: &'static Encoding,
    pub lossy: bool,
}

/// Restore a tab from the session (not shown; the caller picks the current one).
/// `read_as` applies to unsaved text; a file without edits is read again.
pub fn restore(
    id: u64,
    path: Option<PathBuf>,
    unsaved: Option<String>,
    crlf: bool,
    read_as: ReadAs,
    mtime: Option<SystemTime>,
    cursor: i32,
) -> bool {
    let doc = Doc::new(Some(id));
    match (&path, unsaved) {
        (_, Some(text)) => {
            // An untitled tab with text, or a file with edits that weren't saved.
            doc.load_text(&text, true);
            doc.crlf.set(crlf);
            doc.set_read_as(read_as.opened, read_as.encoding, read_as.lossy);
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
            Read::Text { text, crlf, encoding, lossy } => {
                doc.load_text(&text, false);
                doc.crlf.set(crlf);
                doc.set_read_as(encoding, encoding, lossy);
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
        let closed = Closed {
            id: doc.id,
            path: doc.path(),
            text: doc.text(),
            dirty: doc.dirty(),
            crlf: doc.crlf(),
            opened: doc.opened_encoding(),
            encoding: doc.encoding(),
            lossy: doc.lossy(),
            cursor: doc.cursor.get(),
            at: SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64),
        };
        if closed.keeps_text() {
            crate::session::keep_closed_text(&closed);
        }
        CLOSED.with(|c| {
            let mut c = c.borrow_mut();
            c.push(closed);
            if c.len() > MAX_CLOSED {
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
    crate::session::save_soon();
    if !c.keeps_text()
        && let Some(p) = &c.path
    {
        open(p);
        return;
    }
    let doc = Doc::new(None);
    doc.load_text(&c.text, c.dirty);
    doc.crlf.set(c.crlf);
    doc.set_read_as(c.opened, c.encoding, c.lossy);
    doc.cursor.set(c.cursor);
    if let Some(p) = &c.path {
        doc.mtime.set(modified_time(p));
    }
    *doc.path.borrow_mut() = c.path;
    push(doc, true);
}

// ---------- Saving ----------

fn write(doc: &Rc<Doc>, path: &Path, bytes: Vec<u8>) -> bool {
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

/// Save `doc` to `path` (the control interface: no dialog). Text that can't be saved
/// as it is without asking is left for the user to save in the window.
pub fn write_to(doc: &Rc<Doc>, path: &Path) -> Result<(), String> {
    let bytes = bytes(doc).map_err(|why| match why {
        Unsaveable::Lossy => "the file has unreadable bytes (shown as �); save it in Notepad to confirm".to_string(),
        Unsaveable::Unmappable(e) => format!("{} can't hold some of the text; save it in Notepad to choose UTF-8", e.name()),
    })?;
    if write(doc, path, bytes) { Ok(()) } else { Err("couldn't save".into()) }
}

/// Save `doc` to `path`, asking first when its text can't be saved as it is.
fn save_to(doc: Rc<Doc>, path: PathBuf) {
    match bytes(&doc) {
        Ok(bytes) => {
            if write(&doc, &path, bytes) {
                window::toast(&format!("Saved {}", paths::pretty(&path)));
            }
        }
        Err(why) => ask_to_save(doc, path, why),
    }
}

fn ask_to_save(doc: Rc<Doc>, path: PathBuf, why: Unsaveable) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (message, detail, button) = match why {
        Unsaveable::Lossy => (
            format!("Save {name} with � in place of unreadable bytes?"),
            "Some of the file isn't text Notepad can read. It shows as �, and saving writes � in its place for good.".to_string(),
            "Save Anyway",
        ),
        Unsaveable::Unmappable(e) => (
            format!("{name} can't hold all of this text in {}", e.name()),
            format!(
                "Some characters have no {0} form. Saving as UTF-8 keeps every character; programs that expect {0} may show them differently.",
                e.name()
            ),
            "Save as UTF-8",
        ),
    };
    let dialog = gtk::AlertDialog::builder()
        .modal(true)
        .message(message)
        .detail(detail)
        .buttons(["Cancel", button])
        .cancel_button(0)
        .default_button(1)
        .build();
    let Some(win) = window::window() else { return };
    dialog.choose(Some(&win), gio::Cancellable::NONE, move |res| {
        if res != Ok(1) {
            return;
        }
        match why {
            Unsaveable::Lossy => doc.lossy.set(false),
            Unsaveable::Unmappable(_) => doc.set_encoding(UTF_8),
        }
        window::refresh_status();
        save_to(doc, path);
    });
}

pub fn save() {
    let Some(doc) = current() else { return };
    match doc.path() {
        Some(p) => save_to(doc, p),
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
        {
            save_to(doc, path);
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
            if let Read::Text { text, crlf, encoding, lossy } = read_file(&p) {
                let showing = is_current(&d);
                let offset = if showing { d.buffer.cursor_position() } else { d.cursor.get() };
                d.load_text(&text, false);
                d.crlf.set(crlf);
                d.set_read_as(encoding, encoding, lossy);
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
            Read::Text { text, crlf, encoding, lossy } => {
                assert!(crlf && !lossy);
                assert_eq!(encoding, encoding_rs::WINDOWS_1252);
                assert_eq!(text, "café\nok");
            }
            Read::Failed(why) => panic!("{why}"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saves_in_the_encoding_it_read() {
        let original = b"Cr\xe8me br\xfbl\xe9e, na\xefve fa\xe7ade\n".to_vec();
        let (text, encoding, lossy) = decode(original.clone());
        assert_eq!(encoding, encoding_rs::WINDOWS_1252);
        assert!(!lossy);
        assert_eq!(text, "Crème brûlée, naïve façade\n");
        assert_eq!(to_encoding(&text, encoding).unwrap(), original);
        assert_eq!(to_encoding("Crème 😀", encoding), Err(Unsaveable::Unmappable(encoding)));
        assert_eq!(to_encoding("Crème 😀", UTF_8).unwrap(), "Crème 😀".as_bytes());
    }

    #[test]
    fn utf8_stays_utf8() {
        let (text, encoding, lossy) = decode("naïve ✓".as_bytes().to_vec());
        assert_eq!((text.as_str(), encoding, lossy), ("naïve ✓", UTF_8, false));
    }

    #[test]
    fn reads_other_legacy_encodings() {
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("日本語のテキストです。これは長めの文章で、文字コードを判定するためのものです。");
        let (text, encoding, lossy) = decode(sjis.to_vec());
        assert_eq!(encoding, encoding_rs::SHIFT_JIS);
        assert!(!lossy);
        assert!(text.starts_with("日本語"));
    }
}
