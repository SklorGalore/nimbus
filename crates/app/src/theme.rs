//! Colour themes: interface colours, and syntax colours by capture name.
//!
//! Two themes are built in (`themes/dark.toml` and `themes/light.toml`,
//! compiled into the binary). A user theme is a file of the same format in
//! the config directory's `themes` folder; anything it leaves out comes from
//! its `base` theme.

use std::collections::HashMap;
use std::path::PathBuf;

use nimbus_render::Style;

/// Straight RGBA, each channel 0–1.
pub type Color = [f32; 4];

/// How a syntax capture is drawn: its colour (the text colour if none) and
/// whether it is bold or italic. A theme writes it as `"#rrggbb"`, or as a
/// table such as `{ color = "#c678dd", bold = true, italic = true }`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Look {
    /// The colour; `None` keeps the text colour.
    pub color: Option<Color>,
    /// Bold and italic.
    pub style: Style,
}

const DARK: &str = include_str!("../themes/dark.toml");
const LIGHT: &str = include_str!("../themes/light.toml");

/// Names of the built-in themes.
#[cfg(test)]
const BUILT_IN: [&str; 2] = ["dark", "light"];

macro_rules! ui_colors {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {
        /// Colours for everything other than syntax.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Ui {
            $($(#[$doc])* pub $name: Color,)*
        }

        impl Ui {
            /// Set the colour named `key`; `false` if there is no such colour.
            fn set(&mut self, key: &str, color: Color) -> bool {
                match key {
                    $(stringify!($name) => self.$name = color,)*
                    _ => return false,
                }
                true
            }
        }

        impl Default for Ui {
            /// Everything magenta, so a colour missing from a built-in
            /// theme stands out (and fails the tests).
            fn default() -> Self {
                Self { $($name: [1.0, 0.0, 1.0, 1.0],)* }
            }
        }

        #[cfg(test)]
        const UI_KEYS: &[&str] = &[$(stringify!($name)),*];
    };
}

ui_colors! {
    /// The editor and window background.
    background,
    /// Text with no syntax colour.
    text,
    /// Line numbers, labels and other secondary text.
    gutter,
    /// The cursor line's number.
    current_line_number,
    /// The text cursor.
    cursor,
    /// The underline of text being composed with an input method.
    preedit,
    /// Selected text's background.
    selection,
    /// Behind the bracket at the cursor and its partner.
    bracket,
    /// The scrollbar's thumb.
    scrollbar,
    /// Search matches.
    find_match,
    /// The current search match.
    current_match,
    /// The find bar and message line.
    bar,
    /// Text fields.
    field,
    /// The border of the focused text field.
    field_focus,
    /// An option toggle that is on.
    toggle_on,
    /// Error messages.
    error,
    /// The tab bar.
    tab_bar,
    /// The active tab.
    tab_active,
    /// The line marking the active tab.
    tab_accent,
    /// The picker's box.
    picker,
    /// The picker's border.
    picker_border,
    /// The picker's selected row.
    picker_selected,
    /// The matched text in a find-in-files preview.
    search_match,
    /// A file's row in the find-in-files results.
    search_file,
    /// Compare view: lines only in the new text.
    diff_added,
    /// Compare view: lines only in the old text.
    diff_removed,
    /// Compare view: lines changed on both sides.
    diff_changed,
    /// Compare view: the changed words within a changed line.
    diff_word,
    /// Compare view: filler rows where the other side has more lines.
    diff_filler,
    /// Merge view: a conflict.
    merge_conflict,
    /// Behind the chip after a folded line (M13).
    fold_chip,
    /// Behind the lines pinned by sticky scroll (M20).
    sticky,
    /// The shadow under the pinned lines.
    sticky_shadow,
    /// The minimap's mark over the lines on screen (M20).
    minimap_slider,
    /// The `·` and `→` of visible whitespace (M16).
    whitespace,
    /// Indent guides.
    guide,
    /// The indent guide enclosing the cursor.
    guide_active,
    /// The box marking a character that would otherwise be invisible: a
    /// bidirectional control or a zero-width character (M26).
    hidden_char,
}

/// A loaded theme.
#[derive(Debug, Clone)]
pub struct Theme {
    /// The built-in theme it starts from: its own name for a built-in.
    pub base: String,
    /// Interface colours.
    pub ui: Ui,
    syntax: HashMap<String, Look>,
    /// Look per capture index, per capture table (one per grammar, one
    /// for the simple rules; `nimbus_syntax::registry::TABLES`), built on
    /// first use.
    resolved: [Option<Vec<Option<Look>>>; nimbus_syntax::registry::TABLES],
}

impl Default for Theme {
    fn default() -> Self {
        Self::built_in("dark").unwrap_or_else(|| Self::empty("dark"))
    }
}

/// Parse `"#rrggbb"` or `"#rrggbbaa"`.
fn parse_color(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
        return None;
    }
    let channel = |i: usize| -> Option<f32> {
        let v = u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()?;
        Some(f32::from(v) / 255.0)
    };
    let a = if hex.len() == 8 { channel(6)? } else { 1.0 };
    Some([channel(0)?, channel(2)?, channel(4)?, a])
}

