# Notepad — notes for working on this repo

- The command, crate and config/data folders are all `nyx-notepad`.
  App id `io.github.design_nexus.nyx.Notepad`.
- GTK4 (gtk4-rs 0.11) + Rust, no libadwaita, no GtkSourceView. Built to
  `STYLE.md`. The window is one flat, monospace surface split by hairlines: the tab
  bar (file menu, the tabs or with one tab its name, find, settings), the find and
  replace strip while finding, the editor and a status bar (`F1 Shortcuts` · line, column, words, line
  endings). There is no sidebar. Settings is a card over the window
  (`settings_dialog.rs`, groups built in `sections/settings.rs`). The theme, widgets and settings card come from the `nyx-*` crates. Every colour is a `@theme_*` token; the text tags'
  colours (current line, matches) are mixed from `theme::palette()` in `editor::recolor`.
- **Tabs** (`doc.rs`): `OPEN` holds each `Doc` (a `TextBuffer` sharing one tag table, its
  path, CRLF flag and mtime) in tab order; `CURRENT` the one showing. Changes go out as
  `doc::Change` (Tabs / Current / Dirty); never hold an `OPEN` borrow while emitting.
  "Unsaved" is the buffer's modified flag. Text is loaded with an irreversible action so
  undo can't empty a freshly opened file. `doc::watch` polls mtimes every 2 s.
- **Editor** (`editor.rs`): one `TextView`; switching tabs swaps its buffer and restores
  that tab's cursor and scroll. Font, size and spacing go through a CSS provider on
  `.editor-font` (an empty `font_family` means the app font stack); tab stops and the
  centred 80/100-column width are computed from the measured character width.
  `LineGutter` is a small `gtk::Widget` subclass drawn with `Snapshot::append_layout`.
- **Session** (`session.rs`): `session.toml` plus `scratch/<id>.txt` for unsaved or
  untitled text, written a second after the last change and on close. `restore_tabs`
  only governs reopening clean files; unsaved text always comes back.
- **Control** (`control.rs`): `nyx_core::control` on the app's bus name and object path
  (registered in `connect_startup`), for Nyx Assistant: `notepad.new_tab`, `open`, `read`,
  `tabs`, `insert_text`, `save` (`doc::write_to`, no dialog). Files only inside home.
- Checks: `cargo clippy --all-targets -- -D warnings`, `cargo test`. Layout check:
  `NOTEPAD_SNAPSHOT=/tmp/x.png NOTEPAD_APP_ID=io.github.design_nexus.nyx.NotepadTest nyx-notepad FILE`
  (renders offscreen and quits; the compositor decides the size). Point
  `XDG_CONFIG_HOME`/`XDG_DATA_HOME` at scratch dirs so runs don't touch the real session.
  A scratch `settings.toml` with `mode = "theme"`, `theme = "catppuccin-latte"` checks
  light mode.
