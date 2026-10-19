//! The tree's file operations as the app runs them (M12): the inline name
//! field's keys, starting each operation on a worker, and what follows
//! when it is done (tabs follow a rename, a new file opens).

use std::path::{Path, PathBuf};

use super::workspace::{MAX_UNDO_MOVES, UndoMove};
use super::{App, AppEvent, Asked, cancel_or, edit_field};
use crate::input::{Command, Motion};
use crate::io;
use crate::tree_ops::{self, EditKind, Op};
use nimbus_core::Text;

/// Run a file operation on its own thread and report back.
fn spawn_op(proxy: &winit::event_loop::EventLoopProxy<AppEvent>, op: Op) {
    let sender = proxy.clone();
    let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(op)));
    let theirs = std::sync::Arc::clone(&slot);
    let spawned = std::thread::Builder::new()
        .name("nimbus-fileop".to_owned())
        .spawn(move || {
            if let Some(op) = theirs.lock().ok().and_then(|mut o| o.take()) {
                let result = tree_ops::run(&op);
                let _ = sender.send_event(AppEvent::FileOp(op, result));
            }
        });
    if let Err(e) = spawned {
        log::error!("could not start a file operation thread ({e}); running it here");
        if let Some(op) = slot.lock().ok().and_then(|mut o| o.take()) {
            let result = tree_ops::run(&op);
            let _ = proxy.send_event(AppEvent::FileOp(op, result));
        }
    }
}

impl App {
    /// The tree's commands. Returns `false` for any other.
    pub(super) fn run_tree_command(&mut self, cmd: Command<'_>) -> bool {
        match cmd {
            Command::TreeNewFile => self.begin_tree_edit(EditKind::NewFile),
            Command::TreeNewFolder => self.begin_tree_edit(EditKind::NewFolder),
            Command::TreeRename => self.begin_tree_edit(EditKind::Rename),
            Command::TreeDelete => self.tree_delete(),
            Command::TreeReveal => self.tree_reveal(),
            Command::SearchInFolder => self.search_in_folder(),
            Command::UndoMove => self.undo_move(),
            _ => return false,
        }
        self.changed(false);
        true
    }

    /// Open the tree's name field for `kind`, showing the sidebar if
    /// needed.
    fn begin_tree_edit(&mut self, kind: EditKind) {
        if !self.sidebar.visible {
            self.sidebar.visible = true;
            self.show_sidebar();
            self.remember();
        }
        match self.sidebar.begin_edit(kind) {
            Some(requests) => {
                self.find.focused = false;
                self.start_listings(requests);
            }
            None => self.say("Select a file or folder in the tree first", false),
        }
    }

    /// A command while the tree's name field has the keyboard: editing
    /// keys edit the name, Enter does it, Escape gives up. Returns `false`
    /// for commands that run as usual (saving, quitting, the palette).
    pub(super) fn run_tree_edit(&mut self, cmd: Command<'_>) -> bool {
        match cmd {
            Command::Escape => self.sidebar.edit = None,
            Command::Newline => self.commit_tree_edit(),
            Command::Tab
            | Command::Outdent
            | Command::Move(Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown, _) => {}
            Command::Move(..)
            | Command::Insert(_)
            | Command::DeleteBack
            | Command::DeleteForward
            | Command::DeleteWordBack
            | Command::DeleteWordForward
            | Command::DeleteToLineStart
            | Command::Undo
            | Command::Redo
            | Command::SelectAll
            | Command::Copy
            | Command::Cut
            | Command::Paste => {
                let paste = (cmd == Command::Paste)
                    .then(|| self.clipboard_get())
                    .flatten();
                let tab_width = self.tab_width();
                let Some(edit) = self.sidebar.edit.as_mut() else {
                    return false;
                };
                if let Some(text) = edit_field(&mut edit.field, cmd, paste.as_deref(), tab_width) {
                    self.clipboard_set(text);
                }
            }
            _ => return false,
        }
        true
    }

