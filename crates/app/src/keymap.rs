//! Key bindings: from chords like `"Primary+Shift+P"` to actions.
//!
//! The defaults come from the action registry; the config file can replace
//! an action's chords or unbind it. A key press is looked up by its logical
//! key first (what the layout calls it), then by its physical position, so
//! Option+Z on macOS still finds `Alt+Z` even though the layout turns it
//! into "Ω".

use std::collections::HashMap;

use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

use crate::action::{ACTIONS, ActionInfo};

const CTRL: u8 = 1;
const ALT: u8 = 2;
const SHIFT: u8 = 4;
const META: u8 = 8;

/// The key of a chord, without modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum KeyName {
    Named(NamedKey),
    /// A character key, lowercased.
    Char(char),
}

/// A key plus the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    mods: u8,
    key: KeyName,
}

const NAMES: &[(&str, NamedKey)] = &[
    ("Left", NamedKey::ArrowLeft),
    ("Right", NamedKey::ArrowRight),
    ("Up", NamedKey::ArrowUp),
    ("Down", NamedKey::ArrowDown),
    ("Home", NamedKey::Home),
    ("End", NamedKey::End),
    ("PageUp", NamedKey::PageUp),
    ("PageDown", NamedKey::PageDown),
    ("Enter", NamedKey::Enter),
    ("Tab", NamedKey::Tab),
    ("Escape", NamedKey::Escape),
    ("Backspace", NamedKey::Backspace),
    ("Delete", NamedKey::Delete),
    ("Space", NamedKey::Space),
    ("F1", NamedKey::F1),
    ("F2", NamedKey::F2),
    ("F3", NamedKey::F3),
    ("F4", NamedKey::F4),
    ("F5", NamedKey::F5),
    ("F6", NamedKey::F6),
    ("F7", NamedKey::F7),
    ("F8", NamedKey::F8),
    ("F9", NamedKey::F9),
    ("F10", NamedKey::F10),
    ("F11", NamedKey::F11),
    ("F12", NamedKey::F12),
];

/// Parse a chord such as `"Primary+Shift+P"`, `"Ctrl+Tab"` or `"Alt+Left"`.
///
/// Modifiers are `Primary` (Command on macOS, Control elsewhere), `Ctrl`,
/// `Alt` (Option on macOS), `Shift` and `Meta` (Command, or the Windows
/// key). The key is a name from the list above or a single character.
///
/// # Errors
///
/// Returns a message naming the part that could not be understood.
pub fn parse_chord(text: &str) -> Result<Chord, String> {
    let mut mods = 0;
    let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
    // "Primary++" binds the plus key: an empty last part after a '+'.
    if text.ends_with("++") {
        parts.pop();
        parts.pop();
        parts.push("+");
    }
    let Some((key, modifiers)) = parts.split_last() else {
        return Err(format!("empty key chord {text:?}"));
    };
    for m in modifiers {
        mods |= match m.to_ascii_lowercase().as_str() {
            "primary" => {
                if cfg!(target_os = "macos") {
                    META
                } else {
                    CTRL
                }
            }
            "ctrl" | "control" => CTRL,
            "alt" | "option" => ALT,
            "shift" => SHIFT,
            "meta" | "cmd" | "command" | "super" | "win" => META,
            other => return Err(format!("unknown modifier {other:?} in {text:?}")),
        };
    }
    let key = if let Some((_, named)) = NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(key)) {
        KeyName::Named(*named)
    } else {
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => KeyName::Char(c.to_ascii_lowercase()),
            _ => return Err(format!("unknown key {key:?} in {text:?}")),
        }
    };
    Ok(Chord { mods, key })
}

fn mods_of(m: ModifiersState) -> u8 {
    let mut out = 0;
    if m.control_key() {
        out |= CTRL;
    }
    if m.alt_key() {
        out |= ALT;
    }
    if m.shift_key() {
        out |= SHIFT;
    }
    if m.super_key() {
        out |= META;
    }
    out
}

