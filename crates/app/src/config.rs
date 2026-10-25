//! The config file: `config.toml` in the platform's config directory.
//!
//! Every setting has a default, so a missing file, a partial file, or one
//! with mistakes still gives a working editor. Problems become warnings shown
//! in the message line, never errors that stop startup.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

/// Settings read from the config file.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent on/off settings, each one key in the file"
)]
pub struct Config {
    /// Editor font size in points.
    pub font_size: f64,
    /// Columns per tab stop.
    pub tab_width: usize,
    /// Start new tabs with soft wrap on.
    pub soft_wrap: bool,
    /// Colour theme: "dark", "light", or the name of a user theme in the
    /// config directory's `themes` folder. Checked when the theme loads.
    pub theme: String,
    /// Key binding overrides: action id to chords; an empty list unbinds.
    pub keys: HashMap<String, Vec<String>>,
    /// File name or `*.ext` pattern to language name.
    pub languages: HashMap<String, String>,
    /// File name or `*.ext` pattern to the encoding such files are read
    /// and saved in, skipping detection (M17): `"*.csv" = "windows-1252"`.
    /// Checked by `io::set_encoding_rules`.
    pub encodings: HashMap<String, String>,
    /// Ask GitHub for the latest release once at startup. On unless set
    /// to `false` (M31, reversing M15's default): one HTTPS request with
    /// a `nimbus/X.Y.Z` user agent, which speaks only when a newer release
    /// exists. Installing it still waits for Install Update.
    pub check_updates: bool,
    /// Editing behaviour: auto-pairs and what happens on save, with
    /// per-language overrides (`[editor.rust]`).
    pub editor: EditorSettings,
    /// Indentation defaults and detection, with per-language overrides
    /// (`[indent.python]`).
    pub indent: IndentSettings,
    /// The editor font: an installed font's family name
    /// (`"JetBrains Mono"`) or a font file's path. Empty means the bundled Hack Nerd Font
    /// Mono. A named font is looked up on a worker after the first frame,
    /// which draws with the bundled font.
    pub font: String,
    /// Let the font join characters such as `->` and `!=` into ligatures,
    /// for fonts that have them (Hack has none).
    pub ligatures: bool,
    /// The weight to draw the editor font at (M24), 100 to 900, or `None`
    /// for its regular weight. A variable font draws at exactly this
    /// weight; a family of separate files uses the nearest one it has.
    /// The config takes a number or a name (`"light"`, `"medium"`,
    /// `"semibold"`, ...).
    #[serde(skip)]
    pub font_weight: Option<u16>,
    /// Fonts to try first for characters the editor font lacks, by script
    /// (M24): `han = ["Hiragino Sans"]`, `cyrillic = [...]`. Keys are
    /// `han`, `kana`, `hangul`, `emoji`, `symbols` or a Unicode script
    /// name in lower case. Tried before the built-in list per platform.
    pub fallback_fonts: HashMap<String, Vec<String>>,
    /// Which regional shapes Han ideographs take (M24): `"ja"`,
    /// `"zh-Hans"`, `"zh-Hant"` or `"ko"`; empty follows the system
    /// language. Checked by [`Config::cjk_language`].
    pub cjk_language: String,
    /// Start with the zoom the last session ended with (kept in
    /// `state.toml`); `false` starts every session at `font_size`.
    pub remember_zoom: bool,
    /// Reopen the last session at startup and save this one (the open
    /// tabs, editor groups and tree, `DESIGN.md` §9). `false` starts every
    /// run fresh, as `--no-session` does, but still keeps recent files,
    /// the window position and the zoom. Read once at startup.
    pub remember_workspace: bool,
    /// Where spaces and tabs show as `·` and `→`.
    pub render_whitespace: Whitespace,
    /// Draw a hairline at each indentation level.
    pub indent_guides: bool,
    /// Pin the enclosing scopes (function, impl, heading) of the top line
    /// above the text (M20).
    pub sticky_scroll: bool,
    /// Show the minimap at the right edge of the text (M20). Off unless
    /// set; "Toggle Minimap" switches it for the session.
    pub minimap: bool,
    /// The minimap's width in points (logical pixels), 40 to 400.
    pub minimap_width: f64,
    /// A proportional font for the interface: the sidebar, tabs, status
    /// bar, palette and pickers (M20). An installed font's family name or
    /// a font file's path; empty uses the bundled editor font, so the
    /// first frame needs no font lookup. The editor text stays monospace.
    pub ui_font: String,
    /// Draw on the CPU (compatibility mode, M23) even when a GPU is
    /// there, as `--compat` does. Without it, Nimbus switches to
    /// compatibility mode by itself only when the GPU cannot be used.
    /// Read once at startup.
    pub compat: bool,
}