    /// Enter in the name field: check the name and start the operation.
    /// A bad name keeps the field open with a message.
    fn commit_tree_edit(&mut self) {
        let Some(edit) = self.sidebar.edit.take() else {
            return;
        };
        let name = edit.field.doc.buffer.line(0).into_owned();
        let to = match tree_ops::target(&edit.dir, &name) {
            Ok(to) => to,
            Err(e) => {
                self.say(e, true);
                self.sidebar.edit = Some(edit);
                return;
            }
        };
        let op = match edit.kind {
            EditKind::NewFile | EditKind::NewFolder if to.exists() => {
                self.say(format!("{name} already exists"), true);
                self.sidebar.edit = Some(edit);
                return;
            }
            EditKind::NewFile => Op::NewFile(to),
            EditKind::NewFolder => Op::NewFolder(to),
            EditKind::Rename => {
                let Some(from) = edit
                    .node
                    .and_then(|id| self.sidebar.tree.node(id))
                    .map(|n| n.path.clone())
                else {
                    return;
                };
                if to == from {
                    return;
                }
                // A rename that only changes case finds itself on a
                // case-insensitive disk: that is not a clash.
                let clash = to.exists() && io::canonical(&to) != io::canonical(&from);
                if clash {
                    // The field stays open under the question: Cancel goes
                    // back to editing the name.
                    self.sidebar.edit = Some(edit);
                    self.ask(
                        "Replace?",
                        format!(
                            "{name} already exists in this folder. \
                             Replace it with the one you are renaming?"
                        ),
                        &cancel_or("Replace"),
                        Asked::Replace { from, to },
                    );
                    return;
                }
                self.ws.moving.push(from.clone());
                Op::Rename { from, to }
            }
        };
        self.sidebar.focused = true;
        spawn_op(&self.proxy, op);
    }

    /// The answer to "Replace?" for a rename from `from` onto the
    /// existing `to`: go ahead, or leave the name field open to change it.
    pub(super) fn replace_answered(&mut self, replace: bool, from: PathBuf, to: PathBuf) {
        if !replace {
            return;
        }
        self.sidebar.edit = None;
        self.ws.moving.push(from.clone());
        self.sidebar.focused = true;
        spawn_op(&self.proxy, Op::Rename { from, to });
    }

    /// A file operation finished on its worker.
    pub(super) fn file_op_done(&mut self, op: &Op, result: Result<(), String>) {
        if let Op::Rename { from, .. } | Op::Trash(from) = op {
            self.ws.moving.retain(|p| p != from);
        }
        // An operation Undo Move started is not itself one to undo.
        let undoing = self.ws.undo_ops.iter().position(|o| o == op);
        if let Some(i) = undoing {
            self.ws.undo_ops.remove(i);
        }
        if let Err(e) = result {
            let what = match op {
                Op::NewFile(_) => "create the file",
                Op::NewFolder(_) => "create the folder",
                Op::Rename { .. } => "move",
                Op::Trash(_) => "move to the trash",
                Op::Copy { .. } => "copy",
                Op::Restore(_) => "put it back",
            };
            self.say(format!("Cannot {what}: {e}"), true);
            return;
        }
        if undoing.is_none()
            && let Some(undo) = UndoMove::of(op)
        {
            self.ws.undo_moves.push(undo);
            if self.ws.undo_moves.len() > MAX_UNDO_MOVES {
                self.ws.undo_moves.remove(0);
            }
        }
        if let Op::Trash(path) = op {
            let requests = self.sidebar.tree.affected_by(path);
            self.start_listings(requests);
            self.close_tabs_under(path);
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            let undo = if tree_ops::can_restore() && undoing.is_none() {
                "; Undo Move puts it back"
            } else {
                ""
            };
            self.say(
                format!("Moved {} to the trash{undo}", name.unwrap_or_default()),
                false,
            );
            return;
        }
        // The tree shows what the disk says: list the folders again and
        // select the result when it appears.
        let mut requests = self.sidebar.tree.select_when_listed(op.result_path());
        match op {
            Op::Rename { from, to } => {
                requests.extend(self.sidebar.tree.affected_by(from));
                self.start_listings(requests);
                self.follow_move(from, to);
            }
            Op::NewFile(path) => {
                self.start_listings(requests);
                self.open_path(path);
            }
            Op::NewFolder(_) | Op::Trash(_) | Op::Copy { .. } | Op::Restore(_) => {
                self.start_listings(requests);
            }
        }
        if undoing.is_some() {
            self.say("Undone", false);
        }
        self.changed(false);
    }

