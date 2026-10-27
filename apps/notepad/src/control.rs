//! Notepad's tools on the control interface (`nyx_core::control`), for Nyx Assistant:
//! open, read and write tabs directly instead of typing into the window.

use crate::doc;
use gtk::prelude::*;
use nyx_core::control::{self, text};
use serde_json::{json, Value};

pub fn tools() -> Value {
    json!([
        {"name": "notepad.new_tab", "description": "Open a new, empty Notepad tab.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "notepad.open", "description": "Open a text file (inside the home folder) in a Notepad tab.", "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}},
        {"name": "notepad.read", "description": "Read the current Notepad tab: name, file and text.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "notepad.tabs", "description": "List Notepad's tabs: name, file, unsaved changes.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "notepad.insert_text", "description": "Insert text into the current Notepad tab, at the cursor or at the end.", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}, "at": {"enum": ["cursor", "end"]}}, "required": ["text"]}},
        {"name": "notepad.save", "description": "Save the current Notepad tab; give a path (inside the home folder) for an untitled tab or to save under a new name.", "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}, "overwrite": {"type": "boolean"}}}},
    ])
}

fn tab(d: &doc::Doc) -> Value {
    json!({"name": d.title(), "path": d.path(), "unsaved": d.dirty()})
}

pub fn handle(name: &str, args: Value) -> Result<Value, String> {
    match name {
        "notepad.new_tab" => {
            doc::new_tab();
            Ok(json!({"tab": doc::current().map(|d| tab(&d))}))
        }
        "notepad.open" => {
            let path = control::home_path(&text(&args, "path")?)?;
            if path.is_dir() {
                return Err("that's a folder".into());
            }
            doc::open(&path);
            let d = doc::current().ok_or("no tab")?;
            if d.path().as_deref() != Some(path.as_path()) {
                return Err("couldn't open it".into());
            }
            Ok(json!({"tab": tab(&d)}))
        }
        "notepad.read" => {
            let d = doc::current().ok_or("no tab is open")?;
            Ok(json!({"tab": tab(&d), "text": d.text(), "untrusted_content": true}))
        }
        "notepad.tabs" => Ok(json!({"tabs": doc::all().iter().map(|d| tab(d)).collect::<Vec<_>>(), "current": doc::current_index()})),
        "notepad.insert_text" => {
            let insert = text(&args, "text")?;
            if insert.is_empty() || insert.len() > 100_000 {
                return Err("text must be 1 to 100000 bytes".into());
            }
            let d = match doc::current() {
                Some(d) => d,
                None => {
                    doc::new_tab();
                    doc::current().ok_or("no tab")?
                }
            };
            let buffer = &d.buffer;
            let mut at = if args["at"] == "end" { buffer.end_iter() } else { buffer.iter_at_mark(&buffer.get_insert()) };
            buffer.begin_user_action();
            buffer.insert(&mut at, &insert);
            buffer.end_user_action();
            buffer.place_cursor(&at);
            Ok(json!({"tab": tab(&d), "inserted": insert.chars().count()}))
        }
        "notepad.save" => {
            let d = doc::current().ok_or("no tab is open")?;
            let path = match args["path"].as_str() {
                Some(_) => control::save_target(&args)?,
                None => d.path().ok_or("this tab has no file yet; give a path")?,
            };
            if !doc::write_to(&d, &path) {
                return Err("couldn't save".into());
            }
            Ok(json!({"tab": tab(&d), "saved": path}))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}