impl Theme {
    fn empty(name: &str) -> Self {
        Self {
            base: name.to_owned(),
            ui: Ui::default(),
            syntax: HashMap::new(),
            resolved: Default::default(),
        }
    }

    /// A theme compiled into the binary.
    #[must_use]
    pub fn built_in(name: &str) -> Option<Self> {
        let source = match name {
            "dark" => DARK,
            "light" => LIGHT,
            _ => return None,
        };
        let (theme, warnings) = Self::parse(name, source, &Self::empty(name));
        debug_assert!(warnings.is_empty(), "{warnings:?}");
        Some(theme)
    }

    /// Where user themes live: `themes/NAME.toml` next to the config file.
    #[must_use]
    pub fn user_path(name: &str) -> Option<PathBuf> {
        crate::config::path().and_then(|p| {
            p.parent()
                .map(|d| d.join("themes").join(format!("{name}.toml")))
        })
    }

    /// The theme called `name`: built in, or a user theme. Falls back to
    /// dark, with a warning, if it cannot be found or read.
    #[must_use]
    pub fn load(name: &str) -> (Self, Vec<String>) {
        if let Some(t) = Self::built_in(name) {
            return (t, Vec::new());
        }
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
        let Some(path) = Self::user_path(name).filter(|_| valid) else {
            return (
                Self::default(),
                vec![format!("theme {name:?} not found; using dark")],
            );
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let base = toml::from_str::<toml::Table>(&text)
                    .ok()
                    .and_then(|t| t.get("base").and_then(|b| b.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| "dark".to_owned());
                let Some(base_theme) = Self::built_in(&base) else {
                    return (
                        Self::default(),
                        vec![format!(
                            "theme {name}: base {base:?} must be a built-in theme; using dark"
                        )],
                    );
                };
                Self::parse(name, &text, &base_theme)
            }
            Err(e) => (
                Self::default(),
                vec![format!(
                    "theme {name:?}: cannot read {}: {e}; using dark",
                    path.display()
                )],
            ),
        }
    }

    /// Parse theme text over `base`. Bad entries become warnings and keep
    /// the base's colour.
    #[must_use]
    pub fn parse(name: &str, text: &str, base: &Self) -> (Self, Vec<String>) {
        let mut theme = Self {
            base: base.base.clone(),
            ui: base.ui,
            syntax: base.syntax.clone(),
            resolved: Default::default(),
        };
        let mut warnings = Vec::new();
        let table = match text.parse::<toml::Table>() {
            Ok(t) => t,
            Err(e) => {
                let msg = e.to_string();
                let first = msg.lines().next().unwrap_or("syntax error");
                warnings.push(format!("theme {name}: {first}"));
                return (theme, warnings);
            }
        };
        let color = |section: &str, key: &str, value: &toml::Value, warnings: &mut Vec<String>| {
            let c = value.as_str().and_then(parse_color);
            if c.is_none() {
                warnings.push(format!(
                    "theme {name}: {section}.{key}: expected \"#rrggbb\" or \"#rrggbbaa\""
                ));
            }
            c
        };
        for (section, value) in &table {
            match (section.as_str(), value) {
                ("base", toml::Value::String(_)) => {}
                ("ui", toml::Value::Table(t)) => {
                    for (key, v) in t {
                        if let Some(c) = color("ui", key, v, &mut warnings)
                            && !theme.ui.set(key, c)
                        {
                            warnings.push(format!("theme {name}: unknown ui colour {key:?}"));
                        }
                    }
                }
                ("syntax", toml::Value::Table(t)) => {
                    for (key, v) in t {
                        let look = match v {
                            toml::Value::Table(fields) => {
                                syntax_look(name, key, fields, &mut warnings)
                            }
                            _ => color("syntax", key, v, &mut warnings).map(|c| Look {
                                color: Some(c),
                                style: Style::default(),
                            }),
                        };
                        if let Some(look) = look {
                            theme.syntax.insert(key.clone(), look);
                        }
                    }
                }
                _ => warnings.push(format!("theme {name}: unknown setting {section:?}")),
            }
        }
        (theme, warnings)
    }

