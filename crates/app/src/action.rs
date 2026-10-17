//! The action registry: every operation a key, menu item or the command
//! palette can trigger.
//!
//! Each action has a stable id (used in the config file, so renaming one is
//! a breaking change), a title shown in the palette and menus, and default
//! key bindings per platform. Keys are bound only through this table and
//! the config overrides on top of it, so nothing is reachable by a
//! hard-coded key alone (DESIGN.md §4.4).

use crate::input::{Command, Motion};

/// When an action applies. A key bound to an action that does not apply
/// right now is treated as unbound, so it can type its character instead
/// (Option+C types "ç" on macOS while the find bar is closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Always.
    Always,
    /// Only while the find bar is open.
    FindBar,
}

/// Default keys for one action. Chords use `Primary` for Command on macOS
/// and Control elsewhere; see `keymap::parse_chord` for the syntax.
#[derive(Debug, Clone, Copy)]
pub struct Keys {
    /// Bound on every platform.
    pub all: &'static [&'static str],
    /// Bound on macOS only.
    pub mac: &'static [&'static str],
    /// Bound on Windows and Linux only.
    pub other: &'static [&'static str],
}

impl Keys {
    const NONE: Self = Self {
        all: &[],
        mac: &[],
        other: &[],
    };

    const fn all(all: &'static [&'static str]) -> Self {
        Self {
            all,
            mac: &[],
            other: &[],
        }
    }

    /// The default chords for the platform this binary was built for.
    pub fn for_this_platform(&self) -> impl Iterator<Item = &'static str> {
        let extra = if cfg!(target_os = "macos") {
            self.mac
        } else {
            self.other
        };
        self.all.iter().chain(extra.iter()).copied()
    }
}

/// One entry of the registry.
#[derive(Debug, Clone, Copy)]
pub struct ActionInfo {
    /// Stable identifier, used in the config file: `"edit.undo"`.
    pub id: &'static str,
    /// Human-readable name for the palette and menus: `"Undo"`.
    pub title: &'static str,
    /// What running the action does.
    pub command: Command<'static>,
    /// Default key bindings.
    pub keys: Keys,
    /// When the action applies.
    pub when: When,
}

const fn act(
    id: &'static str,
    title: &'static str,
    command: Command<'static>,
    keys: Keys,
) -> ActionInfo {
    ActionInfo {
        id,
        title,
        command,
        keys,
        when: When::Always,
    }
}

const fn find_only(
    id: &'static str,
    title: &'static str,
    command: Command<'static>,
    keys: Keys,
) -> ActionInfo {
    ActionInfo {
        id,
        title,
        command,
        keys,
        when: When::FindBar,
    }
}

const fn mv(motion: Motion) -> Command<'static> {
    Command::Move(motion, false)
}

const fn sel(motion: Motion) -> Command<'static> {
    Command::Move(motion, true)
}

const fn mac_other(mac: &'static [&'static str], other: &'static [&'static str]) -> Keys {
    Keys {
        all: &[],
        mac,
        other,
    }
}