/// Which spaces and tabs are drawn visibly (`render_whitespace`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Whitespace {
    /// Nowhere.
    None,
    /// Inside selections, as VS Code does by default.
    #[default]
    Selection,
    /// Everywhere.
    All,
}

impl Whitespace {
    /// The next mode, for the palette's toggle: none, selection, all.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::None => Self::Selection,
            Self::Selection => Self::All,
            Self::All => Self::None,
        }
    }

    /// The mode's name as the config spells it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Selection => "selection",
            Self::All => "all",
        }
    }
}

/// The `[editor]` table and its per-language sub-tables. Every field is
/// optional so that a language table can override just one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    /// Typing an opening bracket or quote inserts its closer. Unset means:
    /// on for code, off for Markdown and plain text.
    pub auto_pairs: Option<bool>,
    /// Remove spaces and tabs from line ends when saving.
    pub trim_trailing_whitespace: Option<bool>,
    /// End the file with a line break when saving.
    pub final_newline: Option<bool>,
    /// Open files of this language with soft wrap on. Unset means: on for
    /// Markdown, else the top-level `soft_wrap`.
    pub wrap: Option<bool>,
    /// Overrides by language key (`rust`, `markdown`, a user language's
    /// name), each with the same settings.
    #[serde(flatten)]
    pub languages: HashMap<String, EditorSettings>,
}

/// The `[indent]` table and its per-language sub-tables.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct IndentSettings {
    /// Columns per level.
    pub width: Option<usize>,
    /// A tab per level rather than spaces.
    pub tabs: Option<bool>,
    /// Let the file's own indentation win over these settings (the
    /// default).
    pub detect: Option<bool>,
    /// Overrides by language key.
    #[serde(flatten)]
    pub languages: HashMap<String, IndentSettings>,
}

/// What one document edits with: the editor settings resolved for its
/// language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Editing {
    /// Whether typing an opener inserts its closer.
    pub auto_pairs: bool,
    /// Trim line ends on save.
    pub trim_trailing_whitespace: bool,
    /// Add a final line break on save.
    pub final_newline: bool,
    /// Start with soft wrap on: what the language's table, the `[editor]`
    /// table or the prose default says, or `None` for the top-level
    /// `soft_wrap` setting.
    pub wrap: Option<bool>,
}

/// Indentation resolved for one document's language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indenting {
    /// The indentation when the file does not say.
    pub indent: nimbus_core::indent::Indent,
    /// Whether the file's own indentation wins.
    pub detect: bool,
}

/// Languages that are prose rather than code, where auto-pairs are off
/// unless the config says otherwise.
const PROSE: &[&str] = &["markdown"];

impl EditorSettings {
    /// The settings for a document in `language` (its key, or `None` for
    /// plain text): the language's table over the general one over the
    /// defaults.
    #[must_use]
    pub fn for_language(&self, language: Option<&str>) -> Editing {
        let lang = language.and_then(|l| self.languages.get(l));
        let pick = |f: fn(&Self) -> Option<bool>, default: bool| {
            lang.and_then(f).or_else(|| f(self)).unwrap_or(default)
        };
        let prose = language.is_some_and(|l| PROSE.contains(&l));
        let code = language.is_some() && !prose;
        Editing {
            auto_pairs: pick(|s| s.auto_pairs, code),
            trim_trailing_whitespace: pick(|s| s.trim_trailing_whitespace, false),
            final_newline: pick(|s| s.final_newline, false),
            wrap: lang
                .and_then(|s| s.wrap)
                .or(self.wrap)
                .or_else(|| prose.then_some(true)),
        }
    }
}

