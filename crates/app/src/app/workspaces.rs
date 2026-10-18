//! Workspaces as the app runs them (M19): adding and removing root
//! folders, and saving and opening workspace files
//! ([`crate::workspace_file`]).
//!
//! A child module of `app`, so it can reach the app's fields.

use std::path::Path;

use rfd::FileDialog;

use super::App;
use crate::input::Command;
use crate::io;
use crate::picker::{Kind, Naming};
use crate::sidebar::{Mode, Sidebar};
use crate::tree::folder_name;
use crate::workspace_file;

impl App {
    /// The workspace's commands. Returns `false` for any other.
    pub(super) fn run_roots_command(&mut self, cmd: Command<'_>) -> bool {
        match cmd {
            Command::AddRoot => self.add_root_dialog(),
            Command::RemoveRoot => self.remove_selected_root(),
            Command::SaveWorkspaceAs => self.open_picker(Kind::Name(Naming::Workspace)),
            Command::OpenWorkspace => {
                if workspace_file::names().is_empty() {
                    self.say(
                        "No saved workspaces yet: Save Workspace As… makes one",
                        false,
                    );
                } else {
                    self.open_picker(Kind::Workspaces);
                }
            }
            Command::SaveWorkspaceHere => self.save_workspace_here(),
            Command::OpenProjectWorkspace => self.open_project_workspace(),
            _ => return false,
        }
        self.changed(false);
        true
    }

    /// Ask for a folder and add it to the workspace.
    fn add_root_dialog(&mut self) {
        let mut dialog = FileDialog::new()
            .set_title("Add Folder to Workspace")
            .set_directory(&self.root);
        if let Some(window) = &self.window {
            dialog = dialog.set_parent(window.as_ref());
        }
        if let Some(dir) = dialog.pick_folder() {
            self.add_root(&dir);
        }
    }

    /// Add `dir` to the workspace as another root, open in the sidebar.
    pub(super) fn add_root(&mut self, dir: &Path) {
        let dir = io::canonical(dir);
        if let Err(e) = self.sidebar.tree.add_root(dir.clone()) {
            self.say(e, true);
            return;
        }
        if let Some(id) = self.sidebar.tree.root_of(&dir) {
            // Asked for, so shown open; one restored from a session waits
            // until it is opened.
            let _ = self.sidebar.tree.set_expanded(id, true);
        }
        self.sidebar.mode = Mode::Files;
        if !self.sidebar.visible {
            self.sidebar.visible = true;
            self.remember();
        }
        self.show_sidebar();
        self.sync_watch();
        self.session_changed();
        self.say(
            format!("Added {} to the workspace", folder_name(&dir)),
            false,
        );
    }

    /// Take the root holding the tree's selection out of the workspace.
    /// Its open tabs stay open; only the sidebar, the opener and the search
    /// stop covering it.
    fn remove_selected_root(&mut self) {
        let tree = &self.sidebar.tree;
        let selected = tree
            .selected
            .and_then(|s| tree.node(s))
            .and_then(|n| tree.root_of(&n.path));
        let Some(id) = selected else {
            self.say("Select a folder of the workspace in the tree first", false);
            return;
        };
        let name = tree.node(id).map(|n| n.name.clone()).unwrap_or_default();
        if !self.sidebar.tree.remove_root(id) {
            self.say(
                format!(
                    "{name} is the workspace's first folder and stays; open another folder instead"
                ),
                false,
            );
            return;
        }
        self.sync_watch();
        self.session_changed();
        self.say(format!("Removed {name} from the workspace"), false);
    }

    /// Save the workspace as `name` in the config folder: its roots and
    /// the session as it stands. It is then the workspace shown in the
    /// sidebar's header.
    pub(super) fn save_workspace_as(&mut self, name: &str) {
        let path = match workspace_file::named(name) {
            Ok(p) => p,
            Err(e) => {
                self.say(e.to_string(), true);
                return;
            }
        };
        let replaced = path.exists();
        let session = self.snapshot_session(self.sidebar.tree.root().to_path_buf());
        match workspace_file::save(&path, &session, None) {
            Ok(()) => {
                let name = name.trim().to_owned();
                let note = if replaced {
                    ", replacing the old one"
                } else {
                    ""
                };
                self.say(format!("Saved workspace {name}{note}"), false);
                self.sidebar.workspace = Some(name);
            }
            Err(e) => self.say(format!("Cannot save the workspace: {e}"), true),
        }
    }

