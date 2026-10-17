//! Turning key presses into editor commands.
//!
//! winit's `KeyEvent` cannot be built in tests, so [`resolve`] takes its
//! parts instead. Which key does what lives in the action registry and the
//! keymap; this module only decides between running an action and typing.

#[cfg(test)]
use winit::keyboard::KeyCode;
use winit::keyboard::{Key, ModifiersState, PhysicalKey};

use crate::action::{ActionInfo, When};
use crate::keymap::{Chord, Keymap, Lookup};

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
    /// Comment or uncomment the selected lines with the language's line
    /// comment.
    ToggleLineComment,
    /// Wrap the selection in the language's block comment, or unwrap it.
    ToggleBlockComment,
    /// Insert the selection again after itself, or the cursor's line below.
    Duplicate,
    /// Swap the selected lines with the line above.
    MoveLineUp,
    /// Swap the selected lines with the line below.
    MoveLineDown,
    /// Delete the selected lines whole.
    DeleteLine,
    /// Join each selection's lines into one.
    JoinLines,
    /// Open a new line below the cursor's, without breaking it.
    InsertLineBelow,
    /// Open a new line above the cursor's.
    InsertLineAbove,
    /// Select the cursor's line; again, the line below too.
    SelectLine,
    /// Grow the selection to the word, pair, line, then everything.
    ExpandSelection,
    /// Undo the last Expand Selection.
    ShrinkSelection,
    /// Move to the bracket pairing with the one at the cursor; with
    /// `true`, select up to it.
    MatchingBracket(bool),
    /// Change the selected text's case.
    ChangeCase(nimbus_core::transform::Case),
    /// Sort, reverse-sort or deduplicate the selected lines.
    RewriteLines(nimbus_core::transform::Lines),
    /// Remove trailing whitespace from the selected lines, or all.
    TrimWhitespace,
    /// Indent this file with spaces, at the configured width.
    IndentUsingSpaces,
    /// Indent this file with tabs.
    IndentUsingTabs,
    /// Work the indentation out from the file again.
    DetectIndent,
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
    /// Quit (asking about unsaved changes first).
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
                | Command::ToggleLineComment
                | Command::ToggleBlockComment
                | Command::Duplicate
                | Command::MoveLineUp
                | Command::MoveLineDown
                | Command::DeleteLine
                | Command::JoinLines
                | Command::InsertLineBelow
                | Command::InsertLineAbove
                | Command::ChangeCase(_)
                | Command::RewriteLines(_)
                | Command::TrimWhitespace
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
            | Command::Duplicate
            | Command::MoveLineUp
            | Command::MoveLineDown
            | Command::DeleteLine
            | Command::JoinLines
            | Command::SelectLine
            | Command::ExpandSelection
            | Command::ShrinkSelection
    )
}

/// What a key press resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved<'a> {
    /// Run this.
    Command(Command<'a>),
    /// The first chord of a two-chord binding: wait for the second.
    Prefix(Chord),
    /// Nothing: an unbound chord, a key release, or a modifier alone.
    Nothing,
}

/// Turn a key press into a command: the action bound to it in `keymap` if
/// that action applies now (`find_open` says whether find-bar actions do),
/// otherwise the text the key types, if any.
///
/// With `pending`, the first chord of a two-chord binding was just
/// pressed: the key completes it, or cancels it, and never types. A
/// modifier pressed on its own is ignored, so holding Ctrl for the second
/// chord does not cancel the first.
pub fn resolve<'a>(
    keymap: &Keymap,
    k: KeyPress<'a>,
    find_open: bool,
    pending: Option<Chord>,
) -> Resolved<'a> {
    if !k.pressed {
        return Resolved::Nothing;
    }
    let applies = |action: &ActionInfo| match action.when {
        When::Always => true,
        When::FindBar => find_open,
    };
    if let Some(first) = pending {
        if is_modifier(k.key) {
            return Resolved::Prefix(first);
        }
        return match keymap.lookup_second(first, k.key, k.physical, k.mods) {
            Some(action) if applies(action) && !k.repeat => Resolved::Command(action.command),
            _ => Resolved::Nothing,
        };
    }
    match keymap.lookup(k.key, k.physical, k.mods, find_open) {
        Lookup::Action(action) if applies(action) => {
            return if !k.repeat || repeats(action.command) {
                Resolved::Command(action.command)
            } else {
                Resolved::Nothing
            };
        }
        Lookup::Prefix(chord) if !k.repeat => return Resolved::Prefix(chord),
        _ => {}
    }
    let Some(text) = k.text else {
        return Resolved::Nothing;
    };
    // Control characters (\r, \x08, \x1b, \x7f) arrive as text on some
    // platforms for keys like Enter; never type them.
    if may_insert_text(k.mods) && !text.chars().any(char::is_control) {
        Resolved::Command(Command::Insert(text))
    } else {
        Resolved::Nothing
    }
}

/// Whether the key is a modifier on its own (Shift, Control, Alt, Super).
fn is_modifier(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            winit::keyboard::NamedKey::Shift
                | winit::keyboard::NamedKey::Control
                | winit::keyboard::NamedKey::Alt
                | winit::keyboard::NamedKey::Super
                | winit::keyboard::NamedKey::Meta
                | winit::keyboard::NamedKey::AltGraph
        )
    )
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

    /// The command a press resolves to, with no chord pending.
    fn resolve<'a>(km: &Keymap, k: KeyPress<'a>, find_open: bool) -> Option<Command<'a>> {
        match super::resolve(km, k, find_open, None) {
            Resolved::Command(c) => Some(c),
            Resolved::Prefix(_) | Resolved::Nothing => None,
        }
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

    #[test]
    fn two_chord_bindings_wait_for_the_second_chord() {
        let mut o = HashMap::new();
        o.insert("edit.toggleLineComment".to_owned(), vec!["Ctrl+K Ctrl+C".to_owned()]);
        let (km, warnings) = Keymap::new(&o);
        assert_eq!(warnings, Vec::<String>::new());
        let k = ch("k");
        let first = super::resolve(
            &km,
            press(&k, KeyCode::KeyK, Some("k"), ModifiersState::CONTROL),
            false,
            None,
        );
        let Resolved::Prefix(chord) = first else {
            panic!("{first:?}");
        };
        // Releasing and pressing Control again does not cancel.
        let ctrl = Key::Named(NamedKey::Control);
        assert_eq!(
            super::resolve(
                &km,
                press(&ctrl, KeyCode::ControlLeft, None, ModifiersState::CONTROL),
                false,
                Some(chord)
            ),
            Resolved::Prefix(chord)
        );
        let c = ch("c");
        assert_eq!(
            super::resolve(
                &km,
                press(&c, KeyCode::KeyC, Some("c"), ModifiersState::CONTROL),
                false,
                Some(chord)
            ),
            Resolved::Command(Command::ToggleLineComment)
        );
        // An unknown second chord cancels and types nothing.
        let x = ch("x");
        assert_eq!(
            super::resolve(
                &km,
                press(&x, KeyCode::KeyX, Some("x"), ModifiersState::empty()),
                false,
                Some(chord)
            ),
            Resolved::Nothing
        );
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
