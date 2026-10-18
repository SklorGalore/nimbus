//! Sessions and what Nimbus remembers between runs, as the app sees them:
//! restoring tabs lazily, capturing the session to save, writing the state
//! file a second after a change (M12).
//!
//! A child module of `app`, so it can reach the app's fields; it lives in
//! its own file to keep `app.rs` about events and editing.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;

use super::{App, AppEvent, MESSAGE_TIME, Message, Tab, wrap_for};
use crate::config::Config;
use crate::docs::{DocId, Docs};
use crate::document::Document;
use crate::groups::Groups;
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

/// A restored tab's cursor, top of view and folds, as byte offsets, until
/// its file is read and they can be clamped to it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Pending {
    pub(super) cursor: usize,
    pub(super) scroll: usize,
    /// Folds saved with the session, made only if the file is unchanged.
    pub(super) folds: Vec<Range<usize>>,
    /// The file's size and time when the folds were saved.
    pub(super) disk: Option<[u64; 2]>,
}

/// Most folds saved per tab. Fold All on a big file can make a hundred
/// thousand; saving them all would make the state file megabytes, so past
/// this only the outermost are kept, and past it again none.
const MAX_SAVED_FOLDS: usize = 1_000;

/// The folds of a view worth saving: all of them, or only those hiding
/// text now when there are too many, or none.
fn saveable(folds: &nimbus_core::fold::Folds) -> Vec<Range<usize>> {
    if folds.all().count() <= MAX_SAVED_FOLDS {
        folds.all().cloned().collect()
    } else if folds.len() <= MAX_SAVED_FOLDS {
        folds.active().to_vec()
    } else {
        Vec::new()
    }
}