impl IndentSettings {
    /// The indentation settings for a document in `language`.
    #[must_use]
    pub fn for_language(&self, language: Option<&str>) -> Indenting {
        let lang = language.and_then(|l| self.languages.get(l));
        let base = nimbus_core::indent::Indent::default();
        let width = lang
            .and_then(|s| s.width)
            .or(self.width)
            .filter(|w| (1..=16).contains(w))
            .unwrap_or(base.width);
        let tabs = lang.and_then(|s| s.tabs).or(self.tabs).unwrap_or(base.tabs);
        let detect = lang.and_then(|s| s.detect).or(self.detect).unwrap_or(true);
        Indenting {
            indent: nimbus_core::indent::Indent { width, tabs },
            detect,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            tab_width: 4,
            soft_wrap: false,
            theme: "dark".to_owned(),
            keys: HashMap::new(),
            languages: HashMap::new(),
            encodings: HashMap::new(),
            check_updates: true,
            editor: EditorSettings::default(),
            indent: IndentSettings::default(),
            font: String::new(),
            ligatures: false,
            font_weight: None,
            fallback_fonts: HashMap::new(),
            cjk_language: String::new(),
            remember_zoom: true,
            remember_workspace: true,
            render_whitespace: Whitespace::Selection,
            indent_guides: false,
            sticky_scroll: true,
            minimap: false,
            minimap_width: 100.0,
            ui_font: String::new(),
            compat: false,
        }
    }
}

/// Read one setting from `table` into `into`, keeping the default and
/// adding a warning if it has the wrong type.
fn take<T: serde::de::DeserializeOwned>(
    table: &toml::Table,
    key: &str,
    into: &mut T,
    warnings: &mut Vec<String>,
) {
    if let Some(v) = table.get(key) {
        match v.clone().try_into::<T>() {
            Ok(x) => *into = x,
            Err(e) => warnings.push(format!("config: {key}: {}; using the default", e.message())),
        }
    }
}

/// Read the international text settings (M24): `font_weight`,
/// `fallback_fonts` and `cjk_language`, checking each.
fn take_international(table: &toml::Table, config: &mut Config, warnings: &mut Vec<String>) {
    config.font_weight = table.get("font_weight").and_then(|v| match font_weight(v) {
        Ok(w) => Some(w),
        Err(why) => {
            warnings.push(format!("config: font_weight: {why}; using the default"));
            None
        }
    });
    take(
        table,
        "fallback_fonts",
        &mut config.fallback_fonts,
        warnings,
    );
    for key in config.fallback_fonts.keys() {
        if !nimbus_render::script::KNOWN_KEYS.contains(&key.as_str()) {
            warnings.push(format!(
                "config: fallback_fonts: unknown script {key:?} (han, kana, hangul, \
                     emoji, symbols or a script name such as cyrillic)"
            ));
        }
    }
    take(table, "cjk_language", &mut config.cjk_language, warnings);
    if !config.cjk_language.is_empty()
        && nimbus_render::CjkLanguage::parse(&config.cjk_language).is_none()
    {
        warnings.push(format!(
            "config: cjk_language {:?} is not ja, zh-Hans, zh-Hant or ko; \
                 following the system language",
            config.cjk_language
        ));
        config.cjk_language.clear();
    }
}

/// Read `font_weight`: a number from 100 to 900, or a weight's name.
fn font_weight(v: &toml::Value) -> Result<u16, String> {
    if let Some(n) = v.as_integer() {
        return u16::try_from(n)
            .ok()
            .filter(|n| (100..=900).contains(n))
            .ok_or_else(|| format!("{n} out of range 100–900"));
    }
    let Some(name) = v.as_str() else {
        return Err("expected a number or a name such as \"bold\"".to_owned());
    };
    let w = match name
        .to_ascii_lowercase()
        .replace(['-', ' ', '_'], "")
        .as_str()
    {
        "thin" | "hairline" => 100,
        "extralight" | "ultralight" => 200,
        "light" => 300,
        "regular" | "normal" | "book" => 400,
        "medium" => 500,
        "semibold" | "demibold" => 600,
        "bold" => 700,
        "extrabold" | "ultrabold" => 800,
        "black" | "heavy" => 900,
        _ => return Err(format!("unknown weight {name:?}")),
    };
    Ok(w)
}

impl Config {
    /// The language Han ideographs are drawn for: the configured one, or
    /// the system's from `system_locale` (a BCP 47 tag such as `ja-JP`).
    #[must_use]
    pub fn cjk_language(&self, system_locale: Option<&str>) -> nimbus_render::CjkLanguage {
        nimbus_render::CjkLanguage::parse(&self.cjk_language).unwrap_or_else(|| {
            system_locale
                .map(nimbus_render::CjkLanguage::from_locale)
                .unwrap_or_default()
        })
    }
}

const KNOWN: &[&str] = &[
    "font_size",
    "tab_width",
    "soft_wrap",
    "theme",
    "keys",
    "languages",
    "encodings",
    "check_updates",
    "editor",
    "indent",
    "font",
    "ligatures",
    "font_weight",
    "fallback_fonts",
    "cjk_language",
    "remember_zoom",
    "remember_workspace",
    "render_whitespace",
    "indent_guides",
    "sticky_scroll",
    "minimap",
    "minimap_width",
    "ui_font",
    "compat",
];

