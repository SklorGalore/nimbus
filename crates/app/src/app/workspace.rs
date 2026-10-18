//! Sessions and what Nimbus remembers between runs, as the app sees them:
//! restoring tabs lazily, capturing the session to save, writing the state
//! file a second after a change (M12).
//!
//! A child module of `app`, so it can reach the app's fields; it lives in
//! its own file to keep `app.rs` about events and editing.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;

use super::{App, AppEvent, MESSAGE_TIME, Message, Tab};
use crate::document::Document;
use crate::input::Command;
use crate::picker::Kind;
use crate::session::{self, Restore};
use crate::sidebar::Sidebar;
use crate::state::{Session, SessionTab, State};

/// What `main` worked out about this run's session.
#[derive(Debug)]
pub struct Startup {
    /// What was remembered from earlier runs.
    pub state: State,
    /// The session to restore, if any: its tabs replace the command line's
    /// (which named none).
    pub restore: Option<Restore>,
    /// The project folder whose session this run saves; `None` when files
    /// were named, with `--no-session`, and in unattended runs.
    pub session_root: Option<PathBuf>,
}

/// How long after a tab opens or closes the state file is written: a burst
/// of changes (closing five tabs) writes once.
const STATE_SAVE_DELAY: Duration = Duration::from_secs(1);

/// Read restored tabs' files on one worker, one after another, sending each
/// document back as it is ready.
fn spawn_restore(proxy: &EventLoopProxy<AppEvent>, paths: Vec<PathBuf>) {
    let proxy = proxy.clone();
    let spawned = std::thread::Builder::new()
        .name("nimbus-restore".to_owned())
        .spawn(move || {
            for path in paths {
                let doc = Document::open(&path)
                    .map(Box::new)
                    .map_err(|e| e.to_string());
                if proxy.send_event(AppEvent::Restored(path, doc)).is_err() {
                    return; // the app is gone
                }
            }
        });
    if let Err(e) = spawned {
        // Each tab is still read when it is first shown.
        log::warn!("could not start the restore thread: {e}");
    }
}

/// What is remembered between runs, and when it is next written.
#[derive(Debug)]
pub(super) struct Workspace {
    /// The sidebar, the window, recent files and sessions.
    pub(super) state: State,
    /// The project folder whose session is saved ([`Startup::session_root`]).
    pub(super) session_root: Option<PathBuf>,
    /// Write the state file at this time.
    pub(super) save_at: Option<Instant>,
    /// The worker reading restored tabs has been started.
    pub(super) restore_started: bool,
    /// Tabs closed in this run, most recent last, for Reopen Closed Tab.
    pub(super) closed: Vec<(PathBuf, Pending)>,
    /// Paths being renamed from the tree: their disappearance on disk is
    /// the rename, not a deletion, until it reports back.
    pub(super) moving: Vec<PathBuf>,
}

/// Most closed tabs remembered for Reopen Closed Tab.
const MAX_CLOSED: usize = 50;

/// A restored tab's cursor and top of view, as byte offsets, until its
/// file is read and they can be clamped to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Pending {
    pub(super) cursor: usize,
    pub(super) scroll: usize,
}

impl App {
    /// Write the state file a moment from now, so a burst of changes
    /// writes once. Never in an unattended run.
    pub(super) fn save_state_soon(&mut self) {
        if !self.options.unattended() {
            self.ws
                .save_at
                .get_or_insert_with(|| Instant::now() + STATE_SAVE_DELAY);
        }
    }

    /// A tab opened, closed or came to the front: the session changed.
    pub(super) fn session_changed(&mut self) {
        if self.ws.session_root.is_some() {
            self.save_state_soon();
        }
    }

    /// Write the state file now, with this run's session in it.
    pub(super) fn save_state(&mut self) {
        self.ws.save_at = None;
        if self.options.unattended() {
            return;
        }
        if let Some(session) = self.capture_session() {
            self.ws.state.put_session(session);
        }
        self.ws.state.save();
    }