/// Every action, in the order the palette lists them when nothing is typed.
pub const ACTIONS: &[ActionInfo] = &[
    // Files and tabs.
    act(
        "file.quickOpen",
        "Go to File…",
        Command::QuickOpen,
        Keys::all(&["Primary+P"]),
    ),
    act(
        "app.palette",
        "Command Palette",
        Command::Palette,
        Keys::all(&["Primary+Shift+P"]),
    ),
    act(
        "file.new",
        "New File",
        Command::NewFile,
        Keys::all(&["Primary+N", "Primary+T"]),
    ),
    act(
        "file.open",
        "Open File…",
        Command::OpenFile,
        Keys::all(&["Primary+O"]),
    ),
    act(
        "file.save",
        "Save",
        Command::Save,
        Keys::all(&["Primary+S"]),
    ),
    act(
        "file.saveAs",
        "Save As…",
        Command::SaveAs,
        Keys::all(&["Primary+Shift+S"]),
    ),
    act(
        "file.close",
        "Close Tab",
        Command::CloseTab,
        Keys::all(&["Primary+W"]),
    ),
    act(
        "app.quit",
        "Quit",
        Command::Close,
        Keys::all(&["Primary+Q"]),
    ),
    act(
        "tab.next",
        "Next Tab",
        Command::NextTab,
        Keys::all(&["Ctrl+Tab"]),
    ),
    act(
        "tab.previous",
        "Previous Tab",
        Command::PrevTab,
        Keys::all(&["Ctrl+Shift+Tab"]),
    ),
    act(
        "tab.goto1",
        "Go to Tab 1",
        Command::GoToTab(0),
        Keys::all(&["Primary+1"]),
    ),
    act(
        "tab.goto2",
        "Go to Tab 2",
        Command::GoToTab(1),
        Keys::all(&["Primary+2"]),
    ),
    act(
        "tab.goto3",
        "Go to Tab 3",
        Command::GoToTab(2),
        Keys::all(&["Primary+3"]),
    ),
    act(
        "tab.goto4",
        "Go to Tab 4",
        Command::GoToTab(3),
        Keys::all(&["Primary+4"]),
    ),
    act(
        "tab.goto5",
        "Go to Tab 5",
        Command::GoToTab(4),
        Keys::all(&["Primary+5"]),
    ),
    act(
        "tab.goto6",
        "Go to Tab 6",
        Command::GoToTab(5),
        Keys::all(&["Primary+6"]),
    ),
    act(
        "tab.goto7",
        "Go to Tab 7",
        Command::GoToTab(6),
        Keys::all(&["Primary+7"]),
    ),
    act(
        "tab.goto8",
        "Go to Tab 8",
        Command::GoToTab(7),
        Keys::all(&["Primary+8"]),
    ),
    act(
        "tab.gotoLast",
        "Go to Last Tab",
        Command::GoToTab(usize::MAX),
        Keys::all(&["Primary+9"]),
    ),
    act(
        "app.reloadConfig",
        "Reload Config",
        Command::ReloadConfig,
        Keys::NONE,
    ),
    act(
        "app.openConfig",
        "Open Config File",
        Command::OpenConfig,
        Keys::NONE,
    ),
    // Compare (M9).
    act(
        "diff.compareSaved",
        "Compare with Saved",
        Command::CompareSaved,
        Keys::NONE,
    ),
    act(
        "diff.compareFile",
        "Compare with File…",
        Command::CompareFile,
        Keys::NONE,
    ),
    act(
        "diff.compareTab",
        "Compare with Tab…",
        Command::CompareTab,
        Keys::NONE,
    ),
    act(
        "diff.next",
        "Next Change",
        Command::DiffNext,
        Keys::all(&["F7"]),
    ),
    act(
        "diff.previous",
        "Previous Change",
        Command::DiffPrev,
        Keys::all(&["Shift+F7"]),
    ),
    act(
        "diff.toggleUnified",
        "Toggle Unified Compare",
        Command::DiffUnified,
        Keys::NONE,
    ),
    act(
        "diff.close",
        "Close Compare or Merge",
        Command::DiffClose,
        Keys::NONE,
    ),
    act(
        "merge.files",
        "Merge Files…",
        Command::MergeFiles,
        Keys::NONE,
    ),
    act(
        "merge.withBase",
        "Merge with Base…",
        Command::MergeWithBase,
        Keys::NONE,
    ),
    act("merge.takeLeft", "Take Left", Command::TakeLeft, Keys::NONE),
    act(
        "merge.takeRight",
        "Take Right",
        Command::TakeRight,
        Keys::NONE,
    ),
    act("merge.takeBoth", "Take Both", Command::TakeBoth, Keys::NONE),
    act(
        "merge.takeBothReversed",
        "Take Both, Right First",
        Command::TakeBothReversed,
        Keys::NONE,
    ),
    act(
        "merge.nextConflict",
        "Next Conflict",
        Command::NextConflict,
        Keys::all(&["F8"]),
    ),
    act(
        "merge.previousConflict",
        "Previous Conflict",
        Command::PrevConflict,
        Keys::all(&["Shift+F8"]),
    ),
    act(
        "merge.resolveMarkers",
        "Resolve Conflicts",
        Command::ResolveMarkers,
        Keys::NONE,
    ),
    act(
        "merge.withDisk",
        "Merge with Disk",
        Command::MergeWithDisk,
        Keys::NONE,
    ),
    // Editing.
    act(
        "edit.undo",
        "Undo",
        Command::Undo,
        Keys::all(&["Primary+Z"]),
    ),
    act(
        "edit.redo",
        "Redo",
        Command::Redo,
        Keys {
            all: &["Primary+Shift+Z"],
            mac: &[],
            other: &["Ctrl+Y"],
        },
    ),
    act("edit.cut", "Cut", Command::Cut, Keys::all(&["Primary+X"])),
    act(
        "edit.copy",
        "Copy",
        Command::Copy,
        Keys::all(&["Primary+C"]),
    ),
    act(
        "edit.paste",
        "Paste",
        Command::Paste,
        Keys::all(&["Primary+V"]),
    ),
    act(
        "edit.selectAll",
        "Select All",
        Command::SelectAll,
        Keys::all(&["Primary+A"]),
    ),
    act(
        "selection.addNextOccurrence",
        "Add Next Occurrence",
        Command::AddNextOccurrence,
        Keys::all(&["Primary+D"]),
    ),
    act(
        "selection.addCursorAbove",
        "Add Cursor Above",
        Command::AddCursorAbove,
        Keys::all(&["Primary+Alt+Up"]),
    ),
    act(
        "selection.addCursorBelow",
        "Add Cursor Below",
        Command::AddCursorBelow,
        Keys::all(&["Primary+Alt+Down"]),
    ),
    act(
        "edit.newline",
        "Insert Line Break",
        Command::Newline,
        Keys::all(&["Enter", "Shift+Enter"]),
    ),
    act(
        "edit.tab",
        "Indent / Insert Tab",
        Command::Tab,
        Keys::all(&["Tab"]),
    ),
    act(
        "edit.outdent",
        "Outdent",
        Command::Outdent,
        Keys::all(&["Shift+Tab"]),
    ),
    act(
        "edit.deleteBack",
        "Delete Backward",
        Command::DeleteBack,
        Keys::all(&["Backspace", "Shift+Backspace"]),
    ),
    act(
        "edit.deleteForward",
        "Delete Forward",
        Command::DeleteForward,
        Keys::all(&["Delete", "Shift+Delete"]),
    ),
    act(
        "edit.deleteWordBack",
        "Delete Word Backward",
        Command::DeleteWordBack,
        mac_other(&["Alt+Backspace"], &["Ctrl+Backspace"]),
    ),
    act(
        "edit.deleteWordForward",
        "Delete Word Forward",
        Command::DeleteWordForward,
        mac_other(&["Alt+Delete"], &["Ctrl+Delete"]),
    ),
    act(
        "edit.deleteToLineStart",
        "Delete to Line Start",
        Command::DeleteToLineStart,
        mac_other(&["Primary+Backspace"], &[]),
    ),
    act(
        "edit.cancel",
        "Cancel",
        Command::Escape,
        Keys::all(&["Escape"]),
    ),
    // Find.
    act(
        "find.open",
        "Find",
        Command::Find,
        Keys::all(&["Primary+F"]),
    ),
    act(
        "find.replace",
        "Replace",
        Command::Replace,
        mac_other(&["Primary+Alt+F"], &["Ctrl+H"]),
    ),
    find_only(
        "find.toggleCase",
        "Toggle Match Case",
        Command::ToggleCase(""),
        Keys::all(&["Alt+C"]),
    ),
    find_only(
        "find.toggleRegex",
        "Toggle Regular Expression",
        Command::ToggleRegex(""),
        Keys::all(&["Alt+R"]),
    ),
    find_only(
        "find.replaceAll",
        "Replace All",
        Command::ReplaceAll,
        Keys::all(&["Primary+Enter"]),
    ),
    // View.
    act(
        "view.toggleWrap",
        "Toggle Soft Wrap",
        Command::ToggleWrap,
        Keys::all(&["Alt+Z"]),
    ),
    act(
        "edit.goToLine",
        "Go to Line…",
        Command::GoToLine,
        mac_other(&["Primary+L"], &["Primary+G"]),
    ),
    act(
        "edit.goToSymbol",
        "Go to Symbol…",
        Command::GoToSymbol,
        Keys::all(&["Primary+Shift+R"]),
    ),
    act(
        "view.toggleSidebar",
        "Toggle Sidebar",
        Command::ToggleSidebar,
        Keys::all(&["Primary+B"]),
    ),
    act(
        "view.focusSidebar",
        "Focus Sidebar",
        Command::FocusSidebar,
        Keys::all(&["Primary+Shift+E"]),
    ),
    act(
        "view.toggleDebugOverlay",
        "Toggle Debug Overlay",
        Command::ToggleDebugOverlay,
        Keys::all(&["F12"]),
    ),
    act(
        "view.toggleIgnored",
        "Show/Hide Ignored Files in Sidebar",
        Command::ToggleIgnored,
        Keys::NONE,
    ),
    act(
        "file.reloadFromDisk",
        "Reload from Disk",
        Command::ReloadFromDisk,
        Keys::NONE,
    ),
    act(
        "file.keepMine",
        "Keep My Version (ignore the change on disk)",
        Command::KeepMine,
        Keys::NONE,
    ),
    act(
        "view.toggleTheme",
        "Toggle Light/Dark Theme",
        Command::ToggleTheme,
        Keys::NONE,
    ),
    // Cursor movement and selection.
    act(
        "cursor.left",
        "Cursor Left",
        mv(Motion::Left),
        Keys::all(&["Left"]),
    ),
    act(
        "cursor.right",
        "Cursor Right",
        mv(Motion::Right),
        Keys::all(&["Right"]),
    ),
    act("cursor.up", "Cursor Up", mv(Motion::Up), Keys::all(&["Up"])),
    act(
        "cursor.down",
        "Cursor Down",
        mv(Motion::Down),
        Keys::all(&["Down"]),
    ),
    act(
        "cursor.wordLeft",
        "Cursor Word Left",
        mv(Motion::WordLeft),
        mac_other(&["Alt+Left"], &["Ctrl+Left"]),
    ),
    act(
        "cursor.wordRight",
        "Cursor Word Right",
        mv(Motion::WordRight),
        mac_other(&["Alt+Right"], &["Ctrl+Right"]),
    ),
    act(
        "cursor.lineStart",
        "Cursor to Line Start",
        mv(Motion::LineStart),
        mac_other(&["Home", "Primary+Left"], &["Home"]),
    ),
    act(
        "cursor.lineEnd",
        "Cursor to Line End",
        mv(Motion::LineEnd),
        mac_other(&["End", "Primary+Right"], &["End"]),
    ),
    act(
        "cursor.pageUp",
        "Page Up",
        mv(Motion::PageUp),
        Keys::all(&["PageUp"]),
    ),
    act(
        "cursor.pageDown",
        "Page Down",
        mv(Motion::PageDown),
        Keys::all(&["PageDown"]),
    ),
    act(
        "cursor.docStart",
        "Cursor to Start of File",
        mv(Motion::DocStart),
        mac_other(
            &["Primary+Up", "Primary+Home", "Ctrl+Home"],
            &["Ctrl+Home", "Ctrl+Up"],
        ),
    ),
    act(
        "cursor.docEnd",
        "Cursor to End of File",
        mv(Motion::DocEnd),
        mac_other(
            &["Primary+Down", "Primary+End", "Ctrl+End"],
            &["Ctrl+End", "Ctrl+Down"],
        ),
    ),
    act(
        "select.left",
        "Select Left",
        sel(Motion::Left),
        Keys::all(&["Shift+Left"]),
    ),
    act(
        "select.right",
        "Select Right",
        sel(Motion::Right),
        Keys::all(&["Shift+Right"]),
    ),
    act(
        "select.up",
        "Select Up",
        sel(Motion::Up),
        Keys::all(&["Shift+Up"]),
    ),
    act(
        "select.down",
        "Select Down",
        sel(Motion::Down),
        Keys::all(&["Shift+Down"]),
    ),
    act(
        "select.wordLeft",
        "Select Word Left",
        sel(Motion::WordLeft),
        mac_other(&["Alt+Shift+Left"], &["Ctrl+Shift+Left"]),
    ),
    act(
        "select.wordRight",
        "Select Word Right",
        sel(Motion::WordRight),
        mac_other(&["Alt+Shift+Right"], &["Ctrl+Shift+Right"]),
    ),
    act(
        "select.lineStart",
        "Select to Line Start",
        sel(Motion::LineStart),
        mac_other(&["Shift+Home", "Primary+Shift+Left"], &["Shift+Home"]),
    ),
    act(
        "select.lineEnd",
        "Select to Line End",
        sel(Motion::LineEnd),
        mac_other(&["Shift+End", "Primary+Shift+Right"], &["Shift+End"]),
    ),
    act(
        "select.pageUp",
        "Select Page Up",
        sel(Motion::PageUp),
        Keys::all(&["Shift+PageUp"]),
    ),
    act(
        "select.pageDown",
        "Select Page Down",
        sel(Motion::PageDown),
        Keys::all(&["Shift+PageDown"]),
    ),
    act(
        "select.docStart",
        "Select to Start of File",
        sel(Motion::DocStart),
        mac_other(
            &["Primary+Shift+Up", "Primary+Shift+Home"],
            &["Ctrl+Shift+Home"],
        ),
    ),
    act(
        "select.docEnd",
        "Select to End of File",
        sel(Motion::DocEnd),
        mac_other(
            &["Primary+Shift+Down", "Primary+Shift+End"],
            &["Ctrl+Shift+End"],
        ),
    ),
];