/// A commented starting point written by "Open Config File" when there is
/// no config yet.
pub const TEMPLATE: &str = r#"# Nimbus configuration. Every setting is optional.

# font_size = 14.0
# tab_width = 4
# soft_wrap = false
# theme = "dark"            # or "light", or NAME for themes/NAME.toml here
# check_updates = true      # false: do not ask GitHub for a newer release at startup

# The editor font: an installed font's name or a font file's path. Empty is
# the bundled Hack Nerd Font Mono. ligatures joins -> != => and friends in
# fonts that have them (Fira Code, JetBrains Mono, Cascadia Code).
# font = "JetBrains Mono"
# ligatures = false
# The weight to draw it at: 100 to 900 or a name ("light", "regular",
# "medium", "semibold", "bold"). A variable font takes any weight; a family
# of separate files uses its nearest. Unset is the font's regular weight.
# font_weight = 450
# Which regional shapes Chinese characters take: "ja", "zh-Hans",
# "zh-Hant" or "ko". Empty follows the system language.
# cjk_language = ""
# Zoom (View > Zoom In/Out, Primary+= and Primary+-) is remembered between
# sessions; false starts every session at font_size.
# remember_zoom = true
# `nimbus` alone reopens the last session's tabs, and `nimbus FOLDER` that
# folder's; false starts every run with one empty tab and saves no session.
# remember_workspace = true
# Show spaces as · and tabs as →: "selection", "all" or "none".
# render_whitespace = "selection"
# indent_guides = false     # true: a hairline at each indentation level
# sticky_scroll = true      # pin the enclosing function or heading at the top
# minimap = false           # true: a map of the lines around the view, at the right
# minimap_width = 100       # in points
# A proportional font for the sidebar, tabs, status bar, pickers and the
# Markdown preview, by name or path. Empty uses the editor font. The text
# stays monospace.
# ui_font = "Inter"
# Draw on the CPU instead of the GPU (compatibility mode), for a GPU or
# driver that misdraws. Nimbus does this by itself when there is no usable
# GPU. Restart to apply.
# compat = false

# Key bindings: action id = list of chords. "Primary" is Cmd on macOS and
# Ctrl elsewhere. An empty list unbinds the action. The command palette
# (Primary+Shift+P) shows every action's name.
# [keys]
# "view.toggleWrap" = ["Alt+W"]
# "file.quickOpen" = ["Primary+P", "Primary+E"]

# Fonts for characters the editor font lacks, tried before the built-in
# list for each script. Keys: han, kana, hangul, emoji, symbols, or a script
# name such as cyrillic, greek, devanagari, thai. A font that is not
# installed is skipped.
# [fallback_fonts]
# han = ["Hiragino Sans"]
# hangul = ["Apple SD Gothic Neo"]

# Languages: a file name or "*.ext" pattern = language. Built-in languages
# are rust, markdown, json, toml, yaml, python, javascript, typescript,
# bash, c, go, html, css, tsx and powershell. Add your own as languages/NAME.toml here (simple colouring rules),
# and change a built-in's colours with queries/NAME.scm (restart to apply).
# [languages]
# "Justfile" = "bash"
# "*.jsonc" = "json"

# Editing. auto_pairs: typing ( [ { " ' or ` inserts the closer; on for
# code and off for Markdown and plain text unless set. The two save
# settings are off so files round-trip byte for byte. wrap: soft wrap for
# files of a language; Markdown wraps unless told not to. A language table
# ([editor.markdown], [editor.lua]) overrides any of the four.
# [editor]
# auto_pairs = true
# trim_trailing_whitespace = false
# final_newline = false
# [editor.markdown]
# wrap = false

# Indentation. With detect = true (the default) the file's own
# indentation wins over width and tabs. [indent.python] and the like
# override per language.
# [indent]
# width = 4
# tabs = false
# detect = true
"#;

