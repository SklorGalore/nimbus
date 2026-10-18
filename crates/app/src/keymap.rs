//! Key bindings: from chords like `"Primary+Shift+P"` to actions.
//!
//! The defaults come from the action registry; the config file can replace
//! an action's chords or unbind it. A key press is looked up by its logical
//! key first (what the layout calls it), then by its physical position, so
//! Option+Z on macOS still finds `Alt+Z` even though the layout turns it
//! into "Ω".
//!
//! A binding may also be two chords, `"Ctrl+K Ctrl+C"` (M11). The map is
//! then a trie two levels deep: the first chord is a *prefix* that leads
//! to a second table, and the app waits (for a while) for the second
//! press. No default binding uses one; they exist for the config file.
//! A chord cannot be both a complete binding and a prefix, since the
//! first press could not tell which was meant; that is a warning at load.

use std::collections::{HashMap, HashSet};

use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

use crate::action::{ACTIONS, ActionInfo, When};

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
        (KeyCode::Slash, '/'),
        (KeyCode::Backslash, '\\'),
        (KeyCode::BracketLeft, '['),
        (KeyCode::BracketRight, ']'),
        (KeyCode::Comma, ','),
        (KeyCode::Period, '.'),
        (KeyCode::Semicolon, ';'),
        (KeyCode::Quote, '\''),
        (KeyCode::Minus, '-'),
        (KeyCode::Equal, '='),
        (KeyCode::Backquote, '`'),
    ];
    letters.iter().find(|(k, _)| *k == code).map(|(_, c)| *c)
}

/// A binding: one chord, or a prefix chord and the chord that follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Binding {
    first: Chord,
    second: Option<Chord>,
}

/// Parse a binding: a chord, or two chords separated by a space
/// (`"Ctrl+K Ctrl+C"`).
///
/// # Errors
///
/// As [`parse_chord`], or for more than two chords.
pub fn parse_binding(text: &str) -> Result<Binding, String> {
    let mut parts = text.split_whitespace();
    let Some(first) = parts.next() else {
        return Err(format!("empty key chord {text:?}"));
    };
    let first = parse_chord(first)?;
    let second = parts.next().map(parse_chord).transpose()?;
    if parts.next().is_some() {
        return Err(format!("too many chords in {text:?}; at most two"));
    }
    Ok(Binding { first, second })
}