    /// A name was typed in a [`Kind::Name`] picker.
    pub(super) fn name_chosen(&mut self, kind: Kind, name: &str) {
        match kind {
            Kind::Name(Naming::Workspace) => self.save_workspace_as(name),
            Kind::Name(Naming::Search) => self.save_search(name),
            _ => {}
        }
    }

    /// Open the workspace saved as `name`.
    pub(super) fn open_named_workspace(&mut self, name: &str) {
        let loaded = workspace_file::named(name).and_then(|p| workspace_file::load(&p, None));
        match loaded {
            Ok(session) if session.root.is_dir() => {
                self.switch_session(&session, Some(name.to_owned()), name);
            }
            Ok(session) => self.say(
                format!(
                    "Cannot open workspace {name}: {} is gone",
                    session.root.display()
                ),
                true,
            ),
            Err(e) => self.say(format!("Cannot open workspace {name}: {e}"), true),
        }
    }

    /// Save the workspace as `.nimbus/workspace.toml` in the first root,
    /// so it travels with the repository. The only file Nimbus writes
    /// inside a project, and only here.
    fn save_workspace_here(&mut self) {
        let root = self.sidebar.tree.root().to_path_buf();
        let path = workspace_file::local(&root);
        let session = self.snapshot_session(root.clone());
        match workspace_file::save(&path, &session, Some(&root)) {
            Ok(()) => self.say(
                format!(
                    "Saved {}/{} in {}; add {}/ to .gitignore unless the project should share it",
                    workspace_file::LOCAL_DIR,
                    workspace_file::LOCAL_FILE,
                    folder_name(&root),
                    workspace_file::LOCAL_DIR
                ),
                false,
            ),
            Err(e) => self.say(format!("Cannot save the workspace: {e}"), true),
        }
    }

    /// Open the first root's `.nimbus/workspace.toml`.
    fn open_project_workspace(&mut self) {
        let root = self.sidebar.tree.root().to_path_buf();
        let path = workspace_file::local(&root);
        let name = folder_name(&root);
        if !path.is_file() {
            self.say(
                format!(
                    "{name} has no {}/{}: Save Workspace Here makes one",
                    workspace_file::LOCAL_DIR,
                    workspace_file::LOCAL_FILE
                ),
                false,
            );
            return;
        }
        match workspace_file::load(&path, Some(&root)) {
            Ok(session) => self.switch_session(&session, None, &format!("{name}'s workspace")),
            Err(e) => self.say(format!("Cannot open the project's workspace: {e}"), true),
        }
    }
}

/// The note offering a folder's own workspace file, when it has one and
/// this machine has no session for it yet (a fresh clone, say): opening it
/// is the user's choice, never automatic.
#[must_use]
pub(super) fn local_workspace_offer(root: &Path, remembered: bool) -> Option<String> {
    (!remembered && workspace_file::local(root).is_file()).then(|| {
        format!(
            "{} has a saved workspace: run Open Project Workspace from the palette to load it",
            folder_name(root)
        )
    })
}

/// The first sidebar and tabs: [`super::workspace::startup_tabs`] for the
/// tabs, then the workspace's roots. Further folders from the command line
/// replace the session's ([`super::workspace::set_more_roots`]), a named
/// workspace shows in the header, and a folder with its own workspace file
/// that this machine has no session for gets an offer to load it, after
/// the session's note.
pub(super) fn startup_tabs_and_roots(
    (root, docs, restore): (
        &Path,
        Vec<crate::document::Document>,
        Option<crate::session::Restore>,
    ),
    (more, workspace, session_root): (Vec<std::path::PathBuf>, Option<String>, Option<&Path>),
    (state, open, warnings): (
        &mut crate::state::State,
        &mut crate::docs::Docs,
        &mut Vec<String>,
    ),
    wrap: bool,
) -> (
    Sidebar,
    (Vec<super::Tab>, super::Groups, usize, Option<String>),
) {
    let mut sidebar = Sidebar::new(
        io::canonical(root),
        state.sidebar_visible,
        state.sidebar_width,
    );
    let remembered = restore.is_some();
    let (tabs, groups, active, note) =
        super::workspace::startup_tabs(docs, restore, state, &mut sidebar, wrap, open);
    super::workspace::set_more_roots(&mut sidebar, more, warnings);
    sidebar.workspace = workspace;
    let offer = session_root.and_then(|r| local_workspace_offer(r, remembered));
    let note = match (note, offer) {
        (Some(n), Some(o)) => Some(format!("{n}; {o}")),
        (n, o) => n.or(o),
    };
    (sidebar, (tabs, groups, active, note))
}
