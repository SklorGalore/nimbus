//! What Nimbus remembers between runs: `state.toml` beside the config.
//!
//! The config file is the user's, edited by hand and full of their comments,
//! so Nimbus never writes to it. What the app remembers by itself goes in
//! this separate file instead: whether the sidebar is shown and how wide,
//! the zoom (M16), the window's size and place, the files opened lately,
//! and one *session* per project folder (its tabs, cursors and expanded
//! folders), so closing Nimbus and opening it again brings you back where
//! you were.
//!
//! The file never holds any text from your files, only paths and byte
//! offsets, so it stays a few kilobytes. A missing or broken file just
//! means the defaults.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sidebar::{DEFAULT_WIDTH, Mode};

/// Most recently opened files remembered.
pub const MAX_RECENT: usize = 100;

/// Most project folders whose sessions are remembered.
pub const MAX_SESSIONS: usize = 20;

/// Everything remembered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The sidebar is shown.
    pub sidebar_visible: bool,
    /// Sidebar width in logical pixels.
    pub sidebar_width: f32,
    /// The editor font size the zoom actions left, in points (M16); `None`
    /// when it is the config's `font_size`, so a later change to the config
    /// still takes effect. Read at startup unless `remember_zoom = false`.
    pub font_size: Option<f64>,
    /// The window's size and place when it last closed.
    pub window: Option<WindowGeometry>,
    /// Files opened in any session, most recent first, at most
    /// [`MAX_RECENT`].
    pub recent: Vec<RecentFile>,
    /// One session per project folder, most recent first, at most
    /// [`MAX_SESSIONS`]. The first is what `nimbus` with no arguments
    /// reopens.
    pub sessions: Vec<Session>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            sidebar_visible: true,
            sidebar_width: DEFAULT_WIDTH,
            font_size: None,
            window: None,
            recent: Vec::new(),
            sessions: Vec::new(),
        }
    }
}

/// A file opened lately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentFile {
    /// Its canonical path (`io::canonical`), so one file is one entry
    /// however it was named.
    pub path: PathBuf,
}

/// Where the window was. Logical pixels, so it means the same on a display
/// with another scale factor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    /// Inner width.
    pub width: f64,
    /// Inner height.
    pub height: f64,
    /// Left edge of the outer frame, in the desktop's coordinates.
    pub x: f64,
    /// Top edge of the outer frame.
    pub y: f64,
    /// The display it was on, by the name the OS gives it. A position is
    /// only used again when that display is still there.
    pub monitor: Option<String>,
    /// Maximized.
    #[serde(default)]
    pub maximized: bool,
}

/// The sidebar's mode, as the state file spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarMode {
    /// The project tree.
    #[default]
    Files,
    /// Find in files.
    Search,
}

impl From<Mode> for SidebarMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Files => Self::Files,
            Mode::Search => Self::Search,
        }
    }
}

impl From<SidebarMode> for Mode {
    fn from(m: SidebarMode) -> Self {
        match m {
            SidebarMode::Files => Self::Files,
            SidebarMode::Search => Self::Search,
        }
    }
}

/// One project folder as it was left.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// The project folder (canonical).
    pub root: PathBuf,
    /// What the sidebar showed.
    #[serde(default)]
    pub mode: SidebarMode,
    /// Folders open in the tree, relative to the root.
    #[serde(default)]
    pub expanded: Vec<PathBuf>,
    /// The tabs, in order.
    #[serde(default)]
    pub tabs: Vec<SessionTab>,
    /// Each editor group's share of the editor area's width, left to
    /// right (M13). Empty, as an older file has it, means one group.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<f32>,
}

/// One tab of a session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTab {
    /// The file.
    pub path: PathBuf,
    /// The cursor, as a byte offset. Byte offsets rather than lines,
    /// because that is what the document and view hold, and a huge file
    /// may not have its lines counted yet. On restore they are clamped to
    /// the file's length and moved back to a character boundary, so a file
    /// that changed meanwhile puts the cursor somewhere nearby, never
    /// somewhere invalid.
    #[serde(default)]
    pub cursor: usize,
    /// The byte the view's top row starts in.
    #[serde(default)]
    pub scroll: usize,
    /// The tab that was in front: the focused group's active tab.
    #[serde(default)]
    pub active: bool,
    /// The editor group it was in, counted from the left (M13).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub group: usize,
    /// It was its group's active tab (M13); [`SessionTab::active`] says
    /// which group had the focus.
    #[serde(default, skip_serializing_if = "is_false")]
    pub front: bool,
    /// Its folds, as `[start, end]` byte ranges of the hidden text (M13).
    /// Byte ranges only fit the file they were made on, so they are saved
    /// only for a tab with no unsaved edits, along with [`SessionTab::disk`],
    /// and dropped on restore when the file changed since.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folds: Vec<[usize; 2]>,
    /// The file's size and modification time (nanoseconds since 1970) when
    /// the folds were saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<[u64; 2]>,
}