/// The character a physical key produces on a US layout, for chords whose
/// logical character the layout changes.
fn physical_char(key: PhysicalKey) -> Option<char> {
    let PhysicalKey::Code(code) = key else {
        return None;
    };
    let letters = [
        (KeyCode::KeyA, 'a'),
        (KeyCode::KeyB, 'b'),
        (KeyCode::KeyC, 'c'),
        (KeyCode::KeyD, 'd'),
        (KeyCode::KeyE, 'e'),
        (KeyCode::KeyF, 'f'),
        (KeyCode::KeyG, 'g'),
        (KeyCode::KeyH, 'h'),
        (KeyCode::KeyI, 'i'),
        (KeyCode::KeyJ, 'j'),
        (KeyCode::KeyK, 'k'),
        (KeyCode::KeyL, 'l'),
        (KeyCode::KeyM, 'm'),
        (KeyCode::KeyN, 'n'),
        (KeyCode::KeyO, 'o'),
        (KeyCode::KeyP, 'p'),
        (KeyCode::KeyQ, 'q'),
        (KeyCode::KeyR, 'r'),
        (KeyCode::KeyS, 's'),
        (KeyCode::KeyT, 't'),
        (KeyCode::KeyU, 'u'),
        (KeyCode::KeyV, 'v'),
        (KeyCode::KeyW, 'w'),
        (KeyCode::KeyX, 'x'),
        (KeyCode::KeyY, 'y'),
        (KeyCode::KeyZ, 'z'),
        (KeyCode::Digit0, '0'),
        (KeyCode::Digit1, '1'),
        (KeyCode::Digit2, '2'),
        (KeyCode::Digit3, '3'),
        (KeyCode::Digit4, '4'),
        (KeyCode::Digit5, '5'),
        (KeyCode::Digit6, '6'),
        (KeyCode::Digit7, '7'),
        (KeyCode::Digit8, '8'),
        (KeyCode::Digit9, '9'),
    ];
    letters.iter().find(|(k, _)| *k == code).map(|(_, c)| *c)
}

/// Bindings from chords to actions.
#[derive(Debug, Default)]
pub struct Keymap {
    map: HashMap<Chord, &'static ActionInfo>,
    /// First chord of each action, for labels in the palette and menus.
    primary_chord: HashMap<&'static str, (Chord, String)>,
}

impl Keymap {
    /// The default bindings with `overrides` applied. An override maps an
    /// action id to its new chords; an empty list unbinds it. Returns the
    /// keymap and warnings for unknown ids, bad chords and conflicts.
    #[must_use]
    pub fn new(overrides: &HashMap<String, Vec<String>>) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        for id in overrides.keys() {
            if crate::action::by_id(id).is_none() {
                warnings.push(format!("keys: unknown action {id:?}"));
            }
        }
        let mut km = Self::default();
        // Each action's chords, as configured or by default.
        let chords_of = |a: &ActionInfo| -> Vec<String> {
            overrides
                .get(a.id)
                .cloned()
                .unwrap_or_else(|| a.keys.for_this_platform().map(str::to_owned).collect())
        };
        let mut parsed: Vec<(&'static ActionInfo, Vec<(Chord, String)>)> = Vec::new();
        for action in ACTIONS {
            let mut list = Vec::new();
            for text in chords_of(action) {
                match parse_chord(&text) {
                    Ok(chord) => list.push((chord, text)),
                    Err(e) => warnings.push(format!("keys: {e}")),
                }
            }
            parsed.push((action, list));
        }
        // Defaults first, then the user's overrides, so a configured chord
        // always wins over a default one, whatever order actions are in.
        for pass_overrides in [false, true] {
            for (action, list) in &parsed {
                if overrides.contains_key(action.id) != pass_overrides {
                    continue;
                }
                for (chord, text) in list {
                    if let Some(prev) = km.map.insert(*chord, action)
                        && prev.id != action.id
                    {
                        warnings.push(format!(
                            "keys: {text:?} moved from {} to {}",
                            prev.id, action.id
                        ));
                    }
                }
            }
        }
        // Labels and menu shortcuts use the first chord an action still owns.
        for (action, list) in &parsed {
            if let Some((chord, text)) = list
                .iter()
                .find(|(c, _)| km.map.get(c).is_some_and(|a| a.id == action.id))
            {
                km.primary_chord.insert(action.id, (*chord, text.clone()));
            }
        }
        (km, warnings)
    }