    /// The session as it stands: the open files with their cursors and
    /// views, the expanded folders and the sidebar's mode. Untitled tabs
    /// are left out; there is no autosave to bring their text back.
    pub(super) fn capture_session(&self) -> Option<Session> {
        let root = self.ws.session_root.clone()?;
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let path = if t.doc.is_placeholder() {
                    t.doc.path.clone()?
                } else {
                    t.doc.canonical.clone()?
                };
                let (cursor, scroll) = t.pending.map_or((t.doc.cursor(), t.view.scroll_top), |p| {
                    (p.cursor, p.scroll)
                });
                Some(SessionTab {
                    path,
                    cursor,
                    scroll,
                    active: i == self.active,
                })
            })
            .collect();
        let tree_root = self.sidebar.tree.root();
        let expanded = self
            .sidebar
            .tree
            .expanded_dirs()
            .into_iter()
            .filter_map(|p| p.strip_prefix(tree_root).ok().map(Path::to_path_buf))
            .collect();
        Some(Session {
            root,
            mode: self.sidebar.mode.into(),
            expanded,
            tabs,
        })
    }

    /// Read tab `i`'s file if it is still a restored placeholder, so it can
    /// be shown. A file that cannot be read closes its tab with a message.
    /// Returns whether tab `i` is still there.
    pub(super) fn ensure_loaded(&mut self, i: usize) -> bool {
        let Some(tab) = self.tabs.get(i) else {
            return false;
        };
        if !tab.doc.is_placeholder() {
            return true;
        }
        let Some(path) = tab.doc.path.clone() else {
            return true;
        };
        match Document::open(&path) {
            Ok(doc) => {
                self.adopt_restored(i, doc);
                true
            }
            Err(e) => {
                self.say(format!("Cannot open {}: {e}", path.display()), true);
                self.drop_tab(i);
                false
            }
        }
    }

    /// Put `doc`, just read, in place of tab `i`'s placeholder.
    pub(super) fn adopt_restored(&mut self, i: usize, mut doc: Document) {
        doc.detect_language(&self.languages);
        let tab = &mut self.tabs[i];
        tab.doc = doc;
        tab.apply_pending();
        self.title_key = None;
        self.sync_watch();
    }

    /// Remove tab `i` without asking: only for a tab with nothing to lose
    /// (a placeholder whose file cannot be read). The last tab becomes an
    /// untitled one rather than quitting.
    pub(super) fn drop_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            self.tabs[0] = Tab::new(Document::untitled(), self.config.soft_wrap);
        } else {
            self.tabs.remove(i);
            if self.active > i || self.active == self.tabs.len() {
                self.active = self.active.saturating_sub(1);
            }
        }
        self.title_key = None;
        self.session_changed();
    }

    /// Commands about recent files, closed tabs and sessions, then the
    /// tree's and the compare view's. Returns `false` for any other.
    pub(super) fn run_workspace_command(&mut self, cmd: Command<'_>) -> bool {
        match cmd {
            Command::OpenRecent => self.open_picker(Kind::Recent),
            Command::ReopenClosedTab => self.reopen_closed(),
            Command::ClearRecent => {
                self.ws.state.recent.clear();
                self.save_state_soon();
                self.say("Recent files cleared", false);
            }
            Command::ReopenLastSession => self.reopen_last_session(),
            _ => return self.run_tree_command(cmd) || self.run_compare_command(cmd),
        }
        self.changed(false);
        true
    }

    /// Recent files under the project folder, relative to it, most recent
    /// first: the file opener lists them first.
    pub(super) fn recent_under_root(&self) -> Vec<PathBuf> {
        let root = self.sidebar.tree.root();
        self.ws
            .state
            .recent
            .iter()
            .filter_map(|r| r.path.strip_prefix(root).ok().map(Path::to_path_buf))
            .collect()
    }

    /// Look for recent files that are gone, on a worker: a hundred `stat`
    /// calls can be slow on a network drive.
    pub(super) fn check_recent(&self, paths: Vec<PathBuf>) {
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new()
            .name("nimbus-recent".to_owned())
            .spawn(move || {
                let gone = crate::state::missing(&paths);
                if !gone.is_empty() {
                    let _ = proxy.send_event(AppEvent::RecentMissing(gone));
                }
            });
        if let Err(e) = spawned {
            log::debug!("could not check recent files: {e}");
        }
    }

    /// Recent files were found missing: forget them, and take them off an
    /// open Open Recent.
    pub(super) fn recent_missing(&mut self, gone: &[PathBuf]) {
        self.ws.state.forget_recent(gone);
        self.save_state_soon();
        if let Some(p) = self.picker.as_mut() {
            p.drop_recent(gone);
            self.changed(false);
        }
    }

    /// Remember tab `i`, about to close, for Reopen Closed Tab.
    pub(super) fn remember_closed(&mut self, i: usize) {
        let tab = &self.tabs[i];
        let Some(path) = tab.doc.path.clone() else {
            return; // untitled: nothing to reopen
        };
        let (cursor, scroll) = tab
            .pending
            .map_or((tab.doc.cursor(), tab.view.scroll_top), |p| {
                (p.cursor, p.scroll)
            });
        self.ws.closed.push((path, Pending { cursor, scroll }));
        if self.ws.closed.len() > MAX_CLOSED {
            self.ws.closed.remove(0);
        }
    }

    /// Open the last closed tab again, with its cursor and view.
    pub(super) fn reopen_closed(&mut self) {
        let Some((path, at)) = self.ws.closed.pop() else {
            self.say("No closed tab to reopen", false);
            return;
        };
        let before = self.tabs.len();
        self.open_path(&path);
        if self.tabs.len() > before || self.tabs[self.active].doc.path.as_deref() == Some(&path) {
            let tab = &mut self.tabs[self.active];
            tab.pending = Some(at);
            tab.apply_pending();
            self.changed(true);
        }
    }

    /// Switch to the most recent session of another project folder: this
    /// one's is saved first, tabs without unsaved work close, and the
    /// other's tabs, tree and sidebar mode come back. Tabs with unsaved
    /// work stay open, so nothing is lost.
    pub(super) fn reopen_last_session(&mut self) {
        let current = self
            .ws
            .session_root
            .clone()
            .unwrap_or_else(|| self.sidebar.tree.root().to_path_buf());
        let Some(session) = self
            .ws
            .state
            .sessions
            .iter()
            .find(|s| s.root != current && s.root.is_dir())
            .cloned()
        else {
            self.say("No other project's session to reopen", false);
            return;
        };
        if let Some(s) = self.capture_session() {
            self.ws.state.put_session(s);
        }
        let languages = &self.languages;
        let (restore, warnings) = session::restore(&session, |p| {
            Document::open(p)
                .map(|mut d| {
                    d.detect_language(languages);
                    d
                })
                .map_err(|e| e.to_string())
        });
        // Keep only tabs with unsaved work (or saving).
        let keep: Vec<Tab> = std::mem::take(&mut self.tabs)
            .into_iter()
            .filter(|t| t.doc.is_dirty() || t.doc.is_saving() || t.doc.deleted)
            .collect();
        self.root.clone_from(&session.root);
        let (visible, width) = (self.sidebar.visible, self.sidebar.width);
        self.sidebar = Sidebar::new(session.root.clone(), visible, width);
        if !self.options.unattended() {
            self.ws.session_root = Some(session.root.clone());
        }
        let mut state = std::mem::take(&mut self.ws.state);
        let (tabs, active, note) = startup_tabs(
            Vec::new(),
            Some(restore),
            &mut state,
            &mut self.sidebar,
            self.config.soft_wrap,
        );
        self.ws.state = state;
        let opened = tabs.len();
        self.tabs = keep;
        let first_new = self.tabs.len();
        self.tabs
            .extend(tabs.into_iter().filter(|t| t.doc.path.is_some()));
        self.active = if self.tabs.len() > first_new {
            (first_new + active).min(self.tabs.len() - 1)
        } else if self.tabs.is_empty() {
            self.tabs
                .push(Tab::new(Document::untitled(), self.config.soft_wrap));
            0
        } else {
            0
        };
        self.ws.restore_started = false;
        self.title_key = None;
        self.preedit = None;
        if self.sidebar.visible {
            self.show_sidebar();
        }
        self.sync_watch();
        let name = session.root.file_name().map_or_else(
            || session.root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let mut text = format!("Reopened {name}: {opened} tab(s)");
        text.extend(note.map(|n| format!("; {n}")));
        text.extend(warnings.iter().map(|w| format!("; {w}")));
        self.say(text, !warnings.is_empty());
        self.session_changed();
    }

    /// After the first frame, read the restored tabs' files on a worker.
    pub(super) fn start_restoring(&mut self) {
        if self.ws.restore_started {
            return;
        }
        self.ws.restore_started = true;
        let paths: Vec<PathBuf> = self
            .tabs
            .iter()
            .filter(|t| t.doc.is_placeholder())
            .filter_map(|t| t.doc.path.clone())
            .collect();
        if !paths.is_empty() {
            spawn_restore(&self.proxy, paths);
        }
    }

    /// A restored tab's file arrived from the worker. It is dropped if the
    /// tab was shown (and so read) meanwhile, or closed.
    pub(super) fn restored_arrived(&mut self, path: &Path, doc: Result<Box<Document>, String>) {
        let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.doc.is_placeholder() && t.doc.path.as_deref() == Some(path))
        else {
            return;
        };
        match doc {
            Ok(doc) => {
                self.adopt_restored(i, *doc);
                self.changed(false);
            }
            // Tried again, with a message, when the tab is shown.
            Err(e) => log::debug!("restoring {}: {e}", path.display()),
        }
    }
}

