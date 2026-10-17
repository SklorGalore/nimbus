//! Turning key presses into editor commands.
//!
//! winit's `KeyEvent` cannot be built in tests, so [`resolve`] takes its
//! parts instead. Which key does what lives in the action registry and the
//! keymap; this module only decides between running an action and typing.

#[cfg(test)]
use winit::keyboard::KeyCode;
use winit::keyboard::{Key, ModifiersState, PhysicalKey};

use crate::action::When;
use crate::keymap::Keymap;

/// A cursor movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One grapheme left.
    Left,
    /// One grapheme right.
    Right,
    /// To the start of the previous word.
    WordLeft,
    /// To the end of the next word.
    WordRight,
    /// One line up, keeping the preferred column.
    Up,
    /// One line down, keeping the preferred column.
    Down,
    /// First non-blank character of the line, or column 0 if already there.
    LineStart,
    /// End of the line's content.
    LineEnd,
    /// One screenful up.
    PageUp,
    /// One screenful down.
    PageDown,
    /// Start of the document.
    DocStart,
    /// End of the document.
    DocEnd,
}

/// What a key press asks the editor to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command<'a> {
    /// Move the cursor; with `extend` (Shift held), grow the selection.
    Move(Motion, bool),
    /// Type this text at the cursor.
    Insert(&'a str),
    /// Insert the document's line ending plus the current indentation.
    Newline,
    /// Insert a tab, or indent the selected lines.
    Tab,
    /// Remove one level of indentation from the selected lines.
    Outdent,
    /// Delete the grapheme before the cursor, or the selection.
    DeleteBack,
    /// Delete the grapheme after the cursor, or the selection.
    DeleteForward,
    /// Delete back to the start of the word, or the selection.
    DeleteWordBack,
    /// Delete forward to the end of the word, or the selection.
    DeleteWordForward,
    /// Delete back to the start of the line, or the selection.
    DeleteToLineStart,
    /// Undo the last change.
    Undo,
    /// Redo the last undone change.
    Redo,
    /// Select the whole document.
    SelectAll,
    /// Copy the selection to the clipboard and delete it.
    Cut,
    /// Copy the selection to the clipboard.
    Copy,
    /// Insert the clipboard's text.
    Paste,
    /// Open the find bar.
    Find,
    /// Open the find bar with the replace field.
    Replace,
    /// Toggle soft wrap.
    ToggleWrap,
    /// Switch between the light and dark themes.
    ToggleTheme,
    /// Show or hide the sidebar.
    ToggleSidebar,
    /// Ask for a line number and move the cursor there.
    GoToLine,
    /// Show the sidebar and move the keyboard into it.
    FocusSidebar,
    /// Show or hide `.gitignore`d files in the sidebar.
    ToggleIgnored,
    /// Show or hide the debug overlay: frame time, latency, atlas use.
    ToggleDebugOverlay,
    /// Select the word under the cursor, or add the next occurrence of the
    /// selection as another cursor.
    AddNextOccurrence,
    /// Add a cursor on the line above the primary.
    AddCursorAbove,
    /// Add a cursor on the line below the primary.
    AddCursorBelow,
    /// Replace the text with the file on disk.
    ReloadFromDisk,
    /// Keep the edits here over a change on disk.
    KeepMine,
    /// Find bar: toggle case-sensitive matching. Carries the text the key
    /// would type (Option+C is "ç" on macOS), typed instead when the find
    /// bar is closed.
    ToggleCase(&'a str),
    /// Find bar: toggle regular-expression matching; carries text likewise.
    ToggleRegex(&'a str),
    /// Find bar: replace every match.
    ReplaceAll,
    /// Close the find bar, or collapse the selection.
    Escape,
    /// Save the document.
    Save,
    /// Save under a new name, chosen in a dialog.
    SaveAs,
    /// Open a file chosen in a dialog.
    OpenFile,
    /// Open an untitled tab.
    NewFile,
    /// Close the current tab (asking about unsaved changes first).
    CloseTab,
    /// Switch to the next tab.
    NextTab,
    /// Switch to the previous tab.
    PrevTab,
    /// Switch to tab n (0-based); `usize::MAX` is the last tab.
    GoToTab(usize),
    /// Open the fuzzy file opener.
    QuickOpen,
    /// Open the command palette.
    Palette,
    /// Read the config file again.
    ReloadConfig,
    /// Open the config file in a tab, creating it if needed.
    OpenConfig,
    /// Ask GitHub whether a newer release exists.
    CheckUpdates,
    /// Open the latest release's page in the browser.
    OpenRelease,
    /// Quit (asking about unsaved changes first).
    /// Compare the active tab with its file on disk (M9).
    CompareSaved,
    /// Compare the active tab with a file chosen in a dialog.
    CompareFile,
    /// Compare the active tab with another open tab.
    CompareTab,
    /// Go to the next change in a compare.
    DiffNext,
    /// Go to the previous change in a compare.
    DiffPrev,
    /// Switch a compare between two columns and one.
    DiffUnified,
    /// Leave the compare, keeping the tab.
    DiffClose,
    /// Merge two files into a new tab (M9).
    MergeFiles,
    /// Merge the active tab with another file, given their common base.
    MergeWithBase,
    /// In a merge: take the left column's lines for the change at the cursor.
    TakeLeft,
    /// In a merge: take the right column's lines.
    TakeRight,
    /// In a merge: take both, left first.
    TakeBoth,
    /// In a merge: take both, right first.
    TakeBothReversed,
    /// In a merge: go to the next open conflict.
    NextConflict,
    /// In a merge: go to the previous open conflict.
    PrevConflict,
    /// Open the three-way merge view on a file with conflict markers.
    ResolveMarkers,
    /// Merge unsaved edits with the file as it changed on disk.
    MergeWithDisk,
    Close,
}

impl Command<'_> {
    /// Whether the command changes the document's text when the editor
    /// has focus. Such commands wait while the document is being saved.
    /// (Replace and Replace All change it too; they run from the find bar,
    /// which checks for itself.)
    #[must_use]
    pub fn edits_text(self) -> bool {
        matches!(
            self,
            Command::Insert(_)
                | Command::Newline
                | Command::Tab
                | Command::Outdent
                | Command::DeleteBack
                | Command::DeleteForward
                | Command::DeleteWordBack
                | Command::DeleteWordForward
                | Command::DeleteToLineStart
                | Command::Undo
                | Command::Redo
                | Command::Cut
                | Command::Paste
        )
    }
}

/// Whether text from a key press with these modifiers should be typed.
///
/// macOS reports Cmd+S with text "s", so Cmd and Ctrl chords must not type.
/// Option on macOS composes characters (å, ©), so it may. Elsewhere Ctrl+Alt
/// is how `AltGr` arrives on Windows (a German keyboard types @ with it), so
/// text is allowed when both or neither are held.
fn may_insert_text(m: ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        !m.super_key() && !m.control_key()
    } else {
        !m.super_key() && m.control_key() == m.alt_key()
    }
}