    /// Undo Move: undo the tree's last move, copy or trash in this run. A
    /// move goes back with a reverse rename, a copy goes to the trash, and
    /// a trashed item is put back (Windows and Linux).
    fn undo_move(&mut self) {
        let Some(last) = self.ws.undo_moves.pop() else {
            self.say("Nothing to undo: no file moved in the tree yet", false);
            return;
        };
        let op = match last {
            UndoMove::Moved { from, to } => {
                if from.exists() {
                    self.say(
                        format!("Cannot undo the move: {} exists again", from.display()),
                        true,
                    );
                    return;
                }
                self.ws.moving.push(to.clone());
                Op::Rename { from: to, to: from }
            }
            UndoMove::Copied(to) => {
                self.ws.moving.push(to.clone());
                Op::Trash(to)
            }
            UndoMove::Trashed(path) => Op::Restore(path),
        };
        self.ws.undo_ops.push(op.clone());
        spawn_op(&self.proxy, op);
    }

    /// The pointer let go of a tree drag over folder `dir`: move `from`
    /// there, or copy it with `copy` (Alt held). A name already taken asks,
    /// as Rename does; a copy into its own folder is named "… copy".
    pub(super) fn drop_in_tree(&mut self, from: &Path, dir: &Path, copy: bool) {
        let Some(name) = from.file_name() else {
            return;
        };
        let same_folder = from.parent() == Some(dir);
        if dir.starts_with(from) || (same_folder && !copy) {
            return; // into itself, or where it already is
        }
        let shown = name.to_string_lossy().into_owned();
        let to = if copy && same_folder {
            tree_ops::copy_name(dir, &shown, from.is_dir())
        } else {
            dir.join(name)
        };
        if to.exists() {
            if copy {
                self.say(format!("{shown} already exists there"), true);
                return;
            }
            self.ask(
                "Replace?",
                format!(
                    "{shown} already exists in this folder. \
                     Replace it with the one you are moving?"
                ),
                &cancel_or("Replace"),
                Asked::Replace {
                    from: from.to_path_buf(),
                    to,
                },
            );
            return;
        }
        let op = if copy {
            Op::Copy {
                from: from.to_path_buf(),
                to,
            }
        } else {
            self.ws.moving.push(from.to_path_buf());
            Op::Rename {
                from: from.to_path_buf(),
                to,
            }
        };
        spawn_op(&self.proxy, op);
    }

    /// The path the tree's commands act on: the selected node, not the
    /// root.
    fn tree_target(&self) -> Option<std::path::PathBuf> {
        let id = self.sidebar.tree.selected?;
        (id != crate::tree::ROOT)
            .then(|| self.sidebar.tree.node(id).map(|n| n.path.clone()))
            .flatten()
    }

    /// Tabs showing `path` or a file inside it, by index.
    fn tabs_under(&self, path: &Path) -> Vec<usize> {
        (0..self.tabs.len())
            .filter(|&i| {
                let d = &self.docs[self.tabs[i].doc];
                let p = if d.is_placeholder() {
                    d.path.as_deref()
                } else {
                    d.canonical.as_deref()
                };
                p.is_some_and(|p| p.starts_with(path))
            })
            .collect()
    }