/// The first tabs: the files named on the command line (noted as recent),
/// or else a restored session's (the front one read, the rest
/// placeholders, with the sidebar's mode and expanded folders and a note
/// for files that are gone), or else one untitled tab. Returns them and
/// the front tab's index.
pub(super) fn startup_tabs(
    docs: Vec<Document>,
    restore: Option<Restore>,
    state: &mut State,
    sidebar: &mut Sidebar,
    wrap: bool,
) -> (Vec<Tab>, usize, Option<String>) {
    for c in docs.iter().filter_map(|d| d.canonical.as_deref()) {
        state.note_recent(c);
    }
    let mut tabs: Vec<Tab> = docs.into_iter().map(|d| Tab::new(d, wrap)).collect();
    let mut active = 0;
    let mut note = None;
    if let Some(r) = restore.filter(|_| tabs.is_empty()) {
        if let Some(mode) = r.mode {
            sidebar.mode = mode;
        }
        sidebar.tree.expand_later(r.expanded);
        note = session::dropped_note(&r.dropped);
        for t in r.tabs {
            let mut tab = Tab::new(t.doc, wrap);
            tab.pending = Some(Pending {
                cursor: t.cursor,
                scroll: t.scroll,
            });
            tab.apply_pending();
            tabs.push(tab);
        }
        active = r.active;
    }
    if tabs.is_empty() {
        tabs.push(Tab::new(Document::untitled(), wrap));
    }
    let active = active.min(tabs.len() - 1);
    (tabs, active, note)
}

/// The first message: startup warnings (as an error, shown for longer),
/// then any note about the restored session.
pub(super) fn startup_message(warnings: Vec<String>, note: Option<String>) -> Option<Message> {
    for w in &warnings {
        log::warn!("{w}");
    }
    let is_error = !warnings.is_empty();
    let text = warnings
        .into_iter()
        .chain(note)
        .collect::<Vec<_>>()
        .join("; ");
    (!text.is_empty()).then(|| Message {
        text,
        is_error,
        until: Instant::now() + MESSAGE_TIME * if is_error { 2 } else { 1 },
    })
}