/// For `skip_serializing_if`: older files have no group numbers, and a
/// session in one group keeps the file as it was.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes the field by reference"
)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// For `skip_serializing_if`, as [`is_zero`].
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes the field by reference"
)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Where the state file lives: beside the config file.
fn path() -> Option<PathBuf> {
    crate::config::path().and_then(|p| p.parent().map(|d| d.join("state.toml")))
}

/// A path the TOML file can hold: TOML strings are UTF-8, and one path
/// that is not would make the whole file fail to write.
fn writable(p: &Path) -> bool {
    p.to_str().is_some()
}

impl State {
    /// Read the remembered state, or the defaults.
    #[must_use]
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    /// Read state from TOML text, defaults for anything missing or broken.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut s: Self = toml::from_str(text).unwrap_or_default();
        s.recent.truncate(MAX_RECENT);
        s.sessions.truncate(MAX_SESSIONS);
        s
    }

    /// Remember this state. The file is written beside its old self and
    /// renamed over it, as documents are saved, so a crash mid-write never
    /// leaves half a file. Failures are logged: losing it costs a session,
    /// never any text.
    pub fn save(&self) {
        let Some(path) = path() else { return };
        let text = match toml::to_string(self) {
            Ok(t) => t,
            Err(e) => {
                log::warn!("state: {e}");
                return;
            }
        };
        if let Err(e) = write_atomic(&path, text.as_bytes()) {
            log::warn!("cannot save {}: {e}", path.display());
        }
    }

    /// The session for project folder `root`, if one is remembered.
    #[must_use]
    pub fn session(&self, root: &Path) -> Option<&Session> {
        self.sessions.iter().find(|s| s.root == root)
    }

    /// The most recent session: what `nimbus` with no arguments reopens.
    #[must_use]
    pub fn last_session(&self) -> Option<&Session> {
        self.sessions.first()
    }

    /// Remember `session` as the most recent, replacing the one for the
    /// same folder and dropping the oldest past [`MAX_SESSIONS`]. Paths the
    /// file cannot hold are left out.
    pub fn put_session(&mut self, mut session: Session) {
        if !writable(&session.root) {
            return;
        }
        session.tabs.retain(|t| writable(&t.path));
        session.expanded.retain(|p| writable(p));
        self.sessions.retain(|s| s.root != session.root);
        self.sessions.insert(0, session);
        self.sessions.truncate(MAX_SESSIONS);
    }

    /// Note that `path` (canonical) was opened: it moves to the front of
    /// the recent files.
    pub fn note_recent(&mut self, path: &Path) {
        if !writable(path) {
            return;
        }
        self.recent.retain(|r| r.path != path);
        self.recent.insert(
            0,
            RecentFile {
                path: path.to_path_buf(),
            },
        );
        self.recent.truncate(MAX_RECENT);
    }

    /// Drop the recent files in `gone`.
    pub fn forget_recent(&mut self, gone: &[PathBuf]) {
        self.recent.retain(|r| !gone.contains(&r.path));
    }
}

/// Of `paths`, the ones that no longer exist. Each costs a `stat`, which can
/// be slow on a network drive, so callers run this on a worker.
#[must_use]
pub fn missing(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().filter(|p| !p.exists()).cloned().collect()
}