    /// The action bound to a key press, if any.
    #[must_use]
    pub fn lookup(
        &self,
        key: &Key,
        physical: PhysicalKey,
        mods: ModifiersState,
    ) -> Option<&'static ActionInfo> {
        let m = mods_of(mods);
        let logical = match key {
            Key::Named(n) => Some(KeyName::Named(*n)),
            Key::Character(s) => {
                let mut chars = s.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Some(KeyName::Char(c.to_lowercase().next().unwrap_or(c))),
                    _ => None,
                }
            }
            _ => None,
        };
        logical
            .and_then(|k| self.map.get(&Chord { mods: m, key: k }))
            .or_else(|| {
                // Fall back to the key's position only when the layout
                // changed the character because of Option/Alt (Ω for Z), or
                // it is not ASCII at all. On layouts that move letters
                // (Dvorak, AZERTY) an unbound ASCII chord must stay unbound,
                // or Cmd+, would run whatever sits on the US W key.
                let renamed =
                    m & ALT != 0 || matches!(logical, Some(KeyName::Char(c)) if !c.is_ascii());
                if !renamed {
                    return None;
                }
                let c = physical_char(physical)?;
                self.map.get(&Chord {
                    mods: m,
                    key: KeyName::Char(c),
                })
            })
            .copied()
    }

    /// A short label for the action's first chord, such as "⌘⇧P" on macOS or
    /// "Ctrl+Shift+P" elsewhere; empty if it has no binding.
    #[must_use]
    pub fn label(&self, id: &str) -> String {
        self.primary_chord
            .get(id)
            .map_or_else(String::new, |(c, _)| label(*c))
    }

    /// The action's first chord in `muda`'s accelerator syntax, for menus.
    #[must_use]
    #[cfg_attr(
        all(not(test), not(any(target_os = "macos", target_os = "windows"))),
        expect(dead_code, reason = "only native menus use it; Linux has none")
    )]
    pub fn accelerator(&self, id: &str) -> Option<String> {
        let (_, text) = self.primary_chord.get(id)?;
        let parts: Vec<&str> = text
            .split('+')
            .map(|part| {
                if part.eq_ignore_ascii_case("primary") {
                    "CmdOrCtrl"
                } else {
                    part
                }
            })
            .collect();
        Some(parts.join("+"))
    }
}

/// How a shortcut is written in the palette and pickers: as macOS writes
/// them there (⌃⌥⇧⌘ in that order, then the key: ⇧⌘P), and as words
/// elsewhere (Ctrl+Shift+P). The symbols come from a system font through
/// font fallback; the bundled font lacks them.
fn label(c: Chord) -> String {
    if cfg!(target_os = "macos") {
        mac_label(c)
    } else {
        word_label(c)
    }
}

/// The key's name; arrows are symbols on every platform.
fn key_name(c: Chord, mac: bool) -> String {
    match c.key {
        KeyName::Char(ch) => ch.to_ascii_uppercase().to_string(),
        KeyName::Named(n) => {
            let name = NAMES.iter().find(|(_, k)| *k == n).map_or("?", |(s, _)| *s);
            match (name, mac) {
                ("Left", _) => "←".into(),
                ("Right", _) => "→".into(),
                ("Up", _) => "↑".into(),
                ("Down", _) => "↓".into(),
                ("Escape", true) => "⎋".into(),
                ("Escape", false) => "Esc".into(),
                ("Backspace", true) => "⌫".into(),
                ("Delete", true) => "⌦".into(),
                ("Enter", true) => "↩".into(),
                ("Tab", true) => "⇥".into(),
                (other, _) => other.into(),
            }
        }
    }
}

fn mac_label(c: Chord) -> String {
    let mut out = String::new();
    for (bit, symbol) in [(CTRL, '⌃'), (ALT, '⌥'), (SHIFT, '⇧'), (META, '⌘')] {
        if c.mods & bit != 0 {
            out.push(symbol);
        }
    }
    out.push_str(&key_name(c, true));
    out
}

