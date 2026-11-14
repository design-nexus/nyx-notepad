//! The session: which tabs were open, where their cursors were, and recent files
//! (`~/.local/share/nyx-notepad/session.toml`). Unsaved text lives next to it in
//! `scratch/<id>.txt`, so closing the window never loses anything.

use crate::doc::{self, Closed, Doc};
use crate::{cmd, paths, prefs};
use encoding_rs::{Encoding, UTF_8};
use gtk::glib;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_RECENT: usize = 12;

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Session {
    pub active: usize,
    pub tabs: Vec<Tab>,
    pub recent: Vec<PathBuf>,
    /// Closed tabs Ctrl+Shift+T can bring back, oldest first.
    pub closed: Vec<ClosedTab>,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClosedTab {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Closed with edits that weren't saved; with those, and untitled text, the text
    /// is in `scratch/closed/<id>.txt`.
    pub dirty: bool,
    pub crlf: bool,
    #[serde(flatten)]
    pub read_as: Encodings,
    pub cursor: i32,
    /// When it was closed, in seconds since 1970.
    pub at: i64,
}

/// A tab's encodings (encoding_rs names), left out when UTF-8.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Encodings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opened: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub lossy: bool,
}

impl Encodings {
    fn of(opened: &'static Encoding, encoding: &'static Encoding, lossy: bool) -> Encodings {
        let name = |e: &'static Encoding| (e != UTF_8).then(|| e.name().to_string());
        Encodings { opened: name(opened), encoding: name(encoding), lossy }
    }

    fn read_as(&self) -> doc::ReadAs {
        let enc = |n: &Option<String>| n.as_deref().and_then(|n| Encoding::for_label(n.as_bytes())).unwrap_or(UTF_8);
        doc::ReadAs { opened: enc(&self.opened), encoding: enc(&self.encoding), lossy: self.lossy }
    }
}

/// Closed tabs are kept this long, in seconds.
const KEEP_CLOSED: i64 = 7 * 24 * 60 * 60;

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Tab {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// The text is in `scratch/<id>.txt` (edits not saved to `path`, or untitled text).
    pub unsaved: bool,
    pub crlf: bool,
    /// The file's modification time when last read or saved, in nanoseconds since 1970.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    pub cursor: i32,
    #[serde(flatten)]
    pub read_as: Encodings,
}

thread_local! {
    static RECENT: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    static PENDING: Cell<Option<glib::SourceId>> = const { Cell::new(None) };
}

fn to_nanos(t: SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH).ok().and_then(|d| i64::try_from(d.as_nanos()).ok())
}

fn from_nanos(n: i64) -> Option<SystemTime> {
    u64::try_from(n).ok().map(|n| UNIX_EPOCH + Duration::from_nanos(n))
}

fn scratch_file(id: u64) -> PathBuf {
    paths::scratch_dir().join(format!("{id}.txt"))
}

pub fn parse(text: &str) -> Session {
    toml::from_str(text).unwrap_or_default()
}

fn load() -> Session {
    std::fs::read_to_string(paths::session_file()).map(|t| parse(&t)).unwrap_or_default()
}

