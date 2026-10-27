//! Well-known locations. Every path honours the XDG overrides so the whole app
//! can be pointed at a scratch copy of `~/.config` for testing.

pub use nyx_core::paths::*;

use std::path::PathBuf;

pub fn session_file() -> PathBuf {
    data_dir().join("session.toml")
}

/// The text of tabs with unsaved changes, one file per tab.
pub fn scratch_dir() -> PathBuf {
    data_dir().join("scratch")
}