fn word_label(c: Chord) -> String {
    let mut out = String::new();
    for (bit, text) in [
        (CTRL, "Ctrl+"),
        (ALT, "Alt+"),
        (SHIFT, "Shift+"),
        (META, "Win+"),
    ] {
        if c.mods & bit != 0 {
            out.push_str(text);
        }
    }
    out.push_str(&key_name(c, false));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Keymap {
        let (km, warnings) = Keymap::new(&HashMap::new());
        assert_eq!(
            warnings,
            Vec::<String>::new(),
            "default bindings must not conflict"
        );
        km
    }

    fn primary() -> ModifiersState {
        if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        }
    }

    fn id(km: &Keymap, key: &Key, physical: KeyCode, mods: ModifiersState) -> Option<&'static str> {
        km.lookup(key, PhysicalKey::Code(physical), mods)
            .map(|a| a.id)
    }

    #[test]
    fn parses_chords() {
        assert!(parse_chord("Primary+Shift+P").is_ok());
        assert!(parse_chord("Ctrl+Tab").is_ok());
        assert!(parse_chord("Alt+Left").is_ok());
        assert!(parse_chord("Primary++").is_ok());
        assert!(parse_chord("Hyper+X").is_err());
        assert!(parse_chord("Ctrl+NoSuchKey").is_err());
        assert_eq!(parse_chord("ctrl+shift+p"), parse_chord("Ctrl+Shift+P"));
    }

    #[test]
    fn default_bindings_resolve() {
        let km = defaults();
        let s = Key::Character("s".into());
        assert_eq!(id(&km, &s, KeyCode::KeyS, primary()), Some("file.save"));
        let p = Key::Character("P".into());
        assert_eq!(
            id(&km, &p, KeyCode::KeyP, primary() | ModifiersState::SHIFT),
            Some("app.palette")
        );
        let left = Key::Named(NamedKey::ArrowLeft);
        assert_eq!(
            id(&km, &left, KeyCode::ArrowLeft, ModifiersState::SHIFT),
            Some("select.left")
        );
        let a = Key::Character("a".into());
        assert_eq!(
            id(&km, &a, KeyCode::KeyA, ModifiersState::empty()),
            None,
            "typing is not an action"
        );
    }

    #[test]
    fn physical_key_finds_alt_chords_the_layout_renamed() {
        let km = defaults();
        let omega = Key::Character("Ω".into());
        assert_eq!(
            id(&km, &omega, KeyCode::KeyZ, ModifiersState::ALT),
            Some("view.toggleWrap")
        );
    }

    #[test]
    fn overrides_rebind_and_unbind_with_warnings() {
        let mut o = HashMap::new();
        o.insert("file.save".to_owned(), vec!["Primary+Shift+W".to_owned()]);
        o.insert("view.toggleWrap".to_owned(), Vec::new());
        o.insert("no.such".to_owned(), vec!["F5".to_owned()]);
        o.insert("edit.copy".to_owned(), vec!["Bogus+K".to_owned()]);
        let (km, warnings) = Keymap::new(&o);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert_eq!(
            km.label("file.save"),
            if cfg!(target_os = "macos") {
                "⇧⌘W"
            } else {
                "Ctrl+Shift+W"
            }
        );
        let w = Key::Character("w".into());
        assert_eq!(
            id(&km, &w, KeyCode::KeyW, primary() | ModifiersState::SHIFT),
            Some("file.save")
        );
        let s = Key::Character("s".into());
        assert_eq!(
            id(&km, &s, KeyCode::KeyS, primary()),
            None,
            "old chord gone"
        );
        let z = Key::Character("Ω".into());
        assert_eq!(
            id(&km, &z, KeyCode::KeyZ, ModifiersState::ALT),
            None,
            "unbound"
        );
    }

    #[test]
    fn a_configured_chord_beats_a_later_default() {
        let mut o = HashMap::new();
        o.insert("file.save".to_owned(), vec!["Alt+Z".to_owned()]);
        let (km, warnings) = Keymap::new(&o);
        let omega = Key::Character("Ω".into());
        assert_eq!(
            id(&km, &omega, KeyCode::KeyZ, ModifiersState::ALT),
            Some("file.save")
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(km.label("view.toggleWrap"), "", "lost its only chord");
        // And the other way round: the default owner loses its label.
        let mut o = HashMap::new();
        o.insert("view.toggleWrap".to_owned(), vec!["Primary+S".to_owned()]);
        let (km, _) = Keymap::new(&o);
        assert_eq!(km.label("file.save"), "");
        assert_eq!(km.accelerator("file.save"), None);
    }

    #[test]
    fn unbound_ascii_chords_do_not_fall_back_to_position() {
        let km = defaults();
        // Dvorak: Cmd+, is on the US W key. Must not close the tab.
        let comma = Key::Character(",".into());
        assert_eq!(id(&km, &comma, KeyCode::KeyW, primary()), None);
    }

    #[test]
    fn labels_follow_the_platform() {
        let km = defaults();
        let l = km.label("app.palette");
        if cfg!(target_os = "macos") {
            assert_eq!(l, "⇧⌘P");
            assert_eq!(km.label("edit.cancel"), "⎋");
        } else {
            assert_eq!(l, "Ctrl+Shift+P");
        }
        assert_eq!(km.label("app.reloadConfig"), "");
        assert_eq!(
            km.accelerator("app.palette").as_deref(),
            Some("CmdOrCtrl+Shift+P")
        );
    }
}