/// Folds as the state file holds them.
fn saved_folds(folds: &[Range<usize>]) -> Vec<[usize; 2]> {
    folds.iter().map(|f| [f.start, f.end]).collect()
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
        if let Some(window) = &self.window {
            self.ws.state.window = Some(self.window_geometry(window));
        }
        self.ws.state.save();
    }

    /// The session as it stands: the open files with their cursors,
    /// views and folds, the editor groups, the expanded folders and the
    /// sidebar's mode. Untitled tabs are left out; there is no autosave to
    /// bring their text back. A file open in two groups is listed in each.
    pub(super) fn capture_session(&self) -> Option<Session> {
        let root = self.ws.session_root.clone()?;
        let fronts: Vec<usize> = (0..self.groups.len())
            .map(|g| self.group_active(g))
            .collect();
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let doc = &self.docs[t.doc];
                let path = if doc.is_placeholder() {
                    doc.path.clone()?
                } else {
                    doc.canonical.clone()?
                };
                let (cursor, scroll, folds, disk) = if let Some(p) = &t.pending {
                    (p.cursor, p.scroll, saved_folds(&p.folds), p.disk)
                } else {
                    let disk = doc.disk_stamp();
                    let folds = if disk.is_some() {
                        saved_folds(&saveable(&t.view.folds))
                    } else {
                        Vec::new()
                    };
                    (t.carets.cursor(&doc.buffer), t.view.scroll_top, folds, disk)
                };
                let disk = disk.filter(|_| !folds.is_empty());
                Some(SessionTab {
                    path,
                    cursor,
                    scroll,
                    active: i == self.active,
                    group: t.group,
                    front: fronts.get(t.group) == Some(&i),
                    folds,
                    disk,
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
        let groups = if self.groups.len() > 1 {
            self.groups.list.iter().map(|g| g.share).collect()
        } else {
            Vec::new()
        };
        Some(Session {
            root,
            mode: self.sidebar.mode.into(),
            expanded,
            tabs,
            groups,
        })
    }

    /// Read tab `i`'s file if it is still a restored placeholder, so it can
    /// be shown. A file that cannot be read closes its tab with a message.
    /// Returns whether tab `i` is still there.
    pub(super) fn ensure_loaded(&mut self, i: usize) -> bool {
        let Some(tab) = self.tabs.get(i) else {
            return false;
        };
        let doc = &self.docs[tab.doc];
        if !doc.is_placeholder() {
            return true;
        }
        let Some(path) = doc.path.clone() else {
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

    /// Put `doc`, just read, in place of tab `i`'s placeholder, for every
    /// tab showing it.
    pub(super) fn adopt_restored(&mut self, i: usize, mut doc: Document) {
        doc.detect_language(&self.languages);
        self.configure_indent(&mut doc);
        let wrap = self.options.wrap || wrap_for(&self.config, &doc);
        let id = self.tabs[i].doc;
        doc.journal_mut().keep = true;
        self.docs[id] = doc;
        for tab in self.tabs.iter_mut().filter(|t| t.doc == id) {
            tab.view.set_wrap(wrap);
            tab.apply_pending(&mut self.docs[id]);
        }
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
            let id = self.docs.insert(Document::untitled());
            let old = std::mem::replace(&mut self.tabs[0], Tab::new(id, self.config.soft_wrap));
            self.docs.remove(old.doc);
        } else {
            self.remove_tab(i);
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
            Command::FollowTail => self.toggle_follow(),
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
        let doc = &self.docs[tab.doc];
        let Some(path) = doc.path.clone() else {
            return; // untitled: nothing to reopen
        };
        let (cursor, scroll) = tab
            .pending
            .as_ref()
            .map_or((tab.carets.cursor(&doc.buffer), tab.view.scroll_top), |p| {
                (p.cursor, p.scroll)
            });
        self.ws.closed.push((
            path,
            Pending {
                cursor,
                scroll,
                ..Pending::default()
            },
        ));
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
        if self.tabs.len() > before || self.doc().path.as_deref() == Some(&path) {
            let tab = &mut self.tabs[self.active];
            tab.pending = Some(at);
            tab.apply_pending(&mut self.docs[tab.doc]);
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
        // Keep only tabs with unsaved work (or saving), in one group; the
        // other documents close.
        let (keep, gone): (Vec<Tab>, Vec<Tab>) =
            std::mem::take(&mut self.tabs).into_iter().partition(|t| {
                let d = &self.docs[t.doc];
                d.is_dirty() || d.is_saving() || d.deleted
            });
        for t in gone {
            if !keep.iter().any(|k| k.doc == t.doc) {
                self.docs.remove(t.doc);
            }
        }
        self.find.open = false;
        self.root.clone_from(&session.root);
        let (visible, width) = (self.sidebar.visible, self.sidebar.width);
        self.sidebar = Sidebar::new(session.root.clone(), visible, width);
        if !self.options.unattended() {
            self.ws.session_root = Some(session.root.clone());
        }
        let mut state = std::mem::take(&mut self.ws.state);
        let (tabs, mut groups, active, note) = startup_tabs(
            Vec::new(),
            Some(restore),
            &mut state,
            &mut self.sidebar,
            self.config.soft_wrap,
            &mut self.docs,
        );
        self.ws.state = state;
        let mut tabs = tabs;
        configure_tabs(&mut tabs, &mut self.docs, &self.config, false);
        // An untitled tab stands in when the session had none.
        let (tabs, untitled): (Vec<Tab>, Vec<Tab>) = tabs
            .into_iter()
            .partition(|t| self.docs[t.doc].path.is_some());
        for t in untitled {
            self.docs.remove(t.doc);
        }
        let opened = tabs.len();
        // Tabs kept for their unsaved work join the first group.
        self.tabs = keep;
        for tab in &mut self.tabs {
            tab.group = 0;
        }
        let first_new = self.tabs.len();
        self.tabs.extend(tabs);
        self.active = if self.tabs.len() > first_new {
            for g in &mut groups.list {
                g.active += first_new;
            }
            (first_new + active).min(self.tabs.len() - 1)
        } else {
            groups = Groups::default();
            if self.tabs.is_empty() {
                let id = self.docs.insert(Document::untitled());
                self.tabs.push(Tab::new(id, self.config.soft_wrap));
            }
            0
        };
        self.groups = groups;
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
            .map(|t| &self.docs[t.doc])
            .filter(|d| d.is_placeholder())
            .filter_map(|d| d.path.clone())
            .collect();
        if !paths.is_empty() {
            spawn_restore(&self.proxy, paths);
        }
    }

    /// A restored tab's file arrived from the worker. It is dropped if the
    /// tab was shown (and so read) meanwhile, or closed.
    pub(super) fn restored_arrived(&mut self, path: &Path, doc: Result<Box<Document>, String>) {
        let Some(i) = self.tabs.iter().position(|t| {
            let d = &self.docs[t.doc];
            d.is_placeholder() && d.path.as_deref() == Some(path)
        }) else {
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
/// or else a restored session's (the front ones read, the rest
/// placeholders, in their editor groups, with the sidebar's mode and
/// expanded folders and a note for files that are gone), or else one
/// untitled tab. Returns them, the groups and the front tab's index. The
/// documents go in `open`.
pub(super) fn startup_tabs(
    docs: Vec<Document>,
    restore: Option<Restore>,
    state: &mut State,
    sidebar: &mut Sidebar,
    wrap: bool,
    open: &mut Docs,
) -> (Vec<Tab>, Groups, usize, Option<String>) {
    for c in docs.iter().filter_map(|d| d.canonical.as_deref()) {
        state.note_recent(c);
    }
    let mut tabs: Vec<Tab> = docs
        .into_iter()
        .map(|d| Tab::new(open.insert(d), wrap))
        .collect();
    let mut active = 0;
    let mut note = None;
    let mut groups = Groups::default();
    if let Some(r) = restore.filter(|_| tabs.is_empty()) {
        if let Some(mode) = r.mode {
            sidebar.mode = mode;
        }
        sidebar.tree.expand_later(r.expanded);
        note = session::dropped_note(&r.dropped);
        let groups_saved = r.shares.len().max(1);
        let mut fronts = vec![None; groups_saved];
        // Each file once, as one document however many groups show it:
        // two documents on one file would each save over the other.
        let mut seen: Vec<(PathBuf, usize, DocId)> = Vec::new();
        for (k, t) in r.tabs.into_iter().enumerate() {
            let group = t.group.min(groups_saved - 1);
            if let Some(j) = seen
                .iter()
                .position(|(c, g, _)| *c == t.canonical && *g == group)
            {
                // Listed twice in one group (a split saved by an M12
                // build): one tab.
                if k == r.active {
                    active = j;
                }
                continue;
            }
            if k == r.active {
                active = tabs.len();
            }
            if t.front {
                fronts[group] = Some(tabs.len());
            }
            let id = match seen.iter().find(|(c, _, _)| *c == t.canonical) {
                Some(&(_, _, id)) => {
                    if open[id].is_placeholder() && !t.doc.is_placeholder() {
                        open[id] = t.doc;
                        open[id].journal_mut().keep = true;
                    }
                    id
                }
                None => open.insert(t.doc),
            };
            seen.push((t.canonical, group, id));
            let mut tab = Tab::new(id, wrap);
            tab.group = group;
            tab.pending = Some(Pending {
                cursor: t.cursor,
                scroll: t.scroll,
                folds: t.folds,
                disk: t.disk,
            });
            tab.apply_pending(&mut open[id]);
            tabs.push(tab);
        }
        // A tab whose placeholder was replaced by a later group's read
        // document can be placed now too.
        for tab in &mut tabs {
            tab.apply_pending(&mut open[tab.doc]);
        }
        if !tabs.is_empty() {
            let focused = tabs[active.min(tabs.len() - 1)].group;
            let fronts: Vec<usize> = fronts
                .iter()
                .enumerate()
                .map(|(g, f)| {
                    f.or_else(|| tabs.iter().position(|t| t.group == g))
                        .unwrap_or(0)
                })
                .collect();
            groups = Groups::restored(&r.shares, &fronts, focused);
        }
    }
    if tabs.is_empty() {
        tabs.push(Tab::new(open.insert(Document::untitled()), wrap));
    }
    let active = active.min(tabs.len() - 1);
    (tabs, groups, active, note)
}

/// Set each read tab's indentation and wrap from the config and its
/// language; `wrap` (`--wrap`) turns wrap on for all of them.
pub(super) fn configure_tabs(tabs: &mut [Tab], docs: &mut Docs, config: &Config, wrap: bool) {
    for tab in tabs {
        let doc = &mut docs[tab.doc];
        if doc.is_placeholder() {
            continue;
        }
        doc.configure_indent(config.indent.for_language(doc.language_key()));
        tab.view.set_wrap(wrap || wrap_for(config, doc));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::RestoredTab;

    fn restored(name: &str, cursor: usize) -> RestoredTab {
        let mut doc = Document::from_text("some text\n");
        doc.path = Some(PathBuf::from(name));
        RestoredTab {
            doc,
            canonical: PathBuf::from(name),
            cursor,
            scroll: 0,
            group: 0,
            front: false,
            folds: Vec::new(),
            disk: None,
        }
    }

    #[test]
    fn a_file_listed_twice_restores_as_one_tab() {
        let restore = Restore {
            tabs: vec![
                restored("/p/a.rs", 1),
                restored("/p/b.rs", 2),
                restored("/p/b.rs", 3),
            ],
            active: 2,
            mode: None,
            expanded: Vec::new(),
            dropped: Vec::new(),
            shares: vec![1.0],
        };
        let mut state = State::default();
        let mut sidebar = Sidebar::new(PathBuf::from("/p"), false, 200.0);
        let mut open = Docs::default();
        let (tabs, _, active, _) = startup_tabs(
            Vec::new(),
            Some(restore),
            &mut state,
            &mut sidebar,
            false,
            &mut open,
        );
        assert_eq!(tabs.len(), 2);
        assert_eq!(active, 1, "the duplicate's front place goes to its file");
        assert_eq!(open.iter().count(), 2);
        assert_eq!(tabs[1].carets.cursor(&open[tabs[1].doc].buffer), 2);
    }

    #[test]
    fn a_file_in_two_groups_restores_as_one_document() {
        let mut placeholder = restored("/p/b.rs", 4);
        placeholder.doc = Document::placeholder(PathBuf::from("/p/b.rs"));
        let restore = Restore {
            tabs: vec![
                RestoredTab {
                    front: true,
                    ..placeholder
                },
                RestoredTab {
                    group: 1,
                    front: true,
                    ..restored("/p/b.rs", 7)
                },
            ],
            active: 1,
            shares: vec![0.75, 0.25],
            ..Restore::default()
        };
        let mut state = State::default();
        let mut sidebar = Sidebar::new(PathBuf::from("/p"), false, 200.0);
        let mut open = Docs::default();
        let (tabs, groups, active, _) = startup_tabs(
            Vec::new(),
            Some(restore),
            &mut state,
            &mut sidebar,
            false,
            &mut open,
        );
        assert_eq!(open.iter().count(), 1, "one document for both tabs");
        assert_eq!(tabs[0].doc, tabs[1].doc);
        assert!(!open[tabs[0].doc].is_placeholder(), "the read one is kept");
        assert_eq!((tabs[0].group, tabs[1].group), (0, 1));
        assert_eq!((active, groups.focused, groups.len()), (1, 1, 2));
        assert!((groups.list[0].share - 0.75).abs() < 1e-6);
        // Each tab keeps its own cursor in the shared text.
        let d = &open[tabs[0].doc].buffer;
        assert_eq!((tabs[0].carets.cursor(d), tabs[1].carets.cursor(d)), (4, 7));
    }

    #[test]
    fn folds_come_back_only_on_an_unchanged_file() {
        let dir = std::env::temp_dir().join(format!("nimbus-folds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.rs");
        let text = "fn a() {\n    x\n    y\n}\nfn b() {\n    z\n}\n";
        std::fs::write(&path, text).unwrap();
        let doc = Document::open(&path).unwrap();
        let disk = doc.disk_stamp();
        assert!(disk.is_some());
        let fold = 8..20; // the end of `fn a() {` to the end of `    y`
        assert_eq!(&text[fold.clone()], "\n    x\n    y");
        let pending = |disk| Pending {
            cursor: 0,
            scroll: 0,
            // The second is not a fold of this text: dropped.
            folds: vec![fold.clone(), 3..9],
            disk,
        };
        let mut open = Docs::default();
        let id = open.insert(doc);
        let mut tab = Tab::new(id, false);
        tab.pending = Some(pending(disk));
        tab.apply_pending(&mut open[id]);
        assert_eq!(tab.view.folds.active(), std::slice::from_ref(&fold));
        // The file changed since the folds were saved: none come back.
        let mut tab = Tab::new(id, false);
        tab.pending = Some(pending(disk.map(|[len, t]| [len, t + 1])));
        tab.apply_pending(&mut open[id]);
        assert!(tab.view.folds.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
