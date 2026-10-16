//! Turning key presses into editor commands.
//!
//! winit's `KeyEvent` cannot be built in tests, so [`map_key`] takes its
//! parts instead. M4 replaces the hard-coded table here with the action
//! registry and configurable keymap.

use winit::keyboard::{Key, ModifiersState, NamedKey};

/// A cursor movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One grapheme left.
    Left,
    /// One grapheme right.
    Right,
    /// One line up, keeping the preferred column.
    Up,
    /// One line down, keeping the preferred column.
    Down,
    /// Start of the line.
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
    /// Move the cursor.
    Move(Motion),
    /// Type this text at the cursor.
    Insert(&'a str),
    /// Insert the document's line ending.
    Newline,
    /// Insert a tab.
    Tab,
    /// Delete the grapheme before the cursor.
    DeleteBack,
    /// Delete the grapheme after the cursor.
    DeleteForward,
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

/// Map one key press to a command, or `None` if it means nothing to the
/// editor. `text` is `KeyEvent::text`: what the key types with the current
/// layout and modifiers.
pub fn map_key<'a>(
    key: &Key,
    text: Option<&'a str>,
    pressed: bool,
    repeat: bool,
    mods: ModifiersState,
) -> Option<Command<'a>> {
    use NamedKey as N;
    if !pressed {
        return None;
    }
    let mac = cfg!(target_os = "macos");
    // Off macOS, Ctrl+Alt is how AltGr arrives (a German keyboard types @
    // with it), so it is text input, not a Primary chord.
    let altgr = !mac && mods.control_key() && mods.alt_key();
    let primary = primary_held(mods) && !altgr;
    if let Key::Named(n) = key {
        let cmd = match n {
            N::ArrowLeft if primary && mac => Some(Command::Move(Motion::LineStart)),
            N::ArrowRight if primary && mac => Some(Command::Move(Motion::LineEnd)),
            N::ArrowUp if primary => Some(Command::Move(Motion::DocStart)),
            N::ArrowDown if primary => Some(Command::Move(Motion::DocEnd)),
            N::Home if primary || mods.control_key() => Some(Command::Move(Motion::DocStart)),
            N::End if primary || mods.control_key() => Some(Command::Move(Motion::DocEnd)),
            N::ArrowLeft => Some(Command::Move(Motion::Left)),
            N::ArrowRight => Some(Command::Move(Motion::Right)),
            N::ArrowUp => Some(Command::Move(Motion::Up)),
            N::ArrowDown => Some(Command::Move(Motion::Down)),
            N::Home => Some(Command::Move(Motion::LineStart)),
            N::End => Some(Command::Move(Motion::LineEnd)),
            N::PageUp => Some(Command::Move(Motion::PageUp)),
            N::PageDown => Some(Command::Move(Motion::PageDown)),
            N::Backspace => Some(Command::DeleteBack),
            N::Delete => Some(Command::DeleteForward),
            N::Enter if !primary => Some(Command::Newline),
            N::Tab if !primary && !mods.control_key() => Some(Command::Tab),
            // Space and others fall through: their text is typed below.
            _ => None,
        };
        if cmd.is_some() {
            return cmd;
        }
    }
    if primary {
        return match key {
            Key::Character(c) if c.eq_ignore_ascii_case("s") && !repeat => Some(Command::Save),
            Key::Character(c)
                if (c.eq_ignore_ascii_case("w") || c.eq_ignore_ascii_case("q")) && !repeat =>
            {
                Some(Command::Close)
            }
            _ => None,
        };
    }
    let text = text?;
    // Control characters (\r, \x08, \x1b, \x7f) arrive as text on some
    // platforms for keys already handled above; never type them.
    (may_insert_text(mods) && !text.chars().any(char::is_control)).then_some(Command::Insert(text))
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

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    #[test]
    fn primary_s_saves_and_never_types() {
        let cmd = map_key(&ch("s"), Some("s"), true, false, primary());
        assert_eq!(cmd, Some(Command::Save));
        assert_eq!(map_key(&ch("s"), Some("s"), true, true, primary()), None);
    }

    #[test]
    fn primary_w_and_q_close() {
        assert_eq!(
            map_key(&ch("w"), Some("w"), true, false, primary()),
            Some(Command::Close)
        );
        assert_eq!(
            map_key(&ch("Q"), Some("Q"), true, false, primary()),
            Some(Command::Close)
        );
    }

    #[test]
    fn plain_typing_and_space_insert() {
        let none = ModifiersState::empty();
        assert_eq!(
            map_key(&ch("a"), Some("a"), true, false, none),
            Some(Command::Insert("a"))
        );
        let space = Key::Named(NamedKey::Space);
        assert_eq!(
            map_key(&space, Some(" "), true, false, none),
            Some(Command::Insert(" "))
        );
        assert_eq!(
            map_key(&ch("a"), Some("a"), false, false, none),
            None,
            "release"
        );
        assert_eq!(
            map_key(&ch("a"), Some("a"), true, true, none),
            Some(Command::Insert("a")),
            "repeat"
        );
    }

    #[test]
    fn named_keys_map_to_commands() {
        let none = ModifiersState::empty();
        let k = |n| Key::Named(n);
        assert_eq!(
            map_key(&k(NamedKey::Enter), Some("\r"), true, false, none),
            Some(Command::Newline)
        );
        assert_eq!(
            map_key(&k(NamedKey::Tab), Some("\t"), true, false, none),
            Some(Command::Tab)
        );
        assert_eq!(
            map_key(&k(NamedKey::Backspace), Some("\x08"), true, false, none),
            Some(Command::DeleteBack)
        );
        assert_eq!(
            map_key(&k(NamedKey::Delete), None, true, false, none),
            Some(Command::DeleteForward)
        );
        assert_eq!(
            map_key(&k(NamedKey::ArrowUp), None, true, false, primary()),
            Some(Command::Move(Motion::DocStart))
        );
        assert_eq!(
            map_key(&k(NamedKey::Escape), Some("\x1b"), true, false, none),
            None
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn option_composes_characters_on_macos() {
        let cmd = map_key(&ch("å"), Some("å"), true, false, ModifiersState::ALT);
        assert_eq!(cmd, Some(Command::Insert("å")));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn altgr_types_off_macos() {
        let altgr = ModifiersState::CONTROL | ModifiersState::ALT;
        assert_eq!(
            map_key(&ch("@"), Some("@"), true, false, altgr),
            Some(Command::Insert("@"))
        );
    }
}
