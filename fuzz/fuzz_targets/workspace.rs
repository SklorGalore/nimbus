//! Fuzz workspace files (M26): `.nimbus/workspace.toml` arrives inside
//! whatever folder the user cloned or unpacked, so its parser reads text
//! someone else wrote. Reading arbitrary text must never panic, with or
//! without a base folder for relative paths, and whatever it reads must
//! write back out and read again to the same file (one round settles
//! it). Workspace names typed by the user must never leave the
//! `workspaces/` folder.
//!
//! Keeping a project's file inside its project (`load_local`) needs real
//! folders and symlinks, so the unit tests in `workspace_file.rs` cover
//! it, not this target.
//!
//! The app's own `workspace_file.rs`, `state.rs` and `config.rs` are
//! compiled in with `#[path]`, as in the `config` target. `state.rs` also
//! names two of the app's bigger modules (`sidebar`, `io`) for a constant,
//! an enum and three file helpers; small stand-ins for those are below.
//! `cargo fuzz run workspace --features app-workspace`.

#![no_main]

use std::path::Path;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[allow(dead_code, reason = "the app's module, of which this uses a part")]
#[path = "../../crates/app/src/config.rs"]
mod config;

#[allow(dead_code, reason = "the app's module, of which this uses a part")]
#[path = "../../crates/app/src/state.rs"]
mod state;

#[allow(dead_code, reason = "the app's module, of which this uses a part")]
#[path = "../../crates/app/src/workspace_file.rs"]
mod workspace_file;

/// Stand-in for the app's `sidebar.rs`: what `state.rs` uses.
#[allow(dead_code, reason = "mirrors the app's enum")]
mod sidebar {
    pub const DEFAULT_WIDTH: f32 = 240.0;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Mode {
        Files,
        Search,
        Compare,
    }
}

/// Stand-in for the app's `io.rs`: what `state.rs` and
/// `workspace_file.rs` use. Nothing here is called by the target, which
/// never writes files; `canonical` is the app's own logic.
#[allow(dead_code, reason = "only some of these are reached")]
mod io {
    use std::path::{Path, PathBuf};

    pub struct SaveError {
        pub source: std::io::Error,
    }

    pub fn create_temp(_target: &Path) -> Result<(PathBuf, std::fs::File), SaveError> {
        Err(SaveError {
            source: std::io::Error::other("the fuzz target writes no files"),
        })
    }

    pub fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }

    pub fn canonical(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }
}

#[derive(Arbitrary, Debug)]
struct Input {
    text: String,
    local: bool,
    name: String,
}

fuzz_target!(|input: Input| {
    let base = input.local.then(|| Path::new("/project"));
    if let Ok(session) = workspace_file::from_text(&input.text, base) {
        let Ok(once) = workspace_file::to_text(&session, base) else {
            return;
        };
        let Ok(again) = workspace_file::from_text(&once, base) else {
            panic!("a written workspace does not read back:\n{once}");
        };
        let Ok(twice) = workspace_file::to_text(&again, base) else {
            panic!("a read-back workspace does not write");
        };
        assert_eq!(
            once, twice,
            "writing, reading and writing again changed the file"
        );
    }
    if let (Ok(file), Some(dir)) = (workspace_file::named(&input.name), workspace_file::dir()) {
        assert_eq!(
            file.parent(),
            Some(dir.as_path()),
            "{:?} left the folder",
            input.name
        );
    }
});
