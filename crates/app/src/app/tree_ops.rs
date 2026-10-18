//! The tree's file operations as the app runs them (M12): the inline name
//! field's keys, starting each operation on a worker, and what follows
//! when it is done (tabs follow a rename, a new file opens).

use std::path::Path;

use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

use super::{App, AppEvent, edit_field};
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
        let name = edit.field.buffer.line(0).into_owned();
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
                if clash && !self.confirm_replace(&name) {
                    self.sidebar.edit = Some(edit);
                    return;
                }
                self.ws.moving.push(from.clone());
                Op::Rename { from, to }
            }
        };
        self.sidebar.focused = true;
        spawn_op(&self.proxy, op);
    }

    /// Ask whether a rename may replace the existing `name`.
    fn confirm_replace(&self, name: &str) -> bool {
        let mut dialog = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Replace?")
            .set_description(format!(
                "{name} already exists in this folder. Replace it with the one you are renaming?"
            ))
            .set_buttons(MessageButtons::OkCancelCustom(
                "Replace".into(),
                "Cancel".into(),
            ));
        if let Some(window) = &self.window {
            dialog = dialog.set_parent(window.as_ref());
        }
        // Some platforms report a custom button by its label.
        match dialog.show() {
            MessageDialogResult::Ok => true,
            MessageDialogResult::Custom(label) => label == "Replace",
            _ => false,
        }
    }

    /// A file operation finished on its worker.
    pub(super) fn file_op_done(&mut self, op: &Op, result: Result<(), String>) {
        if let Op::Rename { from, .. } = op {
            self.ws.moving.retain(|p| p != from);
        }
        if let Err(e) = result {
            let what = match op {
                Op::NewFile(_) => "create the file",
                Op::NewFolder(_) => "create the folder",
                Op::Rename { .. } => "rename",
            };
            self.say(format!("Cannot {what}: {e}"), true);
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
            Op::NewFolder(_) => self.start_listings(requests),
        }
        self.changed(false);
    }

    /// `from` was renamed to `to`: tabs, recent files and closed tabs
    /// showing it, or anything inside it, follow.
    fn follow_move(&mut self, from: &Path, to: &Path) {
        for tab in &mut self.tabs {
            let current = if tab.doc.is_placeholder() {
                tab.doc.path.clone()
            } else {
                tab.doc.canonical.clone()
            };
            if let Some(new) = current.and_then(|p| tree_ops::moved(&p, from, to)) {
                tab.doc.moved_to(new);
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