    /// Move the selected file or folder to the trash. Open tabs with
    /// unsaved edits ask first, since the trash keeps the file on disk but
    /// not the edits; the tabs close once it is gone.
    fn tree_delete(&mut self) {
        let Some(path) = self.tree_target() else {
            self.say("Select a file or folder in the tree first", false);
            return;
        };
        let dirty: Vec<String> = self
            .tabs_under(&path)
            .into_iter()
            .map(|i| &self.docs[self.tabs[i].doc])
            .filter(|d| d.is_dirty() || d.is_saving())
            .map(crate::document::Document::display_name)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !dirty.is_empty() {
            // Asked in the window; the answer trashes it (App::trash).
            self.ask(
                "Unsaved changes",
                format!(
                    "{} has unsaved changes. Move it to the trash and lose them?",
                    dirty.join(", ")
                ),
                &cancel_or("Move to Trash"),
                Asked::Trash(path),
            );
            return;
        }
        self.trash(path);
    }

    /// Move `path` to the trash on a worker.
    pub(super) fn trash(&mut self, path: PathBuf) {
        self.ws.moving.push(path.clone());
        spawn_op(&self.proxy, Op::Trash(path));
    }

    /// Close the tabs of `path` and anything inside it, which are gone,
    /// without asking (the user agreed to lose any edits).
    fn close_tabs_under(&mut self, path: &Path) {
        for i in self.tabs_under(path).into_iter().rev() {
            self.drop_tab(i);
        }
        self.preedit = None;
        self.sync_watch();
    }

    /// Show the selected node, or else the active file, in the system's
    /// file manager.
    fn tree_reveal(&mut self) {
        let target = self
            .tree_target()
            .filter(|_| self.sidebar.visible)
            .or_else(|| self.doc().canonical.clone());
        let Some(path) = target else {
            self.say("Nothing to reveal: the file is not saved yet", false);
            return;
        };
        if let Err(e) = tree_ops::reveal(&path) {
            self.say(format!("Cannot open the file manager: {e}"), true);
        }
    }

    /// Search in the selected folder (a selected file's folder; the first
    /// root when nothing is selected): its path goes in the Search panel's
    /// include field, which then searches it alone.
    fn search_in_folder(&mut self) {
        let tree = &self.sidebar.tree;
        let node = tree.selected.and_then(|s| tree.node(s));
        let dir = match node {
            Some(n) if n.is_dir => n.path.clone(),
            Some(n) => n
                .path
                .parent()
                .map_or_else(|| tree.root().to_path_buf(), Path::to_path_buf),
            None => tree.root().to_path_buf(),
        };
        let shown = crate::search_panel::scope_text(&dir, dirs::home_dir().as_deref());
        self.open_search_panel(false);
        let panel = &mut self.sidebar.search;
        panel.set_include(&shown);
        if !panel.query().pattern.is_empty() {
            self.search_files_at = Some(std::time::Instant::now());
        }
    }

    /// A right-click in the sidebar: select the row under the pointer and
    /// open the tree's context menu there, drawn by Nimbus.
    pub(super) fn tree_context_menu(&mut self) {
        let Some(layout) = self.layout() else { return };
        let (x, y) = (crate::to_f32(self.mouse.x), crate::to_f32(self.mouse.y));
        if !self.sidebar.contains(&layout, x) || !self.sidebar.select_at(&layout, y) {
            return;
        }
        self.find.focused = false;
        self.changed(false);
        self.menus
            .open_context(crate::menu::TREE_MENU, [x, y], &self.keymap);
    }

    /// `from` was renamed to `to`: tabs, recent files and closed tabs
    /// showing it, or anything inside it, follow.
    fn follow_move(&mut self, from: &Path, to: &Path) {
        for (_, doc) in self.docs.iter_mut() {
            let current = if doc.is_placeholder() {
                doc.path.clone()
            } else {
                doc.canonical.clone()
            };
            if let Some(new) = current.and_then(|p| tree_ops::moved(&p, from, to)) {
                doc.moved_to(new);
            }
        }
        for r in &mut self.ws.state.recent {
            if let Some(new) = tree_ops::moved(&r.path, from, to) {
                r.path = new;
            }
        }
        for (p, _) in &mut self.ws.closed {
            if let Some(new) = tree_ops::moved(p, from, to) {
                *p = new;
            }
        }
        self.title_key = None;
        self.sync_watch();
        self.reveal_active();
        self.save_state_soon();
        self.session_changed();
    }
}