    /// The colour for a capture name: its own entry, or the longest
    /// dotted prefix that has one (`function.method` falls back to
    /// `function`). `None` means the default text colour.
    #[cfg(test)]
    #[must_use]
    pub fn syntax_color(&self, capture: &str) -> Option<Color> {
        lookup(&self.syntax, capture).and_then(|l| l.color)
    }

    /// Whether the theme is dark, judged from its background's brightness
    /// rather than its `base`, so a user theme that starts from `dark` but
    /// paints a pale background counts as light.
    ///
    /// The window frame and native dialogs come from the operating system,
    /// which only knows "light" or "dark"; this is the answer Nimbus gives
    /// it so the title bar matches the editor instead of the system setting.
    #[must_use]
    pub fn is_dark(&self) -> bool {
        // Rec. 709 luma of the sRGB values: a cheap "how bright does this
        // look" that is plenty for a yes/no answer.
        let [r, g, b, _] = self.ui.background;
        0.2126 * r + 0.7152 * g + 0.0722 * b < 0.5
    }

    /// The look of `capture` in capture table `table`, once
    /// [`Theme::capture_looks`] has resolved that table; `None` (the text
    /// colour) before, or for a capture the theme does not name.
    #[must_use]
    pub fn look(&self, table: usize, capture: u32) -> Option<Look> {
        self.resolved
            .get(table)?
            .as_ref()?
            .get(capture as usize)
            .copied()
            .flatten()
    }

    /// The look the theme gives a capture name such as `text.title`,
    /// falling back to its parents (`text`) as captures do.
    #[must_use]
    pub fn named(&self, capture: &str) -> Option<Look> {
        lookup(&self.syntax, capture)
    }

    /// Resolve every capture table whose grammar is loaded, so
    /// [`Theme::look`] answers for spans from any of them (a Markdown
    /// file's fenced Rust block is coloured by Rust's table).
    pub fn resolve_tables(&mut self) {
        for table in 0..nimbus_syntax::registry::TABLES {
            if let Some(names) = nimbus_syntax::registry::capture_table(table) {
                let _ = self.capture_looks(table, names);
            }
        }
    }

    /// Looks by capture index for a list of capture names, cached under
    /// `key` (a capture table: one per grammar, plus one for the simple
    /// rules large files use). Worked out once per key, so each frame only
    /// indexes an array.
    pub fn capture_looks(&mut self, key: usize, names: &[&str]) -> &[Option<Look>] {
        let Some(slot) = self.resolved.get_mut(key) else {
            return &[];
        };
        if slot.as_ref().is_none_or(|v| v.len() != names.len()) {
            *slot = Some(names.iter().map(|n| lookup(&self.syntax, n)).collect());
        }
        slot.as_deref().unwrap_or(&[])
    }
}

/// A syntax entry written as a table: `color`, `bold` and `italic`, each
/// optional. Mistakes become warnings; `None` if nothing usable is left.
fn syntax_look(
    theme: &str,
    key: &str,
    fields: &toml::Table,
    warnings: &mut Vec<String>,
) -> Option<Look> {
    let mut look = Look::default();
    for (field, v) in fields {
        match (field.as_str(), v) {
            ("color", toml::Value::String(s)) if parse_color(s).is_some() => {
                look.color = parse_color(s);
            }
            ("bold", toml::Value::Boolean(b)) => look.style.bold = *b,
            ("italic", toml::Value::Boolean(b)) => look.style.italic = *b,
            _ => warnings.push(format!(
                "theme {theme}: syntax.{key}.{field}: expected color = \"#rrggbb\", bold = true or italic = true"
            )),
        }
    }
    (look != Look::default()).then_some(look)
}