/// The parts of a key press [`map_key`] looks at.
#[derive(Debug, Clone, Copy)]
pub struct KeyPress<'a> {
    /// The key as the current layout names it.
    pub key: &'a Key,
    /// Where the key is on the keyboard, independent of layout. Used for
    /// chords whose character the layout changes, like Option+Z (Ω).
    pub physical: PhysicalKey,
    /// What the key types, if anything.
    pub text: Option<&'a str>,
    /// Pressed rather than released.
    pub pressed: bool,
    /// An auto-repeat from holding the key.
    pub repeat: bool,
    /// Modifiers held.
    pub mods: ModifiersState,
}

/// Whether holding the key down should repeat the command. Movement,
/// typing, deletion and undo repeat; opening things and saving do not.
fn repeats(cmd: Command<'_>) -> bool {
    matches!(
        cmd,
        Command::Move(..)
            | Command::Insert(_)
            | Command::Newline
            | Command::Tab
            | Command::Outdent
            | Command::DeleteBack
            | Command::DeleteForward
            | Command::DeleteWordBack
            | Command::DeleteWordForward
            | Command::DeleteToLineStart
            | Command::Undo
            | Command::Redo
    )
}

/// Turn a key press into a command: the action bound to it in `keymap` if
/// that action applies now (`find_open` says whether find-bar actions do),
/// otherwise the text the key types, if any.
pub fn resolve<'a>(keymap: &Keymap, k: KeyPress<'a>, find_open: bool) -> Option<Command<'a>> {
    if !k.pressed {
        return None;
    }
    if let Some(action) = keymap.lookup(k.key, k.physical, k.mods) {
        let applies = match action.when {
            When::Always => true,
            When::FindBar => find_open,
        };
        if applies {
            return (!k.repeat || repeats(action.command)).then_some(action.command);
        }
    }
    let text = k.text?;
    // Control characters (\r, \x08, \x1b, \x7f) arrive as text on some
    // platforms for keys like Enter; never type them.
    (may_insert_text(k.mods) && !text.chars().any(char::is_control))
        .then_some(Command::Insert(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use winit::keyboard::NamedKey;

    fn primary() -> ModifiersState {
        if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        }
    }

    fn press<'a>(
        key: &'a Key,
        code: KeyCode,
        text: Option<&'a str>,
        mods: ModifiersState,
    ) -> KeyPress<'a> {
        KeyPress {
            key,
            physical: PhysicalKey::Code(code),
            text,
            pressed: true,
            repeat: false,
            mods,
        }
    }

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    fn km() -> Keymap {
        Keymap::new(&HashMap::new()).0
    }

    #[test]
    fn primary_chords_never_type_and_save_does_not_repeat() {
        let km = km();
        let s = ch("s");
        assert_eq!(
            resolve(&km, press(&s, KeyCode::KeyS, Some("s"), primary()), false),
            Some(Command::Save)
        );
        let held = KeyPress {
            repeat: true,
            ..press(&s, KeyCode::KeyS, Some("s"), primary())
        };
        assert_eq!(resolve(&km, held, false), None);
        let z = ch("z");
        let held = KeyPress {
            repeat: true,
            ..press(&z, KeyCode::KeyZ, Some("z"), primary())
        };
        assert_eq!(
            resolve(&km, held, false),
            Some(Command::Undo),
            "undo repeats"
        );
        let unbound = ch("j");
        assert_eq!(
            resolve(
                &km,
                press(&unbound, KeyCode::KeyJ, Some("j"), primary()),
                false
            ),
            None
        );
    }

    #[test]
    fn plain_keys_type_and_named_keys_act() {
        let km = km();
        let none = ModifiersState::empty();
        let a = ch("a");
        assert_eq!(
            resolve(&km, press(&a, KeyCode::KeyA, Some("a"), none), false),
            Some(Command::Insert("a"))
        );
        let space = Key::Named(NamedKey::Space);
        assert_eq!(
            resolve(&km, press(&space, KeyCode::Space, Some(" "), none), false),
            Some(Command::Insert(" "))
        );
        let enter = Key::Named(NamedKey::Enter);
        assert_eq!(
            resolve(&km, press(&enter, KeyCode::Enter, Some("\r"), none), false),
            Some(Command::Newline)
        );
        let esc = Key::Named(NamedKey::Escape);
        assert_eq!(
            resolve(&km, press(&esc, KeyCode::Escape, Some("\x1b"), none), false),
            Some(Command::Escape)
        );
        let left = Key::Named(NamedKey::ArrowLeft);
        assert_eq!(
            resolve(
                &km,
                press(&left, KeyCode::ArrowLeft, None, ModifiersState::SHIFT),
                false
            ),
            Some(Command::Move(Motion::Left, true))
        );
        let release = KeyPress {
            pressed: false,
            ..press(&a, KeyCode::KeyA, Some("a"), none)
        };
        assert_eq!(resolve(&km, release, false), None);
    }

    #[test]
    fn find_bar_keys_type_when_the_bar_is_closed() {
        let km = km();
        let c = ch("ç");
        let k = press(&c, KeyCode::KeyC, Some("ç"), ModifiersState::ALT);
        assert_eq!(resolve(&km, k, true), Some(Command::ToggleCase("")));
        if cfg!(target_os = "macos") {
            assert_eq!(resolve(&km, k, false), Some(Command::Insert("ç")));
        }
    }

    #[test]
    fn alt_z_toggles_wrap_by_physical_key() {
        let km = km();
        let omega = ch("Ω");
        let k = press(&omega, KeyCode::KeyZ, Some("Ω"), ModifiersState::ALT);
        assert_eq!(resolve(&km, k, false), Some(Command::ToggleWrap));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn altgr_types_off_macos() {
        let km = km();
        let at = ch("@");
        let altgr = ModifiersState::CONTROL | ModifiersState::ALT;
        assert_eq!(
            resolve(&km, press(&at, KeyCode::KeyQ, Some("@"), altgr), false),
            Some(Command::Insert("@"))
        );
    }
}