/// Look an action up by its id.
#[must_use]
pub fn by_id(id: &str) -> Option<&'static ActionInfo> {
    ACTIONS.iter().find(|a| a.id == id)
}

/// Actions worth listing in the command palette: everything except plain
/// cursor movement and typing keys, which nobody searches for by name.
pub fn palette_actions() -> impl Iterator<Item = &'static ActionInfo> {
    ACTIONS.iter().filter(|a| {
        !a.id.starts_with("cursor.")
            && !a.id.starts_with("select.")
            && !matches!(
                a.id,
                "edit.newline"
                    | "edit.tab"
                    | "edit.deleteBack"
                    | "edit.deleteForward"
                    | "edit.cancel"
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_and_titles_are_unique_and_well_formed() {
        let mut ids = HashSet::new();
        let mut titles = HashSet::new();
        for a in ACTIONS {
            assert!(ids.insert(a.id), "duplicate id {}", a.id);
            assert!(titles.insert(a.title), "duplicate title {}", a.title);
            let (group, name) = a.id.split_once('.').unwrap();
            assert!(!group.is_empty() && !name.is_empty(), "{}", a.id);
        }
        assert_eq!(by_id("edit.undo").map(|a| a.title), Some("Undo"));
        assert!(by_id("no.such").is_none());
    }

    #[test]
    fn palette_hides_movement_and_typing_keys() {
        assert!(palette_actions().any(|a| a.id == "file.save"));
        assert!(!palette_actions().any(|a| a.id == "cursor.left" || a.id == "edit.newline"));
    }
}