fn lookup(syntax: &HashMap<String, Look>, capture: &str) -> Option<Look> {
    let mut name = capture;
    loop {
        if let Some(look) = syntax.get(name) {
            return Some(*look);
        }
        name = &name[..name.rfind('.')?];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_themes_set_every_colour() {
        for name in BUILT_IN {
            let (theme, warnings) = Theme::parse(
                name,
                if name == "dark" { DARK } else { LIGHT },
                &Theme::empty(name),
            );
            assert_eq!(warnings, Vec::<String>::new(), "{name}");
            let ui = theme.ui;
            assert_ne!(ui, Ui::default());
            let magenta = [1.0, 0.0, 1.0, 1.0];
            for key in UI_KEYS {
                let mut probe = ui;
                assert!(probe.set(key, magenta));
                assert_ne!(probe, ui, "{name}: {key} left at the placeholder");
            }
        }
    }

    #[test]
    fn dark_keeps_the_icon_colours_to_eight_bits() {
        // The dark theme's background is the icon's ink and its cursor the
        // icon's amber; changing either changes the brand, not just the theme.
        let ui = Theme::built_in("dark").unwrap().ui;
        let close = |a: Color, b: Color| {
            a.iter()
                .zip(b)
                .all(|(x, y)| (x - y).abs() <= 0.5 / 255.0 + 1e-6)
        };
        assert!(close(ui.background, [0.102, 0.110, 0.133, 1.0]));
        assert!(close(ui.text, [0.85, 0.87, 0.91, 1.0]));
        assert!(close(ui.cursor, [0.98, 0.749, 0.298, 1.0]));
        assert!(close(ui.tab_accent, ui.cursor));
    }

    #[test]
    fn darkness_follows_the_background() {
        assert!(Theme::built_in("dark").unwrap().is_dark());
        assert!(!Theme::built_in("light").unwrap().is_dark());
        // A user theme on the dark base with a pale background is light.
        let (pale, warnings) = Theme::parse(
            "pale",
            "base = \"dark\"\n[ui]\nbackground = \"#f4f1e8\"\n",
            &Theme::built_in("dark").unwrap(),
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!pale.is_dark());
    }

    #[test]
    fn colours_parse_with_and_without_alpha() {
        assert_eq!(parse_color("#ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_color("#00000000"), Some([0.0; 4]));
        for bad in ["ff0000", "#ff00", "#gg0000", "#ff00000", "#ffé000"] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn longest_prefix_wins() {
        let (t, w) = Theme::parse(
            "t",
            "[syntax]\nfunction = \"#ff0000\"\n\"function.method\" = \"#00ff00\"\n",
            &Theme::empty("t"),
        );
        assert_eq!(w, Vec::<String>::new());
        let red = Some([1.0, 0.0, 0.0, 1.0]);
        let green = Some([0.0, 1.0, 0.0, 1.0]);
        assert_eq!(t.syntax_color("function"), red);
        assert_eq!(t.syntax_color("function.builtin"), red);
        assert_eq!(t.syntax_color("function.method.call"), green);
        assert_eq!(t.syntax_color("variable"), None);
        let mut t = t;
        let names = ["variable", "function.method", "function.macro"];
        let colors: Vec<_> = t
            .capture_looks(nimbus_syntax::LanguageId::Rust.index(), &names)
            .iter()
            .map(|l| l.and_then(|l| l.color))
            .collect();
        assert_eq!(colors, [None, green, red]);
    }

    #[test]
    fn syntax_entries_can_be_bold_or_italic() {
        let (mut t, w) = Theme::parse(
            "t",
            "[syntax]\nkeyword = { color = \"#ff0000\", bold = true }\ncomment = { italic = true }\nstring = { bold = \"yes\" }\n",
            &Theme::empty("t"),
        );
        assert_eq!(w.len(), 1, "{w:?}");
        let looks = t
            .capture_looks(0, &["keyword.control", "comment", "string"])
            .to_vec();
        assert_eq!(
            looks[0],
            Some(Look {
                color: Some([1.0, 0.0, 0.0, 1.0]),
                style: Style {
                    bold: true,
                    italic: false
                },
            })
        );
        assert_eq!(
            looks[1],
            Some(Look {
                color: None,
                style: Style {
                    bold: false,
                    italic: true
                },
            })
        );
        assert_eq!(looks[2], None, "a bad entry is ignored with a warning");
    }

    #[test]
    fn user_themes_override_their_base_and_warn_on_mistakes() {
        let light = Theme::built_in("light").unwrap();
        let (t, w) = Theme::parse(
            "mine",
            "base = \"light\"\n[ui]\ncursor = \"#ff0000\"\nnope = \"#000000\"\n[syntax]\nkeyword = \"red\"\n[extra]\n",
            &light,
        );
        assert_eq!(t.ui.cursor, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(t.base, "light");
        assert_eq!(t.ui.background, light.ui.background);
        assert_eq!(t.syntax_color("keyword"), light.syntax_color("keyword"));
        assert_eq!(w.len(), 3, "{w:?}");
    }

    #[test]
    fn unknown_names_fall_back_to_dark() {
        let (t, w) = Theme::load("../etc/passwd");
        assert_eq!(t.base, "dark");
        assert_eq!(w.len(), 1);
    }
}