/// Reopen last time's tabs. Returns how many came back.
pub fn restore() -> usize {
    let session = load();
    RECENT.with(|r| *r.borrow_mut() = session.recent.clone());
    let reopen_files = prefs::get().restore_tabs;
    let mut active = 0;
    let mut n = 0;
    for (i, tab) in session.tabs.iter().enumerate() {
        let unsaved = if tab.unsaved { std::fs::read_to_string(scratch_file(tab.id)).ok() } else { None };
        // Clean files come back only when asked to; empty untitled tabs never do.
        let keep = match (&tab.path, &unsaved) {
            (_, Some(_)) => true,
            (Some(p), None) => reopen_files && p.exists(),
            (None, None) => false,
        };
        if keep
            && doc::restore(
                tab.id,
                tab.path.clone(),
                unsaved,
                tab.crlf,
                tab.read_as.read_as(),
                tab.mtime.and_then(from_nanos),
                tab.cursor,
            )
        {
            if i <= session.active {
                active = n;
            }
            n += 1;
        }
    }
    doc::set_closed(closed_tabs(session.closed, now_secs()));
    if n > 0 {
        doc::restored(active);
    }
    clean_scratch();
    n
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn closed_file(id: u64) -> PathBuf {
    paths::scratch_dir().join("closed").join(format!("{id}.txt"))
}

/// The closed tabs still worth bringing back: a week old at most, with their text.
fn closed_tabs(saved: Vec<ClosedTab>, now: i64) -> Vec<Closed> {
    saved
        .into_iter()
        .filter(|c| now - c.at < KEEP_CLOSED)
        .filter_map(|c| {
            let read_as = c.read_as.read_as();
            let mut closed = Closed {
                id: c.id,
                path: c.path,
                text: String::new(),
                dirty: c.dirty,
                crlf: c.crlf,
                opened: read_as.opened,
                encoding: read_as.encoding,
                lossy: read_as.lossy,
                cursor: c.cursor,
                at: c.at,
            };
            if closed.keeps_text() {
                closed.text = std::fs::read_to_string(closed_file(c.id)).ok()?;
            }
            Some(closed)
        })
        .collect()
}

/// Keep a closed tab's text until it's reopened or ages out.
pub fn keep_closed_text(c: &Closed) {
    if let Err(e) = cmd::atomic_write(&closed_file(c.id), &c.text) {
        eprintln!("nyx-notepad: couldn't keep a closed tab's text: {e:#}");
    }
}

/// Write the session about a second after the last change.
pub fn save_soon() {
    if let Some(id) = PENDING.with(|p| p.take()) {
        id.remove();
    }
    let id = glib::timeout_add_local_once(Duration::from_millis(1000), || {
        PENDING.with(|p| p.set(None));
        save_now();
    });
    PENDING.with(|p| p.set(Some(id)));
}

pub fn save_now() {
    if let Some(id) = PENDING.with(|p| p.take()) {
        id.remove();
    }
    crate::editor::remember_position();
    let docs = doc::all();
    let mut tabs = Vec::new();
    for d in &docs {
        let keep_text = d.dirty() || (d.path().is_none() && d.buffer.char_count() > 0);
        let file = scratch_file(d.id);
        if keep_text {
            if d.scratch_stale.get() || !file.exists() {
                match cmd::atomic_write(&file, d.text()) {
                    Ok(()) => d.scratch_stale.set(false),
                    Err(e) => eprintln!("nyx-notepad: couldn't keep unsaved text: {e:#}"),
                }
            }
        } else if file.exists() {
            let _ = std::fs::remove_file(&file);
        }
        tabs.push(Tab {
            id: d.id,
            path: d.path(),
            unsaved: keep_text,
            crlf: d.crlf(),
            mtime: d.mtime().and_then(to_nanos),
            cursor: d.cursor.get(),
            read_as: Encodings::of(d.opened_encoding(), d.encoding(), d.lossy()),
        });
    }
    let now = now_secs();
    let closed: Vec<ClosedTab> = doc::with_closed(|closed| {
        closed
            .iter()
            .filter(|c| now - c.at < KEEP_CLOSED)
            .map(|c| ClosedTab {
                id: c.id,
                path: c.path.clone(),
                dirty: c.dirty,
                crlf: c.crlf,
                read_as: Encodings::of(c.opened, c.encoding, c.lossy),
                cursor: c.cursor,
                at: c.at,
            })
            .collect()
    });
    let kept: Vec<u64> = doc::with_closed(|c| c.iter().filter(|c| c.keeps_text()).map(|c| c.id).collect());
    remove_files_except(&paths::scratch_dir().join("closed"), &kept);
    let session = Session { active: doc::current_index(), tabs, recent: RECENT.with(|r| r.borrow().clone()), closed };
    if let Ok(text) = toml::to_string_pretty(&session) {
        let _ = cmd::atomic_write(&paths::session_file(), text);
    }
}

/// Remove the `<id>.txt` files in `dir` whose id isn't in `ids`.
fn remove_files_except(dir: &Path, ids: &[u64]) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let id = e.path().file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u64>().ok());
        if id.is_none_or(|id| !ids.contains(&id)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// A tab is gone for good: drop its unsaved text.
pub fn forget(d: &Doc) {
    let _ = std::fs::remove_file(scratch_file(d.id));
}

/// Remove scratch files no open tab refers to.
fn clean_scratch() {
    let ids: Vec<u64> = doc::all().iter().map(|d| d.id).collect();
    remove_files_except(&paths::scratch_dir(), &ids);
}

pub fn add_recent(path: &Path) {
    RECENT.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|p| p != path);
        r.insert(0, path.to_path_buf());
        r.truncate(MAX_RECENT);
    });
}

/// Recently opened or saved files that still exist, newest first.
pub fn recent() -> Vec<PathBuf> {
    RECENT.with(|r| r.borrow().iter().filter(|p| p.exists()).cloned().collect())
}

pub fn clear_recent() {
    RECENT.with(|r| r.borrow_mut().clear());
    save_soon();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trips() {
        let s = Session {
            active: 1,
            tabs: vec![
                Tab {
                    id: 1,
                    path: Some("/tmp/a.txt".into()),
                    unsaved: false,
                    crlf: true,
                    mtime: Some(1_700_000_000_123_456_789),
                    cursor: 42,
                    read_as: Encodings { opened: Some("windows-1252".into()), encoding: None, lossy: false },
                },
                Tab { id: 7, path: None, unsaved: true, crlf: false, mtime: None, cursor: 0, read_as: Encodings::default() },
            ],
            recent: vec!["/tmp/a.txt".into()],
            closed: vec![ClosedTab {
                id: 9,
                path: Some("/tmp/b.txt".into()),
                dirty: true,
                crlf: false,
                read_as: Encodings { opened: None, encoding: None, lossy: true },
                cursor: 3,
                at: 1_700_000_000,
            }],
        };
        let text = toml::to_string_pretty(&s).unwrap();
        assert_eq!(parse(&text), s);
        let read_as = s.tabs[0].read_as.read_as();
        assert_eq!((read_as.opened, read_as.encoding), (encoding_rs::WINDOWS_1252, UTF_8));
    }

    #[test]
    fn an_old_session_without_encodings_reads() {
        let s = parse("active = 0\n[[tabs]]\nid = 1\npath = \"/tmp/a.txt\"\nunsaved = false\ncrlf = false\ncursor = 0\n");
        assert_eq!(s.tabs.len(), 1);
        assert_eq!(s.tabs[0].read_as, Encodings::default());
        assert!(s.closed.is_empty());
    }

    #[test]
    fn bad_session_is_empty() {
        assert_eq!(parse("tabs = 3"), Session::default());
    }

    #[test]
    fn times_round_trip() {
        let t = UNIX_EPOCH + Duration::from_nanos(1_700_000_000_123_456_789);
        assert_eq!(from_nanos(to_nanos(t).unwrap()), Some(t));
    }
}