/// What a key press is bound to.
#[derive(Debug, Clone, Copy)]
pub enum Lookup {
    /// An action.
    Action(&'static ActionInfo),
    /// The first chord of a two-chord binding.
    Prefix(Chord),
    /// Nothing.
    None,
}

/// Bindings from chords to actions.
#[derive(Debug, Default)]
pub struct Keymap {
    /// Single-chord bindings. A chord may be bound once per `When`, so
    /// Primary+Enter can mean Replace All in the find bar and Insert Line
    /// Below elsewhere.
    map: HashMap<Chord, Vec<&'static ActionInfo>>,
    /// Two-chord bindings, by both chords.
    seq: HashMap<(Chord, Chord), &'static ActionInfo>,
    /// The first chords of two-chord bindings.
    prefixes: HashSet<Chord>,
    /// First binding of each action, for labels in the palette and menus.
    primary: HashMap<&'static str, (Binding, String)>,
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
        let mut parsed: Vec<(&'static ActionInfo, Vec<(Binding, String)>)> = Vec::new();
        for action in ACTIONS {
            let mut list = Vec::new();
            for text in chords_of(action) {
                match parse_binding(&text) {
                    Ok(binding) => list.push((binding, text)),
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
                for (binding, text) in list {
                    km.bind(*binding, text, action, &mut warnings);
                }
            }
        }
        // Labels and menu shortcuts use the first binding an action still
        // owns.
        for (action, list) in &parsed {
            if let Some((binding, text)) =
                list.iter().find(|(b, _)| km.owner(*b) == Some(action.id))
            {
                km.primary.insert(action.id, (*binding, text.clone()));
            }
        }
        (km, warnings)
    }

    /// Bind `binding` to `action`, taking it from whatever had it.
    fn bind(
        &mut self,
        binding: Binding,
        text: &str,
        action: &'static ActionInfo,
        warnings: &mut Vec<String>,
    ) {
        match binding.second {
            None => {
                if self.prefixes.contains(&binding.first) {
                    warnings.push(format!(
                        "keys: {text:?} ({}) is also the start of a two-chord binding",
                        action.id
                    ));
                }
                let list = self.map.entry(binding.first).or_default();
                if let Some(prev) = list.iter_mut().find(|a| a.when == action.when) {
                    if prev.id != action.id {
                        warnings.push(format!(
                            "keys: {text:?} moved from {} to {}",
                            prev.id, action.id
                        ));
                    }
                    *prev = action;
                } else {
                    list.push(action);
                }
            }
            Some(second) => {
                if self.map.get(&binding.first).is_some_and(|l| !l.is_empty()) {
                    let owners: Vec<&str> = self.map[&binding.first].iter().map(|a| a.id).collect();
                    warnings.push(format!(
                        "keys: {text:?} ({}) starts with a chord bound on its own ({})",
                        action.id,
                        owners.join(", ")
                    ));
                }
                self.prefixes.insert(binding.first);
                if let Some(prev) = self.seq.insert((binding.first, second), action)
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

    /// The id of the action that owns `binding` now, if any.
    fn owner(&self, binding: Binding) -> Option<&'static str> {
        match binding.second {
            None => self.map.get(&binding.first)?.first().map(|a| a.id),
            Some(second) => self.seq.get(&(binding.first, second)).map(|a| a.id),
        }
    }

    /// The chords a key press could mean: by its logical key, then by its
    /// physical position when the layout renamed it.
    fn chords_for(key: &Key, physical: PhysicalKey, mods: ModifiersState) -> [Option<Chord>; 2] {
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
        // Fall back to the key's position only when the layout changed the
        // character because of Option/Alt (Ω for Z), it is not ASCII at
        // all, or Shift turned punctuation into other punctuation (? for
        // /). On layouts that move letters (Dvorak, AZERTY) an unbound
        // ASCII chord must stay unbound, or Cmd+, would run whatever sits
        // on the US W key; so shifted punctuation only falls back to a
        // punctuation key.
        let shifted_punct =
            m & SHIFT != 0 && matches!(logical, Some(KeyName::Char(c)) if c.is_ascii_punctuation());
        let renamed = m & ALT != 0 || matches!(logical, Some(KeyName::Char(c)) if !c.is_ascii());
        let by_position = physical_char(physical)
            .filter(|c| renamed || (shifted_punct && c.is_ascii_punctuation()));
        [
            logical.map(|k| Chord { mods: m, key: k }),
            by_position.map(|c| Chord {
                mods: m,
                key: KeyName::Char(c),
            }),
        ]
    }

    /// What a key press is bound to: an action that applies now (find-bar
    /// actions when `find_open`), or the start of a two-chord binding.
    #[must_use]
    pub fn lookup(
        &self,
        key: &Key,
        physical: PhysicalKey,
        mods: ModifiersState,
        find_open: bool,
    ) -> Lookup {
        for chord in Self::chords_for(key, physical, mods).into_iter().flatten() {
            if let Some(list) = self.map.get(&chord) {
                // The find bar's binding first while it is open, so its
                // keys take precedence over the editor's.
                let pick = list
                    .iter()
                    .find(|a| a.when == When::FindBar && find_open)
                    .or_else(|| list.iter().find(|a| a.when == When::Always))
                    .or_else(|| list.first());
                if let Some(a) = pick {
                    return Lookup::Action(a);
                }
            }
            if self.prefixes.contains(&chord) {
                return Lookup::Prefix(chord);
            }
        }
        Lookup::None
    }

    /// The action of the two-chord binding starting with `first` and
    /// completed by this key press, if any.
    #[must_use]
    pub fn lookup_second(
        &self,
        first: Chord,
        key: &Key,
        physical: PhysicalKey,
        mods: ModifiersState,
    ) -> Option<&'static ActionInfo> {
        Self::chords_for(key, physical, mods)
            .into_iter()
            .flatten()
            .find_map(|second| self.seq.get(&(first, second)).copied())
    }

    /// A short label for the action's first chord, such as "⌘⇧P" on macOS or
    /// "Ctrl+Shift+P" elsewhere; empty if it has no binding.
    #[must_use]
    pub fn label(&self, id: &str) -> String {
        self.primary
            .get(id)
            .map_or_else(String::new, |(b, _)| binding_label(*b))
    }

    /// The action's first chord in `muda`'s accelerator syntax, for menus.
    /// A two-chord binding has no accelerator: menus show one key.
    #[must_use]
    #[cfg_attr(
        all(not(test), not(any(target_os = "macos", target_os = "windows"))),
        expect(dead_code, reason = "only native menus use it; Linux has none")
    )]
    pub fn accelerator(&self, id: &str) -> Option<String> {
        let (binding, text) = self.primary.get(id)?;
        if binding.second.is_some() {
            return None;
        }
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

/// The label of a chord, for the status bar while it waits for a second
/// one.
#[must_use]
pub fn chord_label(c: Chord) -> String {
    label(c)
}

fn binding_label(b: Binding) -> String {
    match b.second {
        None => label(b.first),
        Some(second) => format!("{} {}", label(b.first), label(second)),
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
        match km.lookup(key, PhysicalKey::Code(physical), mods, false) {
            Lookup::Action(a) => Some(a.id),
            Lookup::Prefix(_) | Lookup::None => None,
        }
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
    fn shifted_punctuation_finds_its_key_but_letters_do_not() {
        let km = defaults();
        // Primary+Shift+/ arrives as "?" on a US layout.
        let q = Key::Character("?".into());
        assert_eq!(
            id(&km, &q, KeyCode::Slash, primary() | ModifiersState::SHIFT),
            Some("edit.toggleBlockComment")
        );
        let bar = Key::Character("|".into());
        assert_eq!(
            id(
                &km,
                &bar,
                KeyCode::Backslash,
                primary() | ModifiersState::SHIFT
            ),
            Some("cursor.matchingBracket")
        );
        // Shifted punctuation on a letter key (another layout) stays put.
        let mut o = HashMap::new();
        o.insert("file.save".to_owned(), vec!["Primary+Shift+W".to_owned()]);
        let (km, _) = Keymap::new(&o);
        let lt = Key::Character("<".into());
        assert_eq!(
            id(&km, &lt, KeyCode::KeyW, primary() | ModifiersState::SHIFT),
            None
        );
    }

    #[test]
    fn one_chord_may_serve_the_find_bar_and_the_editor() {
        let km = defaults();
        let enter = Key::Named(NamedKey::Enter);
        assert_eq!(
            id(&km, &enter, KeyCode::Enter, primary()),
            Some("edit.insertLineBelow")
        );
        let found = km.lookup(&enter, PhysicalKey::Code(KeyCode::Enter), primary(), true);
        assert!(matches!(found, Lookup::Action(a) if a.id == "find.replaceAll"));
    }

    #[test]
    fn two_chord_bindings_parse_label_and_conflict_with_prefixes() {
        assert!(parse_binding("Ctrl+K Ctrl+C").is_ok());
        assert!(parse_binding("Ctrl+K Ctrl+C Ctrl+D").is_err());
        assert!(parse_binding("").is_err());
        let mut o = HashMap::new();
        o.insert(
            "edit.toggleLineComment".to_owned(),
            vec!["Ctrl+K Ctrl+C".to_owned()],
        );
        let (km, warnings) = Keymap::new(&o);
        assert_eq!(warnings, Vec::<String>::new());
        let k = Key::Character("k".into());
        assert!(matches!(
            km.lookup(
                &k,
                PhysicalKey::Code(KeyCode::KeyK),
                ModifiersState::CONTROL,
                false
            ),
            Lookup::Prefix(_)
        ));
        assert_eq!(
            km.label("edit.toggleLineComment"),
            if cfg!(target_os = "macos") {
                "⌃K ⌃C"
            } else {
                "Ctrl+K Ctrl+C"
            }
        );
        assert_eq!(km.accelerator("edit.toggleLineComment"), None);
        // A chord that is both a binding and a prefix is reported.
        let mut o = HashMap::new();
        o.insert(
            "edit.toggleLineComment".to_owned(),
            vec!["Primary+S Ctrl+C".to_owned()],
        );
        let (_, warnings) = Keymap::new(&o);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("file.save"), "{warnings:?}");
        let mut o = HashMap::new();
        o.insert(
            "edit.toggleLineComment".to_owned(),
            vec!["Ctrl+K Ctrl+C".to_owned()],
        );
        o.insert("file.save".to_owned(), vec!["Ctrl+K".to_owned()]);
        let (_, warnings) = Keymap::new(&o);
        // Ctrl+K also starts the fold chords on Windows and Linux (M13),
        // so binding it alone is reported once more there.
        let expected = if cfg!(target_os = "macos") { 1 } else { 2 };
        assert_eq!(warnings.len(), expected, "{warnings:?}");
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