/// Every setting with its built-in default, as config text, for the
/// "Show Default Config" palette action.
///
/// Unlike [`TEMPLATE`], whose lines are commented examples, each value
/// here is read from [`Config::default`] and written out live, so the text
/// cannot fall behind the code: change a default and this changes with it,
/// and add a field to [`Config`] and this stops compiling until it is
/// listed (the `let Config { .. }` below names every field). Parsing the
/// result gives the defaults back; a test checks that.
///
/// Three settings have no single value and stay commented: `font_weight`
/// (unset means the font's own regular weight), and `[editor]`'s
/// `auto_pairs` and `wrap`, whose defaults depend on the language.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "one format string: the lines are the text it writes"
)]
pub fn defaults_text() -> String {
    let Config {
        font_size,
        tab_width,
        soft_wrap,
        theme,
        keys: _,
        languages: _,
        encodings: _,
        check_updates,
        editor,
        indent,
        font,
        ligatures,
        font_weight: _,
        fallback_fonts: _,
        cjk_language,
        remember_zoom,
        remember_workspace,
        render_whitespace,
        indent_guides,
        sticky_scroll,
        minimap,
        minimap_width,
        ui_font,
        compat,
    } = Config::default();
    let quote = |s: &str| toml::Value::String(s.to_owned()).to_string();
    let theme = quote(&theme);
    let font = quote(&font);
    let cjk_language = quote(&cjk_language);
    let ui_font = quote(&ui_font);
    let render_whitespace = quote(render_whitespace.name());
    let editing = editor.for_language(None);
    let trim = editing.trim_trailing_whitespace;
    let final_newline = editing.final_newline;
    let indenting = indent.for_language(None);
    let (indent_width, indent_tabs, detect) = (
        indenting.indent.width,
        indenting.indent.tabs,
        indenting.detect,
    );
    let builtin: Vec<&str> = nimbus_syntax::LanguageId::ALL
        .iter()
        .map(|l| l.key())
        .collect();
    let builtin = builtin.join(", ");
    format!(
        r#"# Nimbus's default settings, generated from the built-in values of
# this version ({version}). Nothing reads this text: put only the settings
# you want to change in your own config.toml (Open Config File in the
# palette), then run Reload Config.

# The editor font's size in points, 6 to 72.
font_size = {font_size:?}
# Columns per tab stop, 1 to 16.
tab_width = {tab_width}
# Start new tabs with soft wrap on.
soft_wrap = {soft_wrap}
# "dark", "light", or NAME for themes/NAME.toml beside config.toml.
theme = {theme}
# Ask GitHub once at startup whether a newer release exists.
check_updates = {check_updates}

# The editor font: an installed font's name or a font file's path. Empty
# is the bundled Hack Nerd Font Mono.
font = {font}
# Join -> != => and friends, in fonts that have ligatures.
ligatures = {ligatures}
# The weight to draw the font at: 100 to 900 or a name ("light",
# "medium", "bold"). Unset is the font's own regular weight.
# font_weight = 400
# Which regional shapes Chinese characters take: "ja", "zh-Hans",
# "zh-Hant" or "ko". Empty follows the system language.
cjk_language = {cjk_language}
# Start each session at the zoom the last one ended with.
remember_zoom = {remember_zoom}
# Reopen the last session's tabs at startup and save this one.
remember_workspace = {remember_workspace}
# Show spaces as · and tabs as →: "selection", "all" or "none".
render_whitespace = {render_whitespace}
# A hairline at each indentation level.
indent_guides = {indent_guides}
# Pin the enclosing function or heading at the top of the text.
sticky_scroll = {sticky_scroll}
# A map of the lines around the view at the right edge, and its width in
# points (40 to 400).
minimap = {minimap}
minimap_width = {minimap_width:?}
# A proportional font for the sidebar, tabs, status bar and pickers, by
# name or path. Empty uses the editor font.
ui_font = {ui_font}
# Draw on the CPU instead of the GPU (compatibility mode). Nimbus does
# this by itself when there is no usable GPU. Read at startup.
compat = {compat}

# Editing, with per-language tables such as [editor.markdown].
# auto_pairs: typing ( [ {{ " ' or ` inserts the closer. Unset is on for
# code, off for Markdown and plain text.
# wrap: soft wrap for files of a language. Unset is on for Markdown,
# else soft_wrap above.
[editor]
trim_trailing_whitespace = {trim}
final_newline = {final_newline}

# Indentation, with per-language tables such as [indent.python]. With
# detect on, a file's own indentation wins over width and tabs.
[indent]
width = {indent_width}
tabs = {indent_tabs}
detect = {detect}

# These tables are empty by default.

# Key bindings: action id = list of chords, such as
# "view.toggleWrap" = ["Alt+W"]. An empty list unbinds the action.
[keys]

# A file name or "*.ext" pattern = language, such as "*.jsonc" = "json".
# Built-in languages:
# {builtin}.
[languages]

# A file name or "*.ext" pattern = the encoding to read and save it in,
# such as "*.csv" = "windows-1252".
[encodings]

# Fonts to try first for characters the editor font lacks, by script:
# han, kana, hangul, emoji, symbols, or a script name such as cyrillic.
# Such as han = ["Hiragino Sans"].
[fallback_fonts]
"#,
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// Where the config file lives: `~/Library/Application Support/nimbus` on
/// macOS, `~/.config/nimbus` on Linux, `%APPDATA%\nimbus` on Windows, or
/// the folder in `NIMBUS_CONFIG_DIR` if set (for trying a config without
/// touching the real one). Themes, languages and queries live beside it.
#[must_use]
pub fn path() -> Option<PathBuf> {
    let dir = std::env::var_os("NIMBUS_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|d| d.join("nimbus")))?;
    Some(dir.join("config.toml"))
}

impl Config {
    /// Parse config text. Returns the settings (defaults for anything
    /// missing or wrong) and warnings to show.
    #[must_use]
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let table = match text.parse::<toml::Table>() {
            Ok(t) => t,
            Err(e) => {
                let msg = e.to_string();
                let first = msg.lines().next().unwrap_or("syntax error");
                warnings.push(format!("config: {first}; using defaults"));
                return (Self::default(), warnings);
            }
        };
        for key in table.keys() {
            if !KNOWN.contains(&key.as_str()) {
                warnings.push(format!("config: unknown setting {key:?}"));
            }
        }
        // Read each setting on its own, so one wrong value costs only that
        // setting, not the whole file.
        let mut config = Self::default();
        take(&table, "font_size", &mut config.font_size, &mut warnings);
        take(&table, "tab_width", &mut config.tab_width, &mut warnings);
        take(&table, "soft_wrap", &mut config.soft_wrap, &mut warnings);
        take(&table, "theme", &mut config.theme, &mut warnings);
        take(&table, "keys", &mut config.keys, &mut warnings);
        take(&table, "languages", &mut config.languages, &mut warnings);
        take(&table, "encodings", &mut config.encodings, &mut warnings);
        take(
            &table,
            "check_updates",
            &mut config.check_updates,
            &mut warnings,
        );
        take(&table, "editor", &mut config.editor, &mut warnings);
        take(&table, "indent", &mut config.indent, &mut warnings);
        take(&table, "font", &mut config.font, &mut warnings);
        take(&table, "ligatures", &mut config.ligatures, &mut warnings);
        take_international(&table, &mut config, &mut warnings);
        take(
            &table,
            "remember_zoom",
            &mut config.remember_zoom,
            &mut warnings,
        );
        take(
            &table,
            "remember_workspace",
            &mut config.remember_workspace,
            &mut warnings,
        );
        take(
            &table,
            "render_whitespace",
            &mut config.render_whitespace,
            &mut warnings,
        );
        take(
            &table,
            "indent_guides",
            &mut config.indent_guides,
            &mut warnings,
        );
        take(
            &table,
            "sticky_scroll",
            &mut config.sticky_scroll,
            &mut warnings,
        );
        take(&table, "minimap", &mut config.minimap, &mut warnings);
        take(
            &table,
            "minimap_width",
            &mut config.minimap_width,
            &mut warnings,
        );
        take(&table, "ui_font", &mut config.ui_font, &mut warnings);
        take(&table, "compat", &mut config.compat, &mut warnings);
        if !(40.0..=400.0).contains(&config.minimap_width) {
            warnings.push(format!(
                "config: minimap_width {} out of range 40–400",
                config.minimap_width
            ));
            config.minimap_width = Self::default().minimap_width;
        }
        if let Some(w) = config.indent.width.filter(|w| !(1..=16).contains(w)) {
            warnings.push(format!("config: indent.width {w} out of range 1–16"));
            config.indent.width = None;
        }
        if !(6.0..=72.0).contains(&config.font_size) {
            warnings.push(format!(
                "config: font_size {} out of range 6–72",
                config.font_size
            ));
            config.font_size = Self::default().font_size;
        }
        if !(1..=16).contains(&config.tab_width) {
            warnings.push(format!(
                "config: tab_width {} out of range 1–16",
                config.tab_width
            ));
            config.tab_width = Self::default().tab_width;
        }
        (config, warnings)
    }

    /// Read the config file. A missing file gives the defaults silently.
    #[must_use]
    pub fn load() -> (Self, Vec<String>) {
        let Some(path) = path() else {
            return (Self::default(), Vec::new());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), Vec::new()),
            Err(e) => (
                Self::default(),
                vec![format!("config: cannot read {}: {e}", path.display())],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_partial_files_use_defaults() {
        assert_eq!(Config::parse(""), (Config::default(), vec![]));
        let (c, w) = Config::parse("tab_width = 2\n");
        assert_eq!(c.tab_width, 2);
        assert_eq!(c.font_size, 14.0);
        assert_eq!(w, Vec::<String>::new());
    }

    #[test]
    fn keys_table_parses() {
        let (c, w) =
            Config::parse("[keys]\n\"view.toggleWrap\" = [\"Alt+W\"]\n\"file.save\" = []\n");
        assert_eq!(w, Vec::<String>::new());
        assert_eq!(c.keys["view.toggleWrap"], vec!["Alt+W".to_owned()]);
        assert_eq!(c.keys["file.save"], Vec::<String>::new());
    }

    #[test]
    fn mistakes_become_warnings_not_failures() {
        let (c, w) = Config::parse("tab_width = \n");
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 1);
        let (_, w) = Config::parse("tab_widht = 2\n");
        assert!(w[0].contains("tab_widht"), "{w:?}");
        let (c, w) = Config::parse("tab_width = 99\nfont_size = 2.0\ntheme = \"solar\"\n");
        // The theme name is checked when the theme loads, not here.
        assert_eq!(
            (c.tab_width, c.font_size, c.theme.as_str()),
            (4, 14.0, "solar")
        );
        assert_eq!(w.len(), 2, "{w:?}");
        let (c, w) = Config::parse("tab_width = \"wide\"\n");
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 1);
        // One wrong value does not cost the other settings.
        let (c, w) = Config::parse(
            "font_size = 16.0\nsoft_wrap = \"yes\"\n[keys]\n\"file.save\" = [\"F2\"]\n",
        );
        assert_eq!(c.font_size, 16.0);
        assert_eq!(c.keys["file.save"], vec!["F2".to_owned()]);
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn editor_and_indent_tables_resolve_per_language() {
        let (c, w) = Config::parse(
            "[editor]\nauto_pairs = false\nfinal_newline = true\n[editor.rust]\nauto_pairs = true\n\
             wrap = true\n[editor.markdown]\nwrap = false\n\
             [indent]\nwidth = 2\n[indent.python]\nwidth = 4\ntabs = false\ndetect = false\n\
             [indent.makefile]\ntabs = true\n",
        );
        assert_eq!(w, Vec::<String>::new());
        let rust = c.editor.for_language(Some("rust"));
        assert!(rust.auto_pairs && rust.final_newline && !rust.trim_trailing_whitespace);
        assert!(!c.editor.for_language(Some("python")).auto_pairs);
        assert!(!c.editor.for_language(None).auto_pairs);
        // Wrap: a language table says, else Markdown wraps and the rest
        // leave it to the top-level `soft_wrap` (`None`).
        assert_eq!(rust.wrap, Some(true));
        assert_eq!(c.editor.for_language(Some("markdown")).wrap, Some(false));
        assert_eq!(c.editor.for_language(Some("python")).wrap, None);
        assert_eq!(
            Config::default().editor.for_language(Some("markdown")).wrap,
            Some(true)
        );
        let py = c.indent.for_language(Some("python"));
        assert_eq!(
            (py.indent.width, py.indent.tabs, py.detect),
            (4, false, false)
        );
        let mk = c.indent.for_language(Some("makefile"));
        assert_eq!(
            (mk.indent.width, mk.indent.tabs, mk.detect),
            (2, true, true)
        );
        assert_eq!(c.indent.for_language(None).indent.width, 2);
        // Defaults: auto-pairs for code only; detection on; 4 spaces.
        let d = Config::default();
        assert!(d.editor.for_language(Some("rust")).auto_pairs);
        assert!(!d.editor.for_language(Some("markdown")).auto_pairs);
        assert!(!d.editor.for_language(None).auto_pairs);
        let plain = d.indent.for_language(None);
        assert_eq!(
            (plain.indent.width, plain.indent.tabs, plain.detect),
            (4, false, true)
        );
        let (c, w) = Config::parse("[indent]\nwidth = 99\n");
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(c.indent.for_language(None).indent.width, 4);
    }

    #[test]
    fn rendering_settings_parse() {
        let (c, w) = Config::parse(
            "font = \"Fira Code\"\nligatures = true\nremember_zoom = false\n\
             render_whitespace = \"all\"\nindent_guides = true\n",
        );
        assert_eq!(w, Vec::<String>::new());
        assert_eq!(c.font, "Fira Code");
        assert!(c.ligatures && !c.remember_zoom && c.indent_guides);
        assert!(c.remember_workspace, "on unless set");
        assert_eq!(c.render_whitespace, Whitespace::All);
        let (c, w) = Config::parse("render_whitespace = \"some\"\n");
        assert_eq!(c.render_whitespace, Whitespace::Selection);
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(Whitespace::All.next(), Whitespace::None);
        let (c, w) = Config::parse("remember_workspace = false\n");
        assert_eq!(w, Vec::<String>::new());
        assert!(!c.remember_workspace);
    }

    #[test]
    fn reading_aid_settings_parse() {
        let c = Config::default();
        assert!(c.sticky_scroll && !c.minimap && c.ui_font.is_empty());
        let (c, w) = Config::parse(
            "sticky_scroll = false\nminimap = true\nminimap_width = 120\nui_font = \"Inter\"\n",
        );
        assert_eq!(w, Vec::<String>::new());
        assert!(!c.sticky_scroll && c.minimap);
        assert!((c.minimap_width - 120.0).abs() < f64::EPSILON);
        assert_eq!(c.ui_font, "Inter");
        let (c, w) = Config::parse("minimap_width = 5\n");
        assert!((c.minimap_width - 100.0).abs() < f64::EPSILON);
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn international_text_settings_parse() {
        let (c, w) = Config::parse(
            "font_weight = 450\ncjk_language = \"ja\"\n\
             [fallback_fonts]\nhan = [\"Hiragino Sans\"]\ncyrillic = [\"PT Mono\"]\n",
        );
        assert_eq!(w, Vec::<String>::new());
        assert_eq!(c.font_weight, Some(450));
        assert_eq!(c.cjk_language(None), nimbus_render::CjkLanguage::Japanese);
        assert_eq!(c.fallback_fonts["han"], ["Hiragino Sans"]);
        let (c, w) = Config::parse("font_weight = \"semi-bold\"\n");
        assert_eq!((c.font_weight, w.len()), (Some(600), 0));
    }

    #[test]
    fn international_text_mistakes_warn_and_fall_back() {
        let (c, w) = Config::parse(
            "font_weight = 1000\ncjk_language = \"klingon\"\n[fallback_fonts]\nhna = [\"X\"]\n",
        );
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!(c.font_weight, None);
        assert_eq!(c.cjk_language, "");
        // Empty follows the system, and any other system gets the default.
        assert_eq!(
            c.cjk_language(Some("ko-KR")),
            nimbus_render::CjkLanguage::Korean
        );
        assert_eq!(
            c.cjk_language(Some("en-GB")),
            nimbus_render::CjkLanguage::SimplifiedChinese
        );
        let (_, w) = Config::parse("font_weight = \"wobbly\"\n");
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn template_parses_cleanly() {
        assert_eq!(Config::parse(TEMPLATE), (Config::default(), vec![]));
    }

    #[test]
    fn defaults_text_parses_back_to_the_defaults() {
        let text = defaults_text();
        let (mut parsed, warnings) = Config::parse(&text);
        assert_eq!(warnings, Vec::<String>::new());
        // `[editor]` and `[indent]` spell out what unset already means, so
        // compare what they resolve to for a few kinds of file instead.
        let d = Config::default();
        for lang in [None, Some("rust"), Some("markdown"), Some("python")] {
            assert_eq!(
                parsed.editor.for_language(lang),
                d.editor.for_language(lang)
            );
            assert_eq!(
                parsed.indent.for_language(lang),
                d.indent.for_language(lang)
            );
        }
        parsed.editor = d.editor.clone();
        parsed.indent = d.indent.clone();
        assert_eq!(parsed, d);
    }

    #[test]
    fn defaults_text_lists_every_setting() {
        let text = defaults_text();
        for key in KNOWN {
            let listed = text.lines().any(|l| {
                let l = l.strip_prefix("# ").unwrap_or(l);
                l == format!("[{key}]") || l.starts_with(&format!("{key} = "))
            });
            assert!(listed, "{key} missing from defaults_text()");
        }
    }

    #[test]
    fn template_names_every_built_in_language() {
        let words: String = TEMPLATE
            .lines()
            .skip_while(|l| !l.starts_with("# Languages:"))
            .take(3)
            .collect();
        for l in nimbus_syntax::LanguageId::ALL {
            let key = l.key();
            assert!(
                words
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|w| w == key),
                "{key} missing from the template's language list"
            );
        }
    }
}