/// Write `bytes` to `path` through a temporary file beside it and a rename,
/// creating the folder if needed.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let (tmp, mut file) = crate::io::create_temp(path).map_err(|e| e.source)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            crate::io::rename_with_retry(&tmp, path)
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(path: &str, cursor: usize, active: bool) -> SessionTab {
        SessionTab {
            path: PathBuf::from(path),
            cursor,
            scroll: cursor / 2,
            active,
            ..SessionTab::default()
        }
    }

    fn session(root: &str) -> Session {
        Session {
            root: PathBuf::from(root),
            mode: SidebarMode::Search,
            expanded: vec![PathBuf::from("src"), PathBuf::from("src/view")],
            tabs: vec![
                tab("/p/a.rs", 10, false),
                tab("/p/b.rs", 99, true),
                SessionTab {
                    group: 1,
                    front: true,
                    folds: vec![[4, 20], [30, 41]],
                    disk: Some([41, 1_760_000_000_123_456_789]),
                    ..tab("/p/a.rs", 5, false)
                },
            ],
            groups: vec![0.625, 0.375],
        }
    }

    #[test]
    fn round_trips_and_tolerates_partial_files() {
        let mut s = State {
            sidebar_visible: false,
            sidebar_width: 300.0,
            font_size: Some(18.0),
            window: Some(WindowGeometry {
                width: 1000.0,
                height: 700.0,
                x: -20.0,
                y: 40.0,
                monitor: Some("Built-in Display".into()),
                maximized: false,
            }),
            ..State::default()
        };
        s.note_recent(Path::new("/p/a.rs"));
        s.put_session(session("/p"));
        let text = toml::to_string(&s).unwrap();
        assert_eq!(State::parse(&text), s);
        let partial = State::parse("sidebar_width = 200.0\n");
        assert!(partial.sidebar_visible);
        assert_eq!(partial.sidebar_width, 200.0);
        assert_eq!(partial.font_size, None);
        assert_eq!(partial.sessions, Vec::new());
        assert!(partial.window.is_none());
        // A session missing everything but its root still loads.
        let bare = State::parse("[[sessions]]\nroot = \"/q\"\n");
        assert_eq!(bare.sessions[0].mode, SidebarMode::Files);
        assert_eq!(bare.sessions[0].tabs, Vec::new());
        // Broken text is the defaults, not an error.
        assert_eq!(State::parse("sessions = 3\n["), State::default());
    }

    #[test]
    fn sessions_are_most_recent_first_and_capped() {
        let mut s = State::default();
        for i in 0..MAX_SESSIONS + 5 {
            s.put_session(session(&format!("/p{i}")));
        }
        assert_eq!(s.sessions.len(), MAX_SESSIONS);
        let last = format!("/p{}", MAX_SESSIONS + 4);
        assert_eq!(s.last_session().unwrap().root, Path::new(&last));
        // Putting an older root again moves it to the front, once.
        s.put_session(session("/p10"));
        assert_eq!(s.last_session().unwrap().root, Path::new("/p10"));
        assert_eq!(
            s.sessions
                .iter()
                .filter(|x| x.root == Path::new("/p10"))
                .count(),
            1
        );
        assert!(s.session(Path::new("/p0")).is_none(), "the oldest went");
    }

    #[test]
    fn recent_files_dedupe_cap_and_prune() {
        let mut s = State::default();
        for i in 0..MAX_RECENT + 10 {
            s.note_recent(Path::new(&format!("/f{i}")));
        }
        assert_eq!(s.recent.len(), MAX_RECENT);
        s.note_recent(Path::new("/f50"));
        assert_eq!(s.recent[0].path, Path::new("/f50"));
        assert_eq!(
            s.recent
                .iter()
                .filter(|r| r.path == Path::new("/f50"))
                .count(),
            1
        );
        s.forget_recent(&[PathBuf::from("/f50")]);
        assert!(s.recent.iter().all(|r| r.path != Path::new("/f50")));
        let here = std::env::current_dir().unwrap();
        let gone = here.join("no-such-file-nimbus");
        assert_eq!(missing(&[here.clone(), gone.clone()]), vec![gone]);
    }

    #[cfg(unix)]
    #[test]
    fn paths_toml_cannot_hold_are_left_out() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let bad = Path::new(OsStr::from_bytes(b"/p/\xff.rs"));
        let mut s = State::default();
        s.note_recent(bad);
        let mut sess = session("/p");
        sess.tabs.push(SessionTab {
            path: bad.to_path_buf(),
            cursor: 0,
            scroll: 0,
            active: false,
            ..SessionTab::default()
        });
        s.put_session(sess);
        assert_eq!(s.recent, Vec::new());
        assert_eq!(s.sessions[0].tabs.len(), 3, "the bad path left out");
        assert!(toml::to_string(&s).is_ok());
    }

    #[test]
    fn writes_through_a_temp_file() {
        let dir = std::env::temp_dir().join(format!("nimbus-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("deeper").join("state.toml");
        write_atomic(&file, b"a = 1\n").unwrap();
        write_atomic(&file, b"a = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "a = 2\n");
        let left: Vec<_> = std::fs::read_dir(file.parent().unwrap()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temp files left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
