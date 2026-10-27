//! The session: which tabs were open, where their cursors were, and recent files
//! (`~/.local/share/nyx-notepad/session.toml`). Unsaved text lives next to it in
//! `scratch/<id>.txt`, so closing the window never loses anything.

use crate::doc::{self, Doc};
use crate::{cmd, paths, prefs};
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
}

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
        if keep && doc::restore(tab.id, tab.path.clone(), unsaved, tab.crlf, tab.mtime.and_then(from_nanos), tab.cursor) {
            if i <= session.active {
                active = n;
            }
            n += 1;
        }
    }
    if n > 0 {
        doc::restored(active);
    }
    clean_scratch();
    n
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
        });
    }
    let session = Session { active: doc::current_index(), tabs, recent: RECENT.with(|r| r.borrow().clone()) };
    if let Ok(text) = toml::to_string_pretty(&session) {
        let _ = cmd::atomic_write(&paths::session_file(), text);
    }
}

/// A tab is gone for good: drop its unsaved text.
pub fn forget(d: &Doc) {
    let _ = std::fs::remove_file(scratch_file(d.id));
}

/// Remove scratch files no open tab refers to.
fn clean_scratch() {
    let ids: Vec<u64> = doc::all().iter().map(|d| d.id).collect();
    let Ok(rd) = std::fs::read_dir(paths::scratch_dir()) else { return };
    for e in rd.flatten() {
        let id = e.path().file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u64>().ok());
        if id.is_none_or(|id| !ids.contains(&id)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
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
                },
                Tab { id: 7, path: None, unsaved: true, crlf: false, mtime: None, cursor: 0 },
            ],
            recent: vec!["/tmp/a.txt".into()],
        };
        let text = toml::to_string_pretty(&s).unwrap();
        assert_eq!(parse(&text), s);
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
