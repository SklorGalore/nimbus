//! Turning key presses into editor commands.
//!
//! winit's `KeyEvent` cannot be built in tests, so [`map_key`] takes its
//! parts instead. M4 replaces the hard-coded table here with the action
//! registry and configurable keymap.

use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

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
    /// Close the window (asking about unsaved changes first).
    Close,
}

/// The platform's "primary" modifier: Command on macOS, Control elsewhere.
/// Keybindings are written in terms of it (DESIGN.md §3).
pub fn primary_held(mods: ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        mods.super_key()
    } else {
        mods.control_key()
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

fn named(n: NamedKey, mods: ModifiersState, primary: bool) -> Option<Command<'static>> {
    use NamedKey as N;
    let mac = cfg!(target_os = "macos");
    let shift = mods.shift_key();
    // Word movement: Option on macOS, Ctrl elsewhere (where Ctrl is primary).
    let word = if mac {
        mods.alt_key()
    } else {
        mods.control_key()
    };
    let mv = |m| Some(Command::Move(m, shift));
    match n {
        N::ArrowLeft if primary && mac => mv(Motion::LineStart),
        N::ArrowRight if primary && mac => mv(Motion::LineEnd),
        N::ArrowLeft if word => mv(Motion::WordLeft),
        N::ArrowRight if word => mv(Motion::WordRight),
        N::ArrowUp if primary => mv(Motion::DocStart),
        N::ArrowDown if primary => mv(Motion::DocEnd),
        N::Home if primary || mods.control_key() => mv(Motion::DocStart),
        N::End if primary || mods.control_key() => mv(Motion::DocEnd),
        N::ArrowLeft => mv(Motion::Left),
        N::ArrowRight => mv(Motion::Right),
        N::ArrowUp => mv(Motion::Up),
        N::ArrowDown => mv(Motion::Down),
        N::Home => mv(Motion::LineStart),
        N::End => mv(Motion::LineEnd),
        N::PageUp => mv(Motion::PageUp),
        N::PageDown => mv(Motion::PageDown),
        N::Backspace => Some(Command::DeleteBack),
        N::Delete => Some(Command::DeleteForward),
        N::Enter if primary => Some(Command::ReplaceAll),
        N::Enter => Some(Command::Newline),
        N::Tab if shift && !primary => Some(Command::Outdent),
        N::Tab if !primary && !mods.control_key() => Some(Command::Tab),
        N::Escape => Some(Command::Escape),
        // Space and others fall through: their text is typed.
        _ => None,
    }
}

fn chord(c: &str, mods: ModifiersState) -> Option<Command<'static>> {
    let mac = cfg!(target_os = "macos");
    let shift = mods.shift_key();
    let is = |s: &str| c.eq_ignore_ascii_case(s);
    Some(match () {
        () if is("s") => Command::Save,
        () if is("w") || is("q") => Command::Close,
        () if is("z") && shift => Command::Redo,
        () if is("z") => Command::Undo,
        () if is("y") && !mac => Command::Redo,
        () if is("a") => Command::SelectAll,
        () if is("x") => Command::Cut,
        () if is("c") => Command::Copy,
        () if is("v") => Command::Paste,
        // Cmd+H hides the app on macOS, so replace is Cmd+Option+F there.
        () if is("f") && mac && mods.alt_key() => Command::Replace,
        () if is("f") => Command::Find,
        () if is("h") && !mac => Command::Replace,
        () => return None,
    })
}

/// Map one key press to a command, or `None` if it means nothing to the
/// editor.
pub fn map_key(k: KeyPress<'_>) -> Option<Command<'_>> {
    if !k.pressed {
        return None;
    }
    let mac = cfg!(target_os = "macos");
    // Off macOS, Ctrl+Alt is how AltGr arrives (a German keyboard types @
    // with it), so it is text input, not a Primary chord.
    let altgr = !mac && k.mods.control_key() && k.mods.alt_key();
    let primary = primary_held(k.mods) && !altgr;

    if let Key::Named(n) = k.key
        && let Some(cmd) = named(*n, k.mods, primary)
    {
        return Some(cmd);
    }
    // Alt+Z, Alt+C and Alt+R are matched by physical key: on macOS
    // Option+Z types "Ω", so the logical key is not "z".
    if k.mods.alt_key() && !primary && !altgr && !k.repeat {
        let cmd = match k.physical {
            PhysicalKey::Code(KeyCode::KeyZ) => Some(Command::ToggleWrap),
            PhysicalKey::Code(KeyCode::KeyC) => Some(Command::ToggleCase(k.text.unwrap_or(""))),
            PhysicalKey::Code(KeyCode::KeyR) => Some(Command::ToggleRegex(k.text.unwrap_or(""))),
            _ => None,
        };
        if cmd.is_some() {
            return cmd;
        }
    }
    if primary {
        return match k.key {
            // Undo and redo repeat while held; the rest fire once.
            Key::Character(c) => chord(c, k.mods)
                .filter(|cmd| !k.repeat || matches!(cmd, Command::Undo | Command::Redo)),
            _ => None,
        };
    }
    let text = k.text?;
    // Control characters (\r, \x08, \x1b, \x7f) arrive as text on some
    // platforms for keys already handled above; never type them.
    (may_insert_text(k.mods) && !text.chars().any(char::is_control))
        .then_some(Command::Insert(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary() -> ModifiersState {
        if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        }
    }

    fn press<'a>(key: &'a Key, text: Option<&'a str>, mods: ModifiersState) -> KeyPress<'a> {
        KeyPress {
            key,
            physical: PhysicalKey::Code(KeyCode::KeyA),
            text,
            pressed: true,
            repeat: false,
            mods,
        }
    }

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    #[test]
    fn primary_chords_never_type() {
        let s = ch("s");
        assert_eq!(
            map_key(press(&s, Some("s"), primary())),
            Some(Command::Save)
        );
        let r = KeyPress {
            repeat: true,
            ..press(&s, Some("s"), primary())
        };
        assert_eq!(map_key(r), None, "save does not repeat");
        let z = ch("z");
        assert_eq!(
            map_key(press(&z, Some("z"), primary())),
            Some(Command::Undo)
        );
        let zr = KeyPress {
            repeat: true,
            ..press(&z, Some("z"), primary())
        };
        assert_eq!(map_key(zr), Some(Command::Undo), "undo repeats");
        let shift = primary() | ModifiersState::SHIFT;
        assert_eq!(
            map_key(press(&ch("Z"), Some("Z"), shift)),
            Some(Command::Redo)
        );
        for (k, cmd) in [
            ("a", Command::SelectAll),
            ("x", Command::Cut),
            ("c", Command::Copy),
            ("v", Command::Paste),
            ("f", Command::Find),
            ("w", Command::Close),
        ] {
            let key = ch(k);
            assert_eq!(map_key(press(&key, Some(k), primary())), Some(cmd), "{k}");
        }
    }

    #[test]
    fn shift_extends_moves() {
        let left = Key::Named(NamedKey::ArrowLeft);
        let none = ModifiersState::empty();
        assert_eq!(
            map_key(press(&left, None, none)),
            Some(Command::Move(Motion::Left, false))
        );
        assert_eq!(
            map_key(press(&left, None, ModifiersState::SHIFT)),
            Some(Command::Move(Motion::Left, true))
        );
    }

    #[test]
    fn word_movement_modifier_matches_the_platform() {
        let right = Key::Named(NamedKey::ArrowRight);
        let word = if cfg!(target_os = "macos") {
            ModifiersState::ALT
        } else {
            ModifiersState::CONTROL
        };
        assert_eq!(
            map_key(press(&right, None, word)),
            Some(Command::Move(Motion::WordRight, false))
        );
    }

    #[test]
    fn find_bar_toggles_and_replace_all() {
        let c = ch("ç");
        let k = KeyPress {
            physical: PhysicalKey::Code(KeyCode::KeyC),
            ..press(&c, Some("ç"), ModifiersState::ALT)
        };
        assert_eq!(map_key(k), Some(Command::ToggleCase("ç")));
        let enter = Key::Named(NamedKey::Enter);
        assert_eq!(
            map_key(press(&enter, Some("\r"), primary())),
            Some(Command::ReplaceAll)
        );
    }

    #[test]
    fn alt_z_toggles_wrap_by_physical_key() {
        let omega = ch("Ω");
        let k = KeyPress {
            physical: PhysicalKey::Code(KeyCode::KeyZ),
            ..press(&omega, Some("Ω"), ModifiersState::ALT)
        };
        assert_eq!(map_key(k), Some(Command::ToggleWrap));
    }

    #[test]
    fn plain_typing_space_tab_and_escape() {
        let none = ModifiersState::empty();
        let a = ch("a");
        assert_eq!(
            map_key(press(&a, Some("a"), none)),
            Some(Command::Insert("a"))
        );
        let space = Key::Named(NamedKey::Space);
        assert_eq!(
            map_key(press(&space, Some(" "), none)),
            Some(Command::Insert(" "))
        );
        let tab = Key::Named(NamedKey::Tab);
        assert_eq!(map_key(press(&tab, Some("\t"), none)), Some(Command::Tab));
        assert_eq!(
            map_key(press(&tab, Some("\t"), ModifiersState::SHIFT)),
            Some(Command::Outdent)
        );
        let esc = Key::Named(NamedKey::Escape);
        assert_eq!(
            map_key(press(&esc, Some("\x1b"), none)),
            Some(Command::Escape)
        );
        let release = KeyPress {
            pressed: false,
            ..press(&a, Some("a"), none)
        };
        assert_eq!(map_key(release), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn option_composes_characters_and_replace_is_cmd_option_f() {
        let aring = ch("å");
        assert_eq!(
            map_key(press(&aring, Some("å"), ModifiersState::ALT)),
            Some(Command::Insert("å"))
        );
        let f = ch("f");
        let m = ModifiersState::SUPER | ModifiersState::ALT;
        assert_eq!(map_key(press(&f, Some("f"), m)), Some(Command::Replace));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn altgr_types_and_ctrl_y_redoes_off_macos() {
        let altgr = ModifiersState::CONTROL | ModifiersState::ALT;
        let at = ch("@");
        assert_eq!(
            map_key(press(&at, Some("@"), altgr)),
            Some(Command::Insert("@"))
        );
        let y = ch("y");
        assert_eq!(
            map_key(press(&y, Some("y"), ModifiersState::CONTROL)),
            Some(Command::Redo)
        );
        let h = ch("h");
        assert_eq!(
            map_key(press(&h, Some("h"), ModifiersState::CONTROL)),
            Some(Command::Replace)
        );
    }
}
